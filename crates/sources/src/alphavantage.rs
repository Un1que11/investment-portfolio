//! Alpha Vantage daily and weekly prices (`TIME_SERIES_DAILY`, `TIME_SERIES_WEEKLY`).
//! Read-only; needs a free API key.
//!
//! Docs: <https://www.alphavantage.co/documentation/#daily>. Errors arrive as HTTP 200 with an
//! "Error Message", "Information" or "Note" field instead of data. The free tier allows about
//! 25 requests a day, rejects quick bursts, and serves "compact" daily output (the latest 100 trading days); the full daily
//! history is premium, but the weekly series goes back to the listing.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Days, NaiveDate, TimeDelta};
use portfolio_core::price::DailyClose;
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::DailySource;
use crate::prices::Clock;

pub const DEFAULT_BASE_URL: &str = "https://www.alphavantage.co";
pub const API_KEY_ENV: &str = "ALPHAVANTAGE_API_KEY";
/// Default pause between requests. The free tier answers back-to-back requests with a
/// rate-limit notice even 1.1 s apart (its documented limit is one per second); 5 s works
/// (checked 2026-10-15).
pub const DEFAULT_REQUEST_SPACING: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum AlphaVantageError {
    /// The API key environment variable is not set.
    MissingKey,
    /// Transport failure. The URL (which carries the key) is stripped from the error.
    Http(reqwest::Error),
    Status(u16),
    /// The API answered with an error or notice instead of data (bad symbol, rate limit, ...).
    Api(String),
    Parse(String),
}

impl std::fmt::Display for AlphaVantageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlphaVantageError::MissingKey => write!(f, "{API_KEY_ENV} is not set"),
            AlphaVantageError::Http(e) => write!(f, "Alpha Vantage request failed: {e}"),
            AlphaVantageError::Status(code) => write!(f, "Alpha Vantage answered HTTP {code}"),
            AlphaVantageError::Api(msg) => write!(f, "Alpha Vantage: {msg}"),
            AlphaVantageError::Parse(msg) => write!(f, "unexpected Alpha Vantage response: {msg}"),
        }
    }
}

impl std::error::Error for AlphaVantageError {}

impl From<reqwest::Error> for AlphaVantageError {
    fn from(e: reqwest::Error) -> Self {
        AlphaVantageError::Http(e.without_url())
    }
}

#[derive(Deserialize)]
struct SeriesResponse {
    #[serde(rename = "Meta Data")]
    meta: Option<Meta>,
    #[serde(rename = "Time Series (Daily)")]
    daily: Option<BTreeMap<NaiveDate, Bar>>,
    /// Keyed by the last trading day of each week; the newest entry is the week in progress.
    #[serde(rename = "Weekly Time Series")]
    weekly: Option<BTreeMap<NaiveDate, Bar>>,
    #[serde(rename = "Error Message")]
    error: Option<String>,
    #[serde(rename = "Information")]
    information: Option<String>,
    #[serde(rename = "Note")]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "2. Symbol")]
    symbol: String,
}

#[derive(Deserialize)]
struct Bar {
    // Prices arrive as strings ("227.8800"), so they go straight to Decimal.
    #[serde(rename = "4. close")]
    close: String,
}

#[derive(Clone, Copy)]
enum Series {
    Daily,
    Weekly,
}

/// Parses a `TIME_SERIES_DAILY` body into closes, oldest first.
pub fn parse_daily(
    body: &str,
    expected_symbol: &str,
) -> Result<Vec<DailyClose>, AlphaVantageError> {
    parse_series(body, expected_symbol, Series::Daily)
}

/// Parses a `TIME_SERIES_WEEKLY` body into week-end closes, oldest first.
///
/// Each close is the close of the week's last trading day and is dated with that day (a Friday,
/// or earlier when the exchange was closed on Friday). The newest entry is dropped when it is
/// dated after the last Friday that is fully over at `clock`, because that week is still in
/// progress and its "close" is just the latest daily close.
pub fn parse_weekly(
    body: &str,
    expected_symbol: &str,
    clock: Clock,
) -> Result<Vec<DailyClose>, AlphaVantageError> {
    let mut closes = parse_series(body, expected_symbol, Series::Weekly)?;
    let completed = last_completed_friday(clock.today);
    if closes.last().is_some_and(|c| c.date > completed) {
        closes.pop();
    }
    Ok(closes)
}

/// The latest Friday whose trading session is over by the start of `today`: today's own Friday
/// does not count yet, Saturday and Sunday count the Friday just gone.
pub fn last_completed_friday(today: NaiveDate) -> NaiveDate {
    let since_monday = u64::from(today.weekday().num_days_from_monday());
    // Monday -> 3 days back, ..., Friday -> 7, Saturday -> 1, Sunday -> 2.
    today - Days::new((since_monday + 2) % 7 + 1)
}

fn parse_series(
    body: &str,
    expected_symbol: &str,
    kind: Series,
) -> Result<Vec<DailyClose>, AlphaVantageError> {
    let response: SeriesResponse =
        serde_json::from_str(body).map_err(|e| AlphaVantageError::Parse(e.to_string()))?;
    if let Some(msg) = response.error.or(response.information).or(response.note) {
        return Err(AlphaVantageError::Api(msg));
    }
    let series = match kind {
        Series::Daily => response.daily,
        Series::Weekly => response.weekly,
    };
    let (Some(meta), Some(series)) = (response.meta, series) else {
        return Err(AlphaVantageError::Parse(
            "no time series in response".into(),
        ));
    };
    if !meta.symbol.eq_ignore_ascii_case(expected_symbol) {
        return Err(AlphaVantageError::Parse(format!(
            "asked for {expected_symbol}, got {}",
            meta.symbol
        )));
    }
    series
        .into_iter()
        .map(|(date, bar)| {
            let close = Decimal::from_str(&bar.close)
                .map_err(|e| AlphaVantageError::Parse(format!("bad close {:?}: {e}", bar.close)))?;
            if close <= Decimal::ZERO {
                return Err(AlphaVantageError::Parse(format!(
                    "non-positive close on {date}"
                )));
            }
            Ok(DailyClose { date, close })
        })
        .collect()
}

#[derive(Clone)]
pub struct AlphaVantageClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    request_spacing: Duration,
    /// When the last request was sent, shared by clones so they are spaced out together.
    last_request: Arc<Mutex<Option<Instant>>>,
}

// Hand-written so the key never ends up in logs or panic messages.
impl std::fmt::Debug for AlphaVantageClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlphaVantageClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl AlphaVantageClient {
    /// Reads the key from `ALPHAVANTAGE_API_KEY`.
    pub fn from_env() -> Result<Self, AlphaVantageError> {
        let key = std::env::var(API_KEY_ENV)
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or(AlphaVantageError::MissingKey)?;
        Self::new(DEFAULT_BASE_URL, key)
    }

    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, AlphaVantageError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("portfolio/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            request_spacing: DEFAULT_REQUEST_SPACING,
            last_request: Arc::new(Mutex::new(None)),
        })
    }

    /// Changes the minimum pause between requests (tests against a local mock use a short one).
    pub fn with_request_spacing(mut self, spacing: Duration) -> Self {
        self.request_spacing = spacing;
        self
    }

    /// GET `/query` with the key added, at most one request per `request_spacing`.
    async fn query(&self, params: &[(&str, &str)]) -> Result<String, AlphaVantageError> {
        {
            // Held while waiting, so concurrent callers queue up behind each other.
            let mut last = self.last_request.lock().await;
            if let Some(at) = *last {
                tokio::time::sleep_until(at + self.request_spacing).await;
            }
            *last = Some(Instant::now());
        }
        let response = self
            .http
            .get(format!("{}/query", self.base_url))
            .query(params)
            .query(&[("apikey", self.api_key.as_str())])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AlphaVantageError::Status(status.as_u16()));
        }
        Ok(response.text().await?)
    }

    /// The latest ~100 daily closes for `symbol` (e.g. "VWCE.DEX" for Xetra), oldest first.
    pub async fn daily_closes(&self, symbol: &str) -> Result<Vec<DailyClose>, AlphaVantageError> {
        let body = self
            .query(&[
                ("function", "TIME_SERIES_DAILY"),
                ("symbol", symbol),
                ("outputsize", "compact"),
            ])
            .await?;
        parse_daily(&body, symbol)
    }

    /// Completed weeks' closes for `symbol` over its whole history, oldest first (see
    /// [`parse_weekly`]). Free, unlike the full daily history.
    pub async fn weekly_closes(
        &self,
        symbol: &str,
        clock: Clock,
    ) -> Result<Vec<DailyClose>, AlphaVantageError> {
        let body = self
            .query(&[("function", "TIME_SERIES_WEEKLY"), ("symbol", symbol)])
            .await?;
        parse_weekly(&body, symbol, clock)
    }
}

impl DailySource for AlphaVantageClient {
    const NAME: &'static str = "alphavantage";
    // The free tier allows ~25 requests a day.
    const MIN_REFETCH_INTERVAL: TimeDelta = TimeDelta::hours(6);
    type Error = AlphaVantageError;

    fn daily_closes(
        &self,
        symbol: &str,
    ) -> impl Future<Output = Result<Vec<DailyClose>, AlphaVantageError>> + Send {
        AlphaVantageClient::daily_closes(self, symbol)
    }
}
