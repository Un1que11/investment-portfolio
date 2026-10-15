use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_core::AssetClass;
use portfolio_db::open_in_memory;
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::{Clock, Instrument, PriceError, VWCE, close_on_day, value_in_pln};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const IBM_DAILY: &str = include_str!("fixtures/alphavantage/daily_ibm_compact.json");
const DEMO_INFO: &str = include_str!("fixtures/alphavantage/information_demo_key.json");
const USD_OCT: &str = include_str!("fixtures/nbp/usd_2026-09-25_2026-10-09.json");

/// The recorded real series is IBM (the only symbol the demo key serves), quoted in USD.
const IBM: Instrument = Instrument {
    symbol: "IBM",
    name: "IBM",
    class: AssetClass::Vwce,
    currency: "USD",
    provider: "alphavantage",
    provider_symbol: "IBM",
};

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn clock(now: &str) -> Clock {
    let now: DateTime<Utc> = now.parse().unwrap();
    Clock {
        now,
        today: now.date_naive(),
    }
}

struct Setup {
    pool: SqlitePool,
    av: AlphaVantageClient,
    nbp: NbpClient,
    server: MockServer,
}

async fn setup() -> Setup {
    let server = MockServer::start().await;
    Setup {
        pool: open_in_memory().await.unwrap(),
        av: AlphaVantageClient::new(server.uri(), "test-key").unwrap(),
        nbp: NbpClient::with_base_url(server.uri()).unwrap(),
        server,
    }
}

async fn serve_av(s: &Setup, symbol: &str, body: &str, times: u64) {
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("symbol", symbol))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(&s.server)
        .await;
}

#[tokio::test]
async fn weekend_uses_friday_close_then_serves_from_cache() {
    let s = setup().await;
    serve_av(&s, "IBM", IBM_DAILY, 1).await;
    let clock = clock("2026-10-15T09:00:00Z");

    for day in ["2026-10-11", "2026-10-10", "2026-10-11"] {
        let quote = close_on_day(&s.pool, &s.av, &IBM, d(day), clock)
            .await
            .unwrap();
        assert_eq!(
            (quote.date, quote.close),
            (d("2026-10-09"), dec("227.1300")),
            "{day}"
        );
        assert_eq!(quote.currency, "USD");
    }
    // One request in total (asserted by the mock): later lookups hit the cache.
}

#[tokio::test]
async fn trading_day_in_cached_series_needs_no_request() {
    let s = setup().await;
    serve_av(&s, "IBM", IBM_DAILY, 1).await;
    let clock = clock("2026-10-15T09:00:00Z");

    close_on_day(&s.pool, &s.av, &IBM, d("2026-10-14"), clock)
        .await
        .unwrap();
    let older = close_on_day(&s.pool, &s.av, &IBM, d("2026-06-01"), clock)
        .await
        .unwrap();

    assert_eq!(older.date, d("2026-06-01"));
}

#[tokio::test]
async fn today_before_close_falls_back_and_refetches_only_after_interval() {
    let s = setup().await;
    serve_av(&s, "IBM", IBM_DAILY, 2).await;

    // Thursday 10-15: the series ends on Wednesday 10-14.
    let morning = close_on_day(
        &s.pool,
        &s.av,
        &IBM,
        d("2026-10-15"),
        clock("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap();
    assert_eq!(morning.date, d("2026-10-14"));

    // Within the refetch interval: answered from cache, no request.
    let soon = close_on_day(
        &s.pool,
        &s.av,
        &IBM,
        d("2026-10-15"),
        clock("2026-10-15T10:00:00Z"),
    )
    .await
    .unwrap();
    assert_eq!(soon.date, d("2026-10-14"));

    // Seven hours after the first fetch: refetched.
    close_on_day(
        &s.pool,
        &s.av,
        &IBM,
        d("2026-10-15"),
        clock("2026-10-15T16:00:00Z"),
    )
    .await
    .unwrap();
    // Two requests in total (asserted by the mock).
}

#[tokio::test]
async fn day_outside_provider_window_is_no_price() {
    let s = setup().await;
    serve_av(&s, "IBM", IBM_DAILY, 1).await;

    // Compact output starts on 2026-05-22.
    let err = close_on_day(
        &s.pool,
        &s.av,
        &IBM,
        d("2026-04-30"),
        clock("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap_err();

    assert!(matches!(err, PriceError::NoPrice("IBM", day) if day == d("2026-04-30")));
}

#[tokio::test]
async fn provider_notice_is_reported() {
    let s = setup().await;
    serve_av(&s, "VWCE.DEX", DEMO_INFO, 1).await;

    let err = close_on_day(
        &s.pool,
        &s.av,
        &VWCE,
        d("2026-10-14"),
        clock("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap_err();

    assert!(matches!(err, PriceError::Provider(_)), "{err}");
}

#[tokio::test]
async fn future_day_is_rejected_without_request() {
    let s = setup().await;
    serve_av(&s, "IBM", IBM_DAILY, 0).await;

    let err = close_on_day(
        &s.pool,
        &s.av,
        &IBM,
        d("2026-10-16"),
        clock("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap_err();

    assert!(matches!(err, PriceError::FutureDate(_)));
}

#[tokio::test]
async fn value_in_pln_uses_rate_of_the_close_day() {
    let s = setup().await;
    serve_av(&s, "IBM", IBM_DAILY, 1).await;
    // Sunday 10-11 -> Friday 10-09 close, so the NBP rate is fetched for Friday's window.
    Mock::given(method("GET"))
        .and(path("/exchangerates/rates/a/usd/2026-09-25/2026-10-09/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(USD_OCT))
        .expect(1)
        .mount(&s.server)
        .await;

    let v = value_in_pln(
        &s.pool,
        &s.av,
        &s.nbp,
        &IBM,
        d("2026-10-11"),
        clock("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(v.quote.date, d("2026-10-09"));
    let fx = v.fx.unwrap();
    assert_eq!(
        (fx.effective_date, fx.mid),
        (d("2026-10-09"), dec("3.9022"))
    );
    assert_eq!(fx.table_no, "197/A/NBP/2026");
    // 227.13 USD * 3.9022 PLN/USD = 885.7994 + 0.507286 = 886.306686 PLN (by hand).
    assert_eq!(v.close_pln, dec("886.306686"));
}

#[tokio::test]
async fn pln_quoted_instrument_needs_no_rate() {
    let s = setup().await;
    serve_av(&s, "IBM", IBM_DAILY, 1).await;
    let pln_ibm = Instrument {
        currency: "PLN",
        ..IBM
    };

    let v = value_in_pln(
        &s.pool,
        &s.av,
        &s.nbp,
        &pln_ibm,
        d("2026-10-14"),
        clock("2026-10-15T09:00:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(v.fx, None);
    assert_eq!(v.close_pln, dec("227.8800"));
}

#[test]
fn vwce_is_the_xetra_listing_in_eur() {
    assert_eq!(
        (VWCE.symbol, VWCE.currency, VWCE.provider_symbol),
        ("VWCE", "EUR", "VWCE.DEX")
    );
}
