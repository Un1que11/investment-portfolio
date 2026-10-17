//! Period views for the Overview's 1M / 6M / YTD / 1Y / All switch: the P&L, net contributions
//! and XIRR of one period, and how far the portfolio is ahead of "everything in VWCE" over it.
//!
//! A period other than All starts at the end of its start day (one month, six months or a year
//! before the end, or 31 December of the year before for YTD) and runs to the end of the end
//! day. The portfolio's value at the start counts as money put in on the start day; the
//! contributions after it, up to the end, are the period's own. So, with `V0` the start value,
//! `C` the period's net contributions and `V1` the end value:
//! - P&L = V1 - V0 - C, and % = P&L / (V0 + C), as the Overview reference computes it. For All,
//!   V0 is 0 and both are the same as the whole portfolio's ([`crate::pnl`]).
//! - XIRR is that of the flows -V0 on the start day, the contributions, and +V1 on the end day.
//!   It is not given for a period shorter than [`MIN_XIRR_MONTHS`] months, counted from the
//!   start day, or from the first contribution when nothing was held at the start (a young
//!   portfolio, or All): the Overview shows "—" with "Needs 3+ months of data". Annualising less
//!   than a quarter multiplies a small move at least fourfold (1% in a month reads as about 12.7%
//!   a year), which says more about the period's length than about the return.
//! - The benchmark replays the same flows, V0 included, into VWCE ([`crate::benchmark`]): the
//!   gap is V1 minus what VWCE alone would be worth at the end.
//!
//! Results are not rounded.

use std::fmt;

use chrono::{Datelike, Months, NaiveDate};
use rust_decimal::Decimal;

use crate::benchmark::{Benchmark, BenchmarkError, vwce_benchmark};
use crate::xirr::{CashFlow, XirrError, xirr};

/// The shortest period, in months, that gets an XIRR.
pub const MIN_XIRR_MONTHS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Period {
    OneMonth,
    SixMonths,
    YearToDate,
    OneYear,
    All,
}

impl Period {
    /// In the order of the Overview's switch.
    pub const ALL: [Period; 5] = [
        Period::OneMonth,
        Period::SixMonths,
        Period::YearToDate,
        Period::OneYear,
        Period::All,
    ];

    /// The switch's label.
    pub fn label(self) -> &'static str {
        match self {
            Period::OneMonth => "1M",
            Period::SixMonths => "6M",
            Period::YearToDate => "YTD",
            Period::OneYear => "1Y",
            Period::All => "All",
        }
    }

    /// The day whose end-of-day value starts the period ending on `end`; None for All. A month
    /// back from a day the earlier month lacks lands on that month's last day (31 March -> 28
    /// February).
    pub fn start(self, end: NaiveDate) -> Option<NaiveDate> {
        let months_back = |n| end.checked_sub_months(Months::new(n));
        match self {
            Period::OneMonth => months_back(1),
            Period::SixMonths => months_back(6),
            Period::OneYear => months_back(12),
            Period::YearToDate => NaiveDate::from_ymd_opt(end.year() - 1, 12, 31),
            Period::All => None,
        }
    }
}

impl fmt::Display for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeriodError {
    /// Some position at the start of the period has no price.
    MissingStartValue,
    /// Some position at the end of the period has no price.
    MissingEndValue,
    /// The period is shorter than [`MIN_XIRR_MONTHS`] months (see the module docs).
    TooShort,
    Xirr(XirrError),
    Benchmark(BenchmarkError),
}

impl fmt::Display for PeriodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeriodError::MissingStartValue => f.write_str("no price for the start value"),
            PeriodError::MissingEndValue => f.write_str("no price for the end value"),
            PeriodError::TooShort => write!(f, "needs {MIN_XIRR_MONTHS}+ months of data"),
            PeriodError::Xirr(e) => e.fmt(f),
            PeriodError::Benchmark(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for PeriodError {}

/// One period of the portfolio, in PLN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodView {
    pub period: Period,
    /// The start day ([`Period::start`]); None for All.
    pub start: Option<NaiveDate>,
    pub end: NaiveDate,
    /// The day the period's money starts working: the start day when something was held then
    /// (or its value is unknown), otherwise the first contribution after it. None when nothing
    /// was held or put in. The XIRR's length is counted from it.
    pub since: Option<NaiveDate>,
    /// The value at the end of the start day; 0 for All. None when some position had no price.
    pub start_value: Option<Decimal>,
    /// The value at the end of the end day; None when some position had no price.
    pub end_value: Option<Decimal>,
    /// Net PLN put in after the start day, up to the end of the end day.
    pub contributions: Decimal,
    /// The period's annual money-weighted return (0.1 = 10% a year).
    pub xirr: Result<Decimal, PeriodError>,
    /// The start value and the contributions, each put into VWCE on its own day.
    pub benchmark: Result<Benchmark, PeriodError>,
}

impl PeriodView {
    /// End value minus start value minus contributions; None when either value is unknown.
    pub fn pnl(&self) -> Option<Decimal> {
        Some(self.end_value? - self.start_value? - self.contributions)
    }

    /// P&L as a fraction of the start value plus contributions; None when that is not above
    /// zero (everything withdrawn) or a value is unknown.
    pub fn pnl_ratio(&self) -> Option<Decimal> {
        let base = self.start_value? + self.contributions;
        let pnl = self.pnl()?;
        (base > Decimal::ZERO).then(|| pnl / base)
    }

    /// How much more the portfolio is worth at the end than everything in VWCE (negative:
    /// less); None when the end value or the benchmark is unknown.
    pub fn benchmark_gap(&self) -> Option<Decimal> {
        Some(self.end_value? - self.benchmark.as_ref().ok()?.value)
    }
}

/// The view of `period` ending on `end`.
///
/// - `contributions`: the portfolio's contributions as from
///   [`crate::xirr::contribution_flows`] (put in = negative); flows outside the period are
///   ignored, so the whole history can be passed.
/// - `start_value`: the value at the end of [`Period::start`] (0 when nothing was held); not
///   used for All.
/// - `end_value`: the value at the end of `end`.
/// - `vwce_price(day)`: PLN per VWCE unit, asked for the start day (when the start value is not
///   zero), each contribution's day and `end`.
pub fn period_view(
    period: Period,
    end: NaiveDate,
    contributions: &[CashFlow],
    start_value: Option<Decimal>,
    end_value: Option<Decimal>,
    vwce_price: impl FnMut(NaiveDate) -> Option<Decimal>,
) -> PeriodView {
    let start = period.start(end);
    let start_value = if start.is_some() {
        start_value
    } else {
        Some(Decimal::ZERO)
    };
    let own: Vec<CashFlow> = contributions
        .iter()
        .filter(|f| start.is_none_or(|s| f.date > s) && f.date <= end)
        .copied()
        .collect();
    let since = match (start, start_value) {
        (Some(s), v) if v != Some(Decimal::ZERO) => Some(s),
        _ => own.iter().map(|f| f.date).min(),
    };

    let flows: Result<Vec<CashFlow>, PeriodError> = match (start, start_value) {
        (_, None) => Err(PeriodError::MissingStartValue),
        (Some(s), Some(v)) if !v.is_zero() => Ok([CashFlow {
            date: s,
            amount: -v,
        }]
        .into_iter()
        .chain(own.iter().copied())
        .collect()),
        _ => Ok(own.clone()),
    };
    let benchmark = flows
        .clone()
        .and_then(|f| vwce_benchmark(&f, end, vwce_price).map_err(PeriodError::Benchmark));
    let long_enough =
        since.is_none_or(|s| s.checked_add_months(Months::new(MIN_XIRR_MONTHS)) <= Some(end));
    let xirr = flows.and_then(|mut f| {
        let v = end_value.ok_or(PeriodError::MissingEndValue)?;
        if !long_enough {
            return Err(PeriodError::TooShort);
        }
        f.push(CashFlow {
            date: end,
            amount: v,
        });
        xirr(&f).map_err(PeriodError::Xirr)
    });

    PeriodView {
        period,
        start,
        end,
        since,
        start_value,
        end_value,
        contributions: own.iter().map(|f| -f.amount).sum(),
        xirr,
        benchmark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use rust_decimal_macros::dec;

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

    fn prices(days: &[(&str, Decimal)]) -> impl FnMut(NaiveDate) -> Option<Decimal> + use<> {
        let map: BTreeMap<NaiveDate, Decimal> = days.iter().map(|(s, p)| (d(s), *p)).collect();
        move |day| map.get(&day).copied()
    }

    fn no_prices(_: NaiveDate) -> Option<Decimal> {
        None
    }

    #[test]
    fn start_days_count_back_from_the_end() {
        let end = d("2026-10-17");
        let starts = Period::ALL.map(|p| p.start(end));
        assert_eq!(
            starts,
            [
                Some(d("2026-09-17")),
                Some(d("2026-04-17")),
                Some(d("2025-12-31")),
                Some(d("2025-10-17")),
                None
            ]
        );
        assert_eq!(
            Period::ALL.map(Period::label),
            ["1M", "6M", "YTD", "1Y", "All"]
        );
    }

    #[test]
    fn a_day_the_earlier_month_lacks_lands_on_its_last_day() {
        // 31 March: February has no 31st (nor 29th in 2026). 31 August - 6 months: February too.
        assert_eq!(
            Period::OneMonth.start(d("2026-03-31")),
            Some(d("2026-02-28"))
        );
        assert_eq!(
            Period::SixMonths.start(d("2026-08-31")),
            Some(d("2026-02-28"))
        );
        // 29 February 2024 - 1 year: 28 February 2023. YTD on 1 January starts on 31 December.
        assert_eq!(
            Period::OneYear.start(d("2024-02-29")),
            Some(d("2023-02-28"))
        );
        assert_eq!(
            Period::YearToDate.start(d("2026-01-01")),
            Some(d("2025-12-31"))
        );
    }

    #[test]
    fn pnl_takes_out_the_start_value_and_the_periods_own_contributions() {
        // 6M ending 17 Oct 2026 starts at the end of 17 Apr. Worth 9000 then. The 1000 on 17 Apr
        // itself and the 2000 before it are in the start value already; only the 1000 on 1 Jul
        // is the period's. Worth 10 500 at the end: P&L 10 500 - 9000 - 1000 = 500, over 10 000
        // = 5%.
        let flows = [
            put("2025-11-03", dec!(2000)),
            put("2026-04-17", dec!(1000)),
            put("2026-07-01", dec!(1000)),
        ];
        let v = period_view(
            Period::SixMonths,
            d("2026-10-17"),
            &flows,
            Some(dec!(9000)),
            Some(dec!(10500)),
            no_prices,
        );

        assert_eq!(v.start, Some(d("2026-04-17")));
        assert_eq!(v.since, Some(d("2026-04-17")));
        assert_eq!(v.contributions, dec!(1000));
        assert_eq!(
            (v.pnl(), v.pnl_ratio()),
            (Some(dec!(500)), Some(dec!(0.05)))
        );
    }

    #[test]
    fn the_start_value_is_money_put_in_on_the_start_day() {
        // 1Y ending 1 Jan 2024 starts at the end of 1 Jan 2023, worth 1000. Nothing is put in
        // after it and it is worth 1100 365 days later: 10% a year. The earlier contribution and
        // the one on the start day are inside the 1000, so they do not count again.
        let flows = [put("2022-06-01", dec!(800)), put("2023-01-01", dec!(150))];
        let v = period_view(
            Period::OneYear,
            d("2024-01-01"),
            &flows,
            Some(dec!(1000)),
            Some(dec!(1100)),
            no_prices,
        );

        assert_eq!(v.contributions, dec!(0));
        assert_eq!(v.xirr, Ok(dec!(0.1)));
        assert_eq!((v.pnl(), v.pnl_ratio()), (Some(dec!(100)), Some(dec!(0.1))));
    }

    #[test]
    fn all_is_the_whole_portfolio_from_the_first_contribution() {
        // 1000 on 1 Jan 2023 and 1000 a year later, 2310 at the end of 2024 (730 days from the
        // first): 1000 x 1.1^2 + 1000 x 1.1, so 10% a year. P&L 2310 - 2000 = 310, 15.5%.
        let flows = [put("2023-01-01", dec!(1000)), put("2024-01-01", dec!(1000))];
        let v = period_view(
            Period::All,
            d("2024-12-31"),
            &flows,
            // Not used for All.
            Some(dec!(99999)),
            Some(dec!(2310)),
            no_prices,
        );

        assert_eq!((v.start, v.since), (None, Some(d("2023-01-01"))));
        assert_eq!(
            (v.start_value, v.contributions),
            (Some(dec!(0)), dec!(2000))
        );
        assert_eq!(v.xirr, Ok(dec!(0.1)));
        assert_eq!(
            (v.pnl(), v.pnl_ratio()),
            (Some(dec!(310)), Some(dec!(0.155)))
        );
    }

    #[test]
    fn xirr_needs_three_months_from_when_the_money_started() {
        let end = d("2026-10-17");
        let one = |p: Period, flows: &[CashFlow], start_value: Decimal| {
            period_view(
                p,
                end,
                flows,
                Some(start_value),
                Some(dec!(1100)),
                no_prices,
            )
            .xirr
        };
        // 1M is always too short, whatever is held.
        assert_eq!(
            one(Period::OneMonth, &[], dec!(1000)),
            Err(PeriodError::TooShort)
        );
        // 6M, but nothing was held on 17 Apr and the first 1000 went in on 18 Jul: 2 months and
        // 29 days to 17 Oct, too short.
        let young = [put("2026-07-18", dec!(1000))];
        assert_eq!(
            one(Period::SixMonths, &young, dec!(0)),
            Err(PeriodError::TooShort)
        );
        // From 17 Jul it is exactly 3 months: the rate is given (1000 -> 1100 in 92 days).
        let three_months = [put("2026-07-17", dec!(1000))];
        assert!(one(Period::SixMonths, &three_months, dec!(0)).is_ok());
        assert!(one(Period::All, &three_months, dec!(0)).is_ok());
        assert_eq!(
            one(Period::All, &young, dec!(0)),
            Err(PeriodError::TooShort)
        );
    }

    #[test]
    fn a_period_longer_than_the_portfolio_is_counted_from_its_first_contribution() {
        // 1Y ending 17 Oct 2026 with nothing held on 17 Oct 2025: the same as All.
        let flows = [put("2026-05-04", dec!(1000)), put("2026-08-03", dec!(500))];
        let price = || {
            prices(&[
                ("2026-05-04", dec!(100)),
                ("2026-08-03", dec!(125)),
                ("2026-10-17", dec!(130)),
            ])
        };
        let end = d("2026-10-17");
        let year = period_view(
            Period::OneYear,
            end,
            &flows,
            Some(dec!(0)),
            Some(dec!(1700)),
            price(),
        );
        let all = period_view(Period::All, end, &flows, None, Some(dec!(1700)), price());

        assert_eq!(year.since, Some(d("2026-05-04")));
        assert_eq!(
            (year.contributions, year.pnl(), &year.xirr, &year.benchmark),
            (all.contributions, all.pnl(), &all.xirr, &all.benchmark)
        );
        // VWCE: 1000 at 100 = 10 units, 500 at 125 = 4, 14 x 130 = 1820: 120 more than the 1700.
        assert_eq!(year.benchmark_gap(), Some(dec!(-120)));
    }

    #[test]
    fn the_benchmark_starts_with_the_start_value_in_vwce() {
        // 6M from 17 Apr: 1000 held then buys 10 units at 100; 500 on 1 Jul buys 4 at 125. At
        // 130 the 14 units are worth 1820; the portfolio is worth 1900: 80 ahead of all-in VWCE.
        let flows = [put("2026-01-05", dec!(700)), put("2026-07-01", dec!(500))];
        let v = period_view(
            Period::SixMonths,
            d("2026-10-17"),
            &flows,
            Some(dec!(1000)),
            Some(dec!(1900)),
            prices(&[
                ("2026-04-17", dec!(100)),
                ("2026-07-01", dec!(125)),
                ("2026-10-17", dec!(130)),
            ]),
        );

        let b = v.benchmark.as_ref().unwrap();
        assert_eq!(
            (b.units, b.value, b.contributions),
            (dec!(14), dec!(1820), dec!(1500))
        );
        assert_eq!(v.benchmark_gap(), Some(dec!(80)));
        assert_eq!(v.pnl(), Some(dec!(400)));
    }

    #[test]
    fn unknown_values_leave_out_what_needs_them() {
        let flows = [put("2026-07-01", dec!(500))];
        let price = || {
            prices(&[
                ("2026-04-17", dec!(100)),
                ("2026-07-01", dec!(125)),
                ("2026-10-17", dec!(130)),
            ])
        };
        let end = d("2026-10-17");

        // No start value: no P&L, XIRR or benchmark; the contributions are still known.
        let v = period_view(
            Period::SixMonths,
            end,
            &flows,
            None,
            Some(dec!(1900)),
            price(),
        );
        assert_eq!(
            (v.contributions, v.pnl(), v.pnl_ratio()),
            (dec!(500), None, None)
        );
        assert_eq!(v.xirr, Err(PeriodError::MissingStartValue));
        assert_eq!(v.benchmark, Err(PeriodError::MissingStartValue));
        assert_eq!((v.since, v.benchmark_gap()), (Some(d("2026-04-17")), None));

        // No end value: the benchmark is known, but not the gap to it.
        let v = period_view(
            Period::SixMonths,
            end,
            &flows,
            Some(dec!(1000)),
            None,
            price(),
        );
        assert_eq!(v.xirr, Err(PeriodError::MissingEndValue));
        assert_eq!(v.benchmark.as_ref().map(|b| b.value), Ok(dec!(1820)));
        assert_eq!((v.pnl(), v.benchmark_gap()), (None, None));

        // No VWCE price for the start day.
        let v = period_view(
            Period::SixMonths,
            end,
            &flows,
            Some(dec!(1000)),
            Some(dec!(1900)),
            no_prices,
        );
        assert_eq!(
            v.benchmark,
            Err(PeriodError::Benchmark(BenchmarkError::MissingPrice(d(
                "2026-04-17"
            ))))
        );
    }

    #[test]
    fn no_percentage_when_everything_was_taken_out() {
        // 1000 held on 17 Sep, all 1000 withdrawn on 1 Oct, nothing left: P&L 0 - 1000 + 1000 = 0,
        // but the base 1000 - 1000 is zero, so there is no percentage.
        let v = period_view(
            Period::OneMonth,
            d("2026-10-17"),
            &[put("2026-10-01", dec!(-1000))],
            Some(dec!(1000)),
            Some(dec!(0)),
            no_prices,
        );

        assert_eq!(v.contributions, dec!(-1000));
        assert_eq!((v.pnl(), v.pnl_ratio()), (Some(dec!(0)), None));
    }
}
