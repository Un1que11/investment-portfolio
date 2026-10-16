//! Local Axum dashboard.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, NaiveDate, Utc};
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::Sources;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

pub mod base;
pub mod format;
pub mod gold;

/// Price sources (read-only) and the settings the valuation needs.
pub struct Market {
    pub nbp: NbpClient,
    pub kraken: KrakenClient,
    /// Without a key VWCE has no price, and the pages say so.
    pub alphavantage: Option<AlphaVantageClient>,
    /// Share of melt value a dealer pays for coins without a manual quote (config).
    pub gold_buyback_factor: Decimal,
}

/// Shared state: the database, the price sources and a clock, injectable for tests.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub market: Arc<Market>,
    /// Today's date in Warsaw.
    pub today: Arc<dyn Fn() -> NaiveDate + Send + Sync>,
    pub now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl AppState {
    /// State using the machine's clock and local date (Polish time on Misha's Mac).
    pub fn new(pool: SqlitePool, market: Market) -> Self {
        AppState {
            pool,
            market: Arc::new(market),
            today: Arc::new(|| chrono::Local::now().date_naive()),
            now: Arc::new(Utc::now),
        }
    }

    pub fn clock(&self) -> Clock {
        Clock {
            now: (self.now)(),
            today: (self.today)(),
        }
    }

    /// The live sources, valued with the clock read now.
    pub fn sources(&self) -> Sources<'_> {
        Sources {
            pool: &self.pool,
            nbp: &self.market.nbp,
            kraken: &self.market.kraken,
            alphavantage: self.market.alphavantage.as_ref(),
            clock: self.clock(),
            gold_buyback_factor: self.market.gold_buyback_factor,
        }
    }
}

/// Builds the application router. Kept separate from `main` so tests can drive it in-process.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::to("/gold") }))
        .route("/health", get(health))
        .route("/gold", get(gold::show))
        .route("/gold/coins", post(gold::add))
        .route("/gold/coins/{id}/delete", post(gold::delete))
        .merge(base::routes())
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

/// Internal failure. Details go to stderr; the response stays generic.
#[derive(Debug)]
pub enum AppError {
    Db(sqlx::Error),
    Template(askama::Error),
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Db(e)
    }
}

impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> Self {
        AppError::Template(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        eprintln!("request failed: {self:?}");
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
    }
}
