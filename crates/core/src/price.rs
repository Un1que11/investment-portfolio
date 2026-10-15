//! Market prices and their conversion to PLN.

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::fx::FxRate;

/// One trading day's closing price, in the instrument's quote currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyClose {
    pub date: NaiveDate,
    pub close: Decimal,
}

/// A closing price that applies on some day, with its currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceQuote {
    /// The trading day the close is from (may be before the day asked for).
    pub date: NaiveDate,
    pub close: Decimal,
    /// ISO 4217 code of `close`.
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrencyMismatch {
    pub quote: String,
    pub rate: String,
}

impl std::fmt::Display for CurrencyMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "price is in {} but the rate is for {}",
            self.quote, self.rate
        )
    }
}

impl std::error::Error for CurrencyMismatch {}

impl PriceQuote {
    /// The close converted to PLN with `fx` (PLN per unit of the quote currency). Unrounded.
    pub fn to_pln(&self, fx: &FxRate) -> Result<Decimal, CurrencyMismatch> {
        if !self.currency.eq_ignore_ascii_case(&fx.currency) {
            return Err(CurrencyMismatch {
                quote: self.currency.clone(),
                rate: fx.currency.clone(),
            });
        }
        Ok(fx.to_pln(self.close))
    }
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

    fn eur_rate(mid: &str) -> FxRate {
        FxRate {
            currency: "EUR".into(),
            effective_date: d("2026-10-09"),
            mid: dec(mid),
            table_no: "197/A/NBP/2026".into(),
        }
    }

    #[test]
    fn converts_eur_close_to_pln() {
        let quote = PriceQuote {
            date: d("2026-10-09"),
            close: dec("170.12"),
            currency: "EUR".into(),
        };
        // 170.12 * 4.3775 = 744.700300 (by hand: 170 * 4.3775 = 744.175; 0.12 * 4.3775 = 0.5253).
        assert_eq!(
            quote.to_pln(&eur_rate("4.3775")).unwrap(),
            dec("744.700300")
        );
    }

    #[test]
    fn refuses_to_convert_with_rate_for_other_currency() {
        let quote = PriceQuote {
            date: d("2026-10-09"),
            close: dec("100"),
            currency: "USD".into(),
        };
        assert_eq!(
            quote.to_pln(&eur_rate("4.3775")),
            Err(CurrencyMismatch {
                quote: "USD".into(),
                rate: "EUR".into()
            })
        );
    }
}
