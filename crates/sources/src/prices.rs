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
/// series is fetched and cached, at most once per the provider's `MIN_REFETCH_INTERVAL`.
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
        .is_some_and(|t| clock.now - t < S::MIN_REFETCH_INTERVAL);
    if !recently_fetched {
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
pub async fn value_in_pln<S: DailySource>(
    pool: &SqlitePool,
    provider: &S,
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
