use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_db::open_in_memory;
use portfolio_sources::kraken::{KrakenClient, KrakenError, parse_daily_ohlc, parse_ticker};
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::{
    BTC, Clock, ETH, PriceError, VWCE, close_on_day, kraken_spot_in_pln, value_in_pln,
};
use rust_decimal::Decimal;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Real Kraken public responses, recorded 2026-10-15 ~13:40 UTC.
const TICKER: &str = include_str!("fixtures/kraken/ticker_btc_eth_eur.json");
const OHLC_BTC: &str = include_str!("fixtures/kraken/ohlc_xbteur_daily.json");
const OHLC_ETH: &str = include_str!("fixtures/kraken/ohlc_etheur_daily.json");
const UNKNOWN_PAIR: &str = include_str!("fixtures/kraken/error_unknown_pair.json");
const EUR_OCT: &str = include_str!("fixtures/nbp/eur_2026-10-05_2026-10-14.json");

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
fn parses_ticker_last_trade() {
    let prices = parse_ticker(TICKER).unwrap();

    assert_eq!(prices["XXBTZEUR"], dec("71806.40000"));
    assert_eq!(prices["XETHZEUR"], dec("2095.59000"));
    assert_eq!(prices["XXBTZEUR"].to_string(), "71806.40000");
}

#[test]
fn parses_daily_ohlc_without_the_unfinished_day() {
    let closes = parse_daily_ohlc(OHLC_BTC, "XXBTZEUR").unwrap();

    // 721 candles; the newest (2026-10-15, in progress) is dropped.
    assert_eq!(closes.len(), 720);
    assert_eq!(closes.first().unwrap().date, d("2024-10-25"));
    let last = closes.last().unwrap();
    assert_eq!((last.date, last.close), (d("2026-10-14"), dec("72888.3")));
    // Crypto trades every day: Saturday and Sunday are present.
    assert!(closes.iter().any(|c| c.date == d("2026-10-11")));
    assert!(closes.windows(2).all(|w| w[0].date < w[1].date));
}

#[test]
fn api_errors_are_surfaced() {
    let expected = vec!["EQuery:Unknown asset pair".to_owned()];
    assert!(matches!(parse_ticker(UNKNOWN_PAIR), Err(KrakenError::Api(e)) if e == expected));
    assert!(matches!(
        parse_daily_ohlc(UNKNOWN_PAIR, "NOPEEUR"),
        Err(KrakenError::Api(_))
    ));
}

#[test]
fn malformed_bodies_are_rejected() {
    let numeric_price = TICKER.replacen("\"71806.40000\"", "71806.4", 1);
    assert!(matches!(
        parse_ticker(&numeric_price),
        Err(KrakenError::Parse(_))
    ));
    assert!(matches!(
        parse_daily_ohlc(OHLC_BTC, "XETHZEUR"),
        Err(KrakenError::Parse(msg)) if msg.contains("no candles")
    ));
    let no_last = OHLC_BTC.replace("\"last\"", "\"lost\"");
    assert!(matches!(
        parse_daily_ohlc(&no_last, "XXBTZEUR"),
        Err(KrakenError::Parse(_))
    ));
    assert!(matches!(parse_ticker("<html>"), Err(KrakenError::Parse(_))));
}

async fn server_with(endpoint: &str, pair: &str, body: &str, times: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/0/public/{endpoint}")))
        .and(query_param("pair", pair))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(times)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn client_requests_daily_interval() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/0/public/OHLC"))
        .and(query_param("pair", "XXBTZEUR"))
        .and(query_param("interval", "1440"))
        .respond_with(ResponseTemplate::new(200).set_body_string(OHLC_BTC))
        .expect(1)
        .mount(&server)
        .await;

    let client = KrakenClient::with_base_url(server.uri()).unwrap();

    assert_eq!(client.daily_ohlc("XXBTZEUR").await.unwrap().len(), 720);
}

#[tokio::test]
async fn ticker_missing_a_requested_pair_is_an_error() {
    let server = server_with("Ticker", "XXBTZEUR,XXRPZEUR", TICKER, 1).await;
    let client = KrakenClient::with_base_url(server.uri()).unwrap();

    let err = client
        .last_prices(&["XXBTZEUR", "XXRPZEUR"])
        .await
        .unwrap_err();

    assert!(matches!(err, KrakenError::Parse(msg) if msg.contains("XXRPZEUR")));
}

#[tokio::test]
async fn http_error_status_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(520))
        .mount(&server)
        .await;
    let client = KrakenClient::with_base_url(server.uri()).unwrap();

    assert!(matches!(
        client.daily_ohlc("XXBTZEUR").await,
        Err(KrakenError::Status(520))
    ));
}

#[tokio::test]
async fn crypto_close_on_a_sunday_is_that_days_close() {
    let server = server_with("OHLC", "XETHZEUR", OHLC_ETH, 1).await;
    let pool = open_in_memory().await.unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let clock = clock("2026-10-15T14:00:00Z");

    let sunday = close_on_day(&pool, &kraken, &ETH, d("2026-10-11"), clock)
        .await
        .unwrap();
    // Cached now: another day needs no request (the mock expects exactly one).
    let monday = close_on_day(&pool, &kraken, &ETH, d("2026-10-12"), clock)
        .await
        .unwrap();

    assert_eq!(
        (sunday.date, sunday.close),
        (d("2026-10-11"), dec("2262.26"))
    );
    assert_eq!(
        (monday.date, monday.close),
        (d("2026-10-12"), dec("2190.19"))
    );
}

#[tokio::test]
async fn today_uses_yesterdays_final_close_and_refetches_hourly() {
    let server = server_with("OHLC", "XXBTZEUR", OHLC_BTC, 2).await;
    let pool = open_in_memory().await.unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();

    for now in [
        "2026-10-15T14:00:00Z",
        "2026-10-15T14:30:00Z",
        "2026-10-15T15:01:00Z",
    ] {
        let quote = close_on_day(&pool, &kraken, &BTC, d("2026-10-15"), clock(now))
            .await
            .unwrap();
        assert_eq!(quote.date, d("2026-10-14"), "{now}");
    }
    // 14:00 fetches, 14:30 is within the hour, 15:01 fetches again: two requests.
}

#[tokio::test]
async fn btc_close_in_pln_uses_nbp_eur_rate_of_that_day() {
    let server = server_with("OHLC", "XXBTZEUR", OHLC_BTC, 1).await;
    Mock::given(method("GET"))
        .and(path("/exchangerates/rates/a/eur/2026-09-30/2026-10-14/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .expect(1)
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();

    let v = value_in_pln(
        &pool,
        &kraken,
        &nbp,
        &BTC,
        d("2026-10-14"),
        clock("2026-10-15T14:00:00Z"),
    )
    .await
    .unwrap();

    assert_eq!(v.fx.as_ref().unwrap().mid, dec("4.3706"));
    // 72888.3 EUR * 4.3706 = 291553.2 + 26968.671 + 43.73298 = 318565.60398 PLN (by hand).
    assert_eq!(v.close_pln, dec("318565.60398"));
}

#[tokio::test]
async fn spot_values_btc_and_eth_from_one_ticker_call() {
    let server = server_with("Ticker", "XXBTZEUR,XETHZEUR", TICKER, 1).await;
    // Thursday 10-15 before NBP publishes: the 10-14 table (4.3706) applies. One NBP request
    // for both instruments, since both are in EUR.
    Mock::given(method("GET"))
        .and(path("/exchangerates/rates/a/eur/2026-10-01/2026-10-15/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .expect(1)
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let clock = clock("2026-10-15T09:30:00Z");

    let spot = kraken_spot_in_pln(&pool, &kraken, &nbp, &[BTC, ETH], clock)
        .await
        .unwrap();

    assert_eq!(spot.len(), 2);
    assert_eq!((spot[0].price, spot[0].at), (dec("71806.40000"), clock.now));
    assert_eq!(spot[0].fx.effective_date, d("2026-10-14"));
    // 71806.4 * 4.3706 = 313837.05184; 2095.59 * 4.3706 = 9158.985654 (by hand).
    assert_eq!(spot[0].price_pln, dec("313837.05184"));
    assert_eq!(spot[1].price_pln, dec("9158.985654"));
}

#[tokio::test]
async fn instrument_and_provider_must_match() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let clock = clock("2026-10-15T14:00:00Z");

    let close = close_on_day(&pool, &kraken, &VWCE, d("2026-10-14"), clock).await;
    let spot = kraken_spot_in_pln(&pool, &kraken, &nbp, &[BTC, VWCE], clock).await;

    for result in [close.map(|_| ()), spot.map(|_| ())] {
        assert!(matches!(
            result,
            Err(PriceError::WrongProvider {
                instrument: "VWCE",
                expected: "alphavantage",
                got: "kraken"
            })
        ));
    }
}

#[test]
fn crypto_instruments_are_kraken_eur_pairs() {
    assert_eq!(
        (BTC.provider, BTC.provider_symbol, BTC.currency),
        ("kraken", "XXBTZEUR", "EUR")
    );
    assert_eq!(
        (ETH.provider, ETH.provider_symbol, ETH.currency),
        ("kraken", "XETHZEUR", "EUR")
    );
}
