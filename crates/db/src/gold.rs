//! Physical gold coins (`gold_coins`) and their purchase transactions.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::DbDecimal;

pub const ACCOUNT_NAME: &str = "Gold (manual)";
pub const ASSET_SYMBOL: &str = "GOLD_COINS";

const DATE_FORMAT: &str = "%Y-%m-%d";

/// A coin to add. Values must already be validated (positive, sensible dates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCoin {
    pub label: Option<String>,
    pub purchased_on: NaiveDate,
    pub purchase_price_pln: Decimal,
    pub fine_weight_g: Decimal,
    pub manual_quote_pln: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coin {
    pub id: i64,
    pub transaction_id: i64,
    pub label: Option<String>,
    pub purchased_on: NaiveDate,
    pub purchase_price_pln: Decimal,
    pub fine_weight_g: Decimal,
    pub manual_quote_pln: Option<Decimal>,
}

type CoinRow = (
    i64,
    i64,
    Option<String>,
    String,
    DbDecimal,
    DbDecimal,
    Option<DbDecimal>,
);

fn from_row(
    (id, transaction_id, label, purchased_on, price, weight, quote): CoinRow,
) -> Result<Coin, sqlx::Error> {
    Ok(Coin {
        id,
        transaction_id,
        label,
        purchased_on: NaiveDate::parse_from_str(&purchased_on, DATE_FORMAT)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        purchase_price_pln: price.0,
        fine_weight_g: weight.0,
        manual_quote_pln: quote.map(|q| q.0),
    })
}

const SELECT: &str = "SELECT id, transaction_id, label, purchased_on, purchase_price_pln,
                             fine_weight_g, manual_quote_pln
                      FROM gold_coins";

/// Adds a coin and its purchase transaction atomically.
pub async fn add_coin(pool: &SqlitePool, coin: &NewCoin) -> Result<Coin, sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO accounts (name, kind, provider, currency) VALUES (?, 'manual', 'manual', 'PLN')
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(ACCOUNT_NAME)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO assets (symbol, name, asset_class, currency)
         VALUES (?, 'Physical gold coins', 'gold', 'PLN')
         ON CONFLICT (symbol) DO NOTHING",
    )
    .bind(ASSET_SYMBOL)
    .execute(&mut *tx)
    .await?;

    let price = DbDecimal(coin.purchase_price_pln);
    // The external id needs the coin id, which only exists after the insert below: write a
    // placeholder first and replace it within the same database transaction.
    let transaction_id: i64 = sqlx::query_scalar(
        "INSERT INTO transactions
           (account_id, asset_id, kind, executed_at, quantity, unit_price, amount, currency,
            source, external_id, note)
         VALUES ((SELECT id FROM accounts WHERE name = ?), (SELECT id FROM assets WHERE symbol = ?),
                 'buy', ?, '1', ?, ?, 'PLN', 'manual', 'gold-coin-pending', ?)
         RETURNING id",
    )
    .bind(ACCOUNT_NAME)
    .bind(ASSET_SYMBOL)
    .bind(format!(
        "{}T00:00:00Z",
        coin.purchased_on.format(DATE_FORMAT)
    ))
    .bind(price)
    .bind(price)
    .bind(coin.label.as_deref())
    .fetch_one(&mut *tx)
    .await?;

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO gold_coins
           (transaction_id, label, purchased_on, purchase_price_pln, fine_weight_g,
            manual_quote_pln)
         VALUES (?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(transaction_id)
    .bind(coin.label.as_deref())
    .bind(coin.purchased_on.format(DATE_FORMAT).to_string())
    .bind(price)
    .bind(DbDecimal(coin.fine_weight_g))
    .bind(coin.manual_quote_pln.map(DbDecimal))
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query("UPDATE transactions SET external_id = ? WHERE id = ?")
        .bind(format!("gold-coin-{id}"))
        .bind(transaction_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(Coin {
        id,
        transaction_id,
        label: coin.label.clone(),
        purchased_on: coin.purchased_on,
        purchase_price_pln: coin.purchase_price_pln,
        fine_weight_g: coin.fine_weight_g,
        manual_quote_pln: coin.manual_quote_pln,
    })
}

/// All coins, oldest purchase first.
pub async fn list_coins(pool: &SqlitePool) -> Result<Vec<Coin>, sqlx::Error> {
    sqlx::query_as::<_, CoinRow>(&format!("{SELECT} ORDER BY purchased_on, id"))
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(from_row)
        .collect()
}

pub async fn get_coin(pool: &SqlitePool, id: i64) -> Result<Option<Coin>, sqlx::Error> {
    sqlx::query_as::<_, CoinRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(from_row)
        .transpose()
}

/// Deletes a coin together with its purchase transaction. Returns false if it did not exist.
pub async fn delete_coin(pool: &SqlitePool, id: i64) -> Result<bool, sqlx::Error> {
    // Deleting the transaction cascades to the coin.
    let result = sqlx::query(
        "DELETE FROM transactions WHERE id = (SELECT transaction_id FROM gold_coins WHERE id = ?)",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}
