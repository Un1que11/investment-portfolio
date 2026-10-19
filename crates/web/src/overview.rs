//! The Overview page (`design/screens/01–03`): the hero total with the period's P&L, the XIRR
//! and Contributed tiles, and the value chart with the stepped contributions and the dashed
//! "all-in VWCE" benchmark. The 1M / 6M / YTD / 1Y / All switch and the benchmark checkbox
//! re-render the panel through htmx; each state also has its own URL (`/?period=6M&bench=off`),
//! so they work as plain links and survive a reload. The asset cards under the panel
//! ([`crate::assets`]) cover all time, so the switch leaves them alone and only full page loads
//! build them.

use std::collections::BTreeMap;

use askama::Template;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use chrono::{Datelike, NaiveDate};
use portfolio_core::history::chart_dates;
use portfolio_core::period::{Period, PeriodError, PeriodView};
use portfolio_db::gold::list_coins;
use portfolio_db::transactions::load_ledger;
use portfolio_sources::history::{PeriodChart, period_chart, value_series};
use portfolio_sources::periods::period_views;
use portfolio_sources::prices::LastKnown;
use portfolio_sources::valuation::{PortfolioReport, fetch_inputs, report, requirements};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::assets::asset_cards;
use crate::base::{AssetCard, Page, Shell, local};
use crate::chart::{self, Chart};
use crate::format::{self, Delta};
use crate::{AppError, AppState};

#[derive(Debug, Default, Deserialize)]
pub struct OverviewQuery {
    pub period: Option<String>,
    /// "off" hides the benchmark; anything else shows it.
    pub bench: Option<String>,
}

/// The period named by the switch's label; All when absent or unknown.
pub fn parse_period(label: Option<&str>) -> Period {
    Period::ALL
        .into_iter()
        .find(|p| Some(p.label()) == label)
        .unwrap_or(Period::All)
}

/// The URL of a state of the page. All with the benchmark shown is the plain `/`.
pub fn href(period: Period, bench: bool) -> String {
    match (period, bench) {
        (Period::All, true) => "/".into(),
        (Period::All, false) => "/?bench=off".into(),
        (p, true) => format!("/?period={}", p.label()),
        (p, false) => format!("/?period={}&bench=off", p.label()),
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// What the period covers, under the hero: "past month", "since 1 Jan", "since May 2025" (All,
/// from the first contribution).
pub fn caption(period: Period, since: Option<NaiveDate>) -> String {
    match period {
        Period::OneMonth => "past month".into(),
        Period::SixMonths => "past 6 months".into(),
        Period::YearToDate => "since 1 Jan".into(),
        Period::OneYear => "past 12 months".into(),
        Period::All => match since {
            Some(d) => format!("since {} {}", MONTHS[d.month0() as usize], d.year()),
            None => "all time".into(),
        },
    }
}

pub struct PeriodLink {
    pub label: &'static str,
    pub href: String,
    pub current: bool,
}

/// The XIRR tile: figure, its colour token and the note under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XirrTile {
    pub figure: String,
    pub color: &'static str,
    pub note: String,
}

pub fn xirr_tile(xirr: &Result<Decimal, PeriodError>, caption: &str) -> XirrTile {
    match xirr {
        Ok(x) => {
            let d = format::delta_percent(*x);
            XirrTile {
                figure: d.text,
                color: d.tone.color(),
                note: format!("per year, {caption}"),
            }
        }
        Err(PeriodError::TooShort) => XirrTile {
            figure: "—".into(),
            color: "var(--text-3)",
            note: "Needs 3+ months of data".into(),
        },
        Err(e) => XirrTile {
            figure: "—".into(),
            color: "var(--text-3)",
            note: format!("Not available: {e}"),
        },
    }
}

/// The note next to the chart's legend: how far the portfolio is ahead of all-in VWCE.
pub fn benchmark_note(view: &PeriodView, shown: bool) -> String {
    if !shown {
        return String::new();
    }
    match view.benchmark_gap() {
        Some(gap) if gap >= Decimal::ZERO => {
            format!("You’re {} ahead of all-in VWCE", format::signed_pln(gap))
        }
        Some(gap) => format!("All-in VWCE would be {} ahead", format::signed_pln(-gap)),
        None => "All-in VWCE: no price for some days".into(),
    }
}

#[derive(Template)]
#[template(path = "overview_panel.html")]
pub struct OverviewPanel {
    pub periods: Vec<PeriodLink>,
    pub bench: bool,
    /// Where the benchmark checkbox goes: the same period with the benchmark flipped.
    pub bench_toggle: String,
    /// "11 500,00".
    pub total: String,
    pub pnl: Option<Delta>,
    pub pnl_percent: Option<Delta>,
    pub caption: String,
    /// Shown when some position has no price.
    pub incomplete: bool,
    pub xirr: XirrTile,
    pub contributed: String,
    pub contributed_note: String,
    pub bench_note: String,
    pub chart: Option<Chart>,
    /// The chart's "not enough history" state instead of a chart.
    pub one_point: Option<&'static str>,
    pub chart_label: String,
}

/// The chart's "not enough history" state (`screens/08`): exactly one of its days has a value,
/// so there is no line to draw yet.
pub fn one_point_note(points: &[chart::Point]) -> Option<&'static str> {
    if points.iter().filter(|p| p.value.is_some()).count() != 1 {
        return None;
    }
    Some(if points.len() == 1 {
        // The first contribution is today: tomorrow adds the second day.
        "One data point so far. The chart fills in from tomorrow."
    } else {
        "One data point so far. The chart fills in once more days have prices."
    })
}

impl OverviewPanel {
    pub fn new(
        report: &PortfolioReport,
        view: &PeriodView,
        series: &PeriodChart,
        bench: bool,
    ) -> OverviewPanel {
        let period = view.period;
        let all_since = report.contribution_flows.iter().map(|f| f.date).min();
        let caption = caption(period, all_since);
        let points: Vec<chart::Point> = series
            .points
            .iter()
            .map(|p| chart::Point {
                date: p.date,
                value: p.value,
                contributions: p.contributions,
                benchmark: p.benchmark,
            })
            .collect();
        let one_point = one_point_note(&points);
        OverviewPanel {
            periods: Period::ALL
                .into_iter()
                .map(|p| PeriodLink {
                    label: p.label(),
                    href: href(p, bench),
                    current: p == period,
                })
                .collect(),
            bench,
            bench_toggle: href(period, !bench),
            total: format::number(report.valuation.total, 2),
            pnl: view.pnl().map(format::delta_pln),
            pnl_percent: view.pnl_ratio().map(format::delta_percent),
            incomplete: !report.pnl.complete,
            xirr: xirr_tile(&view.xirr, &caption),
            contributed: format::pln(view.contributions),
            contributed_note: format!("net, {caption}"),
            bench_note: benchmark_note(view, bench),
            chart: match one_point {
                Some(_) => None,
                None => chart::chart(&points, bench, period == Period::All),
            },
            one_point,
            chart_label: format!(
                "Portfolio value {} against {} contributed, {caption}",
                format::pln(report.valuation.total),
                format::pln(report.contributions.total)
            ),
            caption,
        }
    }
}

#[derive(Template)]
#[template(path = "overview.html")]
pub struct OverviewPage {
    pub shell: Shell,
    /// The rendered [`OverviewPanel`] or [`OverviewError`].
    pub panel: String,
    /// Ranked by profit; empty when the portfolio cannot be valued.
    pub assets: Vec<AssetCard>,
}

/// The panel's place when the portfolio cannot be valued.
#[derive(Template)]
#[template(
    source = r#"<div id="overview" style="display: flex; flex-direction: column; gap: 16px">
<h1 style="margin: 0; font-size: 15px; font-weight: 500; color: var(--text-2); letter-spacing: 0">Overview</h1>
<p role="alert" style="margin: 0; color: var(--text-2)">{{ error }}</p>
</div>"#,
    ext = "html"
)]
pub struct OverviewError {
    pub error: String,
}

/// The panel's place before anything is imported or added (`screens/08`, "Overview · no data
/// yet").
#[derive(Template)]
#[template(path = "overview_empty.html")]
pub struct OverviewEmpty;

/// What takes the panel's place.
enum Panel {
    Ready(Box<OverviewPanel>),
    /// Nothing in the ledger yet.
    Empty,
    /// The portfolio cannot be valued (a contribution's rate is missing).
    Error(String),
}

impl Panel {
    fn render(&self) -> askama::Result<String> {
        match self {
            Panel::Ready(p) => p.render(),
            Panel::Empty => OverviewEmpty.render(),
            Panel::Error(error) => OverviewError {
                error: error.clone(),
            }
            .render(),
        }
    }
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

struct Built {
    panel: Panel,
    assets: Vec<AssetCard>,
    /// The assets valued at a stored close while their source is down, and whether every
    /// asset has a price: what the shell's banner and sync status say. None without a report.
    prices: Option<(BTreeMap<String, LastKnown>, bool)>,
}

/// The panel for today and, when `with_assets`, the asset cards.
async fn build(
    state: &AppState,
    period: Period,
    bench: bool,
    with_assets: bool,
) -> Result<Built, AppError> {
    let s = state.sources();
    let today = s.clock.today;
    let ledger = load_ledger(&state.pool).await?;
    if ledger.is_empty() {
        return Ok(Built {
            panel: Panel::Empty,
            assets: Vec::new(),
            prices: None,
        });
    }
    let inputs = fetch_inputs(&requirements(&ledger, today), today, &s).await;
    let report = match report(&ledger, today, &inputs) {
        Ok(r) => r,
        Err(e) => {
            for p in &inputs.problems {
                eprintln!("overview: {p}");
            }
            return Ok(Built {
                panel: Panel::Error(format!("The portfolio cannot be valued: {e}")),
                assets: Vec::new(),
                prices: None,
            });
        }
    };
    let periods = period_views(&ledger, &report, &s).await;
    let series = period_chart(&ledger, &report, period, &s).await;
    let mut problems: Vec<String> = Vec::new();
    let assets = if with_assets {
        // The sparklines span all time: the All chart's days, already valued when the panel
        // shows All, and served from the price caches otherwise.
        let all_time = if period == Period::All {
            series.assets.clone()
        } else {
            match report.contribution_flows.iter().map(|f| f.date).min() {
                Some(first) => {
                    let v = value_series(&ledger, &chart_dates(first, today), &s).await;
                    problems.extend(v.problems);
                    v.assets
                }
                None => Default::default(),
            }
        };
        let coins = list_coins(&state.pool).await?;
        let last_price = report
            .last_known
            .iter()
            .map(|(asset, l)| (asset.clone(), l.fetched_at.map(local)))
            .collect();
        asset_cards(
            &report.pnl,
            &report.attribution,
            &ledger,
            &coins,
            &all_time,
            &last_price,
        )
    } else {
        Vec::new()
    };
    for p in report
        .problems
        .iter()
        .chain(&periods.problems)
        .chain(&series.problems)
        .chain(&problems)
    {
        eprintln!("overview: {p}");
    }
    let view = periods.view(period).expect("period_views has every period");
    Ok(Built {
        panel: Panel::Ready(Box::new(OverviewPanel::new(&report, view, &series, bench))),
        assets,
        prices: Some((report.last_known.clone(), report.pnl.complete)),
    })
}

pub async fn show(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OverviewQuery>,
) -> Result<Response, AppError> {
    let period = parse_period(q.period.as_deref());
    let bench = q.bench.as_deref() != Some("off");
    let htmx = is_htmx(&headers);
    let built = build(&state, period, bench, !htmx).await?;
    let panel = built.panel.render()?;
    if htmx {
        return Ok(Html(panel).into_response());
    }
    let mut shell = Shell::load(&state, &headers, Page::Overview).await?;
    if let Some((last_known, complete)) = &built.prices {
        shell = shell.with_prices(last_known, *complete, href(period, bench));
    }
    let page = OverviewPage {
        shell,
        panel,
        assets: built.assets,
    };
    Ok(Html(page.render()?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal_macros::dec;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn periods_come_from_their_labels_and_have_urls() {
        assert_eq!(parse_period(Some("6M")), Period::SixMonths);
        assert_eq!(parse_period(Some("YTD")), Period::YearToDate);
        assert_eq!(parse_period(Some("6m")), Period::All);
        assert_eq!(parse_period(None), Period::All);
        assert_eq!(href(Period::All, true), "/");
        assert_eq!(href(Period::All, false), "/?bench=off");
        assert_eq!(href(Period::OneYear, true), "/?period=1Y");
        assert_eq!(href(Period::OneMonth, false), "/?period=1M&bench=off");
    }

    #[test]
    fn one_valued_day_is_the_not_enough_history_state() {
        let point = |day: &str, value: Option<Decimal>| chart::Point {
            date: d(day),
            value,
            contributions: Some(dec!(500)),
            benchmark: None,
        };
        // The first contribution today: one day, valued.
        assert_eq!(
            one_point_note(&[point("2026-10-17", Some(dec!(500)))]),
            Some("One data point so far. The chart fills in from tomorrow.")
        );
        // Two days, one without a price.
        assert_eq!(
            one_point_note(&[
                point("2026-10-16", None),
                point("2026-10-17", Some(dec!(510)))
            ]),
            Some("One data point so far. The chart fills in once more days have prices.")
        );
        // Two valued days are a line; none valued is no chart at all.
        let two = [
            point("2026-10-16", Some(dec!(500))),
            point("2026-10-17", Some(dec!(510))),
        ];
        assert_eq!(one_point_note(&two), None);
        assert_eq!(one_point_note(&[point("2026-10-17", None)]), None);
        assert_eq!(one_point_note(&[]), None);
    }

    #[test]
    fn captions_name_the_period() {
        assert_eq!(caption(Period::OneMonth, None), "past month");
        assert_eq!(caption(Period::YearToDate, None), "since 1 Jan");
        assert_eq!(
            caption(Period::All, Some(d("2025-05-12"))),
            "since May 2025"
        );
        assert_eq!(caption(Period::All, None), "all time");
    }

    #[test]
    fn xirr_tile_shows_a_dash_for_short_periods() {
        // 0.186 -> "+18,6%" in gain green (screens/01, All).
        let t = xirr_tile(&Ok(dec!(0.186)), "since May 2025");
        assert_eq!(
            (t.figure.as_str(), t.color, t.note.as_str()),
            ("+18,6%", "var(--gain)", "per year, since May 2025")
        );
        assert_eq!(xirr_tile(&Ok(dec!(-0.05)), "x").color, "var(--loss)");
        let short = xirr_tile(&Err(PeriodError::TooShort), "past month");
        assert_eq!(
            (short.figure.as_str(), short.note.as_str()),
            ("—", "Needs 3+ months of data")
        );
        let missing = xirr_tile(&Err(PeriodError::MissingEndValue), "x");
        assert_eq!(missing.note, "Not available: no price for the end value");
    }

    fn view(end_value: Decimal, benchmark_value: Option<Decimal>) -> PeriodView {
        use portfolio_core::benchmark::{Benchmark, BenchmarkError};
        PeriodView {
            period: Period::All,
            start: None,
            end: d("2026-10-17"),
            since: Some(d("2025-05-12")),
            start_value: Some(dec!(0)),
            end_value: Some(end_value),
            contributions: dec!(10092),
            xirr: Err(PeriodError::TooShort),
            benchmark: match benchmark_value {
                Some(value) => Ok(Benchmark {
                    units: dec!(1),
                    contributions: dec!(10092),
                    value,
                    xirr: Err(portfolio_core::xirr::XirrError::NoSignChange),
                }),
                None => Err(PeriodError::Benchmark(BenchmarkError::MissingPrice(d(
                    "2026-10-17",
                )))),
            },
        }
    }

    #[test]
    fn benchmark_note_says_who_is_ahead() {
        let plain = |s: String| s.replace('\u{a0}', " ");
        // 11 500 - 11 160 = 340 ahead; 11 500 - 11 800 = -300: VWCE ahead by 300.
        assert_eq!(
            plain(benchmark_note(&view(dec!(11500), Some(dec!(11160))), true)),
            "You’re +340,00 zł ahead of all-in VWCE"
        );
        assert_eq!(
            plain(benchmark_note(&view(dec!(11500), Some(dec!(11800))), true)),
            "All-in VWCE would be +300,00 zł ahead"
        );
        assert_eq!(
            benchmark_note(&view(dec!(11500), None), true),
            "All-in VWCE: no price for some days"
        );
        assert_eq!(benchmark_note(&view(dec!(11500), Some(dec!(1))), false), "");
    }
}
