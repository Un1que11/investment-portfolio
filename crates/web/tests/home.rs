//! Home page and chart handlers against an in-memory database and mocked price sources.

use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use http_body_util::BodyExt;
use portfolio_core::AssetClass;
use portfolio_db::gold::{NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::transactions::{Account, Asset, NewTransaction, insert_transactions};
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::{AppState, Market, app};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EUR_OCT: &str =
    include_str!("../../sources/tests/fixtures/nbp/eur_2026-10-05_2026-10-14.json");
const OHLC_BTC: &str = include_str!("../../sources/tests/fixtures/kraken/ohlc_xbteur_daily.json");
const GOLD_LAST_90: &str = include_str!("../../sources/tests/fixtures/nbp/gold_last_90.json");

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn at(s: &str) -> NaiveDateTime {
    d(s).and_hms_opt(12, 0, 0).unwrap()
}

fn row(
    kind: &'static str,
    day: &str,
    asset: Option<&'static str>,
    qty: Option<Decimal>,
    amount: Decimal,
    currency: &str,
    id: &str,
) -> NewTransaction {
    NewTransaction {
        kind,
        executed_at: at(day),
        asset,
        quantity: qty,
        unit_price: qty.map(|q| amount / q),
        amount,
        currency: currency.into(),
        external_id: id.into(),
        note: None,
    }
}

async fn mocked_sources() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/exchangerates/rates/a/eur/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cenyzlota/last/90/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(GOLD_LAST_90))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/0/public/OHLC"))
        .respond_with(ResponseTemplate::new(200).set_body_string(OHLC_BTC))
        .mount(&server)
        .await;
    server
}

/// The app on Wed 14 Oct 2026 (evening), with prices from `server` and no Alpha Vantage key.
async fn setup(server: &MockServer) -> (Router, SqlitePool) {
    let pool = open_in_memory().await.unwrap();
    let market = Market {
        nbp: NbpClient::with_base_url(server.uri()).unwrap(),
        kraken: KrakenClient::with_base_url(server.uri()).unwrap(),
        alphavantage: None,
        gold_buyback_factor: dec!(0.95),
    };
    let now: DateTime<Utc> = "2026-10-14T20:00:00Z".parse().unwrap();
    let state = AppState {
        pool: pool.clone(),
        market: Arc::new(market),
        today: Arc::new(move || now.date_naive()),
        now: Arc::new(move || now),
    };
    (app(state), pool)
}

/// A coin for 1600 PLN on 1 Oct; Kraken: 200 EUR in on 12 Oct, 0.002 BTC for 180 EUR + 2.70 fee
/// on 13 Oct.
async fn coin_and_kraken(pool: &SqlitePool) {
    add_coin(
        pool,
        &NewCoin {
            label: None,
            purchased_on: d("2026-10-01"),
            purchase_price_pln: dec!(1600),
            fine_weight_g: dec!(3.11),
            manual_quote_pln: None,
        },
    )
    .await
    .unwrap();
    insert_transactions(
        pool,
        Account {
            name: "Kraken",
            kind: "exchange",
            provider: "kraken",
            currency: "EUR",
        },
        &[Asset {
            symbol: "BTC",
            name: "Bitcoin",
            class: AssetClass::Crypto,
            currency: "EUR",
        }],
        "kraken",
        &[
            row("deposit", "2026-10-12", None, None, dec!(200), "EUR", "k1"),
            row(
                "buy",
                "2026-10-13",
                Some("BTC"),
                Some(dec!(0.002)),
                dec!(180),
                "EUR",
                "k2",
            ),
            row(
                "fee",
                "2026-10-13",
                Some("BTC"),
                None,
                dec!(2.70),
                "EUR",
                "k3",
            ),
        ],
    )
    .await
    .unwrap();
}

/// XTB IKZE: 1000 PLN in on 1 Oct, 1 VWCE for 700 PLN on 2 Oct. VWCE has no price here.
async fn vwce_without_price(pool: &SqlitePool) {
    insert_transactions(
        pool,
        Account {
            name: "XTB IKZE",
            kind: "ikze",
            provider: "xtb",
            currency: "PLN",
        },
        &[Asset {
            symbol: "VWCE",
            name: "VWCE",
            class: AssetClass::Vwce,
            currency: "EUR",
        }],
        "xtb",
        &[
            row("deposit", "2026-10-01", None, None, dec!(1000), "PLN", "x1"),
            row(
                "buy",
                "2026-10-02",
                Some("VWCE"),
                Some(dec!(1)),
                dec!(700),
                "PLN",
                "x2",
            ),
        ],
    )
    .await
    .unwrap();
}

async fn get(router: &Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// The text of the tile with `id` (up to the next tile), tags removed.
fn tile(body: &str, id: &str) -> String {
    let start = body
        .find(&format!("id=\"{id}\""))
        .unwrap_or_else(|| panic!("no #{id} in {body}"));
    let rest = &body[start + body[start..].find('>').unwrap() + 1..];
    let end = ["<div class=\"tile\"", "</section>"]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
    let section = &rest[..end];
    let mut text = String::new();
    let mut in_tag = false;
    for c in section.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The chart data from the fragment's `data-series` attribute.
fn series(body: &str) -> Value {
    let start = body.find("data-series=\"").expect("chart data") + "data-series=\"".len();
    let raw = &body[start..start + body[start..].find('"').unwrap()];
    let json = raw
        .replace("&#34;", "\"")
        .replace("&quot;", "\"")
        .replace("&#38;", "&")
        .replace("&amp;", "&");
    serde_json::from_str(&json).unwrap()
}

#[tokio::test]
async fn home_shows_value_contributions_pnl_and_xirr() {
    let server = mocked_sources().await;
    let (router, pool) = setup(&server).await;
    coin_and_kraken(&pool).await;

    let (status, body) = get(&router, "/").await;

    assert_eq!(status, StatusCode::OK);
    // As the report for 14 Oct: BTC 637.13120796 + coin 1557.73058 + 17.30 EUR cash 75.61138.
    assert_eq!(tile(&body, "value"), "Value (PLN) 2 270.47");
    // 1600 PLN + 200 EUR x 4.3889 (12 Oct).
    assert_eq!(
        tile(&body, "contributions"),
        "Net contributions (PLN) 2 477.78"
    );
    // 2270.47316796 - 2477.78 = -207.30683204; / 2477.78 = -8.37%.
    assert!(
        tile(&body, "pnl").ends_with("-207.31 (-8.37%)"),
        "{}",
        tile(&body, "pnl")
    );
    assert!(tile(&body, "xirr").contains("since 2026-10-01"));
    assert!(
        tile(&body, "xirr").ends_with("a year"),
        "{}",
        tile(&body, "xirr")
    );
    assert!(!body.contains("not available"));
    // The chart is loaded after the page.
    assert!(body.contains("hx-get=\"/chart\""));
}

#[tokio::test]
async fn home_shows_no_value_pnl_or_xirr_when_a_price_is_missing() {
    let server = mocked_sources().await;
    let (router, pool) = setup(&server).await;
    vwce_without_price(&pool).await;

    let (status, body) = get(&router, "/").await;

    assert_eq!(status, StatusCode::OK);
    assert!(tile(&body, "value").ends_with("not available"));
    assert!(tile(&body, "pnl").ends_with("not available"));
    assert!(tile(&body, "xirr").ends_with("not available: some prices are missing"));
    // Contributions need no price.
    assert!(tile(&body, "contributions").ends_with("1 000.00"));
    assert!(body.contains("No price for VWCE."), "{body}");
    // The 300 PLN of cash alone is not passed off as the value.
    assert!(!body.contains("300.00"), "{body}");
}

#[tokio::test]
async fn chart_has_value_and_contributions_for_each_day() {
    let server = mocked_sources().await;
    let (router, pool) = setup(&server).await;
    coin_and_kraken(&pool).await;

    let (status, body) = get(&router, "/chart").await;

    assert_eq!(status, StatusCode::OK);
    // From the first contribution (1 Oct) to today, in one month: just the two days.
    // 1 Oct: the coin, 518.14 PLN/g x 3.11 g x 0.95 = 1530.84463.
    assert_eq!(
        series(&body),
        json!({
            "dates": ["2026-10-01", "2026-10-14"],
            "value": ["1530.84", "2270.47"],
            "contributions": ["1600.00", "2477.78"],
        })
    );
    assert!(!body.contains("gaps"));
}

#[tokio::test]
async fn chart_leaves_a_gap_where_a_price_is_missing() {
    let server = mocked_sources().await;
    let (router, pool) = setup(&server).await;
    vwce_without_price(&pool).await;

    let (_, body) = get(&router, "/chart").await;

    // 1 Oct is all cash; on 14 Oct VWCE has no price, so there is no value (not 300).
    assert_eq!(
        series(&body),
        json!({
            "dates": ["2026-10-01", "2026-10-14"],
            "value": ["1000.00", null],
            "contributions": ["1000.00", "1000.00"],
        })
    );
    assert!(body.contains("1 day(s) have no complete value"));
    assert!(body.contains("first 2026-10-14: VWCE"), "{body}");
}

#[tokio::test]
async fn chart_without_contributions_has_nothing_to_draw() {
    let server = mocked_sources().await;
    let (router, _) = setup(&server).await;

    let (status, body) = get(&router, "/chart").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Nothing to draw yet"));
    assert!(!body.contains("data-series"));
}
