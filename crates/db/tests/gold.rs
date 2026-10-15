use std::str::FromStr;

use chrono::NaiveDate;
use portfolio_db::gold::{
    ACCOUNT_NAME, ASSET_SYMBOL, NewCoin, add_coin, delete_coin, get_coin, list_coins,
};
use portfolio_db::{DbDecimal, open_in_memory};
use rust_decimal::Decimal;
use sqlx::SqlitePool;

fn d(s: &str) -> NaiveDate {
    NaiveDate::from_str(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn coin(date: &str, price: &str) -> NewCoin {
    NewCoin {
        label: Some("Krugerrand 1/10 oz".into()),
        purchased_on: d(date),
        purchase_price_pln: dec(price),
        fine_weight_g: dec("3.11"),
        manual_quote_pln: None,
    }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn add_coin_stores_coin_and_purchase_transaction() {
    let pool = open_in_memory().await.unwrap();

    let added = add_coin(&pool, &coin("2025-03-14", "1234.56"))
        .await
        .unwrap();

    assert_eq!(
        get_coin(&pool, added.id).await.unwrap(),
        Some(added.clone())
    );
    let tx: (
        String,
        String,
        String,
        DbDecimal,
        DbDecimal,
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT t.kind, t.executed_at, t.quantity, t.unit_price, t.amount, t.currency,
                    t.external_id, a.name, s.symbol
             FROM transactions t
             JOIN accounts a ON a.id = t.account_id
             JOIN assets s ON s.id = t.asset_id
             WHERE t.id = ?",
    )
    .bind(added.transaction_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        tx,
        (
            "buy".into(),
            "2025-03-14T00:00:00Z".into(),
            "1".into(),
            DbDecimal(dec("1234.56")),
            DbDecimal(dec("1234.56")),
            "PLN".into(),
            format!("gold-coin-{}", added.id),
            ACCOUNT_NAME.into(),
            ASSET_SYMBOL.into(),
        )
    );
}

#[tokio::test]
async fn account_and_asset_are_created_once() {
    let pool = open_in_memory().await.unwrap();

    add_coin(&pool, &coin("2025-03-14", "1234.56"))
        .await
        .unwrap();
    add_coin(&pool, &coin("2026-01-20", "1600")).await.unwrap();

    assert_eq!(count(&pool, "accounts").await, 1);
    assert_eq!(count(&pool, "assets").await, 1);
    assert_eq!(count(&pool, "transactions").await, 2);
}

#[tokio::test]
async fn list_is_ordered_by_purchase_date_and_keeps_exact_values() {
    let pool = open_in_memory().await.unwrap();
    add_coin(&pool, &coin("2026-01-20", "1600")).await.unwrap();
    let mut with_quote = coin("2025-03-14", "1234.56");
    with_quote.manual_quote_pln = Some(dec("1650.00"));
    with_quote.label = None;
    add_coin(&pool, &with_quote).await.unwrap();

    let coins = list_coins(&pool).await.unwrap();

    assert_eq!(
        coins.iter().map(|c| c.purchased_on).collect::<Vec<_>>(),
        [d("2025-03-14"), d("2026-01-20")]
    );
    assert_eq!(coins[0].manual_quote_pln.unwrap().to_string(), "1650.00");
    assert_eq!(coins[0].label, None);
    assert_eq!(coins[0].fine_weight_g, dec("3.11"));
}

#[tokio::test]
async fn delete_removes_coin_and_transaction() {
    let pool = open_in_memory().await.unwrap();
    let first = add_coin(&pool, &coin("2025-03-14", "1234.56"))
        .await
        .unwrap();
    let second = add_coin(&pool, &coin("2026-01-20", "1600")).await.unwrap();

    assert!(delete_coin(&pool, first.id).await.unwrap());
    assert!(!delete_coin(&pool, first.id).await.unwrap());

    assert_eq!(list_coins(&pool).await.unwrap(), [second]);
    assert_eq!(count(&pool, "transactions").await, 1);
}

#[tokio::test]
async fn invalid_values_are_rejected_and_nothing_is_written() {
    let pool = open_in_memory().await.unwrap();
    let mut zero_weight = coin("2025-03-14", "1234.56");
    zero_weight.fine_weight_g = dec("0");
    let mut long_label = coin("2025-03-14", "1234.56");
    long_label.label = Some("x".repeat(101));

    for bad in [zero_weight, long_label] {
        assert!(add_coin(&pool, &bad).await.is_err());
    }
    // The failed inserts were rolled back, purchase transactions included.
    assert_eq!(count(&pool, "transactions").await, 0);
    assert_eq!(count(&pool, "gold_coins").await, 0);
}
