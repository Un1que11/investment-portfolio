use std::net::SocketAddr;
use std::sync::Arc;

use axum::Extension;
use portfolio_core::config::Config;
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::{AppState, Market, app};

/// Loopback only: the dashboard is meant to be reachable from this machine alone.
const DEFAULT_ADDR: &str = "127.0.0.1:3000";
/// Relative to the working directory; `*.db` files are git-ignored.
const DEFAULT_DB_URL: &str = "sqlite://portfolio.db";
const DEFAULT_CONFIG: &str = "config.toml";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ALPHAVANTAGE_API_KEY may come from `.env`; without it VWCE shows as unpriced.
    let _ = dotenvy::dotenv();
    let addr: SocketAddr = std::env::var("PORTFOLIO_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
        .parse()?;
    let db_url = std::env::var("PORTFOLIO_DB_URL").unwrap_or_else(|_| DEFAULT_DB_URL.to_owned());
    let config_path =
        std::env::var("PORTFOLIO_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.to_owned());
    let text = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("cannot read config {config_path}: {e}"))?;
    let config = Config::from_toml(&text)?;

    let market = Market {
        nbp: NbpClient::new()?,
        kraken: KrakenClient::new()?,
        alphavantage: AlphaVantageClient::from_env().ok(),
        gold_buyback_factor: config.gold_buyback_factor(),
    };
    if market.alphavantage.is_none() {
        eprintln!("no ALPHAVANTAGE_API_KEY: VWCE will have no price");
    }
    let pool = portfolio_db::open(&db_url).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("portfolio-web listening on http://{addr} (database {db_url})");
    let router = app(AppState::new(pool, market)).layer(Extension(Arc::new(config)));
    axum::serve(listener, router).await?;
    Ok(())
}
