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
use portfolio_core::benchmark::{benchmark_series, price_days};
use portfolio_core::gold::value_coin;
use portfolio_core::history::{ValuePoint, chart_dates, value_point};
use portfolio_core::period::{Period, period_flows};
use portfolio_core::portfolio::{LedgerTx, holdings_on, net_contributions_on};
use portfolio_db::gold::{self as coins, Coin};
use rust_decimal::Decimal;

use crate::fx::{FxError, rate_on_day};
use crate::gold::GOLD_GRAM;
use crate::prices::{PriceError, VWCE, close_or_last_known, value_in_pln_or_last_known};
use crate::valuation::{GOLD_COINS, PortfolioReport, Sources, instrument, vwce_prices};

const NBP: &str = "nbp";
const KRAKEN: &str = "kraken";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValueSeries {
    /// One point per day asked for, in the same order.
    pub points: Vec<ValuePoint>,
    /// Each asset's value on the days it was held (quantity not zero), ascending; None on a day
    /// it had no price. For the asset cards' sparklines.
    pub assets: BTreeMap<String, Vec<(NaiveDate, Option<Decimal>)>>,
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
    let mut assets: BTreeMap<String, Vec<(NaiveDate, Option<Decimal>)>> = BTreeMap::new();
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
            assets.entry(asset.clone()).or_default().push((*date, v));
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
    ValueSeries {
        points,
        assets,
        problems,
    }
}

/// One day of the Overview chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartPoint {
    pub date: NaiveDate,
    /// Net contributions since the very start (not just the period's).
    pub contributions: Option<Decimal>,
    pub value: Option<Decimal>,
    /// What everything put in over the period would be worth in VWCE that day.
    pub benchmark: Option<Decimal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeriodChart {
    pub points: Vec<ChartPoint>,
    /// Each asset's value over the period ([`ValueSeries::assets`]).
    pub assets: BTreeMap<String, Vec<(NaiveDate, Option<Decimal>)>>,
    /// Why some points have no value or benchmark, for the page.
    pub problems: Vec<String>,
}

/// The Overview chart of `period`, ending on the report's date: the period's start day (the
/// first contribution for All), the end of every month after it and the end day
/// ([`chart_dates`]). The benchmark replays the period's flows ([`period_flows`]: the start
/// value, then the contributions) into VWCE, so it starts at the value line, as in the design.
/// Empty when nothing was ever put in.
pub async fn period_chart(
    ledger: &[LedgerTx],
    report: &PortfolioReport,
    period: Period,
    s: &Sources<'_>,
) -> PeriodChart {
    let end = report.date;
    let Some(first) = report.contribution_flows.iter().map(|f| f.date).min() else {
        return PeriodChart::default();
    };
    let dates = chart_dates(period.start(end).unwrap_or(first), end);
    let series = value_series(ledger, &dates, s).await;
    let mut problems = series.problems;
    let assets = series.assets;

    let start_value = series.points.first().and_then(|p| p.value);
    let benchmark: Vec<Option<Decimal>> =
        match period_flows(period, end, &report.contribution_flows, start_value) {
            Ok(flows) => {
                // Only days with units held need a price: asking for one older than the daily
                // closes would spend Alpha Vantage requests on the weekly history for nothing.
                let mut days = price_days(&flows, end);
                if let Some(first_flow) = flows.iter().map(|f| f.date).min() {
                    days.extend(dates.iter().filter(|d| **d >= first_flow));
                }
                let (vwce, problem) = vwce_prices(&days, s).await;
                if let Some(p) = problem {
                    problems.push(format!("chart, VWCE benchmark: {p}"));
                }
                benchmark_series(&flows, &dates, |d| vwce.get(&d).copied().flatten())
                    .into_iter()
                    .map(Result::ok)
                    .collect()
            }
            Err(e) => {
                problems.push(format!("chart, VWCE benchmark: {e}"));
                vec![None; dates.len()]
            }
        };

    let points = series
        .points
        .into_iter()
        .zip(benchmark)
        .map(|(p, benchmark)| ChartPoint {
            date: p.date,
            contributions: p.contributions,
            value: p.value,
            benchmark,
        })
        .collect();
    PeriodChart {
        points,
        assets,
        problems,
    }
}

/// Every coin bought by `date`, valued on it.
async fn gold_on(
    held: &[Coin],
    date: NaiveDate,
    s: &Sources<'_>,
    down: &mut BTreeMap<&'static str, String>,
) -> Option<Decimal> {
    let coins: Vec<&Coin> = held.iter().filter(|c| c.purchased_on <= date).collect();
    if coins.is_empty() {
        return Some(Decimal::ZERO);
    }
    if down.contains_key(NBP) {
        return None;
    }
    // Today falls back to the last stored price while NBP is down, as the report does.
    let price = match close_or_last_known(s.pool, s.nbp, &GOLD_GRAM, date, s.clock).await {
        Ok((quote, _)) => quote.close,
        Err(e) => {
            note_failure(&e, NBP, down);
            return None;
        }
    };
    let mut total = Decimal::ZERO;
    for coin in coins {
        let v = value_coin(
            price,
            coin.fine_weight_g,
            s.gold_buyback_factor,
            coin.manual_quote_pln,
        )
        .ok()?;
        total = total.checked_add(v.value_pln)?;
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
    match value_in_pln_or_last_known(s.pool, s.kraken, s.nbp, &i, date, s.clock).await {
        Ok((v, _)) => v.close_pln.checked_mul(quantity),
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
