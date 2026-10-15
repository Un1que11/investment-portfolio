use std::net::SocketAddr;

/// Loopback only: the dashboard is meant to be reachable from this machine alone.
const DEFAULT_ADDR: &str = "127.0.0.1:3000";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::var("PORTFOLIO_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
        .parse()?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("portfolio-web listening on http://{addr}");
    axum::serve(listener, portfolio_web::app()).await?;
    Ok(())
}
