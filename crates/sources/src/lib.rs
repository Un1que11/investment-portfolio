//! External market data sources. Every request is a read-only GET to a public endpoint.

use chrono::TimeDelta;
use portfolio_core::price::DailyClose;

pub mod alphavantage;
pub mod fx;
pub mod gold;
pub mod kraken;
pub mod kraken_ledger;
pub mod kraken_private;
pub mod nbp;
pub mod prices;
pub mod valuation;

/// A provider of daily closing prices, used by the cached lookup in [`prices`].
pub trait DailySource {
    /// Value stored in `prices.source`.
    const NAME: &'static str;
    /// Never refetch the same instrument more often than this.
    const MIN_REFETCH_INTERVAL: TimeDelta;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Recent final daily closes for the provider's symbol, oldest first.
    fn daily_closes(
        &self,
        symbol: &str,
    ) -> impl Future<Output = Result<Vec<DailyClose>, Self::Error>> + Send;
}
