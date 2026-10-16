//! The whole portfolio on a date: holdings, net contributions, value, P&L, each asset's
//! contribution to it, XIRR, and what the same contributions would be worth in VWCE alone, in PLN;
//! and, given the config, how far it is from the targets and where the year's IKZE contributions
//! stand against the limit.
//!
//! Three steps, so the arithmetic stays testable without the network:
//! 1. [`requirements`] lists the prices and rates the ledger needs for the date (pure);
//! 2. [`fetch_inputs`] gets them from the live sources (NBP, Alpha Vantage, Kraken, gold);
//! 3. [`report`] combines ledger and inputs (pure, `portfolio_core::portfolio`).

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use portfolio_core::allocation::{Allocation, AllocationError, Sleeve, allocation, sleeve_values};
use portfolio_core::attribution::{ProfitAttribution, profit_attribution};
use portfolio_core::benchmark::{Benchmark, BenchmarkError, price_days, vwce_benchmark};
use portfolio_core::config::Config;
use portfolio_core::ikze::{IkzeError, IkzeFlows, IkzeYear, ikze_flows, ikze_year};
use portfolio_core::pnl::{PortfolioPnl, asset_flows, portfolio_pnl};
use portfolio_core::portfolio::{
    Contributions, Holdings, LedgerTx, MissingRate, Valuation, holdings_on, net_contributions_on,
    value,
};
use portfolio_core::topup::{TopUp, TopUpError, top_up};
use portfolio_core::xirr::{PortfolioXirr, contribution_flows, portfolio_xirr};
use portfolio_db::gold as coins;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::alphavantage::AlphaVantageClient;
use crate::fx::rate_on_day;
use crate::gold::coin_value_on_day;
use crate::kraken::KrakenClient;
use crate::nbp::NbpClient;
use crate::prices::{BTC, Clock, ETH, Instrument, PriceError, VWCE, backfill_weekly, value_in_pln};

/// Asset symbol of physical gold coins (valued coin by coin).
pub const GOLD_COINS: &str = portfolio_db::gold::ASSET_SYMBOL;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requirements {
    /// Assets held on the date and their quantities (each needs a position value).
    pub assets: BTreeMap<String, Decimal>,
    /// Non-PLN cash currencies held on the date (each needs a rate on the date).
    pub cash_currencies: BTreeSet<String>,
    /// (currency, date) of every non-PLN flow up to the date that P&L needs: contributions
    /// and asset buys, sells, fees and dividends.
    pub flow_rates: BTreeSet<(String, NaiveDate)>,
    /// Days that need a VWCE price for the benchmark: every contribution's date and the date.
    pub benchmark_days: BTreeSet<NaiveDate>,
}

pub fn requirements(ledger: &[LedgerTx], date: NaiveDate) -> Requirements {
    let holdings = holdings_on(ledger, date);
    let mut flow_rates = BTreeSet::new();
    // Ask the contribution calculation itself which rates it needs.
    let _ = net_contributions_on(ledger, date, |currency, day| {
        flow_rates.insert((currency.to_owned(), day));
        Some(Decimal::ONE)
    });
    let _ = asset_flows(ledger, date, |currency, day| {
        flow_rates.insert((currency.to_owned(), day));
        Some(Decimal::ONE)
    });
    let _ = ikze_flows(ledger, date, |currency, day| {
        flow_rates.insert((currency.to_owned(), day));
        Some(Decimal::ONE)
    });
    // Dates do not depend on rates, so any rate lists them all.
    let contributions = contribution_flows(ledger, date, |_, _| Some(Decimal::ONE))
        .expect("a rate is given for every currency");
    Requirements {
        assets: holdings.assets.clone(),
        cash_currencies: holdings
            .cash
            .keys()
            .map(|(_, c)| c.clone())
            .filter(|c| c != "PLN")
            .collect(),
        flow_rates,
        benchmark_days: price_days(&contributions, date),
    }
}

/// Prices and rates for one date. `None` means the value could not be obtained.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inputs {
    /// Asset -> PLN value of the whole position.
    pub position_values: BTreeMap<String, Option<Decimal>>,
    /// Currency -> PLN per unit on the valuation date.
    pub cash_rates: BTreeMap<String, Option<Decimal>>,
    /// (currency, date) -> PLN per unit, for contribution flows.
    pub flow_rates: BTreeMap<(String, NaiveDate), Option<Decimal>>,
    /// Day -> PLN per VWCE unit, for the benchmark.
    pub vwce_prices: BTreeMap<NaiveDate, Option<Decimal>>,
    /// Why values are missing, for the report.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioReport {
    pub date: NaiveDate,
    pub holdings: Holdings,
    pub contributions: Contributions,
    pub valuation: Valuation,
    pub pnl: PortfolioPnl,
    /// Each asset's share of the P&L, ranked from the most earned to the least.
    pub attribution: ProfitAttribution,
    pub xirr: PortfolioXirr,
    /// IKZE deposits and withdrawals in the report date's calendar year, up to the date.
    pub ikze_flows: IkzeFlows,
    /// The same contributions, each put into VWCE on its own date.
    pub benchmark: Result<Benchmark, BenchmarkError>,
    pub problems: Vec<String>,
}

impl PortfolioReport {
    /// How far the holdings are from the targets in `config` (classes and the crypto split).
    pub fn allocation(&self, config: &Config) -> Result<Allocation, AllocationError> {
        allocation(&self.holdings, &self.valuation, config, sleeve)
    }

    /// How to split `amount` PLN of new money between the assets to get closer to the targets
    /// in `config` without selling.
    pub fn top_up(&self, config: &Config, amount: Decimal) -> Result<TopUp, TopUpError> {
        let values = sleeve_values(&self.holdings, &self.valuation, sleeve)?;
        top_up(&values, config, amount)
    }

    /// The year's IKZE contributions against the limit and the tax rate in `config`.
    pub fn ikze(&self, config: &Config) -> Result<IkzeYear, IkzeError> {
        ikze_year(&self.ikze_flows, config)
    }
}

/// Which target an asset counts towards, by its symbol.
pub fn sleeve(symbol: &str) -> Option<Sleeve> {
    match symbol {
        s if s == VWCE.symbol => Some(Sleeve::Vwce),
        s if s == BTC.symbol => Some(Sleeve::Btc),
        s if s == ETH.symbol => Some(Sleeve::Eth),
        GOLD_COINS => Some(Sleeve::Gold),
        _ => None,
    }
}

pub fn report(
    ledger: &[LedgerTx],
    date: NaiveDate,
    inputs: &Inputs,
) -> Result<PortfolioReport, MissingRate> {
    let holdings = holdings_on(ledger, date);
    let rate = |currency: &str, day: NaiveDate| {
        inputs
            .flow_rates
            .get(&(currency.to_owned(), day))
            .copied()
            .flatten()
    };
    let contributions = net_contributions_on(ledger, date, rate)?;
    let flows = asset_flows(ledger, date, rate)?;
    let valuation = value(
        &holdings,
        |asset, _| inputs.position_values.get(asset).copied().flatten(),
        |currency| inputs.cash_rates.get(currency).copied().flatten(),
    );
    let pnl = portfolio_pnl(flows, &valuation, &contributions);
    let attribution = profit_attribution(&pnl);
    // Needs no rates beyond the P&L's: the same rows, dated instead of summed.
    let xirr = portfolio_xirr(ledger, date, rate, &pnl)?;
    let ikze_flows = ikze_flows(ledger, date, rate)?;
    let benchmark = vwce_benchmark(&contribution_flows(ledger, date, rate)?, date, |day| {
        inputs.vwce_prices.get(&day).copied().flatten()
    });
    Ok(PortfolioReport {
        date,
        holdings,
        contributions,
        valuation,
        pnl,
        attribution,
        xirr,
        ikze_flows,
        benchmark,
        problems: inputs.problems.clone(),
    })
}

/// Live price sources. Without an Alpha Vantage key, VWCE is reported as missing.
pub struct Sources<'a> {
    pub pool: &'a SqlitePool,
    pub nbp: &'a NbpClient,
    pub kraken: &'a KrakenClient,
    pub alphavantage: Option<&'a AlphaVantageClient>,
    pub clock: Clock,
    /// Share of melt value a dealer pays for coins without a manual quote (config, P3).
    pub gold_buyback_factor: Decimal,
}

fn instrument(asset: &str) -> Option<Instrument> {
    [VWCE, BTC, ETH].into_iter().find(|i| i.symbol == asset)
}

pub async fn fetch_inputs(req: &Requirements, date: NaiveDate, s: &Sources<'_>) -> Inputs {
    let mut inputs = Inputs::default();

    for (asset, quantity) in &req.assets {
        let result: Result<Decimal, String> = if asset == GOLD_COINS {
            gold_value(date, s).await
        } else {
            let unit_price = match instrument(asset) {
                Some(i) if i.provider == "alphavantage" => match s.alphavantage {
                    Some(av) => value_in_pln(s.pool, av, s.nbp, &i, date, s.clock)
                        .await
                        .map(|v| v.close_pln)
                        .map_err(|e| e.to_string()),
                    None => Err("no Alpha Vantage key".into()),
                },
                Some(i) => value_in_pln(s.pool, s.kraken, s.nbp, &i, date, s.clock)
                    .await
                    .map(|v| v.close_pln)
                    .map_err(|e| e.to_string()),
                None => Err("no price source for this asset".into()),
            };
            unit_price.map(|p| p * quantity)
        };
        match result {
            Ok(v) => {
                inputs.position_values.insert(asset.clone(), Some(v));
            }
            Err(e) => {
                inputs.problems.push(format!("{asset}: {e}"));
                inputs.position_values.insert(asset.clone(), None);
            }
        }
    }

    for currency in &req.cash_currencies {
        let rate = rate_on_day(s.pool, s.nbp, currency, date, s.clock.today).await;
        if let Err(e) = &rate {
            inputs.problems.push(format!("cash {currency}: {e}"));
        }
        inputs
            .cash_rates
            .insert(currency.clone(), rate.ok().map(|r| r.mid));
    }
    for (currency, day) in &req.flow_rates {
        let rate = rate_on_day(s.pool, s.nbp, currency, *day, s.clock.today).await;
        if let Err(e) = &rate {
            inputs.problems.push(format!("{currency} on {day}: {e}"));
        }
        inputs
            .flow_rates
            .insert((currency.clone(), *day), rate.ok().map(|r| r.mid));
    }
    benchmark_prices(&req.benchmark_days, s, &mut inputs).await;
    inputs
}

/// VWCE in PLN on each benchmark day, from the price cache. Days older than the cached closes
/// (the daily series covers ~100 trading days) make it fetch the weekly history once
/// ([`crate::prices::backfill_weekly`], one or two requests) and try them again; once stored,
/// later reports need no such request. Once Alpha Vantage itself fails (down, rate limit), the
/// remaining days are not asked for: each would retry the fetch and spend another of the ~25
/// requests a day. Failures are summed up in one problem, since there can be many days. Without
/// an Alpha Vantage key the prices are missing and the benchmark says so.
async fn benchmark_prices(days: &BTreeSet<NaiveDate>, s: &Sources<'_>, inputs: &mut Inputs) {
    let Some(av) = s.alphavantage else {
        inputs.vwce_prices.extend(days.iter().map(|d| (*d, None)));
        return;
    };
    let price = |day: NaiveDate| value_in_pln(s.pool, av, s.nbp, &VWCE, day, s.clock);
    let mut errors: BTreeMap<NaiveDate, String> = BTreeMap::new();
    let mut provider_failed = false;
    let mut before_cache = Vec::new();
    for day in days {
        let mut close = None;
        if !provider_failed {
            match price(*day).await {
                Ok(v) => close = Some(v.close_pln),
                Err(e) => {
                    provider_failed = matches!(e, PriceError::Provider(_));
                    if matches!(e, PriceError::NoPrice(..)) {
                        before_cache.push(*day);
                    }
                    errors.insert(*day, e.to_string());
                }
            }
        }
        inputs.vwce_prices.insert(*day, close);
    }
    let mut backfill_failed = None;
    if !before_cache.is_empty() && !provider_failed {
        match backfill_weekly(s.pool, av, &VWCE, s.clock).await {
            Ok(_) => {
                for day in before_cache {
                    match price(day).await {
                        Ok(v) => {
                            inputs.vwce_prices.insert(day, Some(v.close_pln));
                            errors.remove(&day);
                        }
                        Err(e) => {
                            errors.insert(day, e.to_string());
                        }
                    }
                }
            }
            Err(e) => backfill_failed = Some(e.to_string()),
        }
    }
    let failed = inputs.vwce_prices.values().filter(|p| p.is_none()).count();
    if let Some((day, e)) = errors.first_key_value() {
        let backfill = backfill_failed.map_or(String::new(), |e| format!("; weekly history: {e}"));
        inputs.problems.push(format!(
            "VWCE benchmark: no price for {failed} of {} days (first {day}: {e}){backfill}",
            days.len()
        ));
    }
}

async fn gold_value(date: NaiveDate, s: &Sources<'_>) -> Result<Decimal, String> {
    let held = coins::list_coins(s.pool)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|c| c.purchased_on <= date);
    let mut total = Decimal::ZERO;
    for coin in held {
        let v = coin_value_on_day(
            s.pool,
            s.nbp,
            date,
            s.clock,
            coin.fine_weight_g,
            s.gold_buyback_factor,
            coin.manual_quote_pln,
        )
        .await
        .map_err(|e| e.to_string())?;
        total += v.valuation.value_pln;
    }
    Ok(total)
}

/// The full report for `date` from the database and live sources.
pub async fn portfolio_on(date: NaiveDate, s: &Sources<'_>) -> Result<PortfolioReport, String> {
    let ledger = portfolio_db::transactions::load_ledger(s.pool)
        .await
        .map_err(|e| e.to_string())?;
    let inputs = fetch_inputs(&requirements(&ledger, date), date, s).await;
    report(&ledger, date, &inputs).map_err(|e| e.to_string())
}
