use std::str::FromStr;

use chrono::NaiveDate;
use portfolio_core::fx::FxRate;
use portfolio_db::fx::{
    rate_for_date, rates_between, record_resolutions, resolved_rate, upsert_rates,
};
use portfolio_db::open_in_memory;
use rust_decimal::Decimal;

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn eur(date: &str, mid: &str, no: &str) -> FxRate {
    FxRate {
        currency: "EUR".into(),
        effective_date: d(date),
        mid: Decimal::from_str(mid).unwrap(),
        table_no: no.into(),
    }
}

fn fri_and_mon() -> Vec<FxRate> {
    vec![
        eur("2026-10-09", "4.3775", "197/A/NBP/2026"),
        eur("2026-10-12", "4.3889", "198/A/NBP/2026"),
    ]
}

#[tokio::test]
async fn stored_rates_read_back_exactly() {
    let pool = open_in_memory().await.unwrap();
    upsert_rates(&pool, &fri_and_mon()).await.unwrap();

    assert_eq!(
        rate_for_date(&pool, "EUR", d("2026-10-09")).await.unwrap(),
        Some(eur("2026-10-09", "4.3775", "197/A/NBP/2026"))
    );
    assert_eq!(
        rate_for_date(&pool, "EUR", d("2026-10-10")).await.unwrap(),
        None
    );
    assert_eq!(
        rate_for_date(&pool, "USD", d("2026-10-09")).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn upsert_replaces_same_day() {
    let pool = open_in_memory().await.unwrap();
    upsert_rates(&pool, &fri_and_mon()).await.unwrap();
    upsert_rates(&pool, &[eur("2026-10-09", "4.3780", "197/A/NBP/2026")])
        .await
        .unwrap();

    let all = rates_between(&pool, "EUR", d("2026-10-01"), d("2026-10-31"))
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].mid, Decimal::from_str("4.3780").unwrap());
}

#[tokio::test]
async fn rates_between_is_inclusive_and_ordered() {
    let pool = open_in_memory().await.unwrap();
    let mut rates = fri_and_mon();
    rates.reverse();
    upsert_rates(&pool, &rates).await.unwrap();

    let got = rates_between(&pool, "EUR", d("2026-10-09"), d("2026-10-12"))
        .await
        .unwrap();
    assert_eq!(
        got.iter().map(|r| r.effective_date).collect::<Vec<_>>(),
        [d("2026-10-09"), d("2026-10-12")]
    );
    assert!(
        rates_between(&pool, "EUR", d("2026-10-10"), d("2026-10-11"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn resolutions_point_weekend_days_at_friday() {
    let pool = open_in_memory().await.unwrap();
    upsert_rates(&pool, &fri_and_mon()).await.unwrap();
    record_resolutions(
        &pool,
        "EUR",
        &[
            (d("2026-10-10"), d("2026-10-09")),
            (d("2026-10-11"), d("2026-10-09")),
        ],
    )
    .await
    .unwrap();

    let sunday = resolved_rate(&pool, "EUR", d("2026-10-11"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sunday.table_no, "197/A/NBP/2026");
    assert_eq!(
        resolved_rate(&pool, "EUR", d("2026-10-13")).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn resolution_must_point_at_a_stored_earlier_rate() {
    let pool = open_in_memory().await.unwrap();
    upsert_rates(&pool, &fri_and_mon()).await.unwrap();

    // No rate stored for that day: foreign key violation.
    let missing = record_resolutions(&pool, "EUR", &[(d("2026-10-11"), d("2026-10-10"))]).await;
    assert!(missing.is_err());
    // A day cannot resolve to a later table.
    let later = record_resolutions(&pool, "EUR", &[(d("2026-10-11"), d("2026-10-12"))]).await;
    assert!(later.is_err());
}
