//! Local Axum dashboard.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::NaiveDate;
use sqlx::SqlitePool;

pub mod gold;

/// Shared state: the database and a clock (today's date), injectable for tests.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub today: Arc<dyn Fn() -> NaiveDate + Send + Sync>,
}

impl AppState {
    /// State using the machine's local date (Polish time on Misha's Mac).
    pub fn new(pool: SqlitePool) -> Self {
        AppState {
            pool,
            today: Arc::new(|| chrono::Local::now().date_naive()),
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
