//! Sync the Kraken account (read-only) into the local database:
//!
//!     cargo run -p portfolio-sources --example kraken_sync
//!
//! Needs KRAKEN_API_KEY and KRAKEN_PRIVATE_KEY (environment or `.env`) and uses
//! PORTFOLIO_DB_URL (default sqlite://portfolio.db). Prints counts only, never amounts.

use std::time::Duration;

use portfolio_sources::kraken_ledger::sync;
use portfolio_sources::kraken_private::KrakenPrivateClient;

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
    let db_url =
        std::env::var("PORTFOLIO_DB_URL").unwrap_or_else(|_| "sqlite://portfolio.db".to_owned());
    let pool = portfolio_db::open(&db_url).await?;
    let client = KrakenPrivateClient::from_env()?;
    // Ledgers costs 2 rate-limit points; a short pause between pages keeps well under the limit.
    let summary = sync(&pool, &client, Duration::from_secs(3)).await?;
    println!(
        "fetched {} ledger entries ({} new); {} transactions written; {} internal groups ignored",
        summary.fetched, summary.new_entries, summary.transactions_written, summary.ignored_groups
    );
    for u in &summary.unsupported {
        println!("  not supported yet: {}", u.entries.join(", "));
    }
    Ok(())
}
