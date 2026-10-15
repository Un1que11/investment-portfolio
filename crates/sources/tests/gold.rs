use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_core::gold::{GoldError, TENTH_OUNCE_GRAMS, ValuationBasis};
use portfolio_db::open_in_memory;
use portfolio_sources::DailySource;
use portfolio_sources::gold::{GOLD_GRAM, GoldValuationError, coin_value_on_day};
use portfolio_sources::nbp::{NbpClient, NbpError, parse_gold};
use portfolio_sources::prices::{Clock, PriceError};
use rust_decimal::Decimal;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Real NBP responses, recorded 2026-10-15 (after the day's publication).
const LAST_90: &str = include_str!("fixtures/nbp/gold_last_90.json");
const XMAS_2025: &str = include_str!("fixtures/nbp/gold_2025-12-15_2025-12-31.json");

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

async fn serve_last_90(body: &str, times: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cenyzlota/last/90/"))
        .and(query_param("format", "json"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(&server)
        .await;
    server
}

#[test]
fn parses_recorded_gold_prices() {
    let prices = parse_gold(LAST_90).unwrap();

    assert_eq!(prices.len(), 90);
    let last = prices.last().unwrap();
    assert_eq!((last.date, last.close), (d("2026-10-15"), dec("520.00")));
    // Scale as published: "520.00", not "520".
    assert_eq!(last.close.to_string(), "520.00");
    let friday = prices.iter().find(|p| p.date == d("2026-10-09")).unwrap();
    assert_eq!(friday.close, dec("519.09"));
    // No weekend quotes.
    assert!(
        !prices
            .iter()
            .any(|p| p.date == d("2026-10-10") || p.date == d("2026-10-11"))
    );
    assert!(prices.windows(2).all(|w| w[0].date < w[1].date));
}

#[test]
fn rejects_malformed_gold_responses() {
    for body in [
        r#"[{"data":"2026-10-14","cena":0}]"#,
        r#"[{"data":"14.10.2026","cena":527.24}]"#,
        r#"{"table":"A"}"#,
        "<html>",
    ] {
        assert!(
            matches!(parse_gold(body), Err(NbpError::Parse(_))),
            "{body}"
        );
    }
}

#[tokio::test]
async fn gold_last_requests_recent_quotes() {
    let server = serve_last_90(LAST_90, 1).await;
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    assert_eq!(nbp.gold_last(90).await.unwrap().len(), 90);
}

#[tokio::test]
async fn gold_source_rejects_other_symbols_without_request() {
    let server = serve_last_90(LAST_90, 0).await;
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    let err = nbp.daily_closes("XAG").await.unwrap_err();

    assert!(matches!(err, NbpError::Parse(msg) if msg.contains("XAU")));
}

#[tokio::test]
async fn coin_on_sunday_uses_friday_price_then_cache() {
    let server = serve_last_90(LAST_90, 1).await;
    let pool = open_in_memory().await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let clock = clock("2026-10-15T14:00:00Z");

    let v = coin_value_on_day(
        &pool,
        &nbp,
        d("2026-10-11"),
        clock,
        TENTH_OUNCE_GRAMS,
        dec("0.95"),
        None,
    )
    .await
    .unwrap();

    assert_eq!(v.price_per_gram.date, d("2026-10-09"));
    assert_eq!(v.price_per_gram.currency, "PLN");
    // 519.09 * 3.11 = 1557.27 + 57.0999 = 1614.3699; * 0.95 = 1533.651405 (by hand).
    assert_eq!(v.valuation.melt_value_pln, dec("1614.3699"));
    assert_eq!(v.valuation.value_pln, dec("1533.651405"));
    assert_eq!(v.valuation.basis, ValuationBasis::NbpPrice);

    // Saturday and a weekday in the cached window: no new request (the mock expects one).
    for day in ["2026-10-10", "2026-10-14"] {
        coin_value_on_day(
            &pool,
            &nbp,
            d(day),
            clock,
            TENTH_OUNCE_GRAMS,
            dec("0.95"),
            None,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn christmas_gap_uses_last_quote_before_it() {
    // Served at the same path as the live call; the body is the real 2025 Christmas range.
    let server = serve_last_90(XMAS_2025, 1).await;
    let pool = open_in_memory().await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    let v = coin_value_on_day(
        &pool,
        &nbp,
        d("2025-12-26"),
        clock("2026-01-10T12:00:00Z"),
        TENTH_OUNCE_GRAMS,
        dec("1"),
        None,
    )
    .await
    .unwrap();

    // No quotes 24-28 Dec 2025; 23 Dec: 509.15 * 3.11 = 1583.4565.
    assert_eq!(v.price_per_gram.date, d("2025-12-23"));
    assert_eq!(v.valuation.value_pln, dec("1583.4565"));
}

#[tokio::test]
async fn manual_quote_overrides_value_but_keeps_melt_value() {
    let server = serve_last_90(LAST_90, 1).await;
    let pool = open_in_memory().await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    let v = coin_value_on_day(
        &pool,
        &nbp,
        d("2026-10-14"),
        clock("2026-10-15T14:00:00Z"),
        TENTH_OUNCE_GRAMS,
        dec("0.95"),
        Some(dec("1600.00")),
    )
    .await
    .unwrap();

    assert_eq!(v.valuation.basis, ValuationBasis::ManualQuote);
    assert_eq!(v.valuation.value_pln, dec("1600.00"));
    // 527.24 * 3.11 = 1639.7164.
    assert_eq!(v.valuation.melt_value_pln, dec("1639.7164"));
}

#[tokio::test]
async fn today_before_publication_falls_back_to_previous_quote() {
    // The recorded list without today's quote, as NBP serves it before noon.
    let morning: Vec<serde_json::Value> = serde_json::from_str::<Vec<serde_json::Value>>(LAST_90)
        .unwrap()
        .into_iter()
        .filter(|p| p["data"] != "2026-10-15")
        .collect();
    let server = serve_last_90(&serde_json::to_string(&morning).unwrap(), 1).await;
    let pool = open_in_memory().await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    let v = coin_value_on_day(
        &pool,
        &nbp,
        d("2026-10-15"),
        clock("2026-10-15T08:00:00Z"),
        TENTH_OUNCE_GRAMS,
        dec("1"),
        None,
    )
    .await
    .unwrap();

    assert_eq!(v.price_per_gram.date, d("2026-10-14"));
}

#[tokio::test]
async fn invalid_buyback_factor_is_reported() {
    let server = serve_last_90(LAST_90, 1).await;
    let pool = open_in_memory().await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    let err = coin_value_on_day(
        &pool,
        &nbp,
        d("2026-10-14"),
        clock("2026-10-15T14:00:00Z"),
        TENTH_OUNCE_GRAMS,
        dec("2"),
        None,
    )
    .await
    .unwrap_err();

    assert!(matches!(
        err,
        GoldValuationError::Gold(GoldError::InvalidBuybackFactor(f)) if f == dec("2")
    ));
}

#[tokio::test]
async fn nbp_failure_is_a_price_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    let err = coin_value_on_day(
        &pool,
        &nbp,
        d("2026-10-14"),
        clock("2026-10-15T14:00:00Z"),
        TENTH_OUNCE_GRAMS,
        dec("0.95"),
        None,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, GoldValuationError::Price(PriceError::Provider(_))),
        "{err}"
    );
}

#[test]
fn gold_instrument_is_nbp_pln_per_gram() {
    assert_eq!(
        (
            GOLD_GRAM.provider,
            GOLD_GRAM.provider_symbol,
            GOLD_GRAM.currency
        ),
        ("nbp", "XAU", "PLN")
    );
}
