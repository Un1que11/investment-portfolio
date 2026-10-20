//! The Allocation page end to end, on the design's holdings: VWCE 6100, BTC 2380, ETH 920 and a
//! coin worth 2100 (11 500 zł). Prices and NBP rates are put in the caches first, so no source
//! is asked (they all point at a closed port).

use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Extension, Router};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_core::AssetClass;
use portfolio_core::config::Config;
use portfolio_core::fx::FxRate;
use portfolio_core::price::DailyClose;
use portfolio_db::fx::upsert_rates;
use portfolio_db::gold::{NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::prices::{ensure_asset, upsert_closes};
use portfolio_db::transactions::{Account, Asset, NewTransaction, insert_transactions};
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::{AppState, Market, app};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tower::ServiceExt;

const CONFIG: &str = r#"
[targets]
vwce = 55
crypto = 25
gold = 20
[targets.crypto_split]
btc = 75
eth = 25
[paused]
crypto = true
gold = true
[ikze.limits]
2026 = "11304.00"
[tax]
rate_percent = 12
[gold]
buyback_factor = "0.95"
"#;

const TODAY: &str = "2026-10-16";
const BOUGHT: &str = "2026-10-01";
const DOWN: &str = "http://127.0.0.1:9";

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn row(kind: &'static str, asset: Option<&'static str>, qty: &str, amount: &str) -> NewTransaction {
    let quantity = (!qty.is_empty()).then(|| dec(qty));
    NewTransaction {
        kind,
        executed_at: d(BOUGHT).and_hms_opt(12, 0, 0).unwrap(),
        asset,
        quantity,
        unit_price: quantity.map(|q| dec(amount) / q),
        amount: dec(amount),
        currency: String::new(),
        external_id: format!("{kind}-{}", asset.unwrap_or("cash")),
        note: None,
    }
}

fn in_currency(mut t: NewTransaction, currency: &str) -> NewTransaction {
    t.currency = currency.into();
    t
}

/// 5652 PLN into the IKZE and 12.5 VWCE bought with it; 825 EUR to Kraken for 0.01 BTC and
/// 0.1 ETH; one coin quoted at 2100 by hand.
async fn holdings(pool: &SqlitePool) {
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
            name: "Vanguard FTSE All-World",
            class: AssetClass::Vwce,
            currency: "EUR",
        }],
        "xtb",
        &[
            in_currency(row("deposit", None, "", "5652"), "PLN"),
            in_currency(row("buy", Some("VWCE"), "12.5", "5652"), "PLN"),
        ],
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
        &[
            Asset {
                symbol: "BTC",
                name: "Bitcoin",
                class: AssetClass::Crypto,
                currency: "EUR",
            },
            Asset {
                symbol: "ETH",
                name: "Ether",
                class: AssetClass::Crypto,
                currency: "EUR",
            },
        ],
        "kraken",
        &[
            in_currency(row("deposit", None, "", "825"), "EUR"),
            in_currency(row("buy", Some("BTC"), "0.01", "595"), "EUR"),
            in_currency(row("buy", Some("ETH"), "0.1", "230"), "EUR"),
        ],
    )
    .await
    .unwrap();
    add_coin(
        pool,
        &NewCoin {
            label: None,
            purchased_on: d("2026-05-12"),
            purchase_price_pln: dec("1640"),
            fine_weight_g: dec("3.11"),
            manual_quote_pln: Some(dec("2100")),
        },
    )
    .await
    .unwrap();
}

/// Today's closes at 4 PLN per EUR: VWCE 122 EUR (12.5 x 122 x 4 = 6100), BTC 59 500 EUR
/// (0.01 -> 2380), ETH 2300 EUR (0.1 -> 920); gold 400 PLN a gram (the coin's own quote wins).
async fn prices(pool: &SqlitePool, with_vwce: bool) {
    let rates: Vec<FxRate> = [BOUGHT, TODAY]
        .into_iter()
        .map(|day| FxRate {
            currency: "EUR".into(),
            effective_date: d(day),
            mid: dec("4"),
            table_no: "1/A/NBP/2026".into(),
        })
        .collect();
    upsert_rates(pool, &rates).await.unwrap();
    let mut closes = vec![
        (
            "BTC",
            "Bitcoin",
            AssetClass::Crypto,
            "EUR",
            "kraken",
            "59500",
        ),
        ("ETH", "Ether", AssetClass::Crypto, "EUR", "kraken", "2300"),
        (
            "XAU_G",
            "Gold, PLN per gram (NBP)",
            AssetClass::Gold,
            "PLN",
            "nbp",
            "400",
        ),
    ];
    if with_vwce {
        closes.push((
            "VWCE",
            "Vanguard FTSE All-World",
            AssetClass::Vwce,
            "EUR",
            "alphavantage",
            "122",
        ));
    }
    for (symbol, name, class, currency, source, close) in closes {
        let id = ensure_asset(pool, symbol, name, class, currency)
            .await
            .unwrap();
        let close = DailyClose {
            date: d(TODAY),
            close: dec(close),
        };
        upsert_closes(pool, id, currency, source, &[close], Utc::now())
            .await
            .unwrap();
    }
}

fn router(pool: SqlitePool, with_av: bool) -> Router {
    let market = Market {
        nbp: NbpClient::with_base_url(DOWN).unwrap(),
        kraken: KrakenClient::with_base_url(DOWN).unwrap(),
        alphavantage: with_av.then(|| AlphaVantageClient::new(DOWN, "test").unwrap()),
        gold_buyback_factor: dec("0.95"),
    };
    let state = AppState {
        pool,
        market: Arc::new(market),
        today: Arc::new(|| d(TODAY)),
        now: Arc::new(Utc::now),
    };
    app(state).layer(Extension(Arc::new(Config::from_toml(CONFIG).unwrap())))
}

async fn design() -> Router {
    let pool = open_in_memory().await.unwrap();
    holdings(&pool).await;
    prices(&pool, true).await;
    router(pool, true)
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
        .replace('\u{2212}', "-")
}

#[tokio::test]
async fn the_page_of_the_design() {
    let page = body(&design().await, "/allocation", false).await;

    assert!(
        page.contains("<title>Allocation · Portfel</title>"),
        "{page}"
    );
    assert!(
        page.contains(r#"<a href="/allocation" aria-current="page""#),
        "{page}"
    );
    assert!(
        page.contains("Total 11 500,00 zł · no selling, top-ups only"),
        "{page}"
    );
    assert!(
        page.contains(r#"aria-label="Actual: VWCE 53,0%, Crypto 28,7%, Gold 18,3%""#),
        "{page}"
    );
    assert!(
        page.contains(r#"aria-label="Target: VWCE 55%, Crypto 25%, Gold 20%""#),
        "{page}"
    );
    // The drift table: VWCE 6100 - 6325, crypto 3300 - 2875, gold 2100 - 2300; inside crypto
    // BTC 2380 - 2475 and ETH 920 - 825.
    for cell in [
        "▼ -2,0 pp",
        "-225,00 zł",
        "▲ +3,7 pp",
        "+425,00 zł",
        "▼ -1,7 pp",
        "-200,00 zł",
        "-95,00 zł",
        "+95,00 zł",
        ">72,1%<",
        ">27,9%<",
    ] {
        assert!(page.contains(cell), "{cell}\n{page}");
    }
    assert!(page.contains("Buying · IKZE"), "{page}");
    assert!(page.contains("Purchases paused · Kraken"), "{page}");
    assert!(page.contains("Purchases paused · coins"), "{page}");
    assert!(page.contains(r#"BTC <span style="color: var(--text-3)">· of crypto</span>"#));
    // The plan for the default 2000: all of it to VWCE, the paused classes say what they need.
    assert!(page.contains(r#"value="2 000""#), "{page}");
    assert!(page.contains("+2 000,00 zł"), "{page}");
    assert!(page.contains("Paused · would need 75,00 zł"), "{page}");
    assert!(page.contains("Paused · would need 600,00 zł"), "{page}");
    assert!(
        page.contains("VWCE 60,0% · Crypto 24,4% · Gold 15,6%"),
        "{page}"
    );
    assert!(
        page.contains("Fits your remaining IKZE room for 2026 (5 652,00 zł)."),
        "{page}"
    );
    assert!(page.contains(r#"href="/allocation?amount=942""#), "{page}");
    assert!(!page.contains(" checked"), "{page}");
}

#[tokio::test]
async fn the_plan_alone_for_htmx_with_the_paused_classes() {
    let router = design().await;
    let panel = body(&router, "/allocation?amount=5000&paused=on", true).await;

    assert!(panel.starts_with(r#"<section id="topup""#), "{panel}");
    assert!(!panel.contains("<html"), "{panel}");
    assert!(panel.contains(" checked"), "{panel}");
    assert!(panel.contains(r#"value="5 000""#), "{panel}");
    // 5000 more: the base is 16 500 and the needs are VWCE 9075 - 6100 = 2975, crypto 4125 -
    // 3300 = 825, gold 3300 - 2100 = 1200, together exactly 5000. Crypto after: 4125, BTC needs
    // 3093,75 - 2380 = 713,75 and ETH 1031,25 - 920 = 111,25.
    for text in [
        "+2 975,00 zł",
        "+825,00 zł",
        "+1 200,00 zł",
        "BTC 713,75 zł · ETH 111,25 zł",
        "Whole coins only (~2 100 zł) — set aside",
        "VWCE 55,0% · Crypto 25,0% · Gold 20,0%",
    ] {
        assert!(panel.contains(text), "{text}\n{panel}");
    }
    assert!(
        panel.contains(r#"href="/allocation?amount=942&#38;paused=on""#),
        "{panel}"
    );

    let typo = body(&router, "/allocation?amount=abc", true).await;
    assert!(
        typo.contains("Enter an amount in złoty, such as 2 000."),
        "{typo}"
    );
    assert!(typo.contains(r#"value="abc""#), "{typo}");
}

#[tokio::test]
async fn a_missing_price_is_said_instead_of_shares() {
    // No Alpha Vantage key: VWCE has no price, so neither the shares nor a plan are shown.
    let pool = open_in_memory().await.unwrap();
    holdings(&pool).await;
    prices(&pool, false).await;
    let page = body(&router(pool, false), "/allocation", false).await;

    assert!(
        page.contains(
            "No price for VWCE right now, so the shares and the top-up plan would be wrong"
        ),
        "{page}"
    );
    assert!(page.contains("No selling, top-ups only"), "{page}");
    assert!(!page.contains("Total "), "{page}");
    assert!(!page.contains("Top-up plan"), "{page}");
}

#[tokio::test]
async fn with_nothing_held_the_plan_still_works() {
    let page = body(
        &router(open_in_memory().await.unwrap(), false),
        "/allocation",
        false,
    )
    .await;

    assert!(
        page.contains("Nothing is held yet, so there is no allocation to compare"),
        "{page}"
    );
    // 2000 into an empty portfolio: VWCE needs 1100 of it, the rest follows its weight; the
    // whole limit of 11 304 is still there.
    assert!(page.contains("+2 000,00 zł"), "{page}");
    assert!(
        page.contains("VWCE 100,0% · Crypto 0,0% · Gold 0,0%"),
        "{page}"
    );
    assert!(
        page.contains("Fits your remaining IKZE room for 2026 (11 304,00 zł)."),
        "{page}"
    );
}

#[tokio::test]
async fn a_huge_coin_stored_before_the_gold_caps_leaves_the_pages_up() {
    // 10^27 g x 400 zł/g is past Decimal. Such a row can only predate the Gold form's caps;
    // its value is missing, as without a price, instead of a panic on every page.
    let pool = open_in_memory().await.unwrap();
    prices(&pool, true).await;
    let coin = NewCoin {
        label: None,
        purchased_on: d("2026-05-12"),
        purchase_price_pln: dec("1000"),
        fine_weight_g: dec("1000000000000000000000000000"),
        manual_quote_pln: None,
    };
    add_coin(&pool, &coin).await.unwrap();
    let router = router(pool, false);
    let overview = body(&router, "/", false).await;
    assert!(overview.contains("—"), "{overview}");
    let allocation = body(&router, "/allocation", false).await;
    assert!(
        allocation.contains("No price for GOLD_COINS right now"),
        "{allocation}"
    );
}
