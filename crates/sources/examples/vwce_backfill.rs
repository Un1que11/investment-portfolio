//! Live check: backfill VWCE week-end closes, then value an old day, using a throwaway database.
//!
//!     cargo run -p portfolio-sources --example vwce_backfill -- 2024-03-13
//!
//! Needs ALPHAVANTAGE_API_KEY in the environment or in `.env` at the workspace root. Makes two
//! Alpha Vantage requests (daily and weekly) and one NBP request.

use chrono::{Local, NaiveDate, Utc};
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::{Clock, VWCE, backfill_weekly, value_in_pln};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Missing .env is fine: the key may already be in the environment.
    let _ = dotenvy::dotenv();
    let clock = Clock {
        now: Utc::now(),
        today: Local::now().date_naive(),
    };
    let day = match std::env::args().nth(1) {
        Some(s) => s.parse::<NaiveDate>()?,
        None => "2024-03-13".parse()?,
    };

    let pool = portfolio_db::open_in_memory().await?;
    let av = AlphaVantageClient::from_env()?;
    let nbp = NbpClient::new()?;
    let done = backfill_weekly(&pool, &av, &VWCE, clock).await?;
    println!(
        "backfill: {} completed weeks, {} week-end closes stored",
        done.weeks, done.inserted
    );

    let v = value_in_pln(&pool, &av, &nbp, &VWCE, day, clock).await?;
    println!(
        "VWCE on {day}: close {} {} ({}) = {} PLN",
        v.quote.close,
        v.quote.currency,
        v.quote.date,
        v.close_pln.round_dp(2)
    );
    Ok(())
}
