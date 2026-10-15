//! NBP (Narodowy Bank Polski) exchange rates API, table A mid rates.
//!
//! Docs: <https://api.nbp.pl/>. No key needed. A date or range with no published table
//! (weekend, holiday) answers 404; a range that ends in the future answers 400.

use std::str::FromStr;
use std::time::Duration;

use chrono::NaiveDate;
use portfolio_core::fx::FxRate;
use rust_decimal::Decimal;
use serde::Deserialize;

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
        let response = self
            .http
            .get(&url)
            .query(&[("format", "json")])
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        match status.as_u16() {
            200 => parse_table_a(&body, currency),
            404 => Ok(Vec::new()),
            code => Err(NbpError::Status(code, body.trim().to_owned())),
        }
    }
}
