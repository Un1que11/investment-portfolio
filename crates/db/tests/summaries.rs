use chrono::{DateTime, Utc};
use portfolio_core::summary::Month;
use portfolio_db::open_in_memory;
use portfolio_db::summaries::{is_sent, mark_sent};

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

#[tokio::test]
async fn a_month_is_recorded_once() {
    let pool = open_in_memory().await.unwrap();
    let september: Month = "2026-09".parse().unwrap();
    assert!(!is_sent(&pool, september).await.unwrap());

    mark_sent(&pool, september, at("2026-10-01T20:30:05Z"))
        .await
        .unwrap();
    // A second record keeps the first time and is not an error.
    mark_sent(&pool, september, at("2026-10-02T20:30:05Z"))
        .await
        .unwrap();
    assert!(is_sent(&pool, september).await.unwrap());
    assert!(!is_sent(&pool, september.next()).await.unwrap());

    let stored: Vec<(String, String)> =
        sqlx::query_as("SELECT month, sent_at FROM telegram_summaries")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        vec![("2026-09".to_owned(), "2026-10-01T20:30:05Z".to_owned())]
    );
}
