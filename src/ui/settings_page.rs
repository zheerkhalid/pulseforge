//! The Settings page and the "Export report" dialog.

use super::alerts_page::setting_row;
use super::app::App;
use super::pages;
use super::theme::{pal, pal_ctx, semibold, Icon};
use super::widgets;
use crate::report::{self, Format, ReportData};
use crate::settings::{ThemeChoice, TrayValue, INTERVALS_MS};
use crate::{sys, util};
use eframe::egui::{self, vec2, Align, CornerRadius, Frame, Layout, Margin, RichText, Stroke, Ui};

/// "0.5 s", "1 s", "10 s".
pub fn interval_label(ms: u32) -> String {
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", f64::from(ms) / 1000.0)
    }
}

/// The CSV row intervals offered, in seconds.
pub const LOG_INTERVALS_S: [u32; 5] = [1, 5, 15, 60, 300];

/// "1 s", "15 s", "1 min", "5 min".
pub fn log_interval_label(seconds: u32) -> String {
    if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    }
}

/// The file name proposed for a report made at `unix`.
pub fn report_file_name(unix: u64, format: Format) -> String {
    format!(
        "PulseForge-report-{}.{}",
        util::file_stamp(unix),
        format.extension()
    )
}

fn switch(ui: &mut Ui, value: &mut bool) {
    if widgets::toggle(ui, *value, true).clicked() {
        *value = !*value;
    }
}

impl App {
    pub(super) fn open_export(&mut self) {
        self.st.export_open = true;
        self.st.export_mask = self.settings.report_mask;
    }

    pub(super) fn settings_ui(&mut self, ui: &mut Ui) {
        App::page_header(ui, "Settings", "Saved automatically", |_| {});
        let mut export = false;
        let mut about = false;
        let mut restart = false;
        let mut browse = false;
        let mut open_logs = false;
        let mut startup: Option<bool> = None;
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            ui.set_max_width(860.0);

            widgets::card(ui, |ui| {
                widgets::section(ui, "Appearance");
                ui.add_space(2.0);
                let mut theme = self.settings.theme;
                setting_row(
                    ui,
                    "Theme",
                    "\"System\" follows the Windows light or dark setting.",
                    |ui| {
                        let options = [ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::System];
                        let current = options.iter().position(|t| *t == theme).unwrap_or(0);
                        if let Some(i) =
                            widgets::segmented(ui, &["Dark", "Light", "System"], current)
                        {
                            theme = options[i];
                        }
                    },
                );
                self.settings.theme = theme;
                widgets::divider(ui);
                let mut fahrenheit = self.settings.fahrenheit;
                setting_row(
                    ui,
                    "Temperature unit",
                    "Used everywhere a temperature is shown, including alert limits and the tray icon.",
                    |ui| {
                        if let Some(i) = widgets::segmented(ui, &["°C", "°F"], usize::from(fahrenheit)) {
                            fahrenheit = i == 1;
                        }
                    },
                );
                self.settings.fahrenheit = fahrenheit;
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Monitoring");
                ui.add_space(2.0);
                let mut interval = self.settings.interval_ms;
                setting_row(
                    ui,
                    "Update interval",
                    "How often readings are taken. Longer intervals cost less processor time.",
                    |ui| {
                        let labels: Vec<String> =
                            INTERVALS_MS.iter().map(|ms| interval_label(*ms)).collect();
                        let labels: Vec<&str> = labels.iter().map(|l| l.as_str()).collect();
                        let current = INTERVALS_MS
                            .iter()
                            .position(|ms| *ms == interval)
                            .unwrap_or(1);
                        if let Some(i) = widgets::segmented(ui, &labels, current) {
                            interval = INTERVALS_MS[i];
                        }
                    },
                );
                self.settings.interval_ms = interval;
                widgets::divider(ui);
                let mut window = self.settings.history_window;
                setting_row(
                    ui,
                    "Graph history",
                    "The time span the graphs show. It can also be changed at the top of the window.",
                    |ui| {
                        egui::ComboBox::from_id_salt("history-window")
                            .width(120.0)
                            .selected_text(crate::history::WINDOWS.get(window).map(|w| w.0).unwrap_or("5 min"))
                            .show_ui(ui, |ui| {
                                for (i, (label, _)) in crate::history::WINDOWS.iter().enumerate() {
                                    ui.selectable_value(&mut window, i, *label);
                                }
                            });
                    },
                );
                self.settings.history_window = window;
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Privacy");
                ui.add_space(2.0);
                let mut mask = self.settings.mask_serials;
                setting_row(
                    ui,
                    "Hide serial numbers",
                    "Shows only the last characters of serial numbers and hardware addresses in this window, so that a screenshot does not give them away.",
                    |ui| switch(ui, &mut mask),
                );
                self.settings.mask_serials = mask;
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Notification area");
                ui.add_space(2.0);
                let mut icon = self.settings.tray_icon;
                setting_row(
                    ui,
                    "Show an icon next to the clock",
                    "The icon shows a live number and is where alert notifications come from.",
                    |ui| switch(ui, &mut icon),
                );
                self.settings.tray_icon = icon;
                widgets::divider(ui);
                let mut value = self.settings.tray_value;
                setting_row(
                    ui,
                    "Number in the icon",
                    "Which reading the icon displays.",
                    |ui| {
                        egui::ComboBox::from_id_salt("tray-value")
                            .width(190.0)
                            .selected_text(value.label())
                            .show_ui(ui, |ui| {
                                for v in TrayValue::ALL {
                                    ui.selectable_value(&mut value, v, v.label());
                                }
                            });
                    },
                );
                self.settings.tray_value = value;
                widgets::divider(ui);
                let mut close = self.settings.close_to_tray;
                setting_row(
                    ui,
                    "Keep running when the window is closed",
                    "Closing the window hides it in the notification area. Use the icon's menu to exit.",
                    |ui| switch(ui, &mut close),
                );
                self.settings.close_to_tray = close;
                widgets::divider(ui);
                let mut mini = self.settings.mini_widget;
                setting_row(
                    ui,
                    "Mini widget",
                    "A small window that stays on top with processor, memory, graphics and network at a glance. Drag it anywhere; right-click it to close.",
                    |ui| switch(ui, &mut mini),
                );
                self.settings.mini_widget = mini;
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Start with Windows");
                ui.add_space(2.0);
                let mut on = self.startup_on;
                setting_row(
                    ui,
                    "Start PulseForge when you sign in",
                    "Adds an entry for your user account only. No administrator rights are needed.",
                    |ui| {
                        if widgets::toggle(ui, on, true).clicked() {
                            on = !on;
                            startup = Some(on);
                        }
                    },
                );
                widgets::divider(ui);
                let mut minimized = self.settings.start_minimized;
                setting_row(
                    ui,
                    "Start in the notification area",
                    "When started with Windows, PulseForge stays out of the way until you open it.",
                    |ui| switch(ui, &mut minimized),
                );
                self.settings.start_minimized = minimized;
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Logging to CSV");
                ui.add_space(2.0);
                let mut log = self.settings.log_csv;
                setting_row(
                    ui,
                    "Write readings to a file",
                    "One row per interval with processor, memory, graphics, drive, network and battery readings. A new file is started every day.",
                    |ui| switch(ui, &mut log),
                );
                self.settings.log_csv = log;
                widgets::divider(ui);
                let mut every = self.settings.log_interval_s;
                setting_row(ui, "Row interval", "How often a row is written.", |ui| {
                    let labels: Vec<String> = LOG_INTERVALS_S
                        .iter()
                        .map(|s| log_interval_label(*s))
                        .collect();
                    let labels: Vec<&str> = labels.iter().map(|l| l.as_str()).collect();
                    let current = LOG_INTERVALS_S
                        .iter()
                        .position(|s| *s == every)
                        .unwrap_or(usize::MAX);
                    if let Some(i) = widgets::segmented(ui, &labels, current) {
                        every = LOG_INTERVALS_S[i];
                    }
                });
                self.settings.log_interval_s = every;
                widgets::divider(ui);
                let folder = self.settings.log_folder();
                setting_row(ui, "Folder", &folder.to_string_lossy(), |ui| {
                    if widgets::ghost_button(ui, &Icon::FOLDER_OPEN.with("Open folder")).clicked() {
                        open_logs = true;
                    }
                    if widgets::ghost_button(ui, "Browse...").clicked() {
                        browse = true;
                    }
                });
                if self.settings.log_csv {
                    widgets::divider(ui);
                    let file = self
                        .data
                        .log_file
                        .as_ref()
                        .and_then(|f| f.file_name())
                        .map(|f| f.to_string_lossy().to_string());
                    let status = match (&self.data.log_error, &file) {
                        (Some(e), _) => format!("Could not write: {e}"),
                        (None, Some(f)) => format!(
                            "Writing to {f} · {} rows since PulseForge started",
                            util::fmt_count(self.data.log_rows)
                        ),
                        (None, None) if self.demo => "Demo mode: nothing is written.".to_string(),
                        (None, None) => "The file is created with the next row.".to_string(),
                    };
                    ui.horizontal(|ui| {
                        let (icon, color) = if self.data.log_error.is_some() {
                            (Icon::ERROR, p.danger)
                        } else {
                            (Icon::DOCUMENT, p.muted)
                        };
                        ui.label(RichText::new(icon.glyph()).color(color));
                        ui.add(
                            egui::Label::new(RichText::new(status).size(12.5).color(p.muted))
                                .truncate(),
                        );
                    });
                }
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Report");
                ui.add_space(2.0);
                let mut mask = self.settings.report_mask;
                setting_row(
                    ui,
                    "Hide serial numbers and names in reports",
                    "Masks serial numbers, the computer and user name and network addresses, so that a report can be shared.",
                    |ui| switch(ui, &mut mask),
                );
                self.settings.report_mask = mask;
                widgets::divider(ui);
                setting_row(
                    ui,
                    "Export a report",
                    "Everything PulseForge knows about this computer in one HTML, text or JSON file.",
                    |ui| {
                        if widgets::ghost_button(ui, &Icon::EXPORT.with("Export report")).clicked() {
                            export = true;
                        }
                    },
                );
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Administrator rights");
                ui.add_space(2.0);
                let (title, text) = if self.elevated {
                    (
                        "PulseForge runs as administrator",
                        "Drive health details (SMART and NVMe logs) and every sensor Windows exposes are available.",
                    )
                } else {
                    (
                        "PulseForge runs with standard rights",
                        "Restarting as administrator unlocks drive health details (SMART and NVMe logs) and some sensors. PulseForge only reads: it never changes a setting, with or without these rights.",
                    )
                };
                let elevated = self.elevated;
                setting_row(ui, title, text, |ui| {
                    if elevated {
                        widgets::badge(ui, &Icon::SHIELD.with("Administrator"), p.good);
                    } else if widgets::ghost_button(
                        ui,
                        &Icon::SHIELD.with("Restart as administrator"),
                    )
                    .clicked()
                    {
                        restart = true;
                    }
                });
            });

            widgets::card(ui, |ui| {
                let o = self.data.overhead;
                ui.horizontal(|ui| {
                    widgets::section(ui, "Overhead");
                    ui.label(
                        RichText::new("what PulseForge itself costs, measured on itself")
                            .size(12.5)
                            .color(p.muted),
                    );
                });
                let columns = pages::columns_for(ui.available_width(), 150.0, 5);
                pages::facts(
                    ui,
                    columns,
                    &[
                        ("Processor now", format!("{:.2}%", o.cpu_pct)),
                        ("Processor average", format!("{:.2}%", o.cpu_avg_pct)),
                        ("Private memory", util::fmt_bytes(o.memory)),
                        ("Time per reading", format!("{:.1} ms", o.collect_avg_ms)),
                        ("Graph history", util::fmt_bytes(o.history_bytes as u64)),
                    ],
                );
            });

            widgets::card(ui, |ui| {
                setting_row(
                    ui,
                    &format!("PulseForge {}", env!("CARGO_PKG_VERSION")),
                    "Developed by Zheer Khalid · zheer.dev",
                    |ui| {
                        if widgets::ghost_button(ui, &Icon::INFO.with("About")).clicked() {
                            about = true;
                        }
                    },
                );
            });
        });

        if about {
            self.about_open = true;
        }
        if export {
            self.open_export();
        }
        if restart {
            if self.demo {
                self.toast("Demo mode: nothing on this PC is changed.", false);
            } else {
                let ctx = ui.ctx().clone();
                self.restart_as_admin(&ctx);
            }
        }
        if let Some(on) = startup {
            match sys::set_startup(on) {
                Ok(()) => self.startup_on = on,
                Err(e) => self.toast(e, !self.demo),
            }
        }
        if browse {
            if self.st.screenshot || self.demo {
                self.toast("Demo mode: the folder stays as it is.", false);
            } else if let Some(dir) = rfd::FileDialog::new()
                .set_title("Folder for the CSV logs")
                .set_directory(self.settings.log_folder())
                .pick_folder()
            {
                self.settings.log_dir = dir.to_string_lossy().to_string();
            }
        }
        if open_logs {
            if self.demo {
                self.toast("Demo mode: folders are not opened.", false);
            } else {
                let folder = self.settings.log_folder();
                let _ = std::fs::create_dir_all(&folder);
                if let Err(e) = sys::open_folder(&folder) {
                    self.toast(e, true);
                }
            }
        }
    }

    pub(super) fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.st.export_open {
            return;
        }
        let p = pal_ctx(ctx);
        let mut save = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("export-report"))
            .frame(
                Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.label(RichText::new("Export report").font(semibold(19.0)));
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "A snapshot of this computer: hardware, drives and their health, network adapters, sensors and the current readings.",
                        )
                        .color(p.muted),
                    )
                    .wrap(),
                );
                ui.add_space(2.0);
                ui.label(RichText::new("Format").size(12.0).color(p.faint));
                let formats = [Format::Html, Format::Text, Format::Json];
                let current = formats.iter().position(|f| *f == self.st.export_format).unwrap_or(0);
                if let Some(i) = widgets::segmented(ui, &["HTML", "Text", "JSON"], current) {
                    self.st.export_format = formats[i];
                }
                ui.label(
                    RichText::new(match self.st.export_format {
                        Format::Html => "A page that opens in any browser and prints well.",
                        Format::Text => "Plain text, easy to paste into a message or a forum post.",
                        Format::Json => "Structured data for scripts and other programs.",
                    })
                    .size(12.5)
                    .color(p.muted),
                );
                ui.add_space(2.0);
                Frame::new()
                    .fill(p.hover)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 2.0;
                        widgets::checkbox(ui, &mut self.st.export_mask, "Mask serial numbers and names");
                        ui.add(
                            egui::Label::new(
                                RichText::new(if self.st.export_mask {
                                    "Serial numbers, the computer and user name and network addresses are partly hidden. Recommended when the report leaves your hands."
                                } else {
                                    "The report will contain full serial numbers, names and addresses. Keep it private."
                                })
                                .size(12.5)
                                .color(if self.st.export_mask { p.muted } else { p.warn }),
                            )
                            .wrap(),
                        );
                    });
                ui.add_space(4.0);
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 34.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        if widgets::primary_button(ui, "Save...", true).clicked() {
                            save = true;
                        }
                        if widgets::ghost_button(ui, "Cancel").clicked() {
                            cancel = true;
                        }
                    },
                );
            });
        if save {
            self.save_report();
        } else if cancel || modal.should_close() {
            self.st.export_open = false;
        }
    }

    /// Asks where to save, then writes the report.
    fn save_report(&mut self) {
        if self.st.screenshot {
            // A screenshot run never opens a native dialog.
            return;
        }
        let format = self.st.export_format;
        let now = util::now_unix();
        let (label, extension) = match format {
            Format::Html => ("HTML page", "html"),
            Format::Text => ("Text file", "txt"),
            Format::Json => ("JSON file", "json"),
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save the report")
            .set_file_name(report_file_name(now, format))
            .add_filter(label, &[extension])
            .save_file()
        else {
            return;
        };
        let (inventory, slow, sample) = self.monitor.snapshot();
        let data = ReportData {
            inventory: &inventory,
            slow: &slow,
            sample: &sample,
            generated: now,
            fahrenheit: self.settings.fahrenheit,
        };
        let text = report::render(&data, format, self.st.export_mask);
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.st.export_open = false;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.toast(format!("Report saved as {name}."), false);
            }
            Err(e) => self.toast(format!("Could not save the report: {e}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(interval_label(500), "0.5 s");
        assert_eq!(interval_label(1000), "1 s");
        assert_eq!(interval_label(10_000), "10 s");
        assert_eq!(log_interval_label(1), "1 s");
        assert_eq!(log_interval_label(15), "15 s");
        assert_eq!(log_interval_label(60), "1 min");
        assert_eq!(log_interval_label(300), "5 min");
        assert_eq!(log_interval_label(90), "90 s");
    }

    #[test]
    fn report_names_carry_the_format() {
        let name = report_file_name(1_794_839_840, Format::Json);
        assert!(name.starts_with("PulseForge-report-2026"), "{name}");
        assert!(name.ends_with(".json"));
        assert!(report_file_name(0, Format::Html).ends_with(".html"));
        assert!(report_file_name(0, Format::Text).ends_with(".txt"));
        // Safe as a file name.
        assert!(!name.contains([':', '/', '\\', ' ']));
    }
}
