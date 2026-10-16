//! Positions, cash, net contributions and value on a date, from the transaction ledger.
//!
//! Conventions (matching how the importers write `transactions`):
//! - Funded accounts (brokers, exchanges) receive money through deposits; money put into the
//!   portfolio is deposits minus withdrawals. Buys and fees are paid from the account's cash.
//! - Manual accounts (physical gold) have no deposits: each purchase is money put in from
//!   outside, and a sale is money taken out. They hold no cash.
//! - A dividend whose currency is the asset itself (a staking reward) adds to the quantity; a
//!   cash dividend adds to cash. Neither is a contribution.
//! - A fee whose currency is the asset itself is informational: the importer has already netted
//!   it out of the quantity, so it is not subtracted again.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    /// Money arrives by deposit (XTB, Kraken).
    Funded,
    /// Holdings bought with outside money directly (physical gold).
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxKind {
    Buy,
    Sell,
    Deposit,
    Withdrawal,
    Fee,
    Dividend,
}

impl TxKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "buy" => TxKind::Buy,
            "sell" => TxKind::Sell,
            "deposit" => TxKind::Deposit,
            "withdrawal" => TxKind::Withdrawal,
            "fee" => TxKind::Fee,
            "dividend" => TxKind::Dividend,
            _ => return None,
        })
    }
}

/// One row of the ledger, as the calculations need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerTx {
    pub date: NaiveDate,
    pub account: String,
    pub account_kind: AccountKind,
    pub kind: TxKind,
    /// Asset symbol, when the transaction concerns an asset.
    pub asset: Option<String>,
    pub quantity: Option<Decimal>,
    /// Always non-negative; direction comes from `kind`.
    pub amount: Decimal,
    /// Currency of `amount`; for in-kind rows, the asset symbol itself.
    pub currency: String,
}

impl LedgerTx {
    /// The row is denominated in its own asset (staking reward, crypto fee).
    fn in_kind(&self) -> bool {
        self.asset.as_deref() == Some(self.currency.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holdings {
    /// Asset symbol -> quantity held (zero positions are dropped).
    pub assets: BTreeMap<String, Decimal>,
    /// (account, currency) -> cash balance (zero balances are dropped).
    pub cash: BTreeMap<(String, String), Decimal>,
}

/// Holdings at the end of `date` (transactions on that day included).
pub fn holdings_on(txs: &[LedgerTx], date: NaiveDate) -> Holdings {
    let mut h = Holdings::default();
    for tx in txs.iter().filter(|t| t.date <= date) {
        let qty = tx.quantity.unwrap_or_default();
        let funded = tx.account_kind == AccountKind::Funded;
        let mut cash = |delta: Decimal| {
            if funded {
                *h.cash
                    .entry((tx.account.clone(), tx.currency.clone()))
                    .or_default() += delta;
            }
        };
        match tx.kind {
            TxKind::Deposit => cash(tx.amount),
            TxKind::Withdrawal => cash(-tx.amount),
            TxKind::Buy => {
                cash(-tx.amount);
                if let Some(a) = &tx.asset {
                    *h.assets.entry(a.clone()).or_default() += qty;
                }
            }
            TxKind::Sell => {
                cash(tx.amount);
                if let Some(a) = &tx.asset {
                    *h.assets.entry(a.clone()).or_default() -= qty;
                }
            }
            TxKind::Fee if tx.in_kind() => {}
            TxKind::Fee => cash(-tx.amount),
            TxKind::Dividend if tx.in_kind() => {
                if let Some(a) = &tx.asset {
                    *h.assets.entry(a.clone()).or_default() += qty;
                }
            }
            TxKind::Dividend => cash(tx.amount),
        }
    }
    h.assets.retain(|_, q| !q.is_zero());
    h.cash.retain(|_, v| !v.is_zero());
    h
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingRate {
    pub currency: String,
    pub date: NaiveDate,
}

impl std::fmt::Display for MissingRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no {}->PLN rate for {}", self.currency, self.date)
    }
}

impl std::error::Error for MissingRate {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contributions {
    /// PLN put in minus PLN taken out, all accounts.
    pub total: Decimal,
    pub by_account: BTreeMap<String, Decimal>,
}

/// Net money put into the portfolio up to the end of `date`, in PLN. Each flow is converted at
/// `rate(currency, flow date)` (PLN per unit; never asked for PLN itself).
pub fn net_contributions_on(
    txs: &[LedgerTx],
    date: NaiveDate,
    mut rate: impl FnMut(&str, NaiveDate) -> Option<Decimal>,
) -> Result<Contributions, MissingRate> {
    let mut c = Contributions::default();
    for tx in txs.iter().filter(|t| t.date <= date) {
        let Some(sign) = contribution_sign(tx) else {
            continue;
        };
        let signed = sign * in_pln(tx, &mut rate)?;
        c.total += signed;
        *c.by_account.entry(tx.account.clone()).or_default() += signed;
    }
    Ok(c)
}

/// 1 for money put into the portfolio, -1 for money taken out, None for anything else.
pub(crate) fn contribution_sign(tx: &LedgerTx) -> Option<Decimal> {
    match (tx.account_kind, tx.kind) {
        (AccountKind::Funded, TxKind::Deposit) | (AccountKind::Manual, TxKind::Buy) => {
            Some(Decimal::ONE)
        }
        (AccountKind::Funded, TxKind::Withdrawal) | (AccountKind::Manual, TxKind::Sell) => {
            Some(Decimal::NEGATIVE_ONE)
        }
        _ => None,
    }
}

/// The row's amount in PLN at its own date's rate.
pub(crate) fn in_pln(
    tx: &LedgerTx,
    rate: &mut impl FnMut(&str, NaiveDate) -> Option<Decimal>,
) -> Result<Decimal, MissingRate> {
    if tx.currency == "PLN" {
        return Ok(tx.amount);
    }
    let r = rate(&tx.currency, tx.date).ok_or_else(|| MissingRate {
        currency: tx.currency.clone(),
        date: tx.date,
    })?;
    Ok(tx.amount * r)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Valuation {
    /// Asset symbol -> PLN value of the position.
    pub by_asset: BTreeMap<String, Decimal>,
    /// (account, currency) -> PLN value of the cash balance.
    pub cash: BTreeMap<(String, String), Decimal>,
    /// Sum of everything that could be valued.
    pub total: Decimal,
    /// Positions or cash currencies with no price; not included in `total`.
    pub missing: Vec<String>,
}

impl Valuation {
    pub fn complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Values holdings in PLN. `position_value(asset, quantity)` gives a whole position's value
/// (so assets valued item by item, like gold coins, fit too); `cash_rate(currency)` gives PLN
/// per unit. Results are not rounded.
pub fn value(
    holdings: &Holdings,
    mut position_value: impl FnMut(&str, Decimal) -> Option<Decimal>,
    mut cash_rate: impl FnMut(&str) -> Option<Decimal>,
) -> Valuation {
    let mut v = Valuation::default();
    for (asset, qty) in &holdings.assets {
        match position_value(asset, *qty) {
            Some(pln) => {
                v.total += pln;
                v.by_asset.insert(asset.clone(), pln);
            }
            None => v.missing.push(asset.clone()),
        }
    }
    for ((account, currency), amount) in &holdings.cash {
        let rate = if currency == "PLN" {
            Some(Decimal::ONE)
        } else {
            cash_rate(currency)
        };
        match rate {
            Some(r) => {
                let pln = amount * r;
                v.total += pln;
                v.cash.insert((account.clone(), currency.clone()), pln);
            }
            None => v.missing.push(format!("cash {currency} ({account})")),
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// XTB in PLN, Kraken in EUR, physical gold bought with outside money.
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
            // Fee taken in BTC: already netted out of the quantity above.
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

    fn eur_rate(currency: &str, date: NaiveDate) -> Option<Decimal> {
        // NBP EUR mid on the deposit day, by hand: 4.2500.
        (currency == "EUR" && date == d(3)).then_some(dec!(4.2500))
    }

    #[test]
    fn holdings_at_month_end() {
        let h = holdings_on(&ledger(), d(31));

        assert_eq!(
            h.assets,
            BTreeMap::from([
                ("BTC".into(), dec!(0.002)),
                ("ETH".into(), dec!(0.0001)),
                ("GOLD_COINS".into(), dec!(1)),
                ("VWCE".into(), dec!(2)),
            ])
        );
        // XTB: 1000 - 609 - 1 = 390 PLN. Kraken: 200 - 180 - 2.70 = 17.30 EUR (the BTC fee is
        // not cash). Gold holds no cash.
        assert_eq!(
            h.cash,
            BTreeMap::from([
                (("Kraken".into(), "EUR".into()), dec!(17.30)),
                (("XTB".into(), "PLN".into()), dec!(390.00)),
            ])
        );
    }

    #[test]
    fn holdings_respect_the_date() {
        let h = holdings_on(&ledger(), d(4));

        // Only the Kraken deposit, buy and fee have happened besides the XTB deposit.
        assert_eq!(h.assets, BTreeMap::from([("BTC".into(), dec!(0.002))]));
        assert_eq!(h.cash[&("XTB".into(), "PLN".into())], dec!(1000));
        assert_eq!(h.cash[&("Kraken".into(), "EUR".into())], dec!(17.30));
        assert_eq!(holdings_on(&ledger(), d(1)), Holdings::default());
    }

    #[test]
    fn sell_and_withdrawal() {
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
        txs.push(tx(13, "XTB", Withdrawal, None, None, dec!(100), "PLN"));
        txs.push(tx(
            14,
            "Gold",
            Sell,
            Some("GOLD_COINS"),
            Some(dec!(1)),
            dec!(1650),
            "PLN",
        ));

        let h = holdings_on(&txs, d(31));

        assert_eq!(h.assets.get("VWCE"), Some(&dec!(1)));
        // The only coin was sold: the position disappears.
        assert_eq!(h.assets.get("GOLD_COINS"), None);
        // 390 + 320 - 100 = 610 PLN at XTB; selling gold adds no cash anywhere.
        assert_eq!(h.cash[&("XTB".into(), "PLN".into())], dec!(610));
        assert_eq!(h.cash.len(), 2);

        // Contributions: 1000 - 100 (XTB) + 850 (Kraken) + 1600 - 1650 (gold) = 1700.
        let c = net_contributions_on(&txs, d(31), eur_rate).unwrap();
        assert_eq!(c.total, dec!(1700));
        assert_eq!(c.by_account["Gold"], dec!(-50));
    }

    #[test]
    fn net_contributions_in_pln() {
        let c = net_contributions_on(&ledger(), d(31), eur_rate).unwrap();

        // XTB 1000 PLN; Kraken 200 EUR x 4.25 = 850 PLN; gold coin 1600 PLN. Fees, buys inside
        // funded accounts and the staking reward are not contributions.
        assert_eq!(c.total, dec!(3450));
        assert_eq!(
            c.by_account,
            BTreeMap::from([
                ("Gold".into(), dec!(1600)),
                ("Kraken".into(), dec!(850.0000)),
                ("XTB".into(), dec!(1000)),
            ])
        );
        // Before the gold purchase: 1000 + 850.
        assert_eq!(
            net_contributions_on(&ledger(), d(4), eur_rate)
                .unwrap()
                .total,
            dec!(1850)
        );
    }

    #[test]
    fn missing_rate_is_an_error() {
        let err = net_contributions_on(&ledger(), d(31), |_, _| None).unwrap_err();

        assert_eq!(
            err,
            MissingRate {
                currency: "EUR".into(),
                date: d(3)
            }
        );
    }

    #[test]
    fn value_with_prices() {
        let h = holdings_on(&ledger(), d(31));
        let unit = |a: &str| match a {
            "VWCE" => Some(dec!(300)),
            "BTC" => Some(dec!(400000)),
            "ETH" => Some(dec!(15000)),
            _ => None,
        };

        let v = value(
            &h,
            |asset, qty| match asset {
                // Gold is valued coin by coin; here the one coin is worth 1700 PLN.
                "GOLD_COINS" => Some(dec!(1700)),
                other => unit(other).map(|p| p * qty),
            },
            |currency| (currency == "EUR").then_some(dec!(4.30)),
        );

        // VWCE 2 x 300 = 600; BTC 0.002 x 400000 = 800; ETH 0.0001 x 15000 = 1.5; gold 1700;
        // cash 390 + 17.30 x 4.30 = 390 + 74.39 = 464.39. Total 3565.89.
        assert_eq!(v.by_asset["VWCE"], dec!(600));
        assert_eq!(v.by_asset["BTC"], dec!(800));
        assert_eq!(v.by_asset["ETH"], dec!(1.5));
        assert_eq!(v.cash[&("Kraken".into(), "EUR".into())], dec!(74.39));
        assert_eq!(v.total, dec!(3565.89));
        assert!(v.complete());
    }

    #[test]
    fn missing_prices_are_reported_and_left_out() {
        let h = holdings_on(&ledger(), d(31));

        let v = value(
            &h,
            |asset, qty| (asset == "VWCE").then(|| qty * dec!(300)),
            |_| None,
        );

        // Only VWCE (600) and PLN cash (390) can be valued.
        assert_eq!(v.total, dec!(990));
        assert_eq!(v.missing, ["BTC", "ETH", "GOLD_COINS", "cash EUR (Kraken)"]);
        assert!(!v.complete());
    }

    #[test]
    fn tx_kind_parse() {
        assert_eq!(TxKind::parse("dividend"), Some(TxKind::Dividend));
        assert_eq!(TxKind::parse("transfer"), None);
    }
}
