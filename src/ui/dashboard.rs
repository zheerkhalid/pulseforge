//! The dashboard: one tile per device family, the active alerts, the main
//! history graphs and the per-thread strip.

use super::app::{App, Tab};
use super::charts::{self, Hist, Line, Unit};
use super::pages::{self, GAP};
use super::theme::{pal, regular, semibold, Icon};
use super::widgets;
use crate::history::{keys, Point};
use crate::model::{GpuInfo, HealthStatus};
use crate::util;
use eframe::egui::{
    pos2, vec2, Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui,
};

/// A short name for an adapter: enough to tell two of them apart in a legend.
pub(super) fn gpu_short(info: Option<&GpuInfo>, index: usize) -> String {
    match info.and_then(|i| i.integrated) {
        Some(true) => "Integrated".to_string(),
        Some(false) => "Discrete".to_string(),
        None => format!("GPU {index}"),
    }
}

struct Tile {
    tab: Tab,
    icon: Icon,
    color: Color32,
    title: String,
    value: String,
    /// Small text in the top right corner, with an optional state colour.
    corner: String,
    corner_color: Option<Color32>,
    lines: Vec<String>,
    sparks: Vec<(Vec<Point>, Color32)>,
    range: Option<(f32, f32)>,
}

const TILE_HEIGHT: f32 = 150.0;

fn paint_tile(ui: &mut Ui, width: f32, tile: &Tile, window_s: f64) -> bool {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::click());
    let hovered = resp.hovered();
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        if hovered { p.hover } else { p.card },
        Stroke::new(1.0_f32, if hovered { p.faint } else { p.border }),
        StrokeKind::Inside,
    );
    if hovered {
        ui.ctx()
            .set_cursor_icon(eframe::egui::CursorIcon::PointingHand);
    }
    let inner = rect.shrink2(vec2(14.0, 12.0));
    // Title row.
    ui.painter().text(
        pos2(inner.left() + 8.0, inner.top() + 9.0),
        Align2::CENTER_CENTER,
        tile.icon.glyph(),
        regular(14.0),
        tile.color,
    );
    let mut title = Rect::from_min_max(
        pos2(inner.left() + 24.0, inner.top()),
        pos2(inner.right(), inner.top() + 18.0),
    );
    if !tile.corner.is_empty() {
        let color = tile.corner_color.unwrap_or(p.muted);
        let g = ui
            .painter()
            .layout_no_wrap(tile.corner.clone(), regular(12.5), color);
        title.max.x -= g.size().x + 8.0;
        ui.painter().galley(
            pos2(
                inner.right() - g.size().x,
                inner.top() + 9.0 - g.size().y / 2.0,
            ),
            g,
            color,
        );
    }
    widgets::text_in(ui, title, &tile.title, semibold(13.0), p.muted);
    // The headline number.
    let value = Rect::from_min_size(inner.min + vec2(0.0, 22.0), vec2(inner.width(), 34.0));
    widgets::text_in(ui, value, &tile.value, semibold(26.0), p.text);
    // Secondary facts.
    for (i, line) in tile.lines.iter().take(3).enumerate() {
        let r = Rect::from_min_size(
            inner.min + vec2(0.0, 58.0 + 17.0 * i as f32),
            vec2(inner.width(), 17.0),
        );
        widgets::text_in(ui, r, line, regular(12.5), p.muted);
    }
    // Trend.
    let spark = Rect::from_min_max(
        pos2(rect.left() + 10.0, rect.bottom() - 40.0),
        pos2(rect.right() - 10.0, rect.bottom() - 8.0),
    );
    if tile.lines.len() <= 2 {
        // Two-line charts share one scale so that the lines compare.
        let range = tile.range.or_else(|| {
            let hi = tile
                .sparks
                .iter()
                .flat_map(|s| s.0.iter())
                .map(|q| q.v)
                .fold(0.0_f32, f32::max);
            (tile.sparks.len() > 1).then_some((0.0, (hi * 1.12).max(1.0)))
        });
        for (points, color) in tile.sparks.iter().rev() {
            charts::sparkline(ui, spark, points, *color, range, window_s);
        }
    }
    resp.on_hover_text(format!("Open {}", tile.tab.label()))
        .clicked()
}

impl App {
    fn tiles(&self, hist: &Hist) -> Vec<Tile> {
        let p_series = charts::series(self.dark());
        let s = &self.data.sample;
        let inv = &self.data.inventory;
        let slow = &self.data.slow;
        let f = self.settings.fahrenheit;
        let mut tiles = Vec::new();

        // Processor.
        let mut first = match s.cpu.current_mhz {
            Some(mhz) => util::fmt_mhz(mhz),
            None => "Clock not reported".to_string(),
        };
        if s.cpu.processes > 0 {
            first.push_str(&format!(" · {} processes", s.cpu.processes));
        }
        tiles.push(Tile {
            tab: Tab::Cpu,
            icon: Icon::CPU,
            color: p_series.cpu,
            title: "Processor".into(),
            value: util::fmt_pct(s.cpu.total_pct),
            corner: s
                .cpu
                .temperature_c
                .map(|t| util::fmt_temp(t, f))
                .unwrap_or_default(),
            corner_color: None,
            lines: vec![first, inv.cpu.name.clone()],
            sparks: vec![(hist.points(keys::CPU_TOTAL), p_series.cpu)],
            range: Some((0.0, 100.0)),
        });

        // Memory.
        let m = &s.memory;
        tiles.push(Tile {
            tab: Tab::Memory,
            icon: Icon::MEMORY,
            color: p_series.memory,
            title: "Memory".into(),
            value: if m.total > 0 {
                util::fmt_pct(m.used_pct())
            } else {
                "Reading...".into()
            },
            corner: String::new(),
            corner_color: None,
            lines: vec![
                format!(
                    "{} of {} · cached {}",
                    util::fmt_bytes(m.used),
                    util::fmt_bytes(m.total),
                    util::fmt_bytes(m.cached)
                ),
                format!(
                    "Commit {} · page file {}",
                    util::fmt_pct(m.commit_pct()),
                    util::fmt_bytes(m.pagefile_used)
                ),
            ],
            sparks: vec![(hist.points(keys::MEM_USED_PCT), p_series.memory)],
            range: Some((0.0, 100.0)),
        });

        // One tile per graphics adapter.
        for (i, g) in s.gpus.iter().enumerate() {
            let info = inv.gpus.iter().find(|a| a.id == g.id);
            if info.is_some_and(|a| a.software) {
                continue;
            }
            let dedicated = info.map(|a| a.dedicated_bytes).unwrap_or(0);
            let memory = if dedicated > 256 << 20 {
                format!(
                    "Memory {} of {}",
                    util::fmt_bytes(g.dedicated_used),
                    util::fmt_bytes(dedicated)
                )
            } else {
                format!("Shared memory {}", util::fmt_bytes(g.shared_used))
            };
            let mut extra = Vec::new();
            if let Some(w) = g.power_w {
                extra.push(format!("{w:.0} W"));
            }
            if let Some(rpm) = g.fan_rpm {
                extra.push(format!("fan {} RPM", util::fmt_count(u64::from(rpm))));
            }
            let second = if extra.is_empty() {
                match g.temperature_c {
                    Some(_) => info.map(|a| a.vendor.clone()).unwrap_or_default(),
                    None => "Temperature not available".to_string(),
                }
            } else {
                extra.join(" · ")
            };
            tiles.push(Tile {
                tab: Tab::Gpu,
                icon: Icon::GPU,
                color: p_series.gpu,
                title: info
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| format!("Graphics {i}")),
                value: util::fmt_pct(g.load_pct),
                corner: g
                    .temperature_c
                    .map(|t| util::fmt_temp(t, f))
                    .unwrap_or_default(),
                corner_color: None,
                lines: vec![memory, second],
                sparks: vec![(hist.points(&keys::gpu_load(&g.id)), p_series.gpu)],
                range: Some((0.0, 100.0)),
            });
        }

        // Drives.
        if !s.disks.is_empty() || !slow.drives.is_empty() {
            let (read, write) = s.disk_totals();
            let active = s.disks.iter().map(|d| d.active_pct).fold(0.0_f32, f32::max);
            let hottest = slow
                .drives
                .iter()
                .filter_map(|d| d.temperature_c)
                .fold(None, |a: Option<f32>, b| Some(a.map_or(b, |a| a.max(b))));
            let worst = slow
                .drives
                .iter()
                .fold(HealthStatus::Unknown, |a, d| a.worst(d.health));
            let p = super::theme::palette(self.dark());
            let mut second = match hottest {
                Some(t) => format!("Hottest {}", util::fmt_temp(t, f)),
                None => "Temperature not available".to_string(),
            };
            let count = slow.drives.len().max(s.disks.len());
            second.push_str(&format!(
                " · {count} {}",
                if count == 1 { "drive" } else { "drives" }
            ));
            tiles.push(Tile {
                tab: Tab::Storage,
                icon: Icon::DRIVE,
                color: p_series.disk,
                title: "Drives".into(),
                value: format!("{} active", util::fmt_pct(active)),
                corner: if self.data.slow_ready {
                    pages::health_text(worst)
                } else {
                    String::new()
                },
                corner_color: Some(pages::health_color(&p, worst)),
                lines: vec![
                    format!(
                        "Read {} · write {}",
                        util::fmt_rate(read),
                        util::fmt_rate(write)
                    ),
                    second,
                ],
                sparks: vec![
                    (hist.points(keys::DISK_READ), p_series.disk),
                    (hist.points(keys::DISK_WRITE), p_series.second),
                ],
                range: None,
            });
        }

        // Network.
        let (rx, tx) = s.net_totals();
        let link = s
            .net
            .iter()
            .filter(|n| n.up && n.kind != "Virtual" && n.kind != "Loopback")
            .max_by(|a, b| {
                (a.rx_bps + a.tx_bps)
                    .partial_cmp(&(b.rx_bps + b.tx_bps))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        tiles.push(Tile {
            tab: Tab::Network,
            icon: Icon::NETWORK,
            color: p_series.net,
            title: "Network".into(),
            value: util::fmt_rate(rx),
            corner: "download".into(),
            corner_color: None,
            lines: vec![
                format!("Upload {}", util::fmt_rate(tx)),
                match link {
                    Some(n) if n.link_bps > 0 => {
                        format!("{} · {}", n.name, util::fmt_bits(n.link_bps))
                    }
                    Some(n) => n.name.clone(),
                    None => "No connection".to_string(),
                },
            ],
            sparks: vec![
                (hist.points(keys::NET_RX), p_series.net),
                (hist.points(keys::NET_TX), p_series.second),
            ],
            range: None,
        });

        // Battery.
        if let Some(b) = &s.battery {
            let mut first = Vec::new();
            if let Some(mw) = b.rate_mw.filter(|mw| *mw != 0) {
                first.push(format!("{:+.1} W", f64::from(mw) / 1000.0));
            }
            if let Some(left) = b.seconds_left {
                first.push(format!(
                    "{} {}",
                    util::fmt_duration(u64::from(left) / 60 * 60),
                    if b.state == crate::model::BatteryState::Charging {
                        "until full"
                    } else {
                        "left"
                    }
                ));
            }
            let mut second = Vec::new();
            if let Some(w) = b.wear_pct {
                second.push(format!("Wear {w:.0}%"));
            }
            if let Some(c) = b.cycle_count {
                second.push(format!("{c} cycles"));
            }
            tiles.push(Tile {
                tab: Tab::Battery,
                icon: Icon::BATTERY,
                color: p_series.battery,
                title: "Battery".into(),
                value: util::fmt_pct(b.percent),
                corner: b.state.label().into(),
                corner_color: None,
                lines: vec![
                    if first.is_empty() {
                        "Rate not reported".into()
                    } else {
                        first.join(" · ")
                    },
                    if second.is_empty() {
                        b.chemistry.clone()
                    } else {
                        second.join(" · ")
                    },
                ],
                sparks: vec![(hist.points(keys::BATTERY_PCT), p_series.battery)],
                range: Some((0.0, 100.0)),
            });
        }

        // Uptime.
        let sys = &inv.system;
        let mut lines = vec![if sys.boot_time.is_empty() {
            "Since the last start of Windows".to_string()
        } else {
            format!("Started {}", sys.boot_time)
        }];
        if s.cpu.processes > 0 {
            lines.push(format!(
                "{} processes · {} threads",
                util::fmt_count(u64::from(s.cpu.processes)),
                util::fmt_count(u64::from(s.cpu.threads))
            ));
            lines.push(format!(
                "{} handles",
                util::fmt_count(u64::from(s.cpu.handles))
            ));
        }
        tiles.push(Tile {
            tab: Tab::System,
            icon: Icon::CLOCK,
            color: super::theme::palette(self.dark()).accent,
            title: "Uptime".into(),
            value: util::fmt_duration(s.uptime_s / 60 * 60),
            corner: sys.computer_name.clone(),
            corner_color: None,
            lines: if lines.len() < 3 {
                // Keep the tile's rhythm when the counts are missing.
                let mut l = lines;
                l.push(sys.os_name.clone());
                l.push(String::new());
                l
            } else {
                lines
            },
            sparks: Vec::new(),
            range: None,
        });
        if self.data.tick == 0 {
            // Nothing has been read yet: no zeros that look like readings.
            for tile in &mut tiles {
                tile.value = "Reading...".to_string();
                tile.corner.clear();
                tile.lines.iter_mut().for_each(String::clear);
                tile.sparks.clear();
            }
        }
        tiles
    }

    pub(super) fn dark(&self) -> bool {
        self.st.dark
    }

    fn alerts_banner(&mut self, ui: &mut Ui) {
        if self.data.active_alerts.is_empty() {
            return;
        }
        let p = pal(ui);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
        ui.painter().rect(
            rect,
            CornerRadius::same(12),
            p.warn.gamma_multiply(0.1),
            Stroke::new(1.0_f32, p.warn.gamma_multiply(0.45)),
            StrokeKind::Inside,
        );
        ui.painter().text(
            pos2(rect.left() + 24.0, rect.center().y),
            Align2::CENTER_CENTER,
            Icon::WARNING.glyph(),
            regular(16.0),
            p.warn,
        );
        let n = self.data.active_alerts.len();
        let described: Vec<String> = self
            .data
            .active_alerts
            .iter()
            .take(3)
            .map(|(metric, subject, value)| {
                super::alerts_page::describe_active(
                    *metric,
                    subject,
                    *value,
                    self.settings.fahrenheit,
                )
            })
            .collect();
        let text = format!(
            "{} active: {}{}",
            if n == 1 {
                "1 alert".to_string()
            } else {
                format!("{n} alerts")
            },
            described.join(" · "),
            if n > 3 { " ..." } else { "" }
        );
        let button = Rect::from_min_size(
            pos2(rect.right() - 118.0, rect.center().y - 15.0),
            vec2(106.0, 30.0),
        );
        widgets::text_in(
            ui,
            Rect::from_min_max(
                pos2(rect.left() + 44.0, rect.top()),
                pos2(button.left() - 12.0, rect.bottom()),
            ),
            &text,
            regular(14.0),
            p.text,
        );
        let resp = ui.interact(button, ui.id().with("alerts-banner"), Sense::click());
        ui.painter().rect(
            button,
            CornerRadius::same(8),
            if resp.hovered() {
                p.hover
            } else {
                Color32::TRANSPARENT
            },
            Stroke::new(1.0_f32, p.border),
            StrokeKind::Inside,
        );
        ui.painter().text(
            button.center(),
            Align2::CENTER_CENTER,
            "View alerts",
            regular(13.5),
            p.text,
        );
        if resp.clicked() {
            self.tab = Tab::Alerts;
        }
    }

    pub(super) fn dashboard_ui(&mut self, ui: &mut Ui) {
        let sys = &self.data.inventory.system;
        let subtitle = if !self.data.inventory_ready {
            "Reading hardware...".to_string()
        } else {
            [
                [sys.manufacturer.as_str(), sys.product.as_str()]
                    .join(" ")
                    .trim()
                    .to_string(),
                sys.os_name.clone(),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
        };
        App::page_header(ui, "Dashboard", &subtitle, |_| {});
        let hist = self.hist();
        let time = self.time_axis();
        let series = charts::series_ui(ui);
        let mut open = None;
        pages::page_scroll(ui, |ui| {
            if !self.data.inventory_ready {
                pages::loading(ui, "The first readings appear in a moment.");
            }
            self.alerts_banner(ui);

            // Tiles.
            let tiles = self.tiles(&hist);
            let width = ui.available_width();
            let columns = pages::columns_for(width, 225.0, 4);
            let tile_w = ((width - GAP * (columns as f32 - 1.0)) / columns as f32).floor();
            for row in tiles.chunks(columns) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for tile in row {
                        if paint_tile(ui, tile_w, tile, hist.window_s) {
                            open = Some(tile.tab);
                        }
                    }
                });
            }

            // The main history graphs.
            let s = self.data.sample.clone();
            let inv = self.data.inventory.clone();
            let mut graphs: Vec<(&str, Unit, Vec<Line>)> = vec![
                (
                    "Processor",
                    Unit::Percent,
                    vec![hist.line("Processor", series.cpu, keys::CPU_TOTAL)],
                ),
                (
                    "Memory",
                    Unit::Percent,
                    vec![
                        hist.line("In use", series.memory, keys::MEM_USED_PCT),
                        hist.line("Committed", series.second, keys::MEM_COMMIT_PCT),
                    ],
                ),
            ];
            let hardware: Vec<_> = s
                .gpus
                .iter()
                .enumerate()
                .filter(|(_, g)| !inv.gpus.iter().any(|a| a.id == g.id && a.software))
                .take(2)
                .collect();
            if !hardware.is_empty() {
                let colors = [series.gpu, series.second];
                graphs.push((
                    "Graphics",
                    Unit::Percent,
                    hardware
                        .iter()
                        .enumerate()
                        .map(|(n, (i, g))| {
                            let name = gpu_short(inv.gpus.iter().find(|a| a.id == g.id), *i);
                            hist.line(&name, colors[n], &keys::gpu_load(&g.id))
                        })
                        .collect(),
                ));
            }
            graphs.push((
                "Drives",
                Unit::Rate,
                vec![
                    hist.line("Read", series.disk, keys::DISK_READ),
                    hist.line("Write", series.second, keys::DISK_WRITE),
                ],
            ));
            graphs.push((
                "Network",
                Unit::Rate,
                vec![
                    hist.line("Download", series.net, keys::NET_RX),
                    hist.line("Upload", series.second, keys::NET_TX),
                ],
            ));
            if s.battery.is_some() {
                graphs.push((
                    "Battery",
                    Unit::Percent,
                    vec![hist.line("Charge", series.battery, keys::BATTERY_PCT)],
                ));
            }
            let per_row = if ui.available_width() >= 760.0 { 2 } else { 1 };
            for (r, row) in graphs.chunks(per_row).enumerate() {
                let weights = vec![1.0; row.len()];
                pages::card_row(ui, &weights, &mut |ui, i| {
                    let (title, unit, lines) = &row[i];
                    // The same card height whether a chart has one line or two.
                    let height = 134.0 - 16.0 * (lines.len() as f32 - 1.0);
                    charts::chart(ui, ("dash", r, i), title, *unit, lines, height, &time, "");
                });
            }

            // Every logical processor.
            if !s.cpu.per_core_pct.is_empty() {
                self.core_strip(ui, &hist, "Processor threads", 54.0);
            }
        });
        if let Some(tab) = open {
            self.tab = tab;
        }
    }

    /// The small multiples: one cell per logical processor.
    pub(super) fn core_strip(&self, ui: &mut Ui, hist: &Hist, title: &str, cell_height: f32) {
        let p = pal(ui);
        let series = charts::series_ui(ui);
        let cores = &self.data.sample.cpu.per_core_pct;
        let mhz = &self.data.sample.cpu.per_core_mhz;
        let keys: Vec<String> = (0..cores.len()).map(keys::cpu_core).collect();
        let all = hist.many(&keys);
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::section(ui, title);
                ui.label(
                    eframe::egui::RichText::new(format!(
                        "{} logical processors, each on a 0 to 100% scale",
                        cores.len()
                    ))
                    .size(12.5)
                    .color(p.muted),
                );
            });
            let width = ui.available_width();
            let gap = 8.0;
            let columns = (((width + gap) / (112.0 + gap)).floor() as usize).clamp(2, 16);
            // Fill whole rows where the count allows it.
            let columns = (2..=columns)
                .rev()
                .find(|c| cores.len().is_multiple_of(*c))
                .filter(|c| *c * 2 > columns)
                .unwrap_or(columns);
            let cell_w = ((width - gap * (columns as f32 - 1.0)) / columns as f32).floor();
            ui.spacing_mut().item_spacing = vec2(gap, gap);
            for (r, row) in cores.chunks(columns).enumerate() {
                ui.horizontal(|ui| {
                    for (c, value) in row.iter().enumerate() {
                        let i = r * columns + c;
                        let label = match mhz.get(i) {
                            Some(m) if cell_w > 150.0 => {
                                format!("CPU {i} · {}", util::fmt_mhz(*m))
                            }
                            _ => format!("CPU {i}"),
                        };
                        charts::mini_chart(
                            ui,
                            vec2(cell_w, cell_height),
                            &label,
                            &util::fmt_pct(*value),
                            &all[i],
                            series.cpu,
                            hist.window_s,
                        );
                    }
                });
            }
        });
    }
}
