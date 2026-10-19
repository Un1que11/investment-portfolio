//! The Import page (`design/screens/07`): Upload → Review → Done for an xStation account history.
//!
//! Nothing is kept between the steps on the server, and nothing is written to disk: the review
//! carries the uploaded file back in a hidden field, and "Add N operations" parses it again and
//! imports it ([`portfolio_xtb::import::import`], idempotent by each operation's contents). An
//! upload is limited to [`MAX_UPLOAD_BYTES`]. The export's account number is never shown: the
//! file name is shown with it (and any other long run of digits) masked.

use axum::Form;
use axum::extract::{Multipart, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Datelike;
use portfolio_core::ikze::ikze_flows;
use portfolio_db::transactions::load_ledger;
use portfolio_xtb::import::{already_imported, import, validate};
use portfolio_xtb::{Export, OperationKind};
use serde::Deserialize;

use askama::Template;

use crate::base::{Page, Shell};
use crate::format;
use crate::{AppError, AppState};

/// The largest export taken: 2 MB. A year of monthly IKZE top-ups is about 40 KB.
pub const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
/// Room for the form fields and multipart boundaries around the file.
const BODY_SLACK: usize = 64 * 1024;
/// What the server reads of an upload request, file and form around it.
pub const UPLOAD_BODY_LIMIT: usize = MAX_UPLOAD_BYTES + BODY_SLACK;
/// What it reads of "Add N operations": the file again, base64-encoded (4 bytes per 3).
pub const CONFIRM_BODY_LIMIT: usize = MAX_UPLOAD_BYTES.div_ceil(3) * 4 + BODY_SLACK;

const TOO_LARGE: &str = "This file is larger than 2 MB. An xStation account history is much smaller: export it again from xStation → Account history.";
const NO_FILE: &str = "Choose the .xlsx file exported from xStation → Account history.";
/// What stands in for the account number and other long digit runs in a file name.
const MASK: &str = "•••";

/// The file name as the page shows it: without a path, and with the export's account number and
/// any run of six or more digits masked ("account_12345678_pl.xlsx" -> "account_•••_pl.xlsx").
/// Dates such as 2026-10-16 are kept: their digit runs are shorter.
pub fn display_name(raw: &str, account: Option<&str>) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw).trim();
    let base = match account.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => base.replace(a, MASK),
        None => base.to_owned(),
    };
    let mut out = String::with_capacity(base.len());
    let mut digits = String::new();
    let flush = |digits: &mut String, out: &mut String| {
        out.push_str(if digits.len() >= 6 { MASK } else { digits });
        digits.clear();
    };
    for c in base.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            flush(&mut digits, &mut out);
            out.push(c);
        }
    }
    flush(&mut digits, &mut out);
    let out: String = out.chars().take(120).collect();
    if out.is_empty() {
        "account history.xlsx".into()
    } else {
        out
    }
}

/// A file size as the card shows it: whole KB rounded up ("38 KB", a 1-byte file is "1 KB"),
/// from 1 MB on one decimal ("1,5 MB").
pub fn size_text(bytes: usize) -> String {
    const KB: usize = 1024;
    if bytes < KB * KB {
        format!("{}{}KB", bytes.div_ceil(KB).max(1), format::NBSP)
    } else {
        // Tenths of a MB, rounded half up: 1 572 864 B = 15 tenths.
        let tenths = (bytes * 10 + KB * KB / 2) / (KB * KB);
        format!("{},{}{}MB", tenths / 10, tenths % 10, format::NBSP)
    }
}

fn operations(n: usize) -> String {
    format!("{n} operation{}", if n == 1 { "" } else { "s" })
}

/// The line under the file name: "XTB xStation · Cash operations sheet · 38 KB · 14 operations".
pub fn file_meta(bytes: usize, count: usize) -> String {
    format!(
        "XTB xStation · Cash operations sheet · {} · {}",
        size_text(bytes),
        operations(count)
    )
}

/// The confirm button: "Add 3 operations", "Add 1 operation"; nothing new: "Nothing new to add".
pub fn confirm_label(new: usize) -> String {
    if new == 0 {
        "Nothing new to add".into()
    } else {
        format!("Add {}", operations(new))
    }
}

/// The Done banner's title.
pub fn added_title(added: usize) -> String {
    if added == 0 {
        "Nothing was added: every operation in this file was already imported".into()
    } else {
        format!("{} added", operations(added))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    New,
    AlreadyImported,
    Added,
}

impl Status {
    pub fn text(self) -> &'static str {
        match self {
            Status::New => "New",
            Status::AlreadyImported => "Already imported",
            Status::Added => "Added",
        }
    }

    /// The chip class, or None for the plain grey "Already imported".
    pub fn chip(self) -> Option<&'static str> {
        match self {
            Status::New => Some("new"),
            Status::AlreadyImported => None,
            Status::Added => Some("added"),
        }
    }
}

/// A row of the preview table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// "03.10.2026" (the export's UTC date).
    pub date: String,
    /// "Deposit", "Stock purchase".
    pub kind: &'static str,
    /// "VWCE.DE", or "—".
    pub instrument: String,
    /// "1,5612" units, or "—".
    pub quantity: String,
    /// "+942,00 zł", "−941,40 zł".
    pub amount: String,
    pub status: Status,
}

/// The preview rows, newest first as in xStation. `imported[i]` says whether operation `i` was
/// already in the database before this import; after it (`done`), the others are "Added".
pub fn rows(export: &Export, imported: &[bool], done: bool) -> Vec<Row> {
    export
        .operations
        .iter()
        .zip(imported)
        .rev()
        .map(|(op, &was_there)| {
            let (kind, instrument, quantity) = match &op.kind {
                OperationKind::Deposit => ("Deposit", "—".to_owned(), "—".to_owned()),
                OperationKind::Purchase { ticker, filled, .. } => {
                    ("Stock purchase", ticker.clone(), format::number(*filled, 4))
                }
            };
            Row {
                date: format::date(op.time.date()),
                kind,
                instrument,
                quantity,
                amount: format::signed_pln(op.amount),
                status: match (was_there, done) {
                    (true, _) => Status::AlreadyImported,
                    (false, false) => Status::New,
                    (false, true) => Status::Added,
                },
            }
        })
        .collect()
}

/// The three steps in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Upload,
    Review,
    Done,
}

/// A step in the progress header: ticked, the current one, or not current (grey, numbered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepItem {
    pub number: u8,
    pub label: &'static str,
    pub ticked: bool,
    pub current: bool,
}

/// As in the reference: Upload is ticked once a file is in; Review and Done are numbered, and
/// the current one is dark.
pub fn steps(at: Step) -> Vec<StepItem> {
    [
        (1, "Upload", Step::Upload),
        (2, "Review", Step::Review),
        (3, "Done", Step::Done),
    ]
    .into_iter()
    .map(|(number, label, step)| StepItem {
        number,
        label,
        ticked: step == Step::Upload && at != Step::Upload,
        current: step == at,
    })
    .collect()
}

/// The uploaded file: its card, and what "Add N operations" sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCard {
    pub name: String,
    pub meta: String,
    /// The file, base64-encoded, for the confirm form (empty once it is imported).
    pub data: String,
}

/// The green banner after the import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneBanner {
    pub title: String,
    /// "IKZE contributions for 2026 are now 5 652,00 zł.", when the file has IKZE operations.
    pub ikze: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub file: FileCard,
    pub new: usize,
    pub already: usize,
    pub rows: Vec<Row>,
    pub done: Option<DoneBanner>,
}

impl Review {
    pub fn confirm_label(&self) -> String {
        confirm_label(self.new)
    }
}

#[derive(Template)]
#[template(path = "import.html")]
pub struct ImportPage {
    pub shell: Shell,
    pub steps: Vec<StepItem>,
    /// Why the upload was turned down (the Upload step only).
    pub error: Option<String>,
    pub review: Option<Review>,
    pub max_size: String,
}

async fn page(
    state: &AppState,
    headers: &HeaderMap,
    at: Step,
    error: Option<String>,
    review: Option<Review>,
) -> Result<ImportPage, AppError> {
    Ok(ImportPage {
        shell: Shell::load(state, headers, Page::Import).await?,
        steps: steps(at),
        error,
        review,
        max_size: size_text(MAX_UPLOAD_BYTES),
    })
}

/// The Upload step with an error, answered 422.
async fn turned_down(
    state: &AppState,
    headers: &HeaderMap,
    error: String,
) -> Result<Response, AppError> {
    let p = page(state, headers, Step::Upload, Some(error), None).await?;
    Ok((StatusCode::UNPROCESSABLE_ENTITY, Html(p.render()?)).into_response())
}

/// Parses an upload and checks that every operation can be imported.
fn read_export(bytes: &[u8]) -> Result<Export, String> {
    let export = portfolio_xtb::parse(std::io::Cursor::new(bytes))
        .map_err(|e| format!("This is not an xStation account history that can be read: {e}."))?;
    validate(&export).map_err(|e| format!("This file cannot be imported: {e}."))?;
    Ok(export)
}

pub async fn show(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    let p = page(&state, &headers, Step::Upload, None, None).await?;
    Ok(Html(p.render()?).into_response())
}

/// Reads the `file` field, at most [`MAX_UPLOAD_BYTES`] of it.
async fn read_upload(multipart: &mut Multipart) -> Result<(String, Vec<u8>), String> {
    let unreadable = |e: axum::extract::multipart::MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            TOO_LARGE.to_owned()
        } else {
            "The upload could not be read. Try choosing the file again.".to_owned()
        }
    };
    while let Some(mut field) = multipart.next_field().await.map_err(unreadable)? {
        if field.name() != Some("file") {
            continue;
        }
        let name = field.file_name().unwrap_or_default().to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(unreadable)? {
            if bytes.len() + chunk.len() > MAX_UPLOAD_BYTES {
                return Err(TOO_LARGE.into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err(NO_FILE.into());
        }
        return Ok((name, bytes));
    }
    Err(NO_FILE.into())
}

/// Upload → Review: the operations in the file, each new or already imported.
pub async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let (name, bytes) = match read_upload(&mut multipart).await {
        Ok(upload) => upload,
        Err(e) => return turned_down(&state, &headers, e).await,
    };
    let export = match read_export(&bytes) {
        Ok(export) => export,
        Err(e) => return turned_down(&state, &headers, e).await,
    };
    let imported = already_imported(&state.pool, &export).await?;
    let new = imported.iter().filter(|&&i| !i).count();
    let review = Review {
        file: FileCard {
            name: display_name(&name, export.account_number.as_deref()),
            meta: file_meta(bytes.len(), export.operations.len()),
            data: STANDARD.encode(&bytes),
        },
        new,
        already: imported.len() - new,
        rows: rows(&export, &imported, false),
        done: None,
    };
    let p = page(&state, &headers, Step::Review, None, Some(review)).await?;
    Ok(Html(p.render()?).into_response())
}

#[derive(Debug, Deserialize)]
pub struct ConfirmForm {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub data: String,
}

/// IKZE contributions this year, after the import, when the export has IKZE operations.
async fn ikze_line(state: &AppState, export: &Export) -> Result<Option<String>, AppError> {
    if !export.operations.iter().any(|o| o.product == "IKZE") {
        return Ok(None);
    }
    let ledger = load_ledger(&state.pool).await?;
    let today = (state.today)();
    // XTB's IKZE is in PLN; a deposit in another currency would need a rate, so no line then.
    Ok(ikze_flows(&ledger, today, |_, _| None).ok().map(|f| {
        format!(
            "IKZE contributions for {} are now {}.",
            today.year(),
            format::pln(f.contributed)
        )
    }))
}

/// Review → Done: imports the file sent back by the review, then shows what was added.
pub async fn confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ConfirmForm>,
) -> Result<Response, AppError> {
    let bytes = match STANDARD.decode(form.data.trim()) {
        Ok(b) if b.is_empty() => return turned_down(&state, &headers, NO_FILE.into()).await,
        Ok(b) if b.len() > MAX_UPLOAD_BYTES => {
            return turned_down(&state, &headers, TOO_LARGE.into()).await;
        }
        Ok(b) => b,
        Err(_) => {
            let e = "The reviewed file did not come back intact. Upload it again.";
            return turned_down(&state, &headers, e.into()).await;
        }
    };
    let export = match read_export(&bytes) {
        Ok(export) => export,
        Err(e) => return turned_down(&state, &headers, e).await,
    };
    let imported = already_imported(&state.pool, &export).await?;
    let summary = match import(&state.pool, &export).await {
        Ok(s) => s,
        Err(e) => {
            return turned_down(&state, &headers, format!("Nothing was imported: {e}.")).await;
        }
    };
    let review = Review {
        file: FileCard {
            name: display_name(&form.name, export.account_number.as_deref()),
            meta: file_meta(bytes.len(), export.operations.len()),
            data: String::new(),
        },
        new: summary.inserted,
        already: summary.already_present,
        rows: rows(&export, &imported, true),
        done: Some(DoneBanner {
            title: added_title(summary.inserted),
            ikze: ikze_line(&state, &export).await?,
        }),
    };
    let p = page(&state, &headers, Step::Done, None, Some(review)).await?;
    Ok(Html(p.render()?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use portfolio_xtb::CashOperation;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    fn plain(s: &str) -> String {
        s.replace(format::NBSP, " ").replace(format::MINUS, "-")
    }

    #[test]
    fn the_file_name_never_shows_the_account_number() {
        assert_eq!(
            display_name("account_history_IKZE_2026-10-16.xlsx", None),
            "account_history_IKZE_2026-10-16.xlsx"
        );
        // The export's own number, and any other run of 6+ digits.
        assert_eq!(
            display_name(
                "account_12345678_pl_xlsx_2005-12-31_2026-10-16.xlsx",
                Some("12345678")
            ),
            "account_•••_pl_xlsx_2005-12-31_2026-10-16.xlsx"
        );
        assert_eq!(display_name("PL123456 IKZE.xlsx", None), "PL••• IKZE.xlsx");
        assert_eq!(display_name("12345.xlsx", None), "12345.xlsx");
        // The account number even when it is shorter than six digits.
        assert_eq!(display_name("ab4321.xlsx", Some("4321")), "ab•••.xlsx");
        // Paths are dropped; nothing left gets a default.
        assert_eq!(display_name("C:\\Users\\m\\IKZE.xlsx", None), "IKZE.xlsx");
        assert_eq!(display_name("/tmp/IKZE.xlsx", None), "IKZE.xlsx");
        assert_eq!(display_name("", None), "account history.xlsx");
    }

    #[test]
    fn sizes_are_whole_kb_then_tenths_of_a_mb() {
        // 38 KB = 38 912 B; one byte more rounds up to 39.
        assert_eq!(plain(&size_text(38 * 1024)), "38 KB");
        assert_eq!(plain(&size_text(38 * 1024 + 1)), "39 KB");
        assert_eq!(plain(&size_text(1)), "1 KB");
        assert_eq!(plain(&size_text(0)), "1 KB");
        // 1 MB - 1 B is still 1024 KB.
        assert_eq!(plain(&size_text(1024 * 1024 - 1)), "1024 KB");
        // 1,5 MB exactly; 2 MB; 1,04 MB (1 090 520 B) rounds to 1,0.
        assert_eq!(plain(&size_text(1_572_864)), "1,5 MB");
        assert_eq!(plain(&size_text(MAX_UPLOAD_BYTES)), "2,0 MB");
        assert_eq!(plain(&size_text(1_090_520)), "1,0 MB");
        assert_eq!(
            plain(&file_meta(38 * 1024, 14)),
            "XTB xStation · Cash operations sheet · 38 KB · 14 operations"
        );
        assert_eq!(
            plain(&file_meta(500, 1)),
            "XTB xStation · Cash operations sheet · 1 KB · 1 operation"
        );
    }

    #[test]
    fn labels_count_operations() {
        assert_eq!(confirm_label(3), "Add 3 operations");
        assert_eq!(confirm_label(1), "Add 1 operation");
        assert_eq!(confirm_label(0), "Nothing new to add");
        assert_eq!(added_title(3), "3 operations added");
        assert_eq!(added_title(1), "1 operation added");
        assert!(added_title(0).starts_with("Nothing was added"));
    }

    #[test]
    fn the_steps_follow_the_reference() {
        let at = |s| {
            steps(s)
                .into_iter()
                .map(|i| (i.ticked, i.current))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            at(Step::Upload),
            [(false, true), (false, false), (false, false)]
        );
        assert_eq!(
            at(Step::Review),
            [(true, false), (false, true), (false, false)]
        );
        assert_eq!(
            at(Step::Done),
            [(true, false), (false, false), (false, true)]
        );
    }

    fn op(time: &str, kind: OperationKind, amount: Decimal) -> CashOperation {
        CashOperation {
            row: 0,
            kind,
            time: NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M:%S").unwrap(),
            amount,
            product: "IKZE".into(),
            comment: String::new(),
            hash: time.into(),
        }
    }

    #[test]
    fn rows_are_newest_first_with_their_status() {
        let export = Export {
            account_number: None,
            operations: vec![
                op("2026-10-02 09:00:00", OperationKind::Deposit, dec!(942)),
                op(
                    "2026-10-03 10:15:00",
                    OperationKind::Purchase {
                        ticker: "VWCE.DE".into(),
                        filled: dec!(1.5612),
                        ordered: None,
                        price: dec!(140.12),
                    },
                    dec!(-941.40),
                ),
            ],
            total: dec!(0.60),
        };
        let review = rows(&export, &[true, false], false);
        let plain_rows: Vec<_> = review
            .iter()
            .map(|r| {
                (
                    r.date.as_str(),
                    r.kind,
                    r.instrument.as_str(),
                    r.quantity.clone(),
                    plain(&r.amount),
                    r.status,
                )
            })
            .collect();
        assert_eq!(
            plain_rows,
            [
                (
                    "03.10.2026",
                    "Stock purchase",
                    "VWCE.DE",
                    "1,5612".to_owned(),
                    "-941,40 zł".to_owned(),
                    Status::New
                ),
                (
                    "02.10.2026",
                    "Deposit",
                    "—",
                    "—".to_owned(),
                    "+942,00 zł".to_owned(),
                    Status::AlreadyImported
                ),
            ]
        );
        // After the import the new one is "Added"; the other stays as it was.
        let done: Vec<_> = rows(&export, &[true, false], true)
            .into_iter()
            .map(|r| r.status)
            .collect();
        assert_eq!(done, [Status::Added, Status::AlreadyImported]);
        assert_eq!(
            (
                Status::New.chip(),
                Status::Added.chip(),
                Status::AlreadyImported.chip()
            ),
            (Some("new"), Some("added"), None)
        );
    }

    #[test]
    fn the_confirm_body_fits_the_largest_upload() {
        // 2 097 152 B in base64 is 2 796 204 characters (4 per 3 bytes, the last group padded).
        assert_eq!(MAX_UPLOAD_BYTES.div_ceil(3) * 4, 2_796_204);
        assert!(CONFIRM_BODY_LIMIT > STANDARD.encode(vec![0u8; MAX_UPLOAD_BYTES]).len());
    }
}
