//! The shared chart: one server-rendered SVG figure for a KPI number, bars,
//! stacked bars, lines, a pie or a donut (BelimbingApp/bilibi#128). No Bilimbi
//! counterpart. A module passes categories and named series and gets a
//! `<figure>` back; it owns no chart markup, CSS or JavaScript.
//!
//! - Colours are the data roles `cat-1..8` (`DESIGN.md`), never status
//!   colours, so a chart reads the same in light and dark themes. Series take
//!   them in order.
//! - Geometry is computed here and written as SVG attributes; nothing uses a
//!   `style` attribute, which the Content Security Policy forbids.
//! - Every figure is labelled (its title names the SVG, and a sentence
//!   describes it), every mark carries a `<title>`, and a "Data" fold holds the
//!   exact numbers as a table: the fallback for a screen reader, for a value too
//!   small to see and for a chart refused below.
//! - A chart that would need more series, categories or slices than it can
//!   draw honestly shows a note and the table instead. It never drops values.
use askama::Template;

/// Series one chart draws; one data colour each.
pub const MAX_SERIES: usize = 8;
/// Categories along a bar or line chart's axis.
pub const MAX_CATEGORIES: usize = 60;
/// Slices of a pie or donut; past this a bar chart reads better.
pub const MAX_SLICES: usize = 8;

const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 240.0;
const LEFT: f64 = 52.0;
const RIGHT: f64 = 12.0;
const TOP: f64 = 12.0;
const BOTTOM: f64 = 36.0;
/// Category labels shown along the axis at most; the rest are in each mark's
/// title and the table.
const AXIS_LABELS: usize = 12;
const LABEL_CHARS: usize = 14;

// Written out so the stylesheet build finds every class.
const FILL: [&str; MAX_SERIES] = [
    "fill-cat-1",
    "fill-cat-2",
    "fill-cat-3",
    "fill-cat-4",
    "fill-cat-5",
    "fill-cat-6",
    "fill-cat-7",
    "fill-cat-8",
];
const STROKE: [&str; MAX_SERIES] = [
    "stroke-cat-1",
    "stroke-cat-2",
    "stroke-cat-3",
    "stroke-cat-4",
    "stroke-cat-5",
    "stroke-cat-6",
    "stroke-cat-7",
    "stroke-cat-8",
];
const SWATCH: [&str; MAX_SERIES] = [
    "bg-cat-1", "bg-cat-2", "bg-cat-3", "bg-cat-4", "bg-cat-5", "bg-cat-6", "bg-cat-7", "bg-cat-8",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartKind {
    /// The first value of the first series, large, with the series name.
    Kpi,
    /// Bars per category, side by side when there are several series.
    Bar,
    /// Each category's series stacked into one bar.
    StackedBar,
    Line,
    /// The first series' values as slices of the whole.
    Pie,
    /// A pie with the total in its centre.
    Donut,
}
impl ChartKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Kpi => "kpi",
            Self::Bar => "bar",
            Self::StackedBar => "stacked_bar",
            Self::Line => "line",
            Self::Pie => "pie",
            Self::Donut => "donut",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        [
            Self::Kpi,
            Self::Bar,
            Self::StackedBar,
            Self::Line,
            Self::Pie,
            Self::Donut,
        ]
        .into_iter()
        .find(|kind| kind.key() == key)
    }
}

/// One named run of values, aligned by position with the chart's categories.
/// A missing value (`None`, or a position past the end) is a gap, not zero.
#[derive(Clone, Debug)]
pub struct Series {
    pub name: String,
    pub values: Vec<Option<f64>>,
}

/// A figure ready to render.
#[derive(Template)]
#[template(path = "chart.html")]
pub struct Chart {
    id: String,
    title: String,
    kind: &'static str,
    /// Why nothing is drawn (no data, or too much to draw honestly).
    note: String,
    /// What the SVG shows, in one sentence.
    summary: String,
    kpi: String,
    kpi_label: String,
    ticks: Vec<Tick>,
    labels: Vec<AxisLabel>,
    rects: Vec<Mark>,
    lines: Vec<Line>,
    slices: Vec<Mark>,
    /// A donut's total.
    centre: String,
    legend: Vec<Legend>,
    headers: Vec<String>,
    rows: Vec<(String, Vec<String>)>,
}

struct Tick {
    y: String,
    label: String,
}
struct AxisLabel {
    x: String,
    text: String,
}
/// A bar (`x`, `y`, `w`, `h`) or a slice (`d`), with its colour and title.
struct Mark {
    x: String,
    y: String,
    w: String,
    h: String,
    d: String,
    class: &'static str,
    title: String,
}
struct Line {
    /// One polyline per unbroken run of values.
    runs: Vec<String>,
    class: &'static str,
    dots: Vec<Dot>,
    dot_class: &'static str,
}
struct Dot {
    x: String,
    y: String,
    title: String,
}
struct Legend {
    class: &'static str,
    label: String,
}

impl Chart {
    /// Builds the figure. `id` prefixes every element id; `title` names it.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        kind: ChartKind,
        categories: Vec<String>,
        series: Vec<Series>,
    ) -> Self {
        let mut chart = Chart {
            id: id.into(),
            title: title.into(),
            kind: kind.key(),
            note: String::new(),
            summary: String::new(),
            kpi: String::new(),
            kpi_label: String::new(),
            ticks: Vec::new(),
            labels: Vec::new(),
            rects: Vec::new(),
            lines: Vec::new(),
            slices: Vec::new(),
            centre: String::new(),
            legend: Vec::new(),
            headers: std::iter::once(String::new())
                .chain(series.iter().map(|s| s.name.clone()))
                .collect(),
            rows: categories
                .iter()
                .enumerate()
                .map(|(i, category)| {
                    let cells = series
                        .iter()
                        .map(|s| {
                            s.values
                                .get(i)
                                .copied()
                                .flatten()
                                .map_or("—".into(), format_number)
                        })
                        .collect();
                    (category.clone(), cells)
                })
                .collect(),
        };
        let value =
            |s: &Series, i: usize| s.values.get(i).copied().flatten().filter(|v| v.is_finite());
        let any = series
            .iter()
            .any(|s| (0..categories.len().max(1)).any(|i| value(s, i).is_some()));
        if kind == ChartKind::Kpi {
            let first = series.first();
            chart.kpi = first
                .and_then(|s| value(s, 0))
                .map_or("—".into(), format_number);
            chart.kpi_label = first.map(|s| s.name.clone()).unwrap_or_default();
            chart.summary = format!("{}: {}", chart.kpi_label, chart.kpi);
            return chart;
        }
        if !any || categories.is_empty() {
            chart.note = "No data to chart.".into();
            return chart;
        }
        if series.len() > MAX_SERIES {
            chart.note = format!(
                "Too many series to chart ({}, at most {MAX_SERIES}). The data is below.",
                series.len()
            );
            return chart;
        }
        match kind {
            ChartKind::Pie | ChartKind::Donut => {
                chart.pie(&categories, &series[0], kind == ChartKind::Donut, value);
            }
            _ => {
                if categories.len() > MAX_CATEGORIES {
                    chart.note = format!("Too many values to chart ({}, at most {MAX_CATEGORIES}). The data is below.", categories.len());
                    return chart;
                }
                chart.legend = series
                    .iter()
                    .enumerate()
                    .map(|(i, s)| Legend {
                        class: SWATCH[i],
                        label: s.name.clone(),
                    })
                    .collect();
                chart.axes(kind, &categories, &series, value);
            }
        }
        chart
    }

    /// Bars, stacked bars and lines on a value axis from zero.
    fn axes(
        &mut self,
        kind: ChartKind,
        categories: &[String],
        series: &[Series],
        value: impl Fn(&Series, usize) -> Option<f64>,
    ) {
        let n = categories.len();
        let (mut lo, mut hi) = (0.0_f64, 0.0_f64);
        for i in 0..n {
            if kind == ChartKind::StackedBar {
                let (up, down) =
                    series
                        .iter()
                        .filter_map(|s| value(s, i))
                        .fold(
                            (0.0, 0.0),
                            |(u, d), v| {
                                if v >= 0.0 {
                                    (u + v, d)
                                } else {
                                    (u, d + v)
                                }
                            },
                        );
                hi = hi.max(up);
                lo = lo.min(down);
            } else {
                for s in series {
                    if let Some(v) = value(s, i) {
                        hi = hi.max(v);
                        lo = lo.min(v);
                    }
                }
            }
        }
        // About four ticks at a round step, the axis ending at the first
        // step past the data and always including zero.
        let step = nice((hi - lo).max(1.0) / 4.0);
        let hi = (hi / step).ceil() * step;
        let lo = (lo / step).floor() * step;
        let (hi, lo) = if hi == lo { (step, 0.0) } else { (hi, lo) };
        let plot_w = WIDTH - LEFT - RIGHT;
        let plot_h = HEIGHT - TOP - BOTTOM;
        let y = |v: f64| TOP + (hi - v) / (hi - lo) * plot_h;
        let steps = ((hi - lo) / step).round() as u32;
        self.ticks = (0..=steps)
            .map(|k| {
                let v = lo + step * f64::from(k);
                Tick {
                    y: num(y(v)),
                    label: format_number(v),
                }
            })
            .collect();
        let band = plot_w / n as f64;
        let every = n.div_ceil(AXIS_LABELS);
        self.labels = categories
            .iter()
            .enumerate()
            .filter(|(i, _)| i % every == 0)
            .map(|(i, text)| AxisLabel {
                x: num(LEFT + (i as f64 + 0.5) * band),
                text: short(text),
            })
            .collect();
        let zero = y(0.0);
        let title = |category: &str, s: &Series, v: f64| {
            if series.len() == 1 {
                format!("{category}: {}", format_number(v))
            } else {
                format!("{category} · {}: {}", s.name, format_number(v))
            }
        };
        match kind {
            ChartKind::Line => {
                for (si, s) in series.iter().enumerate() {
                    let mut runs = Vec::new();
                    let mut run = Vec::new();
                    let mut dots = Vec::new();
                    for (i, category) in categories.iter().enumerate() {
                        let x = LEFT + (i as f64 + 0.5) * band;
                        match value(s, i) {
                            Some(v) => {
                                run.push(format!("{},{}", num(x), num(y(v))));
                                dots.push(Dot {
                                    x: num(x),
                                    y: num(y(v)),
                                    title: title(category, s, v),
                                });
                            }
                            None if !run.is_empty() => {
                                runs.push(std::mem::take(&mut run).join(" "))
                            }
                            None => {}
                        }
                    }
                    if !run.is_empty() {
                        runs.push(run.join(" "));
                    }
                    self.lines.push(Line {
                        runs,
                        class: STROKE[si],
                        dots,
                        dot_class: FILL[si],
                    });
                }
                self.summary = format!("Line chart of {} over {n} values.", names(series));
            }
            ChartKind::StackedBar => {
                let w = band * 0.7;
                for (i, category) in categories.iter().enumerate() {
                    let (mut up, mut down) = (0.0, 0.0);
                    for (si, s) in series.iter().enumerate() {
                        let Some(v) = value(s, i) else { continue };
                        let (from, to) = if v >= 0.0 {
                            (up, up + v)
                        } else {
                            (down + v, down)
                        };
                        if v >= 0.0 {
                            up += v
                        } else {
                            down += v
                        }
                        self.rects.push(bar(
                            LEFT + i as f64 * band + (band - w) / 2.0,
                            w,
                            y(to),
                            y(from),
                            FILL[si],
                            title(category, s, v),
                        ));
                    }
                }
                self.summary = format!("Stacked bar chart of {} across {n} values.", names(series));
            }
            _ => {
                let inner = band * 0.7;
                let w = inner / series.len() as f64;
                for (i, category) in categories.iter().enumerate() {
                    for (si, s) in series.iter().enumerate() {
                        let Some(v) = value(s, i) else { continue };
                        let x = LEFT + i as f64 * band + (band - inner) / 2.0 + si as f64 * w;
                        self.rects.push(bar(
                            x,
                            w,
                            y(v.max(0.0)),
                            y(v.min(0.0)),
                            FILL[si],
                            title(category, s, v),
                        ));
                    }
                }
                self.summary = format!("Bar chart of {} across {n} values.", names(series));
            }
        }
        let _ = zero;
    }

    /// Slices of the first series' positive values, clockwise from the top.
    fn pie(
        &mut self,
        categories: &[String],
        series: &Series,
        donut: bool,
        value: impl Fn(&Series, usize) -> Option<f64>,
    ) {
        let parts: Vec<(usize, f64)> = (0..categories.len())
            .filter_map(|i| value(series, i).map(|v| (i, v)))
            .collect();
        if parts.iter().any(|(_, v)| *v < 0.0) {
            self.note = "A pie cannot show negative values. The data is below.".into();
            return;
        }
        let parts: Vec<(usize, f64)> = parts.into_iter().filter(|(_, v)| *v > 0.0).collect();
        let total: f64 = parts.iter().map(|(_, v)| v).sum();
        if parts.is_empty() || total <= 0.0 {
            self.note = "No data to chart.".into();
            return;
        }
        if parts.len() > MAX_SLICES {
            self.note = format!("Too many slices for a pie ({}, at most {MAX_SLICES}). A bar chart reads better; the data is below.", parts.len());
            return;
        }
        let (cx, cy, r) = (WIDTH / 2.0, HEIGHT / 2.0, 100.0);
        let inner = if donut { r * 0.6 } else { 0.0 };
        let mut start = -std::f64::consts::FRAC_PI_2;
        for (slot, (i, v)) in parts.iter().enumerate() {
            let sweep = v / total * std::f64::consts::TAU;
            let percent = v / total * 100.0;
            self.slices.push(Mark {
                x: String::new(),
                y: String::new(),
                w: String::new(),
                h: String::new(),
                d: arc(cx, cy, r, inner, start, start + sweep),
                class: FILL[slot],
                title: format!(
                    "{}: {} ({:.1}%)",
                    categories[*i],
                    format_number(*v),
                    percent
                ),
            });
            self.legend.push(Legend {
                class: SWATCH[slot],
                label: categories[*i].clone(),
            });
            start += sweep;
        }
        if donut {
            self.centre = format_number(total);
        }
        self.summary = format!(
            "{} chart of {} in {} parts, totalling {}.",
            if donut { "Donut" } else { "Pie" },
            series.name,
            parts.len(),
            format_number(total)
        );
    }
}

fn bar(x: f64, w: f64, top: f64, bottom: f64, class: &'static str, title: String) -> Mark {
    Mark {
        x: num(x),
        y: num(top),
        w: num(w.max(0.5)),
        h: num((bottom - top).max(0.0)),
        d: String::new(),
        class,
        title,
    }
}

/// The path of one slice (or ring segment when `inner` is above zero). A
/// whole circle is drawn as two halves, since one arc cannot close on itself.
fn arc(cx: f64, cy: f64, r: f64, inner: f64, from: f64, to: f64) -> String {
    let point = |radius: f64, angle: f64| (cx + radius * angle.cos(), cy + radius * angle.sin());
    if to - from >= std::f64::consts::TAU - 1e-9 {
        let mid = from + std::f64::consts::PI;
        return format!(
            "{} {}",
            arc(cx, cy, r, inner, from, mid),
            arc(cx, cy, r, inner, mid, to)
        );
    }
    let large = if to - from > std::f64::consts::PI {
        1
    } else {
        0
    };
    let (x0, y0) = point(r, from);
    let (x1, y1) = point(r, to);
    if inner <= 0.0 {
        return format!(
            "M{} {} L{} {} A{} {} 0 {large} 1 {} {} Z",
            num(cx),
            num(cy),
            num(x0),
            num(y0),
            num(r),
            num(r),
            num(x1),
            num(y1)
        );
    }
    let (ix1, iy1) = point(inner, to);
    let (ix0, iy0) = point(inner, from);
    format!(
        "M{} {} A{} {} 0 {large} 1 {} {} L{} {} A{} {} 0 {large} 0 {} {} Z",
        num(x0),
        num(y0),
        num(r),
        num(r),
        num(x1),
        num(y1),
        num(ix1),
        num(iy1),
        num(inner),
        num(inner),
        num(ix0),
        num(iy0)
    )
}

/// The smallest of 1, 2, 2.5 or 5 times a power of ten at or above `value`:
/// a tick step that reads as a round number.
fn nice(value: f64) -> f64 {
    let power = 10f64.powf(value.log10().floor());
    let step = value / power;
    let nice = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .find(|nice| step <= *nice)
        .unwrap_or(10.0);
    nice * power
}

fn names(series: &[Series]) -> String {
    series
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// An axis label, shortened; the full text is in the table and each title.
fn short(text: &str) -> String {
    if text.chars().count() <= LABEL_CHARS {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(LABEL_CHARS - 1).collect();
    short.push('…');
    short
}

/// An SVG coordinate: one decimal, so markup stays small and stable.
fn num(value: f64) -> String {
    format!("{:.1}", (value * 10.0).round() / 10.0)
}

/// A number as a chart labels it: whole numbers without decimals, others with
/// two, thousands grouped with commas. Not localised yet.
fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    let whole = (value - value.round()).abs() < 1e-9;
    let text = if whole {
        format!("{:.0}", value.abs())
    } else {
        format!("{:.2}", value.abs())
    };
    let (int, frac) = text
        .split_once('.')
        .map_or((text.as_str(), ""), |(i, f)| (i, f));
    let mut grouped = String::new();
    for (i, digit) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    // A minus only where a digit other than zero is shown: -0.004 is "0.00".
    let sign = if value < 0.0 && text.bytes().any(|b| b.is_ascii_digit() && b != b'0') {
        "-"
    } else {
        ""
    };
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac}")
    }
}

/// The points of a sparkline polyline for a series, in a 60 by 16 box, for
/// two values or more: the trend lens's small line (`flex_table` cells).
pub fn sparkline(series: &[f64]) -> Option<String> {
    if series.len() < 2 {
        return None;
    }
    let (lo, hi) = series
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), value| {
            (lo.min(*value), hi.max(*value))
        });
    let span = if hi == lo { 1.0 } else { hi - lo };
    let step = 60.0 / (series.len() - 1) as f64;
    // One decimal, always written, as Bilimbi's Float.round(_, 1) prints.
    let one = |value: f64| format!("{:.1}", (value * 10.0).round() / 10.0);
    Some(
        series
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let x = one(index as f64 * step);
                let y = one(15.0 - (value - lo) / span * 14.0);
                format!("{x},{y}")
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}
