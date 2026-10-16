//! The base layout every page shares (`design/README.md`, "Layout"): the 224px sidebar with the
//! nav, the price sync status and the theme toggle; on phones (≤760px) a top bar with nav pills.
//! Also the stylesheets, the theme cookie and `/components`, a sheet of the shared components
//! rendered with the design's sample figures (`screens/09-components.png`).
//!
//! The theme follows `prefers-color-scheme` until the toggle is used; the choice is then kept in
//! the `theme` cookie, so the server renders `data-theme` itself and the page does not flash.

use askama::Template;
use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{Local, NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::format::{self, Delta};
use crate::{AppError, AppState};

/// The design's tokens and base styles, served as they are in `design/`.
pub const TOKENS_CSS: &str = include_str!("../../../design/tokens.css");
/// Layout and component classes on top of the tokens (no colours of their own).
pub const APP_CSS: &str = include_str!("../static/app.css");

const THEME_COOKIE: &str = "theme";
/// A year, in seconds.
const THEME_MAX_AGE: u32 = 365 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn parse(text: &str) -> Option<Theme> {
        match text {
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// The theme the toggle switches to.
    pub fn other(self) -> Theme {
        match self {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        }
    }

    /// The toggle's label: it names the theme it switches to.
    pub fn toggle_label(self) -> &'static str {
        match self {
            Theme::Light => "Dark theme",
            Theme::Dark => "Light theme",
        }
    }
}

/// The theme saved in the `theme` cookie, if any. An unknown value counts as no choice.
pub fn theme_cookie(headers: &HeaderMap) -> Option<Theme> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == THEME_COOKIE)
        .and_then(|(_, value)| Theme::parse(value.trim()))
}

/// The pages in the nav, in the design's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Allocation,
    Ikze,
    Gold,
    Import,
    /// Not in the nav: the component sheet.
    Components,
}

const NAV: [Page; 5] = [
    Page::Overview,
    Page::Allocation,
    Page::Ikze,
    Page::Gold,
    Page::Import,
];

impl Page {
    pub fn label(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Allocation => "Allocation",
            Page::Ikze => "IKZE",
            Page::Gold => "Gold",
            Page::Import => "Import",
            Page::Components => "Components",
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Page::Overview => "/",
            Page::Allocation => "/allocation",
            Page::Ikze => "/ikze",
            Page::Gold => "/gold",
            Page::Import => "/import",
            Page::Components => "/components",
        }
    }

    /// Built to the design yet. The others are shown in the nav but not linked (P20–P24).
    fn built(self) -> bool {
        matches!(self, Page::Gold | Page::Components)
    }

    /// The nav icon from `design/reference/01-overview.html`.
    fn icon(self) -> &'static str {
        match self {
            Page::Overview => {
                r#"<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" aria-hidden="true"><path d="M3.5 20h17M7 16v-5M12 16V6M17 16V9"/></svg>"#
            }
            Page::Allocation => {
                r#"<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round" aria-hidden="true"><path d="M11 4a8 8 0 1 0 8.5 9H11z"/><path d="M14.5 3.3a8 8 0 0 1 6.2 6.2h-6.2z"/></svg>"#
            }
            Page::Ikze => {
                r#"<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round" aria-hidden="true"><rect x="3.5" y="5" width="17" height="14" rx="2.5"/><circle cx="12" cy="12" r="3"/><path d="M7 19v1.5M17 19v1.5"/></svg>"#
            }
            Page::Gold => {
                r#"<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="9.5" cy="12" r="6"/><path d="M14 6.6a6 6 0 0 1 0 10.8"/><path d="M17.5 7.5a6 6 0 0 1 0 9"/></svg>"#
            }
            Page::Import => {
                r#"<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M4.5 19.5h15"/></svg>"#
            }
            Page::Components => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavItem {
    pub label: &'static str,
    pub href: &'static str,
    pub icon: &'static str,
    pub current: bool,
    pub built: bool,
}

/// The price sync line in the sidebar footer, and its short form in the phone top bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    /// "Prices updated 08:00".
    pub long: String,
    /// "08:00".
    pub short: String,
    /// Colour token of the dot.
    pub dot: &'static str,
}

/// From the local time prices were last written: the time when that was today, else the date
/// (amber, since the prices are not today's). P30 adds the source-down variants.
pub fn sync_status(last: Option<NaiveDateTime>, today: NaiveDate) -> SyncStatus {
    match last {
        None => SyncStatus {
            long: "No prices yet".into(),
            short: "—".into(),
            dot: "var(--text-3)",
        },
        Some(t) if t.date() == today => {
            let time = t.format("%H:%M").to_string();
            SyncStatus {
                long: format!("Prices updated {time}"),
                short: time,
                dot: "var(--gain)",
            }
        }
        Some(t) => SyncStatus {
            long: format!("Prices updated {}", format::date(t.date())),
            short: t.format("%d.%m").to_string(),
            dot: "var(--warn)",
        },
    }
}

/// What the layout needs on every page.
#[derive(Debug, Clone)]
pub struct Shell {
    pub theme: Theme,
    /// The theme came from the cookie; without it the page follows `prefers-color-scheme`.
    pub chosen: bool,
    pub nav: Vec<NavItem>,
    pub sync: SyncStatus,
    /// Where the theme toggle returns to.
    pub path: &'static str,
}

impl Shell {
    pub fn new(page: Page, cookie: Option<Theme>, sync: SyncStatus) -> Shell {
        Shell {
            theme: cookie.unwrap_or(Theme::Light),
            chosen: cookie.is_some(),
            nav: NAV
                .iter()
                .map(|&p| NavItem {
                    label: p.label(),
                    href: p.path(),
                    icon: p.icon(),
                    current: p == page,
                    built: p.built(),
                })
                .collect(),
            sync,
            path: page.path(),
        }
    }

    pub async fn load(
        state: &AppState,
        headers: &HeaderMap,
        page: Page,
    ) -> Result<Shell, AppError> {
        let last = portfolio_db::prices::latest_fetch(&state.pool)
            .await?
            .map(|t| t.with_timezone(&Local).naive_local());
        Ok(Shell::new(
            page,
            theme_cookie(headers),
            sync_status(last, (state.today)()),
        ))
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/static/tokens.css", get(|| async { css(TOKENS_CSS) }))
        .route("/static/app.css", get(|| async { css(APP_CSS) }))
        .route("/theme", post(set_theme))
        .route("/components", get(components))
}

fn css(body: &'static str) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ThemeForm {
    pub theme: String,
    #[serde(default)]
    pub next: String,
}

/// A local path to go back to; anything else (another host, `//host`, `/\host`) goes to `/`.
pub fn safe_next(next: &str) -> &str {
    let local = next.starts_with('/') && !next.starts_with("//") && !next.starts_with("/\\");
    if local { next } else { "/" }
}

/// Saves the chosen theme in the cookie and goes back to the page the toggle was on.
async fn set_theme(Form(form): Form<ThemeForm>) -> Response {
    let Some(theme) = Theme::parse(&form.theme) else {
        return (StatusCode::BAD_REQUEST, "unknown theme").into_response();
    };
    let cookie = format!(
        "{THEME_COOKIE}={}; Path=/; Max-Age={THEME_MAX_AGE}; SameSite=Lax; HttpOnly",
        theme.as_str()
    );
    (
        [(header::SET_COOKIE, cookie)],
        Redirect::to(safe_next(&form.next)),
    )
        .into_response()
}

/// An actual-over-target bar segment (`screens/09`, "Allocation bar").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarSegment {
    pub width: String,
    pub target_width: String,
    /// One accent shade per class, stepped in lightness.
    pub color: &'static str,
    pub radius: &'static str,
}

/// Shares (fractions) of the asset classes and their targets, in the same order: VWCE, Crypto,
/// Gold. More than three segments reuse the lightest shade.
pub fn allocation_bar(actual: &[Decimal], target: &[Decimal]) -> Vec<BarSegment> {
    const SHADES: [&str; 3] = ["var(--accent)", "var(--accent-2)", "var(--accent-3)"];
    let last = actual.len().saturating_sub(1);
    actual
        .iter()
        .zip(target)
        .enumerate()
        .map(|(i, (a, t))| BarSegment {
            width: format::css_percent(*a),
            target_width: format::css_percent(*t),
            color: SHADES[i.min(SHADES.len() - 1)],
            radius: match (i == 0, i == last) {
                (true, true) => "4px",
                (true, false) => "4px 2px 2px 4px",
                (false, true) => "2px 4px 4px 2px",
                (false, false) => "2px",
            },
        })
        .collect()
}

/// The figures of an asset card (`screens/09`, "Asset card"; P21 fills them from the report).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetCard {
    pub name: String,
    pub sub: String,
    pub rank: usize,
    pub value: String,
    pub spark: Option<String>,
    pub invested: String,
    pub pnl: Delta,
    pub pnl_percent: String,
    pub share: String,
    pub share_width: String,
}

impl AssetCard {
    /// `share` is the asset's part of the total profit (a fraction).
    pub fn new(
        name: &str,
        sub: &str,
        rank: usize,
        value: Decimal,
        invested: Decimal,
        share: Decimal,
        series: &[Decimal],
    ) -> AssetCard {
        let pnl = value - invested;
        AssetCard {
            name: name.into(),
            sub: sub.into(),
            rank,
            value: format::pln(value),
            spark: format::sparkline_path(series),
            invested: format::pln(invested),
            pnl: format::delta_pln(pnl),
            pnl_percent: if invested.is_zero() {
                "—".into()
            } else {
                format::signed_percent(pnl / invested)
            },
            share: format::percent(share),
            share_width: format::css_percent(share),
        }
    }
}

/// The design's sample figures: the component sheet shows no portfolio data.
#[derive(Template)]
#[template(path = "components.html")]
pub struct ComponentsPage {
    pub shell: Shell,
    pub tokens: Vec<&'static str>,
    pub hero: String,
    pub figure: String,
    pub xirr: Delta,
    pub contributed: String,
    pub gain: Delta,
    pub loss: Delta,
    pub flat: Delta,
    pub gain_percent: Delta,
    pub loss_percent: Delta,
    pub drift: String,
    pub used: String,
    pub limit: String,
    pub used_width: String,
    /// aria-valuenow, 0–100.
    pub used_now: String,
    pub used_caption: String,
    pub bar: Vec<BarSegment>,
    pub card: AssetCard,
    pub rows: Vec<SampleRow>,
}

pub struct SampleRow {
    pub date: String,
    pub kind: &'static str,
    pub new: bool,
    pub muted: bool,
    pub amount: String,
}

fn sample(text: &str) -> Decimal {
    text.parse().expect("sample figures are decimals")
}

impl ComponentsPage {
    pub fn new(shell: Shell) -> ComponentsPage {
        let used = sample("5652");
        let limit = sample("11304");
        let day = |d: u32, m: u32| format::date(NaiveDate::from_ymd_opt(2026, m, d).expect("date"));
        ComponentsPage {
            shell,
            tokens: vec![
                "--bg",
                "--surface",
                "--surface-2",
                "--line",
                "--text",
                "--text-2",
                "--text-3",
                "--accent",
                "--accent-2",
                "--accent-3",
                "--accent-soft",
                "--gain",
                "--gain-soft",
                "--loss",
                "--loss-soft",
                "--warn",
            ],
            hero: format::number(sample("11500"), 2),
            figure: format::pln(sample("6100")),
            xirr: format::delta_percent(sample("0.186")),
            contributed: format::pln(sample("10092")),
            gain: format::delta_pln(sample("1408")),
            loss: format::delta_pln(sample("-312.40")),
            flat: format::delta_pln(Decimal::ZERO),
            gain_percent: format::delta_percent(sample("0.14")),
            loss_percent: format::delta_percent(sample("-0.027")),
            drift: format::drift_pp(sample("-0.02")),
            used: format::pln(used),
            limit: format::pln(limit),
            used_width: format::css_percent(used / limit),
            used_now: (used / limit * Decimal::ONE_HUNDRED).round().to_string(),
            used_caption: format!("{} used", format::percent(used / limit)),
            bar: allocation_bar(
                &[sample("0.53"), sample("0.287"), sample("0.183")],
                &[sample("0.55"), sample("0.25"), sample("0.20")],
            ),
            card: AssetCard::new(
                "Gold",
                "Physical · 1 coin, 1/10 oz",
                1,
                sample("2100"),
                sample("1640"),
                sample("0.327"),
                &[
                    "1640", "1660", "1700", "1720", "1800", "1850", "1830", "1880", "1950", "1990",
                    "1940", "1960", "1990", "2010", "2030", "2060", "2100",
                ]
                .map(sample),
            ),
            rows: vec![
                SampleRow {
                    date: day(2, 10),
                    kind: "Deposit",
                    new: true,
                    muted: false,
                    amount: format::signed_pln(sample("942")),
                },
                SampleRow {
                    date: day(2, 9),
                    kind: "Deposit · imported",
                    new: false,
                    muted: true,
                    amount: format::signed_pln(sample("942")),
                },
                SampleRow {
                    date: day(3, 9),
                    kind: "Stock purchase",
                    new: false,
                    muted: false,
                    amount: format::signed_pln(sample("-941.10")),
                },
            ],
        }
    }
}

async fn components(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let shell = Shell::load(&state, &headers, Page::Components).await?;
    Ok(Html(ComponentsPage::new(shell).render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn cookies(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for v in values {
            headers.append(header::COOKIE, HeaderValue::from_str(v).unwrap());
        }
        headers
    }

    #[test]
    fn theme_comes_from_the_cookie() {
        assert_eq!(theme_cookie(&cookies(&[])), None);
        assert_eq!(theme_cookie(&cookies(&["theme=dark"])), Some(Theme::Dark));
        assert_eq!(
            theme_cookie(&cookies(&["a=1; theme=light; b=2"])),
            Some(Theme::Light)
        );
        assert_eq!(
            theme_cookie(&cookies(&["a=1", "theme=dark"])),
            Some(Theme::Dark)
        );
        assert_eq!(theme_cookie(&cookies(&["theme=blue"])), None);
        assert_eq!(theme_cookie(&cookies(&["mytheme=dark"])), None);
    }

    #[test]
    fn toggle_names_the_other_theme() {
        assert_eq!(Theme::Light.other(), Theme::Dark);
        assert_eq!(Theme::Light.toggle_label(), "Dark theme");
        assert_eq!(Theme::Dark.toggle_label(), "Light theme");
    }

    #[test]
    fn only_local_paths_are_returned_to() {
        assert_eq!(safe_next("/gold"), "/gold");
        assert_eq!(safe_next(""), "/");
        assert_eq!(safe_next("https://example.com"), "/");
        assert_eq!(safe_next("//example.com"), "/");
        assert_eq!(safe_next("/\\example.com"), "/");
    }

    #[test]
    fn sync_status_shows_the_time_today_and_the_date_before() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let at = |d: u32, h: u32, m: u32| {
            NaiveDate::from_ymd_opt(2026, 10, d)
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap()
        };
        let now = sync_status(Some(at(16, 8, 0)), today);
        assert_eq!(
            (now.long.as_str(), now.short.as_str(), now.dot),
            ("Prices updated 08:00", "08:00", "var(--gain)")
        );
        let old = sync_status(Some(at(15, 23, 59)), today);
        assert_eq!(
            (old.long.as_str(), old.short.as_str(), old.dot),
            ("Prices updated 15.10.2026", "15.10", "var(--warn)")
        );
        assert_eq!(sync_status(None, today).long, "No prices yet");
    }

    #[test]
    fn shell_marks_the_current_page_and_unbuilt_ones() {
        let shell = Shell::new(Page::Gold, None, sync_status(None, NaiveDate::MIN));
        assert_eq!((shell.theme, shell.chosen), (Theme::Light, false));
        let labels: Vec<_> = shell.nav.iter().map(|n| n.label).collect();
        assert_eq!(labels, ["Overview", "Allocation", "IKZE", "Gold", "Import"]);
        let current: Vec<_> = shell
            .nav
            .iter()
            .filter(|n| n.current)
            .map(|n| n.href)
            .collect();
        assert_eq!(current, ["/gold"]);
        let built: Vec<_> = shell
            .nav
            .iter()
            .filter(|n| n.built)
            .map(|n| n.href)
            .collect();
        assert_eq!(built, ["/gold"]);

        let dark = Shell::new(
            Page::Gold,
            Some(Theme::Dark),
            sync_status(None, NaiveDate::MIN),
        );
        assert_eq!((dark.theme, dark.chosen), (Theme::Dark, true));
    }

    #[test]
    fn allocation_bar_steps_shades_and_rounds_the_ends() {
        let bar = allocation_bar(
            &[dec!(0.53), dec!(0.287), dec!(0.183)],
            &[dec!(0.55), dec!(0.25), dec!(0.20)],
        );
        let widths: Vec<_> = bar
            .iter()
            .map(|s| (s.width.as_str(), s.target_width.as_str()))
            .collect();
        assert_eq!(widths, [("53%", "55%"), ("28.7%", "25%"), ("18.3%", "20%")]);
        let looks: Vec<_> = bar.iter().map(|s| (s.color, s.radius)).collect();
        assert_eq!(
            looks,
            [
                ("var(--accent)", "4px 2px 2px 4px"),
                ("var(--accent-2)", "2px"),
                ("var(--accent-3)", "2px 4px 4px 2px"),
            ]
        );
        assert_eq!(allocation_bar(&[dec!(1)], &[dec!(1)])[0].radius, "4px");
    }

    #[test]
    fn asset_card_matches_the_components_sheet() {
        // Gold in screens/09: 2 100 value, 1 640 invested -> +460 zł, 460 / 1640 = 28.04% -> +28,0%.
        let card = AssetCard::new(
            "Gold",
            "Physical",
            1,
            dec!(2100),
            dec!(1640),
            dec!(0.327),
            &[dec!(1), dec!(2)],
        );
        let nb = |s: &str| s.replace(' ', "\u{a0}");
        assert_eq!(card.value, nb("2 100,00 zł"));
        assert_eq!(card.invested, nb("1 640,00 zł"));
        assert_eq!(card.pnl.text, nb("+460,00 zł"));
        assert_eq!(card.pnl.arrow(), "▲");
        assert_eq!(card.pnl_percent, "+28,0%");
        assert_eq!(
            (card.share.as_str(), card.share_width.as_str()),
            ("32,7%", "32.7%")
        );
        assert_eq!(card.spark.as_deref(), Some("M0.0 30.0 L96.0 2.0"));

        let nothing_in = AssetCard::new("X", "", 2, dec!(0), dec!(0), dec!(0), &[]);
        assert_eq!(
            (nothing_in.pnl_percent.as_str(), nothing_in.spark),
            ("—", None)
        );
    }
}
