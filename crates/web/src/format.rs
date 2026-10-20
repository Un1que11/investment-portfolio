//! Number formatting for the pages, in the Polish style of `design/README.md`:
//! `11 500,00 zł` with a non-breaking space between thousands and before `zł`, a decimal comma,
//! `−` (U+2212) for minus, one decimal for percentages (`+14,0%`) and drift in `pp`.
//!
//! Amounts are rounded half away from zero. A signed figure takes its sign from the rounded
//! value, so -0.004 zł is shown as `±0,00 zł` in neutral grey rather than as a red `−0,00 zł`.

use chrono::{Days, NaiveDate, NaiveDateTime};
use rust_decimal::{Decimal, RoundingStrategy};

/// Between thousands and before units.
pub const NBSP: char = '\u{a0}';
/// The minus sign (not the ASCII hyphen).
pub const MINUS: char = '\u{2212}';

/// Whether a signed figure reads as a gain, a loss or neither. The pages pair it with a sign
/// and an arrow, never with colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Gain,
    Loss,
    Flat,
}

impl Tone {
    fn of(rounded: Decimal) -> Tone {
        if rounded.is_zero() {
            Tone::Flat
        } else if rounded.is_sign_negative() {
            Tone::Loss
        } else {
            Tone::Gain
        }
    }

    /// ▲ for a gain, ▼ for a loss, nothing when flat.
    pub fn arrow(self) -> &'static str {
        match self {
            Tone::Gain => "▲",
            Tone::Loss => "▼",
            Tone::Flat => "",
        }
    }

    /// The text colour token: `var(--gain)`, `var(--loss)`, or `var(--text-2)` when flat.
    pub fn color(self) -> &'static str {
        match self {
            Tone::Gain => "var(--gain)",
            Tone::Loss => "var(--loss)",
            Tone::Flat => "var(--text-2)",
        }
    }

    /// The badge background token.
    pub fn soft(self) -> &'static str {
        match self {
            Tone::Gain => "var(--gain-soft)",
            Tone::Loss => "var(--loss-soft)",
            Tone::Flat => "var(--surface-2)",
        }
    }
}

/// A signed figure ready for a delta badge: `text` carries the sign, `tone` the colour and arrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub text: String,
    pub tone: Tone,
}

impl Delta {
    pub fn arrow(&self) -> &'static str {
        self.tone.arrow()
    }
}

fn round(value: Decimal, dp: u32) -> Decimal {
    value.round_dp_with_strategy(dp, RoundingStrategy::MidpointAwayFromZero)
}

/// Digits of an already rounded value without its sign: "11 500,00".
fn digits(rounded: Decimal, dp: u32) -> String {
    let text = format!("{:.*}", dp as usize, rounded.abs());
    let (whole, fraction) = match text.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (text.as_str(), None),
    };
    let mut out = String::new();
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(NBSP);
        }
        out.push(digit);
    }
    if let Some(f) = fraction {
        out.push(',');
        out.push_str(f);
    }
    out
}

/// `value` with `dp` decimals: "11 500,00", "−312,40".
pub fn number(value: Decimal, dp: u32) -> String {
    let rounded = round(value, dp);
    let text = digits(rounded, dp);
    if Tone::of(rounded) == Tone::Loss {
        format!("{MINUS}{text}")
    } else {
        text
    }
}

/// Always signed: "+1 408,00", "−312,40", "±0,00".
pub fn signed_number(value: Decimal, dp: u32) -> String {
    let rounded = round(value, dp);
    let sign = match Tone::of(rounded) {
        Tone::Gain => '+',
        Tone::Loss => MINUS,
        Tone::Flat => '±',
    };
    format!("{sign}{}", digits(rounded, dp))
}

/// PLN to the grosz: "11 500,00 zł".
pub fn pln(value: Decimal) -> String {
    format!("{}{NBSP}zł", number(value, 2))
}

/// Signed PLN: "+1 408,00 zł", "−312,40 zł", "±0,00 zł".
pub fn signed_pln(value: Decimal) -> String {
    format!("{}{NBSP}zł", signed_number(value, 2))
}

/// A fraction as a percentage with one decimal: 0.327 -> "32,7%".
pub fn percent(fraction: Decimal) -> String {
    format!("{}%", number(fraction * Decimal::ONE_HUNDRED, 1))
}

/// A fraction as a signed percentage: 0.14 -> "+14,0%".
pub fn signed_percent(fraction: Decimal) -> String {
    format!("{}%", signed_number(fraction * Decimal::ONE_HUNDRED, 1))
}

/// A gain or loss in PLN for a delta badge.
pub fn delta_pln(value: Decimal) -> Delta {
    Delta {
        text: signed_pln(value),
        tone: Tone::of(round(value, 2)),
    }
}

/// A return as a signed percentage for a delta badge.
pub fn delta_percent(fraction: Decimal) -> Delta {
    Delta {
        text: signed_percent(fraction),
        tone: Tone::of(round(fraction * Decimal::ONE_HUNDRED, 1)),
    }
}

/// Drift from target, given as a difference of shares (0.03 = 3 points over), with an arrow:
/// "▲ +3,0 pp", "▼ −2,0 pp", "±0,0 pp". Drift is shown in neutral grey, so it has no tone.
pub fn drift_pp(points: Decimal) -> String {
    let pp = points * Decimal::ONE_HUNDRED;
    let arrow = Tone::of(round(pp, 1)).arrow();
    let text = format!("{}{NBSP}pp", signed_number(pp, 1));
    if arrow.is_empty() {
        text
    } else {
        format!("{arrow} {text}")
    }
}

/// A date as the pages show it: "02.10.2026".
pub fn date(day: NaiveDate) -> String {
    day.format("%d.%m.%Y").to_string()
}

/// A moment as the states sheet writes it (`screens/08`): "15 Oct, 22:00", "3 Jan, 08:05".
pub fn day_time(t: NaiveDateTime) -> String {
    t.format("%-d %b, %H:%M").to_string()
}

/// A past moment relative to `now`: "08:00" on the same day, "yesterday 22:00" the day before,
/// otherwise [`day_time`].
pub fn time_ago(t: NaiveDateTime, now: NaiveDateTime) -> String {
    let time = t.format("%H:%M");
    if t.date() == now.date() {
        time.to_string()
    } else if now.date().checked_sub_days(Days::new(1)) == Some(t.date()) {
        format!("yesterday {time}")
    } else {
        day_time(t)
    }
}

/// A fraction as a CSS width, clamped to 0–100% and rounded to 0.1: 0.287 -> "28.7%".
pub fn css_percent(fraction: Decimal) -> String {
    let p = round(fraction * Decimal::ONE_HUNDRED, 1).clamp(Decimal::ZERO, Decimal::ONE_HUNDRED);
    format!("{}%", p.normalize())
}

/// The path of a 96×32 sparkline, as in the asset card: points evenly spaced from x = 0 to 96,
/// the lowest value at y = 30 and the highest at y = 2, coordinates to one decimal. A flat
/// series sits in the middle (y = 16) and a single value is a dot-like segment across the
/// middle. None when there is no value.
pub fn sparkline_path(values: &[Decimal]) -> Option<String> {
    const WIDTH: Decimal = Decimal::from_parts(96, 0, 0, false, 0);
    const BOTTOM: Decimal = Decimal::from_parts(30, 0, 0, false, 0);
    const SPAN: Decimal = Decimal::from_parts(28, 0, 0, false, 0);
    const MIDDLE: Decimal = Decimal::from_parts(16, 0, 0, false, 0);
    let lo = values.iter().copied().min()?;
    let hi = values.iter().copied().max()?;
    let y = |v: Decimal| {
        if hi == lo {
            MIDDLE
        } else {
            BOTTOM - (v - lo) / (hi - lo) * SPAN
        }
    };
    let coord = |c: Decimal| format!("{:.1}", round(c, 1));
    if values.len() == 1 {
        let y = coord(y(values[0]));
        return Some(format!("M0.0 {y} L96.0 {y}"));
    }
    let last = Decimal::from(values.len() - 1);
    let path = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = Decimal::from(i) * WIDTH / last;
            let cmd = if i == 0 { 'M' } else { 'L' };
            format!("{cmd}{} {}", coord(x), coord(y(*v)))
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// Writes NBSP as "_" and U+2212 as "~" so the expected strings are readable.
    fn plain(s: String) -> String {
        s.replace(NBSP, "_").replace(MINUS, "~")
    }

    #[test]
    fn numbers_group_thousands_with_nbsp_and_use_a_decimal_comma() {
        assert_eq!(plain(number(dec!(11500), 2)), "11_500,00");
        assert_eq!(plain(number(dec!(1234567.891), 2)), "1_234_567,89");
        assert_eq!(plain(number(dec!(999.995), 2)), "1_000,00");
        assert_eq!(plain(number(dec!(123), 2)), "123,00");
        assert_eq!(plain(number(dec!(15000), 0)), "15_000");
        assert_eq!(plain(number(dec!(-312.4), 2)), "~312,40");
        // Rounds to zero, so no minus.
        assert_eq!(plain(number(dec!(-0.004), 2)), "0,00");
    }

    #[test]
    fn pln_puts_nbsp_before_zl() {
        assert_eq!(plain(pln(dec!(10092))), "10_092,00_zł");
        assert_eq!(plain(pln(dec!(-941.1))), "~941,10_zł");
        assert_eq!(plain(pln(dec!(0))), "0,00_zł");
    }

    #[test]
    fn signed_figures_use_plus_minus_and_plus_minus_sign_for_zero() {
        assert_eq!(plain(signed_pln(dec!(1408))), "+1_408,00_zł");
        assert_eq!(plain(signed_pln(dec!(-312.4))), "~312,40_zł");
        assert_eq!(plain(signed_pln(dec!(0))), "±0,00_zł");
        assert_eq!(plain(signed_pln(dec!(-0.004))), "±0,00_zł");
        assert_eq!(plain(signed_pln(dec!(0.005))), "+0,01_zł");
    }

    #[test]
    fn percentages_have_one_decimal() {
        // 0.327 -> 32.7; 0.18649 -> 18.649 -> 18.6; 0.13995 -> 13.995 -> 14.0.
        assert_eq!(percent(dec!(0.327)), "32,7%");
        assert_eq!(signed_percent(dec!(0.18649)), "+18,6%");
        assert_eq!(signed_percent(dec!(0.13995)), "+14,0%");
        assert_eq!(plain(signed_percent(dec!(-0.027))), "~2,7%");
        assert_eq!(signed_percent(dec!(-0.0004)), "±0,0%");
        // 1408 / 10092 = 0.13951... -> "+14,0%", the hero badge in screens/01.
        assert_eq!(signed_percent(dec!(1408) / dec!(10092)), "+14,0%");
    }

    #[test]
    fn deltas_take_tone_from_the_rounded_value() {
        let gain = delta_pln(dec!(1408));
        assert_eq!(
            (plain(gain.text.clone()), gain.tone),
            ("+1_408,00_zł".into(), Tone::Gain)
        );
        assert_eq!(gain.arrow(), "▲");
        let loss = delta_pln(dec!(-312.4));
        assert_eq!((loss.tone, loss.arrow()), (Tone::Loss, "▼"));
        let flat = delta_pln(dec!(-0.004));
        assert_eq!((flat.tone, flat.arrow()), (Tone::Flat, ""));
        assert_eq!(delta_percent(dec!(-0.00049)).tone, Tone::Flat);
        assert_eq!(delta_percent(dec!(-0.0005)).tone, Tone::Loss);
        assert_eq!(Tone::Loss.color(), "var(--loss)");
        assert_eq!(Tone::Flat.soft(), "var(--surface-2)");
    }

    #[test]
    fn drift_is_in_points_with_an_arrow() {
        // 0.53 actual - 0.55 target = -0.02 -> -2.0 pp.
        assert_eq!(plain(drift_pp(dec!(0.53) - dec!(0.55))), "▼ ~2,0_pp");
        assert_eq!(plain(drift_pp(dec!(0.287) - dec!(0.25))), "▲ +3,7_pp");
        assert_eq!(plain(drift_pp(dec!(0.0004))), "±0,0_pp");
    }

    #[test]
    fn dates_are_day_month_year() {
        assert_eq!(
            date(NaiveDate::from_ymd_opt(2026, 10, 2).unwrap()),
            "02.10.2026"
        );
    }

    #[test]
    fn css_widths_are_clamped_percentages() {
        assert_eq!(css_percent(dec!(0.287)), "28.7%");
        assert_eq!(css_percent(dec!(0.5)), "50%");
        assert_eq!(css_percent(dec!(0.18349)), "18.3%");
        assert_eq!(css_percent(dec!(-0.1)), "0%");
        assert_eq!(css_percent(dec!(1.2)), "100%");
    }

    #[test]
    fn sparkline_spans_96_by_28_from_y30_up() {
        // min 10, max 20; x = 0, 48, 96; y = 30 - (v - 10) / 10 * 28 = 30, 2, 16.
        assert_eq!(
            sparkline_path(&[dec!(10), dec!(20), dec!(15)]).unwrap(),
            "M0.0 30.0 L48.0 2.0 L96.0 16.0"
        );
        // x = 0, 32, 64, 96; y = 30 - (v - 1) / 3 * 28 = 30, 20.666.. -> 20.7, 11.333.. -> 11.3, 2.
        assert_eq!(
            sparkline_path(&[dec!(1), dec!(2), dec!(3), dec!(4)]).unwrap(),
            "M0.0 30.0 L32.0 20.7 L64.0 11.3 L96.0 2.0"
        );
        assert_eq!(
            sparkline_path(&[dec!(5), dec!(5)]).unwrap(),
            "M0.0 16.0 L96.0 16.0"
        );
        assert_eq!(sparkline_path(&[dec!(7)]).unwrap(), "M0.0 16.0 L96.0 16.0");
        assert_eq!(sparkline_path(&[]), None);
    }

    #[test]
    fn moments_read_as_in_the_states_sheet() {
        let at = |d: &str, h: u32, m: u32| {
            d.parse::<NaiveDate>()
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap()
        };
        assert_eq!(day_time(at("2026-10-15", 22, 0)), "15 Oct, 22:00");
        assert_eq!(day_time(at("2027-01-03", 8, 5)), "3 Jan, 08:05");
        let now = at("2026-10-16", 9, 30);
        assert_eq!(time_ago(at("2026-10-16", 8, 0), now), "08:00");
        assert_eq!(time_ago(at("2026-10-15", 22, 0), now), "yesterday 22:00");
        assert_eq!(time_ago(at("2026-10-14", 22, 0), now), "14 Oct, 22:00");
        // Across a new year: 31 Dec is yesterday on 1 Jan.
        let new_year = at("2027-01-01", 0, 30);
        assert_eq!(
            time_ago(at("2026-12-31", 23, 50), new_year),
            "yesterday 23:50"
        );
    }
}
