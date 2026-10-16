use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_core::AssetClass;
use portfolio_core::price::{DailyClose, PriceQuote};
use portfolio_db::open_in_memory;
use portfolio_db::prices::{
    closes_between, ensure_asset, has_close_after, insert_missing_closes, last_fetched_at,
    latest_fetch, quote_for_date, upsert_closes,
};
use rust_decimal::Decimal;

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn close(date: &str, price: &str) -> DailyClose {
    DailyClose {
        date: d(date),
        close: Decimal::from_str(price).unwrap(),
    }
}

#[tokio::test]
async fn ensure_asset_is_idempotent() {
    let pool = open_in_memory().await.unwrap();
    let first = ensure_asset(&pool, "VWCE", "Vanguard", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    let again = ensure_asset(&pool, "VWCE", "Other name", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    let btc = ensure_asset(&pool, "BTC", "Bitcoin", AssetClass::Crypto, "PLN")
        .await
        .unwrap();

    assert_eq!(first, again);
    assert_ne!(first, btc);
}

#[tokio::test]
async fn closes_round_trip_and_upsert_replaces() {
    let pool = open_in_memory().await.unwrap();
    let id = ensure_asset(&pool, "VWCE", "Vanguard", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    upsert_closes(
        &pool,
        id,
        "EUR",
        "alphavantage",
        &[
            close("2026-10-09", "170.1200"),
            close("2026-10-12", "171.00"),
        ],
        at("2026-10-12T18:00:00Z"),
    )
    .await
    .unwrap();
    upsert_closes(
        &pool,
        id,
        "EUR",
        "alphavantage",
        &[close("2026-10-12", "171.05")],
        at("2026-10-13T18:00:00Z"),
    )
    .await
    .unwrap();

    let all = closes_between(&pool, id, d("2026-10-01"), d("2026-10-31"))
        .await
        .unwrap();
    assert_eq!(
        all,
        [
            (close("2026-10-09", "170.1200"), "EUR".to_owned()),
            (close("2026-10-12", "171.05"), "EUR".to_owned()),
        ]
    );
    assert_eq!(all[0].0.close.to_string(), "170.1200");
}

#[tokio::test]
async fn quote_for_exact_date_only() {
    let pool = open_in_memory().await.unwrap();
    let id = ensure_asset(&pool, "VWCE", "Vanguard", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    upsert_closes(
        &pool,
        id,
        "EUR",
        "alphavantage",
        &[close("2026-10-09", "170.12")],
        at("2026-10-09T18:00:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(
        quote_for_date(&pool, id, d("2026-10-09")).await.unwrap(),
        Some(PriceQuote {
            date: d("2026-10-09"),
            close: Decimal::from_str("170.12").unwrap(),
            currency: "EUR".into()
        })
    );
    assert_eq!(
        quote_for_date(&pool, id, d("2026-10-10")).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn has_close_after_and_last_fetched() {
    let pool = open_in_memory().await.unwrap();
    let id = ensure_asset(&pool, "VWCE", "Vanguard", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    assert_eq!(
        last_fetched_at(&pool, id, "alphavantage").await.unwrap(),
        None
    );

    upsert_closes(
        &pool,
        id,
        "EUR",
        "alphavantage",
        &[close("2026-10-12", "171.00")],
        at("2026-10-12T18:00:00Z"),
    )
    .await
    .unwrap();

    assert!(has_close_after(&pool, id, d("2026-10-11")).await.unwrap());
    assert!(!has_close_after(&pool, id, d("2026-10-12")).await.unwrap());
    assert_eq!(
        last_fetched_at(&pool, id, "alphavantage").await.unwrap(),
        Some(at("2026-10-12T18:00:00Z"))
    );
}

#[tokio::test]
async fn latest_fetch_is_the_newest_write_of_any_asset_and_source() {
    let pool = open_in_memory().await.unwrap();
    assert_eq!(latest_fetch(&pool).await.unwrap(), None);
    let vwce = ensure_asset(&pool, "VWCE", "Vanguard", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    let btc = ensure_asset(&pool, "BTC", "Bitcoin", AssetClass::Crypto, "EUR")
        .await
        .unwrap();
    upsert_closes(
        &pool,
        vwce,
        "EUR",
        "alphavantage",
        &[close("2026-10-12", "171.00")],
        at("2026-10-12T18:00:00Z"),
    )
    .await
    .unwrap();
    upsert_closes(
        &pool,
        btc,
        "EUR",
        "kraken",
        &[close("2026-10-13", "95000")],
        at("2026-10-14T06:00:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(
        latest_fetch(&pool).await.unwrap(),
        Some(at("2026-10-14T06:00:00Z"))
    );
}

async fn sources(pool: &sqlx::SqlitePool, id: i64) -> Vec<(String, String, String)> {
    sqlx::query_as("SELECT price_date, price, source FROM prices WHERE asset_id = ? ORDER BY 1")
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn insert_missing_closes_never_replaces_a_stored_close() {
    let pool = open_in_memory().await.unwrap();
    let id = ensure_asset(&pool, "VWCE", "Vanguard", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    upsert_closes(
        &pool,
        id,
        "EUR",
        "alphavantage",
        &[close("2026-10-09", "172.8600")],
        at("2026-10-12T18:00:00Z"),
    )
    .await
    .unwrap();

    let inserted = insert_missing_closes(
        &pool,
        id,
        "EUR",
        "alphavantage_weekly",
        &[
            close("2026-10-02", "171.1400"),
            close("2026-10-09", "999.00"),
        ],
        at("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(inserted, 1);
    let own = |a: &str, b: &str, c: &str| (a.to_owned(), b.to_owned(), c.to_owned());
    assert_eq!(
        sources(&pool, id).await,
        [
            own("2026-10-02", "171.1400", "alphavantage_weekly"),
            own("2026-10-09", "172.8600", "alphavantage"),
        ]
    );
}

#[tokio::test]
async fn last_fetched_at_is_per_source() {
    let pool = open_in_memory().await.unwrap();
    let id = ensure_asset(&pool, "VWCE", "Vanguard", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    upsert_closes(
        &pool,
        id,
        "EUR",
        "alphavantage",
        &[close("2026-10-09", "172.8600")],
        at("2026-10-12T18:00:00Z"),
    )
    .await
    .unwrap();
    insert_missing_closes(
        &pool,
        id,
        "EUR",
        "alphavantage_weekly",
        &[close("2026-10-02", "171.1400")],
        at("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(
        last_fetched_at(&pool, id, "alphavantage").await.unwrap(),
        Some(at("2026-10-12T18:00:00Z"))
    );
    assert_eq!(
        last_fetched_at(&pool, id, "alphavantage_weekly")
            .await
            .unwrap(),
        Some(at("2026-10-15T09:00:00Z"))
    );
}
