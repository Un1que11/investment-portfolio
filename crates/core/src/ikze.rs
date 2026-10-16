//! IKZE: money contributed in a calendar year, how much of that year's limit is left, and the
//! estimated tax saved by deducting the contributions from income.
//!
//! A contribution is a deposit into an account that is an IKZE, counted in the calendar year of
//! its ledger date (the UTC date of the operation). The limit for the year and the tax rate come
//! from `config.toml`.
//!
//! Withdrawals are counted separately and change nothing else. Taking money out does not give
//! any of the limit back (the limit caps what is paid in during the year, not the balance), and
//! the deduction is for what was paid in. A withdrawal before retirement (a "zwrot") makes the
//! deducted contributions taxable income in the year it happens; that tax is not estimated here.
//!
//! Payments above the limit are not deductible, so the estimate deducts at most the limit.
//! Results are not rounded.

use std::fmt;

use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;

use crate::config::Config;
use crate::portfolio::{LedgerTx, MissingRate, TxKind, Wrapper, in_pln};

/// Money paid into and taken out of IKZE accounts in one calendar year, in PLN.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IkzeFlows {
    pub year: i32,
    /// Deposits into IKZE accounts.
    pub contributed: Decimal,
    /// Withdrawals from IKZE accounts.
    pub withdrawn: Decimal,
}

/// IKZE deposits and withdrawals in the calendar year of `date`, up to the end of `date`. Each
/// is converted at `rate(currency, flow date)` (PLN per unit; never asked for PLN itself).
pub fn ikze_flows(
    txs: &[LedgerTx],
    date: NaiveDate,
    mut rate: impl FnMut(&str, NaiveDate) -> Option<Decimal>,
) -> Result<IkzeFlows, MissingRate> {
    let year = date.year();
    let mut flows = IkzeFlows {
        year,
        ..IkzeFlows::default()
    };
    let in_year =
        |t: &&LedgerTx| t.wrapper == Some(Wrapper::Ikze) && t.date.year() == year && t.date <= date;
    for tx in txs.iter().filter(in_year) {
        match tx.kind {
            TxKind::Deposit => flows.contributed += in_pln(tx, &mut rate)?,
            TxKind::Withdrawal => flows.withdrawn += in_pln(tx, &mut rate)?,
            _ => {}
        }
    }
    Ok(flows)
}

/// A year of IKZE contributions measured against the limit and the tax rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IkzeYear {
    pub year: i32,
    /// PLN paid in during the year.
    pub contributed: Decimal,
    /// PLN taken out during the year; it does not reduce `contributed` or free any limit.
    pub withdrawn: Decimal,
    /// The year's contribution limit from the config.
    pub limit: Decimal,
    /// What can still be paid in this year: limit minus contributions, never below zero.
    pub remaining: Decimal,
    /// Contributions above the limit; zero unless more was paid in than allowed.
    pub over_limit: Decimal,
    /// The part of the contributions deductible from income: all of them, up to the limit.
    pub deductible: Decimal,
    /// Income tax rate from the config, as a fraction (0.12 for 12%).
    pub tax_rate: Decimal,
    /// Estimated tax saved by the deduction: `deductible` x `tax_rate`.
    pub tax_saving: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IkzeError {
    /// `config.toml` has no IKZE limit for the year.
    NoLimit(i32),
}

impl fmt::Display for IkzeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IkzeError::NoLimit(year) => write!(
                f,
                "no IKZE limit for {year} in the config; add it under [ikze.limits]"
            ),
        }
    }
}

impl std::error::Error for IkzeError {}

/// Measures a year's flows against that year's limit and the tax rate in `config`.
pub fn ikze_year(flows: &IkzeFlows, config: &Config) -> Result<IkzeYear, IkzeError> {
    let limit = config
        .ikze_limit(flows.year)
        .ok_or(IkzeError::NoLimit(flows.year))?;
    let contributed = flows.contributed;
    let deductible = contributed.min(limit);
    Ok(IkzeYear {
        year: flows.year,
        contributed,
        withdrawn: flows.withdrawn,
        limit,
        remaining: limit - deductible,
        over_limit: contributed - deductible,
        deductible,
        tax_rate: config.tax_rate(),
        tax_saving: deductible * config.tax_rate(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::AccountKind;
    use rust_decimal_macros::dec;

    fn config(rate_percent: u32) -> Config {
        Config::from_toml(&format!(
            r#"
            [targets]
            vwce = 55
            crypto = 25
            gold = 20
            [targets.crypto_split]
            btc = 75
            eth = 25
            [ikze.limits]
            2025 = "9388.80"
            2026 = "11304.00"
            [tax]
            rate_percent = {rate_percent}
            [gold]
            buyback_factor = "0.95"
            "#
        ))
        .unwrap()
    }

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn tx(
        date: &str,
        account: &str,
        wrapper: Option<Wrapper>,
        kind: TxKind,
        amount: Decimal,
        currency: &str,
    ) -> LedgerTx {
        LedgerTx {
            date: d(date),
            account: account.into(),
            account_kind: AccountKind::Funded,
            wrapper,
            kind,
            asset: (kind == TxKind::Buy).then(|| "VWCE".into()),
            quantity: (kind == TxKind::Buy).then_some(dec!(1)),
            amount,
            currency: currency.into(),
        }
    }

    fn ikze(date: &str, kind: TxKind, amount: Decimal) -> LedgerTx {
        tx(date, "XTB IKZE", Some(Wrapper::Ikze), kind, amount, "PLN")
    }

    fn pln_only(_: &str, _: NaiveDate) -> Option<Decimal> {
        None
    }

    fn year_on(txs: &[LedgerTx], date: &str, config: &Config) -> IkzeYear {
        ikze_year(&ikze_flows(txs, d(date), pln_only).unwrap(), config).unwrap()
    }

    #[test]
    fn contributions_this_year_remaining_limit_and_tax_saving() {
        // 3000 + 2500.50 = 5500.50 paid in during 2026. Limit 11 304: 5803.50 left. All of it
        // is deductible; at 12% the tax saved is 5500.50 x 0.12 = 660.06.
        let txs = [
            ikze("2026-01-15", TxKind::Deposit, dec!(3000)),
            ikze("2026-01-16", TxKind::Buy, dec!(2900)),
            ikze("2026-06-01", TxKind::Deposit, dec!(2500.50)),
        ];
        let y = year_on(&txs, "2026-10-16", &config(12));

        assert_eq!(
            y,
            IkzeYear {
                year: 2026,
                contributed: dec!(5500.50),
                withdrawn: dec!(0),
                limit: dec!(11304),
                remaining: dec!(5803.50),
                over_limit: dec!(0),
                deductible: dec!(5500.50),
                tax_rate: dec!(0.12),
                tax_saving: dec!(660.06),
            }
        );
    }

    #[test]
    fn only_ikze_deposits_of_the_year_up_to_the_date_count() {
        // Counted: 1000 on 1 Jan 2026 only. Not counted: the 31 Dec 2025 deposit (last year),
        // the IKE and Kraken deposits (not an IKZE), the purchase and the fee (not money paid
        // in), and the 20 Oct deposit (after the date).
        let txs = [
            ikze("2025-12-31", TxKind::Deposit, dec!(5000)),
            ikze("2026-01-01", TxKind::Deposit, dec!(1000)),
            tx(
                "2026-02-01",
                "XTB IKE",
                Some(Wrapper::Ike),
                TxKind::Deposit,
                dec!(700),
                "PLN",
            ),
            tx(
                "2026-02-01",
                "Kraken",
                None,
                TxKind::Deposit,
                dec!(300),
                "PLN",
            ),
            ikze("2026-02-02", TxKind::Buy, dec!(900)),
            ikze("2026-02-02", TxKind::Fee, dec!(5)),
            ikze("2026-10-20", TxKind::Deposit, dec!(2000)),
        ];
        let y = year_on(&txs, "2026-10-16", &config(12));

        assert_eq!((y.year, y.contributed), (2026, dec!(1000)));
        // 11 304 - 1000 = 10 304 left; 1000 x 0.12 = 120 saved.
        assert_eq!((y.remaining, y.tax_saving), (dec!(10304), dec!(120)));

        // On 31 Dec 2025 the year is 2025: 5000 of 9388.80, 4388.80 left, 600 saved.
        let y = year_on(&txs, "2025-12-31", &config(12));
        assert_eq!((y.year, y.contributed), (2025, dec!(5000)));
        assert_eq!((y.remaining, y.tax_saving), (dec!(4388.80), dec!(600)));
    }

    #[test]
    fn the_whole_limit_paid_in_leaves_nothing() {
        // 11 304 in two deposits: nothing left, 11 304 x 0.32 = 3617.28 saved at 32%.
        let txs = [
            ikze("2026-03-01", TxKind::Deposit, dec!(10000)),
            ikze("2026-12-20", TxKind::Deposit, dec!(1304)),
        ];
        let y = year_on(&txs, "2026-12-31", &config(32));

        assert_eq!((y.remaining, y.over_limit), (dec!(0), dec!(0)));
        assert_eq!((y.deductible, y.tax_saving), (dec!(11304), dec!(3617.28)));
    }

    #[test]
    fn payments_above_the_limit_are_not_deductible() {
        // 12 000 paid in: 696 over the limit, nothing left. Only 11 304 is deductible:
        // 11 304 x 0.12 = 1356.48, not 12 000 x 0.12 = 1440.
        let txs = [ikze("2026-05-05", TxKind::Deposit, dec!(12000))];
        let y = year_on(&txs, "2026-10-16", &config(12));

        assert_eq!(y.contributed, dec!(12000));
        assert_eq!((y.remaining, y.over_limit), (dec!(0), dec!(696)));
        assert_eq!((y.deductible, y.tax_saving), (dec!(11304), dec!(1356.48)));
    }

    #[test]
    fn a_withdrawal_gives_no_limit_back() {
        // 4000 in, 1000 out. The limit counts payments in: 11 304 - 4000 = 7304 left, and the
        // deduction is still for 4000 (480 at 12%). The withdrawal is reported on its own.
        let txs = [
            ikze("2026-02-01", TxKind::Deposit, dec!(4000)),
            ikze("2026-09-01", TxKind::Withdrawal, dec!(1000)),
        ];
        let y = year_on(&txs, "2026-10-16", &config(12));

        assert_eq!((y.contributed, y.withdrawn), (dec!(4000), dec!(1000)));
        assert_eq!((y.remaining, y.tax_saving), (dec!(7304), dec!(480)));
    }

    #[test]
    fn nothing_paid_in_leaves_the_whole_limit() {
        let y = year_on(&[], "2026-01-10", &config(12));

        assert_eq!((y.contributed, y.remaining), (dec!(0), dec!(11304)));
        assert_eq!(y.tax_saving, dec!(0));
    }

    #[test]
    fn foreign_currency_deposits_use_the_rate_of_their_date() {
        // 100 EUR at 4.30 on 2 Mar = 430 PLN, plus 570 PLN = 1000.
        let txs = [
            tx(
                "2026-03-02",
                "XTB IKZE",
                Some(Wrapper::Ikze),
                TxKind::Deposit,
                dec!(100),
                "EUR",
            ),
            ikze("2026-03-03", TxKind::Deposit, dec!(570)),
        ];
        let rate =
            |c: &str, day: NaiveDate| (c == "EUR" && day == d("2026-03-02")).then_some(dec!(4.30));
        let flows = ikze_flows(&txs, d("2026-10-16"), rate).unwrap();
        assert_eq!(flows.contributed, dec!(1000));

        assert_eq!(
            ikze_flows(&txs, d("2026-10-16"), pln_only),
            Err(MissingRate {
                currency: "EUR".into(),
                date: d("2026-03-02"),
            })
        );
    }

    #[test]
    fn a_year_without_a_configured_limit_is_an_error() {
        let flows = ikze_flows(&[], d("2027-01-04"), pln_only).unwrap();
        let err = ikze_year(&flows, &config(12)).unwrap_err();

        assert_eq!(err, IkzeError::NoLimit(2027));
        assert_eq!(
            err.to_string(),
            "no IKZE limit for 2027 in the config; add it under [ikze.limits]"
        );
    }

    #[test]
    fn the_repository_config_has_the_2026_limit() {
        let text = include_str!("../../../config.toml");
        let config = Config::from_toml(text).unwrap();
        let y = ikze_year(
            &ikze_flows(&[], d("2026-10-16"), pln_only).unwrap(),
            &config,
        )
        .unwrap();

        assert_eq!(y.limit, dec!(11304));
        assert!(y.tax_rate == dec!(0.12) || y.tax_rate == dec!(0.32));
    }
}
