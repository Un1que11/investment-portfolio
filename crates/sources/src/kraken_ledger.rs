//! Kraken account sync: fetch new ledger entries (read-only), keep them raw, and map them into
//! `transactions`.
//!
//! Patterns seen in a real account (crypto bought with Kraken's instant Buy, not via trades,
//! so `TradesHistory` is empty and the ledger is the only source):
//! - `deposit` of ZEUR (or EUR.HOLD for card purchases), sometimes with a fee;
//! - `spend` of fiat + `receive` of crypto sharing a refid: a purchase (several pairs in one
//!   refid for a multi-asset bundle buy);
//! - `staking` of ETH: a reward, with Kraken's commission as the fee;
//! - `bundletransfer` dissolving a bundle into spot: moves that net to zero per asset.
//!
//! Anything else is kept raw and reported as unsupported rather than guessed at.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime};
use portfolio_core::AssetClass;
use portfolio_db::kraken::{self as raw, RawLedgerEntry};
use portfolio_db::transactions::{Account, Asset, NewTransaction, insert_transactions};
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::kraken_private::{KrakenPrivateClient, KrakenPrivateError, LedgerEntry};

pub const SOURCE: &str = "kraken";

pub const ACCOUNT: Account<'static> = Account {
    name: "Kraken",
    kind: "exchange",
    provider: "kraken",
    currency: "EUR",
};

/// Crypto assets the portfolio tracks: Kraken asset code -> asset row.
const CRYPTO: &[(&str, Asset<'static>)] = &[
    (
        "XXBT",
        Asset {
            symbol: "BTC",
            name: "Bitcoin",
            class: AssetClass::Crypto,
            currency: "EUR",
        },
    ),
    (
        "XETH",
        Asset {
            symbol: "ETH",
            name: "Ether",
            class: AssetClass::Crypto,
            currency: "EUR",
        },
    ),
];

fn crypto(asset: &str) -> Option<&'static Asset<'static>> {
    CRYPTO
        .iter()
        .find(|(code, _)| *code == asset)
        .map(|(_, a)| a)
}

/// Fiat currency of a Kraken asset code: "ZEUR" -> "EUR"; "EUR.HOLD" (funds held for a card
/// purchase) -> "EUR".
fn fiat(asset: &str) -> Option<&'static str> {
    match asset.strip_suffix(".HOLD").unwrap_or(asset) {
        "ZEUR" | "EUR" => Some("EUR"),
        "ZPLN" | "PLN" => Some("PLN"),
        "ZUSD" | "USD" => Some("USD"),
        _ => None,
    }
}

#[derive(Debug)]
pub enum SyncError {
    Kraken(KrakenPrivateError),
    Db(sqlx::Error),
    /// A stored entry has a number or time that does not parse.
    Data(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Kraken(e) => write!(f, "{e}"),
            SyncError::Db(e) => write!(f, "database error during Kraken sync: {e}"),
            SyncError::Data(e) => write!(f, "unexpected Kraken ledger data: {e}"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<KrakenPrivateError> for SyncError {
    fn from(e: KrakenPrivateError) -> Self {
        SyncError::Kraken(e)
    }
}

impl From<sqlx::Error> for SyncError {
    fn from(e: sqlx::Error) -> Self {
        SyncError::Db(e)
    }
}

/// A ledger group (refid) that is not mapped, with its entry types for the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub refid: String,
    /// "type/asset/sign" per entry, e.g. "withdrawal/XXBT/-".
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mapped {
    pub transactions: Vec<NewTransaction>,
    /// Groups recognised as internal moves (bundle dissolves), deliberately not recorded.
    pub ignored: Vec<String>,
    pub unsupported: Vec<Unsupported>,
}

struct Entry<'a> {
    raw: &'a RawLedgerEntry,
    time: NaiveDateTime,
    amount: Decimal,
    fee: Decimal,
}

fn parse_entry(e: &RawLedgerEntry) -> Result<Entry<'_>, SyncError> {
    let bad = |what: &str| SyncError::Data(format!("{what} of ledger entry {}", e.ledger_id));
    let time = Decimal::from_str(&e.time_unix).map_err(|_| bad("time"))?;
    let seconds = i64::try_from(time.trunc()).map_err(|_| bad("time"))?;
    Ok(Entry {
        raw: e,
        time: DateTime::from_timestamp(seconds, 0)
            .ok_or_else(|| bad("time"))?
            .naive_utc(),
        amount: Decimal::from_str(&e.amount).map_err(|_| bad("amount"))?,
        fee: Decimal::from_str(&e.fee).map_err(|_| bad("fee"))?,
    })
}

fn describe(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let sign = if e.amount.is_sign_negative() {
                "-"
            } else {
                "+"
            };
            format!("{}/{}/{sign}", e.raw.kind, e.raw.asset)
        })
        .collect()
}

fn fee_row(
    e: &Entry,
    currency: &str,
    asset: Option<&'static str>,
    what: &str,
) -> Option<NewTransaction> {
    (e.fee > Decimal::ZERO).then(|| NewTransaction {
        kind: "fee",
        executed_at: e.time,
        asset,
        quantity: None,
        unit_price: None,
        amount: e.fee,
        currency: currency.to_owned(),
        external_id: format!("kraken-{}-fee", e.raw.ledger_id),
        note: Some(format!("Kraken {what} fee")),
    })
}

/// Maps one refid group. Returns None when the group is not a known pattern.
fn map_group(entries: &[Entry]) -> Option<Result<Vec<NewTransaction>, ()>> {
    let kinds: Vec<&str> = entries.iter().map(|e| e.raw.kind.as_str()).collect();

    // Internal bundle -> spot move: nets to zero per asset. Ok(empty) = recognised, ignored.
    if kinds.iter().all(|k| *k == "bundletransfer") {
        let mut net: BTreeMap<&str, Decimal> = BTreeMap::new();
        for e in entries {
            *net.entry(e.raw.asset.as_str()).or_default() += e.amount;
        }
        return net.values().all(|v| v.is_zero()).then_some(Err(()));
    }

    match entries {
        [e] if e.raw.kind == "deposit" && e.amount > Decimal::ZERO => {
            let currency = fiat(&e.raw.asset)?;
            let mut rows = vec![NewTransaction {
                kind: "deposit",
                executed_at: e.time,
                asset: None,
                quantity: None,
                unit_price: None,
                amount: e.amount,
                currency: currency.to_owned(),
                external_id: format!("kraken-{}", e.raw.ledger_id),
                note: Some(format!("Kraken deposit ({})", e.raw.asset)),
            }];
            rows.extend(fee_row(e, currency, None, "deposit"));
            Some(Ok(rows))
        }
        [e] if e.raw.kind == "withdrawal" && e.amount < Decimal::ZERO => {
            let currency = fiat(&e.raw.asset)?;
            let mut rows = vec![NewTransaction {
                kind: "withdrawal",
                executed_at: e.time,
                asset: None,
                quantity: None,
                unit_price: None,
                amount: -e.amount,
                currency: currency.to_owned(),
                external_id: format!("kraken-{}", e.raw.ledger_id),
                note: Some(format!("Kraken withdrawal ({})", e.raw.asset)),
            }];
            rows.extend(fee_row(e, currency, None, "withdrawal"));
            Some(Ok(rows))
        }
        [e] if e.raw.kind == "staking" && e.amount > Decimal::ZERO => {
            // A reward paid in the asset itself: recorded as income in kind (`dividend` with the
            // asset as currency). Kraken's commission is charged as the fee, so the reward is
            // amount - fee.
            let asset = crypto(&e.raw.asset)?;
            let net = e.amount - e.fee;
            if net <= Decimal::ZERO {
                return None;
            }
            Some(Ok(vec![NewTransaction {
                kind: "dividend",
                executed_at: e.time,
                asset: Some(asset.symbol),
                quantity: Some(net),
                unit_price: None,
                amount: net,
                currency: asset.symbol.to_owned(),
                external_id: format!("kraken-{}", e.raw.ledger_id),
                note: Some(format!(
                    "Kraken staking reward (after {} {} commission)",
                    e.fee, asset.symbol
                )),
            }]))
        }
        _ if kinds.iter().all(|k| *k == "spend" || *k == "receive") => {
            // Instant Buy: each `receive` of crypto pairs with a `spend` of fiat, in time order.
            let spends: Vec<&Entry> = entries.iter().filter(|e| e.raw.kind == "spend").collect();
            let receives: Vec<&Entry> =
                entries.iter().filter(|e| e.raw.kind == "receive").collect();
            if spends.len() != receives.len() || spends.is_empty() {
                return None;
            }
            let mut rows = Vec::new();
            for (spend, receive) in spends.iter().zip(&receives) {
                let currency = fiat(&spend.raw.asset)?;
                let asset = crypto(&receive.raw.asset)?;
                let quantity = receive.amount - receive.fee;
                let paid = -spend.amount;
                if quantity <= Decimal::ZERO || paid <= Decimal::ZERO {
                    return None;
                }
                rows.push(NewTransaction {
                    kind: "buy",
                    executed_at: receive.time,
                    asset: Some(asset.symbol),
                    quantity: Some(quantity),
                    unit_price: Some((paid / quantity).round_dp(2)),
                    amount: paid,
                    currency: currency.to_owned(),
                    external_id: format!("kraken-{}", receive.raw.ledger_id),
                    note: Some(format!(
                        "Kraken Buy ({} for {})",
                        receive.raw.asset, spend.raw.asset
                    )),
                });
                rows.extend(fee_row(spend, currency, Some(asset.symbol), "purchase"));
                if receive.fee > Decimal::ZERO {
                    // Fee taken in crypto is already deducted from the quantity above.
                    rows.extend(fee_row(
                        receive,
                        asset.symbol,
                        Some(asset.symbol),
                        "purchase",
                    ));
                }
            }
            Some(Ok(rows))
        }
        _ => None,
    }
}

/// Maps stored ledger entries into transactions (pure; safe to run over everything each time,
/// since writing is idempotent).
pub fn map_entries(entries: &[RawLedgerEntry]) -> Result<Mapped, SyncError> {
    let mut groups: BTreeMap<&str, Vec<Entry>> = BTreeMap::new();
    for e in entries {
        groups
            .entry(e.refid.as_str())
            .or_default()
            .push(parse_entry(e)?);
    }
    let mut mapped = Mapped::default();
    for (refid, mut group) in groups {
        group.sort_by(|a, b| {
            a.time
                .cmp(&b.time)
                .then(a.raw.ledger_id.cmp(&b.raw.ledger_id))
        });
        match map_group(&group) {
            Some(Ok(rows)) => mapped.transactions.extend(rows),
            Some(Err(())) => mapped.ignored.push(refid.to_owned()),
            None => mapped.unsupported.push(Unsupported {
                refid: refid.to_owned(),
                entries: describe(&group),
            }),
        }
    }
    mapped.transactions.sort_by(|a, b| {
        a.executed_at
            .cmp(&b.executed_at)
            .then(a.external_id.cmp(&b.external_id))
    });
    Ok(mapped)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncSummary {
    /// Ledger entries received from Kraken in this run.
    pub fetched: usize,
    /// Of those, entries not stored before.
    pub new_entries: usize,
    /// Transactions written in this run.
    pub transactions_written: usize,
    pub ignored_groups: usize,
    pub unsupported: Vec<Unsupported>,
}

impl From<LedgerEntry> for RawLedgerEntry {
    fn from(e: LedgerEntry) -> Self {
        RawLedgerEntry {
            ledger_id: e.id,
            refid: e.refid,
            time_unix: e.time,
            kind: e.kind,
            subtype: e.subtype,
            aclass: e.aclass,
            asset: e.asset,
            amount: e.amount,
            fee: e.fee,
            balance: e.balance,
        }
    }
}

/// Fetches ledger entries newer than the newest stored one (all of them on the first run),
/// stores them, and maps every stored entry into transactions. `pause` is waited between
/// pages to stay under Kraken's rate limit.
///
/// Pages come newest first and the next run starts after the newest stored entry, so the
/// entries are stored only once every page has arrived: storing the first pages of a run that
/// then fails would make the next run skip the older entries it never got.
pub async fn sync(
    pool: &SqlitePool,
    client: &KrakenPrivateClient,
    pause: Duration,
) -> Result<SyncSummary, SyncError> {
    let start = raw::latest_entry_id(pool).await?;
    let mut summary = SyncSummary::default();
    let mut ofs = 0;
    let mut rows: Vec<RawLedgerEntry> = Vec::new();
    loop {
        if ofs > 0 && !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
        let page = client.ledgers(start.as_deref(), ofs).await?;
        if page.entries.is_empty() {
            break;
        }
        ofs += page.entries.len() as u64;
        summary.fetched += page.entries.len();
        rows.extend(page.entries.into_iter().map(RawLedgerEntry::from));
        if ofs >= page.count {
            break;
        }
    }
    summary.new_entries = raw::insert_entries(pool, &rows).await?;

    let mapped = map_entries(&raw::all_entries(pool).await?)?;
    let assets: Vec<Asset> = CRYPTO.iter().map(|(_, a)| *a).collect();
    summary.transactions_written =
        insert_transactions(pool, ACCOUNT, &assets, SOURCE, &mapped.transactions).await?;
    summary.ignored_groups = mapped.ignored.len();
    summary.unsupported = mapped.unsupported;
    Ok(summary)
}
