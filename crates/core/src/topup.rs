//! Top-up calculator: how to split N PLN of new money between VWCE, BTC, ETH and gold to get as
//! close to the target in `config.toml` as possible without selling anything.
//!
//! Classes first. With the new money the base becomes T + N, and each class's target value is
//! its target share of T + N; the gap is that target value minus the class's value now. The gaps
//! add up to N, so when no class is over its new target, buying every gap lands exactly on
//! target. Otherwise the overweight classes get nothing and the biggest gaps are filled first,
//! down to a common level: every class that gets money ends the same PLN short of its target, and
//! every class that gets nothing is no further short. Among plans that only buy, this is the one
//! with the smallest sum of squared PLN deviations, and the smallest largest underweight.
//!
//! The crypto purchase is then split between BTC and ETH the same way, against the crypto split
//! and the crypto value after the purchase. When crypto as a whole gets nothing, neither coin
//! does, however far the split is off.
//!
//! As in `allocation`, cash in the accounts is not part of the base. Results are not rounded.

use std::collections::BTreeMap;
use std::fmt;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::AssetClass;
use crate::allocation::{Allocation, AllocationError, Sleeve, allocation_of};
use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopUp {
    /// PLN to spend.
    pub amount: Decimal,
    /// PLN to buy of every sleeve (zero for the ones that get nothing); adds up to `amount`.
    pub buys: BTreeMap<Sleeve, Decimal>,
    /// The allocation once everything is bought.
    pub after: Allocation,
}

impl TopUp {
    /// PLN to buy of a class (BTC and ETH together for crypto).
    pub fn class_buy(&self, class: AssetClass) -> Decimal {
        self.buys
            .iter()
            .filter(|(s, _)| s.class() == class)
            .map(|(_, v)| *v)
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopUpError {
    /// The amount to spend must be above zero.
    AmountNotPositive(Decimal),
    /// The current allocation could not be worked out.
    Allocation(AllocationError),
}

impl fmt::Display for TopUpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TopUpError::AmountNotPositive(x) => write!(f, "amount must be above zero, got {x}"),
            TopUpError::Allocation(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for TopUpError {}

impl From<AllocationError> for TopUpError {
    fn from(e: AllocationError) -> TopUpError {
        TopUpError::Allocation(e)
    }
}

/// Plans the purchases for `amount` PLN, given the PLN value of each sleeve held now (a missing
/// sleeve is worth nothing; nothing has to be held yet).
pub fn top_up(
    values: &BTreeMap<Sleeve, Decimal>,
    config: &Config,
    amount: Decimal,
) -> Result<TopUp, TopUpError> {
    if amount <= Decimal::ZERO {
        return Err(TopUpError::AmountNotPositive(amount));
    }
    let value = |s: Sleeve| values.get(&s).copied().unwrap_or_default();

    let classes = [
        value(Sleeve::Vwce),
        value(Sleeve::Btc) + value(Sleeve::Eth),
        value(Sleeve::Gold),
    ];
    let targets = AssetClass::ALL.map(|c| config.target_fraction(c));
    let [vwce, crypto, gold] = fill(&classes, &targets, amount)[..] else {
        unreachable!("one buy per class")
    };

    let split = config.crypto_split();
    let coins = [value(Sleeve::Btc), value(Sleeve::Eth)];
    let split = [split.btc_percent / dec!(100), split.eth_percent / dec!(100)];
    let [btc, eth] = fill(&coins, &split, crypto)[..] else {
        unreachable!("one buy per coin")
    };

    let buys = BTreeMap::from([
        (Sleeve::Vwce, vwce),
        (Sleeve::Btc, btc),
        (Sleeve::Eth, eth),
        (Sleeve::Gold, gold),
    ]);
    let after = buys.iter().map(|(s, buy)| (*s, value(*s) + buy)).collect();
    Ok(TopUp {
        amount,
        after: allocation_of(&after, config)?,
        buys,
    })
}

/// Splits `amount` (not negative) between positions worth `values` with target shares
/// `targets` (adding up to 1), filling the biggest gaps to target first down to a common level.
/// Returns the buys in the order given; they are never negative and add up to `amount` exactly.
fn fill(values: &[Decimal], targets: &[Decimal], amount: Decimal) -> Vec<Decimal> {
    let total = values.iter().sum::<Decimal>() + amount;
    let gaps: Vec<Decimal> = values
        .iter()
        .zip(targets)
        .map(|(v, t)| t * total - v)
        .collect();
    let mut order: Vec<usize> = (0..gaps.len()).collect();
    order.sort_by(|&a, &b| gaps[b].cmp(&gaps[a]));

    // Take the k biggest gaps and lower them all to the level that spends exactly `amount`;
    // stop at the first k whose level the next gap does not reach.
    let mut filled = Decimal::ZERO;
    let mut level = Decimal::ZERO;
    let mut k = 0;
    for (i, &idx) in order.iter().enumerate() {
        filled += gaps[idx];
        k = i + 1;
        level = (filled - amount) / Decimal::from(k);
        if order.get(k).is_none_or(|&next| gaps[next] <= level) {
            break;
        }
    }

    let mut buys = vec![Decimal::ZERO; gaps.len()];
    for &idx in &order[..k] {
        buys[idx] = gaps[idx] - level;
    }
    // A level that does not terminate (a third) leaves the sum a few units of the last digit
    // off; the biggest buy takes the difference.
    if let Some(&first) = order.first() {
        let rest: Decimal = order[1..].iter().map(|&i| buys[i]).sum();
        buys[first] = amount - rest;
    }
    buys
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

    fn values(
        vwce: Decimal,
        btc: Decimal,
        eth: Decimal,
        gold: Decimal,
    ) -> BTreeMap<Sleeve, Decimal> {
        BTreeMap::from([
            (Sleeve::Vwce, vwce),
            (Sleeve::Btc, btc),
            (Sleeve::Eth, eth),
            (Sleeve::Gold, gold),
        ])
    }

    fn buys(t: &TopUp) -> [Decimal; 4] {
        [Sleeve::Vwce, Sleeve::Btc, Sleeve::Eth, Sleeve::Gold].map(|s| t.buys[&s])
    }

    #[test]
    fn on_target_stays_on_target() {
        // 5500 / (1875 + 625) / 2000 is exactly 55/25/20 with 75/25 in crypto. 1000 more is split
        // the same way: 550, 250 (187.50 BTC + 62.50 ETH), 200.
        let v = values(dec!(5500), dec!(1875), dec!(625), dec!(2000));
        let t = top_up(&v, &targets(), dec!(1000)).unwrap();

        assert_eq!(buys(&t), [dec!(550), dec!(187.5), dec!(62.5), dec!(200)]);
        assert_eq!(t.class_buy(AssetClass::Crypto), dec!(250));
        assert_eq!(t.after.total, dec!(11000));
        for d in t.after.by_class.values() {
            assert_eq!(d.pln(), dec!(0));
        }
    }

    #[test]
    fn every_gap_is_bought_when_nothing_is_overweight_after() {
        // VWCE 5000, BTC 1600 + ETH 400 = crypto 2000, gold 1000: 8000, plus 2000 = 10 000.
        // Targets 5500 / 2500 / 2000, gaps 500 / 500 / 1000 = 2000: buy each gap.
        // Crypto after 2500: BTC target 1875 (gap 275), ETH 625 (gap 225).
        let v = values(dec!(5000), dec!(1600), dec!(400), dec!(1000));
        let t = top_up(&v, &targets(), dec!(2000)).unwrap();

        assert_eq!(buys(&t), [dec!(500), dec!(275), dec!(225), dec!(1000)]);
        assert_eq!(t.after.total, dec!(10000));
        for d in t.after.by_class.values() {
            assert_eq!((d.points(), d.pln()), (dec!(0), dec!(0)));
        }
        let split = t.after.within_crypto.unwrap();
        assert_eq!((split.btc.pln(), split.eth.pln()), (dec!(0), dec!(0)));
    }

    #[test]
    fn an_overweight_class_gets_nothing_and_the_rest_end_equally_short() {
        // The P16 example: VWCE 4800, BTC 3200 + ETH 800 = crypto 4000, gold 1200 = 10 000.
        // With 1000 more the base is 11 000: targets 6050 / 2750 / 2200, gaps +1250 / -1250 /
        // +1000. VWCE alone: level 1250 - 1000 = 250, but gold's gap (1000) is above it. VWCE and
        // gold: level (1250 + 1000 - 1000) / 2 = 625, above crypto's gap. Buy VWCE 1250 - 625 =
        // 625 and gold 1000 - 625 = 375; crypto (overweight) nothing, so no BTC and no ETH even
        // though ETH is under its split.
        let v = values(dec!(4800), dec!(3200), dec!(800), dec!(1200));
        let t = top_up(&v, &targets(), dec!(1000)).unwrap();

        assert_eq!(buys(&t), [dec!(625), dec!(0), dec!(0), dec!(375)]);
        // After: VWCE 5425 and gold 1575, both 625 PLN (5.68 pp) under; crypto 4000, +1250.
        let after = |c: AssetClass| t.after.by_class[&c];
        assert_eq!(after(AssetClass::Vwce).value, dec!(5425));
        assert_eq!(after(AssetClass::Vwce).pln(), dec!(-625));
        assert_eq!(after(AssetClass::Gold).pln(), dec!(-625));
        assert_eq!(after(AssetClass::Crypto).pln(), dec!(1250));
        assert_eq!(after(AssetClass::Gold).points().round_dp(4), dec!(-0.0568));
    }

    #[test]
    fn a_small_amount_goes_to_the_biggest_gap_only() {
        // Same holdings, 50 PLN: base 10 050, targets 5527.50 / 2512.50 / 2010, gaps +727.50 /
        // -1487.50 / +810. Gold alone: level 810 - 50 = 760, above VWCE's 727.50: all to gold.
        let v = values(dec!(4800), dec!(3200), dec!(800), dec!(1200));
        let t = top_up(&v, &targets(), dec!(50)).unwrap();

        assert_eq!(buys(&t), [dec!(0), dec!(0), dec!(0), dec!(50)]);
        // Gold 1250 is still 760 short, VWCE 727.50.
        assert_eq!(t.after.by_class[&AssetClass::Gold].pln(), dec!(-760));
        assert_eq!(t.after.by_class[&AssetClass::Vwce].pln(), dec!(-727.5));
    }

    #[test]
    fn crypto_money_goes_to_the_coin_under_its_split() {
        // VWCE 5500, BTC 2000 + ETH 0 = crypto 2000, gold 2000: 9500, plus 500 = 10 000.
        // Classes: gaps 0 / 500 / 0, so all 500 to crypto. Crypto after 2500: BTC target 1875
        // (gap -125), ETH 625 (gap +625): ETH alone, level 625 - 500 = 125 >= -125. ETH 500.
        let v = values(dec!(5500), dec!(2000), dec!(0), dec!(2000));
        let t = top_up(&v, &targets(), dec!(500)).unwrap();

        assert_eq!(buys(&t), [dec!(0), dec!(0), dec!(500), dec!(0)]);
        // The crypto class lands on target; within it ETH is 500 of 2500 (20% vs 25%), 125 short.
        assert_eq!(t.after.by_class[&AssetClass::Crypto].pln(), dec!(0));
        let split = t.after.within_crypto.unwrap();
        assert_eq!(split.eth.share, dec!(0.2));
        assert_eq!((split.btc.pln(), split.eth.pln()), (dec!(125), dec!(-125)));
    }

    #[test]
    fn targets_come_from_the_config() {
        // 60/30/10, crypto 50/50, nothing held: 1000 goes 600 / 150 + 150 / 100.
        let t = top_up(&BTreeMap::new(), &config(60, 30, 10, 50, 50), dec!(1000)).unwrap();

        assert_eq!(buys(&t), [dec!(600), dec!(150), dec!(150), dec!(100)]);
        assert_eq!(t.class_buy(AssetClass::Crypto), dec!(300));
        assert_eq!(t.after.total, dec!(1000));
    }

    #[test]
    fn the_amount_must_be_positive() {
        let v = values(dec!(1000), dec!(0), dec!(0), dec!(0));
        for amount in [dec!(0), dec!(-100)] {
            assert_eq!(
                top_up(&v, &targets(), amount),
                Err(TopUpError::AmountNotPositive(amount))
            );
        }
    }

    #[test]
    fn a_non_terminating_level_still_spends_exactly_the_amount() {
        // Four positions at 25% each, 1000 in the last: with 100 more the base is 1100, targets
        // 275 each, gaps 275 / 275 / 275 / -725. Two: level (550 - 100) / 2 = 225 < 275. Three:
        // level (825 - 100) / 3 = 241.666..., so each of the first three gets 33.333...
        let q = dec!(0.25);
        let b = fill(
            &[dec!(0), dec!(0), dec!(0), dec!(1000)],
            &[q, q, q, q],
            dec!(100),
        );

        assert_eq!(b.iter().sum::<Decimal>(), dec!(100));
        let rounded: Vec<_> = b.iter().map(|x| x.round_dp(2)).collect();
        assert_eq!(rounded, [dec!(33.33), dec!(33.33), dec!(33.33), dec!(0)]);
    }

    #[test]
    fn a_zero_gap_gets_nothing_and_equal_gaps_share_equally() {
        // Two halves worth 100 and 300, 200 more: base 600, targets 300 each, gaps 200 / 0.
        // The first alone: level 200 - 200 = 0 >= 0, so it takes all 200 and both land on 300.
        let h = dec!(0.5);
        assert_eq!(
            fill(&[dec!(100), dec!(300)], &[h, h], dec!(200)),
            [dec!(200), dec!(0)]
        );
        // Both at 100, 100 more: gaps 50 / 50, level 0: 50 each.
        assert_eq!(
            fill(&[dec!(100), dec!(100)], &[h, h], dec!(100)),
            [dec!(50), dec!(50)]
        );
    }
}
