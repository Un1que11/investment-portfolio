//! Live check against the real NBP API, using a throwaway database:
//!
//!     cargo run -p portfolio-sources --example fx_rate -- EUR 2026-10-11
//!
//! "Today" is the machine's local date, which is fine on a Mac set to Polish time.

use chrono::{Local, NaiveDate};
use portfolio_sources::fx::rate_on_day;
use portfolio_sources::nbp::NbpClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let currency = args.next().unwrap_or_else(|| "EUR".to_owned());
    let today = Local::now().date_naive();
    let day = match args.next() {
        Some(s) => s.parse::<NaiveDate>()?,
        None => today,
    };

    let pool = portfolio_db::open_in_memory().await?;
    let nbp = NbpClient::new()?;
    let rate = rate_on_day(&pool, &nbp, &currency, day, today).await?;
    println!(
        "{currency} on {day}: {} PLN (NBP table {}, published {})",
        rate.mid, rate.table_no, rate.effective_date
    );
    Ok(())
}
