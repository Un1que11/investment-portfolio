//! Kraken private REST API, read-only: `Balance` and `Ledgers` only.
//!
//! Docs: <https://docs.kraken.com/api/docs/guides/spot-rest-auth>. Requests are POSTs signed
//! with HMAC-SHA512 over `path + SHA256(nonce + body)` using the base64-decoded private key.
//! This client deliberately contains no trading, funding or withdrawal endpoints, so even a
//! key with more permissions than needed cannot be used for anything but reading.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};

pub const DEFAULT_BASE_URL: &str = "https://api.kraken.com";
pub const API_KEY_ENV: &str = "KRAKEN_API_KEY";
pub const PRIVATE_KEY_ENV: &str = "KRAKEN_PRIVATE_KEY";

#[derive(Debug)]
pub enum KrakenPrivateError {
    /// An environment variable with a key is missing or empty.
    MissingKey(&'static str),
    /// The private key is not valid base64.
    InvalidKey,
    /// Transport failure (the request URL carries no secrets, but is stripped anyway).
    Http(reqwest::Error),
    Status(u16),
    /// Kraken's own error codes, e.g. "EAPI:Invalid key".
    Api(Vec<String>),
    Parse(String),
}

impl std::fmt::Display for KrakenPrivateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KrakenPrivateError::MissingKey(var) => write!(f, "{var} is not set"),
            KrakenPrivateError::InvalidKey => write!(f, "{PRIVATE_KEY_ENV} is not valid base64"),
            KrakenPrivateError::Http(e) => write!(f, "Kraken request failed: {e}"),
            KrakenPrivateError::Status(code) => write!(f, "Kraken answered HTTP {code}"),
            KrakenPrivateError::Api(errors) => write!(f, "Kraken: {}", errors.join(", ")),
            KrakenPrivateError::Parse(msg) => write!(f, "unexpected Kraken response: {msg}"),
        }
    }
}

impl std::error::Error for KrakenPrivateError {}

impl From<reqwest::Error> for KrakenPrivateError {
    fn from(e: reqwest::Error) -> Self {
        KrakenPrivateError::Http(e.without_url())
    }
}

/// `API-Sign` for a request: base64(HMAC-SHA512(path + SHA256(nonce + body), secret)).
pub fn sign(path: &str, nonce: u64, body: &str, secret: &[u8]) -> String {
    let digest = Sha256::digest(format!("{nonce}{body}").as_bytes());
    let mut mac = Hmac::<Sha512>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(path.as_bytes());
    mac.update(&digest);
    BASE64.encode(mac.finalize().into_bytes())
}

/// One ledger entry, with numbers kept as their exact decimal text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LedgerEntry {
    #[serde(skip)]
    pub id: String,
    pub refid: String,
    /// Unix time with fractional seconds, as sent (e.g. "1729166612.3456").
    #[serde(deserialize_with = "number_text")]
    pub time: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub aclass: String,
    pub asset: String,
    #[serde(deserialize_with = "number_text")]
    pub amount: String,
    #[serde(deserialize_with = "number_text")]
    pub fee: String,
    #[serde(deserialize_with = "number_text")]
    pub balance: String,
}

/// Accepts a JSON number or a numeric string and keeps its text (serde_json's
/// arbitrary_precision preserves numbers exactly).
fn number_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    match value {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected a number, got {other}"
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPage {
    /// Entries on this page, newest first as Kraken returns them.
    pub entries: Vec<LedgerEntry>,
    /// Total number of entries matching the query (across all pages).
    pub count: u64,
}

#[derive(Deserialize)]
struct Envelope<T> {
    error: Vec<String>,
    result: Option<T>,
}

#[derive(Deserialize)]
struct LedgersResult {
    ledger: BTreeMap<String, LedgerEntry>,
    count: u64,
}

pub fn parse_ledgers(body: &str) -> Result<LedgerPage, KrakenPrivateError> {
    let envelope: Envelope<LedgersResult> =
        serde_json::from_str(body).map_err(|e| KrakenPrivateError::Parse(e.to_string()))?;
    if !envelope.error.is_empty() {
        return Err(KrakenPrivateError::Api(envelope.error));
    }
    let result = envelope
        .result
        .ok_or_else(|| KrakenPrivateError::Parse("no result".into()))?;
    let mut entries: Vec<LedgerEntry> = result
        .ledger
        .into_iter()
        .map(|(id, mut e)| {
            e.id = id;
            e
        })
        .collect();
    // Newest first, like the API (the map above is keyed by id, not ordered by time).
    entries.sort_by(|a, b| {
        b.time
            .parse::<f64>()
            .unwrap_or(0.0)
            .total_cmp(&a.time.parse::<f64>().unwrap_or(0.0))
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(LedgerPage {
        entries,
        count: result.count,
    })
}

pub fn parse_balance(body: &str) -> Result<BTreeMap<String, String>, KrakenPrivateError> {
    let envelope: Envelope<BTreeMap<String, serde_json::Value>> =
        serde_json::from_str(body).map_err(|e| KrakenPrivateError::Parse(e.to_string()))?;
    if !envelope.error.is_empty() {
        return Err(KrakenPrivateError::Api(envelope.error));
    }
    Ok(envelope
        .result
        .ok_or_else(|| KrakenPrivateError::Parse("no result".into()))?
        .into_iter()
        .map(|(asset, v)| {
            let text = match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            (asset, text)
        })
        .collect())
}

pub struct KrakenPrivateClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    secret: Vec<u8>,
    last_nonce: AtomicU64,
}

// Hand-written so neither key ends up in logs or panic messages.
impl std::fmt::Debug for KrakenPrivateClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KrakenPrivateClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl KrakenPrivateClient {
    /// Keys from `KRAKEN_API_KEY` and `KRAKEN_PRIVATE_KEY`.
    pub fn from_env() -> Result<Self, KrakenPrivateError> {
        let read = |var: &'static str| {
            std::env::var(var)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .ok_or(KrakenPrivateError::MissingKey(var))
        };
        Self::new(
            DEFAULT_BASE_URL,
            read(API_KEY_ENV)?,
            &read(PRIVATE_KEY_ENV)?,
        )
    }

    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        private_key_base64: &str,
    ) -> Result<Self, KrakenPrivateError> {
        let secret = BASE64
            .decode(private_key_base64.trim())
            .map_err(|_| KrakenPrivateError::InvalidKey)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("portfolio/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            secret,
            last_nonce: AtomicU64::new(0),
        })
    }

    /// Milliseconds since the epoch, strictly increasing per client (Kraken rejects reused or
    /// decreasing nonces).
    fn next_nonce(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let mut last = self.last_nonce.load(Ordering::SeqCst);
        loop {
            let next = now.max(last + 1);
            match self
                .last_nonce
                .compare_exchange(last, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(actual) => last = actual,
            }
        }
    }

    async fn post(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<String, KrakenPrivateError> {
        let path = format!("/0/private/{method}");
        let nonce = self.next_nonce();
        let mut form: Vec<(&str, String)> = vec![("nonce", nonce.to_string())];
        form.extend(params.iter().cloned());
        let body = form
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let response = self
            .http
            .post(format!("{}{path}", self.base_url))
            .header("API-Key", &self.api_key)
            .header("API-Sign", sign(&path, nonce, &body, &self.secret))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(KrakenPrivateError::Status(status.as_u16()));
        }
        Ok(response.text().await?)
    }

    /// Asset balances as exact decimal text, e.g. {"XXBT": "0.0123400000"}.
    pub async fn balance(&self) -> Result<BTreeMap<String, String>, KrakenPrivateError> {
        parse_balance(&self.post("Balance", &[]).await?)
    }

    /// One page of ledger entries (up to 50), newest first. `start` is exclusive: a ledger id
    /// or unix time; entries at or before it are not returned. `ofs` skips that many results.
    pub async fn ledgers(
        &self,
        start: Option<&str>,
        ofs: u64,
    ) -> Result<LedgerPage, KrakenPrivateError> {
        let mut params = vec![("ofs", ofs.to_string())];
        if let Some(start) = start {
            params.push(("start", start.to_owned()));
        }
        parse_ledgers(&self.post("Ledgers", &params).await?)
    }
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
