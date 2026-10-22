//! Background sync, scheduled daily by launchd (see README, "Daily sync"): Kraken ledger,
//! prices, the day's per-asset snapshot, and last month's Telegram summary when it is turned on.
//! Prints counts and asset names, never amounts, since its output goes to a log file (only
//! `summary` without `--send`, run by hand, prints the summary itself). Exits with a failure code when any step failed, so a broken
//! run shows up in the log and in `launchctl print`.

use std::process::ExitCode;
use std::time::Duration;

use chrono::{Local, NaiveDate, Utc};
use portfolio_core::config::Config;
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::kraken_ledger;
use portfolio_sources::kraken_private::{API_KEY_ENV, KrakenPrivateClient, PRIVATE_KEY_ENV};
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::{Sources, portfolio_on};
use portfolio_sync::backup::BackupOptions;
use portfolio_sync::summary::{
    SummaryOptions, deliver, month_summary, send_due_summary, summary_text,
};
use portfolio_sync::telegram::TelegramClient;
use portfolio_sync::{Command, Options, USAGE, parse_args, write_snapshot};

#[tokio::main]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    let today = Local::now().date_naive();
    let options = match parse_args(std::env::args().skip(1), today) {
        Ok(Command::Run(o)) => o,
        Ok(Command::Summary(o)) => {
            return match summary(o).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(Command::Backup(o)) => {
            return match backup(o, today).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    println!(
        "portfolio-sync {} (snapshot for {})",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        options.date
    );
    match run(options).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read_config() -> Result<Config, Box<dyn std::error::Error>> {
    let path = std::env::var("PORTFOLIO_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read config {path}: {e}"))?;
    Ok(Config::from_toml(&text)?)
}

fn db_url() -> String {
    std::env::var("PORTFOLIO_DB_URL").unwrap_or_else(|_| "sqlite://portfolio.db".to_owned())
}

async fn open_db() -> Result<sqlx::SqlitePool, Box<dyn std::error::Error>> {
    Ok(portfolio_db::open(&db_url()).await?)
}

/// `portfolio-sync summary`: print the month's summary, or send it with `--send`.
async fn summary(options: SummaryOptions) -> Result<(), Box<dyn std::error::Error>> {
    let pool = open_db().await?;
    if options.send {
        let client = TelegramClient::from_env()?;
        let delivery = deliver(&pool, options.month, &client, Utc::now()).await?;
        println!("telegram: {delivery}");
        return Ok(());
    }
    match month_summary(&pool, options.month).await? {
        Some(s) => {
            eprintln!("dry run: not sent (add --send to send it)\n");
            println!("{}", summary_text(&s));
        }
        None => println!("no snapshot in {}, nothing to show", options.month),
    }
    Ok(())
}

/// `portfolio-sync backup`: a dated copy of the database. Prints only the file name.
async fn backup(
    options: BackupOptions,
    today: NaiveDate,
) -> Result<(), Box<dyn std::error::Error>> {
    // Never creates or migrates the database: a missing one is an error, not an empty copy.
    let pool = portfolio_sync::backup::open_source(&db_url()).await?;
    let path = portfolio_sync::backup::backup(&pool, &options, today).await?;
    println!("backup written: {}", path.display());
    Ok(())
}

/// Ok(false) when a step failed but the run could go on; Err when it could not start.
async fn run(options: Options) -> Result<bool, Box<dyn std::error::Error>> {
    let config = read_config()?;
    let pool = open_db().await?;
    let mut ok = true;

    if !options.kraken {
        println!("kraken: skipped (--skip-kraken)");
    } else if !kraken_configured() {
        println!("kraken: skipped ({API_KEY_ENV} and {PRIVATE_KEY_ENV} not set)");
    } else {
        let result = match KrakenPrivateClient::from_env() {
            Ok(client) => kraken_ledger::sync(&pool, &client, Duration::from_secs(3))
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(s) => {
                println!(
                    "kraken: {} ledger entries fetched ({} new), {} transactions written",
                    s.fetched, s.new_entries, s.transactions_written
                );
                for u in &s.unsupported {
                    println!("  not supported yet: {}", u.entries.join(", "));
                }
            }
            Err(e) => {
                // Still snapshot: the stored ledger is only missing the newest entries.
                eprintln!("kraken: sync failed: {e}");
                ok = false;
            }
        }
    }

    let nbp = NbpClient::new()?;
    let kraken = KrakenClient::new()?;
    let av = AlphaVantageClient::from_env().ok();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: av.as_ref(),
        clock: Clock {
            now: Utc::now(),
            today: Local::now().date_naive(),
        },
        gold_buyback_factor: config.gold_buyback_factor(),
    };
    let report = portfolio_on(options.date, &sources).await?;
    for p in &report.problems {
        println!("  note: {p}");
    }
    match write_snapshot(&pool, &report).await {
        Ok(n) => println!("snapshot {}: {n} assets written", options.date),
        Err(e) => {
            eprintln!("snapshot {}: {e}", options.date);
            ok = false;
        }
    }

    if config.telegram_monthly_summary() {
        let today = Local::now().date_naive();
        let result = match TelegramClient::from_env() {
            Ok(client) => send_due_summary(&pool, today, &client, Utc::now()).await,
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(delivery) => println!("telegram: {delivery}"),
            Err(e) => {
                eprintln!("telegram: {e}");
                ok = false;
            }
        }
    }
    Ok(ok)
}

/// At least one Kraken key variable is set; a half-configured pair is reported as an error by
/// the client rather than skipped.
fn kraken_configured() -> bool {
    [API_KEY_ENV, PRIVATE_KEY_ENV]
        .iter()
        .any(|v| std::env::var(v).is_ok_and(|s| !s.trim().is_empty()))
}
