use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use portfolio_core::AssetClass;
use portfolio_core::portfolio::{AccountKind, TxKind};
use portfolio_core::xirr::XirrError;
use portfolio_db::gold::{NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::transactions::{
    Account, Asset, NewTransaction, insert_transactions, load_ledger,
};
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::kraken_ledger::map_entries;
use portfolio_sources::kraken_private::parse_ledgers;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::{
    GOLD_COINS, Inputs, Sources, fetch_inputs, portfolio_on, report, requirements,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EUR_OCT: &str = include_str!("fixtures/nbp/eur_2026-10-05_2026-10-14.json");
const OHLC_BTC: &str = include_str!("fixtures/kraken/ohlc_xbteur_daily.json");
const GOLD_LAST_90: &str = include_str!("fixtures/nbp/gold_last_90.json");
const KRAKEN_PAGE1: &str = include_str!("fixtures/kraken/ledgers_page1.json");
const KRAKEN_PAGE2: &str = include_str!("fixtures/kraken/ledgers_page2.json");
const XTB_FIXTURE: &str = "../xtb/tests/fixtures/xtb_ikze_anonymised.xlsx";

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn at(s: &str) -> NaiveDateTime {
    d(s).and_hms_opt(12, 0, 0).unwrap()
}

const KRAKEN: Account<'static> = Account {
    name: "Kraken",
    kind: "exchange",
    provider: "kraken",
    currency: "EUR",
};
const BTC_ASSET: Asset<'static> = Asset {
    symbol: "BTC",
    name: "Bitcoin",
    class: AssetClass::Crypto,
    currency: "EUR",
};

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
        // Buys and sells must carry a unit price (schema CHECK).
        unit_price: qty.map(|q| amount / q),
        amount,
        currency: currency.into(),
        external_id: id.into(),
        note: None,
    }
}

/// Kraken: deposit 200 EUR (12 Oct), buy 0.002 BTC for 180 EUR + 2.70 fee (13 Oct).
async fn kraken_ledger(pool: &SqlitePool) {
    insert_transactions(
        pool,
        KRAKEN,
        &[BTC_ASSET],
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

#[tokio::test]
async fn load_ledger_maps_accounts_and_kinds() {
    let pool = open_in_memory().await.unwrap();
    kraken_ledger(&pool).await;
    add_coin(
        &pool,
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

    let ledger = load_ledger(&pool).await.unwrap();

    assert_eq!(ledger.len(), 4);
    assert_eq!(
        (
            ledger[0].account.as_str(),
            ledger[0].account_kind,
            ledger[0].kind
        ),
        ("Gold (manual)", AccountKind::Manual, TxKind::Buy)
    );
    assert_eq!(ledger[0].asset.as_deref(), Some(GOLD_COINS));
    assert_eq!(
        (ledger[1].account_kind, ledger[1].kind, ledger[1].date),
        (AccountKind::Funded, TxKind::Deposit, d("2026-10-12"))
    );
    assert_eq!(ledger[2].quantity, Some(dec!(0.002)));
}

#[tokio::test]
async fn real_xtb_export_gives_its_own_cash_total_and_contributions() {
    let pool = open_in_memory().await.unwrap();
    let export = portfolio_xtb::parse_file(XTB_FIXTURE).unwrap();
    portfolio_xtb::import::import(&pool, &export).await.unwrap();
    let ledger = load_ledger(&pool).await.unwrap();

    let req = requirements(&ledger, d("2026-10-15"));
    let r = report(&ledger, d("2026-10-15"), &Inputs::default()).unwrap();

    // Cash left at XTB = deposits 2175 - purchases 2171.86 = 3.14 PLN, the export's own Total.
    assert_eq!(
        r.holdings.cash[&("XTB IKZE".into(), "PLN".into())],
        dec!(3.14)
    );
    // All PLN: no rates needed; contributions are the deposits.
    assert!(req.flow_rates.is_empty() && req.cash_currencies.is_empty());
    assert_eq!(r.contributions.total, dec!(2175));
    // VWCE units = sum of the filled volumes; without a price it is reported missing.
    let units: Decimal = export
        .operations
        .iter()
        .filter_map(|o| match &o.kind {
            portfolio_xtb::OperationKind::Purchase { filled, .. } => Some(*filled),
            _ => None,
        })
        .sum();
    assert_eq!(r.holdings.assets["VWCE"], units);
    assert_eq!(r.valuation.missing, ["VWCE"]);
    assert_eq!(r.valuation.total, dec!(3.14));
}

#[tokio::test]
async fn kraken_mapping_feeds_contributions_in_eur() {
    let pool = open_in_memory().await.unwrap();
    let mut entries = parse_ledgers(KRAKEN_PAGE1).unwrap().entries;
    entries.extend(parse_ledgers(KRAKEN_PAGE2).unwrap().entries);
    let raw: Vec<_> = entries.into_iter().map(Into::into).collect();
    let mapped = map_entries(&raw).unwrap();
    let assets = [
        BTC_ASSET,
        Asset {
            symbol: "ETH",
            name: "Ether",
            class: AssetClass::Crypto,
            currency: "EUR",
        },
    ];
    insert_transactions(&pool, KRAKEN, &assets, "kraken", &mapped.transactions)
        .await
        .unwrap();
    let ledger = load_ledger(&pool).await.unwrap();

    let req = requirements(&ledger, d("2026-01-31"));
    // EUR rates are needed on every day with a non-PLN flow: deposits (1, 2, 5 Jan) for
    // contributions and purchases and their fees (3, 4, 5 Jan) for P&L. EUR cash needs one too.
    assert_eq!(
        req.flow_rates,
        BTreeSet::from([
            ("EUR".into(), d("2026-01-01")),
            ("EUR".into(), d("2026-01-02")),
            ("EUR".into(), d("2026-01-03")),
            ("EUR".into(), d("2026-01-04")),
            ("EUR".into(), d("2026-01-05")),
        ])
    );
    assert_eq!(req.cash_currencies, BTreeSet::from(["EUR".into()]));
    let flat = Inputs {
        flow_rates: req
            .flow_rates
            .iter()
            .map(|k| (k.clone(), Some(dec!(4))))
            .collect(),
        ..Inputs::default()
    };
    let r = report(&ledger, d("2026-01-31"), &flat).unwrap();
    // (500 + 300 + 100) EUR x 4 = 3600 PLN.
    assert_eq!(r.contributions.total, dec!(3600));
    // Cash: 900 deposited - (1.50 + 2.00) deposit fees - (250 + 120 + 60 + 98) buys
    //       - (3.75 + 1.80 + 0.90 + 1.47) purchase fees = 900 - 3.50 - 528 - 7.92 = 360.58 EUR.
    assert_eq!(
        r.holdings.cash[&("Kraken".into(), "EUR".into())],
        dec!(360.58)
    );
    // ETH: 0.05 + 0.04 bought + 2 x 0.000075 staking = 0.09015.
    assert_eq!(r.holdings.assets["ETH"], dec!(0.09015));
}

#[tokio::test]
async fn report_requires_every_flow_rate() {
    let pool = open_in_memory().await.unwrap();
    kraken_ledger(&pool).await;
    let ledger = load_ledger(&pool).await.unwrap();

    let err = report(&ledger, d("2026-10-14"), &Inputs::default()).unwrap_err();

    assert_eq!((err.currency.as_str(), err.date), ("EUR", d("2026-10-12")));
}

#[tokio::test]
async fn live_sources_value_the_portfolio() {
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
    kraken_ledger(&pool).await;
    add_coin(
        &pool,
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
    // A VWCE position, to show what happens without an Alpha Vantage key.
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
    let now: DateTime<Utc> = "2026-10-15T14:00:00Z".parse().unwrap();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: None,
        clock: Clock {
            now,
            today: now.date_naive(),
        },
        gold_buyback_factor: dec!(0.95),
    };

    let r = portfolio_on(d("2026-10-14"), &sources).await.unwrap();

    // BTC: 0.002 x 72888.3 EUR (Kraken close 14 Oct) x 4.3706 (NBP 14 Oct) = 637.13120796.
    assert_eq!(r.valuation.by_asset["BTC"], dec!(637.13120796));
    // Gold coin: 527.24 PLN/g (NBP 14 Oct) x 3.11 g x 0.95 = 1557.73058.
    assert_eq!(r.valuation.by_asset[GOLD_COINS], dec!(1557.73058));
    // Kraken cash 200 - 180 - 2.70 = 17.30 EUR x 4.3706 = 75.61138; XTB cash 300 PLN.
    assert_eq!(
        r.valuation.cash[&("Kraken".into(), "EUR".into())],
        dec!(75.611380)
    );
    assert_eq!(
        r.valuation.cash[&("XTB IKZE".into(), "PLN".into())],
        dec!(300)
    );
    // VWCE has no price without an Alpha Vantage key: reported, not guessed.
    assert_eq!(r.valuation.missing, ["VWCE"]);
    assert!(
        r.problems
            .iter()
            .any(|p| p.contains("VWCE") && p.contains("Alpha Vantage"))
    );
    // Contributions: 200 EUR x 4.3889 (NBP 12 Oct) + 1000 PLN (XTB) + 1600 PLN (coin) = 3477.78.
    assert_eq!(r.contributions.total, dec!(3477.7800));
    assert_eq!(
        r.valuation.total,
        dec!(637.13120796) + dec!(1557.73058) + dec!(75.61138) + dec!(300)
    );
    // BTC P&L: cost (180 + 2.70) EUR x 4.3806 (NBP 13 Oct) = 800.33562; value 637.13120796
    // -> -163.20441204. Gold: 1557.73058 - 1600 = -42.26942. VWCE has no price: unknown.
    let btc = &r.pnl.by_asset["BTC"];
    assert_eq!(btc.cost, dec!(800.335620));
    assert_eq!(btc.pnl(), Some(dec!(-163.20441204)));
    assert_eq!(r.pnl.by_asset[GOLD_COINS].pnl(), Some(dec!(-42.26942)));
    assert_eq!(r.pnl.by_asset["VWCE"].pnl(), None);
    assert!(!r.pnl.complete);
    assert_eq!(r.pnl.pnl(), r.valuation.total - dec!(3477.7800));
    // VWCE is unpriced, so the portfolio P&L is not offered as a result.
    assert_eq!((r.pnl.pnl_if_complete(), r.pnl.pnl_ratio()), (None, None));
    // XIRR: the same gaps. Gold has one flow in and its value, 13 days apart:
    // (1557.73058 / 1600)^(365 / 13) - 1 = -52.84% a year (a short period exaggerates).
    assert_eq!(r.xirr.first_flow, Some(d("2026-10-01")));
    assert_eq!(r.xirr.portfolio, Err(XirrError::MissingValue));
    assert_eq!(r.xirr.by_asset["VWCE"], Err(XirrError::MissingValue));
    assert_eq!(r.xirr.by_asset[GOLD_COINS], Ok(dec!(-0.528446)));
}

#[tokio::test]
async fn fetch_inputs_records_problems_per_value() {
    let server = MockServer::start().await; // nothing mounted: every request fails
    let pool = open_in_memory().await.unwrap();
    kraken_ledger(&pool).await;
    let ledger = load_ledger(&pool).await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let now: DateTime<Utc> = "2026-10-15T14:00:00Z".parse().unwrap();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: None,
        clock: Clock {
            now,
            today: now.date_naive(),
        },
        gold_buyback_factor: Decimal::ONE,
    };

    let inputs = fetch_inputs(
        &requirements(&ledger, d("2026-10-14")),
        d("2026-10-14"),
        &sources,
    )
    .await;

    assert_eq!(
        inputs.position_values,
        BTreeMap::from([("BTC".into(), None)])
    );
    assert_eq!(inputs.cash_rates, BTreeMap::from([("EUR".into(), None)]));
    // BTC price, EUR cash rate, and EUR rates for the deposit (12 Oct) and purchase (13 Oct).
    assert_eq!(inputs.problems.len(), 4, "{:?}", inputs.problems);
}
