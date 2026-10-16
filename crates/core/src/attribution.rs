//! Each asset's contribution to the portfolio's profit, and the ranking "earned the most / the
//! least".
//!
//! An asset's contribution is its P&L in PLN (see [`crate::pnl`]), measured two ways:
//! - Share of the total: asset P&L / portfolio P&L. Together with the share of `other` (cash,
//!   currency moves on cash, account fees) the shares add up to 100%. When the portfolio is at a
//!   loss, a positive share means the asset made part of the loss and a negative one that it
//!   offset some of it.
//! - Percentage points of the portfolio return: asset P&L / net contributions. Together with
//!   `other` they add up to the portfolio's P&L % ([`PortfolioPnl::pnl_ratio`]).
//!
//! The ranking orders assets by P&L in PLN, highest first. Fully sold assets keep their
//! realised P&L and are ranked too; assets without a price have no P&L and are listed apart.
//! Results are not rounded.

use std::cmp::Reverse;

use rust_decimal::Decimal;

use crate::pnl::PortfolioPnl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contribution {
    /// PLN.
    pub pnl: Decimal,
    /// P&L / portfolio P&L (0.25 = a quarter of the total). None when the portfolio P&L is zero
    /// or the valuation is incomplete.
    pub share_of_total: Option<Decimal>,
    /// P&L / net contributions (0.01 = 1 percentage point of the portfolio return). None when
    /// nothing (net) was put in.
    pub return_points: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfitAttribution {
    /// Asset symbol and contribution, highest P&L first; equal P&Ls by symbol.
    pub ranked: Vec<(String, Contribution)>,
    /// Assets with no price on the date: no P&L, so not ranked.
    pub unpriced: Vec<String>,
    /// The part of the portfolio P&L no asset explains; None when the valuation is incomplete.
    pub other: Option<Contribution>,
}

impl ProfitAttribution {
    /// The asset that earned the most (or lost the least).
    pub fn most(&self) -> Option<&(String, Contribution)> {
        self.ranked.first()
    }

    /// The asset that earned the least (or lost the most).
    pub fn least(&self) -> Option<&(String, Contribution)> {
        self.ranked.last()
    }
}

/// Splits the P&L of `pnl` into the assets' contributions and ranks them.
pub fn profit_attribution(pnl: &PortfolioPnl) -> ProfitAttribution {
    let total = pnl.pnl_if_complete().filter(|t| !t.is_zero());
    let invested = (pnl.contributions > Decimal::ZERO).then_some(pnl.contributions);
    let contribution = |p: Decimal| Contribution {
        pnl: p,
        share_of_total: total.map(|t| p / t),
        return_points: invested.map(|c| p / c),
    };

    let mut ranked = Vec::new();
    let mut unpriced = Vec::new();
    for (asset, a) in &pnl.by_asset {
        match a.pnl() {
            Some(p) => ranked.push((asset.clone(), contribution(p))),
            None => unpriced.push(asset.clone()),
        }
    }
    // Stable: `by_asset` is ordered by symbol, so equal P&Ls stay in that order.
    ranked.sort_by_key(|(_, c)| Reverse(c.pnl));

    ProfitAttribution {
        ranked,
        unpriced,
        other: pnl
            .complete
            .then(|| pnl.other())
            .flatten()
            .map(contribution),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pnl::AssetPnl;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    fn asset(cost: Decimal, proceeds: Decimal, value: Option<Decimal>) -> AssetPnl {
        AssetPnl {
            cost,
            proceeds,
            income: Decimal::ZERO,
            value,
        }
    }

    fn portfolio(
        value: Decimal,
        contributions: Decimal,
        assets: &[(&str, AssetPnl)],
    ) -> PortfolioPnl {
        PortfolioPnl {
            value,
            contributions,
            complete: assets.iter().all(|(_, a)| a.value.is_some()),
            by_asset: assets
                .iter()
                .map(|(s, a)| ((*s).to_owned(), a.clone()))
                .collect(),
        }
    }

    /// 10 000 PLN put in. VWCE 6000 -> 6600 (+600), BTC 2000 -> 2500 (+500), gold
    /// 2000 -> 1850 (-150), and 50 PLN of cash: value 11 000, P&L +1000 (10%).
    fn gain() -> PortfolioPnl {
        portfolio(
            dec!(11000),
            dec!(10000),
            &[
                ("VWCE", asset(dec!(6000), dec!(0), Some(dec!(6600)))),
                ("BTC", asset(dec!(2000), dec!(0), Some(dec!(2500)))),
                ("GOLD_COINS", asset(dec!(2000), dec!(0), Some(dec!(1850)))),
            ],
        )
    }

    fn symbols(a: &ProfitAttribution) -> Vec<&str> {
        a.ranked.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[test]
    fn ranks_assets_by_pnl_and_splits_the_total() {
        let p = gain();
        let a = profit_attribution(&p);

        assert_eq!(symbols(&a), ["VWCE", "BTC", "GOLD_COINS"]);
        let c: Vec<_> = a.ranked.iter().map(|(_, c)| *c).collect();
        // Shares of the 1000 total: 600 -> 60%, 500 -> 50%, -150 -> -15%.
        // Points of the 10% return (over 10 000 put in): 6, 5 and -1.5.
        assert_eq!(
            c[0],
            Contribution {
                pnl: dec!(600),
                share_of_total: Some(dec!(0.6)),
                return_points: Some(dec!(0.06)),
            }
        );
        assert_eq!(
            (c[1].pnl, c[1].share_of_total, c[1].return_points),
            (dec!(500), Some(dec!(0.5)), Some(dec!(0.05)))
        );
        assert_eq!(
            (c[2].pnl, c[2].share_of_total, c[2].return_points),
            (dec!(-150), Some(dec!(-0.15)), Some(dec!(-0.015)))
        );
        // The 50 PLN cash: 1000 - (600 + 500 - 150) = 50 -> 5% of the total, 0.5 points.
        assert_eq!(
            a.other,
            Some(Contribution {
                pnl: dec!(50),
                share_of_total: Some(dec!(0.05)),
                return_points: Some(dec!(0.005)),
            })
        );
        assert_eq!(a.most().unwrap().0, "VWCE");
        assert_eq!(a.least().unwrap().0, "GOLD_COINS");
        assert!(a.unpriced.is_empty());
    }

    #[test]
    fn the_parts_add_up_to_the_whole() {
        let p = gain();
        let a = profit_attribution(&p);
        let other = a.other.unwrap();

        let shares: Decimal = a
            .ranked
            .iter()
            .map(|(_, c)| c.share_of_total.unwrap())
            .sum();
        let points: Decimal = a.ranked.iter().map(|(_, c)| c.return_points.unwrap()).sum();
        // 0.6 + 0.5 - 0.15 + 0.05 = 1; 0.06 + 0.05 - 0.015 + 0.005 = 0.1 = 1000 / 10 000.
        assert_eq!(shares + other.share_of_total.unwrap(), dec!(1));
        assert_eq!(
            points + other.return_points.unwrap(),
            p.pnl_ratio().unwrap()
        );
    }

    #[test]
    fn at_a_loss_shares_are_of_the_loss() {
        // 10 000 put in. VWCE -300, BTC -200, gold +100, and 50 PLN of account fees:
        // value 9550, P&L -450 (-4.5%).
        let p = portfolio(
            dec!(9550),
            dec!(10000),
            &[
                ("VWCE", asset(dec!(6000), dec!(0), Some(dec!(5700)))),
                ("BTC", asset(dec!(2000), dec!(0), Some(dec!(1800)))),
                ("GOLD_COINS", asset(dec!(2000), dec!(0), Some(dec!(2100)))),
            ],
        );
        let a = profit_attribution(&p);

        // Gold earned the most, VWCE lost the most.
        assert_eq!(symbols(&a), ["GOLD_COINS", "BTC", "VWCE"]);
        // Of the -450: VWCE -300 is 2/3 of the loss, BTC -200 is 4/9, gold +100 offsets 2/9,
        // the fees -50 are 1/9; 6/9 + 4/9 - 2/9 + 1/9 = 1.
        let share = |i: usize| a.ranked[i].1.share_of_total.unwrap().round_dp(4);
        assert_eq!(
            (share(0), share(1), share(2)),
            (dec!(-0.2222), dec!(0.4444), dec!(0.6667))
        );
        assert_eq!(
            a.other.unwrap().share_of_total.unwrap().round_dp(4),
            dec!(0.1111)
        );
        // Points of the -4.5% return: gold +1, BTC -2, VWCE -3, fees -0.5.
        assert_eq!(a.ranked[0].1.return_points, Some(dec!(0.01)));
        assert_eq!(a.ranked[2].1.return_points, Some(dec!(-0.03)));
        assert_eq!(a.other.unwrap().return_points, Some(dec!(-0.005)));
    }

    #[test]
    fn sold_and_staking_assets_are_ranked_and_ties_go_by_symbol() {
        // VWCE: 1000 deposited and bought, worth 1100 (+100). Gold: a coin bought for 1000 and
        // sold for 1100 (+100 realised, nothing left). ETH from staking only, worth 100 (+100,
        // cost 0). Net contributions 1000 + 1000 - 1100 = 900; value 1100 + 100 = 1200;
        // P&L 300, all from the assets.
        let p = portfolio(
            dec!(1200),
            dec!(900),
            &[
                ("VWCE", asset(dec!(1000), dec!(0), Some(dec!(1100)))),
                ("GOLD_COINS", asset(dec!(1000), dec!(1100), Some(dec!(0)))),
                ("ETH", asset(dec!(0), dec!(0), Some(dec!(100)))),
            ],
        );
        let a = profit_attribution(&p);

        assert_eq!(symbols(&a), ["ETH", "GOLD_COINS", "VWCE"]);
        for (_, c) in &a.ranked {
            // 100 each: a third of the 300, and 100 / 900 = 11.11 points of the 33.33% return.
            assert_eq!(c.pnl, dec!(100));
            assert_eq!(c.share_of_total.unwrap().round_dp(4), dec!(0.3333));
            assert_eq!(c.return_points.unwrap().round_dp(4), dec!(0.1111));
        }
        assert_eq!(a.other.unwrap().pnl, dec!(0));
    }

    #[test]
    fn unpriced_assets_are_listed_apart() {
        let mut p = gain();
        p.by_asset.get_mut("BTC").unwrap().value = None;
        p.complete = false;
        p.value = dec!(8500); // without BTC's 2500

        let a = profit_attribution(&p);

        assert_eq!(symbols(&a), ["VWCE", "GOLD_COINS"]);
        assert_eq!(a.unpriced, ["BTC"]);
        // The total is unknown, so there are no shares and no `other`; points of the return
        // only need the contributions: 600 / 10 000 = 6 points.
        assert_eq!(a.ranked[0].1.share_of_total, None);
        assert_eq!(a.ranked[0].1.return_points, Some(dec!(0.06)));
        assert_eq!(a.other, None);
    }

    #[test]
    fn zero_total_or_nothing_put_in_gives_no_ratios() {
        // VWCE +100 and gold -100: a P&L of exactly 0 has no shares to split.
        let p = portfolio(
            dec!(2000),
            dec!(2000),
            &[
                ("VWCE", asset(dec!(1000), dec!(0), Some(dec!(1100)))),
                ("GOLD_COINS", asset(dec!(1000), dec!(0), Some(dec!(900)))),
            ],
        );
        let a = profit_attribution(&p);
        assert_eq!(a.ranked[0].1.share_of_total, None);
        assert_eq!(a.ranked[0].1.return_points, Some(dec!(0.05)));

        // Staking rewards only: 0 put in, worth 100. P&L 100, but no return to take points of.
        let p = portfolio(
            dec!(100),
            dec!(0),
            &[("ETH", asset(dec!(0), dec!(0), Some(dec!(100))))],
        );
        let a = profit_attribution(&p);
        assert_eq!(
            a.ranked[0].1,
            Contribution {
                pnl: dec!(100),
                share_of_total: Some(dec!(1)),
                return_points: None,
            }
        );
    }

    #[test]
    fn empty_portfolio_has_no_ranking() {
        let p = PortfolioPnl {
            value: dec!(0),
            contributions: dec!(0),
            complete: true,
            by_asset: BTreeMap::new(),
        };
        let a = profit_attribution(&p);

        assert_eq!((a.most(), a.least()), (None, None));
        assert_eq!(a.other.unwrap().pnl, dec!(0));
    }
}
