//! XIRR: the annual return that accounts for when money went in and out.
//!
//! The rate `r` solves `sum(amount_i / (1 + r)^(days_i / 365)) = 0`, with days counted from the
//! first flow (Actual/365, as Excel's XIRR). Flows are from the investor's side: money put in is
//! negative, money taken out is positive, and the value on the valuation date is a final
//! positive flow.
//!
//! - Portfolio: contributions (deposits, withdrawals, manual purchases and sales; see
//!   [`crate::portfolio`]) and the total value.
//! - Per asset: buys and fees (out), sales and cash dividends (in), and the position's value.
//!   In-kind rows carry no money, so an asset held only from staking rewards has no XIRR.
//!
//! Amounts are summed and converted as `Decimal`; only the root finding runs in `f64`, since a
//! fractional power has no exact decimal form. The rate is rounded to 6 decimal places.
//!
//! Over a short period the annualised rate exaggerates: 1% in a month is about 12.7% a year.

use std::collections::BTreeMap;
use std::fmt;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

use crate::pnl::{AssetFlow, PortfolioPnl, asset_flow};
use crate::portfolio::{LedgerTx, MissingRate, contribution_sign, in_pln};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CashFlow {
    pub date: NaiveDate,
    /// PLN; negative = put in, positive = taken out (or still held).
    pub amount: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XirrError {
    /// A position on the valuation date has no price.
    MissingValue,
    /// Nothing was paid in or nothing came back (e.g. holdings from staking rewards only).
    NoSignChange,
    /// No annual rate between -99.9999% and 100 000 000% fits the flows (or they all fall on
    /// one day).
    NoSolution,
}

impl fmt::Display for XirrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            XirrError::MissingValue => "no price for the value",
            XirrError::NoSignChange => "no money paid in, or none came back",
            XirrError::NoSolution => "no rate fits the flows",
        })
    }
}

impl std::error::Error for XirrError {}

const LOWEST_RATE: f64 = -0.999_999;
const HIGHEST_RATE: f64 = 1_000_000.0;
const DECIMALS: u32 = 6;

/// The annual rate as a fraction (0.1 = 10% a year).
pub fn xirr(flows: &[CashFlow]) -> Result<Decimal, XirrError> {
    let flows: Vec<&CashFlow> = flows.iter().filter(|f| !f.amount.is_zero()).collect();
    let paid_in = flows.iter().any(|f| f.amount < Decimal::ZERO);
    let came_back = flows.iter().any(|f| f.amount > Decimal::ZERO);
    if !(paid_in && came_back) {
        return Err(XirrError::NoSignChange);
    }
    let first = flows
        .iter()
        .map(|f| f.date)
        .min()
        .expect("flows are not empty");
    let points: Vec<(f64, f64)> = flows
        .iter()
        .map(|f| {
            let years = (f.date - first).num_days() as f64 / 365.0;
            (
                years,
                f.amount.to_f64().expect("a Decimal always fits an f64"),
            )
        })
        .collect();
    if points.iter().all(|(years, _)| *years == 0.0) {
        return Err(XirrError::NoSolution);
    }
    let npv = |rate: f64| -> f64 {
        points
            .iter()
            .map(|(years, amount)| amount * (1.0 + rate).powf(-years))
            .sum()
    };

    // Bracket a sign change, then bisect: slower than Newton's method but never diverges.
    let (mut lo, mut hi) = (LOWEST_RATE, 1.0);
    let (f_lo, mut f_hi) = (npv(lo), npv(hi));
    while f_lo.signum() == f_hi.signum() && hi < HIGHEST_RATE {
        hi *= 10.0;
        f_hi = npv(hi);
    }
    if f_lo.is_nan() || f_hi.is_nan() || f_lo.signum() == f_hi.signum() {
        return Err(XirrError::NoSolution);
    }
    let lo_sign = f_lo.signum();
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        let f_mid = npv(mid);
        if f_mid.is_nan() {
            return Err(XirrError::NoSolution);
        }
        if f_mid == 0.0 {
            (lo, hi) = (mid, mid);
            break;
        }
        if f_mid.signum() == lo_sign {
            lo = mid;
        } else {
            hi = mid;
        }
        if hi - lo < 1e-12 {
            break;
        }
    }
    Decimal::from_f64((lo + hi) / 2.0)
        .map(|r| r.round_dp(DECIMALS))
        .ok_or(XirrError::NoSolution)
}

/// Contributions up to the end of `date`, in PLN at each row's date, as flows (put in =
/// negative). The same rows as [`crate::portfolio::net_contributions_on`].
pub fn contribution_flows(
    txs: &[LedgerTx],
    date: NaiveDate,
    mut rate: impl FnMut(&str, NaiveDate) -> Option<Decimal>,
) -> Result<Vec<CashFlow>, MissingRate> {
    let mut out = Vec::new();
    for tx in txs.iter().filter(|t| t.date <= date) {
        if let Some(sign) = contribution_sign(tx) {
            out.push(CashFlow {
                date: tx.date,
                amount: -sign * in_pln(tx, &mut rate)?,
            });
        }
    }
    Ok(out)
}

/// Each asset's money flows up to the end of `date`, in PLN at each row's date: buys and fees
/// negative, sales and cash dividends positive. The same rows as [`crate::pnl::asset_flows`].
pub fn asset_cash_flows(
    txs: &[LedgerTx],
    date: NaiveDate,
    mut rate: impl FnMut(&str, NaiveDate) -> Option<Decimal>,
) -> Result<BTreeMap<String, Vec<CashFlow>>, MissingRate> {
    let mut out: BTreeMap<String, Vec<CashFlow>> = BTreeMap::new();
    for tx in txs.iter().filter(|t| t.date <= date) {
        let (Some(asset), Some(flow)) = (&tx.asset, asset_flow(tx)) else {
            continue;
        };
        let pln = in_pln(tx, &mut rate)?;
        let amount = match flow {
            AssetFlow::Cost => -pln,
            AssetFlow::Proceeds | AssetFlow::Income => pln,
        };
        out.entry(asset.clone()).or_default().push(CashFlow {
            date: tx.date,
            amount,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioXirr {
    /// The first contribution: the start of the period the portfolio rate covers.
    pub first_flow: Option<NaiveDate>,
    pub portfolio: Result<Decimal, XirrError>,
    /// Every asset in the P&L, including fully sold ones (their value is 0).
    pub by_asset: BTreeMap<String, Result<Decimal, XirrError>>,
}

/// XIRR on `date` for the portfolio and each asset, using the values from the P&L of the same
/// date (so a missing price shows up the same way in both).
pub fn portfolio_xirr(
    txs: &[LedgerTx],
    date: NaiveDate,
    mut rate: impl FnMut(&str, NaiveDate) -> Option<Decimal>,
    pnl: &PortfolioPnl,
) -> Result<PortfolioXirr, MissingRate> {
    let with_value = |mut flows: Vec<CashFlow>, value: Option<Decimal>| {
        let value = value.ok_or(XirrError::MissingValue)?;
        flows.push(CashFlow {
            date,
            amount: value,
        });
        xirr(&flows)
    };
    let contributions = contribution_flows(txs, date, &mut rate)?;
    let first_flow = contributions.iter().map(|f| f.date).min();
    let portfolio = with_value(contributions, pnl.complete.then_some(pnl.value));
    let mut flows = asset_cash_flows(txs, date, &mut rate)?;
    let by_asset = pnl
        .by_asset
        .iter()
        .map(|(asset, a)| {
            let own = flows.remove(asset).unwrap_or_default();
            (asset.clone(), with_value(own, a.value))
        })
        .collect();
    Ok(PortfolioXirr {
        first_flow,
        portfolio,
        by_asset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pnl::{asset_flows, portfolio_pnl};
    use crate::portfolio::{AccountKind, TxKind, holdings_on, net_contributions_on, value};
    use chrono::Days;
    use rust_decimal_macros::dec;

    /// 2023-01-01; +365 days = 2024-01-01; +730 days = 2024-12-31 (2024 is a leap year).
    fn day(n: u64) -> NaiveDate {
        NaiveDate::from_ymd_opt(2023, 1, 1).unwrap() + Days::new(n)
    }

    fn flow(n: u64, amount: Decimal) -> CashFlow {
        CashFlow {
            date: day(n),
            amount,
        }
    }

    #[test]
    fn one_year_ten_percent() {
        // 1000 in, 1100 back exactly 365 days later.
        assert_eq!(
            xirr(&[flow(0, dec!(-1000)), flow(365, dec!(1100))]),
            Ok(dec!(0.1))
        );
    }

    #[test]
    fn two_years_with_a_loss() {
        // 1000 -> 810 over 730 days: sqrt(0.81) - 1 = -10% a year.
        assert_eq!(
            xirr(&[flow(0, dec!(-1000)), flow(730, dec!(810))]),
            Ok(dec!(-0.1))
        );
    }

    #[test]
    fn contributions_at_different_times() {
        // 1000 now and 1000 in a year, 2310 after two years: 1000 x 1.1^2 + 1000 x 1.1.
        // A plain P&L would say 310 / 2000 = 15.5%; XIRR says 10% a year.
        let flows = [
            flow(365, dec!(-1000)),
            flow(730, dec!(2310)),
            flow(0, dec!(-1000)), // order does not matter
        ];
        assert_eq!(xirr(&flows), Ok(dec!(0.1)));
    }

    #[test]
    fn money_taken_out_along_the_way() {
        // 1000 = 550 / 1.1 + 605 / 1.1^2 = 500 + 500.
        let flows = [
            flow(0, dec!(-1000)),
            flow(365, dec!(550)),
            flow(730, dec!(605)),
        ];
        assert_eq!(xirr(&flows), Ok(dec!(0.1)));
    }

    #[test]
    fn days_over_365_not_calendar_years() {
        // 2024-01-01 to 2025-01-01 is 366 days: 1.1^(365/366) - 1 = 0.0997136 (Excel agrees).
        let flows = [flow(365, dec!(-1000)), flow(731, dec!(1100))];
        assert_eq!(xirr(&flows), Ok(dec!(0.099714)));
    }

    #[test]
    fn excel_documentation_example() {
        // The XIRR example in Microsoft's Excel documentation: 0.373362535.
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
        let flows = [
            (d(2008, 1, 1), dec!(-10000)),
            (d(2008, 3, 1), dec!(2750)),
            (d(2008, 10, 30), dec!(4250)),
            (d(2009, 2, 15), dec!(3250)),
            (d(2009, 4, 1), dec!(2750)),
        ]
        .map(|(date, amount)| CashFlow { date, amount });
        assert_eq!(xirr(&flows), Ok(dec!(0.373363)));
    }

    #[test]
    fn nothing_paid_in_or_nothing_back() {
        assert_eq!(xirr(&[]), Err(XirrError::NoSignChange));
        assert_eq!(
            xirr(&[flow(0, dec!(0)), flow(10, dec!(5))]),
            Err(XirrError::NoSignChange)
        );
        assert_eq!(
            xirr(&[flow(0, dec!(-100)), flow(10, dec!(0))]),
            Err(XirrError::NoSignChange)
        );
    }

    #[test]
    fn unsolvable_flows() {
        // Everything on one day: there is no time for a rate to act on.
        assert_eq!(
            xirr(&[flow(3, dec!(-100)), flow(3, dec!(110))]),
            Err(XirrError::NoSolution)
        );
        // Doubling in one day is a rate of 2^365 - 1, about 7.5 x 10^109: beyond the search range.
        assert_eq!(
            xirr(&[flow(0, dec!(-100)), flow(1, dec!(200))]),
            Err(XirrError::NoSolution)
        );
    }

    #[test]
    fn total_loss_is_close_to_minus_one_hundred_percent() {
        let r = xirr(&[flow(0, dec!(-1000)), flow(365, dec!(0.01))]).unwrap();
        // 0.01 / 1000 = 1e-5 in a year: -99.999%.
        assert_eq!(r, dec!(-0.99999));
    }

    // --- from the ledger -------------------------------------------------------------------

    fn tx(
        n: u64,
        account: &str,
        kind: TxKind,
        asset: Option<&str>,
        amount: Decimal,
        currency: &str,
    ) -> LedgerTx {
        LedgerTx {
            date: day(n),
            account: account.into(),
            account_kind: if account == "Gold" {
                AccountKind::Manual
            } else {
                AccountKind::Funded
            },
            kind,
            asset: asset.map(Into::into),
            quantity: asset.map(|_| dec!(1)),
            amount,
            currency: currency.into(),
        }
    }

    /// XTB: 1000 PLN deposited and put into VWCE on day 0 and again on day 365.
    /// Kraken: 100 EUR deposited at 4.00 on day 0 and spent on BTC the same day.
    fn ledger() -> Vec<LedgerTx> {
        use TxKind::*;
        vec![
            tx(0, "XTB", Deposit, None, dec!(1000), "PLN"),
            tx(0, "XTB", Buy, Some("VWCE"), dec!(1000), "PLN"),
            tx(0, "Kraken", Deposit, None, dec!(100), "EUR"),
            tx(0, "Kraken", Buy, Some("BTC"), dec!(99), "EUR"),
            tx(0, "Kraken", Fee, Some("BTC"), dec!(1), "EUR"),
            tx(365, "XTB", Deposit, None, dec!(1000), "PLN"),
            tx(365, "XTB", Buy, Some("VWCE"), dec!(1000), "PLN"),
        ]
    }

    fn rate(currency: &str, date: NaiveDate) -> Option<Decimal> {
        (currency == "EUR" && date == day(0)).then_some(dec!(4.00))
    }

    /// XIRR on day 730 with VWCE worth 2310 and BTC worth 484 (unless overridden).
    fn xirr_on_day_730(
        txs: &[LedgerTx],
        prices: impl Fn(&str) -> Option<Decimal>,
    ) -> PortfolioXirr {
        let date = day(730);
        let holdings = holdings_on(txs, date);
        let valuation = value(&holdings, |asset, _| prices(asset), |_| None);
        let contributions = net_contributions_on(txs, date, rate).unwrap();
        let pnl = portfolio_pnl(
            asset_flows(txs, date, rate).unwrap(),
            &valuation,
            &contributions,
        );
        portfolio_xirr(txs, date, rate, &pnl).unwrap()
    }

    fn standard_prices(asset: &str) -> Option<Decimal> {
        match asset {
            "VWCE" => Some(dec!(2310)),
            "BTC" => Some(dec!(484)),
            "GOLD_COINS" => Some(dec!(1100)),
            "ETH" => Some(dec!(50)),
            _ => None,
        }
    }

    #[test]
    fn portfolio_and_assets_from_the_ledger() {
        let x = xirr_on_day_730(&ledger(), standard_prices);

        // VWCE: 1000 on day 0, 1000 on day 365, worth 2310: 10%.
        assert_eq!(x.by_asset["VWCE"], Ok(dec!(0.1)));
        // BTC: (99 + 1 fee) EUR x 4.00 = 400 PLN, worth 484 = 400 x 1.1^2: 10%.
        assert_eq!(x.by_asset["BTC"], Ok(dec!(0.1)));
        // Portfolio: 1000 + 400 in on day 0, 1000 on day 365, worth 2794
        // = 1400 x 1.21 + 1000 x 1.1: 10%.
        assert_eq!(x.portfolio, Ok(dec!(0.1)));
        assert_eq!(x.first_flow, Some(day(0)));
    }

    #[test]
    fn flows_follow_the_contribution_and_cost_rules() {
        let txs = ledger();
        assert_eq!(
            contribution_flows(&txs, day(730), rate).unwrap(),
            vec![
                flow(0, dec!(-1000)),
                flow(0, dec!(-400.00)),
                flow(365, dec!(-1000)),
            ]
        );
        let assets = asset_cash_flows(&txs, day(730), rate).unwrap();
        assert_eq!(
            assets["BTC"],
            vec![flow(0, dec!(-396.00)), flow(0, dec!(-4.00))]
        );
        // Only rows up to the date count.
        assert_eq!(
            asset_cash_flows(&txs, day(100), rate).unwrap()["VWCE"].len(),
            1
        );
    }

    #[test]
    fn sales_dividends_and_manual_accounts() {
        use TxKind::*;
        let txs = vec![
            // Gold: bought for 1000 on day 0, sold for 1100 on day 365; nothing left.
            tx(0, "Gold", Buy, Some("GOLD_COINS"), dec!(1000), "PLN"),
            tx(365, "Gold", Sell, Some("GOLD_COINS"), dec!(1100), "PLN"),
            // VWCE: 1000 on day 0, a 100 dividend on day 365, worth 1100 on day 730:
            // 100 / 1.1 + 1100 / 1.21 = 90.91 + 909.09 = 1000.
            tx(0, "XTB", Deposit, None, dec!(1000), "PLN"),
            tx(0, "XTB", Buy, Some("VWCE"), dec!(1000), "PLN"),
            tx(365, "XTB", Dividend, Some("VWCE"), dec!(100), "PLN"),
        ];
        let x = xirr_on_day_730(&txs, |a| (a == "VWCE").then_some(dec!(1100)));

        assert_eq!(x.by_asset["GOLD_COINS"], Ok(dec!(0.1)));
        assert_eq!(x.by_asset["VWCE"], Ok(dec!(0.1)));
        // Portfolio: 2000 in on day 0, 1100 out on day 365 (the coin sale), and 1200 at the
        // end (VWCE 1100 + the 100 dividend in cash): 1100 / 1.1 + 1200 / 1.21 = 1991.74 < 2000.
        let r = x.portfolio.unwrap();
        assert!(r < dec!(0.1) && r > dec!(0.09), "{r}");
    }

    #[test]
    fn staking_only_asset_has_no_xirr() {
        use TxKind::*;
        let mut txs = ledger();
        txs.push(tx(100, "Kraken", Dividend, Some("ETH"), dec!(0.01), "ETH"));

        let x = xirr_on_day_730(&txs, standard_prices);

        assert_eq!(x.by_asset["ETH"], Err(XirrError::NoSignChange));
        assert_eq!(x.by_asset["VWCE"], Ok(dec!(0.1)));
    }

    #[test]
    fn missing_price_means_no_xirr() {
        let x = xirr_on_day_730(&ledger(), |a| (a == "VWCE").then_some(dec!(2310)));

        assert_eq!(x.by_asset["BTC"], Err(XirrError::MissingValue));
        assert_eq!(x.by_asset["VWCE"], Ok(dec!(0.1)));
        // The total would leave BTC out: no portfolio XIRR either.
        assert_eq!(x.portfolio, Err(XirrError::MissingValue));
    }

    #[test]
    fn missing_rate_is_an_error() {
        let txs = ledger();
        let pnl = PortfolioPnl {
            value: dec!(0),
            contributions: dec!(0),
            complete: true,
            by_asset: BTreeMap::new(),
        };

        let err = portfolio_xirr(&txs, day(730), |_, _| None, &pnl).unwrap_err();

        assert_eq!((err.currency.as_str(), err.date), ("EUR", day(0)));
    }
}
