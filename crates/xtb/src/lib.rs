//! XTB xStation account-history export (xlsx): parsing and idempotent import.
//!
//! The XTB API has been switched off since 14.03.2025, so this export is the only source for the
//! IKZE account. Only the "Cash Operations" sheet is read: it lists every deposit and purchase
//! with its PLN amount. The open/closed positions sheets are snapshots and are not needed for
//! contributions.
//!
//! Observed in a real export (anonymised copy in `tests/fixtures`):
//! - a header block (account number, date range) above a header row starting with "Type";
//! - rows newest first, ending with a "Total" row that sums the Amount column;
//! - times are UTC with milliseconds; amounts are PLN with 2 decimals (negative for purchases);
//! - the "ID" and "Position ID" columns hold the account number, not a per-operation id, so
//!   operations are identified by a hash of their contents instead;
//! - purchase comments look like `OPEN BUY [TICKER ]<filled>[/<ordered>] @ <price>`, with the
//!   price in the instrument's currency (EUR for VWCE.DE); one order can be filled in parts.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::str::FromStr;

use calamine::{Data, DataType, Range, Reader, Xlsx};
use chrono::NaiveDateTime;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

pub mod import;

const SHEET: &str = "Cash Operations";

#[derive(Debug)]
pub enum XtbError {
    /// The file is not a readable xlsx workbook.
    Workbook(String),
    /// A sheet, header or column the parser relies on is missing.
    Layout(String),
    /// A row could not be understood.
    Row { row: usize, message: String },
    /// Operation types this parser does not handle yet. Importing them silently would make
    /// contributions wrong, so the whole file is rejected.
    UnsupportedTypes(Vec<String>),
    /// The rows do not add up to the export's own Total.
    TotalMismatch { rows: Decimal, total: Decimal },
}

impl std::fmt::Display for XtbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XtbError::Workbook(e) => write!(f, "cannot read the XTB export: {e}"),
            XtbError::Layout(e) => write!(f, "unexpected XTB export layout: {e}"),
            XtbError::Row { row, message } => write!(f, "XTB export row {row}: {message}"),
            XtbError::UnsupportedTypes(types) => write!(
                f,
                "XTB export has operation types that are not supported yet: {}",
                types.join(", ")
            ),
            XtbError::TotalMismatch { rows, total } => write!(
                f,
                "XTB export rows add up to {rows} but its Total row says {total}"
            ),
        }
    }
}

impl std::error::Error for XtbError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationKind {
    /// Cash paid into the account ("IKZE deposit", "IKE deposit", "deposit").
    Deposit,
    /// A purchase ("Stock purchase").
    Purchase {
        ticker: String,
        /// Units actually bought in this fill.
        filled: Decimal,
        /// Units of the whole order, when the comment shows a partial fill (`filled/ordered`).
        ordered: Option<Decimal>,
        /// Price per unit in the instrument's currency (EUR for VWCE.DE).
        price: Decimal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CashOperation {
    /// 1-based row number in the sheet, for messages.
    pub row: usize,
    pub kind: OperationKind,
    /// UTC, as exported (milliseconds included).
    pub time: NaiveDateTime,
    /// PLN, signed as exported: positive for deposits, negative for purchases.
    pub amount: Decimal,
    /// Account product, e.g. "IKZE".
    pub product: String,
    pub comment: String,
    /// Stable identity of the operation, derived from its contents (see module docs).
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// As shown in the export header; informational only.
    pub account_number: Option<String>,
    /// Oldest first.
    pub operations: Vec<CashOperation>,
    pub total: Decimal,
}

/// Parses an export read from any seekable source (a file, or bytes in a `Cursor`).
pub fn parse<R: Read + Seek>(reader: R) -> Result<Export, XtbError> {
    let mut workbook = Xlsx::new(reader).map_err(|e| XtbError::Workbook(e.to_string()))?;
    let range = workbook
        .worksheet_range(SHEET)
        .map_err(|e| XtbError::Layout(format!("no '{SHEET}' sheet ({e})")))?;
    parse_cash_operations(&range)
}

/// Parses the export at `path`.
pub fn parse_file(path: impl AsRef<std::path::Path>) -> Result<Export, XtbError> {
    let file = std::fs::File::open(path).map_err(|e| XtbError::Workbook(e.to_string()))?;
    parse(std::io::BufReader::new(file))
}

fn text(cell: &Data) -> Option<String> {
    match cell {
        Data::String(s) => Some(s.trim().to_owned()),
        Data::Float(f) => Some(format_float(*f)),
        Data::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

/// Floats print in their shortest round-trip form ("-6.44"), so this recovers the number as it
/// was written in the file without f64 arithmetic.
fn format_float(f: f64) -> String {
    let s = f.to_string();
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

fn decimal(cell: &Data) -> Option<Decimal> {
    match cell {
        Data::Float(f) if f.is_finite() => Decimal::from_str(&f.to_string()).ok(),
        Data::Int(i) => Some(Decimal::from(*i)),
        Data::String(s) => Decimal::from_str(s.trim()).ok(),
        _ => None,
    }
}

/// `OPEN BUY [TICKER ]<filled>[/<ordered>] @ <price>`.
fn parse_purchase_comment(
    comment: &str,
) -> Option<(Option<String>, Decimal, Option<Decimal>, Decimal)> {
    let rest = comment.strip_prefix("OPEN BUY ")?;
    let (left, price) = rest.rsplit_once(" @ ")?;
    let price = Decimal::from_str(price.trim()).ok()?;
    let mut parts: Vec<&str> = left.split_whitespace().collect();
    let volume = parts.pop()?;
    let ticker = match parts.as_slice() {
        [] => None,
        [t] => Some((*t).to_owned()),
        _ => return None,
    };
    let (filled, ordered) = match volume.split_once('/') {
        Some((f, o)) => (Decimal::from_str(f).ok()?, Some(Decimal::from_str(o).ok()?)),
        None => (Decimal::from_str(volume).ok()?, None),
    };
    (filled > Decimal::ZERO && price > Decimal::ZERO).then_some((ticker, filled, ordered, price))
}

fn hash_fields(fields: &[&str], occurrence: usize) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update(field.as_bytes());
        hasher.update([0x1f]);
    }
    hasher.update(occurrence.to_string().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn parse_cash_operations(range: &Range<Data>) -> Result<Export, XtbError> {
    let rows: Vec<&[Data]> = range.rows().collect();
    let account_number = rows
        .iter()
        .find(|r| text(&r[0]).as_deref() == Some("Account number"))
        .and_then(|r| r.get(1).and_then(text));

    // The header row is the first one naming the columns we need, wherever they are.
    let header_index = rows
        .iter()
        .position(|r| {
            let names: Vec<String> = r.iter().filter_map(text).collect();
            ["Type", "Time", "Comment"]
                .iter()
                .all(|needed| names.iter().any(|n| n == needed))
        })
        .ok_or_else(|| {
            XtbError::Layout("no header row with 'Type', 'Time' and 'Comment'".into())
        })?;
    let header: HashMap<String, usize> = rows[header_index]
        .iter()
        .enumerate()
        .filter_map(|(i, c)| text(c).map(|t| (t, i)))
        .collect();
    let column = |name: &str| {
        header
            .get(name)
            .copied()
            .ok_or_else(|| XtbError::Layout(format!("no '{name}' column")))
    };
    let (c_type, c_time, c_amount, c_comment) = (
        column("Type")?,
        column("Time")?,
        column("Amount")?,
        column("Comment")?,
    );
    let c_ticker = header.get("Ticker").copied();
    let c_product = header.get("Product").copied();
    let cell = |r: &[Data], c: usize| r.get(c).cloned().unwrap_or(Data::Empty);

    let mut operations = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();
    let mut total = None;
    let mut seen: HashMap<Vec<String>, usize> = HashMap::new();

    for (offset, r) in rows[header_index + 1..].iter().enumerate() {
        let row = header_index + offset + 2; // 1-based, as shown in Excel
        let Some(kind_text) = text(&cell(r, c_type)) else {
            continue; // blank line
        };
        let row_err = |message: String| XtbError::Row { row, message };
        let amount =
            decimal(&cell(r, c_amount)).ok_or_else(|| row_err("amount is not a number".into()))?;
        if kind_text == "Total" {
            total = Some(amount);
            break;
        }
        let time = cell(r, c_time)
            .as_datetime()
            .ok_or_else(|| row_err("time is not a date and time".into()))?;
        let comment = text(&cell(r, c_comment)).unwrap_or_default();
        let ticker_cell = c_ticker.and_then(|c| text(&cell(r, c)));
        let product = c_product
            .and_then(|c| text(&cell(r, c)))
            .unwrap_or_default();

        let kind = match kind_text.as_str() {
            t if t.to_ascii_lowercase().ends_with("deposit") => {
                if amount <= Decimal::ZERO {
                    return Err(row_err(format!("deposit amount {amount} is not positive")));
                }
                OperationKind::Deposit
            }
            "Stock purchase" => {
                let (comment_ticker, filled, ordered, price) = parse_purchase_comment(&comment)
                    .ok_or_else(|| row_err(format!("unexpected purchase comment {comment:?}")))?;
                if amount >= Decimal::ZERO {
                    return Err(row_err(format!("purchase amount {amount} is not negative")));
                }
                let ticker = ticker_cell
                    .clone()
                    .or(comment_ticker)
                    .ok_or_else(|| row_err("purchase without a ticker".into()))?;
                OperationKind::Purchase {
                    ticker,
                    filled,
                    ordered,
                    price,
                }
            }
            other => {
                if !unsupported.iter().any(|u| u == other) {
                    unsupported.push(other.to_owned());
                }
                continue;
            }
        };

        // Identity: the operation's contents, plus how many identical rows came before it in
        // this file (so two genuinely identical operations are not merged).
        let raw_time = match cell(r, c_time) {
            Data::DateTime(dt) => dt.as_f64().to_string(),
            other => other.to_string(),
        };
        let fields = vec![
            kind_text.clone(),
            raw_time,
            amount.to_string(),
            comment.clone(),
            ticker_cell.unwrap_or_default(),
            product.clone(),
        ];
        let occurrence = seen.entry(fields.clone()).or_insert(0);
        let field_refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        let hash = hash_fields(&field_refs, *occurrence);
        *occurrence += 1;

        operations.push(CashOperation {
            row,
            kind,
            time,
            amount,
            product,
            comment,
            hash,
        });
    }

    if !unsupported.is_empty() {
        return Err(XtbError::UnsupportedTypes(unsupported));
    }
    let total = total.ok_or_else(|| XtbError::Layout("no 'Total' row".into()))?;
    let rows_sum: Decimal = operations.iter().map(|o| o.amount).sum();
    if rows_sum != total {
        return Err(XtbError::TotalMismatch {
            rows: rows_sum,
            total,
        });
    }
    operations.sort_by(|a, b| a.time.cmp(&b.time).then(a.row.cmp(&b.row).reverse()));
    Ok(Export {
        account_number,
        operations,
        total,
    })
}
