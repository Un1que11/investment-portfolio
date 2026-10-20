//! From a ledger and fixed prices (no network) to stored snapshot rows.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use portfolio_core::AssetClass;
use portfolio_core::portfolio::{AccountKind, LedgerTx, TxKind, Wrapper};
use portfolio_core::snapshot::SnapshotRow;
use portfolio_db::prices::ensure_asset;
use portfolio_db::snapshots::{load_day, replace_day};
use portfolio_sources::prices::LastKnown;
use portfolio_sources::valuation::{Inputs, report};
use portfolio_sync::{SnapshotFailure, write_snapshot};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn d(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn tx(
    date: &str,
    kind: TxKind,
    asset: Option<&str>,
    quantity: Option<Decimal>,
    amount: Decimal,
) -> LedgerTx {
    LedgerTx {
        date: d(date),
        account: "IKZE".into(),
        account_kind: AccountKind::Funded,
        wrapper: Some(Wrapper::Ikze),
        kind,
        asset: asset.map(Into::into),
        quantity,
        amount,
        currency: "PLN".into(),
    }
}

/// Deposit 2 000 PLN, buy 3 VWCE for 1 500.00 PLN plus a 4.995 PLN fee, then 1 more for 520.
/// (All in PLN, so no exchange rates are needed.)
fn ledger() -> Vec<LedgerTx> {
    vec![
        tx("2026-09-01", TxKind::Deposit, None, None, dec!(2000)),
        tx(
            "2026-09-02",
            TxKind::Buy,
            Some("VWCE"),
            Some(dec!(3)),
            dec!(1500.00),
        ),
        tx("2026-09-02", TxKind::Fee, Some("VWCE"), None, dec!(4.995)),
        tx("2026-10-01", TxKind::Deposit, None, None, dec!(500)),
        tx(
            "2026-10-01",
            TxKind::Buy,
            Some("VWCE"),
            Some(dec!(1)),
            dec!(520),
        ),
    ]
}

fn inputs(vwce: Option<Decimal>) -> Inputs {
    Inputs {
        position_values: BTreeMap::from([("VWCE".to_owned(), vwce)]),
        ..Inputs::default()
    }
}

#[tokio::test]
async fn a_priced_day_is_stored_per_asset() {
    let pool = portfolio_db::open_in_memory().await.unwrap();
    ensure_asset(&pool, "VWCE", "VWCE", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    // 4 units at 538.70625 PLN = 2154.825 -> 2154.83 (half away from zero).
    // Invested: 1500.00 + 4.995 + 520 = 2024.995 -> 2025.00. The 475.005 PLN cash is not an asset.
    let r = report(&ledger(), d("2026-10-19"), &inputs(Some(dec!(2154.825)))).unwrap();
    assert_eq!(write_snapshot(&pool, &r).await.unwrap(), 1);
    let rows = load_day(&pool, d("2026-10-19")).await.unwrap();
    assert_eq!(
        rows,
        vec![SnapshotRow {
            asset: "VWCE".into(),
            quantity: dec!(4),
            value_pln: dec!(2154.83),
            invested_pln: dec!(2025.00),
        }]
    );
    assert_eq!(rows[0].invested_pln.to_string(), "2025.00");
}

#[tokio::test]
async fn an_unpriced_day_writes_nothing_and_keeps_the_earlier_run() {
    let pool = portfolio_db::open_in_memory().await.unwrap();
    ensure_asset(&pool, "VWCE", "VWCE", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    let earlier = vec![SnapshotRow {
        asset: "VWCE".into(),
        quantity: dec!(4),
        value_pln: dec!(2150.00),
        invested_pln: dec!(2025.00),
    }];
    replace_day(&pool, d("2026-10-19"), &earlier).await.unwrap();

    let r = report(&ledger(), d("2026-10-19"), &inputs(None)).unwrap();
    let err = write_snapshot(&pool, &r).await.unwrap_err();
    assert!(matches!(err, SnapshotFailure::Incomplete(_)), "{err}");
    assert_eq!(err.to_string(), "snapshot not written: no price for VWCE");
    assert_eq!(load_day(&pool, d("2026-10-19")).await.unwrap(), earlier);
}

#[tokio::test]
async fn a_day_valued_at_a_last_known_close_writes_nothing_and_keeps_the_earlier_run() {
    // Alpha Vantage failed at 22:30, so VWCE is valued at a stored close of an earlier day
    // (P30's stand-in for the pages). Stored as the day's snapshot it would stay wrong for good;
    // nothing is written, and `--date` on a later day asks for that day's own close instead.
    let pool = portfolio_db::open_in_memory().await.unwrap();
    ensure_asset(&pool, "VWCE", "VWCE", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    let earlier = vec![SnapshotRow {
        asset: "VWCE".into(),
        quantity: dec!(4),
        value_pln: dec!(2150.00),
        invested_pln: dec!(2025.00),
    }];
    replace_day(&pool, d("2026-10-19"), &earlier).await.unwrap();

    let mut stale = inputs(Some(dec!(2100)));
    stale.last_known.insert(
        "VWCE".into(),
        LastKnown {
            provider: "alphavantage",
            fetched_at: None,
            error: "rate limited".into(),
        },
    );
    let r = report(&ledger(), d("2026-10-19"), &stale).unwrap();
    let err = write_snapshot(&pool, &r).await.unwrap_err();
    assert!(matches!(err, SnapshotFailure::LastKnown { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "snapshot not written: VWCE has only a stored close of an earlier day (its source failed); \
         re-run later with --date 2026-10-19"
    );
    assert_eq!(load_day(&pool, d("2026-10-19")).await.unwrap(), earlier);
}
