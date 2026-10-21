//! External market data sources. Every request is a read-only GET to a public endpoint.

use chrono::{NaiveDate, TimeDelta};
use portfolio_core::price::DailyClose;

pub mod alphavantage;
pub mod fx;
pub mod gold;
pub mod history;
pub mod kraken;
pub mod kraken_ledger;
pub mod kraken_private;
pub mod nbp;
pub mod periods;
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

    /// Final daily closes with `from <= date <= to`, oldest first, from a provider that serves
    /// any past range (`to` is never in the future). None, the default, when it serves only
    /// the recent window of [`Self::daily_closes`].
    fn closes_between(
        &self,
        _symbol: &str,
        _from: NaiveDate,
        _to: NaiveDate,
    ) -> impl Future<Output = Result<Option<Vec<DailyClose>>, Self::Error>> + Send {
        std::future::ready(Ok(None))
    }
}
