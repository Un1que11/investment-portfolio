//! Import an XTB account-history export into the local database:
//!
//!     cargo run -p portfolio-xtb --example import -- ~/Downloads/IKZE_....xlsx
//!
//! Uses PORTFOLIO_DB_URL (default sqlite://portfolio.db, git-ignored). Safe to re-run: operations
//! already imported are skipped.

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
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: import <export.xlsx>")?;
    let db_url =
        std::env::var("PORTFOLIO_DB_URL").unwrap_or_else(|_| "sqlite://portfolio.db".to_owned());
    let export = portfolio_xtb::parse_file(&path)?;
    let pool = portfolio_db::open(&db_url).await?;
    let summary = portfolio_xtb::import::import(&pool, &export).await?;
    println!(
        "{} operations in the export: {} imported, {} already present ({db_url})",
        export.operations.len(),
        summary.inserted,
        summary.already_present
    );
    Ok(())
}
