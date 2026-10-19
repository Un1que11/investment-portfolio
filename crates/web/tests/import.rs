//! The Import page (screens/07) through the router: an in-memory database, the anonymised XTB
//! fixture, and nothing listening for any source.
//!
//! The fixture holds 26 IKZE operations from 17.04.2026 to 08.10.2026: seven deposits (six of
//! 308 zł, then 327 zł on 08.10) and 19 VWCE purchases.

use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use portfolio_db::open_in_memory;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_web::import::MAX_UPLOAD_BYTES;
use portfolio_web::{AppState, Market, app};
use portfolio_xtb::Export;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tower::ServiceExt;

const FIXTURE: &[u8] = include_bytes!("../../xtb/tests/fixtures/xtb_ikze_anonymised.xlsx");
/// The fixture's (made-up) account number.
const ACCOUNT: &str = "12345678";
const TODAY: &str = "2026-10-15";
const DOWN: &str = "http://127.0.0.1:9";
const BOUNDARY: &str = "----portfel-test-boundary";

fn router(pool: SqlitePool) -> Router {
    let market = Market {
        nbp: NbpClient::with_base_url(DOWN).unwrap(),
        kraken: KrakenClient::with_base_url(DOWN).unwrap(),
        alphavantage: None,
        gold_buyback_factor: Decimal::from_str("0.95").unwrap(),
    };
    let state = AppState {
        pool,
        market: Arc::new(market),
        today: Arc::new(|| NaiveDate::from_str(TODAY).unwrap()),
        now: Arc::new(Utc::now),
    };
    app(state)
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace('\u{a0}', " ")
        .replace('\u{2212}', "-");
    (status, body)
}

/// A multipart upload of `bytes` as the `file` field.
fn upload(name: &str, bytes: &[u8]) -> Request<Body> {
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    Request::post("/import")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap()
}

/// The page's own confirm form, sent back: its hidden `name` and `data`.
fn confirm_from(review: &str) -> Request<Body> {
    let hidden = |field: &str| {
        let start = format!(r#"<input type="hidden" name="{field}" value=""#);
        let from = review.find(&start).expect(field) + start.len();
        let len = review[from..].find('"').unwrap();
        review[from..from + len]
            .replace("&#43;", "+")
            .replace("&#61;", "=")
            .replace("&#47;", "/")
    };
    let form = format!(
        "name={}&data={}",
        encode(&hidden("name")),
        encode(&hidden("data"))
    );
    Request::post("/import/confirm")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form))
        .unwrap()
}

/// Percent-encodes everything but unreserved characters.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn transactions(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM transactions")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn fixture() -> Export {
    portfolio_xtb::parse(std::io::Cursor::new(FIXTURE)).unwrap()
}

/// The first `n` operations of the fixture (oldest first), imported beforehand.
async fn imported_before(pool: &SqlitePool, n: usize) {
    let mut older = fixture();
    older.operations.truncate(n);
    portfolio_xtb::import::import(pool, &older).await.unwrap();
}

#[tokio::test]
async fn the_upload_step_is_the_drop_zone() {
    let pool = open_in_memory().await.unwrap();
    let (status, page) = send(
        &router(pool),
        Request::get("/import").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for text in [
        "<title>Import · Portfel</title>",
        r#"<a href="/import" aria-current="page""#,
        "Drop your xStation account history",
        "up to 2,0 MB",
        r#"enctype="multipart/form-data""#,
        r#"<li aria-current="step""#,
    ] {
        assert!(page.contains(text), "{text}\n{page}");
    }
    assert!(!page.contains("Operations to add"), "{page}");
}

#[tokio::test]
async fn the_review_tells_new_from_already_imported_and_writes_nothing() {
    let pool = open_in_memory().await.unwrap();
    // Up to the 08.09 deposit: 20 operations. The two 08.09 purchases and the four of 08.10 are
    // new.
    imported_before(&pool, 20).await;
    let router = router(pool.clone());

    let (status, page) = send(
        &router,
        upload(
            &format!("account_{ACCOUNT}_pl_xlsx_2026-10-16.xlsx"),
            FIXTURE,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for text in [
        // The account number is masked in the file name, and shown nowhere.
        "account_•••_pl_xlsx_2026-10-16.xlsx",
        "XTB xStation · Cash operations sheet · ",
        " · 26 operations",
        r#"<span style="color: var(--text); font-weight: 500">6 new</span> · 20 already imported"#,
        "Add 6 operations",
        // Newest first: the last purchase of 08.10, 0,0085 units for 6,44 zł.
        "<td style=\"padding: 13px 12px 13px 24px; white-space: nowrap\">08.10.2026</td>\n<td style=\"padding: 13px 12px\">Stock purchase</td>\n<td style=\"padding: 13px 12px\">VWCE.DE</td>\n<td style=\"padding: 13px 12px; text-align: right\">0,0085</td>\n<td style=\"padding: 13px 12px; text-align: right; white-space: nowrap\">-6,44 zł</td>",
        "+327,00 zł",
        r#"<span class="chip chip-new">New</span>"#,
        r#"<tr class="dup""#,
        r#"<input id="show-dup" type="checkbox" checked"#,
    ] {
        assert!(page.contains(text), "{text}\n{page}");
    }
    assert_eq!(page.matches("chip-new\">New<").count(), 6);
    assert_eq!(page.matches("<tr class=\"dup\"").count(), 20);
    assert!(!page.contains(ACCOUNT), "the account number is on the page");
    // Reviewing writes nothing.
    assert_eq!(transactions(&pool).await, 20);
}

#[tokio::test]
async fn adding_imports_the_new_operations_once() {
    let pool = open_in_memory().await.unwrap();
    imported_before(&pool, 20).await;
    let router = router(pool.clone());
    let (_, review) = send(&router, upload("history.xlsx", FIXTURE)).await;

    let (status, done) = send(&router, confirm_from(&review)).await;
    assert_eq!(status, StatusCode::OK);
    // IKZE in 2026: six deposits of 308 and one of 327 = 1848 + 327 = 2175.
    for text in [
        "6 operations added",
        "IKZE contributions for 2026 are now 2 175,00 zł.",
        "Back to overview",
        r#"<span style="color: var(--text); font-weight: 500">6 added</span> · 20 already imported"#,
        ">Added</button>",
        r#"<li aria-current="step" style="display: flex; align-items: center; gap: 8px; color: var(--text); font-weight: 500"><span style="width: 20px; height: 20px; border-radius: 50%; display: flex; align-items: center; justify-content: center; font-size: 11px; background: var(--text); color: var(--bg)">3</span>Done</li>"#,
    ] {
        assert!(done.contains(text), "{text}\n{done}");
    }
    assert_eq!(done.matches("chip-added\">Added<").count(), 6);
    assert!(!done.contains("name=\"data\""), "{done}");
    assert_eq!(transactions(&pool).await, 26);

    // Sending the same review again (a reload, a second click) adds nothing.
    let (status, again) = send(&router, confirm_from(&review)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        again.contains("Nothing was added: every operation in this file was already imported"),
        "{again}"
    );
    assert_eq!(transactions(&pool).await, 26);

    // And uploading the file again finds nothing new.
    let (_, review) = send(&router, upload("history.xlsx", FIXTURE)).await;
    for text in [
        "0 new</span> · 26 already imported",
        "Nothing new to add</button>",
    ] {
        assert!(review.contains(text), "{text}\n{review}");
    }
    assert!(review.contains(r#"type="submit" disabled"#), "{review}");
}

#[tokio::test]
async fn a_bad_upload_is_turned_down_on_the_upload_step() {
    let pool = open_in_memory().await.unwrap();
    let router = router(pool.clone());
    let too_large = vec![0u8; MAX_UPLOAD_BYTES + 1];
    // Past the request limit itself, too: the upload stops being read there.
    let far_too_large = vec![0u8; 3 * MAX_UPLOAD_BYTES];
    for (name, bytes, error) in [
        ("big.xlsx", &too_large[..], "This file is larger than 2 MB."),
        (
            "huge.xlsx",
            &far_too_large[..],
            "This file is larger than 2 MB.",
        ),
        (
            "notes.xlsx",
            &b"not a workbook"[..],
            "This is not an xStation account history that can be read",
        ),
        (
            "empty.xlsx",
            &b""[..],
            "Choose the .xlsx file exported from xStation",
        ),
    ] {
        let (status, page) = send(&router, upload(name, bytes)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name}");
        assert!(page.contains(error), "{name}: {error}\n{page}");
        assert!(page.contains(r#"<span id="import-msg" class="field-error" role="alert">"#));
        assert!(
            page.contains("Drop your xStation account history"),
            "{page}"
        );
    }

    // A confirm whose file did not survive the round trip.
    let form = Request::post("/import/confirm")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("name=x.xlsx&data=%%%"))
        .unwrap();
    let (status, page) = send(&router, form).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains("did not come back intact"), "{page}");
    assert_eq!(transactions(&pool).await, 0);
}
