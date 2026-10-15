//! Exchange-rate lookup with a database cache in front of NBP.

use chrono::{Days, NaiveDate};
use portfolio_core::fx::{FxRate, rate_on};
use portfolio_db::fx as cache;
use sqlx::SqlitePool;

use crate::nbp::{NbpClient, NbpError};

/// How far back to look for the last published table. The longest real gap is around
/// Christmas (e.g. 2025-12-23 -> 2025-12-29), well within two weeks.
pub const LOOKBACK_DAYS: u64 = 14;

#[derive(Debug)]
pub enum FxError {
    Db(sqlx::Error),
    Nbp(NbpError),
    /// Asked for a day after `today`.
    FutureDate(NaiveDate),
    /// PLN is the base currency: there is no NBP rate for it.
    BaseCurrency,
    /// NBP published nothing for `currency` in the lookback window ending on the given day.
    NoRate(String, NaiveDate),
}

impl std::fmt::Display for FxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FxError::Db(e) => write!(f, "fx cache error: {e}"),
            FxError::Nbp(e) => write!(f, "{e}"),
            FxError::FutureDate(day) => write!(f, "no exchange rate for a future day ({day})"),
            FxError::BaseCurrency => write!(f, "PLN is the base currency and has no NBP rate"),
            FxError::NoRate(currency, day) => write!(
                f,
                "NBP published no {currency} rate in the {LOOKBACK_DAYS} days up to {day}"
            ),
        }
    }
}

impl std::error::Error for FxError {}

impl From<sqlx::Error> for FxError {
    fn from(e: sqlx::Error) -> Self {
        FxError::Db(e)
    }
}

impl From<NbpError> for FxError {
    fn from(e: NbpError) -> Self {
        FxError::Nbp(e)
    }
}

/// NBP rate for `currency` that applies on `day`: that day's table, or the last one published
/// before it (weekends, holidays, or today before NBP publishes around noon).
///
/// `today` is the current date in Warsaw; it is passed in so behaviour is testable.
/// Past days are answered from the cache when possible and cached once resolved; today is
/// always re-checked against NBP until its own table appears.
pub async fn rate_on_day(
    pool: &SqlitePool,
    nbp: &NbpClient,
    currency: &str,
    day: NaiveDate,
    today: NaiveDate,
) -> Result<FxRate, FxError> {
    let currency = currency.to_ascii_uppercase();
    if currency == "PLN" {
        return Err(FxError::BaseCurrency);
    }
    if day > today {
        return Err(FxError::FutureDate(day));
    }

    if let Some(rate) = cache::rate_for_date(pool, &currency, day).await? {
        return Ok(rate);
    }
    if day < today
        && let Some(rate) = cache::resolved_rate(pool, &currency, day).await?
    {
        return Ok(rate);
    }

    let from = day - Days::new(LOOKBACK_DAYS);
    let fetched = nbp.table_a_range(&currency, from, day).await?;
    cache::upsert_rates(pool, &fetched).await?;

    let known = cache::rates_between(pool, &currency, from, day).await?;
    let rate = rate_on(&known, day)
        .cloned()
        .ok_or_else(|| FxError::NoRate(currency.clone(), day))?;

    // Every day in the window from the first known table up to yesterday now has a final
    // answer; cache them so later lookups (weekends especially) skip the network.
    if let Some(first) = known.first().map(|r| r.effective_date) {
        let last_final = day.min(today.pred_opt().expect("date after 0001-01-01"));
        let resolutions: Vec<(NaiveDate, NaiveDate)> = first
            .iter_days()
            .take_while(|d| *d <= last_final)
            .filter_map(|d| rate_on(&known, d).map(|r| (d, r.effective_date)))
            .collect();
        cache::record_resolutions(pool, &currency, &resolutions).await?;
    }

    Ok(rate)
}
