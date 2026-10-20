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
use std::collections::BTreeMap;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use portfolio_sources::prices::LastKnown;
use portfolio_sources::valuation::GOLD_COINS;
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

const ALL_PAGES: [Page; 6] = [
    Page::Overview,
    Page::Allocation,
    Page::Ikze,
    Page::Gold,
    Page::Import,
    Page::Components,
];

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
}

/// Prices fetched longer ago than this are stale (`screens/08`, "Stale · over 6 h").
pub const STALE_AFTER: TimeDelta = TimeDelta::hours(6);

/// The sync status variants of `screens/08`. "Updating prices…" is not one of them: the layout
/// shows it by itself while an htmx request is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    NoPrices,
    Fresh,
    Stale,
    /// A price source failed on this page: "Couldn't update prices" with Retry.
    Failed,
}

/// The price sync line in the sidebar footer, and its short form in the phone top bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    pub kind: SyncKind,
    /// "Prices updated 08:00".
    pub long: String,
    /// "08:00".
    pub short: String,
    /// Colour token of the dot (of the ✕ when failed).
    pub dot: &'static str,
    /// Colour token of the text.
    pub color: &'static str,
}

impl SyncStatus {
    pub fn is_failed(&self) -> bool {
        self.kind == SyncKind::Failed
    }

    pub fn failed() -> SyncStatus {
        SyncStatus {
            kind: SyncKind::Failed,
            long: "Couldn’t update prices".into(),
            short: "Retry".into(),
            dot: "var(--loss)",
            color: "var(--loss)",
        }
    }
}

/// From the local time prices were last fetched: fresh under [`STALE_AFTER`] ("Prices updated
/// 08:00"), stale after it, in amber ("Prices from yesterday 22:00").
pub fn sync_status(last: Option<NaiveDateTime>, now: NaiveDateTime) -> SyncStatus {
    let Some(t) = last else {
        return SyncStatus {
            kind: SyncKind::NoPrices,
            long: "No prices yet".into(),
            short: "—".into(),
            dot: "var(--text-3)",
            color: "var(--text-3)",
        };
    };
    let short = if t.date() == now.date() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%d.%m").to_string()
    };
    let when = format::time_ago(t, now);
    if now - t < STALE_AFTER {
        SyncStatus {
            kind: SyncKind::Fresh,
            long: format!("Prices updated {when}"),
            short,
            dot: "var(--gain)",
            color: "var(--text-3)",
        }
    } else {
        SyncStatus {
            kind: SyncKind::Stale,
            long: format!("Prices from {when}"),
            short,
            dot: "var(--warn)",
            color: "var(--warn)",
        }
    }
}

/// The source's name in the banner.
fn provider_name(provider: &str) -> &str {
    match provider {
        "kraken" => "Kraken",
        "alphavantage" => "Alpha Vantage",
        "nbp" => "NBP",
        other => other,
    }
}

/// An asset as the banner names it: its symbol, or "Gold" for the coins.
fn banner_asset(symbol: &str) -> &str {
    if symbol == GOLD_COINS { "Gold" } else { symbol }
}

/// "BTC", "BTC and ETH", "VWCE, BTC and ETH".
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The "price source unavailable" banner of `screens/08`, one per source that is down:
/// "Kraken isn't responding. BTC and ETH use last known prices from 15 Oct, 22:00."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceBanner {
    pub title: String,
    pub detail: String,
}

/// One stand-in price: the asset, its provider and when its closes were fetched (local time).
pub type StandIn<'a> = (&'a str, &'a str, Option<NaiveDateTime>);

/// The banners for the assets valued at a stored close, grouped by source in name order. The
/// time is the group's oldest fetch, so it never claims fresher prices than some asset has.
pub fn price_banners(stand_ins: &[StandIn<'_>]) -> Vec<PriceBanner> {
    let mut by_provider: BTreeMap<&str, Vec<(&str, Option<NaiveDateTime>)>> = BTreeMap::new();
    for (asset, provider, at) in stand_ins {
        by_provider
            .entry(*provider)
            .or_default()
            .push((banner_asset(asset), *at));
    }
    by_provider
        .into_iter()
        .map(|(provider, assets)| {
            let names: Vec<&str> = assets.iter().map(|(name, _)| *name).collect();
            let oldest = assets.iter().filter_map(|(_, at)| *at).min();
            let one = names.len() == 1;
            let (verb, what) = if one {
                ("uses its", "last known price")
            } else {
                ("use", "last known prices")
            };
            let when = oldest.map_or(String::new(), |t| format!(" from {}", format::day_time(t)));
            PriceBanner {
                title: format!("{} isn’t responding.", provider_name(provider)),
                detail: format!("{} {verb} {what}{when}.", and_list(&names)),
            }
        })
        .collect()
}

/// A UTC moment in the Mac's local time, as the pages show times.
pub fn local(t: DateTime<Utc>) -> NaiveDateTime {
    t.with_timezone(&Local).naive_local()
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
    /// What Retry reloads: this page, in its current state.
    pub retry: String,
    /// One per price source that is down (only pages that value the portfolio set them).
    pub banners: Vec<PriceBanner>,
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
                })
                .collect(),
            sync,
            path: page.path(),
            retry: page.path().into(),
            banners: Vec::new(),
        }
    }

    /// A page that valued the portfolio: a banner per source that is down, and the sync status
    /// "Couldn't update prices" when a source failed or a price is missing. `retry` reloads the
    /// page as it is (with its query).
    pub fn with_prices(
        mut self,
        last_known: &BTreeMap<String, LastKnown>,
        complete: bool,
        retry: String,
    ) -> Shell {
        let stand_ins: Vec<StandIn<'_>> = last_known
            .iter()
            .map(|(asset, l)| (asset.as_str(), l.provider, l.fetched_at.map(local)))
            .collect();
        self.banners = price_banners(&stand_ins);
        if !last_known.is_empty() || !complete {
            self.sync = SyncStatus::failed();
        }
        self.retry = retry;
        self
    }

    pub async fn load(
        state: &AppState,
        headers: &HeaderMap,
        page: Page,
    ) -> Result<Shell, AppError> {
        let last = portfolio_db::prices::latest_fetch(&state.pool)
            .await?
            .map(local);
        Ok(Shell::new(
            page,
            theme_cookie(headers),
            sync_status(last, local((state.now)())),
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

/// Where the toggle goes back to: one of the app's own pages (the form only sends
/// `Shell::path`). Anything else, such as another host, goes to `/`.
pub fn safe_next(next: &str) -> &'static str {
    ALL_PAGES
        .iter()
        .map(|p| p.path())
        .find(|path| *path == next)
        .unwrap_or("/")
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

/// The figures of an asset card (`screens/09`, "Asset card"; the Overview fills them from the
/// report, see [`crate::assets`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetCard {
    pub name: String,
    /// Under the name on desktop: "Physical · 1 coin, 1/10 oz".
    pub sub: String,
    /// Under the name on phones (`screens/03`): "1 coin · 1/10 oz".
    pub phone_sub: String,
    /// Place in the profit ranking; None for an asset without a price.
    pub rank: Option<usize>,
    pub value: String,
    pub spark: Option<String>,
    pub invested: String,
    /// None without a price.
    pub pnl: Option<Delta>,
    pub pnl_percent: String,
    pub share: String,
    pub share_width: String,
    /// "Last price 15 Oct, 22:00" while the asset is valued at a stored close because its
    /// source is down: the card gets the "Stale" chip (`screens/08`).
    pub stale: Option<String>,
}

/// An asset's figures in PLN, as `core` computes them (fractions for the ratios).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardFigures {
    /// None without a price.
    pub value: Option<Decimal>,
    /// What is still in the asset: cost minus proceeds and income.
    pub invested: Decimal,
    pub pnl: Option<Decimal>,
    /// P&L / cost; None without a price or without a cost.
    pub pnl_ratio: Option<Decimal>,
    /// The asset's part of the total profit; None when there is no total to share.
    pub share: Option<Decimal>,
}

impl CardFigures {
    /// A holding with no sales or income: invested = cost, P&L = value - invested.
    pub fn held(value: Decimal, invested: Decimal, share: Decimal) -> CardFigures {
        let pnl = value - invested;
        CardFigures {
            value: Some(value),
            invested,
            pnl: Some(pnl),
            pnl_ratio: (!invested.is_zero()).then(|| pnl / invested),
            share: Some(share),
        }
    }
}

impl AssetCard {
    pub fn new(
        name: &str,
        sub: &str,
        phone_sub: &str,
        rank: Option<usize>,
        figures: &CardFigures,
        series: &[Decimal],
    ) -> AssetCard {
        let dash = || "—".to_owned();
        AssetCard {
            name: name.into(),
            sub: sub.into(),
            phone_sub: phone_sub.into(),
            rank,
            value: figures.value.map_or_else(dash, format::pln),
            spark: format::sparkline_path(series),
            invested: format::pln(figures.invested),
            pnl: figures.pnl.map(format::delta_pln),
            pnl_percent: figures.pnl_ratio.map_or_else(dash, format::signed_percent),
            share: figures.share.map_or_else(dash, format::percent),
            share_width: format::css_percent(figures.share.unwrap_or_default()),
            stale: None,
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
                "1 coin · 1/10 oz",
                Some(1),
                &CardFigures::held(sample("2100"), sample("1640"), sample("0.327")),
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
    use chrono::TimeZone;
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
    fn only_the_apps_own_pages_are_returned_to() {
        assert_eq!(safe_next("/gold"), "/gold");
        assert_eq!(safe_next(""), "/");
        assert_eq!(safe_next("https://example.com"), "/");
        assert_eq!(safe_next("//example.com"), "/");
        assert_eq!(safe_next("/\\example.com"), "/");
        assert_eq!(safe_next("/\t/example.com"), "/");
        assert_eq!(safe_next("/gold?x=1"), "/");
        assert_eq!(safe_next("/components"), "/components");
    }

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn sync_status_is_fresh_for_six_hours_then_stale() {
        let now = at(16, 9, 30);
        let fresh = sync_status(Some(at(16, 8, 0)), now);
        assert_eq!(
            (fresh.kind, fresh.long.as_str(), fresh.short.as_str()),
            (SyncKind::Fresh, "Prices updated 08:00", "08:00")
        );
        assert_eq!((fresh.dot, fresh.color), ("var(--gain)", "var(--text-3)"));
        // 23:50 the day before is 9 h 40 min ago: stale, in amber.
        let stale = sync_status(Some(at(15, 23, 50)), now);
        assert_eq!(
            (stale.kind, stale.long.as_str(), stale.short.as_str()),
            (SyncKind::Stale, "Prices from yesterday 23:50", "15.10")
        );
        assert_eq!((stale.dot, stale.color), ("var(--warn)", "var(--warn)"));
        // Exactly 6 h is stale; a minute less is fresh, even across midnight.
        assert_eq!(sync_status(Some(at(16, 3, 30)), now).kind, SyncKind::Stale);
        let late = sync_status(Some(at(15, 22, 0)), at(16, 3, 59));
        assert_eq!(
            (late.kind, late.long.as_str()),
            (SyncKind::Fresh, "Prices updated yesterday 22:00")
        );
        let old = sync_status(Some(at(13, 22, 0)), now);
        assert_eq!(old.long, "Prices from 13 Oct, 22:00");
        let none = sync_status(None, now);
        assert_eq!(
            (none.kind, none.long.as_str()),
            (SyncKind::NoPrices, "No prices yet")
        );
    }

    #[test]
    fn a_banner_per_source_that_is_down() {
        let banners = price_banners(&[
            ("BTC", "kraken", Some(at(15, 22, 0))),
            ("ETH", "kraken", Some(at(15, 21, 0))),
            (GOLD_COINS, "nbp", Some(at(15, 12, 15))),
        ]);
        let text: Vec<_> = banners
            .iter()
            .map(|b| format!("{} {}", b.title, b.detail))
            .collect();
        // Kraken's time is the older of its two fetches.
        assert_eq!(
            text,
            [
                "Kraken isn’t responding. BTC and ETH use last known prices from 15 Oct, 21:00.",
                "NBP isn’t responding. Gold uses its last known price from 15 Oct, 12:15.",
            ]
        );
        let av = price_banners(&[("VWCE", "alphavantage", None)]);
        assert_eq!(
            (av[0].title.as_str(), av[0].detail.as_str()),
            (
                "Alpha Vantage isn’t responding.",
                "VWCE uses its last known price."
            )
        );
        assert!(price_banners(&[]).is_empty());
        assert_eq!(and_list(&["VWCE", "BTC", "ETH"]), "VWCE, BTC and ETH");
    }

    #[test]
    fn a_failed_source_turns_the_sync_status_to_retry() {
        let fresh = || {
            Shell::new(
                Page::Overview,
                None,
                sync_status(Some(at(16, 8, 0)), at(16, 9, 0)),
            )
        };
        let fetched = Utc.with_ymd_and_hms(2026, 10, 15, 20, 0, 0).unwrap();
        let down = BTreeMap::from([(
            "BTC".to_owned(),
            LastKnown {
                provider: "kraken",
                fetched_at: Some(fetched),
                error: "HTTP 500".into(),
            },
        )]);
        let shell = fresh().with_prices(&down, true, "/?period=6M".into());
        assert_eq!(shell.sync.kind, SyncKind::Failed);
        assert_eq!(shell.sync.long, "Couldn’t update prices");
        assert_eq!(shell.retry, "/?period=6M");
        assert_eq!(shell.banners.len(), 1);
        // A missing price fails too, without a banner; all prices fresh changes nothing.
        let missing = fresh().with_prices(&BTreeMap::new(), false, "/".into());
        assert_eq!(
            (missing.sync.kind, missing.banners.len()),
            (SyncKind::Failed, 0)
        );
        let fine = fresh().with_prices(&BTreeMap::new(), true, "/".into());
        assert_eq!(fine.sync.kind, SyncKind::Fresh);
    }

    #[test]
    fn shell_marks_the_current_page() {
        let shell = Shell::new(Page::Gold, None, sync_status(None, NaiveDateTime::MIN));
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
        let hrefs: Vec<_> = shell.nav.iter().map(|n| n.href).collect();
        assert_eq!(hrefs, ["/", "/allocation", "/ikze", "/gold", "/import"]);

        let dark = Shell::new(
            Page::Gold,
            Some(Theme::Dark),
            sync_status(None, NaiveDateTime::MIN),
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
            "1 coin",
            Some(1),
            &CardFigures::held(dec!(2100), dec!(1640), dec!(0.327)),
            &[dec!(1), dec!(2)],
        );
        let nb = |s: &str| s.replace(' ', "\u{a0}");
        assert_eq!(card.value, nb("2 100,00 zł"));
        assert_eq!(card.invested, nb("1 640,00 zł"));
        let pnl = card.pnl.clone().unwrap();
        assert_eq!(pnl.text, nb("+460,00 zł"));
        assert_eq!(pnl.arrow(), "▲");
        assert_eq!(card.pnl_percent, "+28,0%");
        assert_eq!(
            (card.share.as_str(), card.share_width.as_str()),
            ("32,7%", "32.7%")
        );
        assert_eq!(card.spark.as_deref(), Some("M0.0 30.0 L96.0 2.0"));

        let nothing_in = AssetCard::new(
            "X",
            "",
            "",
            Some(2),
            &CardFigures::held(dec!(0), dec!(0), dec!(0)),
            &[],
        );
        assert_eq!(
            (nothing_in.pnl_percent.as_str(), nothing_in.spark),
            ("—", None)
        );
    }

    #[test]
    fn an_unpriced_asset_card_shows_dashes() {
        // 500 zł in, no price: no value, P&L or share; the bar is empty.
        let card = AssetCard::new(
            "Ether",
            "ETH · Kraken",
            "ETH · Kraken",
            None,
            &CardFigures {
                value: None,
                invested: dec!(500),
                pnl: None,
                pnl_ratio: None,
                share: None,
            },
            &[],
        );
        assert_eq!(card.value, "—");
        assert_eq!(card.invested, "500,00\u{a0}zł");
        assert_eq!(card.pnl, None);
        assert_eq!(
            (
                card.pnl_percent.as_str(),
                card.share.as_str(),
                card.share_width.as_str()
            ),
            ("—", "—", "0%")
        );
    }

    #[test]
    fn a_negative_share_has_an_empty_bar() {
        // Lost 150 of a 1 000 total profit: −15,0%, and the bar does not go below zero.
        let card = AssetCard::new(
            "Gold",
            "",
            "",
            Some(3),
            &CardFigures::held(dec!(1850), dec!(2000), dec!(-0.15)),
            &[],
        );
        assert_eq!(card.share, "\u{2212}15,0%");
        assert_eq!(card.share_width, "0%");
        assert_eq!(card.pnl.unwrap().arrow(), "▼");
        assert_eq!(card.pnl_percent, "\u{2212}7,5%");
    }
}
