//! Charts drawn with the egui painter: history graphs with axes, a hover
//! read-out and statistics, sparklines for tiles and small multiples, plus
//! the usage bars. No plotting library is involved.
//!
//! Colour rules: every metric family has one colour that it keeps on every
//! page (processor blue, memory orange, graphics aqua, drives yellow, network
//! magenta, battery green); the second line of a two-line chart (write,
//! upload) is violet. Text never takes a series colour: a coloured dot next
//! to it carries the identity. Good / caution / bad colours are reserved for
//! states and always come with a word.

use super::theme::{pal, regular, semibold, Palette};
use super::widgets;
use crate::history::{Point, Stats};
use crate::sampler::Shared;
use crate::util;
use eframe::egui::{
    self, epaint::Mesh, epaint::PathShape, pos2, vec2, Align2, Color32, CornerRadius, Id, Pos2,
    Rect, Sense, Shape, Stroke, Ui,
};
use std::sync::{Arc, Mutex};

// ----------------------------------------------------------------------------------
// Colours
// ----------------------------------------------------------------------------------

/// One colour per metric family, chosen separately for each theme.
#[derive(Clone, Copy)]
pub struct Series {
    pub cpu: Color32,
    pub memory: Color32,
    pub gpu: Color32,
    pub disk: Color32,
    pub net: Color32,
    pub battery: Color32,
    /// The second line of a two-line chart: written data, uploads.
    pub second: Color32,
}

pub fn series(dark: bool) -> Series {
    if dark {
        Series {
            cpu: Color32::from_rgb(0x39, 0x87, 0xe5),
            memory: Color32::from_rgb(0xd9, 0x59, 0x26),
            gpu: Color32::from_rgb(0x19, 0x9e, 0x70),
            disk: Color32::from_rgb(0xc9, 0x85, 0x00),
            net: Color32::from_rgb(0xd5, 0x51, 0x81),
            battery: Color32::from_rgb(0x0c, 0xa3, 0x0c),
            second: Color32::from_rgb(0x90, 0x85, 0xe9),
        }
    } else {
        Series {
            cpu: Color32::from_rgb(0x2a, 0x78, 0xd6),
            memory: Color32::from_rgb(0xeb, 0x68, 0x34),
            gpu: Color32::from_rgb(0x1b, 0xaf, 0x7a),
            disk: Color32::from_rgb(0xed, 0xa1, 0x00),
            net: Color32::from_rgb(0xe8, 0x7b, 0xa4),
            battery: Color32::from_rgb(0x00, 0x83, 0x00),
            second: Color32::from_rgb(0x62, 0x50, 0xd6),
        }
    }
}

pub fn series_ui(ui: &Ui) -> Series {
    series(ui.visuals().dark_mode)
}

/// The colour at a fraction of its opacity. Done on the premultiplied
/// values, which is what keeps the hue when it is blended over the card.
fn alpha(color: Color32, a: f32) -> Color32 {
    color.gamma_multiply(a.clamp(0.0, 1.0))
}

// ----------------------------------------------------------------------------------
// Units and axes (pure)
// ----------------------------------------------------------------------------------

/// What a chart measures. Values are stored as the data model stores them:
/// percent, degrees Celsius, bytes per second, megahertz, watts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Percent,
    Temperature,
    Rate,
    Frequency,
    Watts,
    Rpm,
}

/// The stored value in the unit it is shown in (only temperatures change).
pub fn to_display(unit: Unit, value: f32, fahrenheit: bool) -> f32 {
    if unit == Unit::Temperature && fahrenheit {
        util::c_to_f(value)
    } else {
        value
    }
}

/// A stored value in words: "42%", "61 °C", "12.4 MB/s", "4.20 GHz", "13.2 W".
pub fn fmt_value(unit: Unit, value: f32, fahrenheit: bool) -> String {
    match unit {
        Unit::Percent => format!("{:.0}%", value.clamp(0.0, 100.0)),
        Unit::Temperature => util::fmt_temp(value, fahrenheit),
        Unit::Rate => util::fmt_rate(f64::from(value)),
        Unit::Frequency => util::fmt_mhz(value),
        Unit::Watts => format!("{value:.1} W"),
        Unit::Rpm => format!("{} RPM", util::fmt_count(value.max(0.0).round() as u64)),
    }
}

/// An axis label for a value that is already in display units.
pub fn fmt_tick(unit: Unit, shown: f32, fahrenheit: bool) -> String {
    match unit {
        Unit::Percent => format!("{shown:.0}%"),
        Unit::Temperature => format!("{shown:.0} {}", if fahrenheit { "°F" } else { "°C" }),
        Unit::Rate => {
            if shown <= 0.0 {
                "0".to_string()
            } else {
                let mut v = f64::from(shown);
                let mut unit = 0;
                const UNITS: [&str; 5] = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"];
                while v >= 1024.0 && unit < UNITS.len() - 1 {
                    v /= 1024.0;
                    unit += 1;
                }
                if (v - v.round()).abs() < 0.05 {
                    format!("{v:.0} {}", UNITS[unit])
                } else {
                    format!("{v:.1} {}", UNITS[unit])
                }
            }
        }
        Unit::Frequency => {
            if shown <= 0.0 {
                "0".to_string()
            } else {
                let ghz = shown / 1000.0;
                if (ghz - ghz.round()).abs() < 0.05 {
                    format!("{ghz:.0} GHz")
                } else {
                    format!("{ghz:.1} GHz")
                }
            }
        }
        Unit::Watts => format!("{shown:.0} W"),
        Unit::Rpm => util::fmt_count(shown.max(0.0).round() as u64),
    }
}

/// The vertical scale of a chart, in display units.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub lo: f32,
    pub hi: f32,
    pub ticks: Vec<f32>,
}

impl Axis {
    /// Where `shown` (display units) lands between `bottom` and `top` pixels.
    pub fn y(&self, shown: f32, top: f32, bottom: f32) -> f32 {
        let span = (self.hi - self.lo).max(f32::EPSILON);
        let u = ((shown - self.lo) / span).clamp(0.0, 1.0);
        bottom - u * (bottom - top)
    }
}

/// The smallest of 1, 2, 2.5, 5, 10 times a power of ten that is not below `v`.
pub fn nice_ceil(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return 1.0;
    }
    let exp = v.log10().floor();
    let base = 10f64.powf(exp);
    let frac = v / base;
    let nice = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .find(|n| frac <= *n * (1.0 + 1e-9))
        .unwrap_or(10.0);
    nice * base
}

/// Round maxima for transfer rates in bytes per second: binary units, and
/// numbers whose quarters are round too.
pub fn nice_rate_max(v: f64) -> f64 {
    const STEPS: [f64; 16] = [
        1.0, 2.0, 4.0, 8.0, 12.0, 16.0, 20.0, 40.0, 60.0, 80.0, 100.0, 120.0, 160.0, 200.0, 400.0,
        800.0,
    ];
    // Never zoom in further than 100 KB/s: idle noise should look idle.
    let v = if v.is_finite() {
        v.max(100.0 * 1024.0)
    } else {
        100.0 * 1024.0
    };
    let mut unit = 1024.0_f64;
    while v / unit > 800.0 {
        unit *= 1024.0;
    }
    let scaled = v / unit;
    let step = STEPS
        .into_iter()
        .find(|s| scaled <= *s * (1.0 + 1e-9))
        .unwrap_or(800.0);
    step * unit
}

fn even_ticks(lo: f32, hi: f32, intervals: usize) -> Vec<f32> {
    (0..=intervals)
        .map(|i| lo + (hi - lo) * i as f32 / intervals as f32)
        .collect()
}

/// A scale from a "nice" step: `lo` and `hi` are multiples of the step that
/// enclose the data, with about four intervals.
pub fn nice_range(lo: f64, hi: f64) -> Axis {
    let (lo, hi) = if hi > lo { (lo, hi) } else { (lo, lo + 1.0) };
    let step = nice_ceil((hi - lo) / 4.0);
    let first = (lo / step).floor();
    let last = (hi / step).ceil();
    let ticks = ((first as i64)..=(last as i64))
        .map(|i| (i as f64 * step) as f32)
        .collect();
    Axis {
        lo: (first * step) as f32,
        hi: (last * step) as f32,
        ticks,
    }
}

/// The scale for a chart whose data (in display units) spans `data_min..=data_max`.
pub fn axis_for(unit: Unit, data_min: f32, data_max: f32) -> Axis {
    let (data_min, data_max) = if data_min.is_finite() && data_max.is_finite() {
        (data_min.min(data_max), data_max.max(data_min))
    } else {
        (0.0, 0.0)
    };
    match unit {
        Unit::Percent => Axis {
            lo: 0.0,
            hi: 100.0,
            ticks: even_ticks(0.0, 100.0, 4),
        },
        Unit::Rate => {
            let hi = nice_rate_max(f64::from(data_max)) as f32;
            Axis {
                lo: 0.0,
                hi,
                ticks: even_ticks(0.0, hi, 4),
            }
        }
        Unit::Frequency => {
            let top = (f64::from(data_max.max(1.0)) / 1000.0).ceil().max(1.0);
            let step = if top > 6.0 { 2.0 } else { 1.0 };
            let top = (top / step).ceil() * step;
            Axis {
                lo: 0.0,
                hi: (top * 1000.0) as f32,
                ticks: (0..=((top / step) as usize))
                    .map(|i| (i as f64 * step * 1000.0) as f32)
                    .collect(),
            }
        }
        Unit::Temperature => {
            // Tens of degrees around the data, at least a 20 degree span.
            let mut lo = ((f64::from(data_min) - 2.0) / 10.0).floor() * 10.0;
            let mut hi = ((f64::from(data_max) + 2.0) / 10.0).ceil() * 10.0;
            if hi - lo < 20.0 {
                hi = lo + 20.0;
            }
            let mut step = 10.0;
            while (hi - lo) / step > 5.0 {
                step *= 2.0;
            }
            lo = (lo / step).floor() * step;
            hi = (hi / step).ceil() * step;
            Axis {
                lo: lo as f32,
                hi: hi as f32,
                ticks: (0..=(((hi - lo) / step).round() as usize))
                    .map(|i| (lo + i as f64 * step) as f32)
                    .collect(),
            }
        }
        Unit::Watts | Unit::Rpm => {
            let lo = f64::from(data_min.min(0.0));
            let hi = f64::from(data_max.max(0.0));
            let floor = if unit == Unit::Rpm { 1000.0 } else { 4.0 };
            if lo < 0.0 {
                nice_range(lo, hi.max(lo + floor))
            } else {
                let top = nice_ceil(hi.max(floor));
                Axis {
                    lo: 0.0,
                    hi: top as f32,
                    ticks: even_ticks(0.0, top as f32, 4),
                }
            }
        }
    }
}

/// When a chart ends and how long it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeAxis {
    pub window_s: f64,
    /// Wall-clock time of the right edge.
    pub end_unix: u64,
    /// False while the graphs are paused.
    pub live: bool,
    pub fahrenheit: bool,
    /// Seconds local time is ahead of UTC.
    pub utc_offset_s: i64,
}

fn clock(seconds_of_day: i64, with_seconds: bool) -> String {
    let s = seconds_of_day.rem_euclid(86_400);
    if with_seconds {
        format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 3600, s % 3600 / 60)
    }
}

/// Time labels along the bottom: (seconds relative to the right edge, text).
/// Short windows count back from "now"; long ones show clock times on round
/// minutes or hours.
pub fn time_ticks(axis: &TimeAxis) -> Vec<(f64, String)> {
    let w = axis.window_s;
    let local_end = axis.end_unix as i64 + axis.utc_offset_s;
    if w <= 600.0 {
        let step = if w <= 60.0 {
            15.0
        } else if w <= 300.0 {
            60.0
        } else {
            120.0
        };
        let mut out = Vec::new();
        let mut t = 0.0;
        while t <= w + 1e-6 {
            let label = if t == 0.0 {
                if axis.live {
                    "now".to_string()
                } else {
                    clock(local_end, true)
                }
            } else if step < 60.0 {
                format!("-{t:.0} s")
            } else {
                format!("-{:.0} min", t / 60.0)
            };
            out.push((-t, label));
            t += step;
        }
        out.reverse();
        out
    } else {
        let step: i64 = if w <= 3600.0 {
            600
        } else if w <= 7200.0 {
            1200
        } else if w <= 21_600.0 {
            3600
        } else {
            4 * 3600
        };
        let mut tick = local_end.div_euclid(step) * step;
        let mut out = Vec::new();
        while (local_end - tick) as f64 <= w {
            out.push((-((local_end - tick) as f64), clock(tick, false)));
            tick -= step;
        }
        out.reverse();
        out
    }
}

/// "12 s ago", "3 min 20 s ago" or a clock time, for the hover read-out.
pub fn hover_time(axis: &TimeAxis, t: f64) -> String {
    if axis.window_s > 600.0 || !axis.live {
        let at = axis.end_unix as i64 + axis.utc_offset_s + t.round() as i64;
        clock(at, axis.window_s <= 7200.0)
    } else {
        let back = (-t).max(0.0).round() as u64;
        if back == 0 {
            "now".to_string()
        } else {
            format!("{} ago", util::fmt_duration(back))
        }
    }
}

/// The horizontal pixel of time `t` (zero or negative seconds).
pub fn time_x(t: f64, window_s: f64, left: f32, right: f32) -> f32 {
    let u = (1.0 + t / window_s.max(1e-9)).clamp(0.0, 1.0) as f32;
    left + u * (right - left)
}

// ----------------------------------------------------------------------------------
// History access
// ----------------------------------------------------------------------------------

/// One line of a chart with the data it needs.
#[derive(Clone, Debug)]
pub struct Line {
    pub label: String,
    pub color: Color32,
    pub points: Vec<Point>,
    pub stats: Option<Stats>,
}

/// Short-lived view of the graph history. Every query locks the monitor's
/// shared state only for the copy.
pub struct Hist {
    pub shared: Arc<Mutex<Shared>>,
    pub now: f64,
    pub window_s: f64,
}

impl Hist {
    pub fn line(&self, label: &str, color: Color32, key: &str) -> Line {
        let (points, stats) = match self.shared.lock() {
            Ok(s) => (
                s.history.points(key, self.now, self.window_s),
                s.history.stats(key, self.now, self.window_s),
            ),
            Err(_) => (Vec::new(), None),
        };
        Line {
            label: label.to_string(),
            color,
            points,
            stats,
        }
    }

    pub fn points(&self, key: &str) -> Vec<Point> {
        match self.shared.lock() {
            Ok(s) => s.history.points(key, self.now, self.window_s),
            Err(_) => Vec::new(),
        }
    }

    pub fn stats(&self, key: &str) -> Option<Stats> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.history.stats(key, self.now, self.window_s))
    }

    /// Several series at once under a single lock (for the per-core strip).
    pub fn many(&self, keys: &[String]) -> Vec<Vec<Point>> {
        match self.shared.lock() {
            Ok(s) => keys
                .iter()
                .map(|k| s.history.points(k, self.now, self.window_s))
                .collect(),
            Err(_) => keys.iter().map(|_| Vec::new()).collect(),
        }
    }
}

// ----------------------------------------------------------------------------------
// Drawing
// ----------------------------------------------------------------------------------

fn grid_color(p: &Palette) -> Color32 {
    alpha(p.border, if p.dark { 0.75 } else { 0.9 })
}

/// Paints the band, the soft fill and the line of one series into `plot`.
#[allow(clippy::too_many_arguments)]
fn paint_series(
    painter: &egui::Painter,
    plot: Rect,
    points: &[Point],
    color: Color32,
    axis: &Axis,
    window_s: f64,
    map: &dyn Fn(f32) -> f32,
    fill: f32,
    width: f32,
) {
    if points.is_empty() {
        return;
    }
    let x_of = |t: f64| time_x(t, window_s, plot.left(), plot.right());
    let y_of = |v: f32| axis.y(map(v), plot.top(), plot.bottom());
    let zero_y = axis.y(0.0_f32.clamp(axis.lo, axis.hi), plot.top(), plot.bottom());
    let line: Vec<Pos2> = points.iter().map(|q| pos2(x_of(q.t), y_of(q.v))).collect();

    // The range inside each bucket, for the long windows.
    if points.iter().any(|q| q.max - q.min > f32::EPSILON) && points.len() > 1 {
        let mut mesh = Mesh::default();
        let c = alpha(color, 0.2);
        for (i, q) in points.iter().enumerate() {
            let x = x_of(q.t);
            mesh.colored_vertex(pos2(x, y_of(q.max)), c);
            mesh.colored_vertex(pos2(x, y_of(q.min)), c);
            if i > 0 {
                let b = (i * 2) as u32;
                mesh.add_triangle(b - 2, b - 1, b);
                mesh.add_triangle(b - 1, b, b + 1);
            }
        }
        painter.add(Shape::mesh(mesh));
    }
    // A wash that fades towards the baseline.
    if fill > 0.0 && line.len() > 1 {
        let mut mesh = Mesh::default();
        let top = alpha(color, fill);
        let bottom = alpha(color, fill * 0.08);
        for (i, q) in line.iter().enumerate() {
            mesh.colored_vertex(*q, top);
            mesh.colored_vertex(pos2(q.x, zero_y), bottom);
            if i > 0 {
                let b = (i * 2) as u32;
                mesh.add_triangle(b - 2, b - 1, b);
                mesh.add_triangle(b - 1, b, b + 1);
            }
        }
        painter.add(Shape::mesh(mesh));
    }
    if line.len() == 1 {
        painter.circle_filled(line[0], width, color);
    } else {
        painter.add(PathShape::line(line, Stroke::new(width, color)));
    }
}

const STATS_ROW: f32 = 16.0;

/// The height a chart needs besides its plot: title row, statistics rows
/// and the time labels.
pub fn chrome_height(lines: usize) -> f32 {
    22.0 + STATS_ROW * lines as f32 + 4.0 + 20.0
}

fn data_range(lines: &[Line], map: &dyn Fn(f32) -> f32) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for q in lines.iter().flat_map(|l| l.points.iter()) {
        lo = lo.min(map(q.min));
        hi = hi.max(map(q.max));
    }
    if lo.is_finite() && hi.is_finite() {
        (lo, hi)
    } else {
        (0.0, 0.0)
    }
}

fn nearest(points: &[Point], t: f64) -> Option<&Point> {
    points.iter().min_by(|a, b| {
        (a.t - t)
            .abs()
            .partial_cmp(&(b.t - t).abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// A history chart: header with the current values and min / max / avg,
/// then the plot with a value axis, time labels and a hover read-out.
/// `height` is the height of the plot area itself. `empty_note` is shown
/// instead of the plot when there is nothing to draw.
#[allow(clippy::too_many_arguments)]
pub fn chart(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    title: &str,
    unit: Unit,
    lines: &[Line],
    height: f32,
    time: &TimeAxis,
    empty_note: &str,
) {
    let p = pal(ui);
    let width = ui.available_width();
    let f = time.fahrenheit;
    let map = move |v: f32| to_display(unit, v, f);
    let has_data = lines.iter().any(|l| !l.points.is_empty());
    let spacing = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);

    // Header, first row: title on the left, current values on the right.
    let (head, _) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
    let painter = ui.painter().clone();
    let mut right = head.right();
    for line in lines.iter().rev() {
        let Some(stats) = line.stats else { continue };
        let value = fmt_value(unit, stats.last, f);
        let g = painter.layout_no_wrap(value, semibold(14.0), p.text);
        right -= g.size().x;
        painter.galley(pos2(right, head.center().y - g.size().y / 2.0), g, p.text);
        if lines.len() > 1 {
            let g = painter.layout_no_wrap(line.label.clone(), regular(12.5), p.muted);
            right -= g.size().x + 6.0;
            painter.galley(pos2(right, head.center().y - g.size().y / 2.0), g, p.muted);
        }
        right -= 11.0;
        painter.circle_filled(pos2(right + 3.0, head.center().y + 1.0), 4.0, line.color);
        right -= 14.0;
    }
    widgets::text_in(
        ui,
        Rect::from_min_max(
            head.min,
            pos2((right - 6.0).max(head.left() + 40.0), head.bottom()),
        ),
        title,
        semibold(14.5),
        p.text,
    );

    // Below it: the statistics over the window, one row per line.
    for line in lines {
        let (sub, _) = ui.allocate_exact_size(vec2(width, STATS_ROW), Sense::hover());
        let Some(s) = line.stats else { continue };
        let body = format!(
            "min {} · max {} · avg {}",
            fmt_value(unit, s.min, f),
            fmt_value(unit, s.max, f),
            fmt_value(unit, s.avg, f)
        );
        let text = if lines.len() > 1 {
            format!("{}: {body}", line.label)
        } else {
            body
        };
        widgets::text_in(ui, sub, &text, regular(12.0), p.muted);
    }
    ui.add_space(4.0);

    // Plot.
    let (min, max) = data_range(lines, &map);
    let axis = axis_for(unit, min, max);
    let labels: Vec<String> = axis.ticks.iter().map(|t| fmt_tick(unit, *t, f)).collect();
    let gutter = labels
        .iter()
        .map(|l| {
            painter
                .layout_no_wrap(l.clone(), regular(11.0), p.faint)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
        + 8.0;
    let (outer, _) = ui.allocate_exact_size(vec2(width, height + 20.0), Sense::hover());
    ui.spacing_mut().item_spacing.y = spacing;
    let plot = Rect::from_min_max(
        pos2(outer.left() + gutter, outer.top() + 6.0),
        pos2(outer.right() - 6.0, outer.top() + height),
    );
    if !has_data {
        painter.rect_filled(plot, CornerRadius::same(6), alpha(p.hover, 0.5));
        painter.text(
            plot.center(),
            Align2::CENTER_CENTER,
            if empty_note.is_empty() {
                "No readings yet"
            } else {
                empty_note
            },
            regular(13.0),
            p.faint,
        );
        return;
    }
    for (tick, label) in axis.ticks.iter().zip(&labels) {
        let y = axis.y(*tick, plot.top(), plot.bottom()).round() + 0.5;
        let strong = *tick == 0.0 || *tick == axis.lo;
        painter.hline(
            plot.x_range(),
            y,
            Stroke::new(
                1.0_f32,
                if strong {
                    p.border
                } else {
                    alpha(grid_color(&p), 0.55)
                },
            ),
        );
        painter.text(
            pos2(plot.left() - 7.0, y),
            Align2::RIGHT_CENTER,
            label,
            regular(11.0),
            p.faint,
        );
    }
    let mut last_right = f32::NEG_INFINITY;
    let ticks = time_ticks(time);
    // Right-most label first, so that "now" always shows.
    let mut placed: Vec<(f32, std::sync::Arc<egui::Galley>)> = Vec::new();
    let mut limit = f32::INFINITY;
    for (t, label) in ticks.iter().rev() {
        let x = time_x(*t, time.window_s, plot.left(), plot.right());
        let g = painter.layout_no_wrap(label.clone(), regular(11.0), p.faint);
        let left = (x - g.size().x / 2.0).clamp(plot.left(), plot.right() - g.size().x);
        if left + g.size().x + 10.0 > limit || left < last_right {
            continue;
        }
        limit = left;
        painter.vline(
            x.round() + 0.5,
            plot.bottom()..=plot.bottom() + 3.0,
            Stroke::new(1.0_f32, p.border),
        );
        placed.push((left, g));
    }
    for (left, g) in placed {
        last_right = last_right.max(left);
        painter.galley(pos2(left, plot.bottom() + 5.0), g, p.faint);
    }

    let clip = painter.with_clip_rect(plot.expand2(vec2(1.0, 2.0)));
    let fill = if lines.len() == 1 { 0.2 } else { 0.09 };
    for line in lines {
        paint_series(
            &clip,
            plot,
            &line.points,
            line.color,
            &axis,
            time.window_s,
            &map,
            fill,
            1.75,
        );
    }
    if time.live {
        for line in lines {
            if let Some(q) = line.points.last() {
                let at = pos2(
                    time_x(q.t, time.window_s, plot.left(), plot.right()),
                    axis.y(map(q.v), plot.top(), plot.bottom()),
                );
                painter.circle_filled(at, 4.5, p.card);
                painter.circle_filled(at, 3.0, line.color);
            }
        }
    }

    // Hover read-out.
    let resp = ui.interact(plot, ui.id().with(("chart", id)), Sense::hover());
    let Some(pointer) = resp.hover_pos() else {
        return;
    };
    let u = ((pointer.x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
    let t = (f64::from(u) - 1.0) * time.window_s;
    let Some(anchor) = lines.iter().find_map(|l| nearest(&l.points, t)) else {
        return;
    };
    let t = anchor.t;
    let x = time_x(t, time.window_s, plot.left(), plot.right());
    painter.vline(x, plot.y_range(), Stroke::new(1.0_f32, p.faint));
    let mut rows: Vec<(Color32, String, String)> = Vec::new();
    for line in lines {
        let Some(q) = nearest(&line.points, t) else {
            continue;
        };
        let at = pos2(x, axis.y(map(q.v), plot.top(), plot.bottom()));
        painter.circle_filled(at, 5.5, p.card);
        painter.circle_filled(at, 3.5, line.color);
        let mut value = fmt_value(unit, q.v, f);
        if q.max - q.min > f32::EPSILON {
            value.push_str(&format!(
                "  ({} to {})",
                fmt_value(unit, q.min, f),
                fmt_value(unit, q.max, f)
            ));
        }
        rows.push((line.color, line.label.clone(), value));
    }
    let top = egui::Painter::new(
        ui.ctx().clone(),
        egui::LayerId::new(egui::Order::Tooltip, Id::new("chart-hover")),
        ui.ctx().screen_rect(),
    );
    let when = top.layout_no_wrap(hover_time(time, t), regular(11.5), p.muted);
    let galleys: Vec<_> = rows
        .iter()
        .map(|(_, label, value)| {
            (
                top.layout_no_wrap(label.clone(), regular(12.5), p.muted),
                top.layout_no_wrap(value.clone(), semibold(13.0), p.text),
            )
        })
        .collect();
    let label_w = galleys.iter().map(|g| g.0.size().x).fold(0.0_f32, f32::max);
    let value_w = galleys.iter().map(|g| g.1.size().x).fold(0.0_f32, f32::max);
    let body_w = (14.0 + label_w + 10.0 + value_w).max(when.size().x);
    let size = vec2(body_w + 20.0, 16.0 + 18.0 + 20.0 * rows.len() as f32);
    let mut origin = pos2(x + 12.0, pointer.y - size.y - 10.0);
    if origin.x + size.x > plot.right() {
        origin.x = x - 12.0 - size.x;
    }
    origin.y = origin.y.max(plot.top() - 30.0);
    let rect = Rect::from_min_size(origin, size);
    top.rect_filled(rect.expand(1.0), CornerRadius::same(9), p.border);
    top.rect_filled(
        rect,
        CornerRadius::same(8),
        if p.dark { p.hover } else { p.card },
    );
    top.galley(rect.min + vec2(10.0, 8.0), when, p.muted);
    for (i, ((color, _, _), (label, value))) in rows.iter().zip(galleys).enumerate() {
        let y = rect.top() + 28.0 + 20.0 * i as f32;
        top.circle_filled(pos2(rect.left() + 14.0, y + 9.0), 4.0, *color);
        top.galley(pos2(rect.left() + 24.0, y), label, p.muted);
        top.galley(pos2(rect.right() - 10.0 - value.size().x, y), value, p.text);
    }
}

/// A line with a soft fill and nothing else, for tiles. `range` fixes the
/// scale (percentages); without it the data's own nice maximum is used.
pub fn sparkline(
    ui: &Ui,
    rect: Rect,
    points: &[Point],
    color: Color32,
    range: Option<(f32, f32)>,
    window_s: f64,
) {
    if points.is_empty() || rect.width() < 8.0 || rect.height() < 6.0 {
        return;
    }
    let axis = match range {
        Some((lo, hi)) => Axis {
            lo,
            hi,
            ticks: Vec::new(),
        },
        None => {
            let lo = points.iter().map(|q| q.min).fold(f32::INFINITY, f32::min);
            let hi = points
                .iter()
                .map(|q| q.max)
                .fold(f32::NEG_INFINITY, f32::max);
            let lo = lo.min(0.0);
            let span = (hi - lo).max(1.0);
            Axis {
                lo,
                hi: lo + span * 1.12,
                ticks: Vec::new(),
            }
        }
    };
    let painter = ui.painter().with_clip_rect(rect.expand(1.0));
    // Averages only: the bucket ranges would be noise at this size.
    let flat: Vec<Point> = points
        .iter()
        .map(|q| Point {
            t: q.t,
            v: q.v,
            min: q.v,
            max: q.v,
        })
        .collect();
    paint_series(
        &painter,
        rect,
        &flat,
        color,
        &axis,
        window_s,
        &|v| v,
        0.16,
        1.5,
    );
}

/// One cell of a small-multiples grid: label, current value and a sparkline
/// on a fixed 0 to 100 scale.
pub fn mini_chart(
    ui: &mut Ui,
    size: egui::Vec2,
    label: &str,
    value: &str,
    points: &[Point],
    color: Color32,
    window_s: f64,
) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(8),
        alpha(p.hover, if p.dark { 0.55 } else { 0.7 }),
    );
    let head = Rect::from_min_max(
        rect.min + vec2(8.0, 4.0),
        pos2(rect.right() - 8.0, rect.top() + 22.0),
    );
    widgets::text_in(ui, head, label, regular(11.5), p.muted);
    widgets::text_in_right(ui, head, value, semibold(12.0), p.text);
    let plot = Rect::from_min_max(
        pos2(rect.left() + 1.0, rect.top() + 24.0),
        rect.max - vec2(1.0, 3.0),
    );
    ui.painter().hline(
        plot.x_range(),
        plot.center().y,
        Stroke::new(1.0_f32, alpha(grid_color(&p), 0.4)),
    );
    sparkline(ui, plot, points, color, Some((0.0, 100.0)), window_s);
}

/// A usage bar: the fill on a lighter track of the same colour.
pub fn meter(ui: &mut Ui, fraction: f32, color: Color32, width: f32, height: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    paint_meter(ui, rect, fraction, color);
    rect
}

pub fn paint_meter(ui: &Ui, rect: Rect, fraction: f32, color: Color32) {
    let p = pal(ui);
    let radius = CornerRadius::same((rect.height() / 2.0).min(6.0) as u8);
    ui.painter()
        .rect_filled(rect, radius, alpha(color, if p.dark { 0.2 } else { 0.18 }));
    let f = fraction.clamp(0.0, 1.0);
    if f > 0.0 {
        let fill = Rect::from_min_max(
            rect.min,
            pos2(
                rect.left() + (rect.width() * f).max(rect.height().min(rect.width())),
                rect.bottom(),
            ),
        );
        ui.painter().rect_filled(fill, radius, color);
    }
}

/// One whole split into parts, with a gap in the surface colour between
/// them. `parts` are (share of the whole, colour); shares at zero draw nothing.
pub fn segmented_bar(ui: &mut Ui, parts: &[(f32, Color32)], height: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let total: f32 = parts.iter().map(|q| q.0.max(0.0)).sum();
    if total <= 0.0 {
        return rect;
    }
    let visible = parts.iter().filter(|q| q.0 > 0.0).count();
    let gap = 2.0;
    let usable = rect.width() - gap * visible.saturating_sub(1) as f32;
    let mut x = rect.left();
    for (share, color) in parts.iter().filter(|q| q.0 > 0.0) {
        let w = usable * share / total;
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(w.max(1.0), rect.height()));
        ui.painter().rect_filled(r, CornerRadius::same(4), *color);
        x += w + gap;
    }
    rect
}

/// A legend entry: coloured dot, name and value in text colours.
pub fn legend_item(ui: &mut Ui, color: Color32, label: &str, value: &str) {
    let p = pal(ui);
    let name = ui
        .painter()
        .layout_no_wrap(label.to_string(), regular(12.5), p.muted);
    let val = ui
        .painter()
        .layout_no_wrap(value.to_string(), semibold(12.5), p.text);
    let size = vec2(14.0 + name.size().x + 6.0 + val.size().x, 20.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .circle_filled(pos2(rect.left() + 4.0, rect.center().y + 1.0), 4.0, color);
    let y = |g: &egui::Galley| rect.center().y - g.size().y / 2.0;
    let name_w = name.size().x;
    ui.painter()
        .galley(pos2(rect.left() + 14.0, y(&name)), name, p.muted);
    ui.painter()
        .galley(pos2(rect.left() + 20.0 + name_w, y(&val)), val, p.text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_maxima() {
        assert_eq!(nice_ceil(0.0), 1.0);
        assert_eq!(nice_ceil(f64::NAN), 1.0);
        assert_eq!(nice_ceil(1.0), 1.0);
        assert_eq!(nice_ceil(1.2), 2.0);
        assert_eq!(nice_ceil(2.3), 2.5);
        assert_eq!(nice_ceil(26.0), 50.0);
        assert_eq!(nice_ceil(50.0), 50.0);
        assert_eq!(nice_ceil(730.0), 1000.0);
        assert!((nice_ceil(0.03) - 0.05).abs() < 1e-12);
        let mb = 1024.0 * 1024.0;
        assert_eq!(nice_rate_max(0.0), 100.0 * 1024.0);
        assert_eq!(nice_rate_max(f64::INFINITY), 100.0 * 1024.0);
        assert_eq!(nice_rate_max(130.0 * 1024.0), 160.0 * 1024.0);
        assert_eq!(nice_rate_max(3.2 * mb), 4.0 * mb);
        assert_eq!(nice_rate_max(88.0 * mb), 100.0 * mb);
        assert_eq!(nice_rate_max(801.0 * mb), 1024.0 * mb);
        assert_eq!(nice_rate_max(900.0 * 1024.0), mb);
    }

    #[test]
    fn axes_enclose_the_data() {
        let a = axis_for(Unit::Percent, 12.0, 31.0);
        assert_eq!((a.lo, a.hi), (0.0, 100.0));
        assert_eq!(a.ticks, vec![0.0, 25.0, 50.0, 75.0, 100.0]);

        let mb = 1024.0 * 1024.0;
        let a = axis_for(Unit::Rate, 0.0, 37.0 * mb);
        assert_eq!(a.hi, 40.0 * mb);
        assert_eq!(a.ticks.len(), 5);
        assert_eq!(a.ticks[1], 10.0 * mb);

        let a = axis_for(Unit::Frequency, 900.0, 4720.0);
        assert_eq!((a.lo, a.hi), (0.0, 5000.0));
        assert_eq!(a.ticks.len(), 6);

        let a = axis_for(Unit::Temperature, 44.0, 93.0);
        assert!(a.lo <= 44.0 && a.hi >= 93.0);
        assert!(a.ticks.len() <= 6 && a.ticks.len() >= 3);
        assert!(a.ticks.windows(2).all(|w| w[1] > w[0]));
        // A flat reading still gets a sensible span.
        let a = axis_for(Unit::Temperature, 36.0, 36.0);
        assert_eq!((a.lo, a.hi), (30.0, 50.0));

        // Charging and discharging: zero is a tick and both signs fit.
        let a = axis_for(Unit::Watts, -13.2, 21.5);
        assert!(a.lo <= -13.2 && a.hi >= 21.5);
        assert!(a.ticks.contains(&0.0));
        let a = axis_for(Unit::Watts, 3.0, 96.0);
        assert_eq!((a.lo, a.hi), (0.0, 100.0));

        // Nonsense input does not produce nonsense axes.
        let a = axis_for(Unit::Rate, f32::NAN, f32::INFINITY);
        assert!(a.hi.is_finite() && a.hi > 0.0);
        let a = nice_range(5.0, 5.0);
        assert!(a.hi > a.lo);
    }

    #[test]
    fn values_map_to_pixels() {
        let a = axis_for(Unit::Percent, 0.0, 100.0);
        assert_eq!(a.y(0.0, 10.0, 110.0), 110.0);
        assert_eq!(a.y(100.0, 10.0, 110.0), 10.0);
        assert_eq!(a.y(25.0, 10.0, 110.0), 85.0);
        // Out-of-range values are pinned to the frame.
        assert_eq!(a.y(250.0, 10.0, 110.0), 10.0);
        assert_eq!(a.y(-5.0, 10.0, 110.0), 110.0);
        assert_eq!(time_x(0.0, 300.0, 50.0, 350.0), 350.0);
        assert_eq!(time_x(-300.0, 300.0, 50.0, 350.0), 50.0);
        assert_eq!(time_x(-150.0, 300.0, 50.0, 350.0), 200.0);
        assert_eq!(time_x(-900.0, 300.0, 50.0, 350.0), 50.0);
        assert_eq!(to_display(Unit::Temperature, 100.0, true), 212.0);
        assert_eq!(to_display(Unit::Temperature, 100.0, false), 100.0);
        assert_eq!(to_display(Unit::Percent, 40.0, true), 40.0);
    }

    #[test]
    fn units_are_written_out() {
        assert_eq!(fmt_value(Unit::Percent, 41.6, false), "42%");
        assert_eq!(fmt_value(Unit::Temperature, 61.2, false), "61 °C");
        assert_eq!(fmt_value(Unit::Temperature, 100.0, true), "212 °F");
        assert_eq!(
            fmt_value(Unit::Rate, 2.0 * 1024.0 * 1024.0, false),
            "2.00 MB/s"
        );
        assert_eq!(fmt_value(Unit::Frequency, 4200.0, false), "4.20 GHz");
        assert_eq!(fmt_value(Unit::Watts, -13.24, false), "-13.2 W");
        assert_eq!(fmt_value(Unit::Rpm, 2340.0, false), "2,340 RPM");
        assert_eq!(fmt_tick(Unit::Percent, 75.0, false), "75%");
        assert_eq!(fmt_tick(Unit::Temperature, 60.0, false), "60 °C");
        assert_eq!(fmt_tick(Unit::Temperature, 140.0, true), "140 °F");
        assert_eq!(fmt_tick(Unit::Rate, 0.0, false), "0");
        assert_eq!(
            fmt_tick(Unit::Rate, 40.0 * 1024.0 * 1024.0, false),
            "40 MB/s"
        );
        assert_eq!(fmt_tick(Unit::Rate, 1536.0, false), "1.5 KB/s");
        assert_eq!(fmt_tick(Unit::Frequency, 3000.0, false), "3 GHz");
        assert_eq!(fmt_tick(Unit::Watts, -20.0, false), "-20 W");
        assert_eq!(fmt_tick(Unit::Rpm, 3000.0, false), "3,000");
    }

    fn axis(window_s: f64, live: bool) -> TimeAxis {
        TimeAxis {
            window_s,
            // 2026-11-16 14:37:20 UTC.
            end_unix: 1_794_839_840,
            live,
            fahrenheit: false,
            utc_offset_s: 3600,
        }
    }

    #[test]
    fn short_windows_count_back_from_now() {
        let t = time_ticks(&axis(300.0, true));
        let labels: Vec<&str> = t.iter().map(|x| x.1.as_str()).collect();
        assert_eq!(
            labels,
            ["-5 min", "-4 min", "-3 min", "-2 min", "-1 min", "now"]
        );
        assert_eq!(t[0].0, -300.0);
        assert_eq!(t[5].0, 0.0);
        let t = time_ticks(&axis(60.0, true));
        let labels: Vec<&str> = t.iter().map(|x| x.1.as_str()).collect();
        assert_eq!(labels, ["-60 s", "-45 s", "-30 s", "-15 s", "now"]);
        // Paused: the right edge is a moment in the past, so it gets its clock time.
        let t = time_ticks(&axis(600.0, false));
        assert_eq!(t.last().unwrap().1, "15:37:20");
        assert_eq!(t.len(), 6);
    }

    #[test]
    fn long_windows_show_round_clock_times() {
        let t = time_ticks(&axis(3600.0, true));
        let labels: Vec<&str> = t.iter().map(|x| x.1.as_str()).collect();
        assert_eq!(
            labels,
            ["14:40", "14:50", "15:00", "15:10", "15:20", "15:30"]
        );
        assert!((t.last().unwrap().0 + 440.0).abs() < 1e-9);
        assert!(t.iter().all(|x| x.0 <= 0.0 && x.0 >= -3600.0));
        let t = time_ticks(&axis(86_400.0, true));
        let labels: Vec<&str> = t.iter().map(|x| x.1.as_str()).collect();
        assert_eq!(
            labels,
            ["16:00", "20:00", "00:00", "04:00", "08:00", "12:00"]
        );
        assert!(t.windows(2).all(|w| w[1].0 > w[0].0));
        assert_eq!(hover_time(&axis(300.0, true), -200.0), "3 min 20 s ago");
        assert_eq!(hover_time(&axis(300.0, true), 0.0), "now");
        assert_eq!(hover_time(&axis(86_400.0, true), -3600.0), "14:37");
        assert_eq!(hover_time(&axis(3600.0, true), -20.0), "15:37:00");
    }
}
