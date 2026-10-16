//! How far the current allocation is from the target in `config.toml`: per asset class, and the
//! BTC/ETH split within crypto.
//!
//! - Classes: each class's value as a share of the three classes together, against its target
//!   share. Cash left in accounts belongs to no class and is left out of the base, so the
//!   deviations add up to zero (in percentage points and in PLN).
//! - Within crypto: BTC and ETH as shares of the crypto value, against the crypto split. The PLN
//!   deviation there is what would have to move between the two to match the split with the
//!   crypto total unchanged.
//!
//! A deviation is actual minus target: positive means overweight, negative underweight.
//! Results are not rounded.

use std::collections::BTreeMap;
use std::fmt;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::AssetClass;
use crate::config::Config;
use crate::portfolio::{Holdings, Valuation};

/// The positions targets are set for: the three classes, with crypto split into its coins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sleeve {
    Vwce,
    Btc,
    Eth,
    Gold,
}

impl Sleeve {
    pub fn class(self) -> AssetClass {
        match self {
            Sleeve::Vwce => AssetClass::Vwce,
            Sleeve::Btc | Sleeve::Eth => AssetClass::Crypto,
            Sleeve::Gold => AssetClass::Gold,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Sleeve::Vwce => "vwce",
            Sleeve::Btc => "btc",
            Sleeve::Eth => "eth",
            Sleeve::Gold => "gold",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deviation {
    /// PLN.
    pub value: Decimal,
    /// Value / base (0.48 = 48%).
    pub share: Decimal,
    /// Target share of the same base.
    pub target_share: Decimal,
    /// Target share x base, PLN.
    pub target_value: Decimal,
}

impl Deviation {
    fn new(value: Decimal, base: Decimal, target_share: Decimal) -> Deviation {
        Deviation {
            value,
            share: value / base,
            target_share,
            target_value: target_share * base,
        }
    }

    /// Share minus target share (0.03 = 3 percentage points over).
    pub fn points(&self) -> Decimal {
        self.share - self.target_share
    }

    /// Value minus target value, PLN (positive = overweight).
    pub fn pln(&self) -> Decimal {
        self.value - self.target_value
    }
}

/// BTC and ETH against the crypto split, as shares of the crypto value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CryptoSplitDeviation {
    pub btc: Deviation,
    pub eth: Deviation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// PLN in the three classes together (cash left out): the base of the class shares.
    pub total: Decimal,
    /// Every class, held or not.
    pub by_class: BTreeMap<AssetClass, Deviation>,
    /// None when no crypto is held (there is nothing to split).
    pub within_crypto: Option<CryptoSplitDeviation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocationError {
    /// Held assets that have no target (symbols).
    Unclassified(Vec<String>),
    /// Held assets with no price on the date (symbols): the shares would be wrong.
    Unpriced(Vec<String>),
    /// No asset is held, or all are worth nothing: there is no base to take shares of.
    NothingHeld,
}

impl fmt::Display for AllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AllocationError::Unclassified(s) => write!(f, "no target for {}", s.join(", ")),
            AllocationError::Unpriced(s) => write!(f, "no price for {}", s.join(", ")),
            AllocationError::NothingHeld => f.write_str("no assets held"),
        }
    }
}

impl std::error::Error for AllocationError {}

/// Compares the held assets' values with the targets. `sleeve(symbol)` says which target an
/// asset counts towards; cash (and anything missing from `holdings`) is not part of it.
pub fn allocation(
    holdings: &Holdings,
    valuation: &Valuation,
    config: &Config,
    sleeve: impl Fn(&str) -> Option<Sleeve>,
) -> Result<Allocation, AllocationError> {
    allocation_of(&sleeve_values(holdings, valuation, sleeve)?, config)
}

/// PLN value of each sleeve held (sleeves not held are absent). `sleeve(symbol)` says which
/// target an asset counts towards; cash (and anything missing from `holdings`) is not part of it.
pub fn sleeve_values(
    holdings: &Holdings,
    valuation: &Valuation,
    sleeve: impl Fn(&str) -> Option<Sleeve>,
) -> Result<BTreeMap<Sleeve, Decimal>, AllocationError> {
    let mut values: BTreeMap<Sleeve, Decimal> = BTreeMap::new();
    let mut unclassified = Vec::new();
    let mut unpriced = Vec::new();
    for asset in holdings.assets.keys() {
        match (sleeve(asset), valuation.by_asset.get(asset)) {
            (None, _) => unclassified.push(asset.clone()),
            (Some(_), None) => unpriced.push(asset.clone()),
            (Some(s), Some(v)) => *values.entry(s).or_default() += v,
        }
    }
    if !unclassified.is_empty() {
        return Err(AllocationError::Unclassified(unclassified));
    }
    if !unpriced.is_empty() {
        return Err(AllocationError::Unpriced(unpriced));
    }
    Ok(values)
}

/// Compares sleeve values (PLN; a missing sleeve is worth nothing) with the targets.
pub fn allocation_of(
    values: &BTreeMap<Sleeve, Decimal>,
    config: &Config,
) -> Result<Allocation, AllocationError> {
    let value = |s: Sleeve| values.get(&s).copied().unwrap_or_default();
    let class_value = |class: AssetClass| -> Decimal {
        values
            .iter()
            .filter(|(s, _)| s.class() == class)
            .map(|(_, v)| *v)
            .sum()
    };
    let total: Decimal = values.values().copied().sum();
    if total <= Decimal::ZERO {
        return Err(AllocationError::NothingHeld);
    }

    let by_class = AssetClass::ALL
        .into_iter()
        .map(|class| {
            let d = Deviation::new(class_value(class), total, config.target_fraction(class));
            (class, d)
        })
        .collect();

    let crypto = class_value(AssetClass::Crypto);
    let split = config.crypto_split();
    let within_crypto = (crypto > Decimal::ZERO).then(|| CryptoSplitDeviation {
        btc: Deviation::new(value(Sleeve::Btc), crypto, split.btc_percent / dec!(100)),
        eth: Deviation::new(value(Sleeve::Eth), crypto, split.eth_percent / dec!(100)),
    });

    Ok(Allocation {
        total,
        by_class,
        within_crypto,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vwce: u32, crypto: u32, gold: u32, btc: u32, eth: u32) -> Config {
        Config::from_toml(&format!(
            r#"
            [targets]
            vwce = {vwce}
            crypto = {crypto}
            gold = {gold}
            [targets.crypto_split]
            btc = {btc}
            eth = {eth}
            [ikze.limits]
            2026 = "11304.00"
            [tax]
            rate_percent = 12
            [gold]
            buyback_factor = "0.95"
            "#
        ))
        .unwrap()
    }

    /// The targets of the repository's `config.toml`: 55/25/20, crypto 75 BTC / 25 ETH.
    fn targets() -> Config {
        config(55, 25, 20, 75, 25)
    }

    fn sleeve(symbol: &str) -> Option<Sleeve> {
        match symbol {
            "VWCE" => Some(Sleeve::Vwce),
            "BTC" => Some(Sleeve::Btc),
            "ETH" => Some(Sleeve::Eth),
            "GOLD_COINS" => Some(Sleeve::Gold),
            _ => None,
        }
    }

    /// Holdings and their values; the quantities do not matter here, only which assets are held.
    /// `cash` PLN sits in an account.
    fn held(values: &[(&str, Option<Decimal>)], cash: Decimal) -> (Holdings, Valuation) {
        let mut h = Holdings::default();
        let mut v = Valuation::default();
        for (asset, value) in values {
            h.assets.insert((*asset).to_owned(), Decimal::ONE);
            match value {
                Some(x) => {
                    v.by_asset.insert((*asset).to_owned(), *x);
                    v.total += x;
                }
                None => v.missing.push((*asset).to_owned()),
            }
        }
        if !cash.is_zero() {
            h.cash.insert(("XTB".into(), "PLN".into()), cash);
            v.cash.insert(("XTB".into(), "PLN".into()), cash);
            v.total += cash;
        }
        (h, v)
    }

    fn dev(value: Decimal, share: Decimal, target_share: Decimal, target: Decimal) -> Deviation {
        Deviation {
            value,
            share,
            target_share,
            target_value: target,
        }
    }

    #[test]
    fn deviations_per_class_and_within_crypto() {
        // VWCE 4800, BTC 3200, ETH 800, gold 1200, plus 500 PLN of cash that counts nowhere:
        // the base is 4800 + 4000 + 1200 = 10 000.
        let (h, v) = held(
            &[
                ("VWCE", Some(dec!(4800))),
                ("BTC", Some(dec!(3200))),
                ("ETH", Some(dec!(800))),
                ("GOLD_COINS", Some(dec!(1200))),
            ],
            dec!(500),
        );
        let a = allocation(&h, &v, &targets(), sleeve).unwrap();

        assert_eq!(a.total, dec!(10000));
        // VWCE 48% vs 55%: -7 pp, 4800 - 5500 = -700 PLN.
        let vwce = a.by_class[&AssetClass::Vwce];
        assert_eq!(vwce, dev(dec!(4800), dec!(0.48), dec!(0.55), dec!(5500)));
        assert_eq!((vwce.points(), vwce.pln()), (dec!(-0.07), dec!(-700)));
        // Crypto 40% vs 25%: +15 pp, 4000 - 2500 = +1500 PLN.
        let crypto = a.by_class[&AssetClass::Crypto];
        assert_eq!(crypto, dev(dec!(4000), dec!(0.4), dec!(0.25), dec!(2500)));
        assert_eq!((crypto.points(), crypto.pln()), (dec!(0.15), dec!(1500)));
        // Gold 12% vs 20%: -8 pp, 1200 - 2000 = -800 PLN.
        let gold = a.by_class[&AssetClass::Gold];
        assert_eq!(gold, dev(dec!(1200), dec!(0.12), dec!(0.2), dec!(2000)));
        assert_eq!((gold.points(), gold.pln()), (dec!(-0.08), dec!(-800)));

        // Within the 4000 of crypto: BTC 80% vs 75% (+5 pp, 3200 - 3000 = +200 PLN), ETH 20%
        // vs 25% (-5 pp, 800 - 1000 = -200 PLN): 200 PLN of BTC would have to become ETH.
        let split = a.within_crypto.unwrap();
        assert_eq!(
            split.btc,
            dev(dec!(3200), dec!(0.8), dec!(0.75), dec!(3000))
        );
        assert_eq!(
            (split.btc.points(), split.btc.pln()),
            (dec!(0.05), dec!(200))
        );
        assert_eq!(split.eth, dev(dec!(800), dec!(0.2), dec!(0.25), dec!(1000)));
        assert_eq!(
            (split.eth.points(), split.eth.pln()),
            (dec!(-0.05), dec!(-200))
        );
    }

    #[test]
    fn deviations_add_up_to_zero() {
        // 1000 each of VWCE, BTC and gold: thirds, which do not terminate as decimals.
        let (h, v) = held(
            &[
                ("VWCE", Some(dec!(1000))),
                ("BTC", Some(dec!(1000))),
                ("GOLD_COINS", Some(dec!(1000))),
            ],
            dec!(0),
        );
        let a = allocation(&h, &v, &targets(), sleeve).unwrap();

        // Targets of 3000: 1650, 750, 600 -> -650, +250, +400 PLN; 33.33% each vs 55/25/20 ->
        // -21.67, +8.33, +13.33 pp.
        let pln: Vec<_> = a.by_class.values().map(Deviation::pln).collect();
        assert_eq!(pln, [dec!(-650), dec!(250), dec!(400)]);
        let points: Vec<_> = a
            .by_class
            .values()
            .map(|d| d.points().round_dp(4))
            .collect();
        assert_eq!(points, [dec!(-0.2167), dec!(0.0833), dec!(0.1333)]);
        assert_eq!(pln.iter().sum::<Decimal>(), dec!(0));
        assert_eq!(
            a.by_class
                .values()
                .map(Deviation::points)
                .sum::<Decimal>()
                .round_dp(20),
            dec!(0)
        );
        // All the crypto is BTC: +25 pp and +250 PLN over the split, ETH the opposite.
        let split = a.within_crypto.unwrap();
        assert_eq!(
            (split.btc.points(), split.btc.pln()),
            (dec!(0.25), dec!(250))
        );
        assert_eq!(
            (split.eth.points(), split.eth.pln()),
            (dec!(-0.25), dec!(-250))
        );
    }

    #[test]
    fn targets_come_from_the_config() {
        // 60/30/10 with crypto split 50/50. VWCE 6000, ETH 3000, gold 1000 of 10 000: every
        // class is on target, but the crypto is all ETH.
        let (h, v) = held(
            &[
                ("VWCE", Some(dec!(6000))),
                ("ETH", Some(dec!(3000))),
                ("GOLD_COINS", Some(dec!(1000))),
            ],
            dec!(0),
        );
        let a = allocation(&h, &v, &config(60, 30, 10, 50, 50), sleeve).unwrap();

        for d in a.by_class.values() {
            assert_eq!((d.points(), d.pln()), (dec!(0), dec!(0)));
        }
        assert_eq!(a.by_class[&AssetClass::Crypto].target_value, dec!(3000));
        // BTC 0% vs 50%: -50 pp, 0 - 1500 = -1500 PLN; ETH +50 pp, 3000 - 1500 = +1500 PLN.
        let split = a.within_crypto.unwrap();
        assert_eq!(split.btc, dev(dec!(0), dec!(0), dec!(0.5), dec!(1500)));
        assert_eq!(
            (split.btc.points(), split.btc.pln()),
            (dec!(-0.5), dec!(-1500))
        );
        assert_eq!(
            (split.eth.points(), split.eth.pln()),
            (dec!(0.5), dec!(1500))
        );
    }

    #[test]
    fn a_class_not_held_is_fully_underweight() {
        // VWCE 7000 and gold 3000, no crypto: crypto is 0% vs 25%, -2500 PLN; VWCE 70% (+15 pp,
        // +1500), gold 30% (+10 pp, +1000). Nothing to split within crypto.
        let (h, v) = held(
            &[("VWCE", Some(dec!(7000))), ("GOLD_COINS", Some(dec!(3000)))],
            dec!(0),
        );
        let a = allocation(&h, &v, &targets(), sleeve).unwrap();

        let crypto = a.by_class[&AssetClass::Crypto];
        assert_eq!(crypto, dev(dec!(0), dec!(0), dec!(0.25), dec!(2500)));
        assert_eq!((crypto.points(), crypto.pln()), (dec!(-0.25), dec!(-2500)));
        let vwce = a.by_class[&AssetClass::Vwce];
        assert_eq!((vwce.points(), vwce.pln()), (dec!(0.15), dec!(1500)));
        let gold = a.by_class[&AssetClass::Gold];
        assert_eq!((gold.points(), gold.pln()), (dec!(0.1), dec!(1000)));
        assert_eq!(a.within_crypto, None);
    }

    #[test]
    fn unpriced_assets_give_no_allocation_but_unpriced_cash_does_not_matter() {
        let (h, mut v) = held(
            &[
                ("VWCE", Some(dec!(4800))),
                ("BTC", None),
                ("GOLD_COINS", Some(dec!(1200))),
            ],
            dec!(0),
        );
        assert_eq!(
            allocation(&h, &v, &targets(), sleeve),
            Err(AllocationError::Unpriced(vec!["BTC".into()]))
        );

        // Price BTC; EUR cash without a rate is still fine, since cash is not in the base.
        v.by_asset.insert("BTC".into(), dec!(4000));
        v.missing = vec!["cash EUR (Kraken)".into()];
        let mut h = h;
        h.cash.insert(("Kraken".into(), "EUR".into()), dec!(10));
        let a = allocation(&h, &v, &targets(), sleeve).unwrap();
        assert_eq!(a.total, dec!(10000));
    }

    #[test]
    fn assets_without_a_target_are_rejected() {
        let (h, v) = held(
            &[("VWCE", Some(dec!(1000))), ("DOGE", Some(dec!(10)))],
            dec!(0),
        );
        assert_eq!(
            allocation(&h, &v, &targets(), sleeve),
            Err(AllocationError::Unclassified(vec!["DOGE".into()]))
        );
    }

    #[test]
    fn only_cash_has_no_allocation() {
        let (h, v) = held(&[], dec!(500));
        assert_eq!(
            allocation(&h, &v, &targets(), sleeve),
            Err(AllocationError::NothingHeld)
        );
    }

    #[test]
    fn sleeves_belong_to_their_classes() {
        assert_eq!(Sleeve::Vwce.class(), AssetClass::Vwce);
        assert_eq!(Sleeve::Btc.class(), AssetClass::Crypto);
        assert_eq!(Sleeve::Eth.class(), AssetClass::Crypto);
        assert_eq!(Sleeve::Gold.class(), AssetClass::Gold);
        assert_eq!(Sleeve::Btc.as_str(), "btc");
    }
}
