//! The states of `design/screens/08` through the router: the empty Overview, a price source
//! that is down (banner, stale card, sync status), the one-point chart, the loading skeleton
//! and the sync status variants. Every source points at a closed port, so each request to one
//! fails as if it were down.

use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Extension, Router};
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_core::AssetClass;
use portfolio_core::config::Config;
use portfolio_core::price::DailyClose;
use portfolio_db::gold::{NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::prices::{ensure_asset, upsert_closes};
use portfolio_db::transactions::{Account, NewTransaction, insert_transactions};
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::{AppState, Market, app};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tower::ServiceExt;

const DOWN: &str = "http://127.0.0.1:9";
/// Thursday 15 Oct 2026, noon UTC.
const NOW: &str = "2026-10-15T12:00:00Z";

const CONFIG: &str = r#"
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
"#;

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn router(pool: SqlitePool) -> Router {
    let market = Market {
        nbp: NbpClient::with_base_url(DOWN).unwrap(),
        kraken: KrakenClient::with_base_url(DOWN).unwrap(),
        alphavantage: None,
        gold_buyback_factor: Decimal::ONE,
    };
    let now = t(NOW);
    app(AppState {
        pool,
        market: Arc::new(market),
        today: Arc::new(move || now.date_naive()),
        now: Arc::new(move || now),
    })
    .layer(Extension(Arc::new(Config::from_toml(CONFIG).unwrap())))
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

/// One 1/10 oz coin bought on 1 Oct for 1500 zł.
async fn coin(pool: &SqlitePool) {
    let coin = NewCoin {
        label: None,
        purchased_on: d("2026-10-01"),
        purchase_price_pln: Decimal::from(1500),
        fine_weight_g: Decimal::from_str("3.11").unwrap(),
        manual_quote_pln: None,
    };
    add_coin(pool, &coin).await.unwrap();
}

/// NBP's gold price of `day`, 500 zł a gram, as fetched at `fetched_at`.
async fn gold_close(pool: &SqlitePool, day: &str, fetched_at: &str) {
    let id = ensure_asset(
        pool,
        "XAU_G",
        "Gold, PLN per gram (NBP)",
        AssetClass::Gold,
        "PLN",
    )
    .await
    .unwrap();
    let close = DailyClose {
        date: d(day),
        close: Decimal::from(500),
    };
    upsert_closes(pool, id, "PLN", "nbp", &[close], t(fetched_at))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_empty_portfolio_points_at_import_and_gold() {
    let router = router(open_in_memory().await.unwrap());
    let page = body(&router, "/", false).await;
    assert!(page.contains("Nothing to show yet"), "{page}");
    assert!(
        page.contains("Import your XTB account history, or add a gold coin"),
        "{page}"
    );
    assert!(page.contains(r#"<a href="/import""#), "{page}");
    assert!(page.contains(r#"<a href="/gold""#), "{page}");
    assert!(page.contains(">0,00<span"), "{page}");
    // Nothing was valued: no banner, no asset cards, and no price source was asked.
    assert!(!page.contains("isn’t responding"), "{page}");
    assert!(!page.contains("earned most"), "{page}");
    assert!(page.contains("No prices yet"), "{page}");

    // The period switch gets the same state as its panel.
    let panel = body(&router, "/?period=1M", true).await;
    assert!(panel.starts_with(r#"<div id="overview""#), "{panel}");
    assert!(panel.contains("Nothing to show yet"), "{panel}");
}

#[tokio::test]
async fn a_source_that_is_down_leaves_the_last_known_price_marked_stale() {
    // Wednesday's gold price was fetched at 20:00 UTC; at noon on Thursday NBP is asked again
    // and does not answer.
    let pool = open_in_memory().await.unwrap();
    coin(&pool).await;
    gold_close(&pool, "2026-10-14", "2026-10-14T20:00:00Z").await;
    let router = router(pool);

    let page = body(&router, "/", false).await;
    // 500 x 3,11 x 1 = 1555 zł, against 1500 paid.
    assert!(page.contains("1 555,00<span"), "{page}");
    // The banner, with Retry reloading the same view (the time is the Mac's local time).
    assert!(page.contains("NBP isn’t responding."), "{page}");
    assert!(
        page.contains("Gold uses its last known price from 14 Oct, "),
        "{page}"
    );
    assert!(page.contains(">Retry</a>"), "{page}");
    // The card: the Stale chip and when the price is from, on desktop and phone.
    assert_eq!(page.matches(">Stale</span>").count(), 2, "{page}");
    assert_eq!(page.matches("Last price 14 Oct, ").count(), 2, "{page}");
    // The sidebar: "Couldn't update prices" with Retry instead of the time.
    assert!(page.contains("Couldn’t update prices"), "{page}");
    assert!(!page.contains("Prices updated"), "{page}");

    // Retry keeps the period and the benchmark setting.
    let six = body(&router, "/?period=6M&bench=off", false).await;
    assert!(six.contains(r#"href="/?period=6M&#38;bench=off""#), "{six}");

    // Allocation values the same holdings, so it says the same.
    let allocation = body(&router, "/allocation", false).await;
    assert!(allocation.contains("NBP isn’t responding."), "{allocation}");
    assert!(
        allocation.contains("Couldn’t update prices"),
        "{allocation}"
    );
    assert!(allocation.contains(r#"href="/allocation""#), "{allocation}");
}

#[tokio::test]
async fn a_price_older_than_the_gap_limit_is_missing_not_stale() {
    // A gold price from 1 Oct is 14 days old on the 15th, past the 10-day limit: no value.
    let pool = open_in_memory().await.unwrap();
    coin(&pool).await;
    gold_close(&pool, "2026-10-01", "2026-10-01T20:00:00Z").await;
    let page = body(&router(pool), "/", false).await;
    assert!(!page.contains("isn’t responding"), "{page}");
    assert!(!page.contains(">Stale</span>"), "{page}");
    assert!(page.contains("Some prices are missing"), "{page}");
    // The sync status still says the update failed.
    assert!(page.contains("Couldn’t update prices"), "{page}");
}

#[tokio::test]
async fn a_first_contribution_today_is_a_one_point_chart() {
    let pool = open_in_memory().await.unwrap();
    let deposit = NewTransaction {
        kind: "deposit",
        executed_at: d("2026-10-15").and_hms_opt(9, 0, 0).unwrap(),
        asset: None,
        quantity: None,
        unit_price: None,
        amount: Decimal::from(500),
        currency: "PLN".into(),
        external_id: "d1".into(),
        note: None,
    };
    let account = Account {
        name: "XTB IKZE",
        kind: "ikze",
        provider: "xtb",
        currency: "PLN",
    };
    insert_transactions(&pool, account, &[], "xtb", &[deposit])
        .await
        .unwrap();
    let page = body(&router(pool), "/", false).await;
    assert!(
        page.contains("One data point so far. The chart fills in from tomorrow."),
        "{page}"
    );
    assert!(!page.contains(r#"viewBox="0 0 1000 300""#), "{page}");
    // Cash in PLN needs no price: nothing failed.
    assert!(!page.contains("Couldn’t update prices"), "{page}");
}

#[tokio::test]
async fn the_panel_carries_its_loading_skeleton() {
    let pool = open_in_memory().await.unwrap();
    coin(&pool).await;
    gold_close(&pool, "2026-10-15", "2026-10-15T11:00:00Z").await;
    let panel = body(&router(pool), "/?period=1M", true).await;
    assert!(
        panel.contains(
            r#"<div class="ov-skel" role="status" aria-busy="true" aria-label="Loading portfolio""#
        ),
        "{panel}"
    );
    assert!(panel.contains("Updating prices…"), "{panel}");
    // Every control that re-renders the panel shows it, and the sync status's spinner.
    assert_eq!(
        panel
            .matches(r##"hx-indicator="#overview, .sync""##)
            .count(),
        11,
        "{panel}"
    );
}

#[tokio::test]
async fn the_sync_status_is_fresh_for_six_hours_then_stale() {
    // The Import page values nothing, so the status is only the last fetch's age.
    let pool = open_in_memory().await.unwrap();
    gold_close(&pool, "2026-10-15", "2026-10-15T11:00:00Z").await;
    let fresh = body(&router(pool.clone()), "/import", false).await;
    assert!(fresh.contains("Prices updated "), "{fresh}");
    assert!(fresh.contains("Updating prices…"), "{fresh}");

    let pool = open_in_memory().await.unwrap();
    gold_close(&pool, "2026-10-14", "2026-10-15T05:00:00Z").await;
    let stale = body(&router(pool), "/import", false).await;
    assert!(stale.contains("Prices from "), "{stale}");
    assert!(stale.contains("color: var(--warn)"), "{stale}");
}
