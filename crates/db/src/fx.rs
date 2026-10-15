//! Cache of NBP exchange rates (`fx_rates`) and of day -> rate resolutions (`fx_rate_days`).

use chrono::NaiveDate;
use portfolio_core::fx::FxRate;
use sqlx::SqlitePool;

use crate::DbDecimal;

const DATE_FORMAT: &str = "%Y-%m-%d";

fn date_text(day: NaiveDate) -> String {
    day.format(DATE_FORMAT).to_string()
}

type RateRow = (String, String, DbDecimal, String);

fn from_row((currency, rate_date, mid, table_no): RateRow) -> Result<FxRate, sqlx::Error> {
    let effective_date = NaiveDate::parse_from_str(&rate_date, DATE_FORMAT)
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(FxRate {
        currency,
        effective_date,
        mid: mid.0,
        table_no,
    })
}

/// Inserts rates, replacing any stored rate for the same currency and day. NBP does not
/// revise published tables, so replacing is only a safety net.
pub async fn upsert_rates(pool: &SqlitePool, rates: &[FxRate]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for rate in rates {
        sqlx::query(
            "INSERT INTO fx_rates (currency, rate_date, rate, table_no) VALUES (?, ?, ?, ?)
             ON CONFLICT (currency, rate_date) DO UPDATE
             SET rate = excluded.rate, table_no = excluded.table_no,
                 fetched_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        )
        .bind(&rate.currency)
        .bind(date_text(rate.effective_date))
        .bind(DbDecimal(rate.mid))
        .bind(&rate.table_no)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// The rate published for exactly `day`, if cached.
pub async fn rate_for_date(
    pool: &SqlitePool,
    currency: &str,
    day: NaiveDate,
) -> Result<Option<FxRate>, sqlx::Error> {
    sqlx::query_as::<_, RateRow>(
        "SELECT currency, rate_date, rate, table_no FROM fx_rates
         WHERE currency = ? AND rate_date = ?",
    )
    .bind(currency)
    .bind(date_text(day))
    .fetch_optional(pool)
    .await?
    .map(from_row)
    .transpose()
}

/// Cached rates for `currency` with `from <= rate_date <= to`, oldest first.
pub async fn rates_between(
    pool: &SqlitePool,
    currency: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<FxRate>, sqlx::Error> {
    sqlx::query_as::<_, RateRow>(
        "SELECT currency, rate_date, rate, table_no FROM fx_rates
         WHERE currency = ? AND rate_date BETWEEN ? AND ?
         ORDER BY rate_date",
    )
    .bind(currency)
    .bind(date_text(from))
    .bind(date_text(to))
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(from_row)
    .collect()
}

/// The rate previously resolved for `day`, if any.
pub async fn resolved_rate(
    pool: &SqlitePool,
    currency: &str,
    day: NaiveDate,
) -> Result<Option<FxRate>, sqlx::Error> {
    sqlx::query_as::<_, RateRow>(
        "SELECT r.currency, r.rate_date, r.rate, r.table_no
         FROM fx_rate_days d
         JOIN fx_rates r ON r.currency = d.currency AND r.rate_date = d.rate_date
         WHERE d.currency = ? AND d.day = ?",
    )
    .bind(currency)
    .bind(date_text(day))
    .fetch_optional(pool)
    .await?
    .map(from_row)
    .transpose()
}

/// Records that `day` resolves to the rate published on `rate_date`. Callers must only do this
/// for days that are already over, when the answer can no longer change.
pub async fn record_resolutions(
    pool: &SqlitePool,
    currency: &str,
    resolutions: &[(NaiveDate, NaiveDate)],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for (day, rate_date) in resolutions {
        sqlx::query(
            "INSERT INTO fx_rate_days (currency, day, rate_date) VALUES (?, ?, ?)
             ON CONFLICT (currency, day) DO UPDATE SET rate_date = excluded.rate_date",
        )
        .bind(currency)
        .bind(date_text(*day))
        .bind(date_text(*rate_date))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}
