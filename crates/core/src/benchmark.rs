//! Benchmark "everything in VWCE": what the portfolio would be worth if every contribution had
//! gone into VWCE on the same day, and every withdrawal had been paid out by selling it.
//!
//! The contributions are the portfolio's own (see [`crate::portfolio`]): deposits and
//! withdrawals of funded accounts and purchases and sales of manual holdings, in PLN at each
//! flow's own date. On each day with a net flow, money put in buys `amount / price` units and
//! money taken out sells them, at that day's VWCE price in PLN; the units held on the valuation
//! date are worth `units x price` on it.
//!
//! Assumptions: no fees or spread, fractional units, no cash waiting to be invested. VWCE
//! accumulates its dividends, so the price alone carries the whole return. A withdrawal larger
//! than the benchmark's value on its day cannot be paid out of it: that is reported, not
//! carried on as a negative holding.
//!
//! Results are not rounded.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::pnl::PortfolioPnl;
use crate::xirr::{CashFlow, XirrError, xirr};

/// A withdrawal that leaves the benchmark less than this much PLN short is a rounding remainder
/// of the unit divisions (everything sold), not a withdrawal it could not pay.
const ROUNDING_TOLERANCE: Decimal = dec!(0.01);

/// The portfolio's contributions replayed into VWCE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Benchmark {
    /// VWCE units held on the valuation date.
    pub units: Decimal,
    /// Net PLN put in (the same as the portfolio's net contributions).
    pub contributions: Decimal,
    /// `units` at the VWCE price on the valuation date, PLN.
    pub value: Decimal,
    /// Annual return of the same contributions in VWCE (see [`crate::xirr`]).
    pub xirr: Result<Decimal, XirrError>,
}

impl Benchmark {
    /// Value minus net contributions.
    pub fn pnl(&self) -> Decimal {
        self.value - self.contributions
    }

    /// P&L as a fraction of net contributions; None if nothing (net) was put in.
    pub fn pnl_ratio(&self) -> Option<Decimal> {
        (self.contributions > Decimal::ZERO).then(|| self.pnl() / self.contributions)
    }

    /// How much more the actual portfolio is worth than the benchmark (negative: less). None
    /// when some position has no price, since the actual value would then be understated.
    pub fn outperformance(&self, actual: &PortfolioPnl) -> Option<Decimal> {
        actual.complete.then(|| actual.value - self.value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchmarkError {
    /// No (positive) VWCE price in PLN for a day the benchmark needs.
    MissingPrice(NaiveDate),
    /// A withdrawal took out `short` PLN more than the benchmark was worth on its day.
    Overdrawn { date: NaiveDate, short: Decimal },
}

impl fmt::Display for BenchmarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BenchmarkError::MissingPrice(day) => write!(f, "no VWCE price in PLN for {day}"),
            BenchmarkError::Overdrawn { date, short } => write!(
                f,
                "the withdrawal on {date} is {short} PLN more than VWCE would have been worth"
            ),
        }
    }
}

impl std::error::Error for BenchmarkError {}

/// Days that need a VWCE price: every flow's date up to `date`, and `date` itself.
pub fn price_days(flows: &[CashFlow], date: NaiveDate) -> BTreeSet<NaiveDate> {
    flows
        .iter()
        .map(|f| f.date)
        .filter(|d| *d <= date)
        .chain([date])
        .collect()
}

/// Replays `flows` (contributions as from [`crate::xirr::contribution_flows`]: put in =
/// negative) into VWCE and values the result on `date`. `price(day)` is PLN per VWCE unit on
/// that day. Flows after `date` are ignored; flows on one day are netted first.
pub fn vwce_benchmark(
    flows: &[CashFlow],
    date: NaiveDate,
    mut price: impl FnMut(NaiveDate) -> Option<Decimal>,
) -> Result<Benchmark, BenchmarkError> {
    let flows: Vec<CashFlow> = flows.iter().filter(|f| f.date <= date).copied().collect();
    let mut put_in_by_day: BTreeMap<NaiveDate, Decimal> = BTreeMap::new();
    for f in &flows {
        *put_in_by_day.entry(f.date).or_default() -= f.amount;
    }
    let mut price_on = |day: NaiveDate| {
        price(day)
            .filter(|p| *p > Decimal::ZERO)
            .ok_or(BenchmarkError::MissingPrice(day))
    };

    let mut units = Decimal::ZERO;
    for (&day, &put_in) in &put_in_by_day {
        if put_in.is_zero() {
            continue;
        }
        let p = price_on(day)?;
        let short = -put_in - units * p;
        if short >= ROUNDING_TOLERANCE {
            return Err(BenchmarkError::Overdrawn { date: day, short });
        }
        // Selling (about) everything can leave a sub-grosz remainder of either sign.
        units = if short > Decimal::ZERO {
            Decimal::ZERO
        } else {
            (units + put_in / p).max(Decimal::ZERO)
        };
    }
    let value = if units.is_zero() {
        Decimal::ZERO
    } else {
        units * price_on(date)?
    };
    let mut dated = flows;
    dated.push(CashFlow {
        date,
        amount: value,
    });
    Ok(Benchmark {
        units,
        contributions: put_in_by_day.values().sum(),
        value,
        xirr: xirr(&dated),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    /// A contribution of `put_in` PLN on `date` (negative = withdrawal), as an investor flow.
    fn put(date: &str, put_in: Decimal) -> CashFlow {
        CashFlow {
            date: d(date),
            amount: -put_in,
        }
    }

    fn prices(days: &[(&str, Decimal)]) -> impl FnMut(NaiveDate) -> Option<Decimal> {
        let map: BTreeMap<NaiveDate, Decimal> = days.iter().map(|(s, p)| (d(s), *p)).collect();
        move |day| map.get(&day).copied()
    }

    fn pnl(value: Decimal, complete: bool) -> PortfolioPnl {
        PortfolioPnl {
            value,
            contributions: dec!(2000),
            complete,
            by_asset: BTreeMap::new(),
        }
    }

    #[test]
    fn buys_and_sells_units_at_each_days_price() {
        // 1000 at 100 = 10 units; 1500 at 125 = 12; 500 out at 100 = -5. 17 units x 130 = 2210.
        // Net contributions 1000 + 1500 - 500 = 2000: P&L 210, 10.5%.
        let flows = [
            put("2026-01-05", dec!(1000)),
            put("2026-02-02", dec!(1500)),
            put("2026-03-02", dec!(-500)),
        ];
        let b = vwce_benchmark(
            &flows,
            d("2026-06-30"),
            prices(&[
                ("2026-01-05", dec!(100)),
                ("2026-02-02", dec!(125)),
                ("2026-03-02", dec!(100)),
                ("2026-06-30", dec!(130)),
            ]),
        )
        .unwrap();

        assert_eq!(b.units, dec!(17));
        assert_eq!(b.value, dec!(2210));
        assert_eq!(b.contributions, dec!(2000));
        assert_eq!((b.pnl(), b.pnl_ratio()), (dec!(210), Some(dec!(0.105))));
    }

    #[test]
    fn xirr_of_the_same_contributions_in_vwce() {
        // 1000 at 50 = 20 units; 365 days later at 55 they are worth 1100: 10% a year.
        let b = vwce_benchmark(
            &[put("2023-01-01", dec!(1000))],
            d("2024-01-01"),
            prices(&[("2023-01-01", dec!(50)), ("2024-01-01", dec!(55))]),
        )
        .unwrap();

        assert_eq!((b.units, b.value), (dec!(20), dec!(1100)));
        assert_eq!(b.xirr, Ok(dec!(0.1)));
    }

    #[test]
    fn compares_with_the_actual_value_only_when_it_is_complete() {
        // Benchmark 2210 (as above); the portfolio is worth 2400: 190 better than VWCE alone.
        let b = Benchmark {
            units: dec!(17),
            contributions: dec!(2000),
            value: dec!(2210),
            xirr: Err(XirrError::NoSolution),
        };
        assert_eq!(b.outperformance(&pnl(dec!(2400), true)), Some(dec!(190)));
        assert_eq!(b.outperformance(&pnl(dec!(2000), true)), Some(dec!(-210)));
        assert_eq!(b.outperformance(&pnl(dec!(2400), false)), None);
    }

    #[test]
    fn flows_on_one_day_are_netted_and_later_flows_ignored() {
        // 2 Jan: 600 in and 200 out = 400 at 80 = 5 units. 10 Jan nets to zero, so it needs no
        // price. The 1 Feb deposit is after the date. 5 x 90 = 450 on 31 Jan.
        let flows = [
            put("2026-01-02", dec!(600)),
            put("2026-01-02", dec!(-200)),
            put("2026-01-10", dec!(300)),
            put("2026-01-10", dec!(-300)),
            put("2026-02-01", dec!(1000)),
        ];
        let b = vwce_benchmark(
            &flows,
            d("2026-01-31"),
            prices(&[("2026-01-02", dec!(80)), ("2026-01-31", dec!(90))]),
        )
        .unwrap();

        assert_eq!(
            (b.units, b.value, b.contributions),
            (dec!(5), dec!(450), dec!(400))
        );
    }

    #[test]
    fn a_missing_or_non_positive_price_is_reported_with_its_day() {
        let flows = [put("2026-01-05", dec!(1000)), put("2026-02-02", dec!(500))];
        let date = d("2026-03-31");
        let err = |p: &[(&str, Decimal)]| vwce_benchmark(&flows, date, prices(p)).unwrap_err();

        assert_eq!(
            err(&[("2026-01-05", dec!(100)), ("2026-03-31", dec!(110))]),
            BenchmarkError::MissingPrice(d("2026-02-02"))
        );
        assert_eq!(
            err(&[("2026-01-05", dec!(100)), ("2026-02-02", dec!(105))]),
            BenchmarkError::MissingPrice(date)
        );
        // A zero price would divide by zero: treated as missing.
        assert_eq!(
            err(&[("2026-01-05", dec!(0)), ("2026-02-02", dec!(105))]),
            BenchmarkError::MissingPrice(d("2026-01-05"))
        );
    }

    #[test]
    fn a_withdrawal_larger_than_the_benchmark_is_reported() {
        // 10 units at 110 are worth 1100; taking out 1200 leaves it 100 PLN short.
        let flows = [
            put("2026-01-05", dec!(1000)),
            put("2026-02-02", dec!(-1200)),
        ];
        let p = [
            ("2026-01-05", dec!(100)),
            ("2026-02-02", dec!(110)),
            ("2026-03-31", dec!(120)),
        ];
        assert_eq!(
            vwce_benchmark(&flows, d("2026-03-31"), prices(&p)),
            Err(BenchmarkError::Overdrawn {
                date: d("2026-02-02"),
                short: dec!(100)
            })
        );
        // Taking out exactly 1100 sells everything: nothing left, no price needed at the end.
        let flows = [
            put("2026-01-05", dec!(1000)),
            put("2026-02-02", dec!(-1100)),
        ];
        let b = vwce_benchmark(&flows, d("2026-03-31"), prices(&p[..2])).unwrap();
        assert_eq!(
            (b.units, b.value, b.contributions),
            (dec!(0), dec!(0), dec!(-100))
        );
        // 1000 in, 1100 out: XIRR of the money taken out, (1100 / 1000)^(365 / 28) - 1.
        assert!(b.xirr.unwrap() > dec!(2));
    }

    #[test]
    fn a_rounding_remainder_is_not_an_overdraft() {
        // 1 PLN at 3 three times = 3 x 0.3333333333333333333333333333 (28 digits), a hair under 1
        // unit; taking out 3 PLN at 3 sells 1. The 1e-28 unit short is rounding: units end at 0.
        let flows = [
            put("2026-01-05", dec!(1)),
            put("2026-01-06", dec!(1)),
            put("2026-01-07", dec!(1)),
            put("2026-01-08", dec!(-3)),
        ];
        let p = [
            ("2026-01-05", dec!(3)),
            ("2026-01-06", dec!(3)),
            ("2026-01-07", dec!(3)),
            ("2026-01-08", dec!(3)),
        ];
        let b = vwce_benchmark(&flows, d("2026-01-31"), prices(&p)).unwrap();
        assert_eq!((b.units, b.value), (dec!(0), dec!(0)));
    }

    #[test]
    fn no_contributions_need_no_prices() {
        let b = vwce_benchmark(&[], d("2026-01-31"), |_| None).unwrap();
        assert_eq!(
            (b.units, b.value, b.contributions),
            (dec!(0), dec!(0), dec!(0))
        );
        assert_eq!(
            (b.pnl_ratio(), b.xirr),
            (None, Err(XirrError::NoSignChange))
        );
    }

    #[test]
    fn price_days_are_the_flow_days_and_the_date() {
        let flows = [
            put("2026-01-05", dec!(1000)),
            put("2026-01-05", dec!(-100)),
            put("2026-02-02", dec!(500)),
            put("2026-05-01", dec!(500)),
        ];
        assert_eq!(
            price_days(&flows, d("2026-03-31")),
            BTreeSet::from([d("2026-01-05"), d("2026-02-02"), d("2026-03-31")])
        );
    }
}
