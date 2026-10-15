//! Alpha Vantage daily prices (`TIME_SERIES_DAILY`). Read-only; needs a free API key.
//!
//! Docs: <https://www.alphavantage.co/documentation/#daily>. Errors arrive as HTTP 200 with an
//! "Error Message", "Information" or "Note" field instead of data. The free tier allows about
//! 25 requests a day and "compact" output (the latest 100 trading days).

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use chrono::NaiveDate;
use portfolio_core::price::DailyClose;
use rust_decimal::Decimal;
use serde::Deserialize;

pub const DEFAULT_BASE_URL: &str = "https://www.alphavantage.co";
pub const API_KEY_ENV: &str = "ALPHAVANTAGE_API_KEY";

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
struct DailyResponse {
    #[serde(rename = "Meta Data")]
    meta: Option<Meta>,
    #[serde(rename = "Time Series (Daily)")]
    series: Option<BTreeMap<NaiveDate, Bar>>,
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

/// Parses a `TIME_SERIES_DAILY` body into closes, oldest first.
pub fn parse_daily(
    body: &str,
    expected_symbol: &str,
) -> Result<Vec<DailyClose>, AlphaVantageError> {
    let response: DailyResponse =
        serde_json::from_str(body).map_err(|e| AlphaVantageError::Parse(e.to_string()))?;
    if let Some(msg) = response.error.or(response.information).or(response.note) {
        return Err(AlphaVantageError::Api(msg));
    }
    let (Some(meta), Some(series)) = (response.meta, response.series) else {
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
        })
    }

    /// The latest ~100 daily closes for `symbol` (e.g. "VWCE.DEX" for Xetra), oldest first.
    pub async fn daily_closes(&self, symbol: &str) -> Result<Vec<DailyClose>, AlphaVantageError> {
        let response = self
            .http
            .get(format!("{}/query", self.base_url))
            .query(&[
                ("function", "TIME_SERIES_DAILY"),
                ("symbol", symbol),
                ("outputsize", "compact"),
                ("apikey", self.api_key.as_str()),
            ])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AlphaVantageError::Status(status.as_u16()));
        }
        parse_daily(&response.text().await?, symbol)
    }
}
