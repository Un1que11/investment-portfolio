//! The Overview's period views ([`portfolio_core::period`]) for a report: 1M, 6M, YTD, 1Y and
//! All, each ending on the report's date.
//!
//! The end value and the dated contributions come from the report itself. The value at the end
//! of each start day comes from [`crate::history::value_series`], so it goes through the same
//! sources and caches as the chart. VWCE prices for the benchmarks come from the price cache:
//! the contributions' days are the report's own benchmark days, so the only new days are the
//! start days, and only those with something held (a start day before the first contribution
//! needs no price, and asking for one older than the daily closes would spend Alpha Vantage
//! requests on the weekly history for nothing).

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use portfolio_core::benchmark::price_days;
use portfolio_core::period::{Period, PeriodView, period_view};
use portfolio_core::portfolio::LedgerTx;
use rust_decimal::Decimal;

use crate::history::value_series;
use crate::valuation::{PortfolioReport, Sources, vwce_prices};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodReport {
    /// One view per period, in the order of [`Period::ALL`].
    pub views: Vec<PeriodView>,
    /// Why some start values or benchmark prices are missing, for the page.
    pub problems: Vec<String>,
}

impl PeriodReport {
    pub fn view(&self, period: Period) -> Option<&PeriodView> {
        self.views.iter().find(|v| v.period == period)
    }
}

/// The period views of `report`, whose `ledger` it was made from.
pub async fn period_views(
    ledger: &[LedgerTx],
    report: &PortfolioReport,
    s: &Sources<'_>,
) -> PeriodReport {
    let end = report.date;
    let mut problems = Vec::new();
    let starts: Vec<NaiveDate> = Period::ALL
        .iter()
        .filter_map(|p| p.start(end))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let series = value_series(ledger, &starts, s).await;
    problems.extend(series.problems.iter().map(|p| format!("periods, {p}")));
    let start_values: BTreeMap<NaiveDate, Option<Decimal>> =
        series.points.iter().map(|p| (p.date, p.value)).collect();

    let mut days = price_days(&report.contribution_flows, end);
    days.extend(
        start_values
            .iter()
            .filter(|(_, v)| **v != Some(Decimal::ZERO))
            .map(|(d, _)| *d),
    );
    let (vwce, problem) = vwce_prices(&days, s).await;
    if let Some(p) = problem {
        problems.push(format!("periods, VWCE benchmark: {p}"));
    }

    let end_value = report.pnl.complete.then_some(report.pnl.value);
    let views = Period::ALL
        .into_iter()
        .map(|p| {
            let start_value = p
                .start(end)
                .and_then(|d| start_values.get(&d).copied().flatten());
            period_view(
                p,
                end,
                &report.contribution_flows,
                start_value,
                end_value,
                |day| vwce.get(&day).copied().flatten(),
            )
        })
        .collect();
    PeriodReport { views, problems }
}
