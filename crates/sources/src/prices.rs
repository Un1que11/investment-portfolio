//! Instrument prices with a database cache in front of the provider, and their PLN value.

use std::collections::HashMap;

use chrono::{DateTime, Days, NaiveDate, Utc};
use portfolio_core::AssetClass;
use portfolio_core::fx::FxRate;
use portfolio_core::price::{CurrencyMismatch, PriceQuote};
use portfolio_db::prices as cache;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::DailySource;
use crate::alphavantage::{AlphaVantageClient, AlphaVantageError};
use crate::fx::{FxError, rate_on_day};
use crate::kraken::{KrakenClient, KrakenError};
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
    /// Which [`DailySource`] prices it (its `NAME`).
    pub provider: &'static str,
    /// Symbol or pair at that provider.
    pub provider_symbol: &'static str,
}

/// Vanguard FTSE All-World (Acc) on Xetra, in EUR.
pub const VWCE: Instrument = Instrument {
    symbol: "VWCE",
    name: "Vanguard FTSE All-World UCITS ETF (USD) Accumulating",
    class: AssetClass::Vwce,
    currency: "EUR",
    provider: "alphavantage",
    provider_symbol: "VWCE.DEX",
};

/// Bitcoin, from Kraken's BTC/EUR market (Kraken has no PLN markets).
pub const BTC: Instrument = Instrument {
    symbol: "BTC",
    name: "Bitcoin",
    class: AssetClass::Crypto,
    currency: "EUR",
    provider: "kraken",
    provider_symbol: "XXBTZEUR",
};

/// Ether, from Kraken's ETH/EUR market.
pub const ETH: Instrument = Instrument {
    symbol: "ETH",
    name: "Ether",
    class: AssetClass::Crypto,
    currency: "EUR",
    provider: "kraken",
    provider_symbol: "XETHZEUR",
};

/// Longest stretch without a close that is still a market holiday rather than missing data
/// (Xetra over Christmas and New Year closes for at most a few days in a row).
pub const MAX_GAP_DAYS: u64 = 10;

#[derive(Debug)]
pub enum PriceError {
    Db(sqlx::Error),
    /// The price provider failed (network, API error, unexpected response).
    Provider(Box<dyn std::error::Error + Send + Sync>),
    /// The instrument is priced by a different provider than the one passed in.
    WrongProvider {
        instrument: &'static str,
        expected: &'static str,
        got: &'static str,
    },
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
            PriceError::WrongProvider {
                instrument,
                expected,
                got,
            } => write!(f, "{instrument} is priced by {expected}, not {got}"),
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

impl From<KrakenError> for PriceError {
    fn from(e: KrakenError) -> Self {
        PriceError::Provider(Box::new(e))
    }
}

impl From<AlphaVantageError> for PriceError {
    fn from(e: AlphaVantageError) -> Self {
        PriceError::Provider(Box::new(e))
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

/// Whether the next stored close after `day` lies more than `MAX_GAP_DAYS` after `earlier`, the
/// close that would answer `day`. Then the two closes came from separate fetches (the app was not
/// run for longer than the provider's window), and the days between them were never fetched:
/// they are missing data, not a market holiday.
async fn gap_after(
    pool: &SqlitePool,
    asset_id: i64,
    day: NaiveDate,
    earlier: NaiveDate,
) -> Result<bool, sqlx::Error> {
    let bridged = cache::closes_between(
        pool,
        asset_id,
        day + Days::new(1),
        earlier + Days::new(MAX_GAP_DAYS),
    )
    .await?;
    Ok(bridged.is_empty() && cache::has_close_after(pool, asset_id, day).await?)
}

/// The close that applies on `day`: that day's, or the last trading day's before it.
///
/// Answered from the cache when it can be: an exact close, or an earlier one when a close within
/// `MAX_GAP_DAYS` after it is stored too (so the gap is known to be a market holiday). Otherwise
/// the provider's recent series is fetched and cached, at most once per the provider's
/// `MIN_REFETCH_INTERVAL`. A past day the recent series does not answer is fetched as a range
/// around it when the provider serves past ranges (NBP gold). An earlier close is never used
/// across a longer gap in the cache: the day is then [`PriceError::NoPrice`] (for VWCE that
/// triggers the weekly backfill).
pub async fn close_on_day<S: DailySource>(
    pool: &SqlitePool,
    provider: &S,
    instrument: &Instrument,
    day: NaiveDate,
    clock: Clock,
) -> Result<PriceQuote, PriceError> {
    if instrument.provider != S::NAME {
        return Err(PriceError::WrongProvider {
            instrument: instrument.symbol,
            expected: instrument.provider,
            got: S::NAME,
        });
    }
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

    if let Some(quote) = answer_from_cache_only(pool, asset_id, day).await? {
        return Ok(quote);
    }

    refresh_daily(pool, provider, instrument, asset_id, clock).await?;
    if let Some(quote) = answer_from_cache(pool, asset_id, day).await? {
        return Ok(quote);
    }
    if day < clock.today
        && backfill_around(pool, provider, instrument, asset_id, day, clock).await?
        && let Some(quote) = answer_from_cache(pool, asset_id, day).await?
    {
        return Ok(quote);
    }
    Err(PriceError::NoPrice(instrument.symbol, day))
}

/// What [`close_on_day`] answers without asking the provider: the stored close for exactly
/// `day`, or an earlier one when a close after `day` is stored too and there is no gap between.
async fn answer_from_cache_only(
    pool: &SqlitePool,
    asset_id: i64,
    day: NaiveDate,
) -> Result<Option<PriceQuote>, sqlx::Error> {
    if let Some(quote) = cache::quote_for_date(pool, asset_id, day).await? {
        return Ok(Some(quote));
    }
    if cache::has_close_after(pool, asset_id, day).await?
        && let Some(quote) = cached_close(pool, asset_id, day).await?
        && !gap_after(pool, asset_id, day, quote.date).await?
    {
        return Ok(Some(quote));
    }
    Ok(None)
}

/// The close [`close_on_day`] would answer for `day` from the cache alone, with no request to
/// any provider: for when the provider is known to be down. None when only a fetch could tell.
pub async fn cached_close_on_day(
    pool: &SqlitePool,
    instrument: &Instrument,
    day: NaiveDate,
) -> Result<Option<PriceQuote>, PriceError> {
    let asset_id = cache::ensure_asset(
        pool,
        instrument.symbol,
        instrument.name,
        instrument.class,
        instrument.currency,
    )
    .await?;
    Ok(answer_from_cache_only(pool, asset_id, day).await?)
}

/// The stored close that answers `day`, unless the cache has a gap after it (see [`gap_after`]).
async fn answer_from_cache(
    pool: &SqlitePool,
    asset_id: i64,
    day: NaiveDate,
) -> Result<Option<PriceQuote>, sqlx::Error> {
    match cached_close(pool, asset_id, day).await? {
        Some(quote) if !gap_after(pool, asset_id, day, quote.date).await? => Ok(Some(quote)),
        _ => Ok(None),
    }
}

/// Days fetched on each side of a past day outside the recent window. 2 x 180 + 1 days stays
/// within NBP's 367-day limit per request, and a series of days in either order costs about
/// one request per half year.
pub const BACKFILL_DAYS_EACH_SIDE: u64 = 180;

/// For a provider that serves past ranges ([`DailySource::closes_between`]), stores the closes
/// within [`BACKFILL_DAYS_EACH_SIDE`] of `day` (up to today) that are not stored yet. False when
/// the provider has no such endpoint.
///
/// They are the provider's own final quotes, stored under its name. That counts as a fetch for
/// [`refresh_daily`]'s throttle, which [`close_on_day`] has just run, so the next refresh of
/// recent closes moves by less than `MIN_REFETCH_INTERVAL`.
async fn backfill_around<S: DailySource>(
    pool: &SqlitePool,
    provider: &S,
    instrument: &Instrument,
    asset_id: i64,
    day: NaiveDate,
    clock: Clock,
) -> Result<bool, PriceError> {
    let from = day - Days::new(BACKFILL_DAYS_EACH_SIDE);
    let to = (day + Days::new(BACKFILL_DAYS_EACH_SIDE)).min(clock.today);
    let Some(closes) = provider
        .closes_between(instrument.provider_symbol, from, to)
        .await
        .map_err(|e| PriceError::Provider(Box::new(e)))?
    else {
        return Ok(false);
    };
    cache::insert_missing_closes(
        pool,
        asset_id,
        instrument.currency,
        S::NAME,
        &closes,
        clock.now,
    )
    .await?;
    Ok(true)
}

/// A stored close that stands in for today's because the provider failed to give a fresh one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastKnown {
    /// The provider that failed (its [`DailySource::NAME`]).
    pub provider: &'static str,
    /// When the stored closes were last fetched; None if never recorded.
    pub fetched_at: Option<DateTime<Utc>>,
    /// The provider's error.
    pub error: String,
}

/// [`close_on_day`], except that for today a provider failure falls back to the latest stored
/// close within `MAX_GAP_DAYS`, reported as [`LastKnown`]: the portfolio keeps a value while a
/// source is down, marked as stale. Other days, and other errors, are as in [`close_on_day`]
/// (history is never filled with an older close).
pub async fn close_or_last_known<S: DailySource>(
    pool: &SqlitePool,
    provider: &S,
    instrument: &Instrument,
    day: NaiveDate,
    clock: Clock,
) -> Result<(PriceQuote, Option<LastKnown>), PriceError> {
    match close_on_day(pool, provider, instrument, day, clock).await {
        Ok(quote) => Ok((quote, None)),
        Err(PriceError::Provider(e)) if day == clock.today => {
            let asset_id = cache::ensure_asset(
                pool,
                instrument.symbol,
                instrument.name,
                instrument.class,
                instrument.currency,
            )
            .await?;
            match cached_close(pool, asset_id, day).await? {
                Some(quote) => {
                    let fetched_at = cache::last_fetched_at(pool, asset_id, S::NAME).await?;
                    let last = LastKnown {
                        provider: S::NAME,
                        fetched_at,
                        error: e.to_string(),
                    };
                    Ok((quote, Some(last)))
                }
                None => Err(PriceError::Provider(e)),
            }
        }
        Err(e) => Err(e),
    }
}

/// Fetches and caches the provider's recent daily series unless it was fetched within the
/// provider's `MIN_REFETCH_INTERVAL`.
async fn refresh_daily<S: DailySource>(
    pool: &SqlitePool,
    provider: &S,
    instrument: &Instrument,
    asset_id: i64,
    clock: Clock,
) -> Result<(), PriceError> {
    let recently_fetched = cache::last_fetched_at(pool, asset_id, S::NAME)
        .await?
        .is_some_and(|t| clock.now - t < S::MIN_REFETCH_INTERVAL);
    if recently_fetched {
        return Ok(());
    }
    let closes = provider
        .daily_closes(instrument.provider_symbol)
        .await
        .map_err(|e| PriceError::Provider(Box::new(e)))?;
    cache::upsert_closes(
        pool,
        asset_id,
        instrument.currency,
        S::NAME,
        &closes,
        clock.now,
    )
    .await?;
    Ok(())
}

/// `prices.source` of week-end closes from Alpha Vantage's weekly series.
pub const ALPHAVANTAGE_WEEKLY_SOURCE: &str = "alphavantage_weekly";

/// What [`backfill_weekly`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeeklyBackfill {
    /// Completed weeks in the provider's weekly series.
    pub weeks: usize,
    /// Week-end closes stored for days that had no close yet.
    pub inserted: u64,
}

/// Stores week-end closes for the instrument's whole history, so that [`close_on_day`] can
/// answer days older than the free daily window (about 100 trading days).
///
/// The daily series is refreshed first (unless fetched recently), so every week-end inside the
/// daily window already has its daily close; weekly closes only fill days without a stored
/// close and never replace one. A later daily fetch does replace a weekly close for the same day.
///
/// A week-end close is the close of that week's last trading day, so for a day inside a
/// backfilled week, [`close_on_day`] answers with the previous week-end's close (at most a week
/// back, within `MAX_GAP_DAYS`). Costs two Alpha Vantage requests (one when the daily series
/// is fresh).
pub async fn backfill_weekly(
    pool: &SqlitePool,
    av: &AlphaVantageClient,
    instrument: &Instrument,
    clock: Clock,
) -> Result<WeeklyBackfill, PriceError> {
    if instrument.provider != AlphaVantageClient::NAME {
        return Err(PriceError::WrongProvider {
            instrument: instrument.symbol,
            expected: instrument.provider,
            got: AlphaVantageClient::NAME,
        });
    }
    let asset_id = cache::ensure_asset(
        pool,
        instrument.symbol,
        instrument.name,
        instrument.class,
        instrument.currency,
    )
    .await?;

    refresh_daily(pool, av, instrument, asset_id, clock).await?;
    let weekly = av.weekly_closes(instrument.provider_symbol, clock).await?;
    let inserted = cache::insert_missing_closes(
        pool,
        asset_id,
        instrument.currency,
        ALPHAVANTAGE_WEEKLY_SOURCE,
        &weekly,
        clock.now,
    )
    .await?;
    Ok(WeeklyBackfill {
        weeks: weekly.len(),
        inserted,
    })
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
pub async fn value_in_pln<S: DailySource>(
    pool: &SqlitePool,
    provider: &S,
    nbp: &NbpClient,
    instrument: &Instrument,
    day: NaiveDate,
    clock: Clock,
) -> Result<Valuation, PriceError> {
    let quote = close_on_day(pool, provider, instrument, day, clock).await?;
    quote_in_pln(pool, nbp, quote, clock).await
}

/// [`value_in_pln`] on [`close_or_last_known`]: today's value from the last stored close while
/// the provider is down. The NBP rate is still the close's own day's.
pub async fn value_in_pln_or_last_known<S: DailySource>(
    pool: &SqlitePool,
    provider: &S,
    nbp: &NbpClient,
    instrument: &Instrument,
    day: NaiveDate,
    clock: Clock,
) -> Result<(Valuation, Option<LastKnown>), PriceError> {
    let (quote, last) = close_or_last_known(pool, provider, instrument, day, clock).await?;
    Ok((quote_in_pln(pool, nbp, quote, clock).await?, last))
}

async fn quote_in_pln(
    pool: &SqlitePool,
    nbp: &NbpClient,
    quote: PriceQuote,
    clock: Clock,
) -> Result<Valuation, PriceError> {
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

/// A live price (last trade) converted to PLN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotValuation {
    pub price: Decimal,
    pub currency: String,
    /// When the price was read.
    pub at: DateTime<Utc>,
    /// NBP rate that applies today (yesterday's table until today's is published).
    pub fx: FxRate,
    pub price_pln: Decimal,
}

/// Live values of Kraken-priced instruments in PLN, from one Ticker request.
pub async fn kraken_spot_in_pln(
    pool: &SqlitePool,
    kraken: &KrakenClient,
    nbp: &NbpClient,
    instruments: &[Instrument],
    clock: Clock,
) -> Result<Vec<SpotValuation>, PriceError> {
    if let Some(other) = instruments
        .iter()
        .find(|i| i.provider != KrakenClient::NAME)
    {
        return Err(PriceError::WrongProvider {
            instrument: other.symbol,
            expected: other.provider,
            got: KrakenClient::NAME,
        });
    }
    let pairs: Vec<&str> = instruments.iter().map(|i| i.provider_symbol).collect();
    let prices = kraken.last_prices(&pairs).await?;
    let mut rates: HashMap<&str, FxRate> = HashMap::new();
    let mut valuations = Vec::with_capacity(instruments.len());
    for instrument in instruments {
        let price = prices[instrument.provider_symbol];
        let fx = match rates.get(instrument.currency) {
            Some(rate) => rate.clone(),
            None => {
                let rate =
                    rate_on_day(pool, nbp, instrument.currency, clock.today, clock.today).await?;
                rates.insert(instrument.currency, rate.clone());
                rate
            }
        };
        let quote = PriceQuote {
            date: clock.today,
            close: price,
            currency: instrument.currency.to_owned(),
        };
        valuations.push(SpotValuation {
            price,
            currency: instrument.currency.to_owned(),
            at: clock.now,
            price_pln: quote.to_pln(&fx)?,
            fx,
        });
    }
    Ok(valuations)
}
