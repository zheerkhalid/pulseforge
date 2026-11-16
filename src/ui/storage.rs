//! The Storage page: the drive list, and for the selected drive its
//! identity and health, the SMART / NVMe attributes, activity graphs,
//! partitions and volumes.

use super::app::App;
use super::charts::{self, Unit};
use super::pages::{self, col, col_right, Cell, GAP};
use super::theme::{pal, regular, semibold, Icon, Palette};
use super::widgets;
use crate::history::keys;
use crate::model::{DriveInfo, HealthStatus, Slow, Volume};
use crate::util;
use eframe::egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui,
};

/// "931 GB · NVMe SSD", "3.64 TB · USB HDD, 5400 RPM".
pub(super) fn drive_caption(d: &DriveInfo) -> String {
    let mut kind = [d.interface.as_str(), d.media.as_str()]
        .iter()
        .filter(|x| !x.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if kind.is_empty() {
        kind = "Drive".to_string();
    }
    if let Some(rpm) = d.rotation_rpm {
        kind.push_str(&format!(", {rpm} RPM"));
    }
    format!("{} · {kind}", util::fmt_bytes(d.size_bytes))
}

/// The volumes that live on drive `index`, then (second list) the others.
pub(super) fn volumes_of(slow: &Slow, index: u32) -> (Vec<&Volume>, Vec<&Volume>) {
    slow.volumes
        .iter()
        .partition(|v| v.drive_index == Some(index))
}

/// The parts of a drive for the partition map: (label, bytes, is a gap).
/// Space that no partition covers shows up as "Unallocated" when it is more
/// than a rounding leftover.
pub(super) fn partition_map(d: &DriveInfo) -> Vec<(String, u64, bool)> {
    let mut parts: Vec<_> = d.partitions.iter().collect();
    parts.sort_by_key(|q| q.offset);
    let slack = (d.size_bytes / 200).max(64 << 20);
    let mut out = Vec::new();
    let mut cursor = 0_u64;
    for q in parts {
        if q.offset > cursor && q.offset - cursor > slack {
            out.push(("Unallocated".to_string(), q.offset - cursor, true));
        }
        let label = if q.letter.is_empty() {
            q.kind.clone()
        } else {
            format!("{} {}", q.letter, q.kind)
        };
        out.push((label, q.size, false));
        cursor = cursor.max(q.offset.saturating_add(q.size));
    }
    if d.size_bytes > cursor && d.size_bytes - cursor > slack {
        out.push(("Unallocated".to_string(), d.size_bytes - cursor, true));
    }
    out
}

fn volume_rows(
    p: &Palette,
    volumes: &[&Volume],
    disk: Color32,
    slow: &Slow,
    with_drive: bool,
) -> Vec<Vec<Cell>> {
    volumes
        .iter()
        .map(|v| {
            let used = v.used_pct();
            let mut row = vec![
                Cell::new(if v.label.is_empty() {
                    v.mount.clone()
                } else {
                    format!("{}  {}", v.mount, v.label)
                })
                .strong(),
                Cell::new(v.filesystem.clone()).color(p.muted),
                if v.total > 0 {
                    Cell::new(util::fmt_pct(used))
                        .bar(used / 100.0, pages::usage_color(p, disk, used))
                } else {
                    Cell::new("Not available").color(p.faint)
                },
                Cell::new(util::fmt_bytes(v.used())),
                Cell::new(util::fmt_bytes(v.free)),
                Cell::new(util::fmt_bytes(v.total)),
                match v.bitlocker {
                    Some(true) => Cell::new("Encrypted"),
                    Some(false) => Cell::new("Not encrypted").color(p.muted),
                    None => Cell::new("Unknown").color(p.faint),
                },
            ];
            if with_drive {
                let drive = v
                    .drive_index
                    .and_then(|i| slow.drives.iter().find(|d| d.index == i))
                    .map(|d| d.model.clone())
                    .unwrap_or_default();
                row.push(Cell::new(drive).color(p.muted));
            }
            row
        })
        .collect()
}

const VOLUME_COLS: [pages::Col; 7] = [
    col("Volume", 0.8),
    col("File system", 86.0),
    col("Used", 1.0),
    col_right("Used", 76.0),
    col_right("Free", 76.0),
    col_right("Size", 76.0),
    col("Encryption", 104.0),
];

impl App {
    fn drive_selector(&mut self, ui: &mut Ui, slow: &Slow, selected: u32) {
        let p = pal(ui);
        let f = self.settings.fahrenheit;
        let series = charts::series_ui(ui);
        let width = ui.available_width();
        // A lone drive keeps the size of a card instead of spanning the page.
        let columns = pages::columns_for(width, 250.0, slow.drives.len().clamp(3, 4));
        let card_w = ((width - GAP * (columns as f32 - 1.0)) / columns as f32).floor();
        for row in slow.drives.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for d in row {
                    let (rect, resp) = ui.allocate_exact_size(vec2(card_w, 84.0), Sense::click());
                    let active = d.index == selected;
                    ui.painter().rect(
                        rect,
                        CornerRadius::same(12),
                        if active {
                            p.selected
                        } else if resp.hovered() {
                            p.hover
                        } else {
                            p.card
                        },
                        Stroke::new(1.0_f32, if active { p.accent } else { p.border }),
                        StrokeKind::Inside,
                    );
                    let inner = rect.shrink2(vec2(14.0, 11.0));
                    ui.painter().text(
                        pos2(inner.left() + 9.0, inner.top() + 10.0),
                        Align2::CENTER_CENTER,
                        Icon::DRIVE.glyph(),
                        regular(16.0),
                        series.disk,
                    );
                    widgets::text_in(
                        ui,
                        Rect::from_min_max(
                            pos2(inner.left() + 26.0, inner.top()),
                            pos2(inner.right(), inner.top() + 20.0),
                        ),
                        if d.model.is_empty() {
                            "Unnamed drive"
                        } else {
                            &d.model
                        },
                        semibold(14.5),
                        p.text,
                    );
                    widgets::text_in(
                        ui,
                        Rect::from_min_max(
                            pos2(inner.left() + 26.0, inner.top() + 20.0),
                            pos2(inner.right(), inner.top() + 38.0),
                        ),
                        &format!("Drive {} · {}", d.index, drive_caption(d)),
                        regular(12.5),
                        p.muted,
                    );
                    let low =
                        Rect::from_min_max(pos2(inner.left(), inner.bottom() - 20.0), inner.max);
                    widgets::paint_badge(
                        ui,
                        low,
                        &pages::health_text(d.health),
                        pages::health_color(&p, d.health),
                    );
                    let right = match d.temperature_c {
                        Some(t) => util::fmt_temp(t, f),
                        None => "No temperature".to_string(),
                    };
                    widgets::text_in_right(
                        ui,
                        Rect::from_min_max(pos2(low.center().x, low.top()), low.max),
                        &right,
                        regular(12.5),
                        if d.temperature_c.is_some() {
                            p.text
                        } else {
                            p.faint
                        },
                    );
                    if resp.clicked() {
                        self.st.storage_drive = Some(d.index);
                    }
                }
            });
        }
    }

    /// The box that explains why health details are missing.
    fn smart_note(&mut self, ui: &mut Ui, d: &DriveInfo) {
        if d.smart_note.is_empty() {
            return;
        }
        let p = pal(ui);
        let needs_admin =
            d.smart_note.to_ascii_lowercase().contains("administrator") && !self.elevated;
        let mut restart = false;
        eframe::egui::Frame::new()
            .fill(p.warn.gamma_multiply(0.1))
            .stroke(Stroke::new(1.0_f32, p.warn.gamma_multiply(0.4)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(eframe::egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new(Icon::INFO.glyph()).color(p.warn));
                    ui.label(format!("Health details are not available: {}.", d.smart_note.trim_end_matches('.')));
                });
                if needs_admin
                    && widgets::ghost_button(ui, &Icon::SHIELD.with("Restart as administrator"))
                        .on_hover_text("Windows asks for consent, then PulseForge starts again with the rights to read drive health.")
                        .clicked()
                {
                    restart = true;
                }
            });
        if restart {
            if self.demo {
                self.toast("Demo mode: nothing on this PC is changed.", false);
            } else {
                let ctx = ui.ctx().clone();
                self.restart_as_admin(&ctx);
            }
        }
    }

    fn drive_overview(&mut self, ui: &mut Ui, slow: &Slow, d: &DriveInfo) {
        let hist = self.hist();
        let time = self.time_axis();
        let series = charts::series_ui(ui);
        let f = self.settings.fahrenheit;
        pages::card_row(ui, &[1.0, 1.25], &mut |ui, i| {
            let p = pal(ui);
            if i == 0 {
                let color = pages::health_color(&p, d.health);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(
                        RichText::new(pages::health_icon(d.health).glyph())
                            .size(24.0)
                            .color(color),
                    );
                    ui.label(RichText::new(d.health.label()).font(semibold(24.0)));
                    if let Some(pct) = d.health_pct {
                        ui.vertical(|ui| {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!("{pct}% of its life remaining"))
                                    .color(p.muted),
                            );
                        });
                    }
                });
                if let Some(pct) = d.health_pct {
                    charts::meter(ui, f32::from(pct) / 100.0, color, ui.available_width(), 8.0);
                }
                if !d.health_summary.is_empty() {
                    ui.add(eframe::egui::Label::new(&d.health_summary).wrap());
                }
                self.smart_note(ui, d);
                ui.add_space(2.0);
                let bytes = |v: Option<u64>| v.map(util::fmt_bytes).unwrap_or_default();
                let mut items: Vec<(&str, String)> = vec![
                    ("Model", d.model.clone()),
                    ("Firmware", d.firmware.clone()),
                    ("Serial number", self.serial(&d.serial)),
                    ("Interface", d.interface.clone()),
                    (
                        "Media",
                        match (d.media.as_str(), d.rotation_rpm) {
                            ("", _) => String::new(),
                            (m, Some(rpm)) => format!("{m}, {rpm} RPM"),
                            (m, None) => m.to_string(),
                        },
                    ),
                    ("Capacity", util::fmt_bytes(d.size_bytes)),
                    (
                        "Temperature",
                        d.temperature_c
                            .map(|t| util::fmt_temp(t, f))
                            .unwrap_or_default(),
                    ),
                    (
                        "Power-on time",
                        d.power_on_hours
                            .map(|h| format!("{} ({} h)", util::fmt_hours(h), util::fmt_count(h)))
                            .unwrap_or_default(),
                    ),
                    (
                        "Power cycles",
                        d.power_cycles.map(util::fmt_count).unwrap_or_default(),
                    ),
                    ("Total written", bytes(d.total_written_bytes)),
                    ("Total read", bytes(d.total_read_bytes)),
                    ("Health data from", d.smart_source.clone()),
                ];
                for (label, value) in &d.extra {
                    items.push((label.as_str(), value.clone()));
                }
                pages::facts(ui, 2, &items);
            } else {
                charts::chart(
                    ui,
                    ("drive-rate", d.index),
                    "Read and write",
                    Unit::Rate,
                    &[
                        hist.line("Read", series.disk, &keys::disk_read(d.index)),
                        hist.line("Write", series.second, &keys::disk_write(d.index)),
                    ],
                    150.0,
                    &time,
                    "",
                );
                ui.add_space(4.0);
                charts::chart(
                    ui,
                    ("drive-active", d.index),
                    "Active time",
                    Unit::Percent,
                    &[hist.line("Active", series.disk, &keys::disk_active(d.index))],
                    96.0,
                    &time,
                    "",
                );
                let temp = hist.line("Temperature", series.disk, &keys::disk_temp(d.index));
                if !temp.points.is_empty() {
                    ui.add_space(4.0);
                    charts::chart(
                        ui,
                        ("drive-temp", d.index),
                        "Temperature",
                        Unit::Temperature,
                        &[temp],
                        80.0,
                        &time,
                        "",
                    );
                }
            }
        });
        widgets::card(ui, |ui| {
            let p = pal(ui);
            widgets::section(ui, "Volumes");
            if slow.volumes.is_empty() {
                pages::note(ui, "Windows reports no mounted volumes.");
                return;
            }
            let (mine, others) = volumes_of(slow, d.index);
            let mut all = mine;
            all.extend(others);
            let mut cols = VOLUME_COLS.to_vec();
            cols.push(col("Drive", 0.9));
            pages::table(
                ui,
                &cols,
                &volume_rows(&p, &all, series.disk, slow, true),
                34.0,
            );
        });
    }

    fn drive_health(&mut self, ui: &mut Ui, d: &DriveInfo) {
        widgets::card(ui, |ui| {
            let p = pal(ui);
            ui.horizontal(|ui| {
                widgets::section(ui, "Health attributes");
                if !d.smart_source.is_empty() {
                    ui.label(
                        RichText::new(format!("source: {}", d.smart_source))
                            .size(12.5)
                            .color(p.muted),
                    );
                }
                widgets::badge(
                    ui,
                    &pages::health_text(d.health),
                    pages::health_color(&p, d.health),
                );
            });
            if !d.health_summary.is_empty() {
                ui.add(
                    eframe::egui::Label::new(RichText::new(&d.health_summary).color(p.muted))
                        .wrap(),
                );
            }
            self.smart_note(ui, d);
            if d.attributes.is_empty() {
                if d.smart_note.is_empty() {
                    pages::note(ui, "This drive does not report health attributes.");
                }
                return;
            }
            let ata = d.attributes.iter().any(|a| a.id.is_some());
            let status = |h: HealthStatus| {
                Cell::new(pages::health_text(h)).badge(pages::health_color(&p, h))
            };
            let number = |v: Option<u8>| match v {
                Some(v) => Cell::new(v.to_string()).color(p.muted),
                None => Cell::default(),
            };
            let (cols, rows): (Vec<pages::Col>, Vec<Vec<Cell>>) = if ata {
                (
                    vec![
                        col_right("ID", 30.0),
                        col("Attribute and what it means", 1.0),
                        col_right("Value", 104.0),
                        col_right("Current", 58.0),
                        col_right("Worst", 48.0),
                        col_right("Limit", 44.0),
                        col("Status", 92.0),
                    ],
                    d.attributes
                        .iter()
                        .map(|a| {
                            vec![
                                Cell::new(a.id.map(|i| i.to_string()).unwrap_or_default())
                                    .color(p.faint),
                                Cell::new(a.name.clone())
                                    .strong()
                                    .sub(a.explanation.clone()),
                                Cell::new(a.display.clone()).strong(),
                                number(a.current),
                                number(a.worst),
                                number(a.threshold),
                                status(a.status),
                            ]
                        })
                        .collect(),
                )
            } else {
                (
                    vec![
                        col("Attribute and what it means", 1.0),
                        col_right("Value", 130.0),
                        col("Status", 92.0),
                    ],
                    d.attributes
                        .iter()
                        .map(|a| {
                            vec![
                                Cell::new(a.name.clone())
                                    .strong()
                                    .sub(a.explanation.clone()),
                                Cell::new(a.display.clone()).strong(),
                                status(a.status),
                            ]
                        })
                        .collect(),
                )
            };
            pages::table(ui, &cols, &rows, if ata { 58.0 } else { 46.0 });
            if ata {
                pages::note(
                    ui,
                    "Current and Worst are the drive's own scores (higher is better); the attribute fails when a score falls to its limit.",
                );
            }
        });
    }

    fn drive_volumes(&mut self, ui: &mut Ui, slow: &Slow, d: &DriveInfo) {
        let series = charts::series_ui(ui);
        widgets::card(ui, |ui| {
            let p = pal(ui);
            ui.horizontal(|ui| {
                widgets::section(ui, "Partitions");
                if !d.partition_style.is_empty() {
                    widgets::badge(ui, &d.partition_style, p.muted);
                }
            });
            if d.partitions.is_empty() {
                pages::note(ui, "No partitions were reported for this drive.");
                return;
            }
            // The map: data partitions in the drive colour, the rest neutral.
            let neutral = if p.dark {
                p.faint.gamma_multiply(0.7)
            } else {
                p.faint.gamma_multiply(0.6)
            };
            let gap_color = if p.dark {
                p.border
            } else {
                p.border.gamma_multiply(0.9)
            };
            let map = partition_map(d);
            let total: u64 = map.iter().map(|m| m.1).sum::<u64>().max(1);
            // Tiny partitions still get a visible sliver.
            let shares: Vec<f32> = map
                .iter()
                .map(|m| (m.1 as f32 / total as f32).max(0.006))
                .collect();
            let parts: Vec<(f32, Color32)> = map
                .iter()
                .zip(&shares)
                .map(|((label, _, gap), share)| {
                    let has_letter = label.chars().nth(1) == Some(':');
                    (
                        *share,
                        if *gap {
                            gap_color
                        } else if has_letter {
                            series.disk
                        } else {
                            neutral
                        },
                    )
                })
                .collect();
            let bar = charts::segmented_bar(ui, &parts, 26.0);
            // Names inside the blocks that are wide enough for them.
            let sum: f32 = shares.iter().sum();
            let usable = bar.width() - 2.0 * (shares.len().saturating_sub(1)) as f32;
            let mut x = bar.left();
            for ((label, bytes, gap), share) in map.iter().zip(&shares) {
                let w = usable * share / sum;
                let text = format!("{label} · {}", util::fmt_bytes(*bytes));
                // Only the data partitions are named inside their block (near-black
                // on the drive colour); the table below lists everything.
                let ink = Color32::from_rgb(0x0b, 0x0b, 0x0b);
                let named = !*gap && label.chars().nth(1) == Some(':');
                let g = ui.painter().layout_no_wrap(text, semibold(11.5), ink);
                if named && g.size().x + 16.0 <= w {
                    ui.painter()
                        .galley(pos2(x + 8.0, bar.center().y - g.size().y / 2.0), g, ink);
                }
                x += w + 2.0;
            }
            ui.add_space(2.0);
            let mut sorted: Vec<_> = d.partitions.iter().collect();
            sorted.sort_by_key(|q| q.offset);
            let rows: Vec<Vec<Cell>> = sorted
                .iter()
                .map(|q| {
                    vec![
                        Cell::new(q.number.to_string()).color(p.muted),
                        Cell::new(q.kind.clone()).strong(),
                        if q.letter.is_empty() {
                            Cell::new("No letter").color(p.faint)
                        } else {
                            Cell::new(q.letter.clone())
                        },
                        Cell::new(util::fmt_bytes(q.size)),
                        Cell::new(util::fmt_bytes(q.offset)).color(p.muted),
                    ]
                })
                .collect();
            pages::table(
                ui,
                &[
                    col_right("No.", 30.0),
                    col("Kind", 1.0),
                    col("Letter", 90.0),
                    col_right("Size", 100.0),
                    col_right("Starts at", 100.0),
                ],
                &rows,
                30.0,
            );
        });
        let (mine, others) = volumes_of(slow, d.index);
        widgets::card(ui, |ui| {
            let p = pal(ui);
            widgets::section(ui, "Volumes on this drive");
            if mine.is_empty() {
                pages::note(ui, "No mounted volume is known to be on this drive.");
            } else {
                pages::table(
                    ui,
                    &VOLUME_COLS,
                    &volume_rows(&p, &mine, series.disk, slow, false),
                    34.0,
                );
            }
        });
        if !others.is_empty() {
            widgets::card(ui, |ui| {
                let p = pal(ui);
                widgets::section(ui, "Volumes on other drives");
                let mut cols = VOLUME_COLS.to_vec();
                cols.push(col("Drive", 0.9));
                pages::table(
                    ui,
                    &cols,
                    &volume_rows(&p, &others, series.disk, slow, true),
                    34.0,
                );
            });
        }
    }

    pub(super) fn storage_ui(&mut self, ui: &mut Ui) {
        let slow = self.data.slow.clone();
        let subtitle = if !self.data.slow_ready {
            "Reading drives...".to_string()
        } else {
            format!(
                "{} {} · {} {}",
                slow.drives.len(),
                if slow.drives.len() == 1 {
                    "drive"
                } else {
                    "drives"
                },
                slow.volumes.len(),
                if slow.volumes.len() == 1 {
                    "volume"
                } else {
                    "volumes"
                },
            )
        };
        let mut refresh = false;
        App::page_header(ui, "Storage", &subtitle, |ui| {
            if widgets::ghost_button(ui, &Icon::REFRESH.with("Read again"))
                .on_hover_text(
                    "Drive health and free space are read once a minute. This reads them now.",
                )
                .clicked()
            {
                refresh = true;
            }
            ui.add_space(6.0);
            self.reveal_toggle(ui);
        });
        if refresh {
            self.monitor.refresh_slow();
            self.toast(
                if self.demo {
                    "Demo mode: the invented drives stay as they are."
                } else {
                    "Reading the drives again..."
                },
                false,
            );
        }
        if !self.data.slow_ready {
            pages::loading(ui, "Drive health and volumes take a few seconds.");
            return;
        }
        if slow.drives.is_empty() {
            let p = pal(ui);
            pages::page_scroll(ui, |ui| {
                widgets::card(ui, |ui| {
                    widgets::empty_state(
                        ui,
                        Icon::DRIVE,
                        p.muted,
                        "No drives reported",
                        "Windows did not list any physical drive. Volumes, if any, are shown below.",
                    );
                });
                if !slow.volumes.is_empty() {
                    let series = charts::series_ui(ui);
                    widgets::card(ui, |ui| {
                        widgets::section(ui, "Volumes");
                        let all: Vec<&Volume> = slow.volumes.iter().collect();
                        pages::table(
                            ui,
                            &VOLUME_COLS,
                            &volume_rows(&p, &all, series.disk, &slow, false),
                            34.0,
                        );
                    });
                }
            });
            return;
        }
        let selected = self
            .st
            .storage_drive
            .filter(|i| slow.drives.iter().any(|d| d.index == *i))
            .unwrap_or(slow.drives[0].index);
        pages::page_scroll(ui, |ui| {
            self.drive_selector(ui, &slow, selected);
            let Some(d) = slow.drives.iter().find(|d| d.index == selected) else {
                return;
            };
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 36.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    let attributes = if d.attributes.is_empty() {
                        "Health details".to_string()
                    } else {
                        format!("Health details ({})", d.attributes.len())
                    };
                    if let Some(i) = widgets::segmented(
                        ui,
                        &["Overview", &attributes, "Partitions and volumes"],
                        self.st.storage_tab.min(2),
                    ) {
                        self.st.storage_tab = i;
                    }
                },
            );
            match self.st.storage_tab {
                1 => self.drive_health(ui, d),
                2 => self.drive_volumes(ui, &slow, d),
                _ => self.drive_overview(ui, &slow, d),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Partition;

    #[test]
    fn captions_describe_the_drive() {
        let mut d = DriveInfo {
            size_bytes: 1 << 40,
            interface: "NVMe".into(),
            media: "SSD".into(),
            ..Default::default()
        };
        assert_eq!(drive_caption(&d), "1.00 TB · NVMe SSD");
        d.interface.clear();
        d.media = "HDD".into();
        d.rotation_rpm = Some(5400);
        assert_eq!(drive_caption(&d), "1.00 TB · HDD, 5400 RPM");
        assert_eq!(drive_caption(&DriveInfo::default()), "0 B · Drive");
    }

    #[test]
    fn partition_map_shows_gaps_but_not_rounding() {
        let gb = 1_u64 << 30;
        let part = |number, offset, size, letter: &str| Partition {
            number,
            kind: "Basic data".into(),
            offset,
            size,
            letter: letter.into(),
        };
        let d = DriveInfo {
            size_bytes: 1000 * gb,
            // Out of order on purpose; one megabyte of slack at the start.
            partitions: vec![
                part(2, 500 * gb, 300 * gb, "E:"),
                part(1, 1 << 20, 400 * gb, "C:"),
            ],
            ..Default::default()
        };
        let map = partition_map(&d);
        let labels: Vec<&str> = map.iter().map(|m| m.0.as_str()).collect();
        assert_eq!(
            labels,
            [
                "C: Basic data",
                "Unallocated",
                "E: Basic data",
                "Unallocated"
            ]
        );
        assert!(map[1].2 && map[3].2 && !map[0].2);
        assert_eq!(map[3].1, 200 * gb);
        // No partitions at all: the whole drive is unallocated.
        let empty = DriveInfo {
            size_bytes: 10 * gb,
            ..Default::default()
        };
        assert_eq!(partition_map(&empty).len(), 1);
        // Nonsense sizes do not overflow.
        let odd = DriveInfo {
            size_bytes: 10,
            partitions: vec![part(1, u64::MAX - 5, u64::MAX, "")],
            ..Default::default()
        };
        assert!(!partition_map(&odd).is_empty());
    }

    #[test]
    fn volumes_are_split_by_drive() {
        let vol = |mount: &str, drive| Volume {
            mount: mount.into(),
            drive_index: drive,
            ..Default::default()
        };
        let slow = Slow {
            drives: Vec::new(),
            volumes: vec![
                vol("C:", Some(0)),
                vol("D:", Some(1)),
                vol("X:", None),
                vol("E:", Some(1)),
            ],
        };
        let (mine, others) = volumes_of(&slow, 1);
        assert_eq!(
            mine.iter().map(|v| v.mount.as_str()).collect::<Vec<_>>(),
            ["D:", "E:"]
        );
        assert_eq!(others.len(), 2);
    }
}
