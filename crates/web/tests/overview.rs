//! The Overview page end to end, on a ledger of PLN deposits only: valuing it needs no price
//! source, so nothing listens on the sources' port.

use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_db::open_in_memory;
use portfolio_db::transactions::{Account, NewTransaction, insert_transactions};
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::{AppState, Market, app};
use rust_decimal::Decimal;
use tower::ServiceExt;

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn deposit(day: &str, amount: &str, id: &str) -> NewTransaction {
    NewTransaction {
        kind: "deposit",
        executed_at: d(day).and_hms_opt(12, 0, 0).unwrap(),
        asset: None,
        quantity: None,
        unit_price: None,
        amount: Decimal::from_str(amount).unwrap(),
        currency: "PLN".into(),
        external_id: id.into(),
        note: None,
    }
}

/// 1000 PLN on 10 Jan 2026 and 500 PLN on 20 Sep, kept as cash; today is 15 Oct 2026.
async fn setup() -> Router {
    let pool = open_in_memory().await.unwrap();
    insert_transactions(
        &pool,
        Account {
            name: "XTB IKZE",
            kind: "ikze",
            provider: "xtb",
            currency: "PLN",
        },
        &[],
        "xtb",
        &[
            deposit("2026-01-10", "1000", "a1"),
            deposit("2026-09-20", "500", "a2"),
        ],
    )
    .await
    .unwrap();
    let market = Market {
        nbp: NbpClient::with_base_url("http://127.0.0.1:9").unwrap(),
        kraken: KrakenClient::with_base_url("http://127.0.0.1:9").unwrap(),
        alphavantage: None,
        gold_buyback_factor: Decimal::ONE,
    };
    app(AppState {
        pool,
        market: Arc::new(market),
        today: Arc::new(|| d("2026-10-15")),
        now: Arc::new(Utc::now),
    })
}

async fn body(router: &Router, uri: &str, htmx: bool) -> String {
    let mut request = Request::get(uri);
    if htmx {
        request = request.header("HX-Request", "true");
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace('\u{a0}', " ")
}

#[tokio::test]
async fn all_shows_the_whole_portfolio_and_its_chart() {
    let router = setup().await;
    let page = body(&router, "/", false).await;

    assert!(page.contains("<title>Overview · Portfel</title>"), "{page}");
    // Cash only: worth what went in, so a flat P&L and a 0% XIRR over 9 months.
    assert!(page.contains("1 500,00<span"), "{page}");
    assert!(page.contains("±0,00 zł"), "{page}");
    assert!(page.contains("since January 2026"), "{page}");
    assert!(page.contains("±0,0%"), "{page}");
    assert!(page.contains("per year, since January 2026"), "{page}");
    assert!(page.contains("net, since January 2026"), "{page}");
    assert!(page.contains(r##"href="/" hx-get="/" hx-target="#overview" hx-swap="outerHTML" hx-push-url="true" aria-current="true">All</a>"##), "{page}");
    // 10 Jan, the month ends Jan–Sep and 15 Oct: 11 days, every 2nd labelled (10 Jan and 31
    // Jan share a month, so the first shows its day). Value and contributions are 1000 then
    // 1500: the axis runs 0 to 1500 in steps of 500.
    assert!(page.contains(">10 Jan ’26</span>"), "{page}");
    assert!(
        page.contains("left: 20.00%; transform: translateX(-50%)"),
        "{page}"
    );
    assert!(page.contains(">1 500</span>"), "{page}");
    // Without an Alpha Vantage key there is no VWCE price: no dashed line, and the note says so.
    assert!(
        page.contains("All-in VWCE: no price for some days"),
        "{page}"
    );
    assert!(!page.contains("stroke-dasharray"), "{page}");
    assert!(page.contains(r#"hx-get="/?bench=off""#), "{page}");
}

#[tokio::test]
async fn the_switch_returns_just_the_panel_for_htmx() {
    let router = setup().await;
    let panel = body(&router, "/?period=1M&bench=off", true).await;

    assert!(panel.starts_with(r#"<div id="overview""#), "{panel}");
    assert!(!panel.contains("<html"), "{panel}");
    assert!(panel.contains(r#"aria-current="true">1M</a>"#), "{panel}");
    // From 15 Sep: 1000 held then, 500 put in since, still 1500: flat, too short for XIRR.
    assert!(panel.contains("500,00 zł"), "{panel}");
    assert!(panel.contains("net, past month"), "{panel}");
    assert!(panel.contains("Needs 3+ months of data"), "{panel}");
    // The benchmark is off: no note, and the checkbox turns it back on.
    assert!(!panel.contains("All-in VWCE:"), "{panel}");
    assert!(panel.contains(r#"hx-get="/?period=1M""#), "{panel}");
    assert!(!panel.contains(" checked"), "{panel}");
    // 15 Sep, 30 Sep (same month: with days) and 15 Oct.
    assert!(panel.contains(">15 Sep ’26</span>"), "{panel}");
    assert!(panel.contains(">30 Sep</span>"), "{panel}");
    assert!(panel.contains(">Oct</span>"), "{panel}");

    // The same state as a full page, and an unknown period falls back to All.
    let page = body(&router, "/?period=1M&bench=off", false).await;
    assert!(page.contains("<html"), "{page}");
    let fallback = body(&router, "/?period=2W", true).await;
    assert!(
        fallback.contains(r#"aria-current="true">All</a>"#),
        "{fallback}"
    );
}
