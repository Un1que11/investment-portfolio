//! The monthly summary from stored snapshots, and sending it once (with a recorder, never
//! Telegram).

use std::future::Future;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_core::AssetClass;
use portfolio_core::snapshot::SnapshotRow;
use portfolio_core::summary::Month;
use portfolio_db::prices::ensure_asset;
use portfolio_db::snapshots::replace_day;
use portfolio_db::summaries::is_sent;
use portfolio_sync::summary::{Delivery, deliver, month_summary, send_due_summary, summary_text};
use portfolio_sync::telegram::{MessageSender, TelegramError};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

fn d(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn m(s: &str) -> Month {
    s.parse().unwrap()
}

fn now() -> DateTime<Utc> {
    "2026-10-01T20:30:00Z".parse().unwrap()
}

fn row(asset: &str, quantity: Decimal, value: Decimal, invested: Decimal) -> SnapshotRow {
    SnapshotRow {
        asset: asset.into(),
        quantity,
        value_pln: value,
        invested_pln: invested,
    }
}

/// 31 Aug: VWCE 10 000.00 / 9 000.00 invested, BTC 3 000.00 / 2 500.00.
/// 12 Sep: a snapshot in between, which the summary skips.
/// 30 Sep: VWCE 11 500.00 / 10 000.00 (1 000 bought), BTC 2 800.00 / 2 500.00.
/// 1 Oct: the next month's first day, outside September.
async fn db() -> SqlitePool {
    let pool = portfolio_db::open_in_memory().await.unwrap();
    ensure_asset(&pool, "VWCE", "VWCE", AssetClass::Vwce, "EUR")
        .await
        .unwrap();
    ensure_asset(&pool, "BTC", "Bitcoin", AssetClass::Crypto, "EUR")
        .await
        .unwrap();
    let days = [
        ("2026-08-31", dec!(10000.00), dec!(9000.00), dec!(3000.00)),
        ("2026-09-12", dec!(9000.00), dec!(9000.00), dec!(1.00)),
        ("2026-09-30", dec!(11500.00), dec!(10000.00), dec!(2800.00)),
        ("2026-10-01", dec!(99999.00), dec!(10000.00), dec!(2800.00)),
    ];
    for (day, vwce, vwce_invested, btc) in days {
        replace_day(
            &pool,
            d(day),
            &[
                row("BTC", dec!(0.01), btc, dec!(2500.00)),
                row("VWCE", dec!(20), vwce, vwce_invested),
            ],
        )
        .await
        .unwrap();
    }
    pool
}

#[derive(Default)]
struct Recorder {
    sent: Mutex<Vec<String>>,
    fail: bool,
}

impl MessageSender for Recorder {
    fn send(&self, text: &str) -> impl Future<Output = Result<(), TelegramError>> + Send {
        let result = if self.fail {
            Err(TelegramError::Api {
                status: 502,
                description: "Bad Gateway".into(),
            })
        } else {
            self.sent.lock().unwrap().push(text.to_owned());
            Ok(())
        };
        std::future::ready(result)
    }
}

#[tokio::test]
async fn the_summary_text_for_a_whole_month() {
    // Value 11 500 + 2 800 = 14 300.00, against 13 000.00 on 31 Aug: +1 300.00.
    // New money 12 500 - 11 500 = +1 000.00; profit 1 300 - 1 000 = +300.00.
    // To date 14 300 - 12 500 = +1 800.00, 1 800 / 12 500 = +14.4%.
    // VWCE (1 500 - 1 000) = +500.00; Bitcoin (300 - 500) = -200.00.
    let pool = db().await;
    let s = month_summary(&pool, m("2026-09")).await.unwrap().unwrap();
    let text = summary_text(&s);
    assert!(text.contains("14\u{a0}300,00\u{a0}zł"), "{text}");
    assert_eq!(
        text.replace('\u{a0}', " "),
        "Portfel · September 2026

Value on 30.09.2026: 14 300,00 zł
Change since 31.08.2026: +1 300,00 zł
  new money: +1 000,00 zł
  profit: +300,00 zł
Profit to date: +1 800,00 zł (+14,4%)

Profit by asset:
VWCE: +500,00 zł
Bitcoin: \u{2212}200,00 zł

Cash in the accounts is not included."
    );
}

#[tokio::test]
async fn a_month_whose_snapshots_start_inside_it() {
    // August has one snapshot (31 Aug) and none before: nothing to compare with.
    let pool = db().await;
    let s = month_summary(&pool, m("2026-08")).await.unwrap().unwrap();
    assert!(
        summary_text(&s).contains("No earlier snapshot to compare with yet."),
        "{}",
        summary_text(&s)
    );
    // November has none at all.
    assert_eq!(month_summary(&pool, m("2026-11")).await.unwrap(), None);
}

#[tokio::test]
async fn last_month_is_sent_once() {
    let pool = db().await;
    let telegram = Recorder::default();
    // Any day in October sends September: the 1st, or later if the Mac was off.
    assert_eq!(
        send_due_summary(&pool, d("2026-10-03"), &telegram, now()).await,
        Ok(Delivery::Sent(m("2026-09")))
    );
    assert_eq!(
        send_due_summary(&pool, d("2026-10-04"), &telegram, now()).await,
        Ok(Delivery::AlreadySent(m("2026-09")))
    );
    let sent = telegram.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].starts_with("Portfel · September 2026\n"));
}

#[tokio::test]
async fn a_failed_send_is_not_recorded_and_is_tried_again() {
    let pool = db().await;
    let down = Recorder {
        fail: true,
        ..Recorder::default()
    };
    assert_eq!(
        deliver(&pool, m("2026-09"), &down, now()).await,
        Err("summary for 2026-09 not sent: Telegram answered HTTP 502: Bad Gateway".into())
    );
    assert!(!is_sent(&pool, m("2026-09")).await.unwrap());

    let up = Recorder::default();
    assert_eq!(
        deliver(&pool, m("2026-09"), &up, now()).await,
        Ok(Delivery::Sent(m("2026-09")))
    );
    assert!(is_sent(&pool, m("2026-09")).await.unwrap());
}

#[tokio::test]
async fn a_month_without_snapshots_sends_nothing_and_stays_due() {
    let pool = db().await;
    let telegram = Recorder::default();
    // On 5 Dec, November is due but has no snapshot.
    assert_eq!(
        send_due_summary(&pool, d("2026-12-05"), &telegram, now()).await,
        Ok(Delivery::NoSnapshot(m("2026-11")))
    );
    assert!(telegram.sent.lock().unwrap().is_empty());
    assert!(!is_sent(&pool, m("2026-11")).await.unwrap());
}
