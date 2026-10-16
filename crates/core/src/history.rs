//! Value against net contributions over time, for the home page chart.
//!
//! One point per month keeps the number of price lookups bounded (about a dozen a year): the
//! first contribution's day, the end of every month after it, and today. Each point is valued
//! like the report itself ([`crate::portfolio`]); a point with an unpriced position has no value
//! rather than an understated one (the same rule as the P&L).
//!
//! Results are not rounded.

use chrono::{Datelike, Days, NaiveDate};
use rust_decimal::Decimal;

use crate::portfolio::{LedgerTx, holdings_on, net_contributions_on, value};

/// The chart's days: `first`, the last day of every month from `first`'s month up to the month
/// before `today`'s, and `today`; ascending, without duplicates. Empty when `first` is after
/// `today`.
pub fn chart_dates(first: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
    if first > today {
        return Vec::new();
    }
    let mut dates = vec![first];
    let mut month_end = last_day_of_month(first);
    while month_end < today {
        if month_end > first {
            dates.push(month_end);
        }
        month_end = last_day_of_month(month_end + Days::new(1));
    }
    if today > first {
        dates.push(today);
    }
    dates
}

fn last_day_of_month(day: NaiveDate) -> NaiveDate {
    let (y, m) = if day.month() == 12 {
        (day.year() + 1, 1)
    } else {
        (day.year(), day.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).expect("first of a month") - Days::new(1)
}

/// The portfolio on one chart day, in PLN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValuePoint {
    pub date: NaiveDate,
    /// Net contributions up to the end of the day; None when a flow's rate is missing.
    pub contributions: Option<Decimal>,
    /// Value at the end of the day; None when some position or cash currency has no price.
    pub value: Option<Decimal>,
    /// What had no price or rate (for the page's note).
    pub missing: Vec<String>,
}

/// Values the ledger on `date`. `rate(currency, day)` gives PLN per unit for flows;
/// `position_value(asset, quantity)` and `cash_rate(currency)` value the holdings on `date`
/// (as in [`crate::portfolio::value`]).
pub fn value_point(
    txs: &[LedgerTx],
    date: NaiveDate,
    rate: impl FnMut(&str, NaiveDate) -> Option<Decimal>,
    position_value: impl FnMut(&str, Decimal) -> Option<Decimal>,
    cash_rate: impl FnMut(&str) -> Option<Decimal>,
) -> ValuePoint {
    let holdings = holdings_on(txs, date);
    let valuation = value(&holdings, position_value, cash_rate);
    let mut missing = valuation.missing.clone();
    let contributions = match net_contributions_on(txs, date, rate) {
        Ok(c) => Some(c.total),
        Err(e) => {
            missing.push(e.to_string());
            None
        }
    };
    ValuePoint {
        date,
        contributions,
        value: valuation.complete().then_some(valuation.total),
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::{AccountKind, TxKind};
    use rust_decimal_macros::dec;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn chart_dates_are_the_first_day_month_ends_and_today() {
        // 20 Nov 2025 to 10 Feb 2026: the first day, Nov, Dec and Jan month ends, then today.
        assert_eq!(
            chart_dates(d("2025-11-20"), d("2026-02-10")),
            [
                d("2025-11-20"),
                d("2025-11-30"),
                d("2025-12-31"),
                d("2026-01-31"),
                d("2026-02-10")
            ]
        );
        // February of a leap year ends on the 29th.
        assert_eq!(
            chart_dates(d("2024-01-31"), d("2024-03-01")),
            [d("2024-01-31"), d("2024-02-29"), d("2024-03-01")]
        );
    }

    #[test]
    fn chart_dates_have_no_duplicates_at_the_edges() {
        // Today is a month end: it is not listed twice.
        assert_eq!(
            chart_dates(d("2026-01-15"), d("2026-02-28")),
            [d("2026-01-15"), d("2026-01-31"), d("2026-02-28")]
        );
        // Same month, same day, and a first day after today.
        assert_eq!(
            chart_dates(d("2026-10-01"), d("2026-10-16")),
            [d("2026-10-01"), d("2026-10-16")]
        );
        assert_eq!(
            chart_dates(d("2026-10-16"), d("2026-10-16")),
            [d("2026-10-16")]
        );
        assert!(chart_dates(d("2026-10-17"), d("2026-10-16")).is_empty());
    }

    fn tx(
        date: &str,
        kind: TxKind,
        asset: Option<&str>,
        quantity: Option<Decimal>,
        amount: Decimal,
        currency: &str,
    ) -> LedgerTx {
        LedgerTx {
            date: d(date),
            account: "Kraken".into(),
            account_kind: AccountKind::Funded,
            wrapper: None,
            kind,
            asset: asset.map(str::to_owned),
            quantity,
            amount,
            currency: currency.into(),
        }
    }

    fn ledger() -> Vec<LedgerTx> {
        // 100 EUR in on 2 Jan; 0.001 BTC for 90 EUR on 3 Jan; 50 EUR more on 10 Feb.
        vec![
            tx("2026-01-02", TxKind::Deposit, None, None, dec!(100), "EUR"),
            tx(
                "2026-01-03",
                TxKind::Buy,
                Some("BTC"),
                Some(dec!(0.001)),
                dec!(90),
                "EUR",
            ),
            tx("2026-02-10", TxKind::Deposit, None, None, dec!(50), "EUR"),
        ]
    }

    #[test]
    fn a_point_is_net_contributions_and_value_on_its_day() {
        // 31 Jan: contributions 100 EUR x 4.20 (rate of 2 Jan) = 420. Holdings 0.001 BTC worth
        // 380 PLN and 10 EUR cash x 4.25 (rate of 31 Jan) = 42.5: value 422.5. The 10 Feb
        // deposit is later and does not count.
        let rates = |_: &str, day: NaiveDate| {
            Some(if day == d("2026-01-02") {
                dec!(4.20)
            } else {
                dec!(4.30)
            })
        };
        let p = value_point(
            &ledger(),
            d("2026-01-31"),
            rates,
            |asset, qty| (asset == "BTC" && qty == dec!(0.001)).then_some(dec!(380)),
            |_| Some(dec!(4.25)),
        );
        assert_eq!(
            p,
            ValuePoint {
                date: d("2026-01-31"),
                contributions: Some(dec!(420.00)),
                value: Some(dec!(422.50)),
                missing: vec![],
            }
        );
    }

    #[test]
    fn an_unpriced_position_leaves_the_point_without_a_value() {
        // No BTC price: the 42.5 of cash alone would understate the value, so there is none.
        let p = value_point(
            &ledger(),
            d("2026-01-31"),
            |_, _| Some(dec!(4.20)),
            |_, _| None,
            |_| Some(dec!(4.25)),
        );
        assert_eq!((p.contributions, p.value), (Some(dec!(420.00)), None));
        assert_eq!(p.missing, ["BTC"]);
    }

    #[test]
    fn a_missing_flow_rate_leaves_the_point_without_contributions() {
        let p = value_point(
            &ledger(),
            d("2026-01-31"),
            |_, _| None,
            |_, _| Some(dec!(380)),
            |_| Some(dec!(4.25)),
        );
        assert_eq!((p.contributions, p.value), (None, Some(dec!(422.50))));
        assert_eq!(p.missing, ["no EUR->PLN rate for 2026-01-02"]);
    }
}
