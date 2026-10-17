//! Top-up calculator: how to split N PLN of new money between VWCE, BTC, ETH and gold to get
//! closer to the target in `config.toml` without selling anything. It follows the rule of the
//! Allocation screen (`design/reference/04-allocation.html`).
//!
//! Classes first. With the new money the base becomes T + N, and each class's need is its target
//! share of T + N minus its value now, or zero when it is already at or over that. Classes paused
//! in the config are skipped unless `include_paused` is set; they still count in the base, and
//! the plan reports what they would need. Among the classes that are bought:
//! - when their needs add up to N or more, N is split pro rata to need;
//! - otherwise every need is bought and the surplus is split by target weight among them (this
//!   only happens when a paused class is skipped, since all needs together are at least N).
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
    /// The paused classes this plan skipped, with the PLN each would need to reach its target
    /// share of the new base (zero when it is already there). Empty with `include_paused`.
    pub would_need: BTreeMap<AssetClass, Decimal>,
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
    /// Every class with a target is paused, so nothing can be bought without `include_paused`.
    AllPaused,
    /// The current allocation could not be worked out.
    Allocation(AllocationError),
}

impl fmt::Display for TopUpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TopUpError::AmountNotPositive(x) => write!(f, "amount must be above zero, got {x}"),
            TopUpError::AllPaused => {
                f.write_str("every class with a target is paused; include paused classes")
            }
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
/// sleeve is worth nothing; nothing has to be held yet). Paused classes are bought only with
/// `include_paused`.
pub fn top_up(
    values: &BTreeMap<Sleeve, Decimal>,
    config: &Config,
    amount: Decimal,
    include_paused: bool,
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
    let bought = AssetClass::ALL.map(|c| include_paused || !config.is_paused(c));
    let class_needs = needs(&classes, &targets, classes.iter().sum::<Decimal>() + amount);
    let [vwce, crypto, gold] =
        split(&class_needs, &targets, &bought, amount).ok_or(TopUpError::AllPaused)?[..]
    else {
        unreachable!("one buy per class")
    };
    let would_need = AssetClass::ALL
        .into_iter()
        .zip(class_needs)
        .zip(bought)
        .filter(|(_, bought)| !bought)
        .map(|(need, _)| need)
        .collect();

    let split_cfg = config.crypto_split();
    let coins = [value(Sleeve::Btc), value(Sleeve::Eth)];
    let coin_targets = [
        split_cfg.btc_percent / dec!(100),
        split_cfg.eth_percent / dec!(100),
    ];
    let coin_needs = needs(&coins, &coin_targets, classes[1] + crypto);
    let [btc, eth] = split(&coin_needs, &coin_targets, &[true, true], crypto)
        .expect("the coins' needs cover the crypto purchase")[..]
    else {
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
        would_need,
    })
}

/// What each position worth `values` needs to reach its target share (`targets`) of `base`:
/// the shortfall, or zero when it is at or over its target.
fn needs(values: &[Decimal], targets: &[Decimal], base: Decimal) -> Vec<Decimal> {
    values
        .iter()
        .zip(targets)
        .map(|(v, t)| (t * base - v).max(Decimal::ZERO))
        .collect()
}

/// Splits `amount` (not negative) between the positions marked `bought`: pro rata to `needs`
/// when they add up to at least `amount`, otherwise each need plus a share of the surplus by
/// target weight. Unbought positions get zero. The buys are never negative and add up to
/// `amount` exactly. None when there is a surplus but no bought position has a target.
fn split(
    needs: &[Decimal],
    targets: &[Decimal],
    bought: &[bool],
    amount: Decimal,
) -> Option<Vec<Decimal>> {
    let mut buys = vec![Decimal::ZERO; needs.len()];
    if amount.is_zero() {
        return Some(buys);
    }
    let chosen: Vec<usize> = (0..needs.len()).filter(|&i| bought[i]).collect();
    let need_sum: Decimal = chosen.iter().map(|&i| needs[i]).sum();
    let target_sum: Decimal = chosen.iter().map(|&i| targets[i]).sum();
    if need_sum >= amount {
        for &i in &chosen {
            buys[i] = amount * needs[i] / need_sum;
        }
    } else if target_sum > Decimal::ZERO {
        let surplus = amount - need_sum;
        for &i in &chosen {
            buys[i] = needs[i] + surplus * targets[i] / target_sum;
        }
    } else {
        return None;
    }
    // A share that does not terminate (a third) leaves the sum a few units of the last digit
    // off; the biggest buy takes the difference.
    let biggest = (0..buys.len()).max_by(|&a, &b| buys[a].cmp(&buys[b]))?;
    let rest: Decimal = (0..buys.len())
        .filter(|&i| i != biggest)
        .map(|i| buys[i])
        .sum();
    buys[biggest] = amount - rest;
    Some(buys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vwce: u32, crypto: u32, gold: u32, btc: u32, eth: u32, paused: &str) -> Config {
        Config::from_toml(&format!(
            r#"
            [targets]
            vwce = {vwce}
            crypto = {crypto}
            gold = {gold}
            [targets.crypto_split]
            btc = {btc}
            eth = {eth}
            [paused]
            {paused}
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

    /// The targets of the repository's `config.toml`: 55/25/20, crypto 75 BTC / 25 ETH, with
    /// nothing paused.
    fn targets() -> Config {
        config(55, 25, 20, 75, 25, "")
    }

    /// The same targets with crypto and gold paused, as in `config.toml` and the design.
    fn paused() -> Config {
        config(55, 25, 20, 75, 25, "crypto = true\ngold = true")
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

    /// The holdings of the Allocation screen's demo: VWCE 6100, BTC 2380 + ETH 920 = crypto
    /// 3300, gold 2100; 11 500 together.
    fn design() -> BTreeMap<Sleeve, Decimal> {
        values(dec!(6100), dec!(2380), dec!(920), dec!(2100))
    }

    #[test]
    fn on_target_stays_on_target() {
        // 5500 / (1875 + 625) / 2000 is exactly 55/25/20 with 75/25 in crypto. 1000 more is split
        // the same way: 550, 250 (187.50 BTC + 62.50 ETH), 200.
        let v = values(dec!(5500), dec!(1875), dec!(625), dec!(2000));
        let t = top_up(&v, &targets(), dec!(1000), false).unwrap();

        assert_eq!(buys(&t), [dec!(550), dec!(187.5), dec!(62.5), dec!(200)]);
        assert_eq!(t.class_buy(AssetClass::Crypto), dec!(250));
        assert_eq!(t.after.total, dec!(11000));
        assert!(t.would_need.is_empty());
        for d in t.after.by_class.values() {
            assert_eq!(d.pln(), dec!(0));
        }
    }

    #[test]
    fn every_need_is_bought_when_nothing_is_overweight_after() {
        // VWCE 5000, BTC 1600 + ETH 400 = crypto 2000, gold 1000: 8000, plus 2000 = 10 000.
        // Targets 5500 / 2500 / 2000, needs 500 / 500 / 1000 = 2000: buy each need.
        // Crypto after 2500: BTC target 1875 (need 275), ETH 625 (need 225).
        let v = values(dec!(5000), dec!(1600), dec!(400), dec!(1000));
        let t = top_up(&v, &targets(), dec!(2000), false).unwrap();

        assert_eq!(buys(&t), [dec!(500), dec!(275), dec!(225), dec!(1000)]);
        assert_eq!(t.after.total, dec!(10000));
        for d in t.after.by_class.values() {
            assert_eq!((d.points(), d.pln()), (dec!(0), dec!(0)));
        }
        let split = t.after.within_crypto.unwrap();
        assert_eq!((split.btc.pln(), split.eth.pln()), (dec!(0), dec!(0)));
    }

    #[test]
    fn needs_above_the_amount_are_bought_pro_rata() {
        // VWCE 4900, BTC 2250 + ETH 750 = crypto 3000, gold 1600: 9500, plus 500 = 10 000.
        // Targets 5500 / 2500 / 2000: needs 600 / 0 (crypto is 500 over) / 400 = 1000 > 500.
        // Pro rata: VWCE 500 x 600 / 1000 = 300, gold 500 x 400 / 1000 = 200. Crypto gets
        // nothing, so neither coin does.
        let v = values(dec!(4900), dec!(2250), dec!(750), dec!(1600));
        let t = top_up(&v, &targets(), dec!(500), false).unwrap();

        assert_eq!(buys(&t), [dec!(300), dec!(0), dec!(0), dec!(200)]);
        // After: VWCE 5200 is 300 short, gold 1800 200 short, crypto 3000 still 500 over.
        let after = |c: AssetClass| t.after.by_class[&c].pln();
        assert_eq!(
            AssetClass::ALL.map(after),
            [dec!(-300), dec!(500), dec!(-200)]
        );
    }

    #[test]
    fn a_non_terminating_share_still_spends_exactly_the_amount() {
        // The P16 example: VWCE 4800, BTC 3200 + ETH 800 = crypto 4000, gold 1200 = 10 000.
        // With 1000 more the base is 11 000: targets 6050 / 2750 / 2200, needs 1250 / 0 / 1000
        // = 2250. VWCE 1000 x 1250 / 2250 = 555.555..., gold 1000 x 1000 / 2250 = 444.444...
        let v = values(dec!(4800), dec!(3200), dec!(800), dec!(1200));
        let t = top_up(&v, &targets(), dec!(1000), false).unwrap();

        assert_eq!(t.buys.values().sum::<Decimal>(), dec!(1000));
        assert_eq!(
            buys(&t).map(|b| b.round_dp(2)),
            [dec!(555.56), dec!(0), dec!(0), dec!(444.44)]
        );
        // After: VWCE 5355.56 is 694.44 short, gold 1644.44 555.56 short, crypto +1250.
        let after = |c: AssetClass| t.after.by_class[&c].pln().round_dp(2);
        assert_eq!(
            AssetClass::ALL.map(after),
            [dec!(-694.44), dec!(1250), dec!(-555.56)]
        );
    }

    #[test]
    fn crypto_money_goes_to_the_coin_under_its_split() {
        // VWCE 5500, BTC 2000 + ETH 0 = crypto 2000, gold 2000: 9500, plus 500 = 10 000.
        // Classes: needs 0 / 500 / 0, so all 500 to crypto. Crypto after 2500: BTC target 1875
        // (need 0, it is 125 over), ETH 625 (need 625): ETH 500 x 625 / 625 = 500.
        let v = values(dec!(5500), dec!(2000), dec!(0), dec!(2000));
        let t = top_up(&v, &targets(), dec!(500), false).unwrap();

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
        let t = top_up(
            &BTreeMap::new(),
            &config(60, 30, 10, 50, 50, ""),
            dec!(1000),
            false,
        )
        .unwrap();

        assert_eq!(buys(&t), [dec!(600), dec!(150), dec!(150), dec!(100)]);
        assert_eq!(t.class_buy(AssetClass::Crypto), dec!(300));
        assert_eq!(t.after.total, dec!(1000));
    }

    #[test]
    fn paused_classes_are_skipped_and_report_what_they_would_need() {
        // The design's demo with 2000 zł: base 11 500 + 2000 = 13 500. Needs: VWCE 0.55 x 13 500
        // - 6100 = 1325, crypto 3375 - 3300 = 75, gold 2700 - 2100 = 600. Crypto and gold are
        // paused, so only VWCE is bought: its need 1325 is below 2000, so it takes 1325 plus the
        // whole surplus of 675 (it is the only target weight left): 2000.
        let t = top_up(&design(), &paused(), dec!(2000), false).unwrap();

        assert_eq!(buys(&t), [dec!(2000), dec!(0), dec!(0), dec!(0)]);
        assert_eq!(
            t.would_need,
            BTreeMap::from([
                (AssetClass::Crypto, dec!(75)),
                (AssetClass::Gold, dec!(600))
            ])
        );
        // VWCE after: 8100 of 13 500 = 60% (the screen's "60,0% → target 55%").
        assert_eq!(t.after.by_class[&AssetClass::Vwce].share, dec!(0.6));
    }

    #[test]
    fn include_paused_buys_every_class() {
        // Same as above, paused classes included: needs 1325 / 75 / 600 add up to exactly 2000,
        // so each is bought and the portfolio lands on target. Crypto after 3375: BTC target
        // 0.75 x 3375 = 2531.25 (need 151.25), ETH 843.75 (920 held, need 0): BTC gets all 75.
        let t = top_up(&design(), &paused(), dec!(2000), true).unwrap();

        assert_eq!(buys(&t), [dec!(1325), dec!(75), dec!(0), dec!(600)]);
        assert!(t.would_need.is_empty());
        for d in t.after.by_class.values() {
            assert_eq!(d.pln(), dec!(0));
        }
    }

    #[test]
    fn the_surplus_is_split_by_target_weight_among_the_bought_classes() {
        // Gold alone paused, 2000 zł: needs VWCE 1325 and crypto 75 add up to 1400, 600 short of
        // 2000. The surplus goes 0.55 : 0.25 of 0.80: VWCE 600 x 0.55 / 0.8 = 412.50, crypto
        // 600 x 0.25 / 0.8 = 187.50; so VWCE 1737.50 and crypto 262.50. Crypto after 3562.50:
        // BTC target 2671.875 (need 291.875), ETH 890.625 (need 0): BTC 262.50.
        let config = config(55, 25, 20, 75, 25, "gold = true");
        let t = top_up(&design(), &config, dec!(2000), false).unwrap();

        assert_eq!(buys(&t), [dec!(1737.5), dec!(262.5), dec!(0), dec!(0)]);
        assert_eq!(
            t.would_need,
            BTreeMap::from([(AssetClass::Gold, dec!(600))])
        );
    }

    #[test]
    fn a_paused_class_over_target_would_need_nothing() {
        // The design's 942 zł chip: base 12 442. VWCE need 6843.10 - 6100 = 743.10, below 942,
        // so VWCE takes all 942. Crypto 0.25 x 12 442 = 3110.50 is under the 3300 held: it would
        // need 0. Gold 2488.40 - 2100 = 388.40.
        let t = top_up(&design(), &paused(), dec!(942), false).unwrap();

        assert_eq!(buys(&t), [dec!(942), dec!(0), dec!(0), dec!(0)]);
        assert_eq!(
            t.would_need,
            BTreeMap::from([
                (AssetClass::Crypto, dec!(0)),
                (AssetClass::Gold, dec!(388.4))
            ])
        );
    }

    #[test]
    fn nothing_can_be_bought_when_every_class_with_a_target_is_paused() {
        let all = config(
            55,
            25,
            20,
            75,
            25,
            "vwce = true\ncrypto = true\ngold = true",
        );
        assert_eq!(
            top_up(&design(), &all, dec!(1000), false),
            Err(TopUpError::AllPaused)
        );
        // VWCE alone unpaused but with a 0% target: nothing it may take the money for.
        let zero = config(0, 50, 50, 75, 25, "crypto = true\ngold = true");
        assert_eq!(
            top_up(&design(), &zero, dec!(1000), false),
            Err(TopUpError::AllPaused)
        );
        // Including paused classes makes a plan again.
        assert!(top_up(&design(), &all, dec!(1000), true).is_ok());
    }

    #[test]
    fn the_amount_must_be_positive() {
        let v = values(dec!(1000), dec!(0), dec!(0), dec!(0));
        for amount in [dec!(0), dec!(-100)] {
            assert_eq!(
                top_up(&v, &targets(), amount, false),
                Err(TopUpError::AmountNotPositive(amount))
            );
        }
    }

    #[test]
    fn split_spends_exactly_the_amount_with_thirds() {
        // Three equal needs of 50 for 100: 33.333... each, and the sum is exactly 100.
        let q = dec!(0.25);
        let b = split(
            &[dec!(50), dec!(50), dec!(50), dec!(0)],
            &[q, q, q, q],
            &[true; 4],
            dec!(100),
        )
        .unwrap();

        assert_eq!(b.iter().sum::<Decimal>(), dec!(100));
        let rounded: Vec<_> = b.iter().map(|x| x.round_dp(2)).collect();
        assert_eq!(rounded, [dec!(33.33), dec!(33.33), dec!(33.33), dec!(0)]);
    }

    #[test]
    fn needs_are_shortfalls_to_target_never_negative() {
        // Base 600, halves: targets 300 each. 100 held needs 200; 400 held is over: 0.
        let h = dec!(0.5);
        assert_eq!(
            needs(&[dec!(100), dec!(400)], &[h, h], dec!(600)),
            [dec!(200), dec!(0)]
        );
        // A zero amount buys nothing, whatever the needs.
        assert_eq!(
            split(&[dec!(200), dec!(0)], &[h, h], &[true, true], dec!(0)),
            Some(vec![dec!(0), dec!(0)])
        );
    }
}
