use std::str::FromStr;

use chrono::NaiveDate;
use portfolio_db::open_in_memory;
use portfolio_sources::fx::{FxError, rate_on_day};
use portfolio_sources::nbp::NbpClient;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EUR_OCT: &str = include_str!("fixtures/nbp/eur_2026-10-05_2026-10-14.json");
const USD_XMAS: &str = include_str!("fixtures/nbp/usd_2025-12-15_2025-12-29.json");

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

struct Setup {
    pool: SqlitePool,
    nbp: NbpClient,
    server: MockServer,
}

async fn setup() -> Setup {
    let server = MockServer::start().await;
    Setup {
        pool: open_in_memory().await.unwrap(),
        nbp: NbpClient::with_base_url(server.uri()).unwrap(),
        server,
    }
}

/// Serves `body` for the 14-day window ending on `day`, and expects exactly `times` calls.
async fn serve_window(s: &Setup, currency: &str, from: &str, day: &str, body: &str, times: u64) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/exchangerates/rates/a/{currency}/{from}/{day}/"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(&s.server)
        .await;
}

#[tokio::test]
async fn weekend_day_uses_friday_rate_and_is_then_served_from_cache() {
    let s = setup().await;
    // Sunday 2026-10-11, asked on Thursday 2026-10-15: window 2026-09-27..2026-10-11.
    // The fixture only covers from 10-05, which is fine: the answer is the last table <= day.
    serve_window(&s, "eur", "2026-09-27", "2026-10-11", EUR_OCT, 1).await;

    for _ in 0..3 {
        let rate = rate_on_day(&s.pool, &s.nbp, "EUR", d("2026-10-11"), d("2026-10-15"))
            .await
            .unwrap();
        assert_eq!(rate.effective_date, d("2026-10-09"));
        assert_eq!(rate.mid, dec("4.3775"));
        assert_eq!(rate.table_no, "197/A/NBP/2026");
    }
    // Saturday was resolved by the same fetch: no new request (the mock expects exactly 1).
    let saturday = rate_on_day(&s.pool, &s.nbp, "eur", d("2026-10-10"), d("2026-10-15"))
        .await
        .unwrap();
    assert_eq!(saturday.effective_date, d("2026-10-09"));
}

#[tokio::test]
async fn business_day_already_cached_needs_no_request() {
    let s = setup().await;
    serve_window(&s, "eur", "2026-09-30", "2026-10-14", EUR_OCT, 1).await;

    rate_on_day(&s.pool, &s.nbp, "EUR", d("2026-10-14"), d("2026-10-15"))
        .await
        .unwrap();
    // Every table from 10-05 to 10-14 is now stored; asking for any of them is offline.
    let monday = rate_on_day(&s.pool, &s.nbp, "EUR", d("2026-10-12"), d("2026-10-15"))
        .await
        .unwrap();

    assert_eq!(monday.mid, dec("4.3889"));
}

#[tokio::test]
async fn christmas_gap_resolves_to_last_table_before_it() {
    let s = setup().await;
    // 2025-12-24..28: no tables (24 Dec is a public holiday in Poland since 2025).
    serve_window(&s, "usd", "2025-12-13", "2025-12-27", USD_XMAS, 1).await;

    let rate = rate_on_day(&s.pool, &s.nbp, "USD", d("2025-12-27"), d("2026-10-15"))
        .await
        .unwrap();

    assert_eq!(rate.effective_date, d("2025-12-23"));
    assert_eq!(rate.mid, dec("3.5848"));
    // 24, 25 and 26 Dec were resolved by the same fetch.
    for day in ["2025-12-24", "2025-12-25", "2025-12-26"] {
        let r = rate_on_day(&s.pool, &s.nbp, "USD", d(day), d("2026-10-15"))
            .await
            .unwrap();
        assert_eq!(r.table_no, "248/A/NBP/2025", "{day}");
    }
}

#[tokio::test]
async fn today_before_publication_falls_back_but_is_not_cached() {
    let s = setup().await;
    // Thursday 2026-10-15 morning: the fixture ends on Wednesday 10-14.
    serve_window(&s, "eur", "2026-10-01", "2026-10-15", EUR_OCT, 2).await;

    for _ in 0..2 {
        let rate = rate_on_day(&s.pool, &s.nbp, "EUR", d("2026-10-15"), d("2026-10-15"))
            .await
            .unwrap();
        assert_eq!(rate.effective_date, d("2026-10-14"));
    }
    // Two requests (asserted by the mock on drop): today's answer was never cached.
}

#[tokio::test]
async fn todays_table_once_published_is_cached() {
    let s = setup().await;
    let with_today = EUR_OCT.replace(
        "]}",
        r#",{"no":"201/A/NBP/2026","effectiveDate":"2026-10-15","mid":4.3691}]}"#,
    );
    serve_window(&s, "eur", "2026-10-01", "2026-10-15", &with_today, 1).await;

    for _ in 0..2 {
        let rate = rate_on_day(&s.pool, &s.nbp, "EUR", d("2026-10-15"), d("2026-10-15"))
            .await
            .unwrap();
        assert_eq!(rate.mid, dec("4.3691"));
    }
}

#[tokio::test]
async fn no_table_in_window_is_an_error() {
    let s = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_string("404 NotFound - Brak danych"))
        .mount(&s.server)
        .await;

    let err = rate_on_day(&s.pool, &s.nbp, "XAU", d("2026-10-11"), d("2026-10-15"))
        .await
        .unwrap_err();

    assert!(matches!(err, FxError::NoRate(ref c, day) if c == "XAU" && day == d("2026-10-11")));
}

#[tokio::test]
async fn future_day_and_pln_are_rejected_without_requests() {
    let s = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&s.server)
        .await;

    let future = rate_on_day(&s.pool, &s.nbp, "EUR", d("2026-10-16"), d("2026-10-15")).await;
    let pln = rate_on_day(&s.pool, &s.nbp, "pln", d("2026-10-14"), d("2026-10-15")).await;

    assert!(matches!(future, Err(FxError::FutureDate(day)) if day == d("2026-10-16")));
    assert!(matches!(pln, Err(FxError::BaseCurrency)));
}

#[tokio::test]
async fn nbp_failure_is_reported() {
    let s = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&s.server)
        .await;

    let err = rate_on_day(&s.pool, &s.nbp, "EUR", d("2026-10-14"), d("2026-10-15"))
        .await
        .unwrap_err();

    assert!(matches!(err, FxError::Nbp(_)), "{err}");
}
