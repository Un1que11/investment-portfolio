use std::io::Cursor;
use std::str::FromStr;

use chrono::NaiveDateTime;
use portfolio_db::open_in_memory;
use portfolio_xtb::import::{
    ImportError, ImportSummary, already_imported, deposits_total, external_id, import, validate,
};
use portfolio_xtb::{OperationKind, XtbError, parse, parse_file};
use rust_decimal::Decimal;
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
use sqlx::SqlitePool;

/// A real XTB IKZE export, anonymised (account number replaced, IDs remapped, quantities and
/// amounts scaled; prices and dates kept). Approved for use as a fixture.
const FIXTURE: &str = "tests/fixtures/xtb_ikze_anonymised.xlsx";

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn dt(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.3f").unwrap()
}

// ---- Synthetic exports in XTB's layout, for cases the real sample does not contain. ----

const HEADER: [&str; 10] = [
    "Type",
    "Instrument",
    "Ticker",
    "Category",
    "Time",
    "Amount",
    "ID",
    "Comment",
    "Product",
    "Position ID",
];

/// (type, time, amount, comment, ticker, product)
type Row<'a> = (&'a str, &'a str, f64, &'a str, &'a str, &'a str);

fn workbook(header: &[&str], rows: &[Row], total: Option<f64>) -> Vec<u8> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Cash Operations").unwrap();
    let date_format = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    ws.write_string(0, 0, "Account number").unwrap();
    ws.write_number(0, 1, 12345678.0).unwrap();
    ws.write_string(1, 0, "Cash Operations").unwrap();
    for (c, name) in header.iter().enumerate() {
        ws.write_string(4, c as u16, *name).unwrap();
    }
    let col = |name: &str| header.iter().position(|h| *h == name).map(|c| c as u16);
    let mut r = 5;
    for (kind, time, amount, comment, ticker, product) in rows {
        ws.write_string(r, col("Type").unwrap(), *kind).unwrap();
        if let Some(c) = col("Time") {
            let t = ExcelDateTime::parse_from_str(time).unwrap();
            ws.write_datetime_with_format(r, c, &t, &date_format)
                .unwrap();
        }
        if let Some(c) = col("Amount") {
            ws.write_number(r, c, *amount).unwrap();
        }
        ws.write_string(r, col("Comment").unwrap(), *comment)
            .unwrap();
        if let Some(c) = col("Ticker").filter(|_| !ticker.is_empty()) {
            ws.write_string(r, c, *ticker).unwrap();
        }
        if let Some(c) = col("Product") {
            ws.write_string(r, c, *product).unwrap();
        }
        r += 1;
    }
    if let Some(total) = total {
        ws.write_string(r, col("Type").unwrap(), "Total").unwrap();
        ws.write_number(r, col("Amount").unwrap_or(5), total)
            .unwrap();
    }
    wb.save_to_buffer().unwrap()
}

const DEPOSIT: Row = (
    "IKZE deposit",
    "2026-01-02 10:00:00",
    1000.0,
    "Transfer in operation on account with id 12345678",
    "",
    "IKZE",
);
const BUY: Row = (
    "Stock purchase",
    "2026-01-05 09:30:00",
    -609.0,
    "OPEN BUY 1/1.2345 @ 140.00",
    "VWCE.DE",
    "IKZE",
);

async fn transactions(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM transactions")
        .fetch_one(pool)
        .await
        .unwrap()
}

// ---- The real (anonymised) export. ----

#[test]
fn parses_real_export() {
    let export = parse_file(FIXTURE).unwrap();

    assert_eq!(export.account_number.as_deref(), Some("12345678"));
    assert_eq!(export.operations.len(), 26);
    let deposits = export
        .operations
        .iter()
        .filter(|o| o.kind == OperationKind::Deposit)
        .count();
    assert_eq!((deposits, export.operations.len() - deposits), (7, 19));
    // Deposits 6 x 308 + 327 = 2175; purchases 2171.86; the export's Total is 3.14.
    assert_eq!(deposits_total(&export), dec("2175"));
    assert_eq!(export.total, dec("3.14"));
    assert!(export.operations.iter().all(|o| o.product == "IKZE"));
}

#[test]
fn real_export_is_sorted_oldest_first_with_exact_values() {
    let export = parse_file(FIXTURE).unwrap();
    let first = &export.operations[0];
    let second = &export.operations[1];
    let last = export.operations.last().unwrap();

    assert_eq!(first.kind, OperationKind::Deposit);
    assert_eq!(
        (first.time, first.amount),
        (dt("2026-04-17 12:31:15.362"), dec("308"))
    );
    // A partial fill: 0.1375 of a 0.4640 order at 152.98 EUR, paid 89.34 PLN.
    assert_eq!(
        second.kind,
        OperationKind::Purchase {
            ticker: "VWCE.DE".into(),
            filled: dec("0.1375"),
            ordered: Some(dec("0.4640")),
            price: dec("152.98"),
        }
    );
    assert_eq!(second.amount, dec("-89.34"));
    // The newest row has the ticker inside the comment too.
    assert_eq!(last.comment, "OPEN BUY VWCE.DE 0.0085 @ 172.04");
    assert_eq!(last.amount, dec("-6.44"));
    assert!(export.operations.windows(2).all(|w| w[0].time <= w[1].time));
}

#[test]
fn hashes_are_unique_and_stable() {
    let a = parse_file(FIXTURE).unwrap();
    let b = parse_file(FIXTURE).unwrap();

    let hashes: std::collections::HashSet<_> = a.operations.iter().map(|o| &o.hash).collect();
    assert_eq!(hashes.len(), 26);
    assert_eq!(
        a.operations.iter().map(|o| &o.hash).collect::<Vec<_>>(),
        b.operations.iter().map(|o| &o.hash).collect::<Vec<_>>()
    );
    assert!(a.operations.iter().all(|o| o.hash.len() == 32));
}

#[tokio::test]
async fn importing_real_export_twice_is_idempotent() {
    let pool = open_in_memory().await.unwrap();
    let export = parse_file(FIXTURE).unwrap();

    let first = import(&pool, &export).await.unwrap();
    let second = import(&pool, &export).await.unwrap();

    assert_eq!(
        first,
        ImportSummary {
            inserted: 26,
            already_present: 0
        }
    );
    assert_eq!(
        second,
        ImportSummary {
            inserted: 0,
            already_present: 26
        }
    );
    assert_eq!(transactions(&pool).await, 26);
}

#[tokio::test]
async fn imported_rows_have_expected_shape() {
    let pool = open_in_memory().await.unwrap();
    let export = parse_file(FIXTURE).unwrap();
    import(&pool, &export).await.unwrap();

    let buy: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT a.name, a.kind, t.executed_at, t.quantity, t.unit_price, t.amount, s.symbol, t.note
         FROM transactions t JOIN accounts a ON a.id = t.account_id
         JOIN assets s ON s.id = t.asset_id
         WHERE t.external_id = ?",
    )
    .bind(external_id(&export.operations[1]))
    .fetch_one(&pool)
    .await
    .unwrap();
    // 89.34 PLN / 0.1375 = 649.74545... -> 649.7455 PLN per unit (152.98 EUR at ~4.2473).
    assert_eq!(
        buy,
        (
            "XTB IKZE".into(),
            "ikze".into(),
            "2026-04-17T14:06:11Z".into(),
            "0.1375".into(),
            "649.7455".into(),
            "89.34".into(),
            "VWCE".into(),
            "OPEN BUY 0.1375/0.4640 @ 152.98 (price in EUR)".into(),
        )
    );
    let (kind, amount, asset): (String, String, Option<i64>) =
        sqlx::query_as("SELECT kind, amount, asset_id FROM transactions WHERE external_id = ?")
            .bind(external_id(&export.operations[0]))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (kind.as_str(), amount.as_str(), asset),
        ("deposit", "308", None)
    );
}

// ---- Synthetic edge cases. ----

#[test]
fn whole_share_fill_and_column_order() {
    // The real export also has fills like "1/1.2345" (one whole unit of a fractional order).
    // Columns in a different order must work too.
    let header = [
        "Comment",
        "Amount",
        "Type",
        "Product",
        "Time",
        "Ticker",
        "ID",
        "Instrument",
        "Category",
        "Position ID",
    ];
    let bytes = workbook(&header, &[BUY, DEPOSIT], Some(391.0));

    let export = parse(Cursor::new(bytes)).unwrap();

    assert_eq!(export.operations[0].kind, OperationKind::Deposit);
    assert_eq!(
        export.operations[1].kind,
        OperationKind::Purchase {
            ticker: "VWCE.DE".into(),
            filled: dec("1"),
            ordered: Some(dec("1.2345")),
            price: dec("140.00"),
        }
    );
}

#[test]
fn unsupported_types_reject_the_file() {
    let rows = [
        DEPOSIT,
        (
            "Withholding tax",
            "2026-01-03 10:00:00",
            -1.0,
            "tax",
            "",
            "IKZE",
        ),
        (
            "Stock sale",
            "2026-01-04 10:00:00",
            50.0,
            "CLOSE BUY",
            "VWCE.DE",
            "IKZE",
        ),
        (
            "Withholding tax",
            "2026-01-05 10:00:00",
            -1.0,
            "tax",
            "",
            "IKZE",
        ),
    ];

    let err = parse(Cursor::new(workbook(&HEADER, &rows, Some(1048.0)))).unwrap_err();

    match err {
        XtbError::UnsupportedTypes(types) => {
            assert_eq!(types, ["Withholding tax", "Stock sale"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn total_must_match_rows() {
    let err = parse(Cursor::new(workbook(&HEADER, &[DEPOSIT, BUY], Some(400.0)))).unwrap_err();

    assert!(matches!(
        err,
        XtbError::TotalMismatch { rows, total } if rows == dec("391") && total == dec("400")
    ));
}

#[test]
fn layout_errors() {
    let no_total = parse(Cursor::new(workbook(&HEADER, &[DEPOSIT], None))).unwrap_err();
    assert!(matches!(no_total, XtbError::Layout(m) if m.contains("Total")));

    let without_amount: Vec<&str> = HEADER.iter().copied().filter(|h| *h != "Amount").collect();
    let no_amount = parse(Cursor::new(workbook(
        &without_amount,
        &[DEPOSIT],
        Some(1000.0),
    )));
    assert!(matches!(no_amount, Err(XtbError::Layout(m)) if m.contains("Amount")));

    let mut wb = Workbook::new();
    wb.add_worksheet().set_name("Something else").unwrap();
    let no_sheet = parse(Cursor::new(wb.save_to_buffer().unwrap())).unwrap_err();
    assert!(matches!(no_sheet, XtbError::Layout(m) if m.contains("Cash Operations")));

    let not_xlsx = parse(Cursor::new(b"not a spreadsheet".to_vec())).unwrap_err();
    assert!(matches!(not_xlsx, XtbError::Workbook(_)));
}

#[test]
fn invalid_rows_are_reported_with_their_row_number() {
    let negative_deposit = ("IKZE deposit", "2026-01-02 10:00:00", -5.0, "x", "", "IKZE");
    let positive_buy = (
        "Stock purchase",
        "2026-01-02 10:00:00",
        5.0,
        "OPEN BUY 1 @ 5",
        "VWCE.DE",
        "IKZE",
    );
    let odd_comment = (
        "Stock purchase",
        "2026-01-02 10:00:00",
        -5.0,
        "BUY SOMETHING",
        "VWCE.DE",
        "IKZE",
    );

    for (row, needle) in [
        (negative_deposit, "not positive"),
        (positive_buy, "not negative"),
        (odd_comment, "unexpected purchase comment"),
    ] {
        let err = parse(Cursor::new(workbook(&HEADER, &[row], Some(row.2)))).unwrap_err();
        assert!(
            matches!(&err, XtbError::Row { row: 6, message } if message.contains(needle)),
            "{err:?}"
        );
    }
}

#[tokio::test]
async fn identical_operations_stay_separate() {
    let pool = open_in_memory().await.unwrap();
    let small = (
        "Stock purchase",
        "2026-01-05 09:30:00",
        -14.0,
        "OPEN BUY 0.1 @ 32.00",
        "VWCE.DE",
        "IKZE",
    );
    let bytes = workbook(&HEADER, &[small, small, DEPOSIT], Some(972.0));

    let export = parse(Cursor::new(bytes)).unwrap();

    assert_ne!(export.operations[1].hash, export.operations[2].hash);
    assert_eq!(import(&pool, &export).await.unwrap().inserted, 3);
    assert_eq!(import(&pool, &export).await.unwrap().already_present, 3);
}

#[tokio::test]
async fn overlapping_export_only_adds_new_operations() {
    let pool = open_in_memory().await.unwrap();
    let older = parse(Cursor::new(workbook(&HEADER, &[BUY, DEPOSIT], Some(391.0)))).unwrap();
    let later_deposit = (
        "IKZE deposit",
        "2026-02-02 10:00:00",
        500.0,
        "Transfer",
        "",
        "IKZE",
    );
    let newer = parse(Cursor::new(workbook(
        &HEADER,
        &[later_deposit, BUY, DEPOSIT],
        Some(891.0),
    )))
    .unwrap();

    import(&pool, &older).await.unwrap();
    // Oldest first: the earlier deposit and buy are stored, the later deposit is not.
    assert_eq!(
        already_imported(&pool, &newer).await.unwrap(),
        [true, true, false]
    );
    let summary = import(&pool, &newer).await.unwrap();

    assert_eq!(
        summary,
        ImportSummary {
            inserted: 1,
            already_present: 2
        }
    );
    assert_eq!(transactions(&pool).await, 3);
}

#[tokio::test]
async fn unknown_instrument_or_product_writes_nothing() {
    let pool = open_in_memory().await.unwrap();
    let other_etf = (
        "Stock purchase",
        "2026-01-05 09:30:00",
        -10.0,
        "OPEN BUY 1 @ 2.5",
        "SXR8.DE",
        "IKZE",
    );
    let ike_unknown = ("IKZE deposit", "2026-01-02 10:00:00", 5.0, "x", "", "OKE");

    let instrument = parse(Cursor::new(workbook(
        &HEADER,
        &[DEPOSIT, other_etf],
        Some(990.0),
    )))
    .unwrap();
    let err = validate(&instrument).unwrap_err();
    assert!(matches!(err, ImportError::UnknownInstrument(t) if t == "SXR8.DE"));
    let err = import(&pool, &instrument).await.unwrap_err();
    assert!(matches!(err, ImportError::UnknownInstrument(t) if t == "SXR8.DE"));

    let product = parse(Cursor::new(workbook(&HEADER, &[ike_unknown], Some(5.0)))).unwrap();
    let err = validate(&product).unwrap_err();
    assert!(matches!(err, ImportError::UnknownProduct(p) if p == "OKE"));
    let err = import(&pool, &product).await.unwrap_err();
    assert!(matches!(err, ImportError::UnknownProduct(p) if p == "OKE"));

    // The deposit before the failing row was rolled back with it.
    assert_eq!(transactions(&pool).await, 0);
}

#[tokio::test]
async fn ike_product_gets_its_own_account() {
    let pool = open_in_memory().await.unwrap();
    let ike = (
        "IKE deposit",
        "2026-03-01 10:00:00",
        200.0,
        "Transfer",
        "",
        "IKE",
    );
    let export = parse(Cursor::new(workbook(
        &HEADER,
        &[ike, DEPOSIT],
        Some(1200.0),
    )))
    .unwrap();

    import(&pool, &export).await.unwrap();

    let accounts: Vec<(String, String)> =
        sqlx::query_as("SELECT name, kind FROM accounts ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        accounts,
        [
            ("XTB IKE".into(), "ike".into()),
            ("XTB IKZE".into(), "ikze".into())
        ]
    );
}
