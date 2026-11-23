//! The mini widget: a small borderless window that stays on top and shows
//! the headline readings. It is a second egui viewport that draws straight
//! from the monitor's shared state, so it keeps updating while the main
//! window is hidden, and it repaints only once per reading.

use super::app::App;
use super::charts;
use super::pages;
use super::theme::{pal, pal_ctx, regular, semibold, Icon};
use super::widgets;
use crate::model::Sample;
use crate::sampler::Shared;
use crate::util;
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, Frame, Margin, Rect, Sense, Stroke,
    StrokeKind, Ui,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Size of the widget window in points.
pub const MINI_SIZE: [f32; 2] = [236.0, 138.0];

/// What the widget and the main window tell each other.
#[derive(Default)]
pub struct MiniShared {
    /// Set by the widget when the user closes it.
    pub close_requested: AtomicBool,
    pub fahrenheit: AtomicBool,
    /// Milliseconds between repaints (the update interval).
    pub interval_ms: AtomicU32,
    /// Where the widget was last left, in screen points.
    pub position: Mutex<Option<(f32, f32)>>,
    /// True while the viewport is being shown.
    pub open: AtomicBool,
    /// Set by the widget when the user asks for the main window.
    pub show_main: AtomicBool,
    /// The main window, so that the widget can bring it back while that
    /// window's own event loop is asleep in the notification area.
    pub main_window: std::sync::atomic::AtomicIsize,
}

/// Where the widget may open: `pos` (its remembered top-left corner, in
/// points) if enough of the widget would be on the desktop to grab it, and
/// `None` (let Windows place it) if not, for instance because the display
/// it was left on is gone. `desktop` is the whole desktop as left, top,
/// width and height in points.
pub fn keep_on_screen(
    pos: Option<(f32, f32)>,
    size: [f32; 2],
    desktop: (f32, f32, f32, f32),
) -> Option<(f32, f32)> {
    /// How much of the widget must stay reachable, in points.
    const GRIP: f32 = 48.0;
    let (x, y) = pos?;
    let (left, top, width, height) = desktop;
    if !(x.is_finite() && y.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let on_x = x + size[0] >= left + GRIP && x <= left + width - GRIP;
    let on_y = y >= top && y <= top + height - GRIP;
    (on_x && on_y).then_some((x, y))
}

/// The few numbers the widget shows.
#[derive(Clone, Debug, PartialEq)]
pub struct MiniData {
    pub cpu_pct: f32,
    pub cpu_temp: Option<f32>,
    pub memory_pct: Option<f32>,
    pub gpu_pct: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub rx_bps: f64,
    pub tx_bps: f64,
}

/// Picks the widget's numbers out of a reading.
pub fn mini_data(sample: &Sample) -> MiniData {
    let (rx_bps, tx_bps) = sample.net_totals();
    MiniData {
        cpu_pct: sample.cpu.total_pct,
        cpu_temp: sample.cpu.temperature_c,
        memory_pct: (sample.memory.total > 0).then(|| sample.memory.used_pct()),
        gpu_pct: sample.gpu_load(),
        gpu_temp: sample.gpu_temp(),
        rx_bps,
        tx_bps,
    }
}

/// A rate short enough for the widget: "4.2 MB/s", "812 KB/s", "0 KB/s".
pub fn short_rate(bytes_per_s: f64) -> String {
    let v = if bytes_per_s.is_finite() {
        bytes_per_s.max(0.0)
    } else {
        0.0
    };
    if v >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB/s", v / (1024.0 * 1024.0 * 1024.0))
    } else if v >= 1024.0 * 1024.0 {
        format!("{:.1} MB/s", v / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB/s", v / 1024.0)
    }
}

/// Draws the widget's content into `rect`. Returns true when its close
/// button was clicked.
pub fn mini_content(
    ui: &mut Ui,
    rect: Rect,
    data: &MiniData,
    fahrenheit: bool,
    interactive: bool,
) -> bool {
    let p = pal(ui);
    let series = charts::series_ui(ui);
    let inner = rect.shrink2(vec2(12.0, 9.0));
    // Title row.
    ui.painter().text(
        inner.left_top() + vec2(0.0, 8.0),
        Align2::LEFT_CENTER,
        "PulseForge",
        semibold(12.0),
        p.muted,
    );
    let close = Rect::from_center_size(
        pos2(inner.right() - 8.0, inner.top() + 8.0),
        vec2(20.0, 20.0),
    );
    let mut closed = false;
    if interactive {
        let resp = ui
            .interact(close, ui.id().with("mini-close"), Sense::click())
            .on_hover_text("Close the mini widget");
        if resp.hovered() {
            ui.painter()
                .rect_filled(close, CornerRadius::same(5), p.hover);
        }
        closed = resp.clicked();
    }
    ui.painter().text(
        close.center(),
        Align2::CENTER_CENTER,
        Icon::CLOSE.glyph(),
        regular(10.0),
        p.faint,
    );

    let mut y = inner.top() + 24.0;
    let mut row = |label: &str, color: Color32, pct: Option<f32>, value: String, extra: String| {
        let r = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 22.0));
        ui.painter()
            .circle_filled(pos2(r.left() + 4.0, r.center().y + 1.0), 3.5, color);
        ui.painter().text(
            pos2(r.left() + 13.0, r.center().y),
            Align2::LEFT_CENTER,
            label,
            regular(12.5),
            p.muted,
        );
        let bar = Rect::from_min_max(
            pos2(r.left() + 78.0, r.center().y - 2.5),
            pos2(r.left() + 124.0, r.center().y + 2.5),
        );
        if let Some(pct) = pct {
            charts::paint_meter(ui, bar, pct / 100.0, color);
        }
        ui.painter().text(
            pos2(r.left() + 166.0, r.center().y),
            Align2::RIGHT_CENTER,
            value,
            semibold(13.0),
            p.text,
        );
        ui.painter().text(
            pos2(r.right(), r.center().y),
            Align2::RIGHT_CENTER,
            extra,
            regular(12.0),
            p.muted,
        );
        y += 22.0;
    };
    let temp = |t: Option<f32>| t.map(|t| util::fmt_temp(t, fahrenheit)).unwrap_or_default();
    row(
        "Processor",
        series.cpu,
        Some(data.cpu_pct),
        util::fmt_pct(data.cpu_pct),
        temp(data.cpu_temp),
    );
    match data.memory_pct {
        Some(m) => row(
            "Memory",
            series.memory,
            Some(m),
            util::fmt_pct(m),
            String::new(),
        ),
        None => row("Memory", series.memory, None, "n/a".into(), String::new()),
    }
    match data.gpu_pct {
        Some(g) => row(
            "Graphics",
            series.gpu,
            Some(g),
            util::fmt_pct(g),
            temp(data.gpu_temp),
        ),
        None => row("Graphics", series.gpu, None, "n/a".into(), String::new()),
    }
    // Network: download and upload side by side.
    let r = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 22.0));
    ui.painter()
        .circle_filled(pos2(r.left() + 4.0, r.center().y + 1.0), 3.5, series.net);
    ui.painter().text(
        pos2(r.left() + 13.0, r.center().y),
        Align2::LEFT_CENTER,
        "Network",
        regular(12.5),
        p.muted,
    );
    ui.painter().text(
        pos2(r.right(), r.center().y),
        Align2::RIGHT_CENTER,
        format!(
            "↓ {}   ↑ {}",
            short_rate(data.rx_bps),
            short_rate(data.tx_bps)
        ),
        regular(12.5),
        p.text,
    );
    closed
}

fn read_sample(shared: &Mutex<Shared>) -> Arc<Sample> {
    match shared.lock() {
        Ok(s) => s.sample.clone(),
        Err(_) => Arc::new(Sample::default()),
    }
}

impl App {
    /// Shows (or stops showing) the widget's own window. Called every frame
    /// of the main window; the widget itself repaints on its own timer.
    pub(super) fn mini_viewport(&mut self, ctx: &egui::Context) {
        let mini = self.st.mini.clone();
        if mini.close_requested.swap(false, Ordering::Relaxed) {
            self.settings.mini_widget = false;
        }
        if mini.show_main.swap(false, Ordering::Relaxed) {
            self.bring_up(ctx);
        }
        if let Ok(pos) = mini.position.lock() {
            if pos.is_some() && *pos != self.settings.mini_pos {
                self.settings.mini_pos = *pos;
            }
        }
        if !self.settings.mini_widget || self.st.screenshot {
            mini.open.store(false, Ordering::Relaxed);
            return;
        }
        mini.fahrenheit
            .store(self.settings.fahrenheit, Ordering::Relaxed);
        mini.interval_ms
            .store(self.settings.interval_ms, Ordering::Relaxed);
        let first = !mini.open.swap(true, Ordering::Relaxed);
        if first {
            // The window starts where it was left (if that place is still on
            // a display); afterwards it is only moved by the user.
            let scale = ctx.pixels_per_point().max(0.5);
            let (left, top, width, height) = crate::sys::desktop_rect();
            self.st.mini_start = keep_on_screen(
                self.settings.mini_pos,
                MINI_SIZE,
                (
                    left as f32 / scale,
                    top as f32 / scale,
                    width as f32 / scale,
                    height as f32 / scale,
                ),
            );
            if self.st.mini_start.is_none() {
                self.settings.mini_pos = None;
                if let Ok(mut pos) = mini.position.lock() {
                    *pos = None;
                }
            }
        }
        mini.main_window.store(self.hwnd, Ordering::Relaxed);
        let mut builder = egui::ViewportBuilder::default()
            .with_title("PulseForge mini")
            .with_inner_size(MINI_SIZE)
            .with_min_inner_size(MINI_SIZE)
            .with_resizable(false)
            .with_decorations(false)
            .with_taskbar(false)
            .with_always_on_top();
        if let Some((x, y)) = self.st.mini_start {
            builder = builder.with_position([x, y]);
        }
        let shared = self.monitor.shared.clone();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("pulseforge-mini"),
            builder,
            move |ctx, _class| mini_window(ctx, &shared, &mini),
        );
    }

    /// `--view mini`: the widget's content inside the main window, for the
    /// screenshot and as a preview.
    pub(super) fn mini_preview_ui(&mut self, ui: &mut Ui) {
        App::page_header(
            ui,
            "Mini widget",
            "A small always-on-top window with the headline readings",
            |_| {},
        );
        let data = mini_data(&self.data.sample);
        let f = self.settings.fahrenheit;
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            // A stand-in desktop with the widget at its real size.
            let (stage, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 300.0), Sense::hover());
            ui.painter().rect(
                stage,
                CornerRadius::same(12),
                p.rail,
                Stroke::new(1.0_f32, p.border),
                StrokeKind::Inside,
            );
            let mut x = stage.left() + 24.0;
            while x < stage.right() {
                ui.painter().vline(
                    x,
                    stage.y_range().shrink(1.0),
                    Stroke::new(1.0_f32, p.border.gamma_multiply(0.25)),
                );
                x += 48.0;
            }
            let mut y = stage.top() + 24.0;
            while y < stage.bottom() {
                ui.painter().hline(
                    stage.x_range().shrink(1.0),
                    y,
                    Stroke::new(1.0_f32, p.border.gamma_multiply(0.25)),
                );
                y += 48.0;
            }
            let rect = Rect::from_center_size(stage.center(), vec2(MINI_SIZE[0], MINI_SIZE[1]));
            ui.painter().rect_filled(
                rect.translate(vec2(0.0, 6.0)).expand(4.0),
                CornerRadius::same(14),
                Color32::from_black_alpha(if p.dark { 90 } else { 28 }),
            );
            ui.painter().rect(
                rect,
                CornerRadius::same(10),
                p.card,
                Stroke::new(1.0_f32, p.border),
                StrokeKind::Inside,
            );
            mini_content(ui, rect, &data, f, false);
            ui.painter().text(
                pos2(stage.center().x, stage.bottom() - 22.0),
                Align2::CENTER_CENTER,
                "Shown at its real size",
                regular(12.5),
                p.faint,
            );

            widgets::card(ui, |ui| {
                let mut on = self.settings.mini_widget;
                super::alerts_page::setting_row(
                    ui,
                    "Show the mini widget",
                    "It can also be switched on and off from the PulseForge icon in the notification area.",
                    |ui| {
                        if widgets::toggle(ui, on, true).clicked() {
                            on = !on;
                        }
                    },
                );
                self.settings.mini_widget = on;
                widgets::divider(ui);
                pages::facts(
                    ui,
                    pages::columns_for(ui.available_width(), 210.0, 4),
                    &[
                        ("Move", "Drag it anywhere".to_string()),
                        ("Open PulseForge", "Double-click it".to_string()),
                        ("Close", "Right-click, or its small x".to_string()),
                        ("Updates", "With every reading".to_string()),
                        ("Stays", "On top of other windows".to_string()),
                    ],
                );
            });
        });
    }
}

/// One frame of the widget's own window.
fn mini_window(ctx: &egui::Context, shared: &Mutex<Shared>, mini: &MiniShared) {
    let p = pal_ctx(ctx);
    let data = mini_data(&read_sample(shared));
    let fahrenheit = mini.fahrenheit.load(Ordering::Relaxed);
    let mut close = ctx.input(|i| i.viewport().close_requested());
    egui::CentralPanel::default()
        .frame(Frame::new().fill(p.card).inner_margin(Margin::ZERO))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(1.0_f32, p.border),
                StrokeKind::Inside,
            );
            // The whole surface drags the window; right-click offers to close it.
            let surface = ui.interact(rect, ui.id().with("mini-surface"), Sense::click_and_drag());
            if surface.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let mut open_main = surface.double_clicked();
            surface.context_menu(|ui| {
                if ui.button("Open PulseForge").clicked() {
                    open_main = true;
                    ui.close_menu();
                }
                if ui.button("Close the mini widget").clicked() {
                    close = true;
                    ui.close_menu();
                }
            });
            if open_main {
                // The main window may be asleep in the notification area:
                // show it from here, then tell it so.
                crate::sys::show_window(mini.main_window.load(Ordering::Relaxed));
                mini.show_main.store(true, Ordering::Relaxed);
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
            if mini_content(ui, rect, &data, fahrenheit, true) {
                close = true;
            }
        });
    // Remember where the window was left (not while it is being dragged).
    let (outer, pointer_down) = ctx.input(|i| (i.viewport().outer_rect, i.pointer.any_down()));
    if let (Some(outer), false) = (outer, pointer_down) {
        if let Ok(mut pos) = mini.position.lock() {
            let now = (outer.left().round(), outer.top().round());
            if *pos != Some(now) {
                *pos = Some(now);
            }
        }
    }
    if close {
        mini.close_requested.store(true, Ordering::Relaxed);
        // The main window stops showing the widget on its next frame; hide it
        // right away in case that window is asleep in the notification area.
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint_of(egui::ViewportId::ROOT);
        return;
    }
    // One repaint per reading; nothing in between.
    let interval = u64::from(mini.interval_ms.load(Ordering::Relaxed).clamp(250, 60_000));
    ctx.request_repaint_after(std::time::Duration::from_millis(interval));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_widget_never_opens_off_screen() {
        // One 1920 x 1080 display, and one of the same size to its left.
        let one = (0.0, 0.0, 1920.0, 1080.0);
        let two = (-1920.0, 0.0, 3840.0, 1080.0);
        assert_eq!(keep_on_screen(None, MINI_SIZE, one), None);
        assert_eq!(
            keep_on_screen(Some((1600.0, 40.0)), MINI_SIZE, one),
            Some((1600.0, 40.0))
        );
        // Left on the second display, which is still there ...
        assert_eq!(
            keep_on_screen(Some((-900.0, 300.0)), MINI_SIZE, two),
            Some((-900.0, 300.0))
        );
        // ... or has been unplugged since.
        assert_eq!(keep_on_screen(Some((-900.0, 300.0)), MINI_SIZE, one), None);
        // Hanging over an edge is fine while enough of it can be grabbed.
        assert!(keep_on_screen(Some((-100.0, 500.0)), MINI_SIZE, one).is_some());
        assert!(keep_on_screen(Some((1900.0, 500.0)), MINI_SIZE, one).is_none());
        assert!(keep_on_screen(Some((500.0, 1070.0)), MINI_SIZE, one).is_none());
        assert!(keep_on_screen(Some((500.0, -30.0)), MINI_SIZE, one).is_none());
        // Nonsense in, nothing out.
        assert_eq!(keep_on_screen(Some((f32::NAN, 0.0)), MINI_SIZE, one), None);
        assert_eq!(
            keep_on_screen(Some((10.0, 10.0)), MINI_SIZE, (0.0, 0.0, 0.0, 0.0)),
            None
        );
    }
    use crate::model::{CpuSample, GpuSample, MemorySample, NetSample};

    #[test]
    fn widget_numbers_come_from_the_reading() {
        let sample = Sample {
            cpu: CpuSample {
                total_pct: 23.0,
                temperature_c: Some(58.0),
                ..Default::default()
            },
            memory: MemorySample {
                total: 100,
                used: 45,
                ..Default::default()
            },
            gpus: vec![
                GpuSample {
                    load_pct: 4.0,
                    ..Default::default()
                },
                GpuSample {
                    load_pct: 81.0,
                    temperature_c: Some(70.0),
                    ..Default::default()
                },
            ],
            net: vec![
                NetSample {
                    kind: "Wi-Fi".into(),
                    rx_bps: 2048.0,
                    tx_bps: 512.0,
                    ..Default::default()
                },
                NetSample {
                    kind: "Virtual".into(),
                    rx_bps: 9999.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let d = mini_data(&sample);
        assert_eq!(d.cpu_pct, 23.0);
        assert_eq!(d.cpu_temp, Some(58.0));
        assert_eq!(d.memory_pct, Some(45.0));
        assert_eq!(d.gpu_pct, Some(81.0));
        assert_eq!(d.gpu_temp, Some(70.0));
        assert_eq!((d.rx_bps, d.tx_bps), (2048.0, 512.0));
        // Nothing read yet: no invented zeros for what is unknown.
        let d = mini_data(&Sample::default());
        assert_eq!(d.memory_pct, None);
        assert_eq!(d.gpu_pct, None);
        assert_eq!(d.cpu_temp, None);
    }

    #[test]
    fn short_rates() {
        assert_eq!(short_rate(0.0), "0 KB/s");
        assert_eq!(short_rate(-3.0), "0 KB/s");
        assert_eq!(short_rate(f64::NAN), "0 KB/s");
        assert_eq!(short_rate(831_488.0), "812 KB/s");
        assert_eq!(short_rate(4.2 * 1024.0 * 1024.0), "4.2 MB/s");
        assert_eq!(short_rate(1.5 * 1024.0 * 1024.0 * 1024.0), "1.5 GB/s");
    }
}
