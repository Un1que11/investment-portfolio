use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_sources::alphavantage::{
    AlphaVantageClient, AlphaVantageError, last_completed_friday, parse_daily, parse_weekly,
};
use portfolio_sources::prices::Clock;
use rust_decimal::Decimal;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Real responses from Alpha Vantage's public "demo" key (recorded 2026-10-15).
const IBM_DAILY: &str = include_str!("fixtures/alphavantage/daily_ibm_compact.json");
const MISSING_KEY: &str = include_str!("fixtures/alphavantage/error_missing_key.json");
const DEMO_INFO: &str = include_str!("fixtures/alphavantage/information_demo_key.json");
// Real VWCE.DEX weekly series recorded with a free key on 2026-10-15, trimmed to a few weeks.
const VWCE_WEEKLY: &str = include_str!("fixtures/alphavantage/weekly_vwce_recent.json");
const VWCE_WEEKLY_XMAS: &str = include_str!("fixtures/alphavantage/weekly_vwce_christmas.json");

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

#[test]
fn parses_daily_series_oldest_first() {
    let closes = parse_daily(IBM_DAILY, "IBM").unwrap();

    assert_eq!(closes.len(), 100);
    assert_eq!(closes.first().unwrap().date, d("2026-05-22"));
    let last = closes.last().unwrap();
    assert_eq!((last.date, last.close), (d("2026-10-14"), dec("227.8800")));
    assert!(closes.windows(2).all(|w| w[0].date < w[1].date));
    // Weekend absent: Friday 10-09 is followed by Monday 10-12.
    let fri = closes
        .iter()
        .position(|c| c.date == d("2026-10-09"))
        .unwrap();
    assert_eq!(closes[fri + 1].date, d("2026-10-12"));
}

#[test]
fn close_keeps_its_scale() {
    let closes = parse_daily(IBM_DAILY, "ibm").unwrap();
    assert_eq!(closes.last().unwrap().close.to_string(), "227.8800");
}

#[test]
fn api_errors_and_notices_are_surfaced() {
    for body in [
        MISSING_KEY,
        DEMO_INFO,
        r#"{"Note": "Thank you for using Alpha Vantage!"}"#,
    ] {
        assert!(
            matches!(parse_daily(body, "IBM"), Err(AlphaVantageError::Api(_))),
            "{body}"
        );
    }
}

#[test]
fn rejects_other_symbol_and_malformed_bodies() {
    assert!(matches!(
        parse_daily(IBM_DAILY, "VWCE.DEX"),
        Err(AlphaVantageError::Parse(msg)) if msg.contains("asked for VWCE.DEX")
    ));
    let bad_close = IBM_DAILY.replacen("\"227.8800\"", "\"n/a\"", 1);
    assert!(matches!(
        parse_daily(&bad_close, "IBM"),
        Err(AlphaVantageError::Parse(_))
    ));
    assert!(matches!(
        parse_daily("{}", "IBM"),
        Err(AlphaVantageError::Parse(_))
    ));
    assert!(matches!(
        parse_daily("<html>", "IBM"),
        Err(AlphaVantageError::Parse(_))
    ));
}

#[tokio::test]
async fn client_sends_expected_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", "TIME_SERIES_DAILY"))
        .and(query_param("symbol", "IBM"))
        .and(query_param("outputsize", "compact"))
        .and(query_param("apikey", "test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string(IBM_DAILY))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(server.uri());
    let closes = client.daily_closes("IBM").await.unwrap();

    assert_eq!(closes.len(), 100);
}

#[tokio::test]
async fn http_error_status_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let client = test_client(server.uri());

    assert!(matches!(
        client.daily_closes("IBM").await,
        Err(AlphaVantageError::Status(503))
    ));
}

#[tokio::test]
async fn api_key_never_appears_in_errors_or_debug_output() {
    // Nothing listens on this port, so the request fails at the transport level.
    let client = AlphaVantageClient::new("http://127.0.0.1:9", "super-secret-key").unwrap();

    let err = client.daily_closes("IBM").await.unwrap_err();

    assert!(matches!(err, AlphaVantageError::Http(_)));
    assert!(!err.to_string().contains("super-secret-key"), "{err}");
    assert!(!format!("{err:?}").contains("super-secret-key"));
    assert!(!format!("{client:?}").contains("super-secret-key"));
}

#[test]
fn last_completed_friday_for_each_weekday() {
    // Mon 10-12 .. Sun 10-18. Friday 10-16 counts only once the day is over.
    for (today, friday) in [
        ("2026-10-12", "2026-10-09"),
        ("2026-10-13", "2026-10-09"),
        ("2026-10-14", "2026-10-09"),
        ("2026-10-15", "2026-10-09"),
        ("2026-10-16", "2026-10-09"),
        ("2026-10-17", "2026-10-16"),
        ("2026-10-18", "2026-10-16"),
    ] {
        assert_eq!(last_completed_friday(d(today)), d(friday), "{today}");
    }
}

#[test]
fn parses_weekly_series_and_drops_unfinished_week() {
    // Thursday 10-15: the newest entry (Wednesday 10-14) is the week in progress.
    let closes = parse_weekly(VWCE_WEEKLY, "VWCE.DEX", clock("2026-10-15T09:00:00Z")).unwrap();

    let got: Vec<_> = closes.iter().map(|c| (c.date, c.close)).collect();
    assert_eq!(
        got,
        [
            (d("2026-08-28"), dec("168.5400")),
            (d("2026-09-04"), dec("167.8800")),
            (d("2026-09-11"), dec("166.8600")),
            (d("2026-09-18"), dec("167.1400")),
            (d("2026-09-25"), dec("169.7400")),
            (d("2026-10-02"), dec("171.1400")),
            (d("2026-10-09"), dec("172.8600")),
        ]
    );
    assert_eq!(closes.last().unwrap().close.to_string(), "172.8600");
}

#[test]
fn weekly_entry_is_kept_once_its_week_is_over() {
    // Friday 10-16 itself: still in progress. Saturday 10-17: the week ending 10-14 is over.
    let friday = parse_weekly(VWCE_WEEKLY, "VWCE.DEX", clock("2026-10-16T20:00:00Z")).unwrap();
    assert_eq!(friday.last().unwrap().date, d("2026-10-09"));

    let saturday = parse_weekly(VWCE_WEEKLY, "VWCE.DEX", clock("2026-10-17T09:00:00Z")).unwrap();
    assert_eq!(
        (saturday.len(), saturday.last().unwrap().date),
        (8, d("2026-10-14"))
    );
}

#[test]
fn holiday_weeks_are_dated_with_their_last_trading_day() {
    // Xetra was closed 12-24..12-26 and 12-31, so those weeks end on Tuesdays.
    let closes = parse_weekly(VWCE_WEEKLY_XMAS, "VWCE.DEX", clock("2026-10-15T09:00:00Z")).unwrap();

    let dates: Vec<_> = closes.iter().map(|c| c.date).collect();
    assert_eq!(
        dates,
        [
            d("2025-12-12"),
            d("2025-12-19"),
            d("2025-12-23"),
            d("2025-12-30"),
            d("2026-01-02"),
            d("2026-01-09"),
        ]
    );
}

#[test]
fn weekly_errors_are_surfaced_like_daily_ones() {
    let today = clock("2026-10-15T09:00:00Z");
    for body in [MISSING_KEY, DEMO_INFO] {
        assert!(
            matches!(
                parse_weekly(body, "VWCE.DEX", today),
                Err(AlphaVantageError::Api(_))
            ),
            "{body}"
        );
    }
    assert!(matches!(
        parse_weekly(VWCE_WEEKLY, "IBM", today),
        Err(AlphaVantageError::Parse(msg)) if msg.contains("asked for IBM")
    ));
    // A daily body has no weekly series.
    assert!(matches!(
        parse_weekly(IBM_DAILY, "IBM", today),
        Err(AlphaVantageError::Parse(_))
    ));
    let bad_close = VWCE_WEEKLY.replacen("\"166.8600\"", "\"-1\"", 1);
    assert!(matches!(
        parse_weekly(&bad_close, "VWCE.DEX", today),
        Err(AlphaVantageError::Parse(_))
    ));
}

#[tokio::test]
async fn weekly_client_sends_expected_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", "TIME_SERIES_WEEKLY"))
        .and(query_param("symbol", "VWCE.DEX"))
        .and(query_param("apikey", "test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string(VWCE_WEEKLY))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(server.uri());
    let closes = client
        .weekly_closes("VWCE.DEX", clock("2026-10-15T09:00:00Z"))
        .await
        .unwrap();

    assert_eq!(closes.len(), 7);
}

#[tokio::test]
async fn requests_are_spaced_out_for_the_free_tier_burst_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(VWCE_WEEKLY))
        .expect(2)
        .mount(&server)
        .await;
    let client = test_client(server.uri());
    let today = clock("2026-10-15T09:00:00Z");

    let started = std::time::Instant::now();
    client.weekly_closes("VWCE.DEX", today).await.unwrap();
    // A clone shares the spacing.
    client
        .clone()
        .weekly_closes("VWCE.DEX", today)
        .await
        .unwrap();

    assert!(started.elapsed() >= TEST_SPACING);
}
