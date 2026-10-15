use std::str::FromStr;

use chrono::NaiveDate;
use portfolio_sources::alphavantage::{AlphaVantageClient, AlphaVantageError, parse_daily};
use rust_decimal::Decimal;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Real responses from Alpha Vantage's public "demo" key (recorded 2026-10-15).
const IBM_DAILY: &str = include_str!("fixtures/alphavantage/daily_ibm_compact.json");
const MISSING_KEY: &str = include_str!("fixtures/alphavantage/error_missing_key.json");
const DEMO_INFO: &str = include_str!("fixtures/alphavantage/information_demo_key.json");

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
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

    let client = AlphaVantageClient::new(server.uri(), "test-key").unwrap();
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

    let client = AlphaVantageClient::new(server.uri(), "test-key").unwrap();

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
