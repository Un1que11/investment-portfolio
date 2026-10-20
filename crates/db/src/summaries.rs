//! Months whose summary was sent to Telegram (`telegram_summaries`, BACKLOG P26).

use chrono::{DateTime, SecondsFormat, Utc};
use portfolio_core::summary::Month;
use sqlx::SqlitePool;

/// Whether `month`'s summary was sent already.
pub async fn is_sent(pool: &SqlitePool, month: Month) -> Result<bool, sqlx::Error> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM telegram_summaries WHERE month = ?")
        .bind(month.to_string())
        .fetch_optional(pool)
        .await?;
    Ok(found.is_some())
}

/// Records that `month`'s summary was sent at `at`. Recording it again keeps the first time.
pub async fn mark_sent(
    pool: &SqlitePool,
    month: Month,
    at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO telegram_summaries (month, sent_at) VALUES (?, ?)
         ON CONFLICT (month) DO NOTHING",
    )
    .bind(month.to_string())
    .bind(at.to_rfc3339_opts(SecondsFormat::Secs, true))
    .execute(pool)
    .await?;
    Ok(())
}
