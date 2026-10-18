use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_db::gold::{NewCoin, add_coin, list_coins};
use portfolio_db::open_in_memory;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::gold::{CoinForm, FieldErrors, parse_form};
use portfolio_web::{AppState, Market, app};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tower::ServiceExt;

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

const TODAY: &str = "2026-10-15";

async fn setup() -> (Router, SqlitePool) {
    let pool = open_in_memory().await.unwrap();
    // The gold page asks no price source: nothing listens on the discard port.
    let market = Market {
        nbp: NbpClient::with_base_url("http://127.0.0.1:9").unwrap(),
        kraken: KrakenClient::with_base_url("http://127.0.0.1:9").unwrap(),
        alphavantage: None,
        gold_buyback_factor: Decimal::ONE,
    };
    let state = AppState {
        pool: pool.clone(),
        market: Arc::new(market),
        today: Arc::new(|| NaiveDate::from_str(TODAY).unwrap()),
        now: Arc::new(Utc::now),
    };
    (app(state), pool)
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, String, Option<String>) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_owned());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap(), location)
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post(uri: &str, form: &str, htmx: bool) -> Request<Body> {
    let mut builder =
        Request::post(uri).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if htmx {
        builder = builder.header("HX-Request", "true");
    }
    builder.body(Body::from(form.to_owned())).unwrap()
}

fn form(date: &str, price: &str, weight: &str, quote: &str, label: &str) -> CoinForm {
    CoinForm {
        label: label.into(),
        purchased_on: date.into(),
        purchase_price: price.into(),
        fine_weight: weight.into(),
        manual_quote: quote.into(),
    }
}

fn new_coin(price: &str) -> NewCoin {
    NewCoin {
        label: None,
        purchased_on: d("2025-03-14"),
        purchase_price_pln: dec(price),
        fine_weight_g: dec("3.11"),
        manual_quote_pln: None,
    }
}

#[test]
fn parse_form_accepts_polish_number_formatting() {
    let input = form(
        "2025-03-14",
        "1 234,56",
        "3,11",
        "1\u{a0}650,00",
        "  Maple  ",
    );

    let coin = parse_form(&input, d(TODAY)).unwrap();

    assert_eq!(
        coin,
        NewCoin {
            label: Some("Maple".into()),
            purchased_on: d("2025-03-14"),
            purchase_price_pln: dec("1234.56"),
            fine_weight_g: dec("3.11"),
            manual_quote_pln: Some(dec("1650.00")),
        }
    );
}

#[test]
fn parse_form_optional_fields_may_be_empty() {
    let coin = parse_form(&form(TODAY, "1600", "3.11", " ", ""), d(TODAY)).unwrap();

    assert_eq!((coin.label, coin.manual_quote_pln), (None, None));
    assert_eq!(coin.purchased_on, d(TODAY));
}

#[test]
fn parse_form_reports_every_invalid_field() {
    let input = form("2026-10-16", "-5", "0", "1e3", &"x".repeat(101));

    let errors = parse_form(&input, d(TODAY)).unwrap_err();

    assert_eq!(
        errors,
        FieldErrors {
            label: Some("at most 100 characters".into()),
            purchased_on: Some("cannot be in the future".into()),
            purchase_price: Some("must be more than 0".into()),
            fine_weight: Some("must be more than 0".into()),
            manual_quote: Some("enter a number like 1234.56".into()),
        }
    );
}

#[test]
fn parse_form_limits_decimal_places_and_checks_dates() {
    let input = form("14.03.2025", "12.345", "3.11115", "", "");

    let errors = parse_form(&input, d(TODAY)).unwrap_err();

    assert_eq!(
        errors.purchased_on.as_deref(),
        Some("enter a date (YYYY-MM-DD)")
    );
    assert_eq!(
        errors.purchase_price.as_deref(),
        Some("at most 2 decimal places")
    );
    assert_eq!(
        errors.fine_weight.as_deref(),
        Some("at most 4 decimal places")
    );

    let old = parse_form(&form("1899-12-31", "1", "1", "", ""), d(TODAY)).unwrap_err();
    assert_eq!(old.purchased_on.as_deref(), Some("too far in the past"));
    let empty = parse_form(&form("", "", "", "", ""), d(TODAY)).unwrap_err();
    assert_eq!(empty.purchase_price.as_deref(), Some("required"));
}

#[tokio::test]
async fn health_and_root() {
    let (router, _) = setup().await;

    assert_eq!(send(&router, get("/health")).await.1, "ok");
    // The Overview: with nothing put in yet there is nothing to chart.
    let (status, body, _) = send(&router, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Nothing to chart yet"), "{body}");
    assert_eq!(send(&router, get("/nope")).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn empty_page_has_form_with_default_weight() {
    let (router, _) = setup().await;

    let (status, body, _) = send(&router, get("/gold")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<h1>Gold coins</h1>"));
    assert!(body.contains(r#"name="fine_weight" inputmode="decimal" value="3.11""#));
    assert!(body.contains("No coins yet."));
    assert!(body.contains("htmx.org@2.0.4"));
}

#[tokio::test]
async fn plain_post_adds_coin_and_redirects() {
    let (router, pool) = setup().await;
    let body = "purchased_on=2025-03-14&purchase_price=1+234%2C56&fine_weight=3.11\
                &manual_quote=&label=Krugerrand";

    let (status, _, location) = send(&router, post("/gold/coins", body, false)).await;

    assert_eq!(
        (status, location.as_deref()),
        (StatusCode::SEE_OTHER, Some("/gold"))
    );
    let coins = list_coins(&pool).await.unwrap();
    assert_eq!(coins.len(), 1);
    assert_eq!(coins[0].purchase_price_pln, dec("1234.56"));
    let (_, page, _) = send(&router, get("/gold")).await;
    assert!(page.contains(">1234.56<"));
    assert!(page.contains("Krugerrand"));
}

#[tokio::test]
async fn htmx_post_returns_panel_with_totals() {
    let (router, pool) = setup().await;
    add_coin(&pool, &new_coin("1234.56")).await.unwrap();
    let body = "purchased_on=2026-01-20&purchase_price=1600&fine_weight=3.11\
                &manual_quote=1650&label=";

    let (status, html, _) = send(&router, post("/gold/coins", body, true)).await;

    assert_eq!(status, StatusCode::OK);
    // Only the panel, not the whole page.
    assert!(html.starts_with(r#"<section id="gold-panel">"#));
    assert!(!html.contains("<html"));
    // Totals: 1234.56 + 1600 = 2834.56 PLN; 3.11 + 3.11 = 6.22 g.
    assert!(html.contains(r#"<th class="num">2834.56</th>"#));
    assert!(html.contains(r#"<th class="num">6.22</th>"#));
    assert!(html.contains(">1650.00<"));
    // The form is reset after a successful add.
    assert!(html.contains(r#"name="purchase_price" inputmode="decimal" value="""#));
}

#[tokio::test]
async fn invalid_post_shows_errors_and_keeps_input() {
    let (router, pool) = setup().await;
    let bad = "purchased_on=2030-01-01&purchase_price=abc&fine_weight=3.11&manual_quote=&label=";

    let (plain_status, plain_body, _) = send(&router, post("/gold/coins", bad, false)).await;
    let (htmx_status, htmx_body, _) = send(&router, post("/gold/coins", bad, true)).await;

    assert_eq!(plain_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(plain_body.contains("<html"));
    // htmx only swaps 2xx responses, so the panel with errors comes back as 200.
    assert_eq!(htmx_status, StatusCode::OK);
    for body in [&plain_body, &htmx_body] {
        assert!(body.contains("cannot be in the future"));
        assert!(body.contains("enter a number like 1234.56"));
        assert!(body.contains(r#"value="abc""#));
    }
    assert!(list_coins(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn user_text_is_html_escaped() {
    let (router, _) = setup().await;
    let body = "purchased_on=2025-03-14&purchase_price=1&fine_weight=1&manual_quote=\
                &label=%3Cscript%3Ex%3C%2Fscript%3E";

    send(&router, post("/gold/coins", body, false)).await;
    let (_, page, _) = send(&router, get("/gold")).await;

    assert!(!page.contains("<script>x</script>"));
    assert!(page.contains("&#60;script&#62;x&#60;/script&#62;"));
}

#[tokio::test]
async fn delete_removes_coin_or_404s() {
    let (router, pool) = setup().await;
    let coin = add_coin(&pool, &new_coin("1234.56")).await.unwrap();
    let uri = format!("/gold/coins/{}/delete", coin.id);

    let (status, body, _) = send(&router, post(&uri, "", true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No coins yet."));
    assert!(list_coins(&pool).await.unwrap().is_empty());

    let (missing, _, _) = send(&router, post(&uri, "", false)).await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn plain_delete_redirects() {
    let (router, pool) = setup().await;
    let coin = add_coin(&pool, &new_coin("1")).await.unwrap();

    let uri = format!("/gold/coins/{}/delete", coin.id);
    let (status, _, location) = send(&router, post(&uri, "", false)).await;

    assert_eq!(
        (status, location.as_deref()),
        (StatusCode::SEE_OTHER, Some("/gold"))
    );
}
