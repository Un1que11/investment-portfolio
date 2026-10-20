//! NBP (Narodowy Bank Polski) API: table A exchange rates and the gold price.
//!
//! Docs: <https://api.nbp.pl/>. No key needed. A date or range with no published table
//! (weekend, holiday) answers 404; a range that ends in the future answers 400.

use std::str::FromStr;
use std::time::Duration;

use chrono::{NaiveDate, TimeDelta};
use portfolio_core::fx::FxRate;
use portfolio_core::price::DailyClose;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::DailySource;

pub const DEFAULT_BASE_URL: &str = "https://api.nbp.pl/api";

#[derive(Debug)]
pub enum NbpError {
    Http(reqwest::Error),
    Status(u16, String),
    Parse(String),
}

impl std::fmt::Display for NbpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NbpError::Http(e) => write!(f, "NBP request failed: {e}"),
            NbpError::Status(code, body) => write!(f, "NBP answered {code}: {body}"),
            NbpError::Parse(msg) => write!(f, "unexpected NBP response: {msg}"),
        }
    }
}

impl std::error::Error for NbpError {}

impl From<reqwest::Error> for NbpError {
    fn from(e: reqwest::Error) -> Self {
        NbpError::Http(e)
    }
}

#[derive(Deserialize)]
struct Table {
    table: String,
    code: String,
    rates: Vec<Rate>,
}

#[derive(Deserialize)]
struct Rate {
    no: String,
    #[serde(rename = "effectiveDate")]
    effective_date: NaiveDate,
    // Kept as the original JSON text (serde_json "arbitrary_precision"), never as f64.
    mid: serde_json::Number,
}

/// Parses an `exchangerates/rates/a/{code}/...` response body.
pub fn parse_table_a(body: &str, expected_currency: &str) -> Result<Vec<FxRate>, NbpError> {
    let table: Table = serde_json::from_str(body).map_err(|e| NbpError::Parse(e.to_string()))?;
    if table.table != "A" {
        return Err(NbpError::Parse(format!(
            "expected table A, got {}",
            table.table
        )));
    }
    if !table.code.eq_ignore_ascii_case(expected_currency) {
        return Err(NbpError::Parse(format!(
            "asked for {expected_currency}, got {}",
            table.code
        )));
    }
    table
        .rates
        .into_iter()
        .map(|r| {
            let text = r.mid.to_string();
            let mid = Decimal::from_str(&text)
                .map_err(|e| NbpError::Parse(format!("bad mid {text:?}: {e}")))?;
            if mid <= Decimal::ZERO {
                return Err(NbpError::Parse(format!("non-positive mid {text}")));
            }
            Ok(FxRate {
                currency: table.code.to_ascii_uppercase(),
                effective_date: r.effective_date,
                mid,
                table_no: r.no,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct GoldPrice {
    data: NaiveDate,
    // PLN per gram of fine gold, as the original JSON text (never f64).
    cena: serde_json::Number,
}

/// Symbol accepted by the gold [`DailySource`] implementation.
pub const GOLD_SYMBOL: &str = "XAU";
/// How many recent gold quotes to fetch (the API allows at most 255).
pub const GOLD_LAST_COUNT: u32 = 90;

/// Parses a `cenyzlota` response: PLN per gram of gold (fineness 1000), oldest first.
pub fn parse_gold(body: &str) -> Result<Vec<DailyClose>, NbpError> {
    let rows: Vec<GoldPrice> =
        serde_json::from_str(body).map_err(|e| NbpError::Parse(e.to_string()))?;
    let mut closes = rows
        .into_iter()
        .map(|r| {
            let text = r.cena.to_string();
            let close = Decimal::from_str(&text)
                .map_err(|e| NbpError::Parse(format!("bad gold price {text:?}: {e}")))?;
            if close <= Decimal::ZERO {
                return Err(NbpError::Parse(format!("non-positive gold price {text}")));
            }
            Ok(DailyClose {
                date: r.data,
                close,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    closes.sort_by_key(|c| c.date);
    Ok(closes)
}

#[derive(Debug, Clone)]
pub struct NbpClient {
    http: reqwest::Client,
    base_url: String,
}

impl NbpClient {
    pub fn new() -> Result<Self, NbpError> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// For tests: point the client at a mock server.
    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self, NbpError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("portfolio/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        })
    }

    /// Table A mid rates for `currency` published in `from..=to`. Days without a table are
    /// simply absent; a range with no table at all yields an empty list. `to` must not be in
    /// the future (NBP rejects such ranges).
    pub async fn table_a_range(
        &self,
        currency: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<FxRate>, NbpError> {
        let url = format!(
            "{}/exchangerates/rates/a/{}/{}/{}/",
            self.base_url,
            currency.to_ascii_lowercase(),
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d"),
        );
        match self.get(&url).await? {
            Some(body) => parse_table_a(&body, currency),
            None => Ok(Vec::new()),
        }
    }

    /// The latest `count` published gold prices (PLN per gram), oldest first.
    pub async fn gold_last(&self, count: u32) -> Result<Vec<DailyClose>, NbpError> {
        let url = format!("{}/cenyzlota/last/{count}/", self.base_url);
        match self.get(&url).await? {
            Some(body) => parse_gold(&body),
            None => Ok(Vec::new()),
        }
    }

    /// Gold prices (PLN per gram) published in `from..=to`, oldest first; empty when there is
    /// none. NBP allows at most 367 days per request and refuses a `to` in the future.
    pub async fn gold_range(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<DailyClose>, NbpError> {
        let url = format!(
            "{}/cenyzlota/{}/{}/",
            self.base_url,
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d"),
        );
        match self.get(&url).await? {
            Some(body) => parse_gold(&body),
            None => Ok(Vec::new()),
        }
    }

    /// GET with JSON accepted; `None` for 404 (NBP's "no data"), an error for other failures.
    async fn get(&self, url: &str) -> Result<Option<String>, NbpError> {
        let response = self
            .http
            .get(url)
            .query(&[("format", "json")])
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        match status.as_u16() {
            200 => Ok(Some(body)),
            404 => Ok(None),
            code => Err(NbpError::Status(code, body.trim().to_owned())),
        }
    }
}

/// Gold price per gram as a daily price series (symbol [`GOLD_SYMBOL`]).
impl DailySource for NbpClient {
    const NAME: &'static str = "nbp";
    // Published once per business day around noon; hourly rechecks are plenty.
    const MIN_REFETCH_INTERVAL: TimeDelta = TimeDelta::hours(1);
    type Error = NbpError;

    async fn daily_closes(&self, symbol: &str) -> Result<Vec<DailyClose>, NbpError> {
        gold_only(symbol)?;
        self.gold_last(GOLD_LAST_COUNT).await
    }

    /// Older gold prices: the recent window holds only the last [`GOLD_LAST_COUNT`] quotes.
    async fn closes_between(
        &self,
        symbol: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Option<Vec<DailyClose>>, NbpError> {
        gold_only(symbol)?;
        self.gold_range(from, to).await.map(Some)
    }
}

fn gold_only(symbol: &str) -> Result<(), NbpError> {
    if symbol == GOLD_SYMBOL {
        Ok(())
    } else {
        Err(NbpError::Parse(format!(
            "NBP only provides a daily price for {GOLD_SYMBOL}, not {symbol}"
        )))
    }
}
