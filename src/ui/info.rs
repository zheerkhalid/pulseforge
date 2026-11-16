//! The Network, System, Displays and Sensors pages.

use super::app::App;
use super::charts::{self, Unit};
use super::pages::{self, col, col_right, Cell};
use super::theme::{pal, regular, semibold, Icon};
use super::widgets;
use crate::history::keys;
use crate::model::{DisplayInfo, NetSample, SensorKind, SensorReading};
use crate::util;
use eframe::egui::{
    pos2, vec2, Align2, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};

/// A sensor value in words, in the unit the user chose for temperatures.
pub(super) fn sensor_value(kind: SensorKind, value: f64, fahrenheit: bool) -> String {
    match kind {
        SensorKind::Temperature => util::fmt_temp(value as f32, fahrenheit),
        SensorKind::Fan => format!("{} RPM", util::fmt_count(value.max(0.0).round() as u64)),
        SensorKind::Voltage => format!("{value:.2} V"),
        SensorKind::Power => format!("{value:.1} W"),
        SensorKind::Load => format!("{:.0}%", value.clamp(0.0, 100.0)),
        SensorKind::Clock => util::fmt_mhz(value as f32),
        SensorKind::Current => format!("{value:.2} A"),
        SensorKind::Other => format!("{value:.1}"),
    }
}

/// The kinds that occur in `readings`, in a fixed order, with their counts.
pub(super) fn sensor_kinds(readings: &[SensorReading]) -> Vec<(SensorKind, usize)> {
    const ORDER: [SensorKind; 8] = [
        SensorKind::Temperature,
        SensorKind::Fan,
        SensorKind::Voltage,
        SensorKind::Power,
        SensorKind::Load,
        SensorKind::Clock,
        SensorKind::Current,
        SensorKind::Other,
    ];
    ORDER
        .into_iter()
        .filter_map(|k| {
            let n = readings.iter().filter(|r| r.kind == k).count();
            (n > 0).then_some((k, n))
        })
        .collect()
}

/// The device groups of `readings` in order of first appearance.
pub(super) fn sensor_groups(readings: &[SensorReading]) -> Vec<&str> {
    let mut groups: Vec<&str> = Vec::new();
    for r in readings {
        if !groups.contains(&r.group.as_str()) {
            groups.push(&r.group);
        }
    }
    groups
}

/// Adapters in display order: connected hardware first, then the rest of
/// the hardware, then virtual adapters.
pub(super) fn adapter_order(net: &[NetSample]) -> Vec<&NetSample> {
    let rank = |n: &NetSample| {
        let software = n.kind == "Virtual" || n.kind == "Loopback";
        match (software, n.up) {
            (false, true) => 0,
            (false, false) => 1,
            (true, true) => 2,
            (true, false) => 3,
        }
    };
    let mut v: Vec<&NetSample> = net.iter().collect();
    v.sort_by_key(|n| rank(n));
    v
}

/// The rectangle of each display inside `area`, keeping the arrangement
/// and the proportions of the desktop.
pub(super) fn arrangement(displays: &[DisplayInfo], area: Rect) -> Vec<Rect> {
    let rects: Vec<(f32, f32, f32, f32)> = displays
        .iter()
        .map(|d| {
            (
                d.position.0 as f32,
                d.position.1 as f32,
                d.width.max(1) as f32,
                d.height.max(1) as f32,
            )
        })
        .collect();
    if rects.is_empty() {
        return Vec::new();
    }
    let left = rects.iter().map(|r| r.0).fold(f32::INFINITY, f32::min);
    let top = rects.iter().map(|r| r.1).fold(f32::INFINITY, f32::min);
    let right = rects
        .iter()
        .map(|r| r.0 + r.2)
        .fold(f32::NEG_INFINITY, f32::max);
    let bottom = rects
        .iter()
        .map(|r| r.1 + r.3)
        .fold(f32::NEG_INFINITY, f32::max);
    let scale =
        (area.width() / (right - left).max(1.0)).min(area.height() / (bottom - top).max(1.0));
    let used = vec2((right - left) * scale, (bottom - top) * scale);
    let origin = area.center() - used / 2.0;
    rects
        .iter()
        .map(|r| {
            Rect::from_min_size(
                origin + vec2((r.0 - left) * scale, (r.1 - top) * scale),
                vec2(r.2 * scale, r.3 * scale),
            )
        })
        .collect()
}

impl App {
    /// The "Show serial numbers" switch for pages that list them.
    pub(super) fn reveal_toggle(&mut self, ui: &mut Ui) {
        if self.settings.mask_serials {
            let mut on = self.st.reveal_serials;
            if widgets::checkbox(ui, &mut on, "Show serial numbers")
                .on_hover_text("Serial numbers and hardware addresses are partly hidden. Tick to show them in full.")
                .changed()
            {
                self.st.reveal_serials = on;
            }
        }
    }

    // ------------------------------------------------------------------------------
    // Network
    // ------------------------------------------------------------------------------

    pub(super) fn network_ui(&mut self, ui: &mut Ui) {
        let s = self.data.sample.clone();
        let (rx, tx) = s.net_totals();
        let subtitle = if s.net.is_empty() {
            "No network adapters reported yet".to_string()
        } else {
            format!(
                "{} adapters · download {} · upload {}",
                s.net.len(),
                util::fmt_rate(rx),
                util::fmt_rate(tx)
            )
        };
        App::page_header(ui, "Network", &subtitle, |ui| self.reveal_toggle(ui));
        let hist = self.hist();
        let time = self.time_axis();
        let series = charts::series_ui(ui);
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            if s.net.is_empty() {
                widgets::card(ui, |ui| {
                    widgets::empty_state(
                        ui,
                        Icon::NETWORK,
                        p.muted,
                        "No network adapters",
                        "Windows has not reported any network adapter yet.",
                    );
                });
            }
            for (index, n) in adapter_order(&s.net).into_iter().enumerate() {
                let mut left_height = 0.0_f32;
                pages::card_row(ui, &[1.0, 1.25], &mut |ui, i| {
                    if i == 0 {
                        ui.add(
                            eframe::egui::Label::new(RichText::new(&n.name).font(semibold(16.0)))
                                .truncate(),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if !n.kind.is_empty() {
                                widgets::badge(ui, &n.kind, p.info);
                            }
                            if n.up {
                                widgets::badge(ui, &Icon::DONE.with("Connected"), p.good);
                            } else {
                                widgets::badge(ui, "Not connected", p.muted);
                            }
                        });
                        if !n.description.is_empty() {
                            ui.add(
                                eframe::egui::Label::new(
                                    RichText::new(&n.description).size(12.5).color(p.muted),
                                )
                                .truncate(),
                            );
                        }
                        // IP addresses are shown as they are: they are needed to use
                        // the page; the hardware address is what identifies the device.
                        let list = |v: &[String]| v.join(", ");
                        let mut items: Vec<(&str, String)> = vec![
                            (
                                "Link speed",
                                if n.link_bps > 0 {
                                    util::fmt_bits(n.link_bps)
                                } else if n.up {
                                    String::new()
                                } else {
                                    "No link".to_string()
                                },
                            ),
                            ("Hardware address (MAC)", self.address(&n.mac)),
                            ("IPv4 address", list(&n.ipv4)),
                            ("IPv6 address", list(&n.ipv6)),
                            ("Gateway", n.gateway.clone()),
                            ("DNS servers", n.dns.join(", ")),
                            ("Received since start", util::fmt_bytes(n.rx_total)),
                            ("Sent since start", util::fmt_bytes(n.tx_total)),
                        ];
                        if !n.up {
                            // A disconnected adapter has no addresses: leave the rows out
                            // instead of listing "Not available" four times.
                            items.retain(|(label, value)| {
                                !value.is_empty()
                                    || !matches!(
                                        *label,
                                        "IPv4 address" | "IPv6 address" | "Gateway" | "DNS servers"
                                    )
                            });
                        }
                        for (label, value) in &n.extra {
                            items.push((label.as_str(), value.clone()));
                        }
                        pages::facts(ui, 2, &items);
                        left_height = ui.min_rect().height();
                    } else {
                        // The graph takes the height the facts beside it leave.
                        let plot = (left_height - charts::chrome_height(2)).clamp(96.0, 420.0);
                        charts::chart(
                            ui,
                            ("net", index),
                            "Throughput",
                            Unit::Rate,
                            &[
                                hist.line("Download", series.net, &keys::net_rx(&n.name)),
                                hist.line("Upload", series.second, &keys::net_tx(&n.name)),
                            ],
                            plot,
                            &time,
                            "",
                        );
                    }
                });
            }
        });
    }

    // ------------------------------------------------------------------------------
    // System
    // ------------------------------------------------------------------------------

    pub(super) fn system_ui(&mut self, ui: &mut Ui) {
        let inv = self.data.inventory.clone();
        let sys = &inv.system;
        let subtitle = if !self.data.inventory_ready {
            "Reading hardware...".to_string()
        } else {
            [sys.manufacturer.as_str(), sys.product.as_str()]
                .iter()
                .filter(|x| !x.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        };
        App::page_header(ui, "System", &subtitle, |ui| self.reveal_toggle(ui));
        let uptime = self.data.sample.uptime_s;
        pages::page_scroll(ui, |ui| {
            if !self.data.inventory_ready {
                pages::loading(
                    ui,
                    "Firmware tables and Windows details appear in a moment.",
                );
                return;
            }
            let on_off = |v: Option<bool>| match v {
                Some(true) => "On".to_string(),
                Some(false) => "Off".to_string(),
                None => String::new(),
            };
            pages::card_row(ui, &[1.0, 1.0], &mut |ui, i| {
                if i == 0 {
                    widgets::section(ui, "Computer");
                    pages::facts(
                        ui,
                        2,
                        &[
                            ("Manufacturer", sys.manufacturer.clone()),
                            ("Model", sys.product.clone()),
                            ("Family", sys.family.clone()),
                            ("Model number (SKU)", sys.sku.clone()),
                            ("Chassis", sys.chassis.clone()),
                            ("Serial number", self.serial(&sys.system_serial)),
                            ("Hardware ID (UUID)", self.serial(&sys.uuid)),
                            (
                                "Memory slots",
                                match (sys.memory_slots_used, sys.memory_slots_total) {
                                    (Some(u), Some(t)) => format!("{u} of {t} in use"),
                                    (None, Some(t)) => t.to_string(),
                                    _ => String::new(),
                                },
                            ),
                        ],
                    );
                } else {
                    widgets::section(ui, "Windows");
                    pages::facts(
                        ui,
                        2,
                        &[
                            ("Edition", sys.os_name.clone()),
                            ("Version", sys.os_display_version.clone()),
                            ("Build", sys.os_build.clone()),
                            ("Architecture", sys.os_arch.clone()),
                            ("Installed on", sys.os_install_date.clone()),
                            ("Language", sys.locale.clone()),
                            ("Started", sys.boot_time.clone()),
                            ("Uptime", util::fmt_duration(uptime)),
                        ],
                    );
                }
            });
            pages::card_row(ui, &[1.0, 1.0], &mut |ui, i| {
                if i == 0 {
                    widgets::section(ui, "Motherboard");
                    pages::facts(
                        ui,
                        2,
                        &[
                            ("Manufacturer", sys.board_manufacturer.clone()),
                            ("Model", sys.board_product.clone()),
                            ("Version", sys.board_version.clone()),
                            ("Serial number", self.serial(&sys.board_serial)),
                        ],
                    );
                    ui.add_space(2.0);
                    widgets::section(ui, "Names and time");
                    pages::facts(
                        ui,
                        2,
                        &[
                            ("Computer name", sys.computer_name.clone()),
                            ("User name", sys.user_name.clone()),
                            ("Time zone", sys.time_zone.clone()),
                            (
                                "Largest supported memory",
                                sys.memory_max_bytes
                                    .map(util::fmt_bytes)
                                    .unwrap_or_default(),
                            ),
                        ],
                    );
                } else {
                    widgets::section(ui, "Firmware and security");
                    pages::facts(
                        ui,
                        2,
                        &[
                            ("Firmware vendor", sys.bios_vendor.clone()),
                            ("Firmware version", sys.bios_version.clone()),
                            ("Firmware date", sys.bios_date.clone()),
                            ("Boot mode", sys.firmware_mode.clone()),
                            ("Secure Boot", on_off(sys.secure_boot)),
                            (
                                "Security chip (TPM)",
                                match sys.tpm_present {
                                    Some(true) if !sys.tpm_version.is_empty() => {
                                        format!("Present, version {}", sys.tpm_version)
                                    }
                                    Some(true) => "Present".to_string(),
                                    Some(false) => "Not present".to_string(),
                                    None => String::new(),
                                },
                            ),
                            ("TPM manufacturer", sys.tpm_manufacturer.clone()),
                        ],
                    );
                }
            });
            if !sys.extra.is_empty() {
                widgets::card(ui, |ui| {
                    widgets::section(ui, "More");
                    let items: Vec<(&str, String)> = sys
                        .extra
                        .iter()
                        .map(|(l, v)| (l.as_str(), v.clone()))
                        .collect();
                    let columns = pages::columns_for(ui.available_width(), 220.0, 4);
                    pages::facts(ui, columns, &items);
                });
            }
        });
    }

    // ------------------------------------------------------------------------------
    // Displays
    // ------------------------------------------------------------------------------

    pub(super) fn displays_ui(&mut self, ui: &mut Ui) {
        let inv = self.data.inventory.clone();
        let displays = &inv.displays;
        let subtitle = if !self.data.inventory_ready {
            "Reading hardware...".to_string()
        } else {
            match displays.len() {
                0 => "No display reported".to_string(),
                1 => "1 display".to_string(),
                n => format!("{n} displays"),
            }
        };
        let mut refresh = false;
        App::page_header(ui, "Displays", &subtitle, |ui| {
            if widgets::ghost_button(ui, &Icon::REFRESH.with("Read again"))
                .on_hover_text(
                    "Reads the display list again, for example after plugging a monitor in.",
                )
                .clicked()
            {
                refresh = true;
            }
        });
        if refresh {
            self.monitor.refresh_inventory();
            self.toast(
                if self.demo {
                    "Demo mode: the invented displays stay as they are."
                } else {
                    "Reading the hardware again..."
                },
                false,
            );
        }
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            if !self.data.inventory_ready {
                pages::loading(ui, "The display list appears in a moment.");
                return;
            }
            if displays.is_empty() {
                widgets::card(ui, |ui| {
                    widgets::empty_state(
                        ui,
                        Icon::DISPLAY,
                        p.muted,
                        "No display reported",
                        "Windows did not list an active display (this is normal in some remote sessions).",
                    );
                });
                return;
            }
            // How the displays sit next to each other.
            widgets::card(ui, |ui| {
                widgets::section(ui, "Arrangement");
                let (area, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 132.0), Sense::hover());
                for (i, (d, r)) in displays
                    .iter()
                    .zip(arrangement(displays, area.shrink2(vec2(40.0, 4.0))))
                    .enumerate()
                {
                    let r = r.shrink(2.0);
                    ui.painter().rect(
                        r,
                        CornerRadius::same(6),
                        if d.primary { p.selected } else { p.hover },
                        Stroke::new(1.5_f32, if d.primary { p.accent } else { p.faint }),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        r.center() - vec2(0.0, 8.0),
                        Align2::CENTER_CENTER,
                        (i + 1).to_string(),
                        semibold(20.0),
                        p.text,
                    );
                    let name = Rect::from_center_size(
                        r.center() + vec2(0.0, 14.0),
                        vec2(r.width() - 12.0, 16.0),
                    );
                    let g = widgets::elided(ui, &d.name, regular(11.5), p.muted, name.width());
                    ui.painter().galley(
                        pos2(name.center().x - g.size().x / 2.0, name.top()),
                        g,
                        p.muted,
                    );
                }
            });
            // One card per display.
            let per_row = if ui.available_width() >= 760.0 { 2 } else { 1 };
            let numbered: Vec<(usize, &DisplayInfo)> = displays.iter().enumerate().collect();
            for row in numbered.chunks(per_row) {
                let weights = vec![1.0; row.len()];
                pages::card_row(ui, &weights, &mut |ui, i| {
                    let (index, d) = &row[i];
                    let (head, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 62.0), Sense::hover());
                    // A little monitor with the display's proportions.
                    let aspect = d.width.max(1) as f32 / d.height.max(1) as f32;
                    let (w, h) = if aspect >= 1.0 {
                        (72.0, (72.0 / aspect).max(20.0))
                    } else {
                        ((50.0 * aspect).max(20.0), 50.0)
                    };
                    let screen = Rect::from_center_size(
                        pos2(head.left() + 38.0, head.top() + 26.0),
                        vec2(w, h),
                    );
                    ui.painter().rect(
                        screen,
                        CornerRadius::same(4),
                        if d.primary { p.selected } else { p.hover },
                        Stroke::new(1.5_f32, if d.primary { p.accent } else { p.faint }),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(
                        screen.center(),
                        Align2::CENTER_CENTER,
                        (index + 1).to_string(),
                        semibold(14.0),
                        p.text,
                    );
                    ui.painter().line_segment(
                        [
                            pos2(screen.center().x, screen.bottom()),
                            pos2(screen.center().x, screen.bottom() + 5.0),
                        ],
                        Stroke::new(2.0_f32, p.faint),
                    );
                    ui.painter().line_segment(
                        [
                            pos2(screen.center().x - 12.0, screen.bottom() + 6.0),
                            pos2(screen.center().x + 12.0, screen.bottom() + 6.0),
                        ],
                        Stroke::new(2.0_f32, p.faint),
                    );
                    let text_left = head.left() + 88.0;
                    widgets::text_in(
                        ui,
                        Rect::from_min_max(
                            pos2(text_left, head.top() + 2.0),
                            pos2(head.right(), head.top() + 24.0),
                        ),
                        &d.name,
                        semibold(16.0),
                        p.text,
                    );
                    let mut x = text_left;
                    let badges = Rect::from_min_max(
                        pos2(x, head.top() + 30.0),
                        pos2(head.right(), head.top() + 50.0),
                    );
                    let mut badge = |text: &str, color| {
                        let r = Rect::from_min_max(pos2(x, badges.top()), badges.max);
                        x += widgets::paint_badge(ui, r, text, color) + 6.0;
                    };
                    if d.primary {
                        badge("Primary", p.accent);
                    }
                    badge(&format!("{} x {}", d.width, d.height), p.muted);
                    if d.refresh_hz > 0.0 {
                        badge(&format!("{:.0} Hz", d.refresh_hz), p.muted);
                    }
                    if d.hdr_enabled == Some(true) {
                        badge("HDR on", p.info);
                    }
                    let depth = d
                        .extra
                        .iter()
                        .find(|(l, _)| {
                            l.eq_ignore_ascii_case("colour depth")
                                || l.eq_ignore_ascii_case("color depth")
                        })
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| {
                            if d.bits_per_pixel > 0 {
                                format!("{}-bit colour", d.bits_per_pixel)
                            } else {
                                String::new()
                            }
                        });
                    let mut items: Vec<(&str, String)> = vec![
                        ("Resolution", format!("{} x {} pixels", d.width, d.height)),
                        (
                            "Refresh rate",
                            if d.refresh_hz > 0.0 {
                                format!("{:.0} Hz", d.refresh_hz)
                            } else {
                                String::new()
                            },
                        ),
                        (
                            "Scaling",
                            if d.scale_pct > 0 {
                                format!("{}%", d.scale_pct)
                            } else {
                                String::new()
                            },
                        ),
                        ("Colour depth", depth),
                        (
                            "HDR",
                            match (d.hdr_supported, d.hdr_enabled) {
                                (_, Some(true)) => "Supported, switched on".to_string(),
                                (Some(true), _) => "Supported, switched off".to_string(),
                                (Some(false), _) => "Not supported".to_string(),
                                (None, _) => String::new(),
                            },
                        ),
                        ("Connection", d.connection.clone()),
                        (
                            "Size",
                            match (d.size_mm, d.diagonal_inches) {
                                (Some((w, h)), Some(diag)) => format!("{diag:.1}\" ({w} x {h} mm)"),
                                (Some((w, h)), None) => format!("{w} x {h} mm"),
                                (None, Some(diag)) => format!("{diag:.1}\""),
                                (None, None) => String::new(),
                            },
                        ),
                        (
                            "Position on the desktop",
                            format!("{}, {}", d.position.0, d.position.1),
                        ),
                        ("Orientation", d.orientation.clone()),
                        ("Connected to", d.adapter.clone()),
                        ("Manufacturer", d.manufacturer.clone()),
                    ];
                    for (label, value) in &d.extra {
                        if !label.to_ascii_lowercase().contains("depth") {
                            items.push((label.as_str(), value.clone()));
                        }
                    }
                    pages::facts(ui, 2, &items);
                });
            }
        });
    }

    // ------------------------------------------------------------------------------
    // Sensors
    // ------------------------------------------------------------------------------

    pub(super) fn sensors_ui(&mut self, ui: &mut Ui) {
        let s = self.data.sample.clone();
        let missing = s.sensors.iter().filter(|r| r.value.is_none()).count();
        let subtitle = if s.sensors.is_empty() {
            "No sensor readings yet".to_string()
        } else {
            format!(
                "{} readings{} · minimum and maximum over the last {}",
                s.sensors.len() - missing,
                if missing > 0 {
                    format!(", {missing} not available")
                } else {
                    String::new()
                },
                crate::history::WINDOWS
                    .get(self.settings.history_window)
                    .map(|w| w.0)
                    .unwrap_or("5 min")
            )
        };
        App::page_header(ui, "Sensors", &subtitle, |_| {});
        let hist = self.hist();
        let f = self.settings.fahrenheit;
        let kinds = sensor_kinds(&s.sensors);
        if self
            .st
            .sensor_filter
            .is_some_and(|k| !kinds.iter().any(|x| x.0 == k))
        {
            self.st.sensor_filter = None;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if widgets::chip(
                ui,
                "All",
                Some(s.sensors.len()),
                self.st.sensor_filter.is_none(),
            )
            .clicked()
            {
                self.st.sensor_filter = None;
            }
            for (kind, n) in &kinds {
                if widgets::chip(
                    ui,
                    kind.label(),
                    Some(*n),
                    self.st.sensor_filter == Some(*kind),
                )
                .clicked()
                {
                    self.st.sensor_filter = Some(*kind);
                }
            }
        });
        ui.add_space(4.0);
        let filter = self.st.sensor_filter;
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            if s.sensors.is_empty() {
                widgets::card(ui, |ui| {
                    widgets::empty_state(
                        ui,
                        Icon::SENSOR,
                        p.muted,
                        "No sensor readings yet",
                        "Windows exposes only a few sensors without a driver. Whatever it does expose appears here.",
                    );
                });
                return;
            }
            let cols = [
                col("Sensor", 0.8),
                col("Kind", 96.0),
                col_right("Now", 92.0),
                col_right("Minimum", 84.0),
                col_right("Maximum", 84.0),
                col("Source", 1.0),
            ];
            for group in sensor_groups(&s.sensors) {
                let rows: Vec<Vec<Cell>> = s
                    .sensors
                    .iter()
                    .filter(|r| r.group == group && filter.is_none_or(|k| k == r.kind))
                    .map(|r| {
                        let stats = hist.stats(&keys::sensor(&r.group, &r.name));
                        let extreme = |v: Option<f32>| match (r.value, v) {
                            (Some(_), Some(v)) => {
                                Cell::new(sensor_value(r.kind, f64::from(v), f)).color(p.muted)
                            }
                            _ => Cell::new("").color(p.faint),
                        };
                        let source =
                            match (r.value.is_some(), r.source.is_empty(), r.note.is_empty()) {
                                (true, false, false) => format!("{} ({})", r.source, r.note),
                                (true, false, true) => r.source.clone(),
                                (true, true, _) => r.note.clone(),
                                (false, _, false) => {
                                    let mut note = r.note.clone();
                                    if let Some(first) = note.get(..1) {
                                        note = format!("{}{}", first.to_uppercase(), &note[1..]);
                                    }
                                    note
                                }
                                (false, _, true) => "This computer does not report it".to_string(),
                            };
                        vec![
                            Cell::new(r.name.clone()).strong(),
                            Cell::new(r.kind.label()).color(p.muted),
                            match r.value {
                                Some(v) => Cell::new(sensor_value(r.kind, v, f)).strong(),
                                None => Cell::new("Not available").color(p.faint),
                            },
                            extreme(stats.map(|x| x.min)),
                            extreme(stats.map(|x| x.max)),
                            Cell::new(source).color(p.muted),
                        ]
                    })
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                widgets::card(ui, |ui| {
                    widgets::section(ui, group);
                    pages::table(ui, &cols, &rows, 30.0);
                });
            }
            pages::note(
                ui,
                "PulseForge installs no driver. Voltages, most fan speeds and per-core temperatures are only reachable through one, so they are listed as not available instead of being guessed.",
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(group: &str, name: &str, kind: SensorKind, value: Option<f64>) -> SensorReading {
        SensorReading {
            group: group.into(),
            name: name.into(),
            kind,
            value,
            ..Default::default()
        }
    }

    #[test]
    fn sensor_values_carry_their_unit() {
        assert_eq!(sensor_value(SensorKind::Temperature, 54.4, false), "54 °C");
        assert_eq!(sensor_value(SensorKind::Temperature, 100.0, true), "212 °F");
        assert_eq!(sensor_value(SensorKind::Fan, 2344.6, false), "2,345 RPM");
        assert_eq!(sensor_value(SensorKind::Fan, -5.0, false), "0 RPM");
        assert_eq!(sensor_value(SensorKind::Voltage, 12.036, false), "12.04 V");
        assert_eq!(sensor_value(SensorKind::Power, 13.24, false), "13.2 W");
        assert_eq!(sensor_value(SensorKind::Load, 140.0, false), "100%");
        assert_eq!(sensor_value(SensorKind::Clock, 4200.0, false), "4.20 GHz");
        assert_eq!(sensor_value(SensorKind::Current, 1.2, false), "1.20 A");
    }

    #[test]
    fn sensors_group_and_count() {
        let list = vec![
            reading("Processor", "Package", SensorKind::Temperature, Some(50.0)),
            reading("Mainboard", "Fan", SensorKind::Fan, None),
            reading("Processor", "Core voltage", SensorKind::Voltage, None),
            reading("Mainboard", "Chassis", SensorKind::Temperature, Some(30.0)),
        ];
        assert_eq!(sensor_groups(&list), ["Processor", "Mainboard"]);
        assert_eq!(
            sensor_kinds(&list),
            [
                (SensorKind::Temperature, 2),
                (SensorKind::Fan, 1),
                (SensorKind::Voltage, 1)
            ]
        );
        assert!(sensor_kinds(&[]).is_empty());
    }

    #[test]
    fn connected_hardware_comes_first() {
        let adapter = |name: &str, kind: &str, up| NetSample {
            name: name.into(),
            kind: kind.into(),
            up,
            ..Default::default()
        };
        let list = vec![
            adapter("virtual", "Virtual", true),
            adapter("cable", "Ethernet", false),
            adapter("wifi", "Wi-Fi", true),
        ];
        let names: Vec<&str> = adapter_order(&list)
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(names, ["wifi", "cable", "virtual"]);
    }

    #[test]
    fn arrangement_keeps_proportions_and_fits() {
        let display = |x, y, w, h| DisplayInfo {
            position: (x, y),
            width: w,
            height: h,
            ..Default::default()
        };
        let area = Rect::from_min_size(pos2(100.0, 50.0), vec2(600.0, 120.0));
        let rects = arrangement(
            &[display(0, 0, 2560, 1600), display(2560, -120, 2560, 1440)],
            area,
        );
        assert_eq!(rects.len(), 2);
        for r in &rects {
            assert!(area.expand(0.5).contains_rect(*r), "{r:?}");
        }
        // Same scale for both, side by side, the second one starting higher.
        assert!((rects[0].width() - rects[1].width()).abs() < 0.01);
        assert!((rects[0].right() - rects[1].left()).abs() < 0.01);
        assert!(rects[1].top() < rects[0].top());
        assert!((rects[0].width() / rects[0].height() - 1.6).abs() < 0.01);
        assert!(arrangement(&[], area).is_empty());
        // Zero-sized entries do not divide by zero.
        let odd = arrangement(&[display(0, 0, 0, 0)], area);
        assert!(odd[0].width().is_finite() && odd[0].height().is_finite());
    }
}
