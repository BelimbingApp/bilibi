//! The shared chart: one server-rendered SVG figure for a KPI number, bars,
//! stacked bars, lines, a pie or a donut (BelimbingApp/bilibi#128). A module passes categories and named series and gets a
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
/// The narrowest value-axis margin; it widens to fit the longest tick label.
const LEFT: f64 = 56.0;
const RIGHT: f64 = 12.0;
const TOP: f64 = 12.0;
const BOTTOM: f64 = 36.0;
/// Category labels shown along the axis at most; the rest are in each mark's
/// title and the table.
const AXIS_LABELS: usize = 12;
const LABEL_CHARS: usize = 14;
/// The width of one character of label text (12 units), near enough to lay
/// out by.
const CHAR_W: f64 = 7.2;
/// Category labels that do not fit their spacing turn by this many degrees.
const TURN: f64 = 40.0;
/// Ticks on a value axis at most; a round step gives four to ten.
const MAX_TICKS: f64 = 20.0;
const TOO_LARGE: &str = "These values are beyond the scale a chart can draw. The data is below.";

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
    /// The SVG's height: taller when category labels turn.
    height: String,
    /// Where the plot starts, and where tick labels end, on the x axis.
    left: String,
    tick_x: String,
    label_y: String,
    /// Category labels turn to fit (`rotate(-40 x y)` on each).
    turned: bool,
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
    /// The event a bar or slice raises once [`Chart::drill`] names one.
    drill_event: &'static str,
}

struct Tick {
    y: String,
    label: String,
}
struct AxisLabel {
    x: String,
    text: String,
    /// The label's `transform` when labels turn, else empty.
    turn: String,
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
    /// Which series and category the mark draws.
    at: (usize, usize),
    /// What the mark raises when [`Chart::drill`] makes it a control.
    drill: String,
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
    /// Makes each bar and slice a control that raises `event` with `line`
    /// set to what `pick(series, category)` answers (a value the owner holds,
    /// such as the number of the line it drew), where it answers one. The
    /// "Data" table stays the keyboard route; the marks are a pointer shortcut.
    pub fn drill(
        mut self,
        event: &'static str,
        pick: impl Fn(usize, usize) -> Option<String>,
    ) -> Self {
        self.drill_event = event;
        for mark in self.rects.iter_mut().chain(self.slices.iter_mut()) {
            mark.drill = pick(mark.at.0, mark.at.1).unwrap_or_default();
        }
        self
    }
    /// Builds the figure. `id` prefixes every element id; `title` names it.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        kind: ChartKind,
        categories: Vec<String>,
        series: Vec<Series>,
    ) -> Self {
        Self::new_with_display_values(id, title, kind, categories, series, Vec::new())
    }

    /// Builds a chart with owner-formatted values, indexed by series then category.
    /// Missing display values fall back to the geometry value; missing numeric
    /// values remain gaps. Supply the stored decimal scale in each display value.
    pub fn new_with_display_values(
        id: impl Into<String>,
        title: impl Into<String>,
        kind: ChartKind,
        categories: Vec<String>,
        series: Vec<Series>,
        display_values: Vec<Vec<Option<String>>>,
    ) -> Self {
        let display = |si: usize, i: usize, v: f64| {
            display_values
                .get(si)
                .and_then(|s| s.get(i))
                .and_then(Option::as_ref)
                .cloned()
                .unwrap_or_else(|| format_exact(v))
        };
        let mut chart = Chart {
            id: id.into(),
            title: title.into(),
            kind: kind.key(),
            note: String::new(),
            summary: String::new(),
            kpi: String::new(),
            kpi_label: String::new(),
            height: num(HEIGHT),
            left: num(LEFT),
            tick_x: num(LEFT - 6.0),
            label_y: num(HEIGHT - 18.0),
            turned: false,
            ticks: Vec::new(),
            labels: Vec::new(),
            rects: Vec::new(),
            lines: Vec::new(),
            slices: Vec::new(),
            centre: String::new(),
            legend: Vec::new(),
            drill_event: "",
            headers: std::iter::once(String::new())
                .chain(series.iter().map(|s| s.name.clone()))
                .collect(),
            rows: categories
                .iter()
                .enumerate()
                .map(|(i, category)| {
                    let cells = series
                        .iter()
                        .enumerate()
                        .map(|(si, s)| {
                            s.values
                                .get(i)
                                .copied()
                                .flatten()
                                .filter(|v| v.is_finite())
                                .map_or("—".into(), |v| display(si, i, v))
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
                .map_or("—".into(), |v| display(0, 0, v));
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
                chart.pie(
                    &categories,
                    &series[0],
                    kind == ChartKind::Donut,
                    value,
                    &display,
                );
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
                chart.axes(kind, &categories, &series, value, &display);
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
        display: &impl Fn(usize, usize, f64) -> String,
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
        // About four ticks at a round step of the real span, the axis ending
        // at the first step past the data and always including zero. A span
        // or step a float cannot hold, or too many ticks, is refused in words:
        // the table still has every value.
        let span = hi - lo;
        let step = if span == 0.0 { 1.0 } else { nice(span / 4.0) };
        let (hi, lo) = ((hi / step).ceil() * step, (lo / step).floor() * step);
        let (hi, lo) = if hi == lo { (step, 0.0) } else { (hi, lo) };
        let steps = ((hi - lo) / step).round();
        if !(span.is_finite() && step.is_finite() && step > 0.0 && (hi - lo).is_finite())
            || steps > MAX_TICKS
        {
            self.note = TOO_LARGE.into();
            return;
        }
        let scale = Scale::new(lo.abs().max(hi.abs()), step);
        let ticks: Vec<(f64, String)> = (0..=steps as u32)
            .map(|k| {
                let v = lo + step * f64::from(k);
                (v, scale.label(v))
            })
            .collect();
        let widest = ticks
            .iter()
            .map(|(_, l)| l.chars().count())
            .max()
            .unwrap_or(0);
        let every = n.div_ceil(AXIS_LABELS);
        let shown: Vec<(usize, String)> = categories
            .iter()
            .enumerate()
            .filter(|(i, _)| i % every == 0)
            .map(|(i, text)| (i, short(text)))
            .collect();
        // Labels lie flat while the longest fits the room between two of
        // them; otherwise they all turn, the figure grows to hold them and
        // the margin widens so the first does not run off the left edge.
        let longest = shown
            .iter()
            .map(|(_, t)| t.chars().count())
            .max()
            .unwrap_or(0);
        let mut left = LEFT.max(widest as f64 * CHAR_W + 12.0);
        let band = (WIDTH - left - RIGHT) / n as f64;
        self.turned = longest as f64 * CHAR_W > band * every as f64 - 4.0;
        if self.turned {
            let first = shown.first().map_or(0, |(_, t)| t.chars().count());
            let reach = first as f64 * CHAR_W * TURN.to_radians().cos();
            left = left.max(reach - band / 2.0 + 4.0);
        }
        let plot_w = WIDTH - left - RIGHT;
        let plot_h = HEIGHT - TOP - BOTTOM;
        let y = |v: f64| TOP + (hi - v) / (hi - lo) * plot_h;
        self.left = num(left);
        self.tick_x = num(left - 6.0);
        self.ticks = ticks
            .into_iter()
            .map(|(v, label)| Tick {
                y: num(y(v)),
                label,
            })
            .collect();
        let band = plot_w / n as f64;
        let plot_bottom = TOP + plot_h;
        if self.turned {
            let rise = longest as f64 * CHAR_W * TURN.to_radians().sin() + 12.0;
            self.label_y = num(plot_bottom + 12.0);
            self.height = num(HEIGHT.max(plot_bottom + 12.0 + rise + 6.0));
        }
        self.labels = shown
            .into_iter()
            .map(|(i, text)| {
                let x = num(left + (i as f64 + 0.5) * band);
                let turn = if self.turned {
                    format!("rotate(-{TURN} {x} {})", self.label_y)
                } else {
                    String::new()
                };
                AxisLabel { x, text, turn }
            })
            .collect();
        let title = |category: &str, s: &Series, si: usize, i: usize, v: f64| {
            if series.len() == 1 {
                format!("{category}: {}", display(si, i, v))
            } else {
                format!("{category} · {}: {}", s.name, display(si, i, v))
            }
        };
        match kind {
            ChartKind::Line => {
                for (si, s) in series.iter().enumerate() {
                    let mut runs = Vec::new();
                    let mut run = Vec::new();
                    let mut dots = Vec::new();
                    for (i, category) in categories.iter().enumerate() {
                        let x = left + (i as f64 + 0.5) * band;
                        match value(s, i) {
                            Some(v) => {
                                run.push(format!("{},{}", num(x), num(y(v))));
                                dots.push(Dot {
                                    x: num(x),
                                    y: num(y(v)),
                                    title: title(category, s, si, i, v),
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
                            left + i as f64 * band + (band - w) / 2.0,
                            w,
                            y(to),
                            y(from),
                            FILL[si],
                            title(category, s, si, i, v),
                            (si, i),
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
                        let x = left + i as f64 * band + (band - inner) / 2.0 + si as f64 * w;
                        self.rects.push(bar(
                            x,
                            w,
                            y(v.max(0.0)),
                            y(v.min(0.0)),
                            FILL[si],
                            title(category, s, si, i, v),
                            (si, i),
                        ));
                    }
                }
                self.summary = format!("Bar chart of {} across {n} values.", names(series));
            }
        }
    }

    /// Slices of the first series' positive values, clockwise from the top.
    fn pie(
        &mut self,
        categories: &[String],
        series: &Series,
        donut: bool,
        value: impl Fn(&Series, usize) -> Option<f64>,
        display: &impl Fn(usize, usize, f64) -> String,
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
        if !total.is_finite() {
            self.note = TOO_LARGE.into();
            return;
        }
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
                    display(0, *i, *v),
                    percent
                ),
                at: (0, *i),
                drill: String::new(),
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

fn bar(
    x: f64,
    w: f64,
    top: f64,
    bottom: f64,
    class: &'static str,
    title: String,
    at: (usize, usize),
) -> Mark {
    Mark {
        x: num(x),
        y: num(top),
        w: num(w.max(0.5)),
        h: num((bottom - top).max(0.0)),
        d: String::new(),
        class,
        title,
        at,
        drill: String::new(),
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

/// How a value axis writes its ticks: in thousands, millions, billions or
/// trillions when the axis reaches that far, with just the decimals its step
/// needs, so `0.0001` steps and trillion-scale axes both read in a few
/// characters. Past a quadrillion, or below a millionth, in scientific form.
struct Scale {
    unit: f64,
    suffix: &'static str,
    decimals: usize,
    scientific: bool,
    /// A tick this close to zero is zero (float error from adding steps).
    zero: f64,
}
impl Scale {
    fn new(reach: f64, step: f64) -> Self {
        let (unit, suffix) = [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e4, "K")]
            .into_iter()
            .find(|(unit, _)| reach >= *unit)
            .map_or((1.0, ""), |(unit, suffix)| (unit, suffix));
        let unit = if suffix == "K" { 1e3 } else { unit };
        let scaled = step / unit;
        let decimals = (0..=6)
            .find(|d| {
                let s = scaled * 10f64.powi(*d as i32);
                (s - s.round()).abs() < 1e-6 * s.abs().max(1.0)
            })
            .unwrap_or(6);
        Scale {
            unit,
            suffix,
            decimals,
            scientific: reach >= 1e15 || step < 1e-6,
            zero: step * 1e-9,
        }
    }
    fn label(&self, value: f64) -> String {
        if value.abs() <= self.zero {
            return "0".into();
        }
        if self.scientific {
            return format!("{value:.1e}");
        }
        let text = format!("{:.*}", self.decimals, (value / self.unit).abs());
        format!("{}{}", grouped(value < 0.0, &text), self.suffix)
    }
}

/// A value as the table, a mark's title and a KPI give it: exact (the
/// shortest text that reads back as the same float), thousands grouped, so
/// `0.004` and `1,234.567` keep every digit. Scientific past a quadrillion
/// or below a millionth.
fn format_exact(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    let abs = value.abs();
    if abs != 0.0 && !(1e-6..1e15).contains(&abs) {
        return format!("{value:e}");
    }
    grouped(value < 0.0, &abs.to_string())
}

/// Plain digits with thousands grouped by commas, and a minus only where a
/// digit other than zero is shown (`-0.00` is `0.00`).
fn grouped(negative: bool, text: &str) -> String {
    let (int, frac) = text.split_once('.').unwrap_or((text, ""));
    let mut out = String::new();
    if negative && text.bytes().any(|b| b.is_ascii_digit() && b != b'0') {
        out.push('-');
    }
    for (i, digit) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    out
}

/// Round away floating-point summation noise while retaining small totals.
fn format_number(value: f64) -> String {
    let rounded = format!("{value:.14e}").parse::<f64>().unwrap_or(value);
    format_exact(rounded)
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
    // One decimal, always written.
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
