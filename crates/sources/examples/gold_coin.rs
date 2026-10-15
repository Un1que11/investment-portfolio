//! Live check: value of a 1/10 oz gold coin from the NBP gold price, throwaway database.
//!
//!     cargo run -p portfolio-sources --example gold_coin -- 2026-10-11 0.95
//!
//! Arguments: day (default today) and buyback factor (default 1). "Today" is the machine's local
//! date, which is fine on a Mac set to Polish time.

use chrono::{Local, NaiveDate, Utc};
use portfolio_core::gold::TENTH_OUNCE_GRAMS;
use portfolio_sources::gold::coin_value_on_day;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use rust_decimal::Decimal;

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
    let clock = Clock {
        now: Utc::now(),
        today: Local::now().date_naive(),
    };
    let mut args = std::env::args().skip(1);
    let day = match args.next() {
        Some(s) => s.parse::<NaiveDate>()?,
        None => clock.today,
    };
    let factor = match args.next() {
        Some(s) => s.parse::<Decimal>()?,
        None => Decimal::ONE,
    };

    let pool = portfolio_db::open_in_memory().await?;
    let nbp = NbpClient::new()?;
    let v = coin_value_on_day(&pool, &nbp, day, clock, TENTH_OUNCE_GRAMS, factor, None).await?;
    println!(
        "1/10 oz coin on {day}: NBP {} PLN/g ({}) -> melt {} PLN, at factor {factor}: {} PLN",
        v.price_per_gram.close,
        v.price_per_gram.date,
        v.valuation.melt_value_pln.round_dp(2),
        v.valuation.value_pln.round_dp(2)
    );
    Ok(())
}
