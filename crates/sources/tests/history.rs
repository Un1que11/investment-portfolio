use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use portfolio_core::AssetClass;
use portfolio_core::history::ValuePoint;
use portfolio_db::gold::{NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::transactions::{
    Account, Asset, NewTransaction, insert_transactions, load_ledger,
};
use portfolio_sources::history::value_series;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::Sources;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EUR_OCT: &str = include_str!("fixtures/nbp/eur_2026-10-05_2026-10-14.json");
const OHLC_BTC: &str = include_str!("fixtures/kraken/ohlc_xbteur_daily.json");
const GOLD_LAST_90: &str = include_str!("fixtures/nbp/gold_last_90.json");

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

/// A coin for 1600 PLN on 1 Oct; Kraken: 200 EUR in on 12 Oct, 0.002 BTC for 180 EUR + 2.70 fee
/// on 13 Oct.
async fn ledger(pool: &SqlitePool) {
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

async fn mount(server: &MockServer, kraken: ResponseTemplate, kraken_calls: u64) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/exchangerates/rates/a/eur/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cenyzlota/last/90/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(GOLD_LAST_90))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/0/public/OHLC"))
        .respond_with(kraken)
        .expect(kraken_calls)
        .mount(server)
        .await;
}

fn clock() -> Clock {
    let now: DateTime<Utc> = "2026-10-15T14:00:00Z".parse().unwrap();
    Clock {
        now,
        today: now.date_naive(),
    }
}

#[tokio::test]
async fn each_point_is_the_portfolio_on_its_day() {
    let server = MockServer::start().await;
    // One OHLC request for both BTC days: the second comes from the price cache.
    mount(
        &server,
        ResponseTemplate::new(200).set_body_string(OHLC_BTC),
        1,
    )
    .await;
    let pool = open_in_memory().await.unwrap();
    ledger(&pool).await;
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let s = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: None,
        clock: clock(),
        gold_buyback_factor: dec!(0.95),
    };
    let ledger = load_ledger(&pool).await.unwrap();

    let series = value_series(
        &ledger,
        &[
            d("2026-10-01"),
            d("2026-10-12"),
            d("2026-10-13"),
            d("2026-10-14"),
        ],
        &s,
    )
    .await;

    let point = |date, contributions, value| ValuePoint {
        date: d(date),
        contributions: Some(contributions),
        value: Some(value),
        missing: vec![],
    };
    assert_eq!(
        series.points,
        [
            // Only the coin: 518.14 PLN/g (NBP 1 Oct) x 3.11 g x 0.95 = 1530.84463.
            point("2026-10-01", dec!(1600), dec!(1530.84463)),
            // Coin 525.35 x 3.11 x 0.95 = 1552.146575; 200 EUR x 4.3889 (12 Oct) = 877.78 in
            // cash, also put in at that rate: contributions 1600 + 877.78.
            point("2026-10-12", dec!(2477.7800), dec!(2429.926575)),
            // Coin 526.13 x 3.11 x 0.95 = 1554.451085; 0.002 BTC x 74193.4 EUR x 4.3806 =
            // 650.02321608; cash 17.30 EUR x 4.3806 = 75.78438. Total 2280.25868108.
            point("2026-10-13", dec!(2477.7800), dec!(2280.25868108)),
            // As the report for 14 Oct: 637.13120796 + 1557.73058 + 75.61138.
            point("2026-10-14", dec!(2477.7800), dec!(2270.47316796)),
        ]
    );
    // Per asset, only the days it was held: the coin on all four, BTC from 13 Oct (bought
    // that day; the figures are the coin and BTC parts of the totals above).
    assert_eq!(
        series.assets["GOLD_COINS"],
        [
            (d("2026-10-01"), Some(dec!(1530.84463))),
            (d("2026-10-12"), Some(dec!(1552.146575))),
            (d("2026-10-13"), Some(dec!(1554.451085))),
            (d("2026-10-14"), Some(dec!(1557.73058))),
        ]
    );
    assert_eq!(
        series.assets["BTC"],
        [
            (d("2026-10-13"), Some(dec!(650.02321608))),
            (d("2026-10-14"), Some(dec!(637.13120796))),
        ]
    );
    assert_eq!(series.assets.len(), 2);
    assert!(series.problems.is_empty(), "{:?}", series.problems);
}

#[tokio::test]
async fn a_failed_provider_is_asked_once_and_its_days_have_no_value() {
    let server = MockServer::start().await;
    // Down: without the stop, each BTC day would retry.
    mount(&server, ResponseTemplate::new(503), 1).await;
    let pool = open_in_memory().await.unwrap();
    ledger(&pool).await;
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let s = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: None,
        clock: clock(),
        gold_buyback_factor: dec!(0.95),
    };
    let ledger = load_ledger(&pool).await.unwrap();

    let series = value_series(
        &ledger,
        &[d("2026-10-12"), d("2026-10-13"), d("2026-10-14")],
        &s,
    )
    .await;

    let values: Vec<Option<Decimal>> = series.points.iter().map(|p| p.value).collect();
    assert_eq!(values, [Some(dec!(2429.926575)), None, None]);
    // Contributions do not depend on prices.
    assert!(
        series
            .points
            .iter()
            .all(|p| p.contributions == Some(dec!(2477.7800)))
    );
    assert_eq!(series.points[1].missing, ["BTC"]);
    // BTC's own days have no value either; the coin keeps its value.
    let btc: Vec<Option<Decimal>> = series.assets["BTC"].iter().map(|(_, v)| *v).collect();
    assert_eq!(btc, [None, None]);
    assert_eq!(series.problems.len(), 2, "{:?}", series.problems);
    assert!(series.problems[0].starts_with("chart: kraken failed"));
    assert_eq!(
        series.problems[1],
        "chart: 2 of 3 days have no complete value (first 2026-10-13: BTC)"
    );
}

#[tokio::test]
async fn vwce_without_an_alpha_vantage_key_has_no_value() {
    let server = MockServer::start().await;
    mount(
        &server,
        ResponseTemplate::new(200).set_body_string(OHLC_BTC),
        0,
    )
    .await;
    let pool = open_in_memory().await.unwrap();
    insert_transactions(
        &pool,
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
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let s = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: None,
        clock: clock(),
        gold_buyback_factor: dec!(0.95),
    };
    let ledger = load_ledger(&pool).await.unwrap();

    let series = value_series(&ledger, &[d("2026-10-01"), d("2026-10-14")], &s).await;

    // Before the purchase it is all PLN cash; after it, VWCE has no price and no value is shown.
    let values: Vec<Option<Decimal>> = series.points.iter().map(|p| p.value).collect();
    assert_eq!(values, [Some(dec!(1000)), None]);
    assert_eq!(
        series.problems,
        ["chart: 1 of 2 days have no complete value (first 2026-10-14: VWCE)"]
    );
}
