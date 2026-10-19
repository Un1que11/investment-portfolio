//! The IKZE page (`design/screens/05`): this year's contributions against the limit with the
//! rest hatched, a reminder to top up before 31 December with an even split of what is left,
//! the estimated tax deduction at 12% or 32%, and the contributions by month with the months
//! still to top up drawn dashed.
//!
//! The figures come from `core` ([`portfolio_core::ikze`]); no price is needed, only the NBP rate
//! of a deposit made in another currency. The 12% / 32% switch re-renders the deduction card
//! through htmx and also works as a plain link (`/ikze?rate=32`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use chrono::{Datelike, NaiveDate};
use portfolio_core::config::Config;
use portfolio_core::ikze::{
    IkzeYear, TopUpReminder, ikze_flows, ikze_year, monthly_contributions, top_up_reminder,
};
use portfolio_db::transactions::load_ledger;
use portfolio_sources::fx::rate_on_day;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::base::{Page, Shell};
use crate::format;
use crate::{AppError, AppState};

/// The brackets the deduction card can switch between, in percent.
pub const BRACKETS: [u32; 2] = [12, 32];

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const MONTH_NAMES: [&str; 12] = [
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

const COUNTS: [&str; 12] = [
    "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten", "Eleven",
    "Twelve",
];

/// Height in px of the tallest month bar (the reference's 112).
const BAR_MAX_PX: Decimal = Decimal::from_parts(112, 0, 0, false, 0);
/// The lowest bar still drawn as a bar.
const BAR_MIN_PX: Decimal = Decimal::from_parts(4, 0, 0, false, 0);

#[derive(Debug, Default, Deserialize)]
pub struct IkzeQuery {
    /// "12" or "32"; anything else means the rate in the config.
    pub rate: Option<String>,
}

/// The bracket in percent: the one asked for when it is 12 or 32, the config's otherwise.
pub fn bracket(asked: Option<&str>, config_percent: Decimal) -> Decimal {
    asked
        .and_then(|a| BRACKETS.into_iter().find(|b| a == b.to_string()))
        .map_or(config_percent, Decimal::from)
}

/// The used part of the limit bar and its caption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitBar {
    /// "5 652,00", before the small "zł".
    pub contributed: String,
    /// "11 304,00 zł".
    pub limit: String,
    pub remaining: String,
    /// CSS width of the paid part; the hatched rest starts there.
    pub used_width: String,
    pub rest_width: String,
    /// aria-valuenow / aria-valuemax: whole złoty.
    pub now: String,
    pub max: String,
    /// "50,0% used".
    pub caption: String,
    /// "50,0% of the 2026 limit".
    pub value_text: String,
    /// Paid in above the limit (not deductible).
    pub over: Option<String>,
    /// Taken out this year (it frees none of the limit).
    pub withdrawn: Option<String>,
}

impl LimitBar {
    pub fn new(y: &IkzeYear) -> LimitBar {
        let used = y.deductible / y.limit;
        let whole = |v: Decimal| v.round().normalize().to_string();
        LimitBar {
            contributed: format::number(y.contributed, 2),
            limit: format::pln(y.limit),
            remaining: format::pln(y.remaining),
            used_width: format::css_percent(used),
            rest_width: format::css_percent(Decimal::ONE - used),
            now: whole(y.deductible),
            max: whole(y.limit),
            caption: format!("{} used", format::percent(used)),
            value_text: format!("{} of the {} limit", format::percent(used), y.year),
            over: (y.over_limit > Decimal::ZERO).then(|| {
                format!(
                    "{} paid in above the limit is not deductible",
                    format::pln(y.over_limit)
                )
            }),
            withdrawn: (y.withdrawn > Decimal::ZERO).then(|| {
                format!(
                    "{} withdrawn this year; it frees none of the limit",
                    format::pln(y.withdrawn)
                )
            }),
        }
    }
}

/// The top-up reminder banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// "Top up before 31 Dec · 76 days left".
    pub title: String,
    pub text: String,
    /// "Plan a top-up" goes to the Allocation page once it is built (P22).
    pub plan_href: Option<&'static str>,
}

/// "November and December", "October, November and December", or "one a month from March to
/// December" for more than three months.
fn months_phrase(months: &[u32]) -> String {
    let name = |m: u32| MONTH_NAMES[m as usize - 1];
    match months {
        [] => String::new(),
        [m] => format!("in {}", name(*m)),
        [first @ .., last] if months.len() <= 3 => {
            let firsts: Vec<&str> = first.iter().map(|m| name(*m)).collect();
            format!("in {} and {}", firsts.join(", "), name(*last))
        }
        [first, .., last] => format!("one a month from {} to {}", name(*first), name(*last)),
    }
}

impl Banner {
    pub fn new(r: &TopUpReminder, y: &IkzeYear) -> Banner {
        let title = match r.days_left {
            0 => "Top up by today, 31 Dec".to_owned(),
            1 => "Top up before 31 Dec · 1 day left".to_owned(),
            n => format!("Top up before 31 Dec · {n} days left"),
        };
        let pace = match (r.pace, r.projected) {
            (Some(p), Some(end)) if end >= y.limit => {
                format!("At {} a month you’ll reach the limit.", format::pln(p))
            }
            (Some(p), Some(end)) => format!(
                "At {} a month you’ll reach {}.",
                format::pln(p),
                format::pln(end)
            ),
            _ => "Nothing has been paid in this year yet.".to_owned(),
        };
        let n = r.months.len();
        let split = if n == 1 {
            format!(
                "One top-up of {} {} fills the limit.",
                format::pln(r.each),
                months_phrase(&r.months)
            )
        } else if n <= 3 {
            format!(
                "{} top-ups of {} {} fill the limit.",
                COUNTS[n - 1],
                format::pln(r.each),
                months_phrase(&r.months)
            )
        } else {
            format!(
                "{} top-ups of {}, {}, fill the limit.",
                COUNTS[n - 1],
                format::pln(r.each),
                months_phrase(&r.months)
            )
        };
        Banner {
            title,
            text: format!("{pace} {split}"),
            plan_href: Page::Allocation.built().then_some(Page::Allocation.path()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    /// Paid in: solid accent.
    Paid,
    /// Still to top up to max out: dashed outline.
    Planned,
    /// Nothing paid and nothing planned: a thin line.
    Empty,
}

impl BarKind {
    pub fn style(self) -> &'static str {
        match self {
            BarKind::Paid => "background: var(--accent)",
            BarKind::Planned => "border: 1.5px dashed var(--accent); background: transparent",
            BarKind::Empty => "background: var(--line)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthBar {
    pub label: &'static str,
    /// "942", "2 826", or "—".
    pub value: String,
    /// "37.3px".
    pub height: String,
    pub kind: BarKind,
    pub current: bool,
}

/// One bar per month: what was paid in, or (from the reminder) what each remaining month needs
/// to max out the limit. Heights are relative to the biggest bar, as in the reference: v / max
/// x 112px, at least 4px; an empty month is a 2px line.
pub fn month_bars(
    monthly: &[Decimal; 12],
    reminder: Option<&TopUpReminder>,
    today: NaiveDate,
) -> Vec<MonthBar> {
    let shown: Vec<(BarKind, Decimal)> = (0..12)
        .map(|i| {
            let planned = reminder.filter(|r| r.months.contains(&(i as u32 + 1)));
            match (monthly[i], planned) {
                (paid, _) if paid > Decimal::ZERO => (BarKind::Paid, paid),
                (_, Some(r)) if r.each > Decimal::ZERO => (BarKind::Planned, r.each),
                _ => (BarKind::Empty, Decimal::ZERO),
            }
        })
        .collect();
    let max = shown.iter().map(|(_, v)| *v).max().unwrap_or_default();
    shown
        .into_iter()
        .enumerate()
        .map(|(i, (kind, v))| {
            let height = if kind == BarKind::Empty || max.is_zero() {
                Decimal::TWO
            } else {
                (v / max * BAR_MAX_PX).max(BAR_MIN_PX).round_dp(1)
            };
            MonthBar {
                label: MONTHS[i],
                value: if kind == BarKind::Empty {
                    "—".into()
                } else {
                    format::number(v, 0)
                },
                height: format!("{}px", height.normalize()),
                kind,
                current: i == today.month0() as usize,
            }
        })
        .collect()
}

pub struct BracketLink {
    pub label: String,
    pub href: String,
    pub current: bool,
}

/// The deduction card: what the contributions so far save, and what the whole limit would.
#[derive(Template)]
#[template(path = "ikze_tax.html")]
pub struct TaxCard {
    pub brackets: Vec<BracketLink>,
    /// "+678,24 zł".
    pub so_far: String,
    pub at_max: String,
    pub note: String,
}

impl TaxCard {
    pub fn new(y: &IkzeYear, percent: Decimal) -> TaxCard {
        let rate = percent / Decimal::ONE_HUNDRED;
        let p = percent.normalize();
        TaxCard {
            brackets: BRACKETS
                .into_iter()
                .map(|b| BracketLink {
                    label: format!("{b}%"),
                    href: format!("/ikze?rate={b}"),
                    current: Decimal::from(b) == percent,
                })
                .collect(),
            so_far: format::signed_pln(y.deductible * rate),
            at_max: format::signed_pln(y.limit * rate),
            note: format!(
                "Contributions are deducted from your {} income in the PIT you file in {}; at \
                 the {p}% bracket each 100{nb}zł paid in returns {p}{nb}zł. Withdrawals after 65 \
                 are taxed at 10%.",
                y.year,
                y.year + 1,
                nb = format::NBSP
            ),
        }
    }
}

/// Everything under the header, when the year can be shown.
pub struct IkzeBody {
    pub limit: LimitBar,
    pub banner: Option<Banner>,
    /// The rendered [`TaxCard`].
    pub tax: String,
    pub months: Vec<MonthBar>,
}

#[derive(Template)]
#[template(path = "ikze.html")]
pub struct IkzePage {
    pub shell: Shell,
    pub year: i32,
    /// The year's figures, or why they cannot be shown.
    pub body: Result<IkzeBody, String>,
}

/// The year of `today`: its contributions by month and against the limit. Deposits in another
/// currency are converted at the NBP rate of their day.
async fn load(
    state: &AppState,
    config: &Config,
) -> Result<Result<(IkzeYear, [Decimal; 12]), String>, AppError> {
    let today = (state.today)();
    let ledger = load_ledger(&state.pool).await?;
    let mut needed: BTreeSet<(String, NaiveDate)> = BTreeSet::new();
    // Ask the calculation which rates it needs (deposits and withdrawals alike).
    let _ = ikze_flows(&ledger, today, |currency, day| {
        needed.insert((currency.to_owned(), day));
        Some(Decimal::ONE)
    });
    let mut rates: BTreeMap<(String, NaiveDate), Decimal> = BTreeMap::new();
    for (currency, day) in needed {
        match rate_on_day(&state.pool, &state.market.nbp, &currency, day, today).await {
            Ok(r) => {
                rates.insert((currency, day), r.mid);
            }
            Err(e) => eprintln!("ikze: {currency} on {day}: {e}"),
        }
    }
    let rate = |currency: &str, day: NaiveDate| rates.get(&(currency.to_owned(), day)).copied();
    let flows = match ikze_flows(&ledger, today, rate) {
        Ok(f) => f,
        Err(e) => return Ok(Err(format!("The contributions cannot be added up: {e}."))),
    };
    let year = match ikze_year(&flows, config) {
        Ok(y) => y,
        Err(e) => {
            return Ok(Err(format!(
                "The {} figures cannot be shown: {e}.",
                flows.year
            )));
        }
    };
    let monthly = monthly_contributions(&ledger, today, rate)
        .expect("the same deposits as ikze_flows, whose rates are all there");
    Ok(Ok((year, monthly)))
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

pub async fn show(
    State(state): State<AppState>,
    Extension(config): Extension<Arc<Config>>,
    headers: HeaderMap,
    Query(q): Query<IkzeQuery>,
) -> Result<Response, AppError> {
    let today = (state.today)();
    let percent = bracket(q.rate.as_deref(), config.tax_rate_percent());
    let loaded = load(&state, &config).await?;
    if is_htmx(&headers)
        && let Ok((year, _)) = &loaded
    {
        return Ok(Html(TaxCard::new(year, percent).render()?).into_response());
    }
    let body = match loaded {
        Ok((year, monthly)) => {
            let reminder = top_up_reminder(&year, &monthly, today);
            Ok(IkzeBody {
                limit: LimitBar::new(&year),
                banner: reminder.as_ref().map(|r| Banner::new(r, &year)),
                tax: TaxCard::new(&year, percent).render()?,
                months: month_bars(&monthly, reminder.as_ref(), today),
            })
        }
        Err(e) => Err(e),
    };
    let shell = Shell::load(&state, &headers, Page::Ikze).await?;
    let page = IkzePage {
        shell,
        year: today.year(),
        body,
    };
    Ok(Html(page.render()?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn plain(s: &str) -> String {
        s.replace(format::NBSP, " ").replace(format::MINUS, "-")
    }

    fn year(contributed: Decimal, withdrawn: Decimal) -> IkzeYear {
        let limit = dec!(11304);
        let deductible = contributed.min(limit);
        IkzeYear {
            year: 2026,
            contributed,
            withdrawn,
            limit,
            remaining: limit - deductible,
            over_limit: contributed - deductible,
            deductible,
            tax_rate: dec!(0.12),
            tax_saving: deductible * dec!(0.12),
        }
    }

    /// 942 paid in each month May–Oct 2026, as in screens/05.
    fn design_months() -> [Decimal; 12] {
        let mut m = [dec!(0); 12];
        for month in &mut m[4..10] {
            *month = dec!(942);
        }
        m
    }

    fn design_reminder() -> TopUpReminder {
        TopUpReminder {
            days_left: 76,
            pace: Some(dec!(942)),
            projected: Some(dec!(7536)),
            months: vec![11, 12],
            each: dec!(2826),
        }
    }

    #[test]
    fn the_bracket_is_12_or_32_or_the_configs() {
        assert_eq!(bracket(Some("32"), dec!(12)), dec!(32));
        assert_eq!(bracket(Some("12"), dec!(32)), dec!(12));
        assert_eq!(bracket(Some("20"), dec!(12)), dec!(12));
        assert_eq!(bracket(None, dec!(32)), dec!(32));
    }

    #[test]
    fn the_limit_bar_of_the_design() {
        // 5652 of 11 304 = 50,0% used; the hatched rest is the other 50%.
        let b = LimitBar::new(&year(dec!(5652), dec!(0)));
        assert_eq!(plain(&b.contributed), "5 652,00");
        assert_eq!(plain(&b.limit), "11 304,00 zł");
        assert_eq!(plain(&b.remaining), "5 652,00 zł");
        assert_eq!(
            (b.used_width.as_str(), b.rest_width.as_str()),
            ("50%", "50%")
        );
        assert_eq!((b.now.as_str(), b.max.as_str()), ("5652", "11304"));
        assert_eq!(b.caption, "50,0% used");
        assert_eq!(b.value_text, "50,0% of the 2026 limit");
        assert_eq!((b.over, b.withdrawn), (None, None));
    }

    #[test]
    fn over_the_limit_the_bar_is_full_and_says_so() {
        // 12 000 paid in: the bar stops at 100%, 696 over is not deductible. 1000 withdrawn.
        let b = LimitBar::new(&year(dec!(12000), dec!(1000)));
        assert_eq!(
            (b.used_width.as_str(), b.rest_width.as_str()),
            ("100%", "0%")
        );
        assert_eq!(b.caption, "100,0% used");
        assert_eq!(plain(&b.contributed), "12 000,00");
        assert_eq!(plain(&b.remaining), "0,00 zł");
        assert_eq!(
            plain(&b.over.unwrap()),
            "696,00 zł paid in above the limit is not deductible"
        );
        assert_eq!(
            plain(&b.withdrawn.unwrap()),
            "1 000,00 zł withdrawn this year; it frees none of the limit"
        );
    }

    #[test]
    fn the_banner_of_the_design() {
        let b = Banner::new(&design_reminder(), &year(dec!(5652), dec!(0)));
        assert_eq!(b.title, "Top up before 31 Dec · 76 days left");
        assert_eq!(
            plain(&b.text),
            "At 942,00 zł a month you’ll reach 7 536,00 zł. Two top-ups of 2 826,00 zł in \
             November and December fill the limit."
        );
        // Allocation (P22) is not built yet, so there is no button to it.
        assert_eq!(b.plan_href, None);
    }

    #[test]
    fn the_banner_words_its_other_cases() {
        let y = year(dec!(4710), dec!(0));
        // Three months: listed. A pace that gets there: "the limit".
        let r = TopUpReminder {
            days_left: 1,
            pace: Some(dec!(3000)),
            projected: Some(dec!(13710)),
            months: vec![10, 11, 12],
            each: dec!(2198),
        };
        let b = Banner::new(&r, &y);
        assert_eq!(b.title, "Top up before 31 Dec · 1 day left");
        assert_eq!(
            plain(&b.text),
            "At 3 000,00 zł a month you’ll reach the limit. Three top-ups of 2 198,00 zł in \
             October, November and December fill the limit."
        );
        // Nothing paid yet, twelve months: a range. 11 304 / 12 = 942.
        let r = TopUpReminder {
            days_left: 355,
            pace: None,
            projected: None,
            months: (1..=12).collect(),
            each: dec!(942),
        };
        assert_eq!(
            plain(&Banner::new(&r, &year(dec!(0), dec!(0))).text),
            "Nothing has been paid in this year yet. Twelve top-ups of 942,00 zł, one a month \
             from January to December, fill the limit."
        );
        // The last day, one month.
        let r = TopUpReminder {
            days_left: 0,
            pace: Some(dec!(500)),
            projected: Some(dec!(4710)),
            months: vec![12],
            each: dec!(6594),
        };
        let b = Banner::new(&r, &y);
        assert_eq!(b.title, "Top up by today, 31 Dec");
        assert!(
            plain(&b.text).ends_with("One top-up of 6 594,00 zł in December fills the limit."),
            "{}",
            b.text
        );
    }

    #[test]
    fn month_bars_of_the_design() {
        let bars = month_bars(&design_months(), Some(&design_reminder()), d("2026-10-16"));
        let row = |i: usize| {
            let b = &bars[i];
            (
                b.label,
                plain(&b.value),
                b.height.as_str(),
                b.kind,
                b.current,
            )
        };
        // Jan–Apr nothing: a 2px line and a dash.
        assert_eq!(row(0), ("Jan", "—".into(), "2px", BarKind::Empty, false));
        // Paid 942 against the tallest 2826: 942 / 2826 x 112 = 37,33 -> 37.3px.
        assert_eq!(
            row(4),
            ("May", "942".into(), "37.3px", BarKind::Paid, false)
        );
        assert_eq!(row(9), ("Oct", "942".into(), "37.3px", BarKind::Paid, true));
        // Nov and Dec need 2826 each: full height, dashed.
        assert_eq!(
            row(10),
            ("Nov", "2 826".into(), "112px", BarKind::Planned, false)
        );
        assert_eq!(row(11).3, BarKind::Planned);
    }

    #[test]
    fn month_bars_without_a_reminder_and_tiny_months() {
        // Limit reached: no dashed months. 10 against 11 294 is 0,1px: drawn at the 4px minimum.
        let mut m = [dec!(0); 12];
        m[0] = dec!(11294);
        m[1] = dec!(10);
        let bars = month_bars(&m, None, d("2026-10-16"));
        assert_eq!(
            (bars[0].height.as_str(), bars[1].height.as_str()),
            ("112px", "4px")
        );
        assert!(bars[2..].iter().all(|b| b.kind == BarKind::Empty));
        // Nothing at all: every month is a line.
        let bars = month_bars(&[dec!(0); 12], None, d("2026-01-02"));
        assert!(bars.iter().all(|b| b.height == "2px" && b.value == "—"));
        assert!(bars[0].current);
    }

    #[test]
    fn tax_card_at_both_brackets() {
        // 5652 x 12% = 678,24; 11 304 x 12% = 1356,48 (screens/05). At 32%: 1808,64 and
        // 3617,28.
        let y = year(dec!(5652), dec!(0));
        let card = TaxCard::new(&y, dec!(12));
        assert_eq!(plain(&card.so_far), "+678,24 zł");
        assert_eq!(plain(&card.at_max), "+1 356,48 zł");
        assert_eq!(
            plain(&card.note),
            "Contributions are deducted from your 2026 income in the PIT you file in 2027; at \
             the 12% bracket each 100 zł paid in returns 12 zł. Withdrawals after 65 are taxed \
             at 10%."
        );
        let current: Vec<_> = card.brackets.iter().map(|b| b.current).collect();
        assert_eq!(current, [true, false]);
        assert_eq!(card.brackets[1].href, "/ikze?rate=32");

        let card = TaxCard::new(&y, dec!(32));
        assert_eq!(plain(&card.so_far), "+1 808,64 zł");
        assert_eq!(plain(&card.at_max), "+3 617,28 zł");
        // Above the limit only the limit is deductible: 11 304 x 12% either way.
        let card = TaxCard::new(&year(dec!(12000), dec!(0)), dec!(12));
        assert_eq!(card.so_far, card.at_max);
    }
}
