//! Raw Kraken ledger entries (`kraken_ledger`).

use sqlx::SqlitePool;

/// A ledger entry as stored; numbers are exact decimal text.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RawLedgerEntry {
    pub ledger_id: String,
    pub refid: String,
    pub time_unix: String,
    pub kind: String,
    pub subtype: String,
    pub aclass: String,
    pub asset: String,
    pub amount: String,
    pub fee: String,
    pub balance: String,
}

/// Stores entries that are not stored yet; returns how many were new.
pub async fn insert_entries(
    pool: &SqlitePool,
    entries: &[RawLedgerEntry],
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut new = 0;
    for e in entries {
        let result = sqlx::query(
            "INSERT INTO kraken_ledger
               (ledger_id, refid, time_unix, kind, subtype, aclass, asset, amount, fee, balance)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (ledger_id) DO NOTHING",
        )
        .bind(&e.ledger_id)
        .bind(&e.refid)
        .bind(&e.time_unix)
        .bind(&e.kind)
        .bind(&e.subtype)
        .bind(&e.aclass)
        .bind(&e.asset)
        .bind(&e.amount)
        .bind(&e.fee)
        .bind(&e.balance)
        .execute(&mut *tx)
        .await?;
        new += result.rows_affected() as usize;
    }
    tx.commit().await?;
    Ok(new)
}

/// The id of the newest stored entry, used as the exclusive `start` of the next sync.
pub async fn latest_entry_id(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    // time_unix is text; cast for ordering only (ties broken by id).
    sqlx::query_scalar(
        "SELECT ledger_id FROM kraken_ledger ORDER BY CAST(time_unix AS REAL) DESC, ledger_id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

/// All stored entries, oldest first.
pub async fn all_entries(pool: &SqlitePool) -> Result<Vec<RawLedgerEntry>, sqlx::Error> {
    sqlx::query_as(
        "SELECT ledger_id, refid, time_unix, kind, subtype, aclass, asset, amount, fee, balance
         FROM kraken_ledger ORDER BY CAST(time_unix AS REAL), ledger_id",
    )
    .fetch_all(pool)
    .await
}
