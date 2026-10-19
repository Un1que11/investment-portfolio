//! The Allocation page (`design/screens/04`): the actual shares over the dashed targets, the
//! drift table (with BTC and ETH inside crypto) and the top-up plan: an amount, the quick chips,
//! "Include paused assets", what each class gets, the shares after the top-up and whether the
//! VWCE part fits the year's IKZE room.
//!
//! The figures come from `core` ([`portfolio_core::allocation`] and [`portfolio_core::topup`]),
//! on the same valuation as the Overview. Cash is not part of the base. When a held asset has no
//! price the page says so instead of showing shares that would be wrong. The amount and the
//! checkbox re-render the plan through htmx and also work as a plain form (`/allocation?amount=
//! 2000&paused=on`).

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use portfolio_core::AssetClass;
use portfolio_core::allocation::{
    Allocation, AllocationError, Deviation, Sleeve, allocation_of, sleeve_values,
};
use portfolio_core::config::Config;
use portfolio_core::topup::{TopUp, TopUpError, top_up};
use portfolio_db::gold::list_coins;
use portfolio_db::transactions::load_ledger;
use portfolio_sources::valuation::{GOLD_COINS, fetch_inputs, report, requirements, sleeve};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::assets::vwce_wrappers;
use crate::base::{BarSegment, Page, Shell, allocation_bar};
use crate::format;
use crate::{AppError, AppState};

/// The quick amounts under the input, in PLN (942 is the usual monthly contribution).
pub const CHIPS: [u32; 3] = [942, 2000, 5000];
/// The amount the plan starts with, as in the design.
pub const DEFAULT_AMOUNT: Decimal = Decimal::from_parts(2000, 0, 0, false, 0);
/// The most the plan takes: far above any real top-up, and small enough that the pro rata split
/// (amount × need) stays inside `Decimal`; 10^15 with every class bought would overflow it.
pub const MAX_AMOUNT: Decimal = Decimal::from_parts(1_000_000_000, 0, 0, false, 0);
/// The longest drift bar: half the track, from the middle.
const HALF: Decimal = Decimal::from_parts(5, 0, 0, false, 1);

#[derive(Debug, Default, Deserialize)]
pub struct AllocationQuery {
    /// PLN as typed: "2 000", "2000,50".
    pub amount: Option<String>,
    /// "on" includes the paused classes.
    pub paused: Option<String>,
}

/// An amount as typed into the input: spaces (also non-breaking) as thousands separators, a
/// decimal comma or point, at most two decimals, an optional "zł". None when it is not a number
/// (also "1e5", which `Decimal` would read).
pub fn parse_amount(text: &str) -> Option<Decimal> {
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != format::NBSP && *c != '\u{202f}')
        .collect();
    let number = compact.strip_suffix("zł").unwrap_or(&compact);
    let digits = number.strip_prefix('-').unwrap_or(number);
    if digits.is_empty()
        || !digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
        || (number.contains('.') && number.contains(','))
    {
        return None;
    }
    let value = Decimal::from_str(&number.replace(',', ".")).ok()?;
    (value.scale() <= 2).then_some(value)
}

/// An amount as the input shows it: "2 000" for whole złoty, "2 000,50" otherwise.
pub fn amount_text(amount: Decimal) -> String {
    let dp = if amount.fract().is_zero() { 0 } else { 2 };
    format::number(amount, dp)
}

/// A target share as the plan shows it: 0.55 -> "55%", 0.125 -> "12,5%".
fn target_text(fraction: Decimal) -> String {
    let p = (fraction * Decimal::ONE_HUNDRED).normalize();
    format!("{}%", format::number(p, p.scale()))
}

fn class_name(class: AssetClass) -> &'static str {
    match class {
        AssetClass::Vwce => "VWCE",
        AssetClass::Crypto => "Crypto",
        AssetClass::Gold => "Gold",
    }
}

/// The class's swatch: one accent shade each, as in the allocation bar.
fn class_color(class: AssetClass) -> &'static str {
    match class {
        AssetClass::Vwce => "var(--accent)",
        AssetClass::Crypto => "var(--accent-2)",
        AssetClass::Gold => "var(--accent-3)",
    }
}

/// The neutral drift bar of a row: a bar from the middle of a 96px track, to the left when
/// under the target and to the right when over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftBar {
    /// "right" (anchored at the middle, growing left) when under, "left" when over.
    pub anchor: &'static str,
    /// CSS width as a share of the track: 10% per percentage point, at most the half (5 pp).
    pub width: String,
}

pub fn drift_bar(points: Decimal) -> DriftBar {
    DriftBar {
        anchor: if points < Decimal::ZERO {
            "right"
        } else {
            "left"
        },
        width: format::css_percent((points.abs() * Decimal::TEN).min(HALF)),
    }
}

/// The figures every drift row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftCells {
    /// "55,0%".
    pub target: String,
    /// "53,0%".
    pub actual: String,
    pub bar: DriftBar,
    /// "▼ −2,0 pp".
    pub drift: String,
    /// "−225,00 zł".
    pub pln: String,
}

impl DriftCells {
    pub fn new(d: &Deviation) -> DriftCells {
        DriftCells {
            target: format::percent(d.target_share),
            actual: format::percent(d.share),
            bar: drift_bar(d.points()),
            drift: format::drift_pp(d.points()),
            pln: format::signed_pln(d.pln()),
        }
    }
}

/// A coin inside crypto: "BTC · of crypto".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinRow {
    pub symbol: &'static str,
    pub cells: DriftCells,
}

/// A class row of the drift table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRow {
    pub name: &'static str,
    pub color: &'static str,
    pub paused: bool,
    /// "Buying · IKZE", "Purchases paused · Kraken".
    pub sub: String,
    pub cells: DriftCells,
    /// BTC and ETH under crypto; empty for the other classes and when no crypto is held.
    pub coins: Vec<CoinRow>,
}

impl ClassRow {
    /// The padding of the row's cells: tighter at the bottom when the coin rows follow.
    pub fn pad(&self) -> &'static str {
        if self.coins.is_empty() {
            "16px 12px"
        } else {
            "16px 12px 10px"
        }
    }
}

/// Where a class is held, after its status in the row's second line.
fn held_at(class: AssetClass, wrappers: &str) -> String {
    match class {
        AssetClass::Vwce if wrappers.is_empty() => "XTB".into(),
        AssetClass::Vwce => wrappers.into(),
        AssetClass::Crypto => "Kraken".into(),
        AssetClass::Gold => "coins".into(),
    }
}

/// The bars and the table of the left column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftView {
    pub bar: Vec<BarSegment>,
    /// "Actual: VWCE 53,0%, Crypto 28,7%, Gold 18,3%".
    pub actual_label: String,
    /// "Target: VWCE 55%, Crypto 25%, Gold 20%".
    pub target_label: String,
    pub legend: Vec<(&'static str, &'static str)>,
    pub rows: Vec<ClassRow>,
}

impl DriftView {
    /// `wrappers` is where VWCE is held ("IKZE", "IKZE, IKE"; empty when unknown).
    pub fn new(a: &Allocation, config: &Config, wrappers: &str) -> DriftView {
        let class = |c: AssetClass| a.by_class[&c];
        let shares: Vec<Decimal> = AssetClass::ALL.map(|c| class(c).share).to_vec();
        let targets: Vec<Decimal> = AssetClass::ALL.map(|c| class(c).target_share).to_vec();
        let label = |title: &str, part: &dyn Fn(AssetClass) -> String| {
            let parts: Vec<String> = AssetClass::ALL
                .into_iter()
                .map(|c| format!("{} {}", class_name(c), part(c)))
                .collect();
            format!("{title}: {}", parts.join(", "))
        };
        let rows = AssetClass::ALL
            .into_iter()
            .map(|c| {
                let paused = config.is_paused(c);
                let status = if paused { "Purchases paused" } else { "Buying" };
                let coins = match (c, &a.within_crypto) {
                    (AssetClass::Crypto, Some(split)) => vec![
                        CoinRow {
                            symbol: "BTC",
                            cells: DriftCells::new(&split.btc),
                        },
                        CoinRow {
                            symbol: "ETH",
                            cells: DriftCells::new(&split.eth),
                        },
                    ],
                    _ => Vec::new(),
                };
                ClassRow {
                    name: class_name(c),
                    color: class_color(c),
                    paused,
                    sub: format!("{status} · {}", held_at(c, wrappers)),
                    cells: DriftCells::new(&class(c)),
                    coins,
                }
            })
            .collect();
        DriftView {
            bar: allocation_bar(&shares, &targets),
            actual_label: label("Actual", &|c| format::percent(class(c).share)),
            target_label: label("Target", &|c| target_text(class(c).target_share)),
            legend: AssetClass::ALL
                .into_iter()
                .map(|c| (class_name(c), class_color(c)))
                .collect(),
            rows,
        }
    }
}

/// One class in the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    pub name: &'static str,
    pub color: &'static str,
    /// False for a paused class the plan skips: muted, no amount.
    pub bought: bool,
    /// "+2 000,00 zł", "0,00 zł" or "—".
    pub amount: String,
    /// "XTB · counts toward IKZE", "BTC 75,00 zł · ETH 0,00 zł", "Paused · would need 75,00 zł".
    pub note: String,
    /// "60,0% → target 55%".
    pub weight: String,
}

/// A segment of the after-top-up bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterSegment {
    pub width: String,
    pub color: &'static str,
    pub radius: &'static str,
}

/// The note under the plan: does the VWCE part fit this year's IKZE room?
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IkzeNote {
    pub text: String,
    /// Over the room: shown in the warning colours.
    pub warn: bool,
}

/// What the plan shows for an amount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub rows: Vec<PlanRow>,
    pub after: Vec<AfterSegment>,
    /// "VWCE 60,0% · Crypto 24,4% · Gold 15,6%".
    pub after_label: String,
    pub ikze: Option<IkzeNote>,
}

/// What else the plan needs besides the top-up itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanContext {
    /// The year and what is left of its IKZE limit; None when the config has no limit for it.
    pub ikze_room: Option<(i32, Decimal)>,
    /// What one held coin is worth now; None when no coin is held.
    pub coin_value: Option<Decimal>,
}

impl Plan {
    pub fn new(t: &TopUp, config: &Config, include_paused: bool, ctx: PlanContext) -> Plan {
        let after = |c: AssetClass| t.after.by_class[&c];
        let rows = AssetClass::ALL
            .into_iter()
            .map(|c| {
                let bought = include_paused || !config.is_paused(c);
                let buy = t.class_buy(c);
                let note = if !bought {
                    let need = t.would_need.get(&c).copied().unwrap_or_default();
                    format!("Paused · would need {}", format::pln(need))
                } else {
                    match c {
                        AssetClass::Vwce => "XTB · counts toward IKZE".into(),
                        AssetClass::Crypto => {
                            let coin = |s: Sleeve| t.buys.get(&s).copied().unwrap_or_default();
                            format!(
                                "BTC {} · ETH {}",
                                format::pln(coin(Sleeve::Btc)),
                                format::pln(coin(Sleeve::Eth))
                            )
                        }
                        AssetClass::Gold => match ctx.coin_value {
                            Some(v) => format!(
                                "Whole coins only (~{}{}zł) — set aside",
                                format::number(v, 0),
                                format::NBSP
                            ),
                            None => "Whole coins only — set aside".into(),
                        },
                    }
                };
                let amount = match (bought, buy > Decimal::ZERO) {
                    (false, _) => "—".into(),
                    (true, true) => format::signed_pln(buy),
                    (true, false) => format::pln(Decimal::ZERO),
                };
                PlanRow {
                    name: class_name(c),
                    color: class_color(c),
                    bought,
                    amount,
                    note,
                    weight: format!(
                        "{} → target {}",
                        format::percent(after(c).share),
                        target_text(after(c).target_share)
                    ),
                }
            })
            .collect();
        let radii = ["3px 1px 1px 3px", "1px", "1px 3px 3px 1px"];
        let after_bar = AssetClass::ALL
            .into_iter()
            .zip(radii)
            .map(|(c, radius)| AfterSegment {
                width: format::css_percent(after(c).share),
                color: class_color(c),
                radius,
            })
            .collect();
        let after_label = AssetClass::ALL
            .into_iter()
            .map(|c| format!("{} {}", class_name(c), format::percent(after(c).share)))
            .collect::<Vec<_>>()
            .join(" · ");
        let vwce = t.class_buy(AssetClass::Vwce);
        let ikze = ctx.ikze_room.map(|(year, room)| {
            if vwce <= room {
                IkzeNote {
                    text: format!(
                        "Fits your remaining IKZE room for {year} ({}).",
                        format::pln(room)
                    ),
                    warn: false,
                }
            } else {
                IkzeNote {
                    text: format!(
                        "VWCE part exceeds this year’s IKZE room by {} — the rest would go to a \
                         regular account.",
                        format::pln(vwce - room)
                    ),
                    warn: true,
                }
            }
        });
        Plan {
            rows,
            after: after_bar,
            after_label,
            ikze,
        }
    }
}

/// Why there is no plan for what was typed or asked.
pub fn plan_error(e: &TopUpError) -> String {
    match e {
        TopUpError::AmountNotPositive(_) => "Enter an amount above zero.".into(),
        TopUpError::AllPaused => {
            "Every class is paused. Tick “Include paused assets” to plan anyway.".into()
        }
        TopUpError::Allocation(e) => format!("The plan cannot be worked out: {e}."),
    }
}

pub struct Chip {
    /// "2 000 zł".
    pub label: String,
    /// Without htmx: the amount with the checkbox as the page was rendered.
    pub href: String,
    /// With htmx: the amount alone; the checkbox as it is now is sent with it.
    pub hx_get: String,
}

/// The top-up plan card. Under htmx the input and the checkbox swap `#plan-result` and the
/// chips the whole card.
#[derive(Template)]
#[template(path = "allocation_plan.html")]
pub struct PlanPanel {
    /// What the input shows.
    pub amount: String,
    pub include_paused: bool,
    pub chips: Vec<Chip>,
    /// The plan, or why there is none for this amount.
    pub result: Result<Plan, String>,
}

impl PlanPanel {
    pub fn new(
        values: &BTreeMap<Sleeve, Decimal>,
        config: &Config,
        asked: Option<&str>,
        include_paused: bool,
        ctx: PlanContext,
    ) -> PlanPanel {
        let parsed = match asked {
            None => Ok(DEFAULT_AMOUNT),
            Some(text) => match parse_amount(text) {
                None => Err("Enter an amount in złoty, such as 2 000.".to_owned()),
                Some(a) if a > MAX_AMOUNT => Err(format!(
                    "Enter at most {}{}zł.",
                    format::number(MAX_AMOUNT, 0),
                    format::NBSP
                )),
                Some(a) => Ok(a),
            },
        };
        let amount = match (&parsed, asked) {
            (Ok(a), _) => amount_text(*a),
            (Err(_), Some(text)) => text.to_owned(),
            (Err(_), None) => String::new(),
        };
        let paused_param = if include_paused { "&paused=on" } else { "" };
        PlanPanel {
            amount,
            include_paused,
            chips: CHIPS
                .into_iter()
                .map(|v| {
                    let hx_get = format!("{}?amount={v}", Page::Allocation.path());
                    Chip {
                        label: format!("{}{}zł", format::number(v.into(), 0), format::NBSP),
                        href: format!("{hx_get}{paused_param}"),
                        hx_get,
                    }
                })
                .collect(),
            result: parsed.and_then(|a| {
                top_up(values, config, a, include_paused)
                    .map(|t| Plan::new(&t, config, include_paused, ctx))
                    .map_err(|e| plan_error(&e))
            }),
        }
    }
}

/// Everything under the header, when the holdings can be valued.
pub struct AllocationBody {
    /// The bars and the drift table, or why there are none (nothing held yet).
    pub drift: Result<DriftView, String>,
    /// The rendered [`PlanPanel`].
    pub panel: String,
}

#[derive(Template)]
#[template(path = "allocation.html")]
pub struct AllocationPage {
    pub shell: Shell,
    /// "Total 11 500,00 zł · no selling, top-ups only".
    pub caption: String,
    pub body: Result<AllocationBody, String>,
}

/// Why the shares cannot be shown, in the page's words.
pub fn values_error(e: &AllocationError) -> String {
    match e {
        AllocationError::Unpriced(s) => format!(
            "No price for {} right now, so the shares and the top-up plan would be wrong; they \
             are shown again once the price is back.",
            s.join(", ")
        ),
        AllocationError::Unclassified(s) => format!(
            "{} has no target in the config, so the shares cannot be worked out.",
            s.join(", ")
        ),
        AllocationError::NothingHeld => {
            "Nothing is held yet, so there is no allocation to compare with the targets.".into()
        }
    }
}

struct Loaded {
    values: BTreeMap<Sleeve, Decimal>,
    allocation: Result<Allocation, AllocationError>,
    wrappers: String,
    ctx: PlanContext,
}

/// The holdings valued today, as sleeves, or why they cannot be (a missing price or rate).
async fn load(state: &AppState, config: &Config) -> Result<Result<Loaded, String>, AppError> {
    let s = state.sources();
    let today = s.clock.today;
    let ledger = load_ledger(&state.pool).await?;
    let mut needed = requirements(&ledger, today);
    // The page shows no benchmark: asking for its VWCE days could spend Alpha Vantage requests.
    needed.benchmark_days.clear();
    let inputs = fetch_inputs(&needed, today, &s).await;
    for p in &inputs.problems {
        eprintln!("allocation: {p}");
    }
    let report = match report(&ledger, today, &inputs) {
        Ok(r) => r,
        Err(e) => return Ok(Err(format!("The portfolio cannot be valued: {e}."))),
    };
    let values = match sleeve_values(&report.holdings, &report.valuation, sleeve) {
        Ok(v) => v,
        Err(e) => return Ok(Err(values_error(&e))),
    };
    let held_coins = list_coins(&state.pool)
        .await?
        .into_iter()
        .filter(|c| c.purchased_on <= today)
        .count();
    let coin_value = report
        .valuation
        .by_asset
        .get(GOLD_COINS)
        .filter(|_| held_coins > 0)
        .map(|v| *v / Decimal::from(held_coins));
    let ctx = PlanContext {
        ikze_room: report.ikze(config).ok().map(|y| (y.year, y.remaining)),
        coin_value,
    };
    Ok(Ok(Loaded {
        allocation: allocation_of(&values, config),
        values,
        wrappers: vwce_wrappers(&ledger),
        ctx,
    }))
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

pub async fn show(
    State(state): State<AppState>,
    Extension(config): Extension<Arc<Config>>,
    headers: HeaderMap,
    Query(q): Query<AllocationQuery>,
) -> Result<Response, AppError> {
    let include_paused = q.paused.as_deref() == Some("on");
    let loaded = load(&state, &config).await?;
    let panel = |l: &Loaded| {
        PlanPanel::new(
            &l.values,
            &config,
            q.amount.as_deref(),
            include_paused,
            l.ctx,
        )
    };
    if is_htmx(&headers)
        && let Ok(l) = &loaded
    {
        return Ok(Html(panel(l).render()?).into_response());
    }
    let mut caption = "No selling, top-ups only".to_owned();
    let body = match loaded {
        Ok(l) => {
            if let Ok(a) = &l.allocation {
                caption = format!("Total {} · no selling, top-ups only", format::pln(a.total));
            }
            Ok(AllocationBody {
                drift: l
                    .allocation
                    .as_ref()
                    .map(|a| DriftView::new(a, &config, &l.wrappers))
                    .map_err(values_error),
                panel: panel(&l).render()?,
            })
        }
        Err(e) => Err(e),
    };
    let shell = Shell::load(&state, &headers, Page::Allocation).await?;
    let page = AllocationPage {
        shell,
        caption,
        body,
    };
    Ok(Html(page.render()?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn plain(s: &str) -> String {
        s.replace(format::NBSP, " ").replace(format::MINUS, "-")
    }

    fn config(paused: &str) -> Config {
        Config::from_toml(&format!(
            r#"
            [targets]
            vwce = 55
            crypto = 25
            gold = 20
            [targets.crypto_split]
            btc = 75
            eth = 25
            [paused]
            {paused}
            [ikze.limits]
            2026 = "11304.00"
            [tax]
            rate_percent = 12
            [gold]
            buyback_factor = "0.95"
            "#
        ))
        .unwrap()
    }

    /// The design's holdings: VWCE 6100, BTC 2380, ETH 920 (crypto 3300), gold 2100; 11 500.
    fn design_values() -> BTreeMap<Sleeve, Decimal> {
        BTreeMap::from([
            (Sleeve::Vwce, dec!(6100)),
            (Sleeve::Btc, dec!(2380)),
            (Sleeve::Eth, dec!(920)),
            (Sleeve::Gold, dec!(2100)),
        ])
    }

    const DESIGN_CTX: PlanContext = PlanContext {
        ikze_room: Some((2026, dec!(5652))),
        coin_value: Some(dec!(2100)),
    };

    #[test]
    fn amounts_are_read_as_typed() {
        assert_eq!(parse_amount("2 000"), Some(dec!(2000)));
        assert_eq!(parse_amount("2\u{a0}000,50 zł"), Some(dec!(2000.50)));
        assert_eq!(parse_amount(" 942.5 "), Some(dec!(942.5)));
        assert_eq!(parse_amount("1\u{202f}000"), Some(dec!(1000)));
        // Read, and turned down later as not above zero.
        assert_eq!(parse_amount("-5"), Some(dec!(-5)));
        assert_eq!(parse_amount("0"), Some(dec!(0)));
        for bad in [
            "", "zł", "-", "abc", "1,2,3", "1.000,50", "10,555", "1e5", "+5",
        ] {
            assert_eq!(parse_amount(bad), None, "{bad}");
        }
        assert_eq!(amount_text(dec!(2000)), "2\u{a0}000");
        assert_eq!(amount_text(dec!(2826.00)), "2\u{a0}826");
        assert_eq!(amount_text(dec!(2000.5)), "2\u{a0}000,50");
    }

    #[test]
    fn drift_bars_are_ten_percent_a_point_up_to_the_half() {
        // −2,0 pp: 20% of the track, growing left from the middle.
        let under = drift_bar(dec!(-0.02));
        assert_eq!((under.anchor, under.width.as_str()), ("right", "20%"));
        // +3,7 pp: 37%, growing right.
        let over = drift_bar(dec!(0.037));
        assert_eq!((over.anchor, over.width.as_str()), ("left", "37%"));
        // 8 pp would be 80%: the half is the most.
        assert_eq!(drift_bar(dec!(-0.08)).width, "50%");
        assert_eq!(drift_bar(Decimal::ZERO).width, "0%");
    }

    #[test]
    fn the_drift_table_of_the_design() {
        let config = config("crypto = true\ngold = true");
        let a = allocation_of(&design_values(), &config).unwrap();
        let v = DriftView::new(&a, &config, "IKZE");

        // 6100 / 11 500 = 53,04%; 3300 -> 28,70%; 2100 -> 18,26%.
        assert_eq!(
            v.actual_label,
            "Actual: VWCE 53,0%, Crypto 28,7%, Gold 18,3%"
        );
        assert_eq!(v.target_label, "Target: VWCE 55%, Crypto 25%, Gold 20%");
        let widths: Vec<(&str, &str)> = v
            .bar
            .iter()
            .map(|s| (s.width.as_str(), s.target_width.as_str()))
            .collect();
        assert_eq!(widths, [("53%", "55%"), ("28.7%", "25%"), ("18.3%", "20%")]);

        let row = |i: usize| {
            let r = &v.rows[i];
            (
                r.name,
                plain(&r.sub),
                r.cells.target.as_str(),
                r.cells.actual.as_str(),
                plain(&r.cells.drift),
                plain(&r.cells.pln),
            )
        };
        // VWCE: 6100 - 0,55 x 11 500 = 6100 - 6325 = -225; 53,04 - 55 = -1,96 pp.
        assert_eq!(
            row(0),
            (
                "VWCE",
                "Buying · IKZE".into(),
                "55,0%",
                "53,0%",
                "▼ -2,0 pp".into(),
                "-225,00 zł".into()
            )
        );
        // Crypto: 3300 - 2875 = +425; 28,70 - 25 = +3,70 pp.
        assert_eq!(
            row(1),
            (
                "Crypto",
                "Purchases paused · Kraken".into(),
                "25,0%",
                "28,7%",
                "▲ +3,7 pp".into(),
                "+425,00 zł".into()
            )
        );
        // Gold: 2100 - 2300 = -200; 18,26 - 20 = -1,74 pp.
        assert_eq!(
            row(2),
            (
                "Gold",
                "Purchases paused · coins".into(),
                "20,0%",
                "18,3%",
                "▼ -1,7 pp".into(),
                "-200,00 zł".into()
            )
        );
        assert!(v.rows[1].paused && !v.rows[0].paused);
        assert_eq!(v.rows[1].cells.bar.width, "37%");

        // Inside crypto: BTC 2380 / 3300 = 72,12%, 2380 - 0,75 x 3300 = 2380 - 2475 = -95;
        // ETH 920 / 3300 = 27,88%, +95.
        let coins: Vec<(&str, &str, String, String)> = v.rows[1]
            .coins
            .iter()
            .map(|c| {
                (
                    c.symbol,
                    c.cells.actual.as_str(),
                    plain(&c.cells.drift),
                    plain(&c.cells.pln),
                )
            })
            .collect();
        assert_eq!(
            coins,
            [
                ("BTC", "72,1%", "▼ -2,9 pp".into(), "-95,00 zł".into()),
                ("ETH", "27,9%", "▲ +2,9 pp".into(), "+95,00 zł".into())
            ]
        );
        assert!(v.rows[0].coins.is_empty() && v.rows[2].coins.is_empty());
    }

    #[test]
    fn vwce_held_nowhere_known_is_at_xtb_and_no_crypto_has_no_coin_rows() {
        let config = config("");
        let values = BTreeMap::from([(Sleeve::Vwce, dec!(1000)), (Sleeve::Gold, dec!(1000))]);
        let a = allocation_of(&values, &config).unwrap();
        let v = DriftView::new(&a, &config, "");
        assert_eq!(v.rows[0].sub, "Buying · XTB");
        assert_eq!(v.rows[1].sub, "Buying · Kraken");
        assert!(v.rows[1].coins.is_empty());
        // No crypto: 0% of the 25% target, 25 points under, the bar at its half.
        assert_eq!(v.rows[1].cells.actual, "0,0%");
        assert_eq!(v.rows[1].cells.bar.width, "50%");
    }

    #[test]
    fn the_plan_of_the_design() {
        // 2000 more: the base is 13 500. VWCE needs 0,55 x 13 500 - 6100 = 1325, crypto (paused)
        // 0,25 x 13 500 - 3300 = 75, gold (paused) 0,20 x 13 500 - 2100 = 600. Only VWCE is
        // bought: its need and the 675 left over, all 2000.
        let config = config("crypto = true\ngold = true");
        let t = top_up(&design_values(), &config, dec!(2000), false).unwrap();
        let p = Plan::new(&t, &config, false, DESIGN_CTX);

        let rows: Vec<(&str, bool, String, String, String)> = p
            .rows
            .iter()
            .map(|r| {
                (
                    r.name,
                    r.bought,
                    plain(&r.amount),
                    plain(&r.note),
                    r.weight.clone(),
                )
            })
            .collect();
        // After: VWCE 8100 / 13 500 = 60,0%, crypto 3300 -> 24,4%, gold 2100 -> 15,6%.
        assert_eq!(
            rows,
            [
                (
                    "VWCE",
                    true,
                    "+2 000,00 zł".into(),
                    "XTB · counts toward IKZE".into(),
                    "60,0% → target 55%".into()
                ),
                (
                    "Crypto",
                    false,
                    "—".into(),
                    "Paused · would need 75,00 zł".into(),
                    "24,4% → target 25%".into()
                ),
                (
                    "Gold",
                    false,
                    "—".into(),
                    "Paused · would need 600,00 zł".into(),
                    "15,6% → target 20%".into()
                ),
            ]
        );
        assert_eq!(p.after_label, "VWCE 60,0% · Crypto 24,4% · Gold 15,6%");
        let widths: Vec<&str> = p.after.iter().map(|s| s.width.as_str()).collect();
        assert_eq!(widths, ["60%", "24.4%", "15.6%"]);
        // 2000 of VWCE fits the 5652 left of the 2026 limit.
        assert_eq!(
            p.ikze.map(|n| (plain(&n.text), n.warn)),
            Some((
                "Fits your remaining IKZE room for 2026 (5 652,00 zł).".into(),
                false
            ))
        );
    }

    #[test]
    fn the_plan_with_the_paused_classes() {
        // Same 2000 with every class: the needs 1325 + 75 + 600 add up to exactly 2000, so each
        // gets its need and all three end on target. Crypto after: 3375; BTC needs 0,75 x 3375
        // - 2380 = 151,25, ETH 843,75 - 920 < 0: the 75 all goes to BTC.
        let config = config("crypto = true\ngold = true");
        let t = top_up(&design_values(), &config, dec!(2000), true).unwrap();
        let p = Plan::new(&t, &config, true, DESIGN_CTX);

        let rows: Vec<(String, String, String)> = p
            .rows
            .iter()
            .map(|r| (plain(&r.amount), plain(&r.note), r.weight.clone()))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "+1 325,00 zł".into(),
                    "XTB · counts toward IKZE".into(),
                    "55,0% → target 55%".into()
                ),
                (
                    "+75,00 zł".into(),
                    "BTC 75,00 zł · ETH 0,00 zł".into(),
                    "25,0% → target 25%".into()
                ),
                (
                    "+600,00 zł".into(),
                    "Whole coins only (~2 100 zł) — set aside".into(),
                    "20,0% → target 20%".into()
                ),
            ]
        );
        assert!(p.rows.iter().all(|r| r.bought));
    }

    #[test]
    fn a_vwce_part_over_the_ikze_room_is_flagged() {
        // 5000, only VWCE bought: 5000 against 1500 left: 3500 over.
        let config = config("crypto = true\ngold = true");
        let t = top_up(&design_values(), &config, dec!(5000), false).unwrap();
        let ctx = PlanContext {
            ikze_room: Some((2026, dec!(1500))),
            coin_value: None,
        };
        let note = Plan::new(&t, &config, false, ctx).ikze.unwrap();
        assert_eq!(
            plain(&note.text),
            "VWCE part exceeds this year’s IKZE room by 3 500,00 zł — the rest would go to a \
             regular account."
        );
        assert!(note.warn);
        // No limit for the year in the config: no note at all.
        let no_limit = PlanContext::default();
        assert_eq!(Plan::new(&t, &config, false, no_limit).ikze, None);
    }

    #[test]
    fn an_overweight_class_that_is_bought_gets_nothing() {
        // Nothing paused, 100 more on 11 500: the base is 11 600 and crypto (3300) is already
        // over its 2900; the 100 goes to VWCE (need 6380 - 6100 = 280) and gold (2320 - 2100
        // = 220) pro rata: 100 x 280 / 500 = 56 and 44.
        let config = config("");
        let t = top_up(&design_values(), &config, dec!(100), false).unwrap();
        let p = Plan::new(&t, &config, false, PlanContext::default());
        let amounts: Vec<String> = p.rows.iter().map(|r| plain(&r.amount)).collect();
        assert_eq!(amounts, ["+56,00 zł", "0,00 zł", "+44,00 zł"]);
        assert_eq!(p.rows[2].note, "Whole coins only — set aside");
    }

    #[test]
    fn the_panel_reads_the_amount_and_keeps_the_checkbox_in_the_chips() {
        let cfg = config("crypto = true\ngold = true");
        let values = design_values();

        let default = PlanPanel::new(&values, &cfg, None, false, DESIGN_CTX);
        assert_eq!(plain(&default.amount), "2 000");
        assert!(default.result.is_ok());
        let chips: Vec<(String, &str)> = default
            .chips
            .iter()
            .map(|c| (plain(&c.label), c.href.as_str()))
            .collect();
        assert_eq!(
            chips,
            [
                ("942 zł".into(), "/allocation?amount=942"),
                ("2 000 zł".into(), "/allocation?amount=2000"),
                ("5 000 zł".into(), "/allocation?amount=5000"),
            ]
        );
        let paused = PlanPanel::new(&values, &cfg, Some("942"), true, DESIGN_CTX);
        assert_eq!(paused.chips[0].href, "/allocation?amount=942&paused=on");
        assert_eq!(paused.chips[0].hx_get, "/allocation?amount=942");

        let typo = PlanPanel::new(&values, &cfg, Some("2 0x0"), false, DESIGN_CTX);
        assert_eq!(typo.amount, "2 0x0");
        assert_eq!(
            typo.result.err().as_deref(),
            Some("Enter an amount in złoty, such as 2 000.")
        );
        let zero = PlanPanel::new(&values, &cfg, Some("0"), false, DESIGN_CTX);
        assert_eq!(
            zero.result.err().as_deref(),
            Some("Enter an amount above zero.")
        );
        let all_paused = config("vwce = true\ncrypto = true\ngold = true");
        let none = PlanPanel::new(&values, &all_paused, None, false, DESIGN_CTX);
        assert_eq!(
            none.result.err().as_deref(),
            Some("Every class is paused. Tick “Include paused assets” to plan anyway.")
        );
    }

    #[test]
    fn a_huge_amount_is_turned_down_instead_of_overflowing() {
        // With every class bought, 10^15 x VWCE's need of about 5,5 x 10^14 is past Decimal's
        // ~7,9 x 10^28, and Decimal::MAX plus the 11 500 held overflows the base.
        let cfg = config("crypto = true\ngold = true");
        let values = design_values();
        for (typed, paused) in [
            ("1 000 000 000 000 000", true),
            ("79228162514264337593543950335", false),
        ] {
            let p = PlanPanel::new(&values, &cfg, Some(typed), paused, DESIGN_CTX);
            assert_eq!(
                p.result.err().as_deref().map(plain),
                Some("Enter at most 1 000 000 000 zł.".into()),
                "{typed}"
            );
            assert_eq!(p.amount, typed);
        }
        // The cap itself is still planned.
        let cap = PlanPanel::new(&values, &cfg, Some("1000000000"), true, DESIGN_CTX);
        assert!(cap.result.is_ok());
    }

    #[test]
    fn missing_prices_are_said_instead_of_shares() {
        assert_eq!(
            values_error(&AllocationError::Unpriced(vec![
                "VWCE".into(),
                "BTC".into()
            ])),
            "No price for VWCE, BTC right now, so the shares and the top-up plan would be \
             wrong; they are shown again once the price is back."
        );
        assert_eq!(
            values_error(&AllocationError::NothingHeld),
            "Nothing is held yet, so there is no allocation to compare with the targets."
        );
    }
}
