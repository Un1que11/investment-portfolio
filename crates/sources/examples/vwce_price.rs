//! Live check: VWCE close on a day, in EUR and PLN, using a throwaway database.
//!
//!     cargo run -p portfolio-sources --example vwce_price -- 2026-10-09
//!
//! Needs ALPHAVANTAGE_API_KEY in the environment or in `.env` at the workspace root.
//! "Today" is the machine's local date, which is fine on a Mac set to Polish time.

use chrono::{Local, NaiveDate, Utc};
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::{Clock, VWCE, value_in_pln};

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
        None => clock.today,
    };

    let pool = portfolio_db::open_in_memory().await?;
    let av = AlphaVantageClient::from_env()?;
    let nbp = NbpClient::new()?;
    let v = value_in_pln(&pool, &av, &nbp, &VWCE, day, clock).await?;

    println!(
        "VWCE on {day}: close {} {} ({}) = {} PLN",
        v.quote.close,
        v.quote.currency,
        v.quote.date,
        v.close_pln.round_dp(2)
    );
    if let Some(fx) = v.fx {
        println!(
            "  rate {} PLN/{} (NBP table {})",
            fx.mid, fx.currency, fx.table_no
        );
    }
    Ok(())
}
