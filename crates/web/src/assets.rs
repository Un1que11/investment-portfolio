//! The Overview's asset cards (`design/screens/01` and `03`, "Assets · earned most → least"):
//! one card per asset, in the order of `core`'s profit ranking ([`profit_attribution`]), with
//! its value, a sparkline of its value over all time, what is still invested, its P&L and its
//! share of the total profit. Assets without a price come last, unranked.
//!
//! [`profit_attribution`]: portfolio_core::attribution::profit_attribution

use std::collections::BTreeMap;

use chrono::NaiveDate;
use portfolio_core::attribution::ProfitAttribution;
use portfolio_core::gold::TENTH_OUNCE_GRAMS;
use portfolio_core::pnl::PortfolioPnl;
use portfolio_core::portfolio::{LedgerTx, Wrapper};
use portfolio_db::gold::Coin;
use portfolio_sources::prices::{BTC, ETH, VWCE};
use portfolio_sources::valuation::GOLD_COINS;
use rust_decimal::Decimal;

use crate::base::{AssetCard, CardFigures};
use crate::format;

/// The card's title: "Gold", "VWCE", "Bitcoin", "Ether"; any other asset by its symbol.
pub fn display_name(symbol: &str) -> &str {
    match symbol {
        s if s == GOLD_COINS => "Gold",
        s if s == BTC.symbol => BTC.name,
        s if s == ETH.symbol => ETH.name,
        other => other,
    }
}

/// "1 coin" / "3 coins".
fn coins_label(n: usize) -> String {
    if n == 1 {
        "1 coin".into()
    } else {
        format!("{n} coins")
    }
}

/// "1/10 oz" when every coin is a tenth of an ounce, otherwise their fine weight in grams.
fn weight_label(coins: &[Coin]) -> String {
    if coins.iter().all(|c| c.fine_weight_g == TENTH_OUNCE_GRAMS) {
        "1/10 oz".into()
    } else {
        let grams: Decimal = coins.iter().map(|c| c.fine_weight_g).sum();
        format!("{}{}g", format::number(grams, 2), format::NBSP)
    }
}

/// The wrappers VWCE was bought in, IKZE before IKE: "IKZE", "IKZE, IKE"; empty when none.
fn vwce_wrappers(ledger: &[LedgerTx]) -> String {
    let held_in = |w: Wrapper| {
        ledger
            .iter()
            .any(|t| t.asset.as_deref() == Some(VWCE.symbol) && t.wrapper == Some(w))
    };
    [(Wrapper::Ikze, "IKZE"), (Wrapper::Ike, "IKE")]
        .into_iter()
        .filter(|(w, _)| held_in(*w))
        .map(|(_, label)| label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The line under the name on desktop and on phones: where and how the asset is held.
/// VWCE comes only from the XTB import and BTC/ETH only from Kraken (CLAUDE.md, data sources).
pub fn subtitles(symbol: &str, ledger: &[LedgerTx], coins: &[Coin]) -> (String, String) {
    match symbol {
        s if s == GOLD_COINS => {
            if coins.is_empty() {
                return ("Physical".into(), "Physical".into());
            }
            let (n, w) = (coins_label(coins.len()), weight_label(coins));
            (format!("Physical · {n}, {w}"), format!("{n} · {w}"))
        }
        s if s == VWCE.symbol => {
            let wrappers = vwce_wrappers(ledger);
            if wrappers.is_empty() {
                ("Global equity ETF · XTB".into(), "XTB".into())
            } else {
                (
                    format!("Global equity ETF · {wrappers}, XTB"),
                    format!("{wrappers} · XTB"),
                )
            }
        }
        s if s == BTC.symbol || s == ETH.symbol => {
            let sub = format!("{s} · Kraken");
            (sub.clone(), sub)
        }
        other => (other.into(), other.into()),
    }
}

/// The sparkline's values: the asset's priced days, in order. Unpriced days are left out
/// rather than drawn as zero.
pub fn spark_values(series: Option<&Vec<(NaiveDate, Option<Decimal>)>>) -> Vec<Decimal> {
    series
        .map(|s| s.iter().filter_map(|(_, v)| *v).collect())
        .unwrap_or_default()
}

/// The cards, ranked assets first (highest P&L first), then the unpriced ones by symbol.
/// `series` is each asset's value over all time ([`portfolio_sources::history::ValueSeries`]).
pub fn asset_cards(
    pnl: &PortfolioPnl,
    attribution: &ProfitAttribution,
    ledger: &[LedgerTx],
    coins: &[Coin],
    series: &BTreeMap<String, Vec<(NaiveDate, Option<Decimal>)>>,
) -> Vec<AssetCard> {
    let ranked = attribution
        .ranked
        .iter()
        .enumerate()
        .map(|(i, (symbol, c))| (symbol, Some(i + 1), c.share_of_total));
    let unpriced = attribution.unpriced.iter().map(|s| (s, None, None));
    ranked
        .chain(unpriced)
        .filter_map(|(symbol, rank, share)| {
            let a = pnl.by_asset.get(symbol)?;
            let (sub, phone_sub) = subtitles(symbol, ledger, coins);
            let figures = CardFigures {
                value: a.value,
                invested: a.net_invested(),
                pnl: a.pnl(),
                pnl_ratio: a.pnl_ratio(),
                share,
            };
            Some(AssetCard::new(
                display_name(symbol),
                &sub,
                &phone_sub,
                rank,
                &figures,
                &spark_values(series.get(symbol)),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use portfolio_core::attribution::profit_attribution;
    use portfolio_core::pnl::AssetPnl;
    use portfolio_core::portfolio::{AccountKind, TxKind};
    use rust_decimal_macros::dec;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn coin(id: i64, grams: Decimal) -> Coin {
        Coin {
            id,
            transaction_id: id,
            label: None,
            purchased_on: d("2025-05-12"),
            purchase_price_pln: dec!(1640),
            fine_weight_g: grams,
            manual_quote_pln: None,
        }
    }

    fn buy(asset: &str, wrapper: Option<Wrapper>) -> LedgerTx {
        LedgerTx {
            date: d("2025-06-02"),
            account: "XTB".into(),
            account_kind: AccountKind::Funded,
            wrapper,
            kind: TxKind::Buy,
            asset: Some(asset.into()),
            quantity: Some(dec!(1)),
            amount: dec!(100),
            currency: "PLN".into(),
        }
    }

    fn plain(s: String) -> String {
        s.replace(format::NBSP, " ").replace(format::MINUS, "-")
    }

    #[test]
    fn names_and_subtitles_follow_the_design() {
        assert_eq!(display_name("GOLD_COINS"), "Gold");
        assert_eq!(display_name("VWCE"), "VWCE");
        assert_eq!(display_name("BTC"), "Bitcoin");
        assert_eq!(display_name("ETH"), "Ether");
        assert_eq!(display_name("DOT"), "DOT");

        let one = [coin(1, dec!(3.11))];
        assert_eq!(
            subtitles("GOLD_COINS", &[], &one),
            (
                "Physical · 1 coin, 1/10 oz".to_owned(),
                "1 coin · 1/10 oz".to_owned()
            )
        );
        // A 1/10 oz coin and a 1/4 oz one: 3.11 + 7.78 = 10.89 g.
        let (desk, phone) = subtitles(
            "GOLD_COINS",
            &[],
            &[coin(1, dec!(3.11)), coin(2, dec!(7.78))],
        );
        assert_eq!(plain(desk), "Physical · 2 coins, 10,89 g");
        assert_eq!(plain(phone), "2 coins · 10,89 g");
        assert_eq!(subtitles("GOLD_COINS", &[], &[]).0, "Physical");

        let ikze = [buy("VWCE", Some(Wrapper::Ikze)), buy("BTC", None)];
        assert_eq!(
            subtitles("VWCE", &ikze, &[]),
            (
                "Global equity ETF · IKZE, XTB".to_owned(),
                "IKZE · XTB".to_owned()
            )
        );
        let both = [
            buy("VWCE", Some(Wrapper::Ike)),
            buy("VWCE", Some(Wrapper::Ikze)),
        ];
        assert_eq!(subtitles("VWCE", &both, &[]).1, "IKZE, IKE · XTB");
        assert_eq!(
            subtitles("VWCE", &[buy("VWCE", None)], &[]).0,
            "Global equity ETF · XTB"
        );
        assert_eq!(subtitles("BTC", &[], &[]).0, "BTC · Kraken");
        assert_eq!(subtitles("ETH", &[], &[]).1, "ETH · Kraken");
    }

    #[test]
    fn sparklines_skip_unpriced_days() {
        let s = vec![
            (d("2026-01-31"), Some(dec!(10))),
            (d("2026-02-28"), None),
            (d("2026-03-31"), Some(dec!(12))),
        ];
        assert_eq!(spark_values(Some(&s)), [dec!(10), dec!(12)]);
        assert!(spark_values(None).is_empty());
    }

    fn asset(cost: Decimal, proceeds: Decimal, value: Option<Decimal>) -> AssetPnl {
        AssetPnl {
            cost,
            proceeds,
            income: Decimal::ZERO,
            value,
        }
    }

    /// The four assets of screens/01, no cash: value 11 500, 10 092 in, P&L 1 408.
    fn design() -> PortfolioPnl {
        let by_asset = [
            ("GOLD_COINS", asset(dec!(1640), dec!(0), Some(dec!(2100)))),
            ("VWCE", asset(dec!(5652), dec!(0), Some(dec!(6100)))),
            ("BTC", asset(dec!(2100), dec!(0), Some(dec!(2380)))),
            ("ETH", asset(dec!(700), dec!(0), Some(dec!(920)))),
        ];
        PortfolioPnl {
            value: dec!(11500),
            contributions: dec!(10092),
            complete: true,
            by_asset: by_asset
                .into_iter()
                .map(|(s, a)| (s.to_owned(), a))
                .collect(),
        }
    }

    #[test]
    fn cards_follow_the_profit_ranking_of_the_design() {
        let pnl = design();
        let attribution = profit_attribution(&pnl);
        let series = BTreeMap::from([(
            "VWCE".to_owned(),
            vec![
                (d("2026-08-31"), Some(dec!(4930))),
                (d("2026-10-17"), Some(dec!(6100))),
            ],
        )]);
        let ledger = [buy("VWCE", Some(Wrapper::Ikze))];
        let cards = asset_cards(&pnl, &attribution, &ledger, &[coin(1, dec!(3.11))], &series);

        // Gold +460, VWCE +448, Bitcoin +280, Ether +220: ranked in that order.
        let names: Vec<(&str, Option<usize>)> =
            cards.iter().map(|c| (c.name.as_str(), c.rank)).collect();
        assert_eq!(
            names,
            [
                ("Gold", Some(1)),
                ("VWCE", Some(2)),
                ("Bitcoin", Some(3)),
                ("Ether", Some(4))
            ]
        );
        // Shares of the 1 408 total: 460 / 1408 = 32,67% -> 32,7%; 448 -> 31,8%; 280 -> 19,9%;
        // 220 -> 15,6% (the design's figures; here they add up to 100% since there is no cash).
        let shares: Vec<&str> = cards.iter().map(|c| c.share.as_str()).collect();
        assert_eq!(shares, ["32,7%", "31,8%", "19,9%", "15,6%"]);
        let vwce = &cards[1];
        assert_eq!(plain(vwce.value.clone()), "6 100,00 zł");
        assert_eq!(plain(vwce.invested.clone()), "5 652,00 zł");
        assert_eq!(plain(vwce.pnl.clone().unwrap().text), "+448,00 zł");
        // 448 / 5652 = 7,93% -> +7,9%.
        assert_eq!(vwce.pnl_percent, "+7,9%");
        assert_eq!(vwce.sub, "Global equity ETF · IKZE, XTB");
        assert_eq!(vwce.share_width, "31.8%");
        // Lowest 4930 at the bottom (y 30), 6100 at the top (y 2).
        assert_eq!(vwce.spark.as_deref(), Some("M0.0 30.0 L96.0 2.0"));
        // No series for the others: no sparkline.
        assert_eq!(cards[0].spark, None);
    }

    #[test]
    fn unpriced_assets_come_last_and_sold_ones_keep_their_rank() {
        // 3 000 in. VWCE 1000 -> 1100 (+100). BTC bought for 1000 and sold for 1300: nothing
        // held, +300 realised, invested 1000 - 1300 = -300. ETH 1000 in, no price. Incomplete
        // valuation, so no shares.
        let pnl = PortfolioPnl {
            value: dec!(1100),
            contributions: dec!(3000),
            complete: false,
            by_asset: [
                ("VWCE", asset(dec!(1000), dec!(0), Some(dec!(1100)))),
                ("BTC", asset(dec!(1000), dec!(1300), Some(dec!(0)))),
                ("ETH", asset(dec!(1000), dec!(0), None)),
            ]
            .into_iter()
            .map(|(s, a)| (s.to_owned(), a))
            .collect(),
        };
        let cards = asset_cards(&pnl, &profit_attribution(&pnl), &[], &[], &BTreeMap::new());

        let names: Vec<(&str, Option<usize>)> =
            cards.iter().map(|c| (c.name.as_str(), c.rank)).collect();
        assert_eq!(
            names,
            [("Bitcoin", Some(1)), ("VWCE", Some(2)), ("Ether", None)]
        );
        let btc = &cards[0];
        assert_eq!(plain(btc.invested.clone()), "-300,00 zł");
        assert_eq!(plain(btc.pnl.clone().unwrap().text), "+300,00 zł");
        // P&L on cost: 300 / 1000 = +30,0%.
        assert_eq!(btc.pnl_percent, "+30,0%");
        assert_eq!(btc.share, "—");
        let eth = &cards[2];
        assert_eq!((eth.value.as_str(), eth.pnl.is_none()), ("—", true));
        assert_eq!(plain(eth.invested.clone()), "1 000,00 zł");
    }
}
