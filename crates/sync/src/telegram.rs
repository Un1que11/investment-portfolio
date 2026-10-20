//! Sending a message to a Telegram chat through the Bot API (`sendMessage`), for the monthly
//! summary (BACKLOG P26). The bot is the F1 digest's: `TELEGRAM_BOT_TOKEN` and
//! `TELEGRAM_CHAT_ID`, from the environment or `.env`.
//!
//! Docs: <https://core.telegram.org/bots/api#sendmessage>. The token is part of every Bot API
//! URL, so it is stripped from transport errors, never printed by `Debug`, and an error from
//! Telegram is reported by its status and `description` only.

use std::future::Future;
use std::time::Duration;

use serde::Deserialize;

pub const DEFAULT_BASE_URL: &str = "https://api.telegram.org";
pub const TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";
pub const CHAT_ID_ENV: &str = "TELEGRAM_CHAT_ID";

/// Where a summary goes. [`TelegramClient`] sends it; tests use a recorder instead.
pub trait MessageSender {
    fn send(&self, text: &str) -> impl Future<Output = Result<(), TelegramError>> + Send;
}

#[derive(Debug)]
pub enum TelegramError {
    /// A required environment variable is not set.
    Missing(&'static str),
    /// Transport failure. The URL (which carries the token) is stripped from the error.
    Http(reqwest::Error),
    /// Telegram refused the message (bad chat id, revoked token, flood limit, ...).
    Api { status: u16, description: String },
}

impl std::fmt::Display for TelegramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelegramError::Missing(var) => write!(f, "{var} is not set"),
            TelegramError::Http(e) => write!(f, "Telegram request failed: {e}"),
            TelegramError::Api {
                status,
                description,
            } => write!(f, "Telegram answered HTTP {status}: {description}"),
        }
    }
}

impl std::error::Error for TelegramError {}

impl From<reqwest::Error> for TelegramError {
    fn from(e: reqwest::Error) -> Self {
        TelegramError::Http(e.without_url())
    }
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    description: Option<String>,
}

pub struct TelegramClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    chat_id: String,
}

// Hand-written so the token and the chat never end up in logs or panic messages.
impl std::fmt::Debug for TelegramClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramClient")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .field("chat_id", &"<redacted>")
            .finish()
    }
}

impl TelegramClient {
    /// Reads `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID`.
    pub fn from_env() -> Result<Self, TelegramError> {
        let var = |name: &'static str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .ok_or(TelegramError::Missing(name))
        };
        Self::new(DEFAULT_BASE_URL, var(TOKEN_ENV)?, var(CHAT_ID_ENV)?)
    }

    pub fn new(
        base_url: impl Into<String>,
        token: impl Into<String>,
        chat_id: impl Into<String>,
    ) -> Result<Self, TelegramError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("portfolio/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            token: token.into(),
            chat_id: chat_id.into(),
        })
    }

    /// Sends `text` as plain text (no parse mode, so nothing in it needs escaping).
    pub async fn send_message(&self, text: &str) -> Result<(), TelegramError> {
        let response = self
            .http
            .post(format!("{}/bot{}/sendMessage", self.base_url, self.token))
            .form(&[("chat_id", self.chat_id.as_str()), ("text", text)])
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        let reply: Option<Reply> = serde_json::from_str(&body).ok();
        match reply {
            Some(Reply { ok: true, .. }) if status.is_success() => Ok(()),
            other => Err(TelegramError::Api {
                status: status.as_u16(),
                description: other
                    .and_then(|r| r.description)
                    .map(|d| {
                        d.replace(&self.token, "<redacted>")
                            .chars()
                            .take(200)
                            .collect()
                    })
                    .unwrap_or_else(|| "no description".to_owned()),
            }),
        }
    }
}

impl MessageSender for TelegramClient {
    fn send(&self, text: &str) -> impl Future<Output = Result<(), TelegramError>> + Send {
        self.send_message(text)
    }
}
