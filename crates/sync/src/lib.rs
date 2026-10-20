//! The daily job behind the `portfolio-sync` binary: sync the Kraken ledger (when keys are
//! configured), value the portfolio on the day, and store one snapshot row per asset.
//!
//! The pieces that do not need the network live here so they can be tested: argument parsing
//! and turning a [`PortfolioReport`] into stored snapshot rows.

use std::fmt;

use chrono::NaiveDate;
use portfolio_core::snapshot::{SnapshotError, snapshot_rows};
use portfolio_db::snapshots::{SnapshotWriteError, replace_day};
use portfolio_sources::valuation::PortfolioReport;
use sqlx::SqlitePool;

pub const USAGE: &str = "\
usage: portfolio-sync [--date YYYY-MM-DD] [--skip-kraken]

Syncs the Kraken ledger (read-only; skipped when KRAKEN_API_KEY and KRAKEN_PRIVATE_KEY are not
set), values the portfolio on the date (default: today) and stores one snapshot row per asset,
replacing any snapshot already stored for that date.

Environment (or .env in the working directory): PORTFOLIO_DB_URL (default
sqlite://portfolio.db), PORTFOLIO_CONFIG (default config.toml), ALPHAVANTAGE_API_KEY,
KRAKEN_API_KEY, KRAKEN_PRIVATE_KEY.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Day to snapshot.
    pub date: NaiveDate,
    /// Sync the Kraken ledger first.
    pub kraken: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
}

/// Parses the arguments after the program name. A date after `today` is refused: there are no
/// prices for it yet.
pub fn parse_args<I, S>(args: I, today: NaiveDate) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut options = Options {
        date: today,
        kraken: true,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "-h" | "--help" => return Ok(Command::Help),
            "--skip-kraken" => options.kraken = false,
            "--date" => {
                let value = args.next().ok_or("--date needs a value (YYYY-MM-DD)")?;
                let value = value.as_ref();
                options.date = value
                    .parse()
                    .map_err(|_| format!("invalid date {value:?}, expected YYYY-MM-DD"))?;
                if options.date > today {
                    return Err(format!("{value} is in the future"));
                }
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Command::Run(options))
}

#[derive(Debug)]
pub enum SnapshotFailure {
    /// The report cannot become a complete day (e.g. an asset has no price).
    Incomplete(SnapshotError),
    /// These assets are valued at a stored close of an earlier day because their source failed
    /// (see [`portfolio_sources::prices::close_or_last_known`]); the date with its own closes
    /// can be written later with `--date`.
    LastKnown {
        date: NaiveDate,
        assets: Vec<String>,
    },
    Write(SnapshotWriteError),
}

impl fmt::Display for SnapshotFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotFailure::Incomplete(e) => write!(f, "snapshot not written: {e}"),
            SnapshotFailure::LastKnown { date, assets } => write!(
                f,
                "snapshot not written: {} {} only a stored close of an earlier day (its source \
                 failed); re-run later with --date {date}",
                assets.join(", "),
                if assets.len() == 1 { "has" } else { "have" }
            ),
            SnapshotFailure::Write(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SnapshotFailure {}

/// Stores the report's day as snapshot rows, replacing what was stored for that date, and
/// returns the number of rows. When the day is incomplete, or an asset is valued at a stored
/// close standing in for a failed source, nothing is written and an earlier snapshot of the
/// same date is kept: a stand-in close stored as the day's value would never be corrected.
pub async fn write_snapshot(
    pool: &SqlitePool,
    report: &PortfolioReport,
) -> Result<usize, SnapshotFailure> {
    if !report.last_known.is_empty() {
        return Err(SnapshotFailure::LastKnown {
            date: report.date,
            assets: report.last_known.keys().cloned().collect(),
        });
    }
    let rows = snapshot_rows(&report.holdings, &report.valuation, &report.pnl)
        .map_err(SnapshotFailure::Incomplete)?;
    replace_day(pool, report.date, &rows)
        .await
        .map_err(SnapshotFailure::Write)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn defaults_to_today_with_kraken() {
        assert_eq!(
            parse_args(Vec::<String>::new(), d("2026-10-19")),
            Ok(Command::Run(Options {
                date: d("2026-10-19"),
                kraken: true
            }))
        );
    }

    #[test]
    fn date_and_skip_kraken() {
        assert_eq!(
            parse_args(["--skip-kraken", "--date", "2026-10-17"], d("2026-10-19")),
            Ok(Command::Run(Options {
                date: d("2026-10-17"),
                kraken: false
            }))
        );
    }

    #[test]
    fn bad_arguments_are_refused() {
        let today = d("2026-10-19");
        assert!(parse_args(["--date"], today).is_err());
        assert!(parse_args(["--date", "19.10.2026"], today).is_err());
        assert!(parse_args(["--date", "2026-10-20"], today).is_err());
        assert!(parse_args(["--force"], today).is_err());
    }

    #[test]
    fn help() {
        assert_eq!(parse_args(["--help"], d("2026-10-19")), Ok(Command::Help));
    }
}
