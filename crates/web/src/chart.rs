//! The Overview chart's geometry (`design/reference/01-overview.html`, ported from its demo
//! script): a 1000×300 viewBox stretched to the card, "nice" y-ticks, the value line with its
//! area, the stepped contributions and the dashed "all-in VWCE" benchmark, the x labels and the
//! dot on the last value.
//!
//! The y-axis spans every drawn figure (the benchmark only while it is shown), widened to whole
//! steps of a "nice" size (1, 2, 2.5 or 5 times a power of ten) giving about 4.5 steps. The All
//! view starts the axis at 0, as in the design. A day without a value breaks the line there and
//! leaves out the area (it would otherwise draw a wrong shape); the other lines skip such days.

use chrono::{Datelike, NaiveDate};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::format;

const W: Decimal = Decimal::from_parts(1000, 0, 0, false, 0);
const H: Decimal = Decimal::from_parts(300, 0, 0, false, 0);
const HUNDRED: Decimal = Decimal::ONE_HUNDRED;
const TEN: Decimal = Decimal::TEN;
/// (hi - lo) / 4.5 gives the rough step.
const STEPS: Decimal = Decimal::from_parts(45, 0, 0, false, 1);
/// The most x labels drawn.
const MAX_X_LABELS: usize = 7;

/// One day of the chart, in PLN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub date: NaiveDate,
    pub value: Option<Decimal>,
    pub contributions: Option<Decimal>,
    pub benchmark: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YTick {
    /// "6 000".
    pub label: String,
    /// CSS `top`, a percentage of the plot's height: "33.33".
    pub top: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XTick {
    pub label: String,
    /// CSS `left`, a percentage of the plot's width: "50.00".
    pub left: String,
    /// The CSS `translateX`: "0" for the first label, "-100%" for one pinned to the right
    /// edge, "-50%" (centred) otherwise.
    pub shift: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chart {
    pub y_ticks: Vec<YTick>,
    pub x_ticks: Vec<XTick>,
    pub value_path: String,
    /// Empty when some day has no value.
    pub area_path: String,
    pub contributions_path: String,
    /// Empty while the benchmark is hidden or has no day.
    pub benchmark_path: String,
    /// The last value's dot, as CSS `left` and `top` percentages.
    pub end_dot: Option<(String, String)>,
}

fn round(v: Decimal, dp: u32) -> Decimal {
    v.round_dp_with_strategy(dp, RoundingStrategy::MidpointAwayFromZero)
}

fn fixed(v: Decimal, dp: usize) -> String {
    format!("{:.*}", dp, round(v, dp as u32))
}

/// The smallest of 1, 2, 2.5, 5 and 10 times a power of ten that is at least `x` (> 0).
pub fn nice_step(x: Decimal) -> Decimal {
    let mut power = Decimal::ONE;
    while x >= power * TEN {
        power *= TEN;
    }
    while x < power {
        power /= TEN;
    }
    let f = x / power;
    let m = if f <= Decimal::ONE {
        Decimal::ONE
    } else if f <= Decimal::TWO {
        Decimal::TWO
    } else if f <= Decimal::from_parts(25, 0, 0, false, 1) {
        Decimal::from_parts(25, 0, 0, false, 1)
    } else if f <= Decimal::from(5) {
        Decimal::from(5)
    } else {
        TEN
    };
    m * power
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The x label of each day: the month ("Sep"), with the year on the first day and on January
/// ("May ’25"). A day that shares its month with a neighbour (a period's start day before that
/// month's end) shows its day too ("14 Sep"), so no two labels read the same.
pub fn x_labels(dates: &[NaiveDate]) -> Vec<String> {
    let month = |d: &NaiveDate| (d.year(), d.month());
    dates
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let name = MONTHS[d.month0() as usize];
            let shared = (i > 0 && month(&dates[i - 1]) == month(d))
                || dates.get(i + 1).is_some_and(|n| month(n) == month(d));
            let mut label = if shared {
                format!("{} {name}", d.day())
            } else {
                name.to_owned()
            };
            if i == 0 || d.month() == 1 {
                label.push_str(&format!(" ’{:02}", d.year() % 100));
            }
            label
        })
        .collect()
}

/// The chart of `points` (ascending days). `from_zero` starts the y-axis at 0 (the All view).
/// None when no figure is known.
pub fn chart(points: &[Point], show_benchmark: bool, from_zero: bool) -> Option<Chart> {
    let mut figures: Vec<Decimal> = points
        .iter()
        .flat_map(|p| [p.value, p.contributions])
        .flatten()
        .collect();
    if show_benchmark {
        figures.extend(points.iter().filter_map(|p| p.benchmark));
    }
    let mut lo = figures.iter().copied().min()?;
    let mut hi = figures.iter().copied().max()?;
    if from_zero {
        lo = lo.min(Decimal::ZERO);
    }
    let rough = (hi - lo) / STEPS;
    let step = nice_step(if rough > Decimal::ZERO {
        rough
    } else {
        Decimal::ONE
    });
    lo = (lo / step).floor() * step;
    hi = (hi / step).ceil() * step;
    if hi == lo {
        hi = lo + step;
    }

    let n = points.len();
    let x = |i: usize| {
        if n == 1 {
            W / Decimal::TWO
        } else {
            Decimal::from(i) * W / Decimal::from(n - 1)
        }
    };
    let y = |v: Decimal| H - (v - lo) / (hi - lo) * H;
    let xy = |i: usize, v: Decimal| format!("{} {}", fixed(x(i), 1), fixed(y(v), 1));

    // A line through the known days; an unknown day breaks it.
    let line = |pick: fn(&Point) -> Option<Decimal>| {
        let mut path = Vec::new();
        let mut broken = true;
        for (i, p) in points.iter().enumerate() {
            match pick(p) {
                Some(v) => {
                    path.push(format!("{}{}", if broken { 'M' } else { 'L' }, xy(i, v)));
                    broken = false;
                }
                None => broken = true,
            }
        }
        path.join(" ")
    };
    let value_path = line(|p| p.value);
    let area_path = if points.iter().all(|p| p.value.is_some()) {
        format!("{value_path} L{W} {H} L0 {H} Z")
    } else {
        String::new()
    };
    let mut contributions_path = String::new();
    for (i, v) in points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| Some((i, p.contributions?)))
    {
        if contributions_path.is_empty() {
            contributions_path = format!("M{}", xy(i, v));
        } else {
            contributions_path.push_str(&format!(" H{} V{}", fixed(x(i), 1), fixed(y(v), 1)));
        }
    }
    let benchmark_path = if show_benchmark {
        let mut path = Vec::new();
        for (i, v) in points
            .iter()
            .enumerate()
            .filter_map(|(i, p)| Some((i, p.benchmark?)))
        {
            let cmd = if path.is_empty() { 'M' } else { 'L' };
            path.push(format!("{cmd}{}", xy(i, v)));
        }
        path.join(" ")
    } else {
        String::new()
    };

    let mut y_ticks = Vec::new();
    let mut t = lo;
    while t <= hi {
        y_ticks.push(YTick {
            label: format::number(t, 0),
            top: fixed((Decimal::ONE - (t - lo) / (hi - lo)) * HUNDRED, 2),
        });
        t += step;
    }

    let labels = x_labels(&points.iter().map(|p| p.date).collect::<Vec<_>>());
    let left = |i: usize| x(i) / TEN;
    let every = n.div_ceil(MAX_X_LABELS).max(1);
    let mut x_ticks: Vec<XTick> = (0..n)
        .step_by(every)
        .map(|j| XTick {
            label: labels[j].clone(),
            left: fixed(left(j), 2),
            shift: if j == 0 { "0" } else { "-50%" },
        })
        .collect();
    if n > 1 && !(n - 1).is_multiple_of(every) {
        x_ticks.push(XTick {
            label: labels[n - 1].clone(),
            left: fixed(HUNDRED, 2),
            shift: "-100%",
        });
    }

    let end_dot = points
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, p)| Some((i, p.value?)))
        .map(|(i, v)| (fixed(left(i), 2), fixed(y(v) / Decimal::from(3), 2)));

    Some(Chart {
        y_ticks,
        x_ticks,
        value_path,
        area_path,
        contributions_path,
        benchmark_path,
        end_dot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn point(date: &str, value: Option<Decimal>, contributions: Decimal, bench: Decimal) -> Point {
        Point {
            date: d(date),
            value,
            contributions: Some(contributions),
            benchmark: Some(bench),
        }
    }

    #[test]
    fn nice_steps_round_up_to_1_2_2_5_5_or_10() {
        // The design's All view: (11 500 - 0) / 4.5 = 2555.6 -> 5000; others by hand.
        assert_eq!(nice_step(dec!(11500) / dec!(4.5)), dec!(5000));
        assert_eq!(nice_step(dec!(1)), dec!(1));
        assert_eq!(nice_step(dec!(1.01)), dec!(2));
        assert_eq!(nice_step(dec!(240)), dec!(250));
        assert_eq!(nice_step(dec!(251)), dec!(500));
        assert_eq!(nice_step(dec!(600)), dec!(1000));
        assert_eq!(nice_step(dec!(0.03)), dec!(0.05));
        assert_eq!(nice_step(dec!(1000)), dec!(1000));
    }

    #[test]
    fn labels_are_months_with_the_year_on_the_first_and_january() {
        let labels = x_labels(&[
            d("2025-05-12"),
            d("2025-05-31"),
            d("2025-06-30"),
            d("2025-12-31"),
            d("2026-01-31"),
            d("2026-02-14"),
        ]);
        assert_eq!(
            labels,
            ["12 May ’25", "31 May", "Jun", "Dec", "Jan ’26", "Feb"]
        );
        assert_eq!(
            x_labels(&[d("2025-05-31"), d("2025-06-30")]),
            ["May ’25", "Jun"]
        );
    }

    #[test]
    fn geometry_matches_the_reference_maths() {
        // Values 1000, 2000, 2600; contributions 1000, 1500, 2000; benchmark 1000, 1800, 2400.
        // All view: lo = 0, hi = 2600; (2600 - 0) / 4.5 = 577.8 -> step 1000; hi -> 3000.
        // x = 0, 500, 1000; y = 300 - v / 3000 x 300 = 300 - v / 10: 200, 100, 40.
        let points = [
            point("2026-01-15", Some(dec!(1000)), dec!(1000), dec!(1000)),
            point("2026-01-31", Some(dec!(2000)), dec!(1500), dec!(1800)),
            point("2026-02-28", Some(dec!(2600)), dec!(2000), dec!(2400)),
        ];
        let c = chart(&points, true, true).unwrap();
        assert_eq!(c.value_path, "M0.0 200.0 L500.0 100.0 L1000.0 40.0");
        assert_eq!(
            c.area_path,
            "M0.0 200.0 L500.0 100.0 L1000.0 40.0 L1000 300 L0 300 Z"
        );
        // Contributions y: 200, 150, 100, stepped: across, then up.
        assert_eq!(
            c.contributions_path,
            "M0.0 200.0 H500.0 V150.0 H1000.0 V100.0"
        );
        // Benchmark y: 200, 120, 60.
        assert_eq!(c.benchmark_path, "M0.0 200.0 L500.0 120.0 L1000.0 60.0");
        let ticks: Vec<_> = c
            .y_ticks
            .iter()
            .map(|t| (t.label.replace('\u{a0}', " "), t.top.as_str()))
            .collect();
        assert_eq!(
            ticks,
            [
                ("0".to_owned(), "100.00"),
                ("1 000".to_owned(), "66.67"),
                ("2 000".to_owned(), "33.33"),
                ("3 000".to_owned(), "0.00"),
            ]
        );
        let x: Vec<_> = c
            .x_ticks
            .iter()
            .map(|t| (t.label.as_str(), t.left.as_str(), t.shift))
            .collect();
        assert_eq!(
            x,
            [
                ("15 Jan ’26", "0.00", "0"),
                ("31 Jan ’26", "50.00", "-50%"),
                ("Feb", "100.00", "-50%")
            ]
        );
        // The dot: x 1000 / 10 = 100%, y 40 / 3 = 13.33%.
        assert_eq!(c.end_dot, Some(("100.00".into(), "13.33".into())));

        // Hidden benchmark: no path, and its figures do not stretch the axis. Not from zero:
        // lo 1000, hi 2600, (1600) / 4.5 = 355.6 -> 500: 1000 to 3000, y = 300 - (v - 1000)
        // / 2000 x 300: 300, 150, 60.
        let c = chart(&points, false, false).unwrap();
        assert_eq!(c.benchmark_path, "");
        assert_eq!(c.value_path, "M0.0 300.0 L500.0 150.0 L1000.0 60.0");
        assert_eq!(
            c.y_ticks.first().unwrap().label.replace('\u{a0}', " "),
            "1 000"
        );
        assert_eq!(c.y_ticks.len(), 5);
    }

    #[test]
    fn many_days_get_every_nth_label_and_the_last_one_pinned() {
        // 9 days: every ceil(9 / 7) = 2nd label (0, 2, 4, 6, 8); 8 % 2 == 0, so no extra one.
        // 10 days: 0, 2, 4, 6, 8, and 9 pinned to the right edge.
        let month_ends = [
            "2025-01-31",
            "2025-02-28",
            "2025-03-31",
            "2025-04-30",
            "2025-05-31",
            "2025-06-30",
            "2025-07-31",
            "2025-08-31",
            "2025-09-30",
            "2025-10-31",
        ];
        let pts: Vec<Point> = month_ends
            .iter()
            .map(|s| point(s, Some(dec!(1)), dec!(1), dec!(1)))
            .collect();
        let nine = chart(&pts[..9], true, true).unwrap();
        assert_eq!(
            nine.x_ticks
                .iter()
                .map(|t| t.label.as_str())
                .collect::<Vec<_>>(),
            ["Jan ’25", "Mar", "May", "Jul", "Sep"]
        );
        let ten = chart(&pts, true, true).unwrap();
        let last = ten.x_ticks.last().unwrap();
        assert_eq!(ten.x_ticks.len(), 6);
        assert_eq!(
            (last.label.as_str(), last.left.as_str(), last.shift),
            ("Oct", "100.00", "-100%")
        );
    }

    #[test]
    fn a_missing_value_breaks_the_line_and_drops_the_area() {
        // lo 0 hi 300 -> 300 / 4.5 = 66.7 -> step 100; y = 300 - v: 200, -, 0.
        let points = [
            point("2026-01-31", Some(dec!(100)), dec!(100), dec!(100)),
            point("2026-02-28", None, dec!(200), dec!(200)),
            point("2026-03-31", Some(dec!(300)), dec!(300), dec!(300)),
        ];
        let c = chart(&points, true, true).unwrap();
        assert_eq!(c.value_path, "M0.0 200.0 M1000.0 0.0");
        assert_eq!(c.area_path, "");
        assert_eq!(c.end_dot, Some(("100.00".into(), "0.00".into())));
    }

    #[test]
    fn a_flat_or_single_day_chart_still_has_an_axis() {
        // All zero: the rough step is 0, so 1 is used; the axis runs 0 to 1. One day sits in
        // the middle (x 500 -> 50%).
        let c = chart(
            &[point("2026-10-14", Some(dec!(0)), dec!(0), dec!(0))],
            true,
            true,
        )
        .unwrap();
        assert_eq!(c.value_path, "M500.0 300.0");
        assert_eq!(c.y_ticks.len(), 2);
        assert_eq!(c.x_ticks.len(), 1);
        assert_eq!(c.x_ticks[0].left, "50.00");
        assert_eq!(chart(&[], true, true), None);
    }
}
