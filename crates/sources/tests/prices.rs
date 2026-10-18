use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_core::AssetClass;
use portfolio_db::open_in_memory;
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::{
    ALPHAVANTAGE_WEEKLY_SOURCE, BTC, Clock, Instrument, PriceError, VWCE, WeeklyBackfill,
    backfill_weekly, close_on_day, value_in_pln,
};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const IBM_DAILY: &str = include_str!("fixtures/alphavantage/daily_ibm_compact.json");
const DEMO_INFO: &str = include_str!("fixtures/alphavantage/information_demo_key.json");
// Real VWCE.DEX responses recorded with a free key on 2026-10-15, trimmed: daily 09-28..10-14,
// weekly 08-28..10-14 (the last entry is the unfinished week) and the Christmas 2025 weeks.
const VWCE_DAILY: &str = include_str!("fixtures/alphavantage/daily_vwce_tail.json");
const VWCE_WEEKLY: &str = include_str!("fixtures/alphavantage/weekly_vwce_recent.json");
const VWCE_WEEKLY_XMAS: &str = include_str!("fixtures/alphavantage/weekly_vwce_christmas.json");
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

/// Requests to the local mock are spaced by a short, measurable pause.
const TEST_SPACING: std::time::Duration = std::time::Duration::from_millis(200);

fn test_client(base_url: String) -> AlphaVantageClient {
    AlphaVantageClient::new(base_url, "test-key")
        .unwrap()
        .with_request_spacing(TEST_SPACING)
}

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
        av: test_client(server.uri()),
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

async fn serve_vwce(s: &Setup, function: &str, body: &str, times: u64) {
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", function))
        .and(query_param("symbol", "VWCE.DEX"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(&s.server)
        .await;
}

async fn stored(pool: &SqlitePool, from: &str, to: &str) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT price_date, price, source FROM prices
         WHERE price_date BETWEEN ? AND ? ORDER BY price_date",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn row(date: &str, price: &str, source: &str) -> (String, String, String) {
    (date.to_owned(), price.to_owned(), source.to_owned())
}

#[tokio::test]
async fn backfill_stores_weekly_closes_only_where_no_daily_close_exists() {
    let s = setup().await;
    serve_vwce(&s, "TIME_SERIES_DAILY", VWCE_DAILY, 1).await;
    serve_vwce(&s, "TIME_SERIES_WEEKLY", VWCE_WEEKLY, 1).await;

    let done = backfill_weekly(&s.pool, &s.av, &VWCE, clock("2026-10-15T09:00:00Z"))
        .await
        .unwrap();

    // 7 completed weeks (08-28..10-09); 10-02 and 10-09 already have daily closes.
    assert_eq!(
        done,
        WeeklyBackfill {
            weeks: 7,
            inserted: 5
        }
    );
    let weekly = ALPHAVANTAGE_WEEKLY_SOURCE;
    assert_eq!(
        stored(&s.pool, "2026-08-01", "2026-09-29").await,
        [
            row("2026-08-28", "168.5400", weekly),
            row("2026-09-04", "167.8800", weekly),
            row("2026-09-11", "166.8600", weekly),
            row("2026-09-18", "167.1400", weekly),
            row("2026-09-25", "169.7400", weekly),
            row("2026-09-28", "169.1200", "alphavantage"),
            row("2026-09-29", "169.4000", "alphavantage"),
        ]
    );
    assert_eq!(
        stored(&s.pool, "2026-10-09", "2026-10-09").await,
        [row("2026-10-09", "172.8600", "alphavantage")]
    );
    // The unfinished week (Wednesday 10-14) keeps its daily close only.
    assert_eq!(
        stored(&s.pool, "2026-10-14", "2026-10-14").await,
        [row("2026-10-14", "172.4800", "alphavantage")]
    );
}

#[tokio::test]
async fn days_in_backfilled_weeks_use_the_previous_week_end_close() {
    let s = setup().await;
    serve_vwce(&s, "TIME_SERIES_DAILY", VWCE_DAILY, 1).await;
    serve_vwce(&s, "TIME_SERIES_WEEKLY", VWCE_WEEKLY, 1).await;
    let clock = clock("2026-10-15T09:00:00Z");
    backfill_weekly(&s.pool, &s.av, &VWCE, clock).await.unwrap();

    for (day, close_date, close) in [
        // A week-end itself: that day's close.
        ("2026-09-11", "2026-09-11", "166.8600"),
        // Wednesday and Thursday inside the next week: the last known close, 5 and 6 days back.
        ("2026-09-16", "2026-09-11", "166.8600"),
        ("2026-09-17", "2026-09-11", "166.8600"),
        // Sunday before the daily window: Friday's week-end close.
        ("2026-09-27", "2026-09-25", "169.7400"),
        // Inside the daily window: the daily close as before.
        ("2026-09-30", "2026-09-30", "169.7800"),
    ] {
        let quote = close_on_day(&s.pool, &s.av, &VWCE, d(day), clock)
            .await
            .unwrap();
        assert_eq!(
            (quote.date, quote.close, quote.currency.as_str()),
            (d(close_date), dec(close), "EUR"),
            "{day}"
        );
    }
    // Before the first week-end there is nothing to fall back to. The daily series was just
    // fetched, so this is not refetched (one request each, asserted by the mocks).
    let err = close_on_day(&s.pool, &s.av, &VWCE, d("2026-08-27"), clock)
        .await
        .unwrap_err();
    assert!(matches!(err, PriceError::NoPrice("VWCE", _)), "{err}");
}

#[tokio::test]
async fn holiday_week_ends_stay_within_the_gap_limit() {
    let s = setup().await;
    serve_vwce(&s, "TIME_SERIES_DAILY", VWCE_DAILY, 1).await;
    serve_vwce(&s, "TIME_SERIES_WEEKLY", VWCE_WEEKLY_XMAS, 1).await;
    let clock = clock("2026-10-15T09:00:00Z");
    backfill_weekly(&s.pool, &s.av, &VWCE, clock).await.unwrap();

    for (day, close_date, close) in [
        // Weeks ending Tuesday 12-23 and 12-30 (Xetra closed 12-24..12-26 and 12-31).
        ("2025-12-24", "2025-12-23", "144.7000"),
        ("2025-12-29", "2025-12-23", "144.7000"),
        ("2026-01-01", "2025-12-30", "145.4200"),
        ("2026-01-08", "2026-01-02", "145.1400"),
    ] {
        let quote = close_on_day(&s.pool, &s.av, &VWCE, d(day), clock)
            .await
            .unwrap();
        assert_eq!(
            (quote.date, quote.close),
            (d(close_date), dec(close)),
            "{day}"
        );
    }
}

#[tokio::test]
async fn repeated_backfill_inserts_nothing_and_does_not_refetch_daily() {
    let s = setup().await;
    serve_vwce(&s, "TIME_SERIES_DAILY", VWCE_DAILY, 1).await;
    serve_vwce(&s, "TIME_SERIES_WEEKLY", VWCE_WEEKLY, 2).await;

    backfill_weekly(&s.pool, &s.av, &VWCE, clock("2026-10-15T09:00:00Z"))
        .await
        .unwrap();
    let again = backfill_weekly(&s.pool, &s.av, &VWCE, clock("2026-10-15T10:00:00Z"))
        .await
        .unwrap();

    assert_eq!(
        again,
        WeeklyBackfill {
            weeks: 7,
            inserted: 0
        }
    );
}

#[tokio::test]
async fn weekly_rows_do_not_hold_back_the_daily_fetch() {
    let s = setup().await;
    serve_vwce(&s, "TIME_SERIES_DAILY", VWCE_DAILY, 1).await;
    let id = portfolio_db::prices::ensure_asset(&s.pool, "VWCE", VWCE.name, VWCE.class, "EUR")
        .await
        .unwrap();
    // Weekly closes written just now, but no daily series yet.
    portfolio_db::prices::insert_missing_closes(
        &s.pool,
        id,
        "EUR",
        ALPHAVANTAGE_WEEKLY_SOURCE,
        &[portfolio_core::price::DailyClose {
            date: d("2026-09-25"),
            close: dec("169.7400"),
        }],
        "2026-10-15T09:00:00Z".parse().unwrap(),
    )
    .await
    .unwrap();

    let quote = close_on_day(
        &s.pool,
        &s.av,
        &VWCE,
        d("2026-10-14"),
        clock("2026-10-15T09:30:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(
        (quote.date, quote.close),
        (d("2026-10-14"), dec("172.4800"))
    );
}

#[tokio::test]
async fn backfill_rejects_instruments_of_other_providers() {
    let s = setup().await;

    let err = backfill_weekly(&s.pool, &s.av, &BTC, clock("2026-10-15T09:00:00Z"))
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        PriceError::WrongProvider {
            instrument: "BTC",
            expected: "kraken",
            got: "alphavantage"
        }
    ));
}

#[tokio::test]
async fn weekly_notice_is_reported() {
    let s = setup().await;
    serve_vwce(&s, "TIME_SERIES_DAILY", VWCE_DAILY, 1).await;
    serve_vwce(&s, "TIME_SERIES_WEEKLY", DEMO_INFO, 1).await;

    let err = backfill_weekly(&s.pool, &s.av, &VWCE, clock("2026-10-15T09:00:00Z"))
        .await
        .unwrap_err();

    assert!(matches!(err, PriceError::Provider(_)), "{err}");
}

/// Stores one close per `(day, close)`, each written by its own fetch at `fetched_at`.
async fn store_closes(pool: &SqlitePool, closes: &[(&str, &str, &str)]) -> i64 {
    use portfolio_core::price::DailyClose;
    use portfolio_db::prices::{ensure_asset, upsert_closes};
    let id = ensure_asset(pool, VWCE.symbol, VWCE.name, VWCE.class, VWCE.currency)
        .await
        .unwrap();
    for (day, close, fetched_at) in closes {
        let close = DailyClose {
            date: d(day),
            close: dec(close),
        };
        let at: DateTime<Utc> = fetched_at.parse().unwrap();
        upsert_closes(pool, id, VWCE.currency, VWCE.provider, &[close], at)
            .await
            .unwrap();
    }
    id
}

#[tokio::test]
async fn an_earlier_close_is_not_used_across_a_gap_between_two_fetches() {
    // A close for Fri 9 Jan from a fetch in January, and the next one, 1 Jun, from a fetch in
    // June: nothing was fetched in between. 16 Jan is not a holiday after 9 Jan (the next close
    // is 143 days later, past MAX_GAP_DAYS), so the 9 Jan close must not answer it. The daily
    // series was fetched two hours ago, so no request is made either.
    let s = setup().await;
    store_closes(
        &s.pool,
        &[
            ("2026-01-09", "100", "2026-01-10T10:00:00Z"),
            ("2026-06-01", "150", "2026-06-01T10:00:00Z"),
        ],
    )
    .await;
    let clock = clock("2026-06-01T12:00:00Z");

    let result = close_on_day(&s.pool, &s.av, &VWCE, d("2026-01-16"), clock).await;

    assert!(
        matches!(result, Err(PriceError::NoPrice("VWCE", day)) if day == d("2026-01-16")),
        "{result:?}"
    );
    assert!(s.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_earlier_close_still_answers_a_holiday_inside_one_series() {
    // Fri 9 Jan and Mon 12 Jan are stored: Sat 10 Jan takes Friday's close from the cache.
    let s = setup().await;
    store_closes(
        &s.pool,
        &[
            ("2026-01-09", "100", "2026-01-13T10:00:00Z"),
            ("2026-01-12", "101", "2026-01-13T10:00:00Z"),
        ],
    )
    .await;
    let clock = clock("2026-06-01T12:00:00Z");

    let quote = close_on_day(&s.pool, &s.av, &VWCE, d("2026-01-10"), clock)
        .await
        .unwrap();

    assert_eq!((quote.date, quote.close), (d("2026-01-09"), dec("100")));
    assert!(s.server.received_requests().await.unwrap().is_empty());
}
