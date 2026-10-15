//! Writing rows into `transactions` (shared by importers and syncs).

use chrono::NaiveDateTime;
use portfolio_core::AssetClass;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::DbDecimal;

/// A transaction to record. Amount is always non-negative; its direction comes from `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTransaction {
    /// One of 'buy', 'sell', 'deposit', 'withdrawal', 'fee', 'dividend'.
    pub kind: &'static str,
    /// UTC; stored with whole seconds.
    pub executed_at: NaiveDateTime,
    /// Asset symbol (`assets.symbol`), when the transaction concerns an asset.
    pub asset: Option<&'static str>,
    pub quantity: Option<Decimal>,
    pub unit_price: Option<Decimal>,
    pub amount: Decimal,
    pub currency: String,
    /// Identity at the source, unique per `source`.
    pub external_id: String,
    pub note: Option<String>,
}

/// An account, created if missing.
#[derive(Debug, Clone, Copy)]
pub struct Account<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub provider: &'a str,
    pub currency: &'a str,
}

/// An asset, created if missing.
#[derive(Debug, Clone, Copy)]
pub struct Asset<'a> {
    pub symbol: &'a str,
    pub name: &'a str,
    pub class: AssetClass,
    pub currency: &'a str,
}

/// Writes transactions for one account in a single database transaction, skipping those whose
/// (`source`, `external_id`) already exists. Returns how many were written.
pub async fn insert_transactions(
    pool: &SqlitePool,
    account: Account<'_>,
    assets: &[Asset<'_>],
    source: &str,
    rows: &[NewTransaction],
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO accounts (name, kind, provider, currency) VALUES (?, ?, ?, ?)
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(account.name)
    .bind(account.kind)
    .bind(account.provider)
    .bind(account.currency)
    .execute(&mut *tx)
    .await?;
    for asset in assets {
        sqlx::query(
            "INSERT INTO assets (symbol, name, asset_class, currency) VALUES (?, ?, ?, ?)
             ON CONFLICT (symbol) DO NOTHING",
        )
        .bind(asset.symbol)
        .bind(asset.name)
        .bind(asset.class.as_str())
        .bind(asset.currency)
        .execute(&mut *tx)
        .await?;
    }
    let mut written = 0;
    for row in rows {
        let result = sqlx::query(
            "INSERT INTO transactions
               (account_id, asset_id, kind, executed_at, quantity, unit_price, amount, currency,
                source, external_id, note)
             VALUES ((SELECT id FROM accounts WHERE name = ?),
                     (SELECT id FROM assets WHERE symbol = ?),
                     ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (source, external_id) DO NOTHING",
        )
        .bind(account.name)
        .bind(row.asset)
        .bind(row.kind)
        .bind(row.executed_at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .bind(row.quantity.map(DbDecimal))
        .bind(row.unit_price.map(DbDecimal))
        .bind(DbDecimal(row.amount))
        .bind(&row.currency)
        .bind(source)
        .bind(&row.external_id)
        .bind(row.note.as_deref())
        .execute(&mut *tx)
        .await?;
        written += result.rows_affected() as usize;
    }
    tx.commit().await?;
    Ok(written)
}

/// Every transaction, oldest first, in the shape the portfolio calculations use.
pub async fn load_ledger(
    pool: &SqlitePool,
) -> Result<Vec<portfolio_core::portfolio::LedgerTx>, sqlx::Error> {
    use portfolio_core::portfolio::{AccountKind, LedgerTx, TxKind};
    type Row = (
        String,
        String,
        String,
        Option<String>,
        String,
        Option<DbDecimal>,
        DbDecimal,
        String,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT t.executed_at, a.name, a.kind, s.symbol, t.kind, t.quantity, t.amount, t.currency
         FROM transactions t
         JOIN accounts a ON a.id = t.account_id
         LEFT JOIN assets s ON s.id = t.asset_id
         ORDER BY t.executed_at, t.id",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(
            |(at, account, account_kind, asset, kind, quantity, amount, currency)| {
                let decode = |msg: String| sqlx::Error::Decode(msg.into());
                let date = NaiveDateTime::parse_from_str(&at, "%Y-%m-%dT%H:%M:%SZ")
                    .map_err(|e| decode(format!("executed_at {at:?}: {e}")))?
                    .date();
                Ok(LedgerTx {
                    date,
                    account,
                    account_kind: if account_kind == "manual" {
                        AccountKind::Manual
                    } else {
                        AccountKind::Funded
                    },
                    kind: TxKind::parse(&kind).ok_or_else(|| decode(format!("kind {kind:?}")))?,
                    asset,
                    quantity: quantity.map(|q| q.0),
                    amount: amount.0,
                    currency,
                })
            },
        )
        .collect()
}
