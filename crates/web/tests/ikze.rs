//! The IKZE page end to end. It needs no price, only the NBP rate of a deposit in another
//! currency: rates come from the cache, and NBP itself is a closed port (down).

use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Extension, Router};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_core::config::Config;
use portfolio_core::fx::FxRate;
use portfolio_db::fx::upsert_rates;
use portfolio_db::open_in_memory;
use portfolio_db::transactions::{Account, NewTransaction, insert_transactions};
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
[ikze.limits]
2026 = "11304.00"
[tax]
rate_percent = 12
[gold]
buyback_factor = "0.95"
"#;

const IKZE: Account<'static> = Account {
    name: "XTB IKZE",
    kind: "ikze",
    provider: "xtb",
    currency: "PLN",
};

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn deposit(day: &str, amount: &str, currency: &str, id: &str) -> NewTransaction {
    NewTransaction {
        kind: "deposit",
        executed_at: d(day).and_hms_opt(12, 0, 0).unwrap(),
        asset: None,
        quantity: None,
        unit_price: None,
        amount: Decimal::from_str(amount).unwrap(),
        currency: currency.into(),
        external_id: id.into(),
        note: None,
    }
}

/// 942 PLN into the IKZE on the 2nd of each month May–Oct 2026, as in screens/05.
async fn design_year(pool: &SqlitePool) {
    let rows: Vec<NewTransaction> = (5..=10)
        .map(|m| deposit(&format!("2026-{m:02}-02"), "942", "PLN", &format!("d{m}")))
        .collect();
    insert_transactions(pool, IKZE, &[], "xtb", &rows)
        .await
        .unwrap();
}

fn router(pool: SqlitePool, today: &'static str) -> Router {
    let market = Market {
        nbp: NbpClient::with_base_url("http://127.0.0.1:9").unwrap(),
        kraken: KrakenClient::with_base_url("http://127.0.0.1:9").unwrap(),
        alphavantage: None,
        gold_buyback_factor: Decimal::ONE,
    };
    let state = AppState {
        pool,
        market: Arc::new(market),
        today: Arc::new(move || d(today)),
        now: Arc::new(Utc::now),
    };
    app(state).layer(Extension(Arc::new(Config::from_toml(CONFIG).unwrap())))
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
async fn the_page_of_the_design() {
    let pool = open_in_memory().await.unwrap();
    design_year(&pool).await;
    let page = body(&router(pool, "2026-10-16"), "/ikze", false).await;

    assert!(page.contains("<title>IKZE · Portfel</title>"), "{page}");
    assert!(
        page.contains(r#"<a href="/ikze" aria-current="page""#),
        "{page}"
    );
    assert!(page.contains("tax year 2026"), "{page}");
    // 6 x 942 = 5652 of 11 304: half used, half left (hatched).
    assert!(page.contains(">5 652,00<span"), "{page}");
    assert!(page.contains("of 11 304,00 zł"), "{page}");
    assert!(page.contains(r#"aria-valuenow="5652""#), "{page}");
    assert!(page.contains("50,0% used"), "{page}");
    assert!(
        page.contains("left: 50%; top: 0; bottom: 0; width: 50%"),
        "{page}"
    );
    // 16 Oct: 76 days left; 942 a month reaches 7536; 5652 / 2 = 2826 in Nov and Dec.
    assert!(
        page.contains("Top up before 31 Dec · 76 days left"),
        "{page}"
    );
    assert!(
        page.contains(
            "At 942,00 zł a month you’ll reach 7 536,00 zł. Two top-ups of 2 826,00 zł in \
             November and December fill the limit."
        ),
        "{page}"
    );
    // Allocation is not built yet: no button to it.
    assert!(!page.contains("Plan a top-up"), "{page}");
    // At the config's 12%: 5652 x 0.12 = 678,24 and 11 304 x 0.12 = 1356,48.
    assert!(page.contains("+678,24 zł"), "{page}");
    assert!(page.contains("+1 356,48 zł"), "{page}");
    assert!(
        page.contains(r##"hx-get="/ikze?rate=12" hx-target="#tax" hx-swap="outerHTML" hx-push-url="true" aria-current="true""##),
        "{page}"
    );
    // The months: Oct paid and current, Nov and Dec dashed at full height.
    assert!(page.contains(">2 826</span>"), "{page}");
    assert!(
        page.contains(
            "height: 112px; border-radius: 4px; box-sizing: border-box; border: 1.5px dashed"
        ),
        "{page}"
    );
    assert!(page.contains("font-weight: 500\">Oct</span>"), "{page}");
}

#[tokio::test]
async fn the_bracket_switch_returns_just_the_card_for_htmx() {
    let pool = open_in_memory().await.unwrap();
    design_year(&pool).await;
    let router = router(pool, "2026-10-16");
    let card = body(&router, "/ikze?rate=32", true).await;

    assert!(card.starts_with(r#"<section id="tax""#), "{card}");
    assert!(!card.contains("<html"), "{card}");
    // 5652 x 0.32 = 1808,64; 11 304 x 0.32 = 3617,28.
    assert!(card.contains("+1 808,64 zł"), "{card}");
    assert!(card.contains("+3 617,28 zł"), "{card}");
    assert!(
        card.contains(r#"aria-current="true" style="min-width: 52px">32%"#),
        "{card}"
    );
    assert!(card.contains("at the 32% bracket"), "{card}");

    // The same state as a full page, and an unknown bracket falls back to the config's 12%.
    let page = body(&router, "/ikze?rate=32", false).await;
    assert!(
        page.contains("<html") && page.contains("+1 808,64 zł"),
        "{page}"
    );
    let fallback = body(&router, "/ikze?rate=50", true).await;
    assert!(fallback.contains("+678,24 zł"), "{fallback}");
}

#[tokio::test]
async fn a_euro_deposit_uses_the_cached_rate_of_its_day() {
    // 100 EUR on 2 Mar at 4.30 = 430, plus 570 PLN on 3 Mar: 1000 paid in. 10 304 left.
    let pool = open_in_memory().await.unwrap();
    insert_transactions(
        &pool,
        IKZE,
        &[],
        "xtb",
        &[
            deposit("2026-03-02", "100", "EUR", "e1"),
            deposit("2026-03-03", "570", "PLN", "p1"),
        ],
    )
    .await
    .unwrap();
    upsert_rates(
        &pool,
        &[FxRate {
            currency: "EUR".into(),
            effective_date: d("2026-03-02"),
            mid: Decimal::from_str("4.30").unwrap(),
            table_no: "042/A/NBP/2026".into(),
        }],
    )
    .await
    .unwrap();
    let page = body(&router(pool, "2026-10-16"), "/ikze", false).await;

    assert!(page.contains(">1 000,00<span"), "{page}");
    assert!(page.contains(">10 304,00 zł</div>"), "{page}");
    assert!(page.contains(">1 000</span>"), "{page}");
}

#[tokio::test]
async fn a_missing_rate_or_limit_is_said_instead_of_a_wrong_figure() {
    // NBP is down and the rate of 2 Mar is not cached: no total rather than a wrong one.
    let pool = open_in_memory().await.unwrap();
    insert_transactions(
        &pool,
        IKZE,
        &[],
        "xtb",
        &[deposit("2026-03-02", "100", "EUR", "e1")],
    )
    .await
    .unwrap();
    let page = body(&router(pool, "2026-10-16"), "/ikze", false).await;
    assert!(
        page.contains("The contributions cannot be added up: no EUR-&#62;PLN rate for 2026-03-02."),
        "{page}"
    );
    assert!(!page.contains("Annual limit"), "{page}");

    // 2027 has no limit in the config.
    let pool = open_in_memory().await.unwrap();
    let page = body(&router(pool, "2027-01-04"), "/ikze", false).await;
    assert!(page.contains("tax year 2027"), "{page}");
    assert!(
        page.contains("The 2027 figures cannot be shown: no IKZE limit for 2027 in the config"),
        "{page}"
    );
}

#[tokio::test]
async fn an_empty_year_spreads_the_whole_limit() {
    // Nothing paid in by 10 Jan: 11 304 left, twelve top-ups of 942, every month dashed.
    let pool = open_in_memory().await.unwrap();
    let page = body(&router(pool, "2026-01-10"), "/ikze", false).await;

    assert!(page.contains(">0,00<span"), "{page}");
    assert!(
        page.contains(
            "Nothing has been paid in this year yet. Twelve top-ups of 942,00 zł, one a month \
             from January to December, fill the limit."
        ),
        "{page}"
    );
    assert_eq!(
        page.matches("border: 1.5px dashed var(--accent); background: transparent")
            .count(),
        12
    );
    assert!(page.contains("±0,00 zł"), "{page}");
}
