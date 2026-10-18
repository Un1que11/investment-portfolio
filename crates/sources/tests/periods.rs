use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use portfolio_core::AssetClass;
use portfolio_core::benchmark::BenchmarkError;
use portfolio_core::period::{Period, PeriodError};
use portfolio_db::gold::{NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::transactions::{
    Account, Asset, NewTransaction, insert_transactions, load_ledger,
};
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::history::period_chart;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::periods::period_views;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::{Sources, portfolio_on};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EUR_OCT: &str = include_str!("fixtures/nbp/eur_2026-10-05_2026-10-14.json");
const OHLC_BTC: &str = include_str!("fixtures/kraken/ohlc_xbteur_daily.json");
const GOLD_LAST_90: &str = include_str!("fixtures/nbp/gold_last_90.json");
const VWCE_DAILY: &str = include_str!("fixtures/alphavantage/daily_vwce_tail.json");

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

fn clock() -> Clock {
    let now: DateTime<Utc> = "2026-10-15T14:00:00Z".parse().unwrap();
    Clock {
        now,
        today: now.date_naive(),
    }
}

const KRAKEN: Account<'static> = Account {
    name: "Kraken",
    kind: "exchange",
    provider: "kraken",
    currency: "EUR",
};

#[tokio::test]
async fn a_start_value_counts_as_money_put_in_on_the_start_day() {
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
    let pool = open_in_memory().await.unwrap();
    // A coin for 1600 PLN on 1 Sep; Kraken: 200 EUR in on 12 Oct, 0.002 BTC for 180 EUR + 2.70
    // fee on 13 Oct.
    add_coin(
        &pool,
        &NewCoin {
            label: None,
            purchased_on: d("2026-09-01"),
            purchase_price_pln: dec!(1600),
            fine_weight_g: dec!(3.11),
            manual_quote_pln: None,
        },
    )
    .await
    .unwrap();
    insert_transactions(
        &pool,
        KRAKEN,
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
    let r = portfolio_on(d("2026-10-14"), &s).await.unwrap();

    let p = period_views(&ledger, &r, &s).await;

    assert_eq!(
        p.views.iter().map(|v| v.period).collect::<Vec<_>>(),
        Period::ALL
    );
    // End value as the report for 14 Oct: BTC 637.13120796 + coin 1557.73058 + cash 75.61138.
    assert_eq!(r.valuation.total, dec!(2270.47316796));
    // 1M from the end of 14 Sep: the coin is worth 520.77 PLN/g x 3.11 g x 0.95 = 1538.614965
    // then; only the 200 EUR x 4.3889 = 877.78 PLN deposit is the period's.
    // P&L 2270.47316796 - 1538.614965 - 877.78 = -145.92179704.
    let m = p.view(Period::OneMonth).unwrap();
    assert_eq!(m.start_value, Some(dec!(1538.614965)));
    assert_eq!(m.contributions, dec!(877.7800));
    assert_eq!(m.pnl(), Some(dec!(-145.92179704)));
    assert_eq!(m.xirr, Err(PeriodError::TooShort));
    // Without an Alpha Vantage key there is no VWCE price, so no benchmark.
    assert_eq!(
        m.benchmark,
        Err(PeriodError::Benchmark(BenchmarkError::MissingPrice(d(
            "2026-09-14"
        ))))
    );
    // 6M from 14 Apr: nothing held yet, so it runs from the coin on 1 Sep like All:
    // 2270.47316796 - (1600 + 877.78) = -207.30683204.
    for period in [
        Period::SixMonths,
        Period::YearToDate,
        Period::OneYear,
        Period::All,
    ] {
        let v = p.view(period).unwrap();
        assert_eq!(v.start_value, Some(dec!(0)), "{period}");
        assert_eq!(v.since, Some(d("2026-09-01")), "{period}");
        assert_eq!(v.contributions, dec!(2477.7800), "{period}");
        assert_eq!(v.pnl(), Some(dec!(-207.30683204)), "{period}");
        assert_eq!(v.pnl(), r.pnl.pnl_if_complete(), "{period}");
    }
    assert!(p.problems.is_empty(), "{:?}", p.problems);
}

#[tokio::test]
async fn start_days_before_the_first_contribution_need_no_vwce_price() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/exchangerates/rates/a/eur/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .mount(&server)
        .await;
    // One daily request for the report; the periods use the cache. The daily closes start on
    // 28 Sep, so asking for any start day (14 Sep and earlier) would fetch the weekly history.
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", "TIME_SERIES_DAILY"))
        .respond_with(ResponseTemplate::new(200).set_body_string(VWCE_DAILY))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", "TIME_SERIES_WEEKLY"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    // XTB IKZE: 1516.5059 PLN on 5 Oct and 756.69465 PLN on 11 Oct; Kraken 173.48 EUR on 12 Oct.
    let deposit = |day, amount, currency, id| row("deposit", day, None, None, amount, currency, id);
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
            deposit("2026-10-05", dec!(1516.5059), "PLN", "b1"),
            deposit("2026-10-11", dec!(756.69465), "PLN", "b2"),
        ],
    )
    .await
    .unwrap();
    insert_transactions(
        &pool,
        KRAKEN,
        &[],
        "kraken",
        &[deposit("2026-10-12", dec!(173.48), "EUR", "b3")],
    )
    .await
    .unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let av = AlphaVantageClient::new(server.uri(), "test-key")
        .unwrap()
        .with_request_spacing(std::time::Duration::from_millis(10));
    let s = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: Some(&av),
        clock: clock(),
        gold_buyback_factor: Decimal::ONE,
    };
    let ledger = load_ledger(&pool).await.unwrap();
    let r = portfolio_on(d("2026-10-14"), &s).await.unwrap();

    let p = period_views(&ledger, &r, &s).await;

    // Every period starts before 5 Oct, so each is the whole portfolio: VWCE alone would be
    // 4 units worth 3015.364352, the cash is worth 3031.412238, 16.047886 ahead (as the
    // report's benchmark). Contributions 3034.586922: P&L -3.174684.
    for v in &p.views {
        let b = v.benchmark.as_ref().unwrap();
        assert_eq!(
            (b.units, b.value),
            (dec!(4), dec!(3015.364352)),
            "{}",
            v.period
        );
        assert_eq!(v.benchmark_gap(), Some(dec!(16.047886)), "{}", v.period);
        assert_eq!(v.pnl(), Some(dec!(-3.174684)), "{}", v.period);
        assert_eq!(v.xirr, Err(PeriodError::TooShort), "{}", v.period);
    }
    assert!(p.problems.is_empty(), "{:?}", p.problems);
}

#[tokio::test]
async fn the_chart_replays_each_periods_flows_into_vwce() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/exchangerates/rates/a/eur/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", "TIME_SERIES_DAILY"))
        .respond_with(ResponseTemplate::new(200).set_body_string(VWCE_DAILY))
        .expect(1)
        .mount(&server)
        .await;
    // The 1M chart starts on 14 Sep, before the daily closes, but nothing is held then.
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", "TIME_SERIES_WEEKLY"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    // The same deposits as above: 1516.5059 PLN on 5 Oct, 756.69465 PLN on 11 Oct, 173.48 EUR
    // on 12 Oct.
    let deposit = |day, amount, currency, id| row("deposit", day, None, None, amount, currency, id);
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
            deposit("2026-10-05", dec!(1516.5059), "PLN", "b1"),
            deposit("2026-10-11", dec!(756.69465), "PLN", "b2"),
        ],
    )
    .await
    .unwrap();
    insert_transactions(
        &pool,
        KRAKEN,
        &[],
        "kraken",
        &[deposit("2026-10-12", dec!(173.48), "EUR", "b3")],
    )
    .await
    .unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let av = AlphaVantageClient::new(server.uri(), "test-key")
        .unwrap()
        .with_request_spacing(std::time::Duration::from_millis(10));
    let s = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: Some(&av),
        clock: clock(),
        gold_buyback_factor: Decimal::ONE,
    };
    let ledger = load_ledger(&pool).await.unwrap();
    let r = portfolio_on(d("2026-10-14"), &s).await.unwrap();

    // All: the first contribution and the end day. On 5 Oct the 1516.5059 PLN is all there is,
    // in cash and in VWCE alike; on 14 Oct the values of the test above.
    let all = period_chart(&ledger, &r, Period::All, &s).await;
    assert!(all.problems.is_empty(), "{:?}", all.problems);
    let points: Vec<_> = all
        .points
        .iter()
        .map(|p| {
            (
                p.date,
                p.contributions,
                p.value,
                p.benchmark.map(|b| b.round_dp(6)),
            )
        })
        .collect();
    assert_eq!(
        points,
        [
            (
                d("2026-10-05"),
                Some(dec!(1516.5059)),
                Some(dec!(1516.5059)),
                Some(dec!(1516.5059))
            ),
            (
                d("2026-10-14"),
                Some(dec!(3034.586922)),
                Some(dec!(3031.412238)),
                Some(dec!(3015.364352))
            ),
        ]
    );

    // 1M: 14 Sep, 30 Sep (both before anything was put in: 0 everywhere) and 14 Oct.
    let month = period_chart(&ledger, &r, Period::OneMonth, &s).await;
    assert!(month.problems.is_empty(), "{:?}", month.problems);
    let points: Vec<_> = month
        .points
        .iter()
        .map(|p| (p.date, p.value, p.benchmark.map(|b| b.round_dp(6))))
        .collect();
    assert_eq!(
        points,
        [
            (d("2026-09-14"), Some(dec!(0)), Some(dec!(0))),
            (d("2026-09-30"), Some(dec!(0)), Some(dec!(0))),
            (
                d("2026-10-14"),
                Some(dec!(3031.412238)),
                Some(dec!(3015.364352))
            ),
        ]
    );
}
