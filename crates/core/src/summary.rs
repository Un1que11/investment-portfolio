//! The monthly summary (BACKLOG P26), computed from two stored snapshot days
//! ([`crate::snapshot`]): the last day with a snapshot in the month, and the day to compare it
//! with (the last snapshot before the month, or the month's first one when there is none).
//!
//! Per asset, a snapshot row carries the value and the net amount still invested in it (cost
//! minus sale proceeds and cash dividends), so between the two days:
//! - new money = invested(end) - invested(start): what went into the assets, net of sales;
//! - profit = (value(end) - invested(end)) - (value(start) - invested(start)), which is the
//!   change in value minus the new money.
//!
//! An asset missing from one of the days counts as 0 there: not held yet, or sold out before
//! snapshots kept sold-out rows. Cash in the accounts has no snapshot and is not included.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, Months, NaiveDate};
use rust_decimal::Decimal;

use crate::snapshot::SnapshotRow;

/// A calendar month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Month {
    first: NaiveDate,
}

impl Month {
    /// The month a day falls in.
    pub fn of(day: NaiveDate) -> Month {
        Month {
            first: day.with_day(1).expect("every month has a first day"),
        }
    }

    pub fn first_day(self) -> NaiveDate {
        self.first
    }

    pub fn last_day(self) -> NaiveDate {
        self.next()
            .first
            .pred_opt()
            .expect("a day before every month")
    }

    pub fn previous(self) -> Month {
        Month {
            first: self.first - Months::new(1),
        }
    }

    pub fn next(self) -> Month {
        Month {
            first: self.first + Months::new(1),
        }
    }

    /// "September 2026".
    pub fn name(self) -> String {
        self.first.format("%B %Y").to_string()
    }
}

/// "2026-09", the form [`Month::from_str`] reads.
impl fmt::Display for Month {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.first.format("%Y-%m"))
    }
}

impl FromStr for Month {
    type Err = String;

    /// "2026-09"; other forms ("2026-9", "09.2026") are refused.
    fn from_str(s: &str) -> Result<Month, String> {
        let bad = || format!("invalid month {s:?}, expected YYYY-MM");
        let (y, m) = s.split_once('-').ok_or_else(bad)?;
        if y.len() != 4 || m.len() != 2 || !(y.bytes().chain(m.bytes())).all(|b| b.is_ascii_digit())
        {
            return Err(bad());
        }
        let first = NaiveDate::from_ymd_opt(
            y.parse().map_err(|_| bad())?,
            m.parse().map_err(|_| bad())?,
            1,
        )
        .ok_or_else(bad)?;
        Ok(Month { first })
    }
}

/// The snapshot rows stored for one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDay {
    pub date: NaiveDate,
    pub rows: Vec<SnapshotRow>,
}

impl SnapshotDay {
    fn value(&self) -> Decimal {
        self.rows.iter().map(|r| r.value_pln).sum()
    }

    fn invested(&self) -> Decimal {
        self.rows.iter().map(|r| r.invested_pln).sum()
    }
}

/// What changed between the compared day and the month's last snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthChange {
    /// The day compared with.
    pub since: NaiveDate,
    /// False when `since` is inside the month (no snapshot before it), so the change covers
    /// only part of the month.
    pub whole_month: bool,
    pub value_change: Decimal,
    pub new_money: Decimal,
    pub profit: Decimal,
    /// Each asset's profit, most earned first (ties by symbol).
    pub by_asset: Vec<(String, Decimal)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthSummary {
    pub month: Month,
    /// The month's last day with a snapshot.
    pub end: NaiveDate,
    /// Value of the assets on `end`, PLN.
    pub value: Decimal,
    /// Still invested in them on `end`, PLN.
    pub invested: Decimal,
    /// None when there is no other day to compare with yet.
    pub change: Option<MonthChange>,
}

impl MonthSummary {
    /// Value minus what is still invested: everything earned up to `end`.
    pub fn profit_to_date(&self) -> Decimal {
        self.value - self.invested
    }

    /// [`Self::profit_to_date`] as a fraction of what is invested; None unless invested > 0.
    pub fn profit_to_date_fraction(&self) -> Option<Decimal> {
        (self.invested > Decimal::ZERO).then(|| self.profit_to_date() / self.invested)
    }
}

/// The summary of `month` from its last snapshot day `end` and the day to compare it with
/// (`start`, None or the same day when there is none).
pub fn monthly_summary(
    month: Month,
    start: Option<&SnapshotDay>,
    end: &SnapshotDay,
) -> MonthSummary {
    let change = start.filter(|s| s.date < end.date).map(|start| {
        let mut by_asset: BTreeMap<&str, Decimal> = BTreeMap::new();
        for r in &end.rows {
            *by_asset.entry(&r.asset).or_default() += r.value_pln - r.invested_pln;
        }
        for r in &start.rows {
            *by_asset.entry(&r.asset).or_default() -= r.value_pln - r.invested_pln;
        }
        let mut by_asset: Vec<(String, Decimal)> = by_asset
            .into_iter()
            .map(|(a, p)| (a.to_owned(), p))
            .collect();
        by_asset.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let value_change = end.value() - start.value();
        let new_money = end.invested() - start.invested();
        MonthChange {
            since: start.date,
            whole_month: start.date < month.first_day(),
            value_change,
            new_money,
            profit: value_change - new_money,
            by_asset,
        }
    });
    MonthSummary {
        month,
        end: end.date,
        value: end.value(),
        invested: end.invested(),
        change,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn row(asset: &str, quantity: Decimal, value: Decimal, invested: Decimal) -> SnapshotRow {
        SnapshotRow {
            asset: asset.into(),
            quantity,
            value_pln: value,
            invested_pln: invested,
        }
    }

    fn day(date: &str, rows: Vec<SnapshotRow>) -> SnapshotDay {
        SnapshotDay {
            date: d(date),
            rows,
        }
    }

    #[test]
    fn months_parse_print_and_step() {
        let m: Month = "2026-09".parse().unwrap();
        assert_eq!(m.to_string(), "2026-09");
        assert_eq!(m.name(), "September 2026");
        assert_eq!(
            (m.first_day(), m.last_day()),
            (d("2026-09-01"), d("2026-09-30"))
        );
        assert_eq!(Month::of(d("2026-10-20")).previous(), m);
        // Across a year and in a leap February.
        assert_eq!(Month::of(d("2027-01-01")).previous().to_string(), "2026-12");
        assert_eq!(Month::of(d("2028-02-10")).last_day(), d("2028-02-29"));
        for bad in [
            "2026-9",
            "09.2026",
            "2026-13",
            "2026-00",
            "26-09",
            "2026-09-01",
            "+026-09",
        ] {
            assert!(bad.parse::<Month>().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_whole_month_with_new_money_and_profit_per_asset() {
        // 31 Aug: VWCE 10 000.00 value / 9 000.00 invested; BTC 3 000.00 / 2 500.00.
        // 30 Sep: VWCE 11 500.00 / 10 000.00 (1 000 bought); BTC 2 800.00 / 2 500.00.
        //   Value: 13 000.00 -> 14 300.00, change +1 300.00.
        //   New money: 11 500.00 -> 12 500.00 = +1 000.00.
        //   Profit: 1 300.00 - 1 000.00 = +300.00.
        //   VWCE: (11 500 - 10 000) - (10 000 - 9 000) = 1 500 - 1 000 = +500.00.
        //   BTC: (2 800 - 2 500) - (3 000 - 2 500) = 300 - 500 = -200.00.
        //   To date: 14 300 - 12 500 = +1 800.00, 1 800 / 12 500 = 14.4%.
        let start = day(
            "2026-08-31",
            vec![
                row("BTC", dec!(0.01), dec!(3000.00), dec!(2500.00)),
                row("VWCE", dec!(20), dec!(10000.00), dec!(9000.00)),
            ],
        );
        let end = day(
            "2026-09-30",
            vec![
                row("BTC", dec!(0.01), dec!(2800.00), dec!(2500.00)),
                row("VWCE", dec!(22), dec!(11500.00), dec!(10000.00)),
            ],
        );
        let s = monthly_summary("2026-09".parse().unwrap(), Some(&start), &end);
        assert_eq!(
            (s.end, s.value, s.invested),
            (d("2026-09-30"), dec!(14300.00), dec!(12500.00))
        );
        assert_eq!(s.profit_to_date(), dec!(1800.00));
        assert_eq!(s.profit_to_date_fraction(), Some(dec!(0.144)));
        assert_eq!(
            s.change,
            Some(MonthChange {
                since: d("2026-08-31"),
                whole_month: true,
                value_change: dec!(1300.00),
                new_money: dec!(1000.00),
                profit: dec!(300.00),
                by_asset: vec![("VWCE".into(), dec!(500.00)), ("BTC".into(), dec!(-200.00))],
            })
        );
    }

    #[test]
    fn a_sale_and_a_new_asset_within_the_month() {
        // 31 Aug: ETH worth 1 100.00, invested 1 000.00. VWCE 5 000.00 / 5 000.00.
        // In September ETH is sold out for 1 150.00: its sold-out row is quantity 0, value 0,
        // invested 1 000 - 1 150 = -150.00. Gold bought for 1 200.00, worth 1 140.00 on 30 Sep.
        //   ETH profit: (0 - (-150)) - (1 100 - 1 000) = 150 - 100 = +50.00.
        //   Gold: (1 140 - 1 200) - 0 = -60.00. VWCE: 0.
        //   New money: (-150 + 5 000 + 1 200) - (1 000 + 5 000) = 6 050 - 6 000 = +50.00
        //     (1 200 into gold, 1 150 out of ETH).
        //   Value change: 6 140 - 6 100 = +40.00; profit 40 - 50 = -10.00 = 50 - 60 + 0.
        let start = day(
            "2026-08-31",
            vec![
                row("ETH", dec!(0.5), dec!(1100.00), dec!(1000.00)),
                row("VWCE", dec!(10), dec!(5000.00), dec!(5000.00)),
            ],
        );
        let end = day(
            "2026-09-30",
            vec![
                row("ETH", dec!(0), dec!(0.00), dec!(-150.00)),
                row("GOLD_COINS", dec!(1), dec!(1140.00), dec!(1200.00)),
                row("VWCE", dec!(10), dec!(5000.00), dec!(5000.00)),
            ],
        );
        let change = monthly_summary("2026-09".parse().unwrap(), Some(&start), &end)
            .change
            .unwrap();
        assert_eq!(
            (change.value_change, change.new_money, change.profit),
            (dec!(40.00), dec!(50.00), dec!(-10.00))
        );
        assert_eq!(
            change.by_asset,
            vec![
                ("ETH".into(), dec!(50.00)),
                ("VWCE".into(), dec!(0.00)),
                ("GOLD_COINS".into(), dec!(-60.00)),
            ]
        );
    }

    #[test]
    fn first_snapshots_inside_the_month_cover_part_of_it() {
        // Snapshots start on 12 Sep: VWCE 4 000 / 4 000; on 30 Sep 4 100 / 4 000 -> +100.00.
        let start = day(
            "2026-09-12",
            vec![row("VWCE", dec!(8), dec!(4000), dec!(4000))],
        );
        let end = day(
            "2026-09-30",
            vec![row("VWCE", dec!(8), dec!(4100), dec!(4000))],
        );
        let change = monthly_summary("2026-09".parse().unwrap(), Some(&start), &end)
            .change
            .unwrap();
        assert_eq!(
            (change.since, change.whole_month, change.profit),
            (d("2026-09-12"), false, dec!(100))
        );
    }

    #[test]
    fn a_single_snapshot_has_nothing_to_compare() {
        let end = day(
            "2026-09-30",
            vec![row("VWCE", dec!(8), dec!(4100), dec!(4000))],
        );
        let month = "2026-09".parse().unwrap();
        assert_eq!(monthly_summary(month, None, &end).change, None);
        assert_eq!(monthly_summary(month, Some(&end), &end).change, None);
    }

    #[test]
    fn nothing_invested_has_no_percentage() {
        // Only staked ETH, cost 0: profit to date 25.10, no percentage of 0.
        let end = day(
            "2026-09-30",
            vec![row("ETH", dec!(0.002), dec!(25.10), dec!(0))],
        );
        let s = monthly_summary("2026-09".parse().unwrap(), None, &end);
        assert_eq!(
            (s.profit_to_date(), s.profit_to_date_fraction()),
            (dec!(25.10), None)
        );
    }
}
