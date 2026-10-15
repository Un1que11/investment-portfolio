//! Assets and the daily price cache (`assets`, `prices`).

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_core::AssetClass;
use portfolio_core::price::{DailyClose, PriceQuote};
use sqlx::SqlitePool;

use crate::DbDecimal;

const DATE_FORMAT: &str = "%Y-%m-%d";

fn date_text(day: NaiveDate) -> String {
    day.format(DATE_FORMAT).to_string()
}

fn parse_date(text: &str) -> Result<NaiveDate, sqlx::Error> {
    NaiveDate::parse_from_str(text, DATE_FORMAT).map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

/// Id of the asset with `symbol`, inserting it first if it does not exist yet.
pub async fn ensure_asset(
    pool: &SqlitePool,
    symbol: &str,
    name: &str,
    class: AssetClass,
    currency: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query(
        "INSERT INTO assets (symbol, name, asset_class, currency) VALUES (?, ?, ?, ?)
         ON CONFLICT (symbol) DO NOTHING",
    )
    .bind(symbol)
    .bind(name)
    .bind(class.as_str())
    .bind(currency)
    .execute(pool)
    .await?;
    sqlx::query_scalar("SELECT id FROM assets WHERE symbol = ?")
        .bind(symbol)
        .fetch_one(pool)
        .await
}

/// Inserts closes, replacing a stored close for the same asset and day (providers do correct
/// recent closes occasionally). `fetched_at` comes from the caller's clock so that refetch
/// throttling and tests use one notion of "now".
pub async fn upsert_closes(
    pool: &SqlitePool,
    asset_id: i64,
    currency: &str,
    source: &str,
    closes: &[DailyClose],
    fetched_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let fetched_at = fetched_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut tx = pool.begin().await?;
    for c in closes {
        sqlx::query(
            "INSERT INTO prices (asset_id, price_date, price, currency, source, fetched_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (asset_id, price_date) DO UPDATE
             SET price = excluded.price, currency = excluded.currency, source = excluded.source,
                 fetched_at = excluded.fetched_at",
        )
        .bind(asset_id)
        .bind(date_text(c.date))
        .bind(DbDecimal(c.close))
        .bind(currency)
        .bind(source)
        .bind(&fetched_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Stored closes for the asset with `from <= date <= to`, oldest first.
pub async fn closes_between(
    pool: &SqlitePool,
    asset_id: i64,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<(DailyClose, String)>, sqlx::Error> {
    let rows: Vec<(String, DbDecimal, String)> = sqlx::query_as(
        "SELECT price_date, price, currency FROM prices
         WHERE asset_id = ? AND price_date BETWEEN ? AND ?
         ORDER BY price_date",
    )
    .bind(asset_id)
    .bind(date_text(from))
    .bind(date_text(to))
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(date, close, currency)| {
            Ok((
                DailyClose {
                    date: parse_date(&date)?,
                    close: close.0,
                },
                currency,
            ))
        })
        .collect()
}

/// The stored close for exactly `day`.
pub async fn quote_for_date(
    pool: &SqlitePool,
    asset_id: i64,
    day: NaiveDate,
) -> Result<Option<PriceQuote>, sqlx::Error> {
    Ok(closes_between(pool, asset_id, day, day)
        .await?
        .pop()
        .map(|(c, currency)| PriceQuote {
            date: c.date,
            close: c.close,
            currency,
        }))
}

/// Whether any close after `day` is stored. Closes come from contiguous provider series, so a
/// later close means the series covered `day`, and a missing close there was a non-trading day.
pub async fn has_close_after(
    pool: &SqlitePool,
    asset_id: i64,
    day: NaiveDate,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM prices WHERE asset_id = ? AND price_date > ?)")
        .bind(asset_id)
        .bind(date_text(day))
        .fetch_one(pool)
        .await
}

/// When closes for the asset were last written, if ever.
pub async fn last_fetched_at(
    pool: &SqlitePool,
    asset_id: i64,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let latest: Option<String> =
        sqlx::query_scalar("SELECT max(fetched_at) FROM prices WHERE asset_id = ?")
            .bind(asset_id)
            .fetch_one(pool)
            .await?;
    latest
        .map(|text| {
            DateTime::parse_from_rfc3339(&text)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))
        })
        .transpose()
}
