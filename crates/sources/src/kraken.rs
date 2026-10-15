//! Kraken public market data: `Ticker` (live last trade) and `OHLC` (daily candles).
//!
//! Docs: <https://docs.kraken.com/api/docs/rest-api/get-ticker-information>. Public endpoints,
//! no key, read-only. Errors arrive as HTTP 200 with a non-empty `error` array. Kraken has no
//! PLN markets, so BTC and ETH are taken in EUR and converted with the NBP rate.

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, TimeDelta};
use portfolio_core::price::DailyClose;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use crate::DailySource;

pub const DEFAULT_BASE_URL: &str = "https://api.kraken.com";
const DAILY_INTERVAL_MINUTES: &str = "1440";

#[derive(Debug)]
pub enum KrakenError {
    Http(reqwest::Error),
    Status(u16),
    /// Kraken's own error codes, e.g. "EQuery:Unknown asset pair".
    Api(Vec<String>),
    Parse(String),
}

impl std::fmt::Display for KrakenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KrakenError::Http(e) => write!(f, "Kraken request failed: {e}"),
            KrakenError::Status(code) => write!(f, "Kraken answered HTTP {code}"),
            KrakenError::Api(errors) => write!(f, "Kraken: {}", errors.join(", ")),
            KrakenError::Parse(msg) => write!(f, "unexpected Kraken response: {msg}"),
        }
    }
}

impl std::error::Error for KrakenError {}

impl From<reqwest::Error> for KrakenError {
    fn from(e: reqwest::Error) -> Self {
        KrakenError::Http(e)
    }
}

#[derive(Deserialize)]
struct Envelope {
    error: Vec<String>,
    result: Option<serde_json::Map<String, Value>>,
}

fn result(body: &str) -> Result<serde_json::Map<String, Value>, KrakenError> {
    let envelope: Envelope =
        serde_json::from_str(body).map_err(|e| KrakenError::Parse(e.to_string()))?;
    if !envelope.error.is_empty() {
        return Err(KrakenError::Api(envelope.error));
    }
    envelope
        .result
        .ok_or_else(|| KrakenError::Parse("no result".into()))
}

fn decimal(value: &Value, what: &str) -> Result<Decimal, KrakenError> {
    // Kraken sends prices as strings ("71806.40000"); never go through f64.
    let text = value
        .as_str()
        .ok_or_else(|| KrakenError::Parse(format!("{what} is not a string")))?;
    let d = Decimal::from_str(text)
        .map_err(|e| KrakenError::Parse(format!("bad {what} {text:?}: {e}")))?;
    if d <= Decimal::ZERO {
        return Err(KrakenError::Parse(format!("non-positive {what} {text}")));
    }
    Ok(d)
}

/// Parses a `Ticker` body into pair -> last trade price. Pairs are Kraken's canonical names
/// (e.g. "XXBTZEUR"), which is also what this module requests.
pub fn parse_ticker(body: &str) -> Result<HashMap<String, Decimal>, KrakenError> {
    result(body)?
        .into_iter()
        .map(|(pair, info)| {
            // "c" = [last trade price, lot volume].
            let last = info
                .get("c")
                .and_then(|c| c.get(0))
                .ok_or_else(|| KrakenError::Parse(format!("no last trade for {pair}")))?;
            Ok((pair.clone(), decimal(last, &format!("{pair} last price"))?))
        })
        .collect()
}

/// Parses a daily `OHLC` body into closes, oldest first. The newest candle is the day still in
/// progress and is left out: only candles up to the `last` timestamp are final.
pub fn parse_daily_ohlc(body: &str, pair: &str) -> Result<Vec<DailyClose>, KrakenError> {
    let result = result(body)?;
    let last = result
        .get("last")
        .and_then(Value::as_i64)
        .ok_or_else(|| KrakenError::Parse("no 'last' timestamp".into()))?;
    let candles = result
        .get(pair)
        .and_then(Value::as_array)
        .ok_or_else(|| KrakenError::Parse(format!("no candles for {pair}")))?;
    let mut closes = Vec::with_capacity(candles.len());
    for candle in candles {
        // [time, open, high, low, close, vwap, volume, count]; time is the UTC day start.
        let time = candle
            .get(0)
            .and_then(Value::as_i64)
            .ok_or_else(|| KrakenError::Parse("candle without time".into()))?;
        if time > last {
            continue;
        }
        let date = DateTime::from_timestamp(time, 0)
            .ok_or_else(|| KrakenError::Parse(format!("bad candle time {time}")))?
            .date_naive();
        let close = candle
            .get(4)
            .ok_or_else(|| KrakenError::Parse(format!("candle {date} without close")))?;
        closes.push(DailyClose {
            date,
            close: decimal(close, &format!("{date} close"))?,
        });
    }
    closes.sort_by_key(|c| c.date);
    Ok(closes)
}

#[derive(Debug, Clone)]
pub struct KrakenClient {
    http: reqwest::Client,
    base_url: String,
}

impl KrakenClient {
    pub fn new() -> Result<Self, KrakenError> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self, KrakenError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("portfolio/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        })
    }

    async fn get(&self, endpoint: &str, query: &[(&str, &str)]) -> Result<String, KrakenError> {
        let response = self
            .http
            .get(format!("{}/0/public/{endpoint}", self.base_url))
            .query(query)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(KrakenError::Status(status.as_u16()));
        }
        Ok(response.text().await?)
    }

    /// Last trade price for each pair (canonical names, e.g. "XXBTZEUR").
    pub async fn last_prices(
        &self,
        pairs: &[&str],
    ) -> Result<HashMap<String, Decimal>, KrakenError> {
        let joined = pairs.join(",");
        let prices = parse_ticker(&self.get("Ticker", &[("pair", &joined)]).await?)?;
        if let Some(missing) = pairs.iter().find(|p| !prices.contains_key(**p)) {
            return Err(KrakenError::Parse(format!("no ticker for {missing}")));
        }
        Ok(prices)
    }

    /// Final daily closes (UTC days) for the pair, oldest first; Kraken returns about 720.
    pub async fn daily_ohlc(&self, pair: &str) -> Result<Vec<DailyClose>, KrakenError> {
        let body = self
            .get(
                "OHLC",
                &[("pair", pair), ("interval", DAILY_INTERVAL_MINUTES)],
            )
            .await?;
        parse_daily_ohlc(&body, pair)
    }
}

impl DailySource for KrakenClient {
    const NAME: &'static str = "kraken";
    // Public endpoint with generous limits; hourly is plenty for daily closes.
    const MIN_REFETCH_INTERVAL: TimeDelta = TimeDelta::hours(1);
    type Error = KrakenError;

    fn daily_closes(
        &self,
        symbol: &str,
    ) -> impl Future<Output = Result<Vec<DailyClose>, KrakenError>> + Send {
        self.daily_ohlc(symbol)
    }
}
