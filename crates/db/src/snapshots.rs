//! Daily per-asset snapshots written by `portfolio-sync` (`snapshots`).

use chrono::NaiveDate;
use portfolio_core::snapshot::SnapshotRow;
use sqlx::SqlitePool;

use crate::DbDecimal;

const DATE_FORMAT: &str = "%Y-%m-%d";

#[derive(Debug)]
pub enum SnapshotWriteError {
    /// A row names an asset symbol that is not in `assets`.
    UnknownAsset(String),
    Db(sqlx::Error),
}

impl std::fmt::Display for SnapshotWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotWriteError::UnknownAsset(s) => write!(f, "unknown asset {s:?}"),
            SnapshotWriteError::Db(e) => write!(f, "cannot write snapshot: {e}"),
        }
    }
}

impl std::error::Error for SnapshotWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SnapshotWriteError::UnknownAsset(_) => None,
            SnapshotWriteError::Db(e) => Some(e),
        }
    }
}

impl From<sqlx::Error> for SnapshotWriteError {
    fn from(e: sqlx::Error) -> Self {
        SnapshotWriteError::Db(e)
    }
}

/// Replaces everything stored for `date` with `rows`, in one transaction, and returns how many
/// rows were written. Running the sync twice on a day keeps only the later values, and an asset
/// sold out since the earlier run loses its row. On error nothing for the day changes.
pub async fn replace_day(
    pool: &SqlitePool,
    date: NaiveDate,
    rows: &[SnapshotRow],
) -> Result<usize, SnapshotWriteError> {
    let day = date.format(DATE_FORMAT).to_string();
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM snapshots WHERE snapshot_date = ?")
        .bind(&day)
        .execute(&mut *tx)
        .await?;
    for row in rows {
        let asset_id: Option<i64> = sqlx::query_scalar("SELECT id FROM assets WHERE symbol = ?")
            .bind(&row.asset)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(asset_id) = asset_id else {
            // Dropping `tx` rolls the delete back.
            return Err(SnapshotWriteError::UnknownAsset(row.asset.clone()));
        };
        sqlx::query(
            "INSERT INTO snapshots (snapshot_date, asset_id, quantity, value_pln, invested_pln)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&day)
        .bind(asset_id)
        .bind(DbDecimal(row.quantity))
        .bind(DbDecimal(row.value_pln))
        .bind(DbDecimal(row.invested_pln))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(rows.len())
}

/// The rows stored for `date`, in symbol order (empty when the day has no snapshot).
pub async fn load_day(pool: &SqlitePool, date: NaiveDate) -> Result<Vec<SnapshotRow>, sqlx::Error> {
    let rows: Vec<(String, DbDecimal, DbDecimal, DbDecimal)> = sqlx::query_as(
        "SELECT a.symbol, s.quantity, s.value_pln, s.invested_pln
         FROM snapshots s JOIN assets a ON a.id = s.asset_id
         WHERE s.snapshot_date = ?
         ORDER BY a.symbol",
    )
    .bind(date.format(DATE_FORMAT).to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(asset, q, v, i)| SnapshotRow {
            asset,
            quantity: q.0,
            value_pln: v.0,
            invested_pln: i.0,
        })
        .collect())
}

/// The most recent day with a snapshot, if any.
pub async fn latest_day(pool: &SqlitePool) -> Result<Option<NaiveDate>, sqlx::Error> {
    let day: Option<String> = sqlx::query_scalar("SELECT max(snapshot_date) FROM snapshots")
        .fetch_one(pool)
        .await?;
    day.map(|d| {
        NaiveDate::parse_from_str(&d, DATE_FORMAT).map_err(|e| sqlx::Error::Decode(Box::new(e)))
    })
    .transpose()
}
