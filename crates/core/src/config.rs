//! Portfolio settings from `config.toml`: target allocation, IKZE limits, tax rate and the gold
//! buyback factor.
//!
//! Parsing and validation are pure; reading the file is left to the caller. Every number is an
//! exact `Decimal`: the file may write a whole number bare (`55`) or any number as a string
//! (`"0.95"`). TOML floats are rejected so that no value passes through `f64`.

use std::collections::BTreeMap;
use std::fmt;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

use crate::AssetClass;
use crate::gold::MAX_BUYBACK_FACTOR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The text is not valid TOML or does not have the expected fields.
    Parse(String),
    /// A group of target shares does not add up to 100%.
    SharesNot100 { group: &'static str, sum: Decimal },
    /// A target share is below zero.
    NegativeShare { field: &'static str, value: Decimal },
    /// An IKZE limit key is not a plausible year.
    InvalidYear(String),
    /// Two IKZE limit keys name the same year (e.g. `2026` and `"02026"`).
    DuplicateYear(i32),
    /// An IKZE limit is zero or negative.
    NonPositiveLimit { year: i32, value: Decimal },
    /// The tax rate must be above 0% and below 100%.
    InvalidTaxRate(Decimal),
    /// The buyback factor must be above 0 and at most [`MAX_BUYBACK_FACTOR`].
    InvalidBuybackFactor(Decimal),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Parse(msg) => write!(f, "invalid config: {msg}"),
            ConfigError::SharesNot100 { group, sum } => {
                write!(f, "{group} shares must add up to 100%, got {sum}%")
            }
            ConfigError::NegativeShare { field, value } => {
                write!(f, "target share {field} must not be negative, got {value}%")
            }
            ConfigError::InvalidYear(key) => write!(f, "IKZE limit key {key:?} is not a year"),
            ConfigError::DuplicateYear(year) => write!(f, "IKZE limit for {year} is given twice"),
            ConfigError::NonPositiveLimit { year, value } => {
                write!(f, "IKZE limit for {year} must be positive, got {value}")
            }
            ConfigError::InvalidTaxRate(x) => {
                write!(f, "tax rate must be above 0% and below 100%, got {x}%")
            }
            ConfigError::InvalidBuybackFactor(x) => {
                write!(
                    f,
                    "buyback factor must be above 0 and at most {MAX_BUYBACK_FACTOR}, got {x}"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Split of the crypto share between BTC and ETH, in percent of the crypto share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CryptoSplit {
    pub btc_percent: Decimal,
    pub eth_percent: Decimal,
}

/// Validated settings. Construct with [`Config::from_toml`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    targets_percent: BTreeMap<AssetClass, Decimal>,
    crypto_split: CryptoSplit,
    ikze_limits: BTreeMap<i32, Decimal>,
    tax_rate_percent: Decimal,
    gold_buyback_factor: Decimal,
}

impl Config {
    /// Parses and validates the contents of `config.toml`.
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        raw.validate()
    }

    /// Target share of a class, in percent of the portfolio (55 for 55%).
    pub fn target_percent(&self, class: AssetClass) -> Decimal {
        self.targets_percent[&class]
    }

    /// Target share of a class as a fraction of the portfolio (0.55 for 55%).
    pub fn target_fraction(&self, class: AssetClass) -> Decimal {
        self.target_percent(class) / dec!(100)
    }

    pub fn crypto_split(&self) -> CryptoSplit {
        self.crypto_split
    }

    /// BTC target in percent of the whole portfolio: crypto share x BTC split.
    pub fn btc_target_percent(&self) -> Decimal {
        self.target_percent(AssetClass::Crypto) * self.crypto_split.btc_percent / dec!(100)
    }

    /// ETH target in percent of the whole portfolio: crypto share x ETH split.
    pub fn eth_target_percent(&self) -> Decimal {
        self.target_percent(AssetClass::Crypto) * self.crypto_split.eth_percent / dec!(100)
    }

    /// IKZE contribution limit for a calendar year, in PLN, if the config has one.
    pub fn ikze_limit(&self, year: i32) -> Option<Decimal> {
        self.ikze_limits.get(&year).copied()
    }

    /// All configured IKZE limits by year.
    pub fn ikze_limits(&self) -> &BTreeMap<i32, Decimal> {
        &self.ikze_limits
    }

    /// Tax rate in percent (12 for 12%).
    pub fn tax_rate_percent(&self) -> Decimal {
        self.tax_rate_percent
    }

    /// Tax rate as a fraction (0.12 for 12%).
    pub fn tax_rate(&self) -> Decimal {
        self.tax_rate_percent / dec!(100)
    }

    /// Share of melt value a dealer pays for a coin without a manual quote.
    pub fn gold_buyback_factor(&self) -> Decimal {
        self.gold_buyback_factor
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    targets: RawTargets,
    ikze: RawIkze,
    tax: RawTax,
    gold: RawGold,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTargets {
    vwce: Dec,
    crypto: Dec,
    gold: Dec,
    crypto_split: RawSplit,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSplit {
    btc: Dec,
    eth: Dec,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIkze {
    limits: BTreeMap<String, Dec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTax {
    rate_percent: Dec,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGold {
    buyback_factor: Dec,
}

impl RawConfig {
    fn validate(self) -> Result<Config, ConfigError> {
        let t = &self.targets;
        let shares = [
            ("vwce", AssetClass::Vwce, t.vwce.0),
            ("crypto", AssetClass::Crypto, t.crypto.0),
            ("gold", AssetClass::Gold, t.gold.0),
        ];
        check_group("target", &shares.map(|(field, _, v)| (field, v)))?;
        let targets_percent = shares.iter().map(|&(_, class, v)| (class, v)).collect();

        let split = &t.crypto_split;
        check_group(
            "crypto split",
            &[
                ("crypto_split.btc", split.btc.0),
                ("crypto_split.eth", split.eth.0),
            ],
        )?;
        let crypto_split = CryptoSplit {
            btc_percent: split.btc.0,
            eth_percent: split.eth.0,
        };

        let mut ikze_limits = BTreeMap::new();
        for (key, limit) in &self.ikze.limits {
            let year = key
                .trim()
                .parse::<i32>()
                .ok()
                // IKZE exists since 2012; anything far outside this range is a typo.
                .filter(|y| (2012..=2100).contains(y))
                .ok_or_else(|| ConfigError::InvalidYear(key.clone()))?;
            if limit.0 <= Decimal::ZERO {
                return Err(ConfigError::NonPositiveLimit {
                    year,
                    value: limit.0,
                });
            }
            if ikze_limits.insert(year, limit.0).is_some() {
                return Err(ConfigError::DuplicateYear(year));
            }
        }

        let tax_rate_percent = self.tax.rate_percent.0;
        if tax_rate_percent <= Decimal::ZERO || tax_rate_percent >= dec!(100) {
            return Err(ConfigError::InvalidTaxRate(tax_rate_percent));
        }

        let gold_buyback_factor = self.gold.buyback_factor.0;
        if gold_buyback_factor <= Decimal::ZERO || gold_buyback_factor > MAX_BUYBACK_FACTOR {
            return Err(ConfigError::InvalidBuybackFactor(gold_buyback_factor));
        }

        Ok(Config {
            targets_percent,
            crypto_split,
            ikze_limits,
            tax_rate_percent,
            gold_buyback_factor,
        })
    }
}

/// Checks that every share is non-negative and that they add up to exactly 100.
fn check_group(group: &'static str, shares: &[(&'static str, Decimal)]) -> Result<(), ConfigError> {
    for &(field, value) in shares {
        if value < Decimal::ZERO {
            return Err(ConfigError::NegativeShare { field, value });
        }
    }
    let sum: Decimal = shares.iter().map(|&(_, v)| v).sum();
    if sum != dec!(100) {
        return Err(ConfigError::SharesNot100 { group, sum });
    }
    Ok(())
}

/// A decimal read from a TOML integer or string, never from a float.
struct Dec(Decimal);

impl<'de> Deserialize<'de> for Dec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DecVisitor;

        impl Visitor<'_> for DecVisitor {
            type Value = Dec;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a whole number or a decimal string such as \"0.95\"")
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Dec, E> {
                Ok(Dec(Decimal::from(v)))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Dec, E> {
                Ok(Dec(Decimal::from(v)))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Dec, E> {
                Err(E::custom(format!(
                    "write fractional numbers as strings (\"{v}\"), not TOML floats"
                )))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Dec, E> {
                v.trim()
                    .parse::<Decimal>()
                    .map(Dec)
                    .map_err(|_| E::custom(format!("{v:?} is not a decimal number")))
            }
        }

        deserializer.deserialize_any(DecVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
        [targets]
        vwce = 55
        crypto = 25
        gold = 20

        [targets.crypto_split]
        btc = 75
        eth = 25

        [ikze.limits]
        2026 = "11304.00"

        [tax]
        rate_percent = 32

        [gold]
        buyback_factor = "0.95"
    "#;

    fn with(from: &str, to: &str) -> String {
        assert!(VALID.contains(from), "fixture has no {from:?}");
        VALID.replacen(from, to, 1)
    }

    fn err(text: &str) -> ConfigError {
        Config::from_toml(text).expect_err("config should be rejected")
    }

    #[test]
    fn the_committed_config_is_valid() {
        let c = Config::from_toml(include_str!("../../../config.toml")).unwrap();
        assert_eq!(c.target_percent(AssetClass::Vwce), dec!(55));
        assert_eq!(c.ikze_limit(2026), Some(dec!(11304.00)));
    }

    #[test]
    fn reads_every_setting() {
        let c = Config::from_toml(VALID).unwrap();
        assert_eq!(c.target_percent(AssetClass::Vwce), dec!(55));
        assert_eq!(c.target_percent(AssetClass::Crypto), dec!(25));
        assert_eq!(c.target_percent(AssetClass::Gold), dec!(20));
        assert_eq!(
            c.crypto_split(),
            CryptoSplit {
                btc_percent: dec!(75),
                eth_percent: dec!(25)
            }
        );
        assert_eq!(c.ikze_limit(2026), Some(dec!(11304.00)));
        assert_eq!(c.ikze_limit(2025), None);
        assert_eq!(c.tax_rate_percent(), dec!(32));
        assert_eq!(c.gold_buyback_factor(), dec!(0.95));
    }

    #[test]
    fn fractions_are_percent_over_100() {
        let c = Config::from_toml(VALID).unwrap();
        // 55 / 100 = 0.55; 32 / 100 = 0.32.
        assert_eq!(c.target_fraction(AssetClass::Vwce), dec!(0.55));
        assert_eq!(c.tax_rate(), dec!(0.32));
    }

    #[test]
    fn btc_and_eth_targets_are_shares_of_the_crypto_share() {
        let c = Config::from_toml(VALID).unwrap();
        // 25% x 75% = 18.75% of the portfolio; 25% x 25% = 6.25%. Together the crypto 25%.
        assert_eq!(c.btc_target_percent(), dec!(18.75));
        assert_eq!(c.eth_target_percent(), dec!(6.25));
    }

    #[test]
    fn decimal_shares_that_add_up_exactly_are_accepted() {
        // 33.33 + 33.33 + 33.34 = 100 exactly in decimal (not in f64).
        let text = with("vwce = 55", r#"vwce = "33.33""#)
            .replacen("crypto = 25", r#"crypto = "33.33""#, 1)
            .replacen("gold = 20", r#"gold = "33.34""#, 1);
        let c = Config::from_toml(&text).unwrap();
        assert_eq!(c.target_percent(AssetClass::Gold), dec!(33.34));
    }

    #[test]
    fn targets_not_adding_up_to_100_are_rejected() {
        // 55 + 25 + 19.5 = 99.5.
        assert_eq!(
            err(&with("gold = 20", r#"gold = "19.5""#)),
            ConfigError::SharesNot100 {
                group: "target",
                sum: dec!(99.5)
            }
        );
        // 55 + 25 + 21 = 101.
        assert_eq!(
            err(&with("gold = 20", "gold = 21")),
            ConfigError::SharesNot100 {
                group: "target",
                sum: dec!(101)
            }
        );
    }

    #[test]
    fn crypto_split_not_adding_up_to_100_is_rejected() {
        // 75 + 20 = 95.
        assert_eq!(
            err(&with("eth = 25", "eth = 20")),
            ConfigError::SharesNot100 {
                group: "crypto split",
                sum: dec!(95)
            }
        );
    }

    #[test]
    fn negative_share_is_rejected_even_if_the_sum_is_100() {
        // 55 + 50 + (-5) = 100, but a negative target makes no sense.
        let text = with("crypto = 25", "crypto = 50").replacen("gold = 20", "gold = -5", 1);
        assert_eq!(
            err(&text),
            ConfigError::NegativeShare {
                field: "gold",
                value: dec!(-5)
            }
        );
    }

    #[test]
    fn zero_share_is_allowed() {
        let text = with("crypto = 25", "crypto = 45").replacen("gold = 20", "gold = 0", 1);
        let c = Config::from_toml(&text).unwrap();
        assert_eq!(c.target_percent(AssetClass::Gold), Decimal::ZERO);
    }

    #[test]
    fn toml_floats_are_rejected() {
        let e = err(&with(r#"buyback_factor = "0.95""#, "buyback_factor = 0.95"));
        assert!(
            matches!(&e, ConfigError::Parse(m) if m.contains("strings")),
            "{e}"
        );
    }

    #[test]
    fn missing_class_and_unknown_fields_are_rejected() {
        assert!(matches!(
            err(&with("gold = 20\n", "")),
            ConfigError::Parse(_)
        ));
        assert!(matches!(
            err(&with("gold = 20", "gold = 20\nbonds = 0")),
            ConfigError::Parse(_)
        ));
    }

    #[test]
    fn several_ikze_years_are_kept() {
        let c = Config::from_toml(&with(
            r#"2026 = "11304.00""#,
            "2026 = \"11304.00\"\n2027 = 12000",
        ))
        .unwrap();
        assert_eq!(c.ikze_limits().len(), 2);
        assert_eq!(c.ikze_limit(2027), Some(dec!(12000)));
    }

    #[test]
    fn invalid_ikze_entries_are_rejected() {
        assert_eq!(
            err(&with(r#"2026 = "11304.00""#, r#"twenty = "1""#)),
            ConfigError::InvalidYear("twenty".into())
        );
        assert_eq!(
            err(&with(r#"2026 = "11304.00""#, r#"1999 = "1""#)),
            ConfigError::InvalidYear("1999".into())
        );
        assert_eq!(
            err(&with(r#"2026 = "11304.00""#, "2026 = 0")),
            ConfigError::NonPositiveLimit {
                year: 2026,
                value: Decimal::ZERO
            }
        );
        assert_eq!(
            err(&with(r#"2026 = "11304.00""#, "2026 = 1\n\"02026\" = 2")),
            ConfigError::DuplicateYear(2026)
        );
    }

    #[test]
    fn tax_rate_must_be_between_0_and_100() {
        for bad in ["0", "100", "-12"] {
            assert_eq!(
                err(&with("rate_percent = 32", &format!("rate_percent = {bad}"))),
                ConfigError::InvalidTaxRate(bad.parse().unwrap())
            );
        }
        let c = Config::from_toml(&with("rate_percent = 32", "rate_percent = 12")).unwrap();
        assert_eq!(c.tax_rate(), dec!(0.12));
    }

    #[test]
    fn buyback_factor_must_be_above_0_and_at_most_1_5() {
        for bad in ["0", "1.51", "-0.5"] {
            assert_eq!(
                err(&with(
                    r#"buyback_factor = "0.95""#,
                    &format!("buyback_factor = \"{bad}\"")
                )),
                ConfigError::InvalidBuybackFactor(bad.parse().unwrap())
            );
        }
        let c = Config::from_toml(&with(
            r#"buyback_factor = "0.95""#,
            r#"buyback_factor = "1.5""#,
        ))
        .unwrap();
        assert_eq!(c.gold_buyback_factor(), dec!(1.5));
    }

    #[test]
    fn bad_decimal_string_is_a_parse_error() {
        assert!(matches!(
            err(&with(
                r#"buyback_factor = "0.95""#,
                r#"buyback_factor = "0,95""#
            )),
            ConfigError::Parse(_)
        ));
    }
}
