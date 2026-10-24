//! The Gold page (screens/06) through the router: an in-memory database, a cached NBP gold
//! price, and nothing listening for any source.

use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::{Extension, Router};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_core::AssetClass;
use portfolio_core::config::Config;
use portfolio_core::price::DailyClose;
use portfolio_db::gold::{Coin, NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::prices::{ensure_asset, upsert_closes};
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::{AppState, Market, app};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tower::ServiceExt;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TODAY: &str = "2026-10-15";
const DOWN: &str = "http://127.0.0.1:9";

fn config(paused: &str) -> Config {
    Config::from_toml(&format!(
        r#"
        [targets]
        vwce = 55
        crypto = 25
        gold = 20
        [targets.crypto_split]
        btc = 75
        eth = 25
        [paused]
        {paused}
        [ikze.limits]
        2026 = "11304.00"
        [tax]
        rate_percent = 12
        [gold]
        buyback_factor = "0.95"
        "#
    ))
    .unwrap()
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

/// Two coins: one valued at NBP, one at its dealer quote of 1300.
async fn coins(pool: &SqlitePool) -> Vec<Coin> {
    let mut added = Vec::new();
    for (label, paid, quote) in [
        (Some("Philharmoniker 1/10 oz"), "1100", None),
        (None, "1250", Some("1300")),
    ] {
        let coin = NewCoin {
            label: label.map(str::to_owned),
            purchased_on: d("2025-06-12"),
            purchase_price_pln: dec(paid),
            fine_weight_g: dec("3.11"),
            manual_quote_pln: quote.map(dec),
        };
        added.push(add_coin(pool, &coin).await.unwrap());
    }
    added
}

/// NBP's gold at 400 PLN a gram today, already in the cache.
async fn gold_price(pool: &SqlitePool) {
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
        date: d(TODAY),
        close: dec("400"),
    };
    upsert_closes(pool, id, "PLN", "nbp", &[close], Utc::now())
        .await
        .unwrap();
}

fn router(pool: SqlitePool, config: Config) -> Router {
    router_with_nbp(pool, config, DOWN)
}

fn router_with_nbp(pool: SqlitePool, config: Config, nbp: &str) -> Router {
    let market = Market {
        nbp: NbpClient::with_base_url(nbp).unwrap(),
        kraken: KrakenClient::with_base_url(DOWN).unwrap(),
        alphavantage: None,
        gold_buyback_factor: dec("0.95"),
    };
    let state = AppState {
        pool,
        market: Arc::new(market),
        today: Arc::new(|| d(TODAY)),
        now: Arc::new(Utc::now),
    };
    app(state).layer(Extension(Arc::new(config)))
}

async fn send(router: &Router, request: Request<Body>) -> String {
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace('\u{a0}', " ")
        .replace('\u{2212}', "-")
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn htmx_post(uri: &str, form: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("HX-Request", "true")
        .body(Body::from(form.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn the_page_of_the_design() {
    let pool = open_in_memory().await.unwrap();
    coins(&pool).await;
    gold_price(&pool).await;
    let page = send(&router(pool, config("gold = true")), get("/gold")).await;

    assert!(page.contains("<title>Gold · Portfel</title>"), "{page}");
    assert!(page.contains("Purchases paused"), "{page}");
    // 400 x 3,11 x 0,95 = 1181,80 a coin.
    assert!(
        page.contains("Valued at NBP spot × 0,95 · 1 181,80 zł per 1/10 oz · 15.10.2026"),
        "{page}"
    );
    // Tiles: 1181,80 + 1300 = 2481,80 against 1100 + 1250 = 2350 paid: +131,80, +5,6%.
    for text in [
        ">2 481,80 zł<",
        ">2 350,00 zł<",
        "▲ +131,80 zł · +5,6%",
        "2 coins · 0,2 oz",
    ] {
        assert!(page.contains(text), "{text}\n{page}");
    }
    // Rows: 1181,80 - 1100 = +81,80 (+7,4%); the quoted coin 1300 - 1250 = +50 (+4,0%).
    for text in [
        "Philharmoniker 1/10 oz<span",
        "0,1 oz · 3,11 g fine gold",
        "12.06.2025",
        "▲ +81,80 zł<div style=\"font-size: 12px\">+7,4%",
        "1 300,00 zł<div style=\"font-size: 12px; color: var(--text-3)\">dealer quote</div>",
        "▲ +50,00 zł<div style=\"font-size: 12px\">+4,0%",
        r#"aria-label="Remove 1/10 oz coin bought 12.06.2025""#,
    ] {
        assert!(page.contains(text), "{text}\n{page}");
    }
    assert!(!page.contains("aria-invalid"), "{page}");
}

/// NBP's gold at 380 PLN a gram on 12.06.2025, the coins' purchase day.
async fn purchase_day_price(pool: &SqlitePool) {
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
        date: d("2025-06-12"),
        close: dec("380"),
    };
    // Fetched back then, so it does not count as a recent fetch of NBP's series.
    let fetched_at = "2025-06-12T18:00:00Z".parse().unwrap();
    upsert_closes(pool, id, "PLN", "nbp", &[close], fetched_at)
        .await
        .unwrap();
}

#[tokio::test]
async fn each_coin_shows_the_premium_paid_over_melt() {
    let pool = open_in_memory().await.unwrap();
    coins(&pool).await;
    gold_price(&pool).await;
    purchase_day_price(&pool).await;
    let page = send(&router(pool, config("")), get("/gold")).await;

    // Melt on 12.06.2025: 380 x 3,11 = 1181,80 a coin. Paid 1100: -81,80, 81,80 / 1181,80 =
    // 6,92% -> -6,9%. Paid 1250: +68,20, 68,20 / 1181,80 = 5,77% -> +5,8%.
    // Together 2350 - 2363,60 = -13,60, 13,60 / 2363,60 = 0,58% -> -0,6%.
    for text in [
        "1 100,00 zł<div style=\"font-size: 12px; color: var(--text-3)\" title=\"Price paid against NBP melt value on the purchase day\">-81,80 zł under melt · -6,9%</div>",
        ">+68,20 zł over melt · +5,8%</div>",
        ">-13,60 zł under melt · -0,6% at purchase</div>",
    ] {
        assert!(page.contains(text), "{text}\n{page}");
    }
    // The valuation and P&L still use today's price: 1181,80 at 400 x 3,11 x 0,95.
    assert!(page.contains("▲ +81,80 zł"), "{page}");
}

#[tokio::test]
async fn without_a_purchase_day_price_there_is_no_premium() {
    let pool = open_in_memory().await.unwrap();
    coins(&pool).await;
    gold_price(&pool).await;
    // NBP is not listening: the purchase day cannot be fetched, the page still renders.
    let page = send(&router(pool, config("")), get("/gold")).await;
    assert!(!page.contains("melt ·"), "{page}");
    assert!(page.contains(">2 481,80 zł<"), "{page}");
}

/// Adds a 1/10 oz coin paid `paid` on `day`.
async fn coin_on(pool: &SqlitePool, day: &str, paid: &str) {
    let coin = NewCoin {
        label: None,
        purchased_on: d(day),
        purchase_price_pln: dec(paid),
        fine_weight_g: dec("3.11"),
        manual_quote_pln: None,
    };
    add_coin(pool, &coin).await.unwrap();
}

/// An NBP that fails every request, counting them.
async fn failing_nbp() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    server
}

async fn nbp_requests(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

#[tokio::test]
async fn after_nbp_fails_cached_purchase_days_still_show_their_premium() {
    let pool = open_in_memory().await.unwrap();
    // The oldest coin (10.01.2025) is on a day not in the cache, so it is looked up first; the
    // two coins of 12.06.2025 (380 zł/g) are on a cached day.
    coin_on(&pool, "2025-01-10", "1700").await;
    coins(&pool).await;
    gold_price(&pool).await;
    purchase_day_price(&pool).await;
    let nbp = failing_nbp().await;
    let page = send(&router_with_nbp(pool, config(""), &nbp.uri()), get("/gold")).await;

    // One failed request for 10.01.2025; 12.06.2025 is still answered from the cache:
    // 1100 - 1181,80 = -81,80 (-6,9%), 1250 - 1181,80 = +68,20 (+5,8%).
    assert_eq!(nbp_requests(&nbp).await, 1);
    assert!(
        page.contains(">-81,80 zł under melt · -6,9%</div>"),
        "{page}"
    );
    assert!(
        page.contains(">+68,20 zł over melt · +5,8%</div>"),
        "{page}"
    );
    // The uncached coin has no premium, so there is no total.
    assert!(!page.contains("at purchase</div>"), "{page}");
}

#[tokio::test]
async fn when_todays_price_fails_purchase_days_are_not_asked_for() {
    let pool = open_in_memory().await.unwrap();
    coins(&pool).await;
    coin_on(&pool, "2026-10-01", "1700").await;
    purchase_day_price(&pool).await;
    let nbp = failing_nbp().await;
    let page = send(&router_with_nbp(pool, config(""), &nbp.uri()), get("/gold")).await;

    // Today's lookup asks NBP and fails; 01.10.2026 is not asked for again, and 12.06.2025
    // comes from the cache.
    assert_eq!(nbp_requests(&nbp).await, 1);
    assert!(page.contains("No NBP gold price right now"), "{page}");
    assert!(
        page.contains(">-81,80 zł under melt · -6,9%</div>"),
        "{page}"
    );
}

#[tokio::test]
async fn a_weekend_purchase_uses_fridays_price_and_one_lookup_per_day() {
    // Recorded NBP gold prices 15-31.12.2025: Friday 19.12 is 498,88 zł/g, Monday 22.12 is 500,23.
    const XMAS_2025: &str =
        include_str!("../../sources/tests/fixtures/nbp/gold_2025-12-15_2025-12-31.json");
    let load = |paid: &'static [&'static str]| async move {
        let pool = open_in_memory().await.unwrap();
        for p in paid {
            coin_on(&pool, "2025-12-20", p).await;
        }
        gold_price(&pool).await;
        let nbp = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(
                r"^/cenyzlota/\d{4}-\d{2}-\d{2}/\d{4}-\d{2}-\d{2}/$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(XMAS_2025))
            .mount(&nbp)
            .await;
        let page = send(&router_with_nbp(pool, config(""), &nbp.uri()), get("/gold")).await;
        (page, nbp_requests(&nbp).await)
    };

    // Saturday 20.12.2025: melt 498,88 x 3,11 = 1551,5168. Paid 1630,00: +78,4832 -> +78,48 zł,
    // 78,4832 / 1551,5168 = 5,058% -> +5,1%.
    let (page, one_coin) = load(&["1630"]).await;
    assert!(
        page.contains(">+78,48 zł over melt · +5,1%</div>"),
        "{page}"
    );
    assert_eq!(one_coin, 1);

    // A second coin on the same day asks NBP no more than one coin does.
    let (page, two_coins) = load(&["1630", "1500"]).await;
    assert_eq!(two_coins, 1);
    // 1500 - 1551,5168 = -51,5168 -> -51,52 zł; 51,5168 / 1551,5168 = 3,32% -> -3,3%.
    assert!(
        page.contains(">-51,52 zł under melt · -3,3%</div>"),
        "{page}"
    );
}

#[tokio::test]
async fn without_an_nbp_price_only_quoted_coins_have_a_value() {
    let pool = open_in_memory().await.unwrap();
    coins(&pool).await;
    let page = send(&router(pool, config("")), get("/gold")).await;

    assert!(!page.contains("Purchases paused"), "{page}");
    assert!(page.contains("No NBP gold price right now"), "{page}");
    // The total cannot be known, the quoted coin still is: +50.
    assert!(page.contains("letter-spacing: -0.02em\">—</div>"), "{page}");
    assert!(page.contains("▲ +50,00 zł"), "{page}");
    assert!(!page.contains("+81,80"), "{page}");
}

#[tokio::test]
async fn adding_and_removing_through_htmx_refresh_the_tiles() {
    let pool = open_in_memory().await.unwrap();
    let added = coins(&pool).await;
    gold_price(&pool).await;
    let router = router(pool, config("gold = true"));

    // A third coin at a loss: 1181,80 - 1300 = -118,20. Totals: 3663,60 against 3650: +13,60,
    // 13,60 / 3650 = 0,37% -> +0,4%.
    let panel = send(
        &router,
        htmx_post(
            "/gold/coins",
            "label=&purchased_on=2026-10-01&purchase_price=1+300%2C00&fine_weight=3%2C11&manual_quote=",
        ),
    )
    .await;
    assert!(panel.starts_with(r#"<div id="gold-panel""#), "{panel}");
    for text in [
        "▼ -118,20 zł<div style=\"font-size: 12px\">-9,1%",
        ">3 663,60 zł<",
        "▲ +13,60 zł · +0,4%",
        "3 coins · 0,3 oz",
        r#"value="2026-10-15""#,
    ] {
        assert!(panel.contains(text), "{text}\n{panel}");
    }

    // Removing the quoted coin: 1181,80 x 2 = 2363,60 against 2400: -36,40, -1,5%.
    let uri = format!("/gold/coins/{}/delete", added[1].id);
    let panel = send(&router, htmx_post(&uri, "")).await;
    for text in [">2 363,60 zł<", "▼ -36,40 zł · -1,5%", "2 coins · 0,2 oz"] {
        assert!(panel.contains(text), "{text}\n{panel}");
    }
}

#[tokio::test]
async fn a_bad_field_is_marked_and_explained() {
    let pool = open_in_memory().await.unwrap();
    gold_price(&pool).await;
    let router = router(pool, config(""));

    let panel = send(
        &router,
        htmx_post(
            "/gold/coins",
            "label=&purchased_on=2026-10-15&purchase_price=2050&fine_weight=0&manual_quote=x",
        ),
    )
    .await;
    // The weight and quote errors open their part of the form.
    assert!(panel.contains("<details open"), "{panel}");
    assert!(
        panel.contains(r#"value="0" aria-invalid="true" aria-describedby="coin-weight-msg""#),
        "{panel}"
    );
    assert!(
        panel.contains(
            r#"<span id="coin-weight-msg" class="field-error">Must be more than 0</span>"#
        ),
        "{panel}"
    );
    assert!(
        panel.contains(
            r#"<span id="coin-quote-msg" class="field-error">Enter an amount like 2 050,00</span>"#
        ),
        "{panel}"
    );
    // The price was fine and is kept as typed.
    assert!(
        panel.contains(r#"placeholder="2 050,00" value="2050" style="#),
        "{panel}"
    );
    assert!(panel.contains("No coins yet"), "{panel}");
}

#[tokio::test]
async fn a_huge_amount_or_weight_is_turned_down_instead_of_breaking_the_page() {
    // A weight of 10^27 g x 400 zł/g is past Decimal's ~7,9 x 10^28, and two coins at 5 x 10^28
    // zł each overflow the "Paid" tile: once stored, either would panic every later /gold.
    let pool = open_in_memory().await.unwrap();
    gold_price(&pool).await;
    let router = router(pool, config(""));
    for (form, field, error) in [
        (
            "purchase_price=1000&fine_weight=1000000000000000000000000000",
            "coin-weight-msg",
            "Enter at most 100 000 g",
        ),
        (
            "purchase_price=50000000000000000000000000000&fine_weight=3,11",
            "coin-price-msg",
            "Enter at most 1 000 000 000 zł",
        ),
        (
            "purchase_price=1000&fine_weight=3,11&manual_quote=50000000000000000000000000000",
            "coin-quote-msg",
            "Enter at most 1 000 000 000 zł",
        ),
    ] {
        let body = format!("label=&purchased_on=2026-10-01&{form}");
        let panel = send(&router, htmx_post("/gold/coins", &body)).await;
        let msg = format!(r#"<span id="{field}" class="field-error">{error}</span>"#);
        assert!(panel.contains(&msg), "{msg}\n{panel}");
        assert!(panel.contains("No coins yet"), "{panel}");
    }
    let page = send(&router, get("/gold")).await;
    assert!(page.contains("No coins yet"), "{page}");

    // The caps themselves are still taken: 100 000 g x 400 x 0,95 = 38 000 000 zł.
    let panel = send(
        &router,
        htmx_post(
            "/gold/coins",
            "label=&purchased_on=2026-10-01&purchase_price=1000000000&fine_weight=100000&manual_quote=",
        ),
    )
    .await;
    assert!(panel.contains(">38 000 000,00 zł<"), "{panel}");
}

#[tokio::test]
async fn a_huge_coin_stored_before_the_caps_still_renders_and_can_be_removed() {
    // Rows written before the form had caps: 10^27 g x 400 zł/g overflows Decimal, and two
    // coins paid 5 x 10^28 zł each overflow the "Paid" sum. The page shows "—" where a number
    // cannot be had and keeps the remove buttons.
    let pool = open_in_memory().await.unwrap();
    gold_price(&pool).await;
    let mut added = Vec::new();
    for (paid, grams) in [
        ("1000", "1000000000000000000000000000"),
        ("50000000000000000000000000000", "3.11"),
        ("50000000000000000000000000000", "3.11"),
    ] {
        let coin = NewCoin {
            label: None,
            purchased_on: d("2025-06-12"),
            purchase_price_pln: dec(paid),
            fine_weight_g: dec(grams),
            manual_quote_pln: None,
        };
        added.push(add_coin(&pool, &coin).await.unwrap());
    }
    let router = router(pool, config(""));
    let page = send(&router, get("/gold")).await;
    let tile = |label: &str| {
        format!(
            r#"{label}</div><div style="font-size: 26px; font-weight: 500; letter-spacing: -0.02em">—</div>"#
        )
    };
    assert!(page.contains(&tile("Valuation")), "{page}");
    assert!(page.contains(&tile("Paid")), "{page}");
    assert!(page.contains("3 coins"), "{page}");
    for coin in &added {
        let action = format!("/gold/coins/{}/delete", coin.id);
        assert!(page.contains(&action), "{action}\n{page}");
    }

    // Removing the heavy coin leaves the two that overflow only when added up.
    let uri = format!("/gold/coins/{}/delete", added[0].id);
    let panel = send(&router, htmx_post(&uri, "")).await;
    assert!(panel.contains("2 coins · 0,2 oz"), "{panel}");
    assert!(panel.contains(&tile("Paid")), "{panel}");
    for coin in &added[1..] {
        let uri = format!("/gold/coins/{}/delete", coin.id);
        send(&router, htmx_post(&uri, "")).await;
    }
    let page = send(&router, get("/gold")).await;
    assert!(page.contains("No coins yet"), "{page}");
}
