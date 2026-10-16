//! Home page: total value, net contributions, P&L, XIRR, and a chart of value against net
//! contributions over time.
//!
//! A number that depends on a missing price is not shown: the value, P&L and XIRR say "not
//! available" and name what has no price, instead of showing an understated total (the same
//! rule as the P&L in `portfolio_core::pnl`). The chart is loaded by htmx after the page, since
//! the first time it looks up a price for every month since the first contribution.

use askama::Template;
use axum::extract::State;
use axum::response::Html;
use chrono::NaiveDate;
use portfolio_core::history::chart_dates;
use portfolio_core::xirr::{XirrError, contribution_flows};
use portfolio_db::transactions::load_ledger;
use portfolio_sources::history::value_series;
use portfolio_sources::valuation::{PortfolioReport, portfolio_on};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Serialize;

use crate::{AppError, AppState};

/// PLN rounded to grosze, with spaces between thousands: "-1 234 567.89".
pub fn pln(value: Decimal) -> String {
    let rounded = value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    let text = format!("{:.2}", rounded.abs());
    let (whole, cents) = text.split_once('.').expect("two decimal places");
    let mut grouped = String::new();
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(digit);
    }
    let sign = if rounded.is_sign_negative() && !rounded.is_zero() {
        "-"
    } else {
        ""
    };
    format!("{sign}{grouped}.{cents}")
}

/// Like [`pln`], with a sign for gains too: "+12.30", "-4.00".
pub fn signed_pln(value: Decimal) -> String {
    let text = pln(value);
    if text.starts_with('-') || text == "0.00" {
        text
    } else {
        format!("+{text}")
    }
}

/// A fraction as a signed percentage with two decimals: 0.12345 -> "+12.35%".
pub fn percent(fraction: Decimal) -> String {
    let p = (fraction * Decimal::ONE_HUNDRED)
        .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    if p.is_sign_negative() && !p.is_zero() {
        format!("{p:.2}%")
    } else {
        format!("+{:.2}%", p.abs())
    }
}

#[derive(Template)]
#[template(path = "home.html")]
pub struct HomePage {
    pub date: String,
    /// The report could not be made (a contribution's rate is missing).
    pub error: Option<String>,
    /// Nothing has been put in yet.
    pub empty: bool,
    /// None when some position or cash currency has no price.
    pub value: Option<String>,
    /// What has no price.
    pub missing: Vec<String>,
    pub contributions: String,
    pub pnl: Option<String>,
    pub pnl_percent: Option<String>,
    /// "+8.12% a year", or why there is none.
    pub xirr: String,
    pub xirr_since: Option<String>,
    pub problems: Vec<String>,
}

impl HomePage {
    fn failed(date: NaiveDate, error: String) -> Self {
        HomePage {
            date: date.to_string(),
            error: Some(error),
            empty: false,
            value: None,
            missing: Vec::new(),
            contributions: String::new(),
            pnl: None,
            pnl_percent: None,
            xirr: String::new(),
            xirr_since: None,
            problems: Vec::new(),
        }
    }

    pub fn from_report(r: &PortfolioReport) -> Self {
        let complete = r.pnl.complete;
        let xirr = match &r.xirr.portfolio {
            Ok(rate) => format!("{} a year", percent(*rate)),
            Err(XirrError::MissingValue) => "not available: some prices are missing".into(),
            Err(e) => format!("not available: {e}"),
        };
        HomePage {
            date: r.date.to_string(),
            error: None,
            empty: r.xirr.first_flow.is_none() && r.holdings.assets.is_empty(),
            value: complete.then(|| pln(r.valuation.total)),
            missing: r.valuation.missing.clone(),
            contributions: pln(r.contributions.total),
            pnl: r.pnl.pnl_if_complete().map(signed_pln),
            pnl_percent: r.pnl.pnl_ratio().map(percent),
            xirr,
            xirr_since: r.xirr.first_flow.map(|d| d.to_string()),
            problems: r.problems.clone(),
        }
    }
}

pub async fn show(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let today = (state.today)();
    let page = match portfolio_on(today, &state.sources()).await {
        Ok(r) => HomePage::from_report(&r),
        Err(e) => HomePage::failed(today, e),
    };
    Ok(Html(page.render()?))
}

/// The chart's data: one entry per day, amounts as decimal strings (null when unknown), so no
/// amount passes through binary floating point before the browser draws it.
#[derive(Debug, Serialize)]
pub struct ChartData {
    pub dates: Vec<String>,
    pub value: Vec<Option<String>>,
    pub contributions: Vec<Option<String>>,
}

#[derive(Template)]
#[template(path = "chart.html")]
pub struct ChartPanel {
    /// JSON of [`ChartData`]; None when there is nothing to draw.
    pub data: Option<String>,
    /// Days without a value (gaps in the line).
    pub gaps: usize,
    pub problems: Vec<String>,
}

/// The chart fragment: value and net contributions at each month end since the first
/// contribution, and today.
pub async fn chart(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let today = (state.today)();
    let ledger = load_ledger(&state.pool).await?;
    // Dates do not depend on rates, so any rate lists them all.
    let first = contribution_flows(&ledger, today, |_, _| Some(Decimal::ONE))
        .ok()
        .and_then(|flows| flows.iter().map(|f| f.date).min());
    let Some(first) = first else {
        let panel = ChartPanel {
            data: None,
            gaps: 0,
            problems: Vec::new(),
        };
        return Ok(Html(panel.render()?));
    };
    let series = value_series(&ledger, &chart_dates(first, today), &state.sources()).await;
    let money = |v: Option<Decimal>| v.map(|v| format!("{:.2}", v.round_dp(2)));
    let data = ChartData {
        dates: series.points.iter().map(|p| p.date.to_string()).collect(),
        value: series.points.iter().map(|p| money(p.value)).collect(),
        contributions: series
            .points
            .iter()
            .map(|p| money(p.contributions))
            .collect(),
    };
    let panel = ChartPanel {
        data: Some(serde_json::to_string(&data).expect("strings and nulls serialize")),
        gaps: series.points.iter().filter(|p| p.value.is_none()).count(),
        problems: series.problems,
    };
    Ok(Html(panel.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn amounts_are_grouped_and_rounded_to_grosze() {
        assert_eq!(pln(dec!(1234567.891)), "1 234 567.89");
        assert_eq!(pln(dec!(-1234.5)), "-1 234.50");
        assert_eq!(pln(dec!(999.995)), "1 000.00");
        assert_eq!(pln(dec!(123)), "123.00");
        assert_eq!(pln(dec!(-0.004)), "0.00");
        assert_eq!(signed_pln(dec!(12.3)), "+12.30");
        assert_eq!(signed_pln(dec!(-4)), "-4.00");
        assert_eq!(signed_pln(dec!(0.001)), "0.00");
    }

    #[test]
    fn percentages_are_signed() {
        assert_eq!(percent(dec!(0.12345)), "+12.35%");
        assert_eq!(percent(dec!(-0.083666)), "-8.37%");
        assert_eq!(percent(dec!(0)), "+0.00%");
        assert_eq!(percent(dec!(-0.00001)), "+0.00%");
    }
}
