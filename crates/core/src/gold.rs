//! Physical gold: valuing coins from the NBP price per gram.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Fine gold in a 1/10 troy ounce coin, in grams, as specified for this portfolio (1 troy oz is
/// 31.1034768 g, so 1/10 oz is 3.11035 g; 3.11 g is the agreed working weight).
pub const TENTH_OUNCE_GRAMS: Decimal = dec!(3.11);

/// Highest buyback factor accepted, here and in `config.toml`.
pub const MAX_BUYBACK_FACTOR: Decimal = dec!(1.5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldError {
    /// The buyback factor must be in (0, [`MAX_BUYBACK_FACTOR`]].
    InvalidBuybackFactor(Decimal),
    /// Weights and prices must be positive.
    NonPositive(&'static str, Decimal),
    /// Price x weight (x the factor) is past what `Decimal` holds: a weight or price far beyond
    /// any coin.
    Overflow,
}

impl std::fmt::Display for GoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoldError::InvalidBuybackFactor(x) => {
                write!(
                    f,
                    "buyback factor must be above 0 and at most {MAX_BUYBACK_FACTOR}, got {x}"
                )
            }
            GoldError::NonPositive(what, x) => write!(f, "{what} must be positive, got {x}"),
            GoldError::Overflow => write!(f, "coin value is too large to compute"),
        }
    }
}

impl std::error::Error for GoldError {}

/// Where a coin's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValuationBasis {
    /// NBP price per gram x fine weight x buyback factor.
    NbpPrice,
    /// A dealer quote entered by hand, used as is.
    ManualQuote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinValuation {
    /// Melt value: price per gram x fine weight.
    pub melt_value_pln: Decimal,
    /// What the coin is counted at in the portfolio.
    pub value_pln: Decimal,
    pub basis: ValuationBasis,
}

/// Values one coin. `buyback_factor` is the share of melt value a dealer pays (e.g. 0.95).
/// A `manual_quote` in PLN, when given, replaces the computed value; the melt value is still
/// reported for comparison. Results are not rounded.
pub fn value_coin(
    price_per_gram_pln: Decimal,
    fine_weight_grams: Decimal,
    buyback_factor: Decimal,
    manual_quote_pln: Option<Decimal>,
) -> Result<CoinValuation, GoldError> {
    if price_per_gram_pln <= Decimal::ZERO {
        return Err(GoldError::NonPositive("gold price", price_per_gram_pln));
    }
    if fine_weight_grams <= Decimal::ZERO {
        return Err(GoldError::NonPositive("coin weight", fine_weight_grams));
    }
    if buyback_factor <= Decimal::ZERO || buyback_factor > MAX_BUYBACK_FACTOR {
        return Err(GoldError::InvalidBuybackFactor(buyback_factor));
    }
    let melt = price_per_gram_pln
        .checked_mul(fine_weight_grams)
        .ok_or(GoldError::Overflow)?;
    match manual_quote_pln {
        Some(quote) if quote <= Decimal::ZERO => Err(GoldError::NonPositive("manual quote", quote)),
        Some(quote) => Ok(CoinValuation {
            melt_value_pln: melt,
            value_pln: quote,
            basis: ValuationBasis::ManualQuote,
        }),
        None => Ok(CoinValuation {
            melt_value_pln: melt,
            value_pln: melt
                .checked_mul(buyback_factor)
                .ok_or(GoldError::Overflow)?,
            basis: ValuationBasis::NbpPrice,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_tenth_ounce_coin_from_nbp_price() {
        // NBP 2026-10-14: 527.24 PLN/g. Melt: 527.24 * 3.11 = 1639.7164
        // (527.24 * 3 = 1581.72; 527.24 * 0.11 = 57.9964). At 0.95: 1557.73058.
        let v = value_coin(dec!(527.24), TENTH_OUNCE_GRAMS, dec!(0.95), None).unwrap();
        assert_eq!(v.melt_value_pln, dec!(1639.7164));
        assert_eq!(v.value_pln, dec!(1557.73058));
        assert_eq!(v.basis, ValuationBasis::NbpPrice);
    }

    #[test]
    fn factor_of_one_is_melt_value() {
        let v = value_coin(dec!(500), dec!(3.11), dec!(1), None).unwrap();
        assert_eq!(v.value_pln, dec!(1555));
    }

    #[test]
    fn manual_quote_replaces_computed_value() {
        let v = value_coin(
            dec!(527.24),
            TENTH_OUNCE_GRAMS,
            dec!(0.95),
            Some(dec!(1600)),
        )
        .unwrap();
        assert_eq!(v.value_pln, dec!(1600));
        assert_eq!(v.melt_value_pln, dec!(1639.7164));
        assert_eq!(v.basis, ValuationBasis::ManualQuote);
    }

    #[test]
    fn rejects_invalid_inputs() {
        let w = TENTH_OUNCE_GRAMS;
        assert_eq!(
            value_coin(dec!(527), w, dec!(0), None),
            Err(GoldError::InvalidBuybackFactor(dec!(0)))
        );
        assert_eq!(
            value_coin(dec!(527), w, dec!(1.6), None),
            Err(GoldError::InvalidBuybackFactor(dec!(1.6)))
        );
        assert!(value_coin(dec!(0), w, dec!(0.95), None).is_err());
        assert!(value_coin(dec!(527), dec!(-1), dec!(0.95), None).is_err());
        assert!(value_coin(dec!(527), w, dec!(0.95), Some(dec!(0))).is_err());
        // The upper bound itself is allowed.
        assert!(value_coin(dec!(527), w, dec!(1.5), None).is_ok());
    }

    #[test]
    fn a_value_past_decimal_is_an_error_not_a_panic() {
        // Decimal holds up to about 7.9 x 10^28: 400 x 10^27 is past it.
        let huge = Decimal::from_i128_with_scale(10_i128.pow(27), 0);
        assert_eq!(
            value_coin(dec!(400), huge, dec!(0.95), None),
            Err(GoldError::Overflow)
        );
        // 7 x 10^28 g x 1 zł/g fits as melt value, but x 1.5 does not.
        let near_max = Decimal::from_i128_with_scale(7 * 10_i128.pow(28), 0);
        assert_eq!(
            value_coin(dec!(1), near_max, dec!(1.5), None),
            Err(GoldError::Overflow)
        );
        // With a dealer quote the melt value is still computed, so it fails the same way.
        assert_eq!(
            value_coin(dec!(400), huge, dec!(0.95), Some(dec!(1300))),
            Err(GoldError::Overflow)
        );
    }
}
