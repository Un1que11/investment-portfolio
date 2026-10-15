use std::str::FromStr;

use portfolio_db::{DbDecimal, MIGRATOR, open, open_in_memory};
use rust_decimal::Decimal;
use sqlx::SqlitePool;

fn dec(s: &str) -> DbDecimal {
    DbDecimal(Decimal::from_str(s).unwrap())
}

async fn db() -> SqlitePool {
    open_in_memory()
        .await
        .expect("in-memory database with migrations")
}

/// Account 1 (XTB IKZE) and asset 1 (VWCE), the minimum most rows need.
async fn seeded() -> SqlitePool {
    let pool = db().await;
    sqlx::query(
        "INSERT INTO accounts (id, name, kind, provider, currency)
         VALUES (1, 'XTB IKZE', 'ikze', 'xtb', 'PLN')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO assets (id, symbol, name, asset_class, currency)
         VALUES (1, 'VWCE', 'Vanguard FTSE All-World', 'vwce', 'EUR')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn insert_tx(
    pool: &SqlitePool,
    kind: &str,
    asset_id: Option<i64>,
    quantity: Option<&str>,
    unit_price: Option<&str>,
    amount: &str,
    external_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO transactions
           (account_id, asset_id, kind, executed_at, quantity, unit_price, amount, currency,
            source, external_id)
         VALUES (1, ?, ?, '2026-01-15T10:00:00Z', ?, ?, ?, 'EUR', 'xtb', ?)",
    )
    .bind(asset_id)
    .bind(kind)
    .bind(quantity.map(dec))
    .bind(unit_price.map(dec))
    .bind(dec(amount))
    .bind(external_id)
    .execute(pool)
    .await
    .map(|_| ())
}

fn is_check_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .is_some_and(|e| e.message().contains("CHECK constraint failed"))
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .is_some_and(|e| e.is_unique_violation())
}

#[tokio::test]
async fn migrations_create_all_tables() {
    let pool = db().await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx%'
         ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        tables,
        [
            "accounts",
            "assets",
            "fx_rate_days",
            "fx_rates",
            "gold_coins",
            "prices",
            "snapshots",
            "transactions"
        ]
    );
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let pool = db().await;
    MIGRATOR.run(&pool).await.expect("second run is a no-op");
}

#[tokio::test]
async fn open_creates_file_database_and_reopens_it() {
    let dir = std::env::temp_dir().join(format!("portfolio-db-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("portfolio.db");
    let url = format!("sqlite://{}", path.display());

    let pool = open(&url).await.unwrap();
    sqlx::query("INSERT INTO assets (symbol, name, asset_class, currency) VALUES ('BTC', 'Bitcoin', 'crypto', 'PLN')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let pool = open(&url).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    std::fs::remove_dir_all(&dir).unwrap();

    assert_eq!(count, 1);
}

#[tokio::test]
async fn decimal_round_trips_exactly_with_scale() {
    let pool = seeded().await;
    // 0.1 + 0.2 style values and long scales that a float would mangle.
    for (i, value) in [
        "0.1",
        "0.30000000000000000001",
        "12.3400",
        "1234567.89",
        "0.00000001",
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query(
            "INSERT INTO prices (asset_id, price_date, price, currency, source)
             VALUES (1, ?, ?, 'EUR', 'stooq')",
        )
        .bind(format!("2026-01-{:02}", i + 1))
        .bind(dec(value))
        .execute(&pool)
        .await
        .unwrap();

        let (stored_text, read_back): (String, DbDecimal) =
            sqlx::query_as("SELECT price, price FROM prices WHERE asset_id = 1 AND price_date = ?")
                .bind(format!("2026-01-{:02}", i + 1))
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(stored_text, value);
        assert_eq!(read_back, dec(value));
        assert_eq!(read_back.0.scale(), dec(value).0.scale());
    }
}

#[tokio::test]
async fn float_values_are_rejected_in_decimal_columns() {
    let pool = seeded().await;
    // SQLite renders this REAL as '1.0e-07' in a TEXT column; the CHECK must refuse it.
    let err = sqlx::query(
        "INSERT INTO prices (asset_id, price_date, price, currency, source)
         VALUES (1, '2026-01-01', ?, 'EUR', 'stooq')",
    )
    .bind(1e-7_f64)
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(is_check_violation(&err), "{err}");
}

#[tokio::test]
async fn decoding_exponent_notation_fails() {
    let pool = db().await;
    let result: Result<DbDecimal, _> = sqlx::query_scalar("SELECT '1.0e-07'")
        .fetch_one(&pool)
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn malformed_decimal_strings_are_rejected() {
    let pool = seeded().await;
    for bad in ["", "abc", "1.2.3", " 1", "-5", "1,5"] {
        let err = sqlx::query(
            "INSERT INTO prices (asset_id, price_date, price, currency, source)
             VALUES (1, '2026-01-01', ?, 'EUR', 'stooq')",
        )
        .bind(bad)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(is_check_violation(&err), "{bad:?}: {err}");
    }
}

#[tokio::test]
async fn all_transaction_kinds_are_accepted() {
    let pool = seeded().await;
    insert_tx(&pool, "deposit", None, None, None, "1000.00", "t1")
        .await
        .unwrap();
    insert_tx(
        &pool,
        "buy",
        Some(1),
        Some("7"),
        Some("135.20"),
        "946.40",
        "t2",
    )
    .await
    .unwrap();
    insert_tx(&pool, "fee", None, None, None, "0.50", "t3")
        .await
        .unwrap();
    insert_tx(&pool, "dividend", Some(1), None, None, "3.10", "t4")
        .await
        .unwrap();
    insert_tx(
        &pool,
        "sell",
        Some(1),
        Some("2"),
        Some("140.00"),
        "280.00",
        "t5",
    )
    .await
    .unwrap();
    insert_tx(&pool, "withdrawal", None, None, None, "100.00", "t6")
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6);
}

#[tokio::test]
async fn unknown_transaction_kind_is_rejected() {
    let pool = seeded().await;
    let err = insert_tx(&pool, "transfer", None, None, None, "1.00", "t1")
        .await
        .unwrap_err();
    assert!(is_check_violation(&err), "{err}");
}

#[tokio::test]
async fn trades_require_asset_quantity_and_price() {
    let pool = seeded().await;
    let cases = [
        (None, Some("1"), Some("100")),
        (Some(1), None, Some("100")),
        (Some(1), Some("1"), None),
    ];
    for (i, (asset, qty, price)) in cases.into_iter().enumerate() {
        for kind in ["buy", "sell"] {
            let err = insert_tx(&pool, kind, asset, qty, price, "100", &format!("{kind}{i}"))
                .await
                .unwrap_err();
            assert!(is_check_violation(&err), "{kind} case {i}: {err}");
        }
    }
}

#[tokio::test]
async fn dividend_requires_asset() {
    let pool = seeded().await;
    let err = insert_tx(&pool, "dividend", None, None, None, "3.10", "d1")
        .await
        .unwrap_err();
    assert!(is_check_violation(&err), "{err}");
}

#[tokio::test]
async fn zero_quantity_is_rejected() {
    let pool = seeded().await;
    let err = insert_tx(&pool, "buy", Some(1), Some("0.000"), Some("100"), "0", "z1")
        .await
        .unwrap_err();
    assert!(is_check_violation(&err), "{err}");
}

#[tokio::test]
async fn reimporting_the_same_operation_is_rejected() {
    let pool = seeded().await;
    insert_tx(&pool, "deposit", None, None, None, "500", "xtb-row-hash")
        .await
        .unwrap();
    let err = insert_tx(&pool, "deposit", None, None, None, "500", "xtb-row-hash")
        .await
        .unwrap_err();
    assert!(is_unique_violation(&err), "{err}");
}

#[tokio::test]
async fn foreign_keys_are_enforced() {
    let pool = seeded().await;
    let err = insert_tx(&pool, "dividend", Some(999), None, None, "1", "fk1")
        .await
        .unwrap_err();
    assert!(
        err.as_database_error()
            .is_some_and(|e| e.is_foreign_key_violation()),
        "{err}"
    );
}

#[tokio::test]
async fn invalid_dates_and_timestamps_are_rejected() {
    let pool = seeded().await;
    for bad_date in ["2026-02-30", "15.01.2026", "2026-1-5", ""] {
        let err = sqlx::query(
            "INSERT INTO fx_rates (currency, rate_date, rate, table_no)
             VALUES ('EUR', ?, '4.2512', '001/A/NBP/2026')",
        )
        .bind(bad_date)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(is_check_violation(&err), "{bad_date}: {err}");
    }

    for bad_ts in [
        "yesterday",
        "2026-01-15",
        "2026-01-15 10:00:00",
        "2026-01-15T10:00:00+01:00",
    ] {
        let err = sqlx::query(
            "INSERT INTO transactions (account_id, kind, executed_at, amount, currency, source, external_id)
             VALUES (1, 'deposit', ?, '1', 'PLN', 'manual', ?)",
        )
        .bind(bad_ts)
        .bind(bad_ts)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(is_check_violation(&err), "{bad_ts}: {err}");
    }
}

#[tokio::test]
async fn fx_rate_is_unique_per_currency_and_day_and_never_pln() {
    let pool = db().await;
    let insert = |currency: &'static str, rate: &'static str| {
        sqlx::query(
            "INSERT INTO fx_rates (currency, rate_date, rate, table_no)
             VALUES (?, '2026-10-14', ?, '199/A/NBP/2026')",
        )
        .bind(currency)
        .bind(dec(rate))
        .execute(&pool)
    };

    insert("EUR", "4.2512").await.unwrap();
    assert!(is_unique_violation(
        &insert("EUR", "4.3000").await.unwrap_err()
    ));
    assert!(is_check_violation(&insert("PLN", "1").await.unwrap_err()));
    assert!(is_check_violation(&insert("USD", "0").await.unwrap_err()));
}

#[tokio::test]
async fn snapshot_invested_amount_may_be_negative_but_value_may_not() {
    let pool = seeded().await;
    let insert = |value: &'static str, invested: &'static str| {
        sqlx::query(
            "INSERT INTO snapshots (snapshot_date, asset_id, quantity, value_pln, invested_pln)
             VALUES ('2026-10-14', 1, '5', ?, ?)",
        )
        .bind(value)
        .bind(invested)
        .execute(&pool)
    };

    assert!(is_check_violation(&insert("-1", "100").await.unwrap_err()));
    assert!(is_check_violation(&insert("100", "1-0").await.unwrap_err()));
    insert("2900.50", "-120.25").await.unwrap();
}

#[tokio::test]
async fn currency_codes_must_be_upper_case_iso() {
    let pool = db().await;
    for bad in ["pln", "PL", "EURO"] {
        let err = sqlx::query(
            "INSERT INTO accounts (name, kind, provider, currency) VALUES (?, 'exchange', 'kraken', ?)",
        )
        .bind(format!("Kraken {bad}"))
        .bind(bad)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(is_check_violation(&err), "{bad}: {err}");
    }
}

#[tokio::test]
async fn prices_accept_alphavantage_sources_and_reject_unknown_ones() {
    let pool = seeded().await;
    let insert = |source: &'static str, date: &'static str| {
        sqlx::query(
            "INSERT INTO prices (asset_id, price_date, price, currency, source)
             VALUES (1, ?, '170.12', 'EUR', ?)",
        )
        .bind(date)
        .bind(source)
        .execute(&pool)
    };

    insert("alphavantage", "2026-10-09").await.unwrap();
    insert("alphavantage_weekly", "2026-10-02").await.unwrap();
    let err = insert("yahoo", "2026-10-12").await.unwrap_err();
    assert!(is_check_violation(&err), "{err}");
}

#[tokio::test]
async fn prices_rebuild_migration_keeps_existing_rows() {
    // Apply only the first two migrations, store a price, then apply the rest.
    let options = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let first_two = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(MIGRATOR.iter().take(2).cloned().collect()),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    first_two.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO assets (id, symbol, name, asset_class, currency)
         VALUES (1, 'VWCE', 'Vanguard FTSE All-World', 'vwce', 'EUR')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO prices (asset_id, price_date, price, currency, source, fetched_at)
         VALUES (1, '2026-10-01', '168.4000', 'EUR', 'stooq', '2026-10-01T18:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    MIGRATOR.run(&pool).await.unwrap();

    let row: (String, DbDecimal, String, String) = sqlx::query_as(
        "SELECT price_date, price, source, fetched_at FROM prices WHERE asset_id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "2026-10-01".to_owned(),
            dec("168.4000"),
            "stooq".to_owned(),
            "2026-10-01T18:00:00Z".to_owned()
        )
    );
}
