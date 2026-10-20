use std::str::FromStr;

use chrono::NaiveDate;
use portfolio_core::AssetClass;
use portfolio_core::snapshot::SnapshotRow;
use portfolio_db::open_in_memory;
use portfolio_db::prices::ensure_asset;
use portfolio_db::snapshots::{
    SnapshotWriteError, first_and_last_day, latest_day, latest_day_before, load_day, replace_day,
};
use rust_decimal::Decimal;
use sqlx::SqlitePool;

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn row(asset: &str, quantity: &str, value: &str, invested: &str) -> SnapshotRow {
    SnapshotRow {
        asset: asset.into(),
        quantity: Decimal::from_str(quantity).unwrap(),
        value_pln: Decimal::from_str(value).unwrap(),
        invested_pln: Decimal::from_str(invested).unwrap(),
    }
}

async fn db() -> SqlitePool {
    let pool = open_in_memory().await.unwrap();
    ensure_asset(&pool, "VWCE", "VWCE", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    ensure_asset(&pool, "BTC", "Bitcoin", AssetClass::Crypto, "EUR")
        .await
        .unwrap();
    pool
}

#[tokio::test]
async fn a_day_round_trips_exactly() {
    let pool = db().await;
    let rows = vec![
        row("BTC", "0.01234567", "3801.26", "2999.50"),
        row("VWCE", "3.5", "1932.43", "-120.01"),
    ];
    assert_eq!(replace_day(&pool, d("2026-10-19"), &rows).await.unwrap(), 2);
    let back = load_day(&pool, d("2026-10-19")).await.unwrap();
    assert_eq!(back, rows);
    // Scale is kept: "2999.50" stays "2999.50".
    assert_eq!(back[0].invested_pln.to_string(), "2999.50");
    assert!(load_day(&pool, d("2026-10-18")).await.unwrap().is_empty());
}

#[tokio::test]
async fn rerunning_a_day_replaces_it_and_drops_assets_no_longer_held() {
    let pool = db().await;
    let day = d("2026-10-19");
    replace_day(
        &pool,
        day,
        &[
            row("BTC", "0.01", "3000.00", "2500.00"),
            row("VWCE", "3", "1600.00", "1500.00"),
        ],
    )
    .await
    .unwrap();
    replace_day(&pool, day, &[row("VWCE", "4", "2150.00", "2000.00")])
        .await
        .unwrap();
    assert_eq!(
        load_day(&pool, day).await.unwrap(),
        vec![row("VWCE", "4", "2150.00", "2000.00")]
    );
}

#[tokio::test]
async fn other_days_are_left_alone() {
    let pool = db().await;
    replace_day(
        &pool,
        d("2026-10-18"),
        &[row("VWCE", "3", "1590.00", "1500.00")],
    )
    .await
    .unwrap();
    replace_day(
        &pool,
        d("2026-10-19"),
        &[row("VWCE", "3", "1600.00", "1500.00")],
    )
    .await
    .unwrap();
    assert_eq!(
        load_day(&pool, d("2026-10-18")).await.unwrap(),
        vec![row("VWCE", "3", "1590.00", "1500.00")]
    );
    assert_eq!(latest_day(&pool).await.unwrap(), Some(d("2026-10-19")));
}

#[tokio::test]
async fn an_unknown_asset_fails_and_keeps_the_stored_day() {
    let pool = db().await;
    let day = d("2026-10-19");
    let kept = vec![row("VWCE", "3", "1600.00", "1500.00")];
    replace_day(&pool, day, &kept).await.unwrap();
    let err = replace_day(
        &pool,
        day,
        &[
            row("VWCE", "4", "2150.00", "2000.00"),
            row("DOGE", "1", "1.00", "1.00"),
        ],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SnapshotWriteError::UnknownAsset(s) if s == "DOGE"));
    assert_eq!(load_day(&pool, day).await.unwrap(), kept);
}

#[tokio::test]
async fn no_snapshot_yet() {
    let pool = db().await;
    assert_eq!(latest_day(&pool).await.unwrap(), None);
}

#[tokio::test]
async fn days_around_a_month() {
    // Snapshots on 31 Aug, 12 Sep and 30 Sep; September is looked up as 1-30 Sep.
    let pool = db().await;
    for day in ["2026-08-31", "2026-09-12", "2026-09-30"] {
        replace_day(&pool, d(day), &[row("VWCE", "1", "500.00", "480.00")])
            .await
            .unwrap();
    }
    assert_eq!(
        first_and_last_day(&pool, d("2026-09-01"), d("2026-09-30"))
            .await
            .unwrap(),
        Some((d("2026-09-12"), d("2026-09-30")))
    );
    assert_eq!(
        latest_day_before(&pool, d("2026-09-01")).await.unwrap(),
        Some(d("2026-08-31"))
    );
    // Before the first snapshot and in a month without one: nothing.
    assert_eq!(
        latest_day_before(&pool, d("2026-08-31")).await.unwrap(),
        None
    );
    assert_eq!(
        first_and_last_day(&pool, d("2026-10-01"), d("2026-10-31"))
            .await
            .unwrap(),
        None
    );
}
