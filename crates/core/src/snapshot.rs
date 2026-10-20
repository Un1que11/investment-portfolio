//! The daily per-asset snapshot that `portfolio-sync` stores (table `snapshots`).
//!
//! One row per asset held on the date, and per asset held before and sold out since (quantity
//! and value 0): its quantity, its value in PLN and what is still invested in it in PLN (cost - sale proceeds - cash dividends, see
//! [`crate::pnl::AssetPnl::net_invested`]). PLN amounts are rounded to the grosz, half away from
//! zero, so a stored day reads like a statement; quantities are kept exact.
//!
//! A day is all or nothing: if any held asset has no price, no rows are produced, because the
//! portfolio total of a day is the sum of its rows and a missing row would understate it
//! silently. A sold-out asset keeps its row so that what its sales returned stays in the day
//! (its invested amount, often negative), and the change between two days ([`crate::summary`])
//! includes it. Cash left in accounts has no asset and is not part of the snapshot.

use std::collections::BTreeMap;
use std::fmt;

use rust_decimal::{Decimal, RoundingStrategy};

use crate::pnl::PortfolioPnl;
use crate::portfolio::{Holdings, Valuation};

/// One asset on one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRow {
    /// Asset symbol (`assets.symbol`).
    pub asset: String,
    /// Units held, exact.
    pub quantity: Decimal,
    /// Value of the position, PLN, rounded to 0.01.
    pub value_pln: Decimal,
    /// Cost minus sale proceeds minus cash dividends, PLN, rounded to 0.01. Negative once the
    /// asset has returned more than was put in.
    pub invested_pln: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// These held assets have no price on the date.
    Unpriced(Vec<String>),
    /// A held quantity or a value is negative, which the ledger should never produce.
    Negative(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Unpriced(assets) => {
                write!(f, "no price for {}", assets.join(", "))
            }
            SnapshotError::Negative(asset) => {
                write!(f, "negative quantity or value for {asset}")
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

/// PLN rounded to the grosz, half away from zero (12.345 -> 12.35, -12.345 -> -12.35).
pub fn grosze(v: Decimal) -> Decimal {
    let mut r = v.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    // -0.004 rounds to "-0.00"; store a plain zero instead.
    if r.is_zero() {
        return Decimal::new(0, 2);
    }
    // Rounding never adds digits; always write two ("7" -> "7.00"). Exact after the rounding.
    r.rescale(2);
    r
}

/// Rows for every asset in `holdings`, plus a quantity-0, value-0 row for every asset in `pnl`
/// that is no longer held (sold out), in symbol order.
pub fn snapshot_rows(
    holdings: &Holdings,
    valuation: &Valuation,
    pnl: &PortfolioPnl,
) -> Result<Vec<SnapshotRow>, SnapshotError> {
    let held: BTreeMap<&String, &Decimal> = holdings
        .assets
        .iter()
        .filter(|(_, q)| !q.is_zero())
        .collect();
    let unpriced: Vec<String> = held
        .keys()
        .filter(|a| !valuation.by_asset.contains_key(a.as_str()))
        .map(|a| a.to_string())
        .collect();
    if !unpriced.is_empty() {
        return Err(SnapshotError::Unpriced(unpriced));
    }
    let sold_out = pnl
        .by_asset
        .keys()
        .filter(|a| !held.contains_key(a))
        .map(|a| (a, &Decimal::ZERO));
    let mut rows = held
        .iter()
        .map(|(&asset, &quantity)| (asset, quantity))
        .chain(sold_out)
        .map(|(asset, &quantity)| {
            let value = if quantity.is_zero() {
                Decimal::ZERO
            } else {
                valuation.by_asset[asset.as_str()]
            };
            if quantity < Decimal::ZERO || value < Decimal::ZERO {
                return Err(SnapshotError::Negative(asset.clone()));
            }
            let invested = pnl
                .by_asset
                .get(asset.as_str())
                .map_or(Decimal::ZERO, |a| a.net_invested());
            Ok(SnapshotRow {
                asset: asset.clone(),
                quantity,
                value_pln: grosze(value),
                invested_pln: grosze(invested),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort_by(|a, b| a.asset.cmp(&b.asset));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pnl::AssetPnl;
    use rust_decimal_macros::dec;

    fn holdings(assets: &[(&str, Decimal)]) -> Holdings {
        Holdings {
            assets: assets.iter().map(|(a, q)| (a.to_string(), *q)).collect(),
            cash: BTreeMap::new(),
        }
    }

    fn valuation(values: &[(&str, Decimal)]) -> Valuation {
        Valuation {
            by_asset: values.iter().map(|(a, v)| (a.to_string(), *v)).collect(),
            cash: BTreeMap::new(),
            total: values.iter().map(|(_, v)| *v).sum(),
            missing: Vec::new(),
        }
    }

    fn pnl(assets: &[(&str, Decimal, Decimal, Decimal)]) -> PortfolioPnl {
        PortfolioPnl {
            value: Decimal::ZERO,
            contributions: Decimal::ZERO,
            complete: true,
            by_asset: assets
                .iter()
                .map(|(a, cost, proceeds, income)| {
                    (
                        a.to_string(),
                        AssetPnl {
                            cost: *cost,
                            proceeds: *proceeds,
                            income: *income,
                            value: None,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn grosze_round_half_away_from_zero() {
        assert_eq!(grosze(dec!(12.345)).to_string(), "12.35");
        assert_eq!(grosze(dec!(12.344999)).to_string(), "12.34");
        assert_eq!(grosze(dec!(-12.345)).to_string(), "-12.35");
        assert_eq!(grosze(dec!(7)).to_string(), "7.00");
        assert_eq!(grosze(dec!(-0.004)).to_string(), "0.00");
    }

    #[test]
    fn one_row_per_held_asset_with_value_and_net_invested() {
        // VWCE: 3.5 units worth 3.5 x 552.1234 = 1932.4319 PLN -> 1932.43.
        //   Bought for 1800.005 + fee 4.00 = 1804.005, nothing sold -> invested 1804.01.
        // BTC: 0.01 worth 3 801.255 -> 3801.26 (half rounds up).
        //   Bought for 5000, sold part for 2000.50, no dividends -> invested 2999.50.
        // ETH from staking only: cost 0 -> invested 0.00.
        let h = holdings(&[
            ("VWCE", dec!(3.5)),
            ("BTC", dec!(0.01)),
            ("ETH", dec!(0.002)),
        ]);
        let v = valuation(&[
            ("VWCE", dec!(3.5) * dec!(552.1234)),
            ("BTC", dec!(3801.255)),
            ("ETH", dec!(25.10)),
        ]);
        let p = pnl(&[
            ("VWCE", dec!(1804.005), dec!(0), dec!(0)),
            ("BTC", dec!(5000), dec!(2000.50), dec!(0)),
        ]);
        let rows = snapshot_rows(&h, &v, &p).unwrap();
        let shown: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.asset.as_str(),
                    r.quantity.to_string(),
                    r.value_pln.to_string(),
                    r.invested_pln.to_string(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                ("BTC", "0.01".into(), "3801.26".into(), "2999.50".into()),
                ("ETH", "0.002".into(), "25.10".into(), "0.00".into()),
                ("VWCE", "3.5".into(), "1932.43".into(), "1804.01".into()),
            ]
        );
    }

    #[test]
    fn invested_goes_negative_when_sales_and_dividends_exceed_cost() {
        // Cost 1000, sold for 1100, dividends 20.006 -> 1000 - 1100 - 20.006 = -120.006 -> -120.01.
        let h = holdings(&[("VWCE", dec!(1))]);
        let v = valuation(&[("VWCE", dec!(500))]);
        let p = pnl(&[("VWCE", dec!(1000), dec!(1100), dec!(20.006))]);
        let rows = snapshot_rows(&h, &v, &p).unwrap();
        assert_eq!(rows[0].invested_pln, dec!(-120.01));
        assert_eq!(rows[0].value_pln.to_string(), "500.00");
    }

    #[test]
    fn a_day_with_an_unpriced_asset_produces_no_rows() {
        let h = holdings(&[("VWCE", dec!(1)), ("BTC", dec!(0.1)), ("ETH", dec!(1))]);
        let v = valuation(&[("BTC", dec!(30000))]);
        assert_eq!(
            snapshot_rows(&h, &v, &pnl(&[])),
            Err(SnapshotError::Unpriced(vec!["ETH".into(), "VWCE".into()]))
        );
    }

    #[test]
    fn a_sold_out_asset_keeps_a_row_with_what_its_sales_returned() {
        // ETH bought for 1 000.00 and sold out for 1 150.00: no price needed, quantity and value
        // 0, invested 1 000 - 1 150 = -150.00. BTC at quantity 0 without any flows has no row.
        let h = holdings(&[("VWCE", dec!(2)), ("BTC", dec!(0))]);
        let v = valuation(&[("VWCE", dec!(1000))]);
        let p = pnl(&[
            ("VWCE", dec!(990), dec!(0), dec!(0)),
            ("ETH", dec!(1000), dec!(1150), dec!(0)),
        ]);
        let rows = snapshot_rows(&h, &v, &p).unwrap();
        let shown: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.asset.as_str(),
                    r.quantity.to_string(),
                    r.value_pln.to_string(),
                    r.invested_pln.to_string(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                ("ETH", "0".into(), "0.00".into(), "-150.00".into()),
                ("VWCE", "2".into(), "1000.00".into(), "990.00".into()),
            ]
        );
    }

    #[test]
    fn a_negative_quantity_is_refused() {
        let h = holdings(&[("BTC", dec!(-0.1))]);
        let v = valuation(&[("BTC", dec!(0))]);
        assert_eq!(
            snapshot_rows(&h, &v, &pnl(&[])),
            Err(SnapshotError::Negative("BTC".into()))
        );
    }

    #[test]
    fn nothing_held_is_an_empty_day() {
        assert_eq!(
            snapshot_rows(&holdings(&[]), &valuation(&[]), &pnl(&[])),
            Ok(vec![])
        );
    }
}
