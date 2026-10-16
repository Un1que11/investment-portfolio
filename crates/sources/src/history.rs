//! The home page chart's series: value and net contributions on each chart day
//! ([`portfolio_core::history::chart_dates`]), from the same sources and caches as the report.
//!
//! Every price and rate goes through the database caches, so once a series has been drawn the
//! next one needs no request for past days. A provider that fails is not asked again for the
//! rest of the series (each day would otherwise retry it: slow, and for Alpha Vantage a waste of
//! the daily quota); its days have no value. Run it after the report for today, which fetches
//! the VWCE weekly history when contributions are older than the daily closes.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use portfolio_core::history::{ValuePoint, value_point};
use portfolio_core::portfolio::{LedgerTx, holdings_on, net_contributions_on};
use portfolio_db::gold::{self as coins, Coin};
use rust_decimal::Decimal;

use crate::fx::{FxError, rate_on_day};
use crate::gold::{GoldValuationError, coin_value_on_day};
use crate::prices::{PriceError, VWCE, value_in_pln};
use crate::valuation::{GOLD_COINS, Sources, instrument, vwce_prices};

const NBP: &str = "nbp";
const KRAKEN: &str = "kraken";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValueSeries {
    /// One point per day asked for, in the same order.
    pub points: Vec<ValuePoint>,
    /// Why some points have no value, for the page.
    pub problems: Vec<String>,
}

/// Values the ledger on each of `dates` (ascending).
pub async fn value_series(
    ledger: &[LedgerTx],
    dates: &[NaiveDate],
    s: &Sources<'_>,
) -> ValueSeries {
    let mut problems = Vec::new();
    let holdings: Vec<_> = dates.iter().map(|d| holdings_on(ledger, *d)).collect();

    let vwce_days: BTreeSet<NaiveDate> = dates
        .iter()
        .zip(&holdings)
        .filter(|(_, h)| h.assets.contains_key(VWCE.symbol))
        .map(|(d, _)| *d)
        .collect();
    let (vwce, vwce_problem) = vwce_prices(&vwce_days, s).await;
    if let Some(p) = vwce_problem {
        problems.push(format!("chart, VWCE: {p}"));
    }
    let gold: Option<Vec<Coin>> = if holdings.iter().any(|h| h.assets.contains_key(GOLD_COINS)) {
        match coins::list_coins(s.pool).await {
            Ok(c) => Some(c),
            Err(e) => {
                problems.push(format!("chart, gold coins: {e}"));
                None
            }
        }
    } else {
        None
    };

    // Every flow rate any point needs: the last day's contributions include all earlier ones.
    let mut wanted: BTreeSet<(String, NaiveDate)> = BTreeSet::new();
    if let Some(last) = dates.last() {
        let _ = net_contributions_on(ledger, *last, |currency, day| {
            wanted.insert((currency.to_owned(), day));
            Some(Decimal::ONE)
        });
    }
    for (date, h) in dates.iter().zip(&holdings) {
        for (_, currency) in h.cash.keys().filter(|(_, c)| c != "PLN") {
            wanted.insert((currency.clone(), *date));
        }
    }

    let mut down: BTreeMap<&'static str, String> = BTreeMap::new();
    let mut rates: BTreeMap<(String, NaiveDate), Option<Decimal>> = BTreeMap::new();
    for (currency, day) in wanted {
        let rate = if down.contains_key(NBP) {
            None
        } else {
            match rate_on_day(s.pool, s.nbp, &currency, day, s.clock.today).await {
                Ok(r) => Some(r.mid),
                Err(e) => {
                    if matches!(e, FxError::Nbp(_)) {
                        down.insert(NBP, e.to_string());
                    }
                    None
                }
            }
        };
        rates.insert((currency, day), rate);
    }

    let mut points = Vec::with_capacity(dates.len());
    for (date, h) in dates.iter().zip(&holdings) {
        let mut positions: BTreeMap<&str, Option<Decimal>> = BTreeMap::new();
        for (asset, quantity) in &h.assets {
            let v = if asset == VWCE.symbol {
                vwce.get(date).copied().flatten().map(|p| p * quantity)
            } else if asset == GOLD_COINS {
                match &gold {
                    Some(c) => gold_on(c, *date, s, &mut down).await,
                    None => None,
                }
            } else {
                kraken_on(asset, *quantity, *date, s, &mut down).await
            };
            positions.insert(asset, v);
        }
        points.push(value_point(
            ledger,
            *date,
            |currency, day| rates.get(&(currency.to_owned(), day)).copied().flatten(),
            |asset, _| positions.get(asset).copied().flatten(),
            |currency| rates.get(&(currency.to_owned(), *date)).copied().flatten(),
        ));
    }

    for (provider, e) in down {
        problems.push(format!(
            "chart: {provider} failed ({e}); it was not asked again for later days"
        ));
    }
    let incomplete: Vec<&ValuePoint> = points
        .iter()
        .filter(|p| p.value.is_none() || p.contributions.is_none())
        .collect();
    if let Some(first) = incomplete.first() {
        problems.push(format!(
            "chart: {} of {} days have no complete value (first {}: {})",
            incomplete.len(),
            points.len(),
            first.date,
            first.missing.join(", ")
        ));
    }
    ValueSeries { points, problems }
}

/// Every coin bought by `date`, valued on it.
async fn gold_on(
    held: &[Coin],
    date: NaiveDate,
    s: &Sources<'_>,
    down: &mut BTreeMap<&'static str, String>,
) -> Option<Decimal> {
    let mut total = Decimal::ZERO;
    for coin in held.iter().filter(|c| c.purchased_on <= date) {
        if down.contains_key(NBP) {
            return None;
        }
        match coin_value_on_day(
            s.pool,
            s.nbp,
            date,
            s.clock,
            coin.fine_weight_g,
            s.gold_buyback_factor,
            coin.manual_quote_pln,
        )
        .await
        {
            Ok(v) => total += v.valuation.value_pln,
            Err(GoldValuationError::Price(e)) => {
                note_failure(&e, NBP, down);
                return None;
            }
            Err(GoldValuationError::Gold(_)) => return None,
        }
    }
    Some(total)
}

/// A Kraken-priced position on `date`.
async fn kraken_on(
    asset: &str,
    quantity: Decimal,
    date: NaiveDate,
    s: &Sources<'_>,
    down: &mut BTreeMap<&'static str, String>,
) -> Option<Decimal> {
    let i = instrument(asset).filter(|i| i.provider == KRAKEN)?;
    if down.contains_key(KRAKEN) || down.contains_key(NBP) {
        return None;
    }
    match value_in_pln(s.pool, s.kraken, s.nbp, &i, date, s.clock).await {
        Ok(v) => Some(v.close_pln * quantity),
        Err(e) => {
            note_failure(&e, KRAKEN, down);
            None
        }
    }
}

/// Marks the provider down when the error is the provider's (or NBP's, for the rate).
fn note_failure(e: &PriceError, provider: &'static str, down: &mut BTreeMap<&'static str, String>) {
    match e {
        PriceError::Provider(_) => {
            down.insert(provider, e.to_string());
        }
        PriceError::Fx(FxError::Nbp(_)) => {
            down.insert(NBP, e.to_string());
        }
        _ => {}
    }
}
