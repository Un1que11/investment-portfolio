use std::str::FromStr;

use chrono::NaiveDate;
use portfolio_sources::nbp::{NbpClient, NbpError, parse_table_a};
use rust_decimal::Decimal;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EUR_OCT: &str = include_str!("fixtures/nbp/eur_2026-10-05_2026-10-14.json");

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[test]
fn parses_recorded_range() {
    let rates = parse_table_a(EUR_OCT, "EUR").unwrap();

    // 2026-10-05..14 has 8 business days; the 10th and 11th are a weekend.
    assert_eq!(rates.len(), 8);
    assert_eq!(rates[0].effective_date, d("2026-10-05"));
    assert_eq!(rates[0].mid, dec("4.3855"));
    assert_eq!(rates[0].table_no, "193/A/NBP/2026");
    assert_eq!(rates[7].effective_date, d("2026-10-14"));
    assert_eq!(rates[7].mid, dec("4.3706"));
    assert!(rates.iter().all(|r| r.currency == "EUR"));
    assert!(
        !rates
            .iter()
            .any(|r| r.effective_date == d("2026-10-10") || r.effective_date == d("2026-10-11"))
    );
}

#[test]
fn mid_is_parsed_from_text_not_float() {
    // 17 significant digits: an f64 round-trip would change the last digits.
    let body = r#"{"table":"A","currency":"euro","code":"EUR","rates":[
        {"no":"1/A/NBP/2026","effectiveDate":"2026-01-02","mid":4.2512345678901234}]}"#;

    let rates = parse_table_a(body, "eur").unwrap();

    assert_eq!(rates[0].mid, dec("4.2512345678901234"));
    assert_eq!(rates[0].mid.to_string(), "4.2512345678901234");
}

#[test]
fn trailing_zeros_keep_their_scale() {
    let body = r#"{"table":"A","currency":"x","code":"USD","rates":[
        {"no":"1/A/NBP/2026","effectiveDate":"2026-01-02","mid":3.5930}]}"#;

    assert_eq!(
        parse_table_a(body, "USD").unwrap()[0].mid.to_string(),
        "3.5930"
    );
}

#[test]
fn rejects_other_currency_table_or_bad_values() {
    let wrong_currency = EUR_OCT;
    let table_b = EUR_OCT.replace(r#""table":"A""#, r#""table":"B""#);
    let zero_mid = r#"{"table":"A","currency":"x","code":"EUR","rates":[
        {"no":"1/A/NBP/2026","effectiveDate":"2026-01-02","mid":0}]}"#;
    let bad_date = r#"{"table":"A","currency":"x","code":"EUR","rates":[
        {"no":"1/A/NBP/2026","effectiveDate":"02.01.2026","mid":4.1}]}"#;

    for (case, body, currency) in [
        ("currency", wrong_currency, "USD"),
        ("table", table_b.as_str(), "EUR"),
        ("zero", zero_mid, "EUR"),
        ("date", bad_date, "EUR"),
        ("html", "<html>maintenance</html>", "EUR"),
    ] {
        assert!(
            matches!(parse_table_a(body, currency), Err(NbpError::Parse(_))),
            "{case}"
        );
    }
}

#[tokio::test]
async fn client_requests_range_as_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/exchangerates/rates/a/eur/2026-10-05/2026-10-14/"))
        .and(query_param("format", "json"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .expect(1)
        .mount(&server)
        .await;

    let client = NbpClient::with_base_url(server.uri()).unwrap();
    let rates = client
        .table_a_range("EUR", d("2026-10-05"), d("2026-10-14"))
        .await
        .unwrap();

    assert_eq!(rates.len(), 8);
}

#[tokio::test]
async fn not_found_means_no_table_in_range() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string("404 NotFound - Not Found - Brak danych"),
        )
        .mount(&server)
        .await;

    let client = NbpClient::with_base_url(server.uri()).unwrap();
    let rates = client
        .table_a_range("EUR", d("2026-10-10"), d("2026-10-11"))
        .await
        .unwrap();

    assert!(rates.is_empty());
}

#[tokio::test]
async fn bad_request_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("400 BadRequest - Błędny zakres dat / Invalid date range"),
        )
        .mount(&server)
        .await;

    let client = NbpClient::with_base_url(server.uri()).unwrap();
    let err = client
        .table_a_range("EUR", d("2026-12-20"), d("2026-12-31"))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, NbpError::Status(400, body) if body.contains("Invalid date range")),
        "{err}"
    );
}
