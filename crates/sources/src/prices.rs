//! Instrument prices with a database cache in front of the provider, and their PLN value.

use chrono::{DateTime, Days, NaiveDate, TimeDelta, Utc};
use portfolio_core::AssetClass;
use portfolio_core::fx::FxRate;
use portfolio_core::price::{CurrencyMismatch, PriceQuote};
use portfolio_db::prices as cache;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::alphavantage::{AlphaVantageClient, AlphaVantageError};
use crate::fx::{FxError, rate_on_day};
use crate::nbp::NbpClient;

/// A tradable instrument and how to find it at the price provider.
#[derive(Debug, Clone, Copy)]
pub struct Instrument {
    /// Internal symbol (`assets.symbol`).
    pub symbol: &'static str,
    pub name: &'static str,
    pub class: AssetClass,
    /// Currency the provider quotes it in.
    pub currency: &'static str,
    /// Symbol at Alpha Vantage.
    pub provider_symbol: &'static str,
}

/// Vanguard FTSE All-World (Acc) on Xetra, in EUR.
pub const VWCE: Instrument = Instrument {
    symbol: "VWCE",
    name: "Vanguard FTSE All-World UCITS ETF (USD) Accumulating",
    class: AssetClass::Vwce,
    currency: "EUR",
    provider_symbol: "VWCE.DEX",
};

const SOURCE: &str = "alphavantage";

/// Longest stretch without a close that is still a market holiday rather than missing data
/// (Xetra over Christmas and New Year closes for at most a few days in a row).
pub const MAX_GAP_DAYS: u64 = 10;

/// The free tier allows ~25 requests a day: never refetch the same instrument more often than
/// this, even when today's close is not out yet.
pub const MIN_REFETCH_INTERVAL: TimeDelta = TimeDelta::hours(6);

#[derive(Debug)]
pub enum PriceError {
    Db(sqlx::Error),
    Provider(AlphaVantageError),
    Fx(FxError),
    Currency(CurrencyMismatch),
    FutureDate(NaiveDate),
    /// No close within `MAX_GAP_DAYS` before the day (outside the provider's window, or the
    /// provider has no data for the symbol).
    NoPrice(&'static str, NaiveDate),
}

impl std::fmt::Display for PriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PriceError::Db(e) => write!(f, "price cache error: {e}"),
            PriceError::Provider(e) => write!(f, "{e}"),
            PriceError::Fx(e) => write!(f, "{e}"),
            PriceError::Currency(e) => write!(f, "{e}"),
            PriceError::FutureDate(day) => write!(f, "no price for a future day ({day})"),
            PriceError::NoPrice(symbol, day) => {
                write!(
                    f,
                    "no {symbol} close within {MAX_GAP_DAYS} days up to {day}"
                )
            }
        }
    }
}

impl std::error::Error for PriceError {}

impl From<sqlx::Error> for PriceError {
    fn from(e: sqlx::Error) -> Self {
        PriceError::Db(e)
    }
}

impl From<AlphaVantageError> for PriceError {
    fn from(e: AlphaVantageError) -> Self {
        PriceError::Provider(e)
    }
}

impl From<FxError> for PriceError {
    fn from(e: FxError) -> Self {
        PriceError::Fx(e)
    }
}

impl From<CurrencyMismatch> for PriceError {
    fn from(e: CurrencyMismatch) -> Self {
        PriceError::Currency(e)
    }
}

/// Current time and the current date in Warsaw, passed in so behaviour is testable.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub now: DateTime<Utc>,
    pub today: NaiveDate,
}

/// Latest stored close within `MAX_GAP_DAYS` up to and including `day`.
async fn cached_close(
    pool: &SqlitePool,
    asset_id: i64,
    day: NaiveDate,
) -> Result<Option<PriceQuote>, sqlx::Error> {
    let from = day - Days::new(MAX_GAP_DAYS);
    Ok(cache::closes_between(pool, asset_id, from, day)
        .await?
        .pop()
        .map(|(c, currency)| PriceQuote {
            date: c.date,
            close: c.close,
            currency,
        }))
}

/// The close that applies on `day`: that day's, or the last trading day's before it.
///
/// Answered from the cache when it can be: an exact close, or an earlier one when a later close
/// is stored too (so the gap is known to be a market holiday). Otherwise the provider's recent
/// series is fetched and cached, at most once per `MIN_REFETCH_INTERVAL`.
pub async fn close_on_day(
    pool: &SqlitePool,
    provider: &AlphaVantageClient,
    instrument: &Instrument,
    day: NaiveDate,
    clock: Clock,
) -> Result<PriceQuote, PriceError> {
    if day > clock.today {
        return Err(PriceError::FutureDate(day));
    }
    let asset_id = cache::ensure_asset(
        pool,
        instrument.symbol,
        instrument.name,
        instrument.class,
        instrument.currency,
    )
    .await?;

    if let Some(quote) = cache::quote_for_date(pool, asset_id, day).await? {
        return Ok(quote);
    }
    if cache::has_close_after(pool, asset_id, day).await?
        && let Some(quote) = cached_close(pool, asset_id, day).await?
    {
        return Ok(quote);
    }

    let recently_fetched = cache::last_fetched_at(pool, asset_id)
        .await?
        .is_some_and(|t| clock.now - t < MIN_REFETCH_INTERVAL);
    if !recently_fetched {
        let closes = provider.daily_closes(instrument.provider_symbol).await?;
        cache::upsert_closes(
            pool,
            asset_id,
            instrument.currency,
            SOURCE,
            &closes,
            clock.now,
        )
        .await?;
    }

    cached_close(pool, asset_id, day)
        .await?
        .ok_or(PriceError::NoPrice(instrument.symbol, day))
}

/// An instrument's close on a day, converted to PLN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Valuation {
    pub quote: PriceQuote,
    /// NBP rate for the quote's trading day (absent for PLN-quoted instruments).
    pub fx: Option<FxRate>,
    pub close_pln: Decimal,
}

/// Close on `day` in PLN. The NBP rate is taken for the close's own trading day, so price and
/// rate describe the same moment (a Sunday uses Friday's close and Friday's rate).
pub async fn value_in_pln(
    pool: &SqlitePool,
    provider: &AlphaVantageClient,
    nbp: &NbpClient,
    instrument: &Instrument,
    day: NaiveDate,
    clock: Clock,
) -> Result<Valuation, PriceError> {
    let quote = close_on_day(pool, provider, instrument, day, clock).await?;
    if quote.currency.eq_ignore_ascii_case("PLN") {
        return Ok(Valuation {
            close_pln: quote.close,
            quote,
            fx: None,
        });
    }
    let fx = rate_on_day(pool, nbp, &quote.currency, quote.date, clock.today).await?;
    let close_pln = quote.to_pln(&fx)?;
    Ok(Valuation {
        quote,
        fx: Some(fx),
        close_pln,
    })
}
