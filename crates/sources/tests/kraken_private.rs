use std::str::FromStr;
use std::time::Duration;

use chrono::NaiveDateTime;
use portfolio_db::kraken::all_entries;
use portfolio_db::open_in_memory;
use portfolio_sources::kraken_ledger::{map_entries, sync};
use portfolio_sources::kraken_private::{
    KrakenPrivateClient, KrakenPrivateError, parse_balance, parse_ledgers, sign,
};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

// Synthetic data with the structure of a real account's ledger (types, subtypes, asset codes,
// refid grouping); ids and amounts are made up.
const PAGE1: &str = include_str!("fixtures/kraken/ledgers_page1.json");
const PAGE2: &str = include_str!("fixtures/kraken/ledgers_page2.json");
const INCREMENTAL: &str = include_str!("fixtures/kraken/ledgers_incremental.json");
const INVALID_KEY: &str = include_str!("fixtures/kraken/error_invalid_key.json");
const BALANCE: &str = include_str!("fixtures/kraken/balance.json");

/// The example key from Kraken's API documentation (not a real key).
const DOC_SECRET: &str =
    "kQH5HW/8p1uGOVjbgWA7FunAmGO8lsSUXNsu3eow76sz84Q18fWxnyRzBHCd3pd5nE9qa99HAZtuZuj6F1huXg==";

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn at(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
}

fn secret() -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(DOC_SECRET)
        .unwrap()
}

#[test]
fn signature_matches_kraken_documentation_example() {
    // Example request from Kraken's REST authentication guide (an AddOrder that this client
    // never sends; only the signing algorithm is checked here).
    let signature = sign(
        "/0/private/AddOrder",
        1616492376594,
        "nonce=1616492376594&ordertype=limit&pair=XBTUSD&price=37500&type=buy&volume=1.25",
        &secret(),
    );

    assert_eq!(
        signature,
        "4/dpxb3iT4tp/ZCVEwSnEsLxx0bqyhLpdfOpc6fn7OR8+UClSV5n9E6aSS8MPtnRfp32bAb0nmbRn6H8ndwLUQ=="
    );
}

#[test]
fn signature_for_a_ledgers_request() {
    // Cross-checked with an independent implementation that authenticated against the live API.
    assert_eq!(
        sign(
            "/0/private/Ledgers",
            1700000000000,
            "nonce=1700000000000&ofs=0",
            &secret()
        ),
        "VbnKQqyBr6e05ngWe9BodsMQcMTqymgRJjEil7XPHH3TXk13NHjebdNgiQh4ytcQRwoXZS2qiXW02ZdoFGJXjg=="
    );
}

#[test]
fn parses_ledger_page_newest_first_with_exact_text() {
    let page = parse_ledgers(PAGE1).unwrap();

    assert_eq!((page.entries.len(), page.count), (10, 18));
    assert_eq!(page.entries[0].id, "LAAAAA-AAAAA-AAAA18");
    assert_eq!(page.entries[0].kind, "withdrawal");
    // Numbers and times keep their exact text (no f64 round trip).
    assert_eq!(page.entries[0].amount, "-0.0005000000");
    assert_eq!(page.entries[0].fee, "0.0000100000");
    assert!(
        page.entries
            .windows(2)
            .all(|w| w[0].time.parse::<f64>().unwrap() >= w[1].time.parse::<f64>().unwrap())
    );
    let bundle = page
        .entries
        .iter()
        .find(|e| e.kind == "bundletransfer")
        .unwrap();
    assert!(bundle.subtype.starts_with("dissolve"));
}

#[test]
fn parses_balance_and_errors() {
    let balance = parse_balance(BALANCE).unwrap();
    assert_eq!(balance["XXBT"], "0.0044900000");
    assert_eq!(balance.len(), 4);

    assert!(
        matches!(parse_ledgers(INVALID_KEY), Err(KrakenPrivateError::Api(e)) if e == ["EAPI:Invalid key"])
    );
    assert!(matches!(
        parse_balance(INVALID_KEY),
        Err(KrakenPrivateError::Api(_))
    ));
    assert!(matches!(
        parse_ledgers("<html>"),
        Err(KrakenPrivateError::Parse(_))
    ));
}

#[test]
fn mapping_covers_every_pattern() {
    let mut entries: Vec<_> = parse_ledgers(PAGE1).unwrap().entries;
    entries.extend(parse_ledgers(PAGE2).unwrap().entries);
    let raw: Vec<_> = entries.into_iter().map(Into::into).collect();

    let mapped = map_entries(&raw).unwrap();
    let find = |id: &str| {
        mapped
            .transactions
            .iter()
            .find(|t| t.external_id == id)
            .unwrap()
    };

    // 3 deposits + 2 deposit fees; 4 buys + 4 purchase fees; 2 staking rewards.
    assert_eq!(mapped.transactions.len(), 15);
    assert_eq!(mapped.ignored, ["RAAAAA-AAAAA-AAAAA9"]);
    assert_eq!(mapped.unsupported.len(), 1);
    assert_eq!(mapped.unsupported[0].entries, ["withdrawal/XXBT/-"]);

    let deposit = find("kraken-LAAAAA-AAAAA-AAAAA2");
    assert_eq!(
        (deposit.kind, deposit.amount, deposit.currency.as_str()),
        ("deposit", dec("300.0000"), "EUR")
    );
    assert_eq!(deposit.executed_at, at("2026-01-02 00:00:00"));
    assert_eq!(find("kraken-LAAAAA-AAAAA-AAAAA2-fee").amount, dec("1.5000"));

    // Instant Buy: 250 EUR for 0.004 BTC -> 62500.00 EUR/BTC; 3.75 EUR fee on the spend.
    let buy = find("kraken-LAAAAA-AAAAA-AAAAA4");
    assert_eq!(
        (
            buy.kind,
            buy.asset,
            buy.quantity,
            buy.unit_price,
            buy.amount
        ),
        (
            "buy",
            Some("BTC"),
            Some(dec("0.004")),
            Some(dec("62500.00")),
            dec("250.0000")
        )
    );
    assert_eq!(find("kraken-LAAAAA-AAAAA-AAAAA3-fee").amount, dec("3.7500"));
    assert_eq!(find("kraken-LAAAAA-AAAAA-AAAAA3-fee").asset, Some("BTC"));

    // Bundle buy pairs in time order: ETH with the 120 EUR spend, BTC with the 60 EUR one.
    let eth = find("kraken-LAAAAA-AAAAA-AAAAA5");
    let btc = find("kraken-LAAAAA-AAAAA-AAAAA7");
    assert_eq!(
        (eth.asset, eth.amount, eth.unit_price),
        (Some("ETH"), dec("120.0000"), Some(dec("2400.00")))
    );
    assert_eq!(
        (btc.asset, btc.amount, btc.unit_price),
        (Some("BTC"), dec("60.0000"), Some(dec("60000.00")))
    );

    // Card purchase via EUR.HOLD counts as EUR.
    let held = find("kraken-LAAAAA-AAAAA-AAAAA9");
    assert_eq!(
        (held.kind, held.currency.as_str(), held.amount),
        ("deposit", "EUR", dec("100.0000"))
    );
    assert_eq!(find("kraken-LAAAAA-AAAAA-AAAA10-fee").amount, dec("1.4700"));
    let card_buy = find("kraken-LAAAAA-AAAAA-AAAA11");
    assert_eq!(
        (
            card_buy.currency.as_str(),
            card_buy.amount,
            card_buy.unit_price
        ),
        ("EUR", dec("98.0000"), Some(dec("2450.00")))
    );

    // Staking: 0.0001 - 0.000025 commission = 0.000075 ETH, income in kind.
    let reward = find("kraken-LAAAAA-AAAAA-AAAA12");
    assert_eq!(
        (
            reward.kind,
            reward.asset,
            reward.quantity,
            reward.amount,
            reward.currency.as_str()
        ),
        (
            "dividend",
            Some("ETH"),
            Some(dec("0.000075")),
            dec("0.000075"),
            "ETH"
        )
    );
}

#[test]
fn unbalanced_bundle_or_unpaired_buy_is_unsupported() {
    let mut entries: Vec<_> = parse_ledgers(PAGE1).unwrap().entries;
    entries.extend(parse_ledgers(PAGE2).unwrap().entries);
    // Drop one leg of the bundle dissolve and one spend of the bundle buy.
    entries.retain(|e| e.id != "LAAAAA-AAAAA-AAAA15" && e.id != "LAAAAA-AAAAA-AAAAA8");
    let raw: Vec<_> = entries.into_iter().map(Into::into).collect();

    let mapped = map_entries(&raw).unwrap();
    let unsupported: Vec<_> = mapped
        .unsupported
        .iter()
        .map(|u| u.refid.as_str())
        .collect();

    assert!(unsupported.contains(&"RAAAAA-AAAAA-AAAAA9"));
    assert!(unsupported.contains(&"RAAAAA-AAAAA-AAAAA4"));
    assert!(mapped.ignored.is_empty());
}

/// Accepts only requests whose API-Sign is correct for their path, nonce and body.
struct SignedWith(Vec<u8>);

impl Match for SignedWith {
    fn matches(&self, request: &Request) -> bool {
        let body = String::from_utf8_lossy(&request.body).to_string();
        let Some(nonce) = body
            .split('&')
            .find_map(|kv| kv.strip_prefix("nonce="))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            return false;
        };
        let expected = sign(request.url.path(), nonce, &body, &self.0);
        request
            .headers
            .get("API-Key")
            .is_some_and(|k| k == "test-api-key")
            && request
                .headers
                .get("API-Sign")
                .is_some_and(|s| s == expected.as_str())
    }
}

async fn server_with_ledger_pages() -> MockServer {
    let server = MockServer::start().await;
    for (ofs, body) in [("ofs=0", PAGE1), ("ofs=10", PAGE2)] {
        Mock::given(method("POST"))
            .and(path("/0/private/Ledgers"))
            .and(body_string_contains(ofs))
            .and(SignedWith(secret()))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
    }
    server
}

fn client(server: &MockServer) -> KrakenPrivateClient {
    KrakenPrivateClient::new(server.uri(), "test-api-key", DOC_SECRET).unwrap()
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn first_sync_fetches_all_pages_stores_raw_and_maps() {
    let server = server_with_ledger_pages().await;
    let pool = open_in_memory().await.unwrap();

    let summary = sync(&pool, &client(&server), Duration::ZERO).await.unwrap();

    assert_eq!((summary.fetched, summary.new_entries), (18, 18));
    assert_eq!(summary.transactions_written, 15);
    assert_eq!(summary.ignored_groups, 1);
    assert_eq!(summary.unsupported.len(), 1);
    // The unsupported withdrawal is still kept raw.
    assert_eq!(count(&pool, "kraken_ledger").await, 18);
    assert_eq!(count(&pool, "transactions").await, 15);
    let account: (String, String, String) =
        sqlx::query_as("SELECT name, kind, provider FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        account,
        ("Kraken".into(), "exchange".into(), "kraken".into())
    );
}

#[tokio::test]
async fn next_sync_starts_after_newest_entry_and_adds_only_new() {
    let server = server_with_ledger_pages().await;
    let pool = open_in_memory().await.unwrap();
    sync(&pool, &client(&server), Duration::ZERO).await.unwrap();
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/0/private/Ledgers"))
        .and(body_string_contains("start=LAAAAA-AAAAA-AAAA18"))
        .and(SignedWith(secret()))
        .respond_with(ResponseTemplate::new(200).set_body_string(INCREMENTAL))
        .expect(1)
        .mount(&server)
        .await;

    let summary = sync(&pool, &client(&server), Duration::ZERO).await.unwrap();

    assert_eq!(
        (
            summary.fetched,
            summary.new_entries,
            summary.transactions_written
        ),
        (1, 1, 1)
    );
    assert_eq!(count(&pool, "kraken_ledger").await, 19);
    assert_eq!(count(&pool, "transactions").await, 16);
    assert_eq!(
        all_entries(&pool).await.unwrap().last().unwrap().ledger_id,
        "LAAAAA-AAAAA-AAAA19"
    );
}

#[tokio::test]
async fn api_error_is_reported_and_nothing_is_stored() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(INVALID_KEY))
        .mount(&server)
        .await;
    let pool = open_in_memory().await.unwrap();

    let err = sync(&pool, &client(&server), Duration::ZERO)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("EAPI:Invalid key"), "{err}");
    assert_eq!(count(&pool, "kraken_ledger").await, 0);
}

#[tokio::test]
async fn balance_is_read_with_a_signed_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/0/private/Balance"))
        .and(SignedWith(secret()))
        .respond_with(ResponseTemplate::new(200).set_body_string(BALANCE))
        .expect(1)
        .mount(&server)
        .await;

    let balance = client(&server).balance().await.unwrap();

    assert_eq!(balance["ZEUR"], "362.0500");
}

#[test]
fn keys_never_show_in_debug_output_and_bad_keys_are_rejected() {
    let client =
        KrakenPrivateClient::new("http://127.0.0.1:9", "my-public-key", DOC_SECRET).unwrap();
    let debug = format!("{client:?}");
    assert!(!debug.contains("my-public-key") && !debug.contains(&DOC_SECRET[..10]));

    assert!(matches!(
        KrakenPrivateClient::new("http://x", "k", "not base64!"),
        Err(KrakenPrivateError::InvalidKey)
    ));
}

#[tokio::test]
async fn nonces_strictly_increase() {
    // Two requests in the same millisecond must still get different, increasing nonces.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(SignedWith(secret()))
        .respond_with(ResponseTemplate::new(200).set_body_string(BALANCE))
        .expect(2)
        .mount(&server)
        .await;
    let client = client(&server);

    client.balance().await.unwrap();
    client.balance().await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let nonces: Vec<u64> = requests
        .iter()
        .map(|r| {
            String::from_utf8_lossy(&r.body)
                .split('&')
                .find_map(|kv| kv.strip_prefix("nonce=").map(|n| n.parse().unwrap()))
                .unwrap()
        })
        .collect();
    assert!(nonces[1] > nonces[0]);
}
