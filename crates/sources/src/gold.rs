//! Gold price per gram from NBP, cached like any other daily price, and coin valuation.

use chrono::NaiveDate;
use portfolio_core::AssetClass;
use portfolio_core::gold::{CoinValuation, GoldError, value_coin};
use portfolio_core::price::PriceQuote;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::nbp::{GOLD_SYMBOL, NbpClient};
use crate::prices::{Clock, Instrument, PriceError, close_on_day};

/// NBP's gold price: PLN per gram of fine gold (fineness 1000), one quote per business day.
pub const GOLD_GRAM: Instrument = Instrument {
    symbol: "XAU_G",
    name: "Gold, PLN per gram (NBP)",
    class: AssetClass::Gold,
    currency: "PLN",
    provider: "nbp",
    provider_symbol: GOLD_SYMBOL,
};

#[derive(Debug)]
pub enum GoldValuationError {
    Price(PriceError),
    Gold(GoldError),
}

impl std::fmt::Display for GoldValuationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoldValuationError::Price(e) => write!(f, "{e}"),
            GoldValuationError::Gold(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GoldValuationError {}

impl From<PriceError> for GoldValuationError {
    fn from(e: PriceError) -> Self {
        GoldValuationError::Price(e)
    }
}

impl From<GoldError> for GoldValuationError {
    fn from(e: GoldError) -> Self {
        GoldValuationError::Gold(e)
    }
}

/// A coin's value on a day, with the NBP quote it is based on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldCoinValue {
    /// NBP price per gram that applies on the day (the last business day's on weekends).
    pub price_per_gram: PriceQuote,
    pub valuation: CoinValuation,
}

/// Values a coin of `fine_weight_grams` on `day`: NBP price per gram (cached in `prices`) x
/// weight x `buyback_factor`, or the `manual_quote_pln` when one is given.
pub async fn coin_value_on_day(
    pool: &SqlitePool,
    nbp: &NbpClient,
    day: NaiveDate,
    clock: Clock,
    fine_weight_grams: Decimal,
    buyback_factor: Decimal,
    manual_quote_pln: Option<Decimal>,
) -> Result<GoldCoinValue, GoldValuationError> {
    let price = close_on_day(pool, nbp, &GOLD_GRAM, day, clock).await?;
    let valuation = value_coin(
        price.close,
        fine_weight_grams,
        buyback_factor,
        manual_quote_pln,
    )?;
    Ok(GoldCoinValue {
        price_per_gram: price,
        valuation,
    })
}
