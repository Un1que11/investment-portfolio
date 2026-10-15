//! Writing parsed XTB operations into `transactions`, idempotently.

use portfolio_db::DbDecimal;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::{CashOperation, Export, OperationKind};

/// Instruments the import knows: XTB ticker -> (asset symbol, name, asset class, currency).
const INSTRUMENTS: &[(&str, &str, &str, &str, &str)] = &[(
    "VWCE.DE",
    "VWCE",
    "Vanguard FTSE All-World UCITS ETF (USD) Accumulating",
    "vwce",
    "EUR",
)];

#[derive(Debug)]
pub enum ImportError {
    Db(sqlx::Error),
    /// A ticker without a known asset; it needs an entry in `INSTRUMENTS` first.
    UnknownInstrument(String),
    /// An account product this import does not map to an account kind.
    UnknownProduct(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Db(e) => write!(f, "database error during XTB import: {e}"),
            ImportError::UnknownInstrument(t) => write!(f, "unknown XTB instrument {t:?}"),
            ImportError::UnknownProduct(p) => write!(f, "unknown XTB account product {p:?}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<sqlx::Error> for ImportError {
    fn from(e: sqlx::Error) -> Self {
        ImportError::Db(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportSummary {
    /// Operations written now.
    pub inserted: usize,
    /// Operations that were already imported earlier (same contents), left untouched.
    pub already_present: usize,
}

/// Account for an XTB product: (name, `accounts.kind`).
fn account_for(product: &str) -> Result<(String, &'static str), ImportError> {
    match product {
        "IKZE" => Ok(("XTB IKZE".into(), "ikze")),
        "IKE" => Ok(("XTB IKE".into(), "ike")),
        other => Err(ImportError::UnknownProduct(other.to_owned())),
    }
}

/// `transactions.external_id` for an operation.
pub fn external_id(op: &CashOperation) -> String {
    format!("xtb-{}", op.hash)
}

/// Imports every operation of `export` in one database transaction. Operations already present
/// (same contents, so the same `external_id`) are skipped, so re-importing the same or an
/// overlapping export is safe.
pub async fn import(pool: &SqlitePool, export: &Export) -> Result<ImportSummary, ImportError> {
    let mut tx = pool.begin().await?;
    let mut summary = ImportSummary::default();

    for op in &export.operations {
        let (account, account_kind) = account_for(&op.product)?;
        sqlx::query(
            "INSERT INTO accounts (name, kind, provider, currency) VALUES (?, ?, 'xtb', 'PLN')
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(&account)
        .bind(account_kind)
        .execute(&mut *tx)
        .await?;

        // Stored with whole seconds, as the schema requires; the hash keeps the exact time.
        let executed_at = op.time.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let (kind, asset, quantity, unit_price, amount, note) = match &op.kind {
            OperationKind::Deposit => ("deposit", None, None, None, op.amount, op.comment.clone()),
            OperationKind::Purchase { ticker, filled, .. } => {
                let &(_, symbol, name, class, currency) = INSTRUMENTS
                    .iter()
                    .find(|i| i.0 == ticker)
                    .ok_or_else(|| ImportError::UnknownInstrument(ticker.clone()))?;
                sqlx::query(
                    "INSERT INTO assets (symbol, name, asset_class, currency) VALUES (?, ?, ?, ?)
                     ON CONFLICT (symbol) DO NOTHING",
                )
                .bind(symbol)
                .bind(name)
                .bind(class)
                .bind(currency)
                .execute(&mut *tx)
                .await?;
                let paid = -op.amount;
                // Transactions carry one currency: amounts are PLN, so the unit price is the
                // PLN paid per unit (EUR price x XTB's conversion rate). The EUR price stays in
                // the note.
                let pln_per_unit = (paid / filled).round_dp(4);
                (
                    "buy",
                    Some(symbol),
                    Some(*filled),
                    Some(pln_per_unit),
                    paid,
                    format!("{} (price in {currency})", op.comment),
                )
            }
        };

        let result = sqlx::query(
            "INSERT INTO transactions
               (account_id, asset_id, kind, executed_at, quantity, unit_price, amount, currency,
                source, external_id, note)
             VALUES ((SELECT id FROM accounts WHERE name = ?),
                     (SELECT id FROM assets WHERE symbol = ?),
                     ?, ?, ?, ?, ?, 'PLN', 'xtb', ?, ?)
             ON CONFLICT (source, external_id) DO NOTHING",
        )
        .bind(&account)
        .bind(asset)
        .bind(kind)
        .bind(executed_at)
        .bind(quantity.map(DbDecimal))
        .bind(unit_price.map(DbDecimal))
        .bind(DbDecimal(amount))
        .bind(external_id(op))
        .bind(note)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 1 {
            summary.inserted += 1;
        } else {
            summary.already_present += 1;
        }
    }

    tx.commit().await?;
    Ok(summary)
}

/// Net PLN paid in by deposits in `export` (useful for a quick check against XTB's own view).
pub fn deposits_total(export: &Export) -> Decimal {
    export
        .operations
        .iter()
        .filter(|o| o.kind == OperationKind::Deposit)
        .map(|o| o.amount)
        .sum()
}
