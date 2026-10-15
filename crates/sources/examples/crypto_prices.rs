//! Live check against Kraken's public API and NBP, using a throwaway database:
//!
//!     cargo run -p portfolio-sources --example crypto_prices -- 2026-10-11
//!
//! Prints live BTC/ETH values and the final daily close for the given day (default: yesterday).
//! "Today" is the machine's local date, which is fine on a Mac set to Polish time.

use chrono::{Days, Local, NaiveDate, Utc};
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::{BTC, Clock, ETH, kraken_spot_in_pln, value_in_pln};

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
    let day = match std::env::args().nth(1) {
        Some(s) => s.parse::<NaiveDate>()?,
        None => clock.today - Days::new(1),
    };

    let pool = portfolio_db::open_in_memory().await?;
    let kraken = KrakenClient::new()?;
    let nbp = NbpClient::new()?;

    for (instrument, spot) in [BTC, ETH]
        .iter()
        .zip(kraken_spot_in_pln(&pool, &kraken, &nbp, &[BTC, ETH], clock).await?)
    {
        println!(
            "{} now: {} {} = {} PLN (NBP {} {})",
            instrument.symbol,
            spot.price,
            spot.currency,
            spot.price_pln.round_dp(2),
            spot.fx.mid,
            spot.fx.table_no
        );
    }
    for instrument in [BTC, ETH] {
        let v = value_in_pln(&pool, &kraken, &nbp, &instrument, day, clock).await?;
        println!(
            "{} close {} (UTC day): {} {} = {} PLN",
            instrument.symbol,
            v.quote.date,
            v.quote.close,
            v.quote.currency,
            v.close_pln.round_dp(2)
        );
    }
    Ok(())
}
