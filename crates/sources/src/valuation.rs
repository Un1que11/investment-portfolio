//! The whole portfolio on a date: holdings, net contributions and value in PLN.
//!
//! Three steps, so the arithmetic stays testable without the network:
//! 1. [`requirements`] lists the prices and rates the ledger needs for the date (pure);
//! 2. [`fetch_inputs`] gets them from the live sources (NBP, Alpha Vantage, Kraken, gold);
//! 3. [`report`] combines ledger and inputs (pure, `portfolio_core::portfolio`).

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use portfolio_core::portfolio::{
    Contributions, Holdings, LedgerTx, MissingRate, Valuation, holdings_on, net_contributions_on,
    value,
};
use portfolio_db::gold as coins;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::alphavantage::AlphaVantageClient;
use crate::fx::rate_on_day;
use crate::gold::coin_value_on_day;
use crate::kraken::KrakenClient;
use crate::nbp::NbpClient;
use crate::prices::{BTC, Clock, ETH, Instrument, VWCE, value_in_pln};

/// Asset symbol of physical gold coins (valued coin by coin).
pub const GOLD_COINS: &str = portfolio_db::gold::ASSET_SYMBOL;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requirements {
    /// Assets held on the date and their quantities (each needs a position value).
    pub assets: BTreeMap<String, Decimal>,
    /// Non-PLN cash currencies held on the date (each needs a rate on the date).
    pub cash_currencies: BTreeSet<String>,
    /// (currency, date) of every non-PLN contribution flow up to the date.
    pub flow_rates: BTreeSet<(String, NaiveDate)>,
}

pub fn requirements(ledger: &[LedgerTx], date: NaiveDate) -> Requirements {
    let holdings = holdings_on(ledger, date);
    let mut flow_rates = BTreeSet::new();
    // Ask the contribution calculation itself which rates it needs.
    let _ = net_contributions_on(ledger, date, |currency, day| {
        flow_rates.insert((currency.to_owned(), day));
        Some(Decimal::ONE)
    });
    Requirements {
        assets: holdings.assets.clone(),
        cash_currencies: holdings
            .cash
            .keys()
            .map(|(_, c)| c.clone())
            .filter(|c| c != "PLN")
            .collect(),
        flow_rates,
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
    /// Why values are missing, for the report.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioReport {
    pub date: NaiveDate,
    pub holdings: Holdings,
    pub contributions: Contributions,
    pub valuation: Valuation,
    pub problems: Vec<String>,
}

pub fn report(
    ledger: &[LedgerTx],
    date: NaiveDate,
    inputs: &Inputs,
) -> Result<PortfolioReport, MissingRate> {
    let holdings = holdings_on(ledger, date);
    let contributions = net_contributions_on(ledger, date, |currency, day| {
        inputs
            .flow_rates
            .get(&(currency.to_owned(), day))
            .copied()
            .flatten()
    })?;
    let valuation = value(
        &holdings,
        |asset, _| inputs.position_values.get(asset).copied().flatten(),
        |currency| inputs.cash_rates.get(currency).copied().flatten(),
    );
    Ok(PortfolioReport {
        date,
        holdings,
        contributions,
        valuation,
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
    inputs
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
