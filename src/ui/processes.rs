//! The Processes page: a sortable, searchable live table and the one action
//! in the program that changes anything on the computer, ending a task,
//! which always asks first.

use super::app::App;
use super::pages::{self, col, col_right, Cell, Col};
use super::theme::{pal, pal_ctx, regular, semibold, Icon};
use super::widgets;
use crate::model::ProcessSample;
use crate::util;
use eframe::egui::{
    self, vec2, Align, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui,
};

/// The columns of the process table, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessColumn {
    Name,
    Pid,
    Cpu,
    Memory,
    DiskRead,
    DiskWrite,
    Gpu,
    Network,
    Threads,
}

impl ProcessColumn {
    pub const ALL: [ProcessColumn; 9] = [
        ProcessColumn::Name,
        ProcessColumn::Pid,
        ProcessColumn::Cpu,
        ProcessColumn::Memory,
        ProcessColumn::DiskRead,
        ProcessColumn::DiskWrite,
        ProcessColumn::Gpu,
        ProcessColumn::Network,
        ProcessColumn::Threads,
    ];

    /// Numbers start with the biggest first; names start at A.
    pub fn default_descending(self) -> bool {
        !matches!(self, ProcessColumn::Name | ProcessColumn::Pid)
    }
}

const COLUMNS: [Col; 9] = [
    col("Name", 1.0),
    col_right("PID", 58.0),
    col_right("Processor", 78.0),
    col_right("Memory", 84.0),
    col_right("Disk read", 86.0),
    col_right("Disk write", 86.0),
    col_right("Graphics", 72.0),
    col_right("Network", 66.0),
    col_right("Threads", 62.0),
];

fn finite(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// The processes whose name contains `search` (ignoring case) or whose id
/// starts with it, sorted by `column`. Ties are broken by name, then by id,
/// so that rows do not jump around between readings.
pub fn filter_sort<'a>(
    list: &'a [ProcessSample],
    search: &str,
    column: ProcessColumn,
    descending: bool,
) -> Vec<&'a ProcessSample> {
    let needle = search.trim().to_lowercase();
    let mut rows: Vec<&ProcessSample> = list
        .iter()
        .filter(|q| {
            needle.is_empty()
                || q.name.to_lowercase().contains(&needle)
                || q.pid.to_string().starts_with(&needle)
        })
        .collect();
    let number = |q: &ProcessSample| -> f64 {
        match column {
            ProcessColumn::Name | ProcessColumn::Network => 0.0,
            ProcessColumn::Pid => f64::from(q.pid),
            ProcessColumn::Cpu => finite(f64::from(q.cpu_pct)),
            ProcessColumn::Memory => q.memory as f64,
            ProcessColumn::DiskRead => finite(q.disk_read_bps),
            ProcessColumn::DiskWrite => finite(q.disk_write_bps),
            // Processes without a graphics reading sort below those with zero.
            ProcessColumn::Gpu => q.gpu_pct.map_or(-1.0, |g| finite(f64::from(g))),
            ProcessColumn::Threads => f64::from(q.threads),
        }
    };
    rows.sort_by(|a, b| {
        let by_name = || {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.pid.cmp(&b.pid))
        };
        let primary = match column {
            ProcessColumn::Name | ProcessColumn::Network => by_name(),
            _ => number(a).total_cmp(&number(b)),
        };
        let primary = if descending {
            primary.reverse()
        } else {
            primary
        };
        primary.then_with(by_name)
    });
    rows
}

/// A per-process rate: quiet processes show a dash instead of "0 B/s".
fn rate(bytes_per_s: f64) -> String {
    if bytes_per_s >= 512.0 {
        util::fmt_rate(bytes_per_s)
    } else {
        "–".to_string()
    }
}

impl App {
    pub(super) fn processes_ui(&mut self, ui: &mut Ui) {
        let s = self.data.sample.clone();
        let p = pal(ui);
        let rows = filter_sort(
            &s.processes,
            &self.st.process_search,
            self.st.process_sort,
            self.st.process_descending,
        );
        // The selection follows the process, not the row; it goes away with it.
        if let Some(pid) = self.st.process_selected {
            if !s.processes.is_empty() && !s.processes.iter().any(|q| q.pid == pid) {
                self.st.process_selected = None;
            }
        }
        let selected = self
            .st
            .process_selected
            .and_then(|pid| s.processes.iter().find(|q| q.pid == pid));
        let subtitle = if s.processes.is_empty() {
            "Reading the process list...".to_string()
        } else if rows.len() == s.processes.len() {
            format!("{} processes", s.processes.len())
        } else {
            format!("{} of {} processes", rows.len(), s.processes.len())
        };
        let mut end = None;
        App::page_header(ui, "Processes", &subtitle, |ui| {
            let label = Icon::CLOSE.with("End task");
            let resp = widgets::danger_button(ui, &label, selected.is_some());
            if selected.is_none() {
                resp.on_disabled_hover_text("Select a process first.");
            } else if resp.clicked() {
                end = selected.map(|q| (q.pid, q.name.clone()));
            }
            ui.add_space(6.0);
            let search = egui::TextEdit::singleline(&mut self.st.process_search)
                .hint_text(Icon::SEARCH.with("Search by name or PID"))
                .desired_width(240.0)
                .margin(Margin::symmetric(10, 7));
            ui.add(search);
        });
        if end.is_some() {
            self.st.end_task = end;
        }

        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height() - 4.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                let sorted = ProcessColumn::ALL
                    .iter()
                    .position(|c| *c == self.st.process_sort)
                    .unwrap_or(2);
                if let Some(i) = pages::table_header(ui, &COLUMNS, Some((sorted, self.st.process_descending))) {
                    let column = ProcessColumn::ALL[i];
                    if column == ProcessColumn::Network {
                        // Nothing to sort by: the column is explained instead.
                    } else if column == self.st.process_sort {
                        self.st.process_descending = !self.st.process_descending;
                    } else {
                        self.st.process_sort = column;
                        self.st.process_descending = column.default_descending();
                    }
                }
                if s.processes.is_empty() {
                    widgets::empty_state(
                        ui,
                        Icon::PROCESSES,
                        p.muted,
                        "Reading the process list",
                        "Processes are collected while this window is visible. The first list appears with the next reading.",
                    );
                    return;
                }
                if rows.is_empty() {
                    widgets::empty_state(
                        ui,
                        Icon::SEARCH,
                        p.muted,
                        "No process matches",
                        "Nothing running has that name or process ID.",
                    );
                    return;
                }
                let row_h = 30.0;
                let mut clicked = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, rows.len(), |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for q in &rows[range] {
                            let idle = q.pid == 0;
                            let cells = [
                                Cell::new(q.name.clone()).strong(),
                                Cell::new(q.pid.to_string()).color(p.muted),
                                Cell::new(format!("{:.1}%", q.cpu_pct)).color(if q.cpu_pct >= 0.05 && !idle {
                                    p.text
                                } else {
                                    p.faint
                                }),
                                Cell::new(if idle { "–".into() } else { util::fmt_bytes(q.memory) }),
                                Cell::new(rate(q.disk_read_bps)).color(p.muted),
                                Cell::new(rate(q.disk_write_bps)).color(p.muted),
                                match q.gpu_pct {
                                    Some(g) => Cell::new(format!("{g:.1}%")).color(if g >= 0.05 {
                                        p.text
                                    } else {
                                        p.faint
                                    }),
                                    None => Cell::new("–").color(p.faint),
                                },
                                match q.net_bps {
                                    Some(v) => Cell::new(util::fmt_rate(v)),
                                    None => Cell::new("n/a").color(p.faint),
                                },
                                Cell::new(q.threads.to_string()).color(p.muted),
                            ];
                            let is_selected = self.st.process_selected == Some(q.pid);
                            let resp = pages::table_row(ui, &COLUMNS, &cells, row_h, is_selected, Sense::click());
                            if q.net_bps.is_none() && resp.hovered() {
                                let spans = pages::column_spans(resp.rect.left(), resp.rect.width(), &COLUMNS);
                                let (x, w) = spans[7];
                                let cell = egui::Rect::from_min_size(egui::pos2(x, resp.rect.top()), vec2(w, row_h));
                                ui.interact(cell, resp.id.with("net"), Sense::hover()).on_hover_text(
                                    "Network use per process cannot be measured without a driver, so PulseForge does not show a number here.",
                                );
                            }
                            if resp.clicked() {
                                clicked = Some(q.pid);
                            }
                        }
                    });
                if let Some(pid) = clicked {
                    self.st.process_selected = if self.st.process_selected == Some(pid) {
                        None
                    } else {
                        Some(pid)
                    };
                }
            });
    }

    /// The confirmation before a task is ended. Nothing happens to any
    /// process unless the red button in this dialog is clicked.
    pub(super) fn end_task_dialog(&mut self, ctx: &egui::Context) {
        let Some((pid, name)) = self.st.end_task.clone() else {
            return;
        };
        let p = pal_ctx(ctx);
        let mut confirm = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("end-task"))
            .frame(
                Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(Icon::WARNING.glyph()).size(20.0).color(p.danger));
                    ui.label(RichText::new("End this task?").font(semibold(19.0)));
                });
                Frame::new()
                    .fill(p.hover)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add(egui::Label::new(RichText::new(&name).font(semibold(15.0))).truncate());
                        ui.label(RichText::new(format!("Process ID {pid}")).font(regular(13.0)).color(p.muted));
                    });
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "The program closes at once. Anything in it that has not been saved is lost, and ending a system process can make Windows unstable.",
                        )
                        .color(p.muted),
                    )
                    .wrap(),
                );
                if self.demo {
                    ui.label(
                        RichText::new("Demo mode: confirming ends nothing.")
                            .size(12.5)
                            .color(p.faint),
                    );
                }
                ui.add_space(2.0);
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 34.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        if widgets::danger_button(ui, "End task", true).clicked() {
                            confirm = true;
                        }
                        if widgets::ghost_button(ui, "Cancel").clicked() {
                            cancel = true;
                        }
                    },
                );
            });
        if confirm {
            self.st.end_task = None;
            if self.demo {
                self.toast("Demo mode: nothing was ended.", false);
            } else {
                // The only place a process is ever ended: after the confirmed click.
                match crate::collect::process::end_task(pid) {
                    Ok(()) => {
                        self.st.process_selected = None;
                        self.toast(format!("Ended {name} (process {pid})."), false);
                    }
                    Err(e) => self.toast(format!("Could not end {name}: {e}"), true),
                }
            }
        } else if cancel || modal.should_close() {
            self.st.end_task = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, name: &str, cpu: f32, memory: u64) -> ProcessSample {
        ProcessSample {
            pid,
            name: name.into(),
            cpu_pct: cpu,
            memory,
            ..Default::default()
        }
    }

    fn sample() -> Vec<ProcessSample> {
        let mut list = vec![
            proc(40, "Web Browser", 6.5, 600),
            proc(8, "code editor", 6.5, 900),
            proc(300, "Build Tool", 41.0, 300),
            proc(4, "System", 0.4, 10),
            proc(77, "Web Browser", 1.0, 200),
        ];
        list[0].gpu_pct = Some(0.0);
        list[2].gpu_pct = Some(12.0);
        list[1].disk_read_bps = f64::NAN;
        list
    }

    fn pids(rows: &[&ProcessSample]) -> Vec<u32> {
        rows.iter().map(|q| q.pid).collect()
    }

    #[test]
    fn sorting_is_stable_between_readings() {
        let list = sample();
        // Equal processor use: name decides, whatever the direction.
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Cpu, true)),
            [300, 8, 40, 77, 4]
        );
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Cpu, false)),
            [4, 77, 8, 40, 300]
        );
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Memory, true)),
            [8, 40, 300, 77, 4]
        );
        // Names ignore case; equal names fall back to the process ID.
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Name, false)),
            [300, 8, 4, 40, 77]
        );
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Name, true)),
            [77, 40, 4, 8, 300]
        );
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Pid, false)),
            [4, 8, 40, 77, 300]
        );
        // No graphics reading sorts below a reading of zero.
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Gpu, true))[..2],
            [300, 40]
        );
        // Not-a-number rates do not upset the order.
        assert_eq!(
            filter_sort(&list, "", ProcessColumn::DiskRead, true).len(),
            5
        );
        // The network column has nothing to sort by: it keeps the name order.
        assert_eq!(
            pids(&filter_sort(&list, "", ProcessColumn::Network, false)),
            [300, 8, 4, 40, 77]
        );
    }

    #[test]
    fn searching_matches_names_and_ids() {
        let list = sample();
        assert_eq!(
            pids(&filter_sort(&list, "  BROW ", ProcessColumn::Pid, false)),
            [40, 77]
        );
        assert_eq!(
            pids(&filter_sort(&list, "4", ProcessColumn::Pid, false)),
            [4, 40]
        );
        assert_eq!(
            pids(&filter_sort(&list, "30", ProcessColumn::Pid, false)),
            [300]
        );
        assert!(filter_sort(&list, "nothing like this", ProcessColumn::Cpu, true).is_empty());
        assert!(filter_sort(&[], "", ProcessColumn::Cpu, true).is_empty());
    }

    #[test]
    fn columns_line_up_with_their_headers() {
        assert_eq!(ProcessColumn::ALL.len(), COLUMNS.len());
        assert!(ProcessColumn::Cpu.default_descending());
        assert!(!ProcessColumn::Name.default_descending());
        assert_eq!(rate(0.0), "–");
        assert_eq!(rate(2048.0), "2.00 KB/s");
    }
}
