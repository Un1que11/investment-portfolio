//! The base layout, theme cookie, stylesheets and component sheet, driven in-process.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::{Extension, Router};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_core::config::Config;
use portfolio_db::open_in_memory;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::{AppState, Market, app};
use rust_decimal::Decimal;
use tower::ServiceExt;

async fn setup() -> Router {
    let pool = open_in_memory().await.unwrap();
    // Nothing listens on the discard port: every price is missing.
    let market = Market {
        nbp: NbpClient::with_base_url("http://127.0.0.1:9").unwrap(),
        kraken: KrakenClient::with_base_url("http://127.0.0.1:9").unwrap(),
        alphavantage: None,
        gold_buyback_factor: Decimal::ONE,
    };
    let config = Config::from_toml(
        r#"
        [targets]
        vwce = 55
        crypto = 25
        gold = 20
        [targets.crypto_split]
        btc = 75
        eth = 25
        [ikze.limits]
        2026 = "11304.00"
        [tax]
        rate_percent = 12
        [gold]
        buyback_factor = "1"
        "#,
    )
    .unwrap();
    app(AppState {
        pool,
        market: Arc::new(market),
        today: Arc::new(|| NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()),
        now: Arc::new(Utc::now),
    })
    .layer(Extension(Arc::new(config)))
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::get(uri);
    if let Some(c) = cookie {
        builder = builder.header(header::COOKIE, c);
    }
    builder.body(Body::empty()).unwrap()
}

fn post_theme(form: &str) -> Request<Body> {
    Request::post("/theme")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn pages_use_the_shell_and_follow_the_system_theme_without_a_cookie() {
    let router = setup().await;
    let (status, _, body) = send(&router, get("/gold", None)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"href="/static/tokens.css""#), "{body}");
    assert!(
        body.contains(r#"<div class="app" data-theme="light""#),
        "{body}"
    );
    assert!(body.contains("prefers-color-scheme: dark"), "{body}");
    // The current page is marked; pages not built yet are not links.
    assert!(
        body.contains(r#"<a href="/gold" aria-current="page""#),
        "{body}"
    );
    assert!(!body.contains(r#"href="/import""#), "{body}");
    assert!(body.contains("Import</span>"), "{body}");
    assert!(body.contains(r#"href="/allocation""#), "{body}");
    // No price has been fetched into the empty database.
    assert!(body.contains("No prices yet"), "{body}");
    assert!(body.contains(r#"name="theme" value="dark""#), "{body}");
    assert!(body.contains("Dark theme"), "{body}");
    // The gold page itself is still there.
    assert!(body.contains(r#"<h2 id="coins-h""#), "{body}");
}

#[tokio::test]
async fn a_theme_cookie_is_rendered_by_the_server() {
    let router = setup().await;
    let (_, _, body) = send(&router, get("/gold", Some("theme=dark"))).await;

    assert!(
        body.contains(r#"<div class="app" data-theme="dark""#),
        "{body}"
    );
    assert!(!body.contains("prefers-color-scheme"), "{body}");
    assert!(body.contains(r#"name="theme" value="light""#), "{body}");
    assert!(body.contains("Light theme"), "{body}");
}

#[tokio::test]
async fn the_toggle_saves_the_theme_and_goes_back() {
    let router = setup().await;

    let (status, headers, _) = send(&router, post_theme("theme=dark&next=%2Fgold")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/gold");
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("theme=dark; Path=/;"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");

    let (_, headers, _) = send(
        &router,
        post_theme("theme=light&next=https%3A%2F%2Fexample.com"),
    )
    .await;
    assert_eq!(headers[header::LOCATION], "/");

    let (status, headers, _) = send(&router, post_theme("theme=blue&next=%2Fgold")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(headers.get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn stylesheets_are_served_as_css() {
    let router = setup().await;
    for path in ["/static/tokens.css", "/static/app.css"] {
        let (status, headers, body) = send(&router, get(path, None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            headers[header::CONTENT_TYPE],
            "text/css; charset=utf-8",
            "{path}"
        );
        assert!(body.contains(".side"), "{path}");
    }
    let (_, _, tokens) = send(&router, get("/static/tokens.css", None)).await;
    assert!(tokens.contains("--accent:#3550D0"));
    assert!(tokens.contains("family=Geist"));
}

#[tokio::test]
async fn the_component_sheet_renders_the_design_samples() {
    let router = setup().await;
    let (status, _, body) = send(&router, get("/components", None)).await;
    let nb = |s: &str| s.replace(' ', "\u{a0}");

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&nb("▲&nbsp;+1 408,00 zł")), "{body}");
    assert!(body.contains(&nb("▼&nbsp;\u{2212}312,40 zł")), "{body}");
    assert!(body.contains(&nb(">±0,00 zł<")), "{body}");
    assert!(body.contains("▼ \u{2212}2,0\u{a0}pp"), "{body}");
    assert!(body.contains("of 11\u{a0}304,00\u{a0}zł"), "{body}");
    assert!(body.contains("50,0% used"), "{body}");
    assert!(body.contains("width: 28.7%"), "{body}");
    assert!(body.contains("▲ +460,00\u{a0}zł · +28,0%"), "{body}");
    assert!(body.contains(r#"aria-invalid="true""#), "{body}");
    assert!(body.contains("Purchases paused"), "{body}");
}
