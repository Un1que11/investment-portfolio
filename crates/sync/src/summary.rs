//! The monthly summary in Telegram (BACKLOG P26): built from the stored snapshots, written as
//! plain text in the pages' number format, and sent once per month.
//!
//! - `portfolio-sync summary [--month YYYY-MM]` prints a month's summary without sending it
//!   (a dry run; the default month is the last one).
//! - `portfolio-sync summary --send` sends it, unless it was sent already.
//! - The daily run sends last month's summary when `[telegram] monthly_summary = true` and it
//!   has not been sent yet ([`send_due_summary`]), so a Mac that was off on the 1st catches up.

use chrono::{DateTime, NaiveDate, Utc};
use portfolio_core::summary::{Month, MonthSummary, SnapshotDay, monthly_summary};
use portfolio_db::snapshots::{first_and_last_day, latest_day_before, load_day};
use portfolio_db::summaries::{is_sent, mark_sent};
use portfolio_web::assets::display_name;
use portfolio_web::format::{date, pln, signed_percent, signed_pln};
use sqlx::SqlitePool;

use crate::telegram::MessageSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryOptions {
    pub month: Month,
    /// Send it to Telegram; otherwise only print it.
    pub send: bool,
}

/// Parses the arguments after `summary`. `--send` is refused for a month that has not ended.
pub fn parse_args<I, S>(args: I, today: NaiveDate) -> Result<SummaryOptions, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut options = SummaryOptions {
        month: Month::of(today).previous(),
        send: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--send" => options.send = true,
            "--month" => {
                let value = args.next().ok_or("--month needs a value (YYYY-MM)")?;
                options.month = value.as_ref().parse()?;
                if options.month > Month::of(today) {
                    return Err(format!("{} is in the future", options.month));
                }
            }
            other => return Err(format!("unknown summary argument {other:?}")),
        }
    }
    // A month is sent once, so sending one that has not ended would keep its whole summary
    // from ever going out.
    if options.send && options.month >= Month::of(today) {
        return Err(format!(
            "{} has not ended yet: its summary can be printed but not sent",
            options.month
        ));
    }
    Ok(options)
}

/// `month`'s summary from the stored snapshots: its last snapshot day against the last one
/// before the month (or the month's first one). None when the month has no snapshot.
pub async fn month_summary(
    pool: &SqlitePool,
    month: Month,
) -> Result<Option<MonthSummary>, sqlx::Error> {
    let Some((first, last)) = first_and_last_day(pool, month.first_day(), month.last_day()).await?
    else {
        return Ok(None);
    };
    let start = match latest_day_before(pool, month.first_day()).await? {
        Some(before) => before,
        None => first,
    };
    let end = SnapshotDay {
        date: last,
        rows: load_day(pool, last).await?,
    };
    let start = SnapshotDay {
        date: start,
        rows: load_day(pool, start).await?,
    };
    Ok(Some(monthly_summary(month, Some(&start), &end)))
}

/// The message: value, the month's change split into new money and profit, profit to date,
/// and each asset's profit in the month.
pub fn summary_text(s: &MonthSummary) -> String {
    let mut lines = vec![
        format!("Portfel · {}", s.month.name()),
        String::new(),
        format!("Value on {}: {}", date(s.end), pln(s.value)),
    ];
    match &s.change {
        Some(c) => {
            let since = if c.whole_month {
                date(c.since)
            } else {
                format!("{} (first snapshot)", date(c.since))
            };
            lines.push(format!(
                "Change since {since}: {}",
                signed_pln(c.value_change)
            ));
            lines.push(format!("  new money: {}", signed_pln(c.new_money)));
            lines.push(format!("  profit: {}", signed_pln(c.profit)));
        }
        None => lines.push("No earlier snapshot to compare with yet.".to_owned()),
    }
    let to_date = match s.profit_to_date_fraction() {
        Some(f) => format!("{} ({})", signed_pln(s.profit_to_date()), signed_percent(f)),
        None => signed_pln(s.profit_to_date()),
    };
    lines.push(format!("Profit to date: {to_date}"));
    if let Some(c) = s.change.as_ref().filter(|c| !c.by_asset.is_empty()) {
        lines.push(String::new());
        lines.push("Profit by asset:".to_owned());
        for (asset, profit) in &c.by_asset {
            lines.push(format!("{}: {}", display_name(asset), signed_pln(*profit)));
        }
    }
    lines.push(String::new());
    lines.push("Cash in the accounts is not included.".to_owned());
    lines.join("\n")
}

/// What [`send_due_summary`] did, for the log (no amounts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Sent(Month),
    AlreadySent(Month),
    /// The month has no snapshot, so there is nothing to send (yet).
    NoSnapshot(Month),
}

impl std::fmt::Display for Delivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Delivery::Sent(m) => write!(f, "summary for {m} sent"),
            Delivery::AlreadySent(m) => write!(f, "summary for {m} was sent already"),
            Delivery::NoSnapshot(m) => write!(f, "no snapshot in {m}, nothing to send"),
        }
    }
}

/// Sends `month`'s summary unless it was sent already, and records it once Telegram confirms.
/// A failed send records nothing, so the next run tries again.
pub async fn deliver<S: MessageSender>(
    pool: &SqlitePool,
    month: Month,
    sender: &S,
    now: DateTime<Utc>,
) -> Result<Delivery, String> {
    if is_sent(pool, month).await.map_err(|e| e.to_string())? {
        return Ok(Delivery::AlreadySent(month));
    }
    let Some(summary) = month_summary(pool, month)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(Delivery::NoSnapshot(month));
    };
    sender
        .send(&summary_text(&summary))
        .await
        .map_err(|e| format!("summary for {month} not sent: {e}"))?;
    mark_sent(pool, month, now).await.map_err(|e| {
        format!("summary for {month} sent but not recorded (may be sent again): {e}")
    })?;
    Ok(Delivery::Sent(month))
}

/// The daily run's part: last month's summary, if it is still due.
pub async fn send_due_summary<S: MessageSender>(
    pool: &SqlitePool,
    today: NaiveDate,
    sender: &S,
    now: DateTime<Utc>,
) -> Result<Delivery, String> {
    deliver(pool, Month::of(today).previous(), sender, now).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn m(s: &str) -> Month {
        s.parse().unwrap()
    }

    #[test]
    fn summary_arguments() {
        let today = d("2026-10-20");
        assert_eq!(
            parse_args(Vec::<String>::new(), today),
            Ok(SummaryOptions {
                month: m("2026-09"),
                send: false
            })
        );
        assert_eq!(
            parse_args(["--month", "2026-08", "--send"], today),
            Ok(SummaryOptions {
                month: m("2026-08"),
                send: true
            })
        );
        // The current month can be printed so far, but not sent: sending would record it as
        // sent and the whole month's summary would never go out.
        assert_eq!(
            parse_args(["--month", "2026-10"], today),
            Ok(SummaryOptions {
                month: m("2026-10"),
                send: false
            })
        );
        assert!(parse_args(["--month", "2026-10", "--send"], today).is_err());
        assert!(parse_args(["--send", "--month", "2026-10"], today).is_err());
        // On a month's last day it has not ended yet either.
        assert!(parse_args(["--month", "2026-10", "--send"], d("2026-10-31")).is_err());
        assert!(parse_args(["--send"], d("2026-11-01")).is_ok());
        assert!(parse_args(["--month"], today).is_err());
        assert!(parse_args(["--month", "2026-11"], today).is_err());
        assert!(parse_args(["--month", "09.2026"], today).is_err());
        assert!(parse_args(["--date", "2026-10-01"], today).is_err());
    }
}
