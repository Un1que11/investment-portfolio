//! Profit and loss in PLN: for the whole portfolio and per asset.
//!
//! - Portfolio: P&L = value - net contributions; % = P&L / net contributions.
//! - Per asset: P&L = value - cost + proceeds + income, where cost = buys + the asset's fees,
//!   proceeds = sales and income = cash dividends, each converted at its own date's rate. So for
//!   assets bought in EUR, the EUR->PLN move since purchase is part of the asset's result.
//!   % = P&L / cost (none for holdings that cost nothing, like staking rewards).
//! - The rest of the portfolio P&L (cash left in accounts, currency moves on that cash, fees
//!   not tied to an asset) is reported as `other`, so the parts always add up to the total.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::portfolio::{Contributions, LedgerTx, MissingRate, TxKind, Valuation, in_pln};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetPnl {
    /// Buys plus the asset's fees, PLN.
    pub cost: Decimal,
    /// Sale proceeds, PLN.
    pub proceeds: Decimal,
    /// Cash dividends, PLN (in-kind rewards are part of the value instead).
    pub income: Decimal,
    /// Value on the date; None without a price.
    pub value: Option<Decimal>,
}

impl AssetPnl {
    /// cost - proceeds - income: what is still "in" the asset.
    pub fn net_invested(&self) -> Decimal {
        self.cost - self.proceeds - self.income
    }

    pub fn pnl(&self) -> Option<Decimal> {
        self.value.map(|v| v - self.net_invested())
    }

    /// P&L as a fraction of cost (0.05 = 5%).
    pub fn pnl_ratio(&self) -> Option<Decimal> {
        let pnl = self.pnl()?;
        (!self.cost.is_zero()).then(|| pnl / self.cost)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioPnl {
    pub value: Decimal,
    pub contributions: Decimal,
    /// False when some position had no price (value and P&L are then understated).
    pub complete: bool,
    pub by_asset: BTreeMap<String, AssetPnl>,
}

impl PortfolioPnl {
    /// Value minus contributions. When `complete` is false the value leaves out unpriced
    /// positions, so this understates the result; prefer [`Self::pnl_if_complete`] for display.
    pub fn pnl(&self) -> Decimal {
        self.value - self.contributions
    }

    /// The P&L only when every position had a price.
    pub fn pnl_if_complete(&self) -> Option<Decimal> {
        self.complete.then(|| self.pnl())
    }

    /// P&L as a fraction of net contributions; None if nothing (net) was put in or the
    /// valuation is incomplete.
    pub fn pnl_ratio(&self) -> Option<Decimal> {
        let pnl = self.pnl_if_complete()?;
        (self.contributions > Decimal::ZERO).then(|| pnl / self.contributions)
    }

    /// Portfolio P&L not explained by assets: cash, currency moves on cash, account fees.
    /// None when an asset has no value.
    pub fn other(&self) -> Option<Decimal> {
        let assets: Option<Decimal> = self.by_asset.values().map(AssetPnl::pnl).sum();
        assets.map(|a| self.pnl() - a)
    }
}

/// Per-asset cost, proceeds and income up to the end of `date`, in PLN at each row's date.
/// In-kind rows (staking rewards, fees taken in the asset) carry no cost.
pub fn asset_flows(
    txs: &[LedgerTx],
    date: NaiveDate,
    mut rate: impl FnMut(&str, NaiveDate) -> Option<Decimal>,
) -> Result<BTreeMap<String, AssetPnl>, MissingRate> {
    let mut out: BTreeMap<String, AssetPnl> = BTreeMap::new();
    for tx in txs.iter().filter(|t| t.date <= date) {
        let Some(asset) = &tx.asset else { continue };
        // In-kind rows still make the asset appear (it may have no other rows).
        let entry = out.entry(asset.clone()).or_default();
        let Some(flow) = asset_flow(tx) else { continue };
        let pln = in_pln(tx, &mut rate)?;
        match flow {
            AssetFlow::Cost => entry.cost += pln,
            AssetFlow::Proceeds => entry.proceeds += pln,
            AssetFlow::Income => entry.income += pln,
        }
    }
    Ok(out)
}

/// What an asset row means in money terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssetFlow {
    /// Buy or fee: paid for the asset.
    Cost,
    /// Sale.
    Proceeds,
    /// Cash dividend.
    Income,
}

/// The money flow of an asset row; None for rows without one (no asset, or in kind).
pub(crate) fn asset_flow(tx: &LedgerTx) -> Option<AssetFlow> {
    let asset = tx.asset.as_ref()?;
    if tx.currency == *asset {
        return None;
    }
    match tx.kind {
        TxKind::Buy | TxKind::Fee => Some(AssetFlow::Cost),
        TxKind::Sell => Some(AssetFlow::Proceeds),
        TxKind::Dividend => Some(AssetFlow::Income),
        TxKind::Deposit | TxKind::Withdrawal => None,
    }
}

/// Combines per-asset flows with the valuation and contributions of the same date.
pub fn portfolio_pnl(
    mut flows: BTreeMap<String, AssetPnl>,
    valuation: &Valuation,
    contributions: &Contributions,
) -> PortfolioPnl {
    for (asset, entry) in flows.iter_mut() {
        entry.value = valuation.by_asset.get(asset).copied();
    }
    // Assets held but with no flows at all (should not happen with importers' data) still show.
    for (asset, value) in &valuation.by_asset {
        flows.entry(asset.clone()).or_insert_with(|| AssetPnl {
            value: Some(*value),
            ..AssetPnl::default()
        });
    }
    // Fully sold assets have no value entry: their value is zero, not unknown.
    for (asset, entry) in flows.iter_mut() {
        if entry.value.is_none() && !valuation.missing.iter().any(|m| m == asset) {
            entry.value = Some(Decimal::ZERO);
        }
    }
    PortfolioPnl {
        value: valuation.total,
        contributions: contributions.total,
        complete: valuation.complete(),
        by_asset: flows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::{AccountKind, holdings_on, net_contributions_on, value};
    use chrono::Datelike;
    use rust_decimal_macros::dec;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
    }

    fn tx(
        day: u32,
        account: &str,
        kind: TxKind,
        asset: Option<&str>,
        quantity: Option<Decimal>,
        amount: Decimal,
        currency: &str,
    ) -> LedgerTx {
        LedgerTx {
            date: d(day),
            account: account.into(),
            account_kind: if account == "Gold" {
                AccountKind::Manual
            } else {
                AccountKind::Funded
            },
            kind,
            asset: asset.map(Into::into),
            quantity,
            amount,
            currency: currency.into(),
        }
    }

    /// The same ledger as the portfolio tests: XTB (PLN), Kraken (EUR), physical gold.
    fn ledger() -> Vec<LedgerTx> {
        use TxKind::*;
        vec![
            tx(2, "XTB", Deposit, None, None, dec!(1000), "PLN"),
            tx(3, "Kraken", Deposit, None, None, dec!(200), "EUR"),
            tx(
                4,
                "Kraken",
                Buy,
                Some("BTC"),
                Some(dec!(0.002)),
                dec!(180),
                "EUR",
            ),
            tx(4, "Kraken", Fee, Some("BTC"), None, dec!(2.70), "EUR"),
            tx(4, "Kraken", Fee, Some("BTC"), None, dec!(0.00001), "BTC"),
            tx(
                5,
                "XTB",
                Buy,
                Some("VWCE"),
                Some(dec!(2)),
                dec!(609.00),
                "PLN",
            ),
            tx(
                6,
                "Kraken",
                Dividend,
                Some("ETH"),
                Some(dec!(0.0001)),
                dec!(0.0001),
                "ETH",
            ),
            tx(
                7,
                "Gold",
                Buy,
                Some("GOLD_COINS"),
                Some(dec!(1)),
                dec!(1600),
                "PLN",
            ),
            tx(10, "XTB", Fee, None, None, dec!(1.00), "PLN"),
        ]
    }

    /// NBP EUR mids by hand: 4.2500 on the deposit day, 4.3000 on the purchase day.
    fn rate(currency: &str, date: NaiveDate) -> Option<Decimal> {
        match (currency, date.day()) {
            ("EUR", 3) => Some(dec!(4.2500)),
            ("EUR", 4) => Some(dec!(4.3000)),
            _ => None,
        }
    }

    fn pnl_at_month_end(txs: &[LedgerTx]) -> PortfolioPnl {
        let holdings = holdings_on(txs, d(31));
        let valuation = value(
            &holdings,
            |asset, qty| match asset {
                "VWCE" => Some(qty * dec!(300)),
                "BTC" => Some(qty * dec!(400000)),
                "ETH" => Some(qty * dec!(15000)),
                "GOLD_COINS" => Some(dec!(1700)),
                _ => None,
            },
            |c| (c == "EUR").then_some(dec!(4.30)),
        );
        let contributions = net_contributions_on(txs, d(31), rate).unwrap();
        portfolio_pnl(
            asset_flows(txs, d(31), rate).unwrap(),
            &valuation,
            &contributions,
        )
    }

    #[test]
    fn per_asset_pnl() {
        let p = pnl_at_month_end(&ledger());
        let a = |s: &str| &p.by_asset[s];

        // VWCE: value 2 x 300 = 600, cost 609 -> -9; -9 / 609 = -1.48%.
        assert_eq!(
            (a("VWCE").cost, a("VWCE").pnl()),
            (dec!(609.00), Some(dec!(-9.00)))
        );
        assert_eq!(a("VWCE").pnl_ratio().unwrap().round_dp(4), dec!(-0.0148));
        // BTC: cost (180 + 2.70) EUR x 4.30 = 785.61 (the fee taken in BTC is not a cost);
        // value 0.002 x 400000 = 800 -> +14.39; 14.39 / 785.61 = 1.83%.
        assert_eq!(a("BTC").cost, dec!(785.61));
        assert_eq!(a("BTC").pnl(), Some(dec!(14.39)));
        assert_eq!(a("BTC").pnl_ratio().unwrap().round_dp(4), dec!(0.0183));
        // ETH from staking only: no cost, value 1.5 is all profit, no percentage.
        assert_eq!(
            (a("ETH").cost, a("ETH").pnl(), a("ETH").pnl_ratio()),
            (dec!(0), Some(dec!(1.5)), None)
        );
        // Gold: 1700 - 1600 = +100; 100 / 1600 = 6.25%.
        assert_eq!(a("GOLD_COINS").pnl(), Some(dec!(100)));
        assert_eq!(a("GOLD_COINS").pnl_ratio(), Some(dec!(0.0625)));
    }

    #[test]
    fn portfolio_pnl_and_the_rest() {
        let p = pnl_at_month_end(&ledger());

        // Value 3565.89 - contributions 3450 = 115.89; 115.89 / 3450 = 3.36%.
        assert_eq!(p.pnl(), dec!(115.89));
        assert_eq!(p.pnl_ratio().unwrap().round_dp(4), dec!(0.0336));
        assert!(p.complete);
        // Assets: -9 + 14.39 + 1.5 + 100 = 106.89. The other 9.00: EUR deposited at 4.25 but
        // spent or valued at 4.30 (200 EUR x 0.05 = +10.00) minus the 1.00 XTB account fee.
        assert_eq!(p.other(), Some(dec!(9.00)));
    }

    #[test]
    fn sales_and_cash_dividends() {
        use TxKind::*;
        let mut txs = ledger();
        txs.push(tx(
            12,
            "XTB",
            Sell,
            Some("VWCE"),
            Some(dec!(1)),
            dec!(320),
            "PLN",
        ));
        txs.push(tx(13, "XTB", Dividend, Some("VWCE"), None, dec!(5), "PLN"));

        let p = pnl_at_month_end(&txs);
        let vwce = &p.by_asset["VWCE"];

        // One unit left at 300; cost 609, proceeds 320, income 5:
        // P&L = 300 - (609 - 320 - 5) = 300 - 284 = +16; 16 / 609 = 2.63%.
        assert_eq!((vwce.proceeds, vwce.income), (dec!(320), dec!(5)));
        assert_eq!(vwce.net_invested(), dec!(284.00));
        assert_eq!(vwce.pnl(), Some(dec!(16.00)));
        assert_eq!(vwce.pnl_ratio().unwrap().round_dp(4), dec!(0.0263));
    }

    #[test]
    fn fully_sold_asset_counts_as_zero_value() {
        use TxKind::*;
        let mut txs = ledger();
        txs.push(tx(
            14,
            "Gold",
            Sell,
            Some("GOLD_COINS"),
            Some(dec!(1)),
            dec!(1650),
            "PLN",
        ));

        let p = pnl_at_month_end(&txs);

        // Sold for 1650, bought for 1600: +50 realised, no value left.
        assert_eq!(p.by_asset["GOLD_COINS"].value, Some(dec!(0)));
        assert_eq!(p.by_asset["GOLD_COINS"].pnl(), Some(dec!(50)));
    }

    #[test]
    fn missing_price_leaves_asset_pnl_unknown() {
        let txs = ledger();
        let holdings = holdings_on(&txs, d(31));
        let valuation = value(
            &holdings,
            |a, q| (a == "VWCE").then(|| q * dec!(300)),
            |_| Some(dec!(4.30)),
        );
        let contributions = net_contributions_on(&txs, d(31), rate).unwrap();

        let p = portfolio_pnl(
            asset_flows(&txs, d(31), rate).unwrap(),
            &valuation,
            &contributions,
        );

        assert_eq!(p.by_asset["BTC"].pnl(), None);
        assert!(!p.complete);
        assert_eq!(p.other(), None);
        // The portfolio total would understate the result: not offered for display.
        assert_eq!((p.pnl_if_complete(), p.pnl_ratio()), (None, None));
    }

    #[test]
    fn missing_rate_for_an_asset_flow_is_an_error() {
        let err = asset_flows(&ledger(), d(31), |_, _| None).unwrap_err();

        assert_eq!((err.currency.as_str(), err.date), ("EUR", d(4)));
    }

    #[test]
    fn no_contributions_gives_no_ratio() {
        let p = PortfolioPnl {
            value: dec!(0),
            contributions: dec!(0),
            complete: true,
            by_asset: BTreeMap::new(),
        };
        assert_eq!(p.pnl_ratio(), None);
    }
}
