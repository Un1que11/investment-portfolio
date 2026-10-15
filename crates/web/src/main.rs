use std::net::SocketAddr;

use portfolio_web::{AppState, app};

/// Loopback only: the dashboard is meant to be reachable from this machine alone.
const DEFAULT_ADDR: &str = "127.0.0.1:3000";
/// Relative to the working directory; `*.db` files are git-ignored.
const DEFAULT_DB_URL: &str = "sqlite://portfolio.db";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::var("PORTFOLIO_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
        .parse()?;
    let db_url = std::env::var("PORTFOLIO_DB_URL").unwrap_or_else(|_| DEFAULT_DB_URL.to_owned());

    let pool = portfolio_db::open(&db_url).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("portfolio-web listening on http://{addr} (database {db_url})");
    axum::serve(listener, app(AppState::new(pool))).await?;
    Ok(())
}
