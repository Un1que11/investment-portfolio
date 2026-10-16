//! Print holdings, net contributions and value in PLN for a date (default: today):
//!
//!     cargo run -p portfolio-sources --example portfolio -- [YYYY-MM-DD] [gold buyback factor]
//!
//! Uses PORTFOLIO_DB_URL (default sqlite://portfolio.db) and ALPHAVANTAGE_API_KEY for VWCE
//! (environment or `.env`). The gold buyback factor defaults to 1 until config.toml (P3).

use chrono::{Local, NaiveDate, Utc};
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::{Sources, portfolio_on};
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
    let _ = dotenvy::dotenv();
    let clock = Clock {
        now: Utc::now(),
        today: Local::now().date_naive(),
    };
    let mut args = std::env::args().skip(1);
    let date = match args.next() {
        Some(s) => s.parse::<NaiveDate>()?,
        None => clock.today,
    };
    let factor = match args.next() {
        Some(s) => s.parse::<Decimal>()?,
        None => Decimal::ONE,
    };
    let db_url =
        std::env::var("PORTFOLIO_DB_URL").unwrap_or_else(|_| "sqlite://portfolio.db".to_owned());
    let pool = portfolio_db::open(&db_url).await?;
    let nbp = NbpClient::new()?;
    let kraken = KrakenClient::new()?;
    let av = AlphaVantageClient::from_env().ok();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: av.as_ref(),
        clock,
        gold_buyback_factor: factor,
    };
    let r = portfolio_on(date, &sources).await?;

    let pln = |v: Decimal| format!("{:.2}", v);
    println!("Portfolio on {date}");
    for (asset, qty) in &r.holdings.assets {
        let value = r
            .valuation
            .by_asset
            .get(asset)
            .map_or("no price".into(), |v| format!("{} PLN", pln(*v)));
        println!("  {asset:<11} {qty:>14}  {value:>16}");
    }
    for ((account, currency), amount) in &r.holdings.cash {
        let value = r
            .valuation
            .cash
            .get(&(account.clone(), currency.clone()))
            .map_or("no rate".into(), |v| format!("{} PLN", pln(*v)));
        println!("  cash {currency} ({account}) {amount}  {value}");
    }
    println!(
        "Value: {} PLN{}",
        pln(r.valuation.total),
        if r.valuation.complete() {
            ""
        } else {
            " (incomplete)"
        }
    );
    println!("Net contributions: {} PLN", pln(r.contributions.total));
    for (account, c) in &r.contributions.by_account {
        println!("  {account}: {} PLN", pln(*c));
    }
    let pct = |ratio: Option<Decimal>| {
        ratio.map_or("-".into(), |x| format!("{:.2}%", x * Decimal::ONE_HUNDRED))
    };
    match r.pnl.pnl_if_complete() {
        Some(total) => println!("P&L: {} PLN ({})", pln(total), pct(r.pnl.pnl_ratio())),
        None => println!(
            "P&L: not available, prices missing for {}",
            r.valuation.missing.join(", ")
        ),
    }
    for (asset, a) in &r.pnl.by_asset {
        let p = a
            .pnl()
            .map_or("no price".into(), |v| format!("{} PLN", pln(v)));
        println!(
            "  {asset:<11} cost {:>10}  P&L {p:>14} ({})",
            pln(a.cost),
            pct(a.pnl_ratio())
        );
    }
    if let Some(other) = r.pnl.other() {
        println!("  cash, FX on cash, account fees: {} PLN", pln(other));
    }
    for p in &r.problems {
        println!("  note: {p}");
    }
    Ok(())
}
