use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use portfolio_core::AssetClass;
use portfolio_core::allocation::{AllocationError, Sleeve};
use portfolio_core::benchmark::BenchmarkError;
use portfolio_core::config::Config;
use portfolio_core::ikze::IkzeError;
use portfolio_core::portfolio::{AccountKind, TxKind, Wrapper};
use portfolio_core::topup::TopUpError;
use portfolio_core::xirr::XirrError;
use portfolio_db::gold::{NewCoin, add_coin};
use portfolio_db::open_in_memory;
use portfolio_db::transactions::{
    Account, Asset, NewTransaction, insert_transactions, load_ledger,
};
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::kraken_ledger::map_entries;
use portfolio_sources::kraken_private::parse_ledgers;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::{
    GOLD_COINS, Inputs, Sources, fetch_inputs, portfolio_on, report, requirements, sleeve,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path, path_regex, query_param};
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
    // Neither Kraken nor the gold account is a tax wrapper.
    assert!(ledger.iter().all(|t| t.wrapper.is_none()));
}

#[tokio::test]
async fn report_counts_this_years_ikze_deposits_against_the_limit() {
    let pool = open_in_memory().await.unwrap();
    kraken_ledger(&pool).await;
    let xtb = |name, kind| Account {
        name,
        kind,
        provider: "xtb",
        currency: "PLN",
    };
    let deposit = |day, amount, id| row("deposit", day, None, None, amount, "PLN", id);
    insert_transactions(
        &pool,
        xtb("XTB IKZE", "ikze"),
        &[],
        "xtb",
        &[
            deposit("2025-12-30", dec!(2000), "z1"),
            deposit("2026-01-05", dec!(3000), "z2"),
            deposit("2026-10-01", dec!(1500), "z3"),
        ],
    )
    .await
    .unwrap();
    insert_transactions(
        &pool,
        xtb("XTB IKE", "ike"),
        &[],
        "xtb",
        &[deposit("2026-02-01", dec!(800), "e1")],
    )
    .await
    .unwrap();
    let ledger = load_ledger(&pool).await.unwrap();
    let wrapper = |account: &str| {
        ledger
            .iter()
            .find(|t| t.account == account)
            .unwrap()
            .wrapper
    };
    assert_eq!(wrapper("XTB IKZE"), Some(Wrapper::Ikze));
    assert_eq!(wrapper("XTB IKE"), Some(Wrapper::Ike));
    assert_eq!(wrapper("Kraken"), None);
    let config = |limits: &str| {
        Config::from_toml(&format!(
            r#"
            [targets]
            vwce = 55
            crypto = 25
            gold = 20
            [targets.crypto_split]
            btc = 75
            eth = 25
            [ikze.limits]
            {limits}
            [tax]
            rate_percent = 12
            [gold]
            buyback_factor = "0.95"
            "#
        ))
        .unwrap()
    };
    let date = d("2026-10-15");
    let rate = |k: &(String, NaiveDate)| (k.clone(), Some(dec!(4)));
    let inputs = Inputs {
        flow_rates: requirements(&ledger, date)
            .flow_rates
            .iter()
            .map(rate)
            .collect(),
        ..Inputs::default()
    };
    let r = report(&ledger, date, &inputs).unwrap();

    // 2026 IKZE deposits: 3000 + 1500 = 4500. Not the 2025 one, the IKE one or Kraken's EUR.
    // Limit 11 304: 6804 left; 4500 x 0.12 = 540 saved.
    let y = r.ikze(&config(r#"2026 = "11304.00""#)).unwrap();
    assert_eq!(
        (y.year, y.contributed, y.withdrawn),
        (2026, dec!(4500), dec!(0))
    );
    assert_eq!((y.remaining, y.tax_saving), (dec!(6804), dec!(540)));
    // The limit is the config's, not a constant: with 5000 configured, 500 is left.
    let y = r.ikze(&config(r#"2026 = "5000""#)).unwrap();
    assert_eq!(y.remaining, dec!(500));
    assert_eq!(
        r.ikze(&config(r#"2025 = "9388.80""#)),
        Err(IkzeError::NoLimit(2026))
    );
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

#[test]
fn every_priced_symbol_has_a_target() {
    assert_eq!(sleeve("VWCE"), Some(Sleeve::Vwce));
    assert_eq!(sleeve("BTC"), Some(Sleeve::Btc));
    assert_eq!(sleeve("ETH"), Some(Sleeve::Eth));
    assert_eq!(sleeve(GOLD_COINS), Some(Sleeve::Gold));
    assert_eq!(sleeve("EUR"), None);
}

#[tokio::test]
async fn report_compares_holdings_with_the_targets() {
    let pool = open_in_memory().await.unwrap();
    let export = portfolio_xtb::parse_file(XTB_FIXTURE).unwrap();
    portfolio_xtb::import::import(&pool, &export).await.unwrap();
    kraken_ledger(&pool).await;
    let ledger = load_ledger(&pool).await.unwrap();
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
    let config = Config::from_toml(CONFIG).unwrap();
    let date = d("2026-10-15");
    let req = requirements(&ledger, date);
    let rate = |k: &(String, NaiveDate)| (k.clone(), Some(dec!(4)));
    let mut inputs = Inputs {
        position_values: BTreeMap::from([
            ("VWCE".into(), Some(dec!(1200))),
            ("BTC".into(), Some(dec!(800))),
        ]),
        cash_rates: BTreeMap::from([("EUR".into(), Some(dec!(4)))]),
        flow_rates: req.flow_rates.iter().map(rate).collect(),
        vwce_prices: BTreeMap::new(),
        last_known: BTreeMap::new(),
        problems: vec![],
    };

    let a = report(&ledger, date, &inputs)
        .unwrap()
        .allocation(&config)
        .unwrap();

    // VWCE 1200 + BTC 800 = 2000; the cash at XTB (3.14 PLN) and Kraken (17.30 EUR) is left
    // out. VWCE 60% vs 55%: +5 pp, 1200 - 1100 = +100 PLN. Crypto 40% vs 25%: +15 pp,
    // 800 - 500 = +300 PLN. No gold: -20 pp, -400 PLN.
    assert_eq!(a.total, dec!(2000));
    let dev = |c: AssetClass| (a.by_class[&c].points(), a.by_class[&c].pln());
    assert_eq!(dev(AssetClass::Vwce), (dec!(0.05), dec!(100)));
    assert_eq!(dev(AssetClass::Crypto), (dec!(0.15), dec!(300)));
    assert_eq!(dev(AssetClass::Gold), (dec!(-0.2), dec!(-400)));
    // The crypto is all BTC: +25 pp of the crypto, 800 - 600 = +200 PLN; ETH -200 PLN.
    let split = a.within_crypto.unwrap();
    assert_eq!(
        (split.btc.points(), split.btc.pln()),
        (dec!(0.25), dec!(200))
    );
    assert_eq!(split.eth.pln(), dec!(-200));

    // 1000 PLN more: base 3000, targets 1650 / 750 / 600, needs VWCE 450, crypto 0 (50 over),
    // gold 600 = 1050, above 1000: pro rata, VWCE 1000 x 450 / 1050 = 428.571..., gold
    // 1000 x 600 / 1050 = 571.428... VWCE ends 21.43 PLN short, gold 28.57, crypto 50 over.
    let r = report(&ledger, date, &inputs).unwrap();
    let t = r.top_up(&config, dec!(1000), false).unwrap();
    let buy = |s: Sleeve| t.buys[&s].round_dp(2);
    assert_eq!(
        [Sleeve::Vwce, Sleeve::Btc, Sleeve::Eth, Sleeve::Gold].map(buy),
        [dec!(428.57), dec!(0), dec!(0), dec!(571.43)]
    );
    let after = |c: AssetClass| t.after.by_class[&c].pln().round_dp(2);
    assert_eq!(
        AssetClass::ALL.map(after),
        [dec!(-21.43), dec!(50), dec!(-28.57)]
    );

    // With gold paused, VWCE and crypto are bought: needs 450 + 0 leave a surplus of 550, split
    // 0.55 : 0.25 of 0.80. VWCE 450 + 378.125 = 828.125, crypto 171.875. Crypto after 971.875:
    // BTC (800 held) is over its 728.906..., ETH needs 242.968..., so ETH takes all 171.875.
    // Gold would need its 600.
    let paused = Config::from_toml(&format!("{CONFIG}\n[paused]\ngold = true\n")).unwrap();
    let t = r.top_up(&paused, dec!(1000), false).unwrap();
    assert_eq!(
        [Sleeve::Vwce, Sleeve::Btc, Sleeve::Eth, Sleeve::Gold].map(|s| t.buys[&s]),
        [dec!(828.125), dec!(0), dec!(171.875), dec!(0)]
    );
    assert_eq!(
        t.would_need,
        BTreeMap::from([(AssetClass::Gold, dec!(600))])
    );
    // Including paused classes gives the first plan again.
    let all = r.top_up(&paused, dec!(1000), true).unwrap();
    assert_eq!(all.buys[&Sleeve::Gold].round_dp(2), dec!(571.43));

    // Without a VWCE price there is no allocation to show, and no plan.
    inputs.position_values.insert("VWCE".into(), None);
    let r = report(&ledger, date, &inputs).unwrap();
    let err = r.allocation(&config).unwrap_err();
    assert_eq!(err, AllocationError::Unpriced(vec!["VWCE".into()]));
    assert_eq!(
        r.top_up(&config, dec!(1000), false),
        Err(TopUpError::Allocation(err))
    );
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
    // Contributions: gold lost less than BTC; VWCE has no P&L and is not ranked. Without the
    // total there are no shares, but points of the return: -42.26942 / 3477.78 = -1.2154 pp.
    let ranked: Vec<&str> = r
        .attribution
        .ranked
        .iter()
        .map(|(a, _)| a.as_str())
        .collect();
    assert_eq!(ranked, [GOLD_COINS, "BTC"]);
    assert_eq!(r.attribution.unpriced, ["VWCE"]);
    let gold = r.attribution.ranked[0].1;
    assert_eq!(gold.share_of_total, None);
    assert_eq!(gold.return_points.unwrap().round_dp(6), dec!(-0.012154));
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

const VWCE_DAILY: &str = include_str!("fixtures/alphavantage/daily_vwce_tail.json");

fn test_av(base_url: String) -> AlphaVantageClient {
    AlphaVantageClient::new(base_url, "test-key")
        .unwrap()
        .with_request_spacing(std::time::Duration::from_millis(10))
}

/// XTB IKZE: 1516.5059 PLN on Mon 5 Oct and 756.69465 PLN on Sun 11 Oct; Kraken: 173.48 EUR on
/// Mon 12 Oct. Each buys a whole number of VWCE units in the benchmark (see the test).
async fn benchmark_ledger(pool: &SqlitePool) {
    let deposit = |day, amount, currency, id| row("deposit", day, None, None, amount, currency, id);
    insert_transactions(
        pool,
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
        pool,
        KRAKEN,
        &[],
        "kraken",
        &[deposit("2026-10-12", dec!(173.48), "EUR", "b3")],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn benchmark_puts_every_contribution_into_vwce_on_its_date() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/exchangerates/rates/a/eur/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .mount(&server)
        .await;
    // One request for every day: the rest come from the price cache.
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("function", "TIME_SERIES_DAILY"))
        .respond_with(ResponseTemplate::new(200).set_body_string(VWCE_DAILY))
        .expect(1)
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    benchmark_ledger(&pool).await;
    let ledger = load_ledger(&pool).await.unwrap();
    let date = d("2026-10-14");
    assert_eq!(
        requirements(&ledger, date).benchmark_days,
        BTreeSet::from([d("2026-10-05"), d("2026-10-11"), d("2026-10-12"), date])
    );
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let av = test_av(server.uri());
    let now: DateTime<Utc> = "2026-10-15T14:00:00Z".parse().unwrap();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: Some(&av),
        clock: Clock {
            now,
            today: now.date_naive(),
        },
        gold_buyback_factor: Decimal::ONE,
    };

    let r = portfolio_on(date, &sources).await.unwrap();

    // VWCE in PLN = Xetra close x NBP EUR rate of the close's day:
    //   5 Oct:  172.90 x 4.3855 = 758.25295; 1516.5059 buys 2 units.
    //   11 Oct (Sunday): Friday 9 Oct, 172.86 x 4.3775 = 756.69465; 756.69465 buys 1.
    //   12 Oct: 173.48 x 4.3889 = 761.386372; 173.48 EUR x 4.3889 = 761.386372 PLN buys 1.
    //   14 Oct: 172.48 x 4.3706 = 753.841088; 4 units are worth 3015.364352.
    let b = r.benchmark.as_ref().unwrap();
    assert_eq!(b.units, dec!(4));
    assert_eq!(b.value, dec!(3015.364352));
    // Contributions 1516.5059 + 756.69465 + 761.386372 = 3034.586922, as the portfolio's.
    assert_eq!(b.contributions, dec!(3034.586922));
    assert_eq!(b.contributions, r.contributions.total);
    assert_eq!(b.pnl(), dec!(-19.222570));
    // The portfolio is all cash: 2273.20055 PLN + 173.48 EUR x 4.3706 = 758.211688 PLN, so
    // 3031.412238 in total, 16.047886 more than VWCE (which fell over the week).
    assert_eq!(r.valuation.total, dec!(3031.412238));
    assert_eq!(b.outperformance(&r.pnl), Some(dec!(16.047886)));
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}

#[tokio::test]
async fn benchmark_stops_asking_alpha_vantage_once_it_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/exchangerates/rates/a/eur/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .mount(&server)
        .await;
    // Down: without the stop, each of the 4 days would retry (and spend the daily quota).
    Mock::given(method("GET"))
        .and(path("/query"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    benchmark_ledger(&pool).await;
    let ledger = load_ledger(&pool).await.unwrap();
    let date = d("2026-10-14");
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let av = test_av(server.uri());
    let now: DateTime<Utc> = "2026-10-15T14:00:00Z".parse().unwrap();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: Some(&av),
        clock: Clock {
            now,
            today: now.date_naive(),
        },
        gold_buyback_factor: Decimal::ONE,
    };

    let inputs = fetch_inputs(&requirements(&ledger, date), date, &sources).await;

    assert_eq!(inputs.vwce_prices.len(), 4);
    assert!(inputs.vwce_prices.values().all(Option::is_none));
    assert_eq!(inputs.problems.len(), 1, "{:?}", inputs.problems);
    assert!(
        inputs.problems[0]
            .starts_with("VWCE benchmark: no price for 4 of 4 days (first 2026-10-05: "),
        "{}",
        inputs.problems[0]
    );
    let r = report(&ledger, date, &inputs).unwrap();
    assert_eq!(
        r.benchmark,
        Err(BenchmarkError::MissingPrice(d("2026-10-05")))
    );
}

#[tokio::test]
async fn without_an_alpha_vantage_key_the_benchmark_has_no_prices() {
    let pool = open_in_memory().await.unwrap();
    benchmark_ledger(&pool).await;
    let ledger = load_ledger(&pool).await.unwrap();
    let date = d("2026-10-14");
    let req = requirements(&ledger, date);
    let inputs = Inputs {
        flow_rates: req
            .flow_rates
            .iter()
            .map(|k| (k.clone(), Some(dec!(4))))
            .collect(),
        vwce_prices: req.benchmark_days.iter().map(|d| (*d, None)).collect(),
        ..Inputs::default()
    };

    let r = report(&ledger, date, &inputs).unwrap();

    assert_eq!(
        r.benchmark,
        Err(BenchmarkError::MissingPrice(d("2026-10-05")))
    );
}

#[tokio::test]
async fn benchmark_fetches_the_weekly_history_for_days_before_the_daily_closes() {
    const VWCE_WEEKLY: &str = include_str!("fixtures/alphavantage/weekly_vwce_recent.json");
    // Synthetic NBP answer for the window up to Fri 11 Sep (the fixtures cover October only).
    const EUR_SEP_11: &str = r#"{"table":"A","currency":"euro","code":"EUR","rates":[{"no":"176/A/NBP/2026","effectiveDate":"2026-09-11","mid":4.25}]}"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/exchangerates/rates/a/eur/2026-08-28/2026-09-11/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_SEP_11))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/exchangerates/rates/a/eur/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EUR_OCT))
        .mount(&server)
        .await;
    // Daily closes start on 28 Sep; the weekly series is fetched once for the older day.
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
        .respond_with(ResponseTemplate::new(200).set_body_string(VWCE_WEEKLY))
        .expect(1)
        .mount(&server)
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
        &[],
        "xtb",
        &[row(
            "deposit",
            "2026-09-14",
            None,
            None,
            dec!(1418.31),
            "PLN",
            "w1",
        )],
    )
    .await
    .unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    let av = test_av(server.uri());
    let now: DateTime<Utc> = "2026-10-15T14:00:00Z".parse().unwrap();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: Some(&av),
        clock: Clock {
            now,
            today: now.date_naive(),
        },
        gold_buyback_factor: Decimal::ONE,
    };
    let date = d("2026-10-14");

    let r = portfolio_on(date, &sources).await.unwrap();

    // Mon 14 Sep uses the week-end close of Fri 11 Sep: 166.86 x 4.25 = 709.155, so 1418.31
    // buys 2 units. On 14 Oct they are worth 2 x 753.841088 = 1507.682176.
    let b = r.benchmark.as_ref().unwrap();
    assert_eq!((b.units, b.value), (dec!(2), dec!(1507.682176)));
    assert!(r.problems.is_empty(), "{:?}", r.problems);

    // The history is stored: a second report needs no Alpha Vantage request (the expectations
    // above are checked when the server is dropped).
    let again = portfolio_on(date, &sources).await.unwrap();
    assert_eq!(again.benchmark, r.benchmark);
}

#[tokio::test]
async fn while_kraken_and_nbp_are_down_today_uses_the_last_stored_prices() {
    use portfolio_core::fx::FxRate;
    use portfolio_core::price::DailyClose;
    use portfolio_db::fx::upsert_rates;
    use portfolio_db::prices::{ensure_asset, upsert_closes};

    // NBP answers 404 for a range without data, which counts as "no closes": a 500 is down.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();
    kraken_ledger(&pool).await;
    let coin = NewCoin {
        label: None,
        purchased_on: d("2026-10-01"),
        purchase_price_pln: dec!(1600),
        fine_weight_g: dec!(3.11),
        manual_quote_pln: None,
    };
    add_coin(&pool, &coin).await.unwrap();
    // Wednesday's closes, fetched that evening: BTC 60 000 EUR, gold 500 PLN a gram; 4 PLN/EUR.
    let fetched: DateTime<Utc> = "2026-10-14T20:00:00Z".parse().unwrap();
    for (symbol, name, class, currency, source, close) in [
        (
            "BTC",
            "Bitcoin",
            AssetClass::Crypto,
            "EUR",
            "kraken",
            dec!(60000),
        ),
        (
            "XAU_G",
            "Gold, PLN per gram (NBP)",
            AssetClass::Gold,
            "PLN",
            "nbp",
            dec!(500),
        ),
    ] {
        let id = ensure_asset(&pool, symbol, name, class, currency)
            .await
            .unwrap();
        let close = DailyClose {
            date: d("2026-10-14"),
            close,
        };
        upsert_closes(&pool, id, currency, source, &[close], fetched)
            .await
            .unwrap();
    }
    let rates: Vec<FxRate> = ["2026-10-12", "2026-10-13", "2026-10-14"]
        .into_iter()
        .map(|day| FxRate {
            currency: "EUR".into(),
            effective_date: d(day),
            mid: dec!(4),
            table_no: "200/A/NBP/2026".into(),
        })
        .collect();
    upsert_rates(&pool, &rates).await.unwrap();
    let ledger = load_ledger(&pool).await.unwrap();
    let nbp = NbpClient::with_base_url(server.uri()).unwrap();
    let kraken = KrakenClient::with_base_url(server.uri()).unwrap();
    // Thursday 14:00: 18 h after the last fetch, so both are asked again, and both fail.
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
    let today = d("2026-10-15");

    let inputs = fetch_inputs(&requirements(&ledger, today), today, &sources).await;

    // BTC: 0.002 x 60 000 EUR x 4 = 480 PLN. Gold: 500 x 3.11 x 0.95 = 1477.25 PLN.
    assert_eq!(
        inputs.position_values,
        BTreeMap::from([
            ("BTC".into(), Some(dec!(480))),
            (GOLD_COINS.into(), Some(dec!(1477.25))),
        ])
    );
    let last: Vec<_> = inputs
        .last_known
        .iter()
        .map(|(asset, l)| (asset.as_str(), l.provider, l.fetched_at))
        .collect();
    assert_eq!(
        last,
        [
            ("BTC", "kraken", Some(fetched)),
            (GOLD_COINS, "nbp", Some(fetched)),
        ]
    );
    let r = report(&ledger, today, &inputs).unwrap();
    assert_eq!(r.last_known, inputs.last_known);

    // A past day is never valued at an older close: Kraken's failure leaves 13 Oct without BTC.
    let yesterday = fetch_inputs(
        &requirements(&ledger, d("2026-10-13")),
        d("2026-10-13"),
        &sources,
    )
    .await;
    assert_eq!(yesterday.position_values.get("BTC"), Some(&None));
    assert!(yesterday.last_known.is_empty());
}
