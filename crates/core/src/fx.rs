//! Exchange rates: NBP table A mid rates, quoted as PLN per one unit of a foreign currency.

use chrono::NaiveDate;
use rust_decimal::Decimal;

/// One published NBP table A mid rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxRate {
    /// ISO 4217 code, upper case, never "PLN".
    pub currency: String,
    /// The day the table was published for (NBP "effectiveDate").
    pub effective_date: NaiveDate,
    /// PLN per 1 unit of `currency`.
    pub mid: Decimal,
    /// NBP table number, e.g. "200/A/NBP/2026".
    pub table_no: String,
}

impl FxRate {
    /// Converts an amount in `self.currency` to PLN. Not rounded: rounding is a presentation
    /// concern.
    pub fn to_pln(&self, amount: Decimal) -> Decimal {
        amount * self.mid
    }
}

/// The rate that applies on `day`: the latest one published on or before it.
///
/// NBP publishes on business days only, so a weekend or holiday uses the previous business
/// day's table. `rates` may be in any order and should hold a single currency.
pub fn rate_on(rates: &[FxRate], day: NaiveDate) -> Option<&FxRate> {
    rates
        .iter()
        .filter(|r| r.effective_date <= day)
        .max_by_key(|r| r.effective_date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::from_str(s).unwrap()
    }

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn eur(date: &str, mid: &str, no: &str) -> FxRate {
        FxRate {
            currency: "EUR".into(),
            effective_date: d(date),
            mid: dec(mid),
            table_no: no.into(),
        }
    }

    // Real NBP EUR mids, 2026-10-08 (Thu) to 2026-10-12 (Mon), deliberately out of order.
    fn week() -> Vec<FxRate> {
        vec![
            eur("2026-10-12", "4.3889", "198/A/NBP/2026"),
            eur("2026-10-08", "4.3789", "196/A/NBP/2026"),
            eur("2026-10-09", "4.3775", "197/A/NBP/2026"),
        ]
    }

    #[test]
    fn business_day_uses_its_own_table() {
        let rates = week();
        assert_eq!(
            rate_on(&rates, d("2026-10-09")).unwrap().table_no,
            "197/A/NBP/2026"
        );
    }

    #[test]
    fn weekend_uses_previous_friday() {
        let rates = week();
        for day in ["2026-10-10", "2026-10-11"] {
            let rate = rate_on(&rates, d(day)).unwrap();
            assert_eq!(rate.effective_date, d("2026-10-09"), "{day}");
            assert_eq!(rate.mid, dec("4.3775"));
        }
    }

    #[test]
    fn day_before_all_rates_has_none() {
        assert_eq!(rate_on(&week(), d("2026-10-07")), None);
    }

    #[test]
    fn empty_rates_have_none() {
        assert_eq!(rate_on(&[], d("2026-10-09")), None);
    }

    #[test]
    fn converts_to_pln_exactly() {
        let rate = eur("2026-10-14", "4.3706", "200/A/NBP/2026");
        // 100 EUR * 4.3706 = 437.06 PLN; 12.34 EUR * 4.3706 = 53.933204 PLN (by hand).
        assert_eq!(rate.to_pln(dec("100")), dec("437.06"));
        assert_eq!(rate.to_pln(dec("12.34")), dec("53.933204"));
        assert_eq!(rate.to_pln(Decimal::ZERO), Decimal::ZERO);
    }
}
