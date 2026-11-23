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

/// One row per program name: the processes that share a name (ignoring
/// case) added up. The row keeps the lowest process id of its members; the
/// second value says how many processes it stands for.
pub fn group_by_name(list: &[ProcessSample]) -> Vec<(ProcessSample, u32)> {
    let mut order: Vec<(ProcessSample, u32)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for q in list {
        let key = q.name.to_lowercase();
        match index.get(&key) {
            None => {
                index.insert(key, order.len());
                order.push((q.clone(), 1));
            }
            Some(&i) => {
                let (g, count) = &mut order[i];
                *count += 1;
                if q.pid < g.pid {
                    g.pid = q.pid;
                    g.parent_pid = q.parent_pid;
                }
                g.cpu_pct += q.cpu_pct;
                g.memory = g.memory.saturating_add(q.memory);
                g.disk_read_bps += finite(q.disk_read_bps);
                g.disk_write_bps += finite(q.disk_write_bps);
                g.gpu_pct = match (g.gpu_pct, q.gpu_pct) {
                    (Some(a), Some(b)) => Some(a + b),
                    (a, b) => a.or(b),
                };
                g.gpu_memory = match (g.gpu_memory, q.gpu_memory) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    (a, b) => a.or(b),
                };
                g.net_bps = match (g.net_bps, q.net_bps) {
                    (Some(a), Some(b)) => Some(a + b),
                    (a, b) => a.or(b),
                };
                g.threads = g.threads.saturating_add(q.threads);
                g.handles = g.handles.saturating_add(q.handles);
                g.running_s = g.running_s.max(q.running_s);
                if g.user != q.user {
                    g.user.clear();
                }
            }
        }
    }
    for (g, _) in &mut order {
        g.disk_read_bps = finite(g.disk_read_bps);
        g.disk_write_bps = finite(g.disk_write_bps);
    }
    order
}

/// The facts shown under the table for the selected row.
pub fn detail_facts(
    q: &ProcessSample,
    count: u32,
    parent_name: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if count > 1 {
        out.push(("Processes", count.to_string()));
    } else {
        out.push((
            "Started by",
            match parent_name {
                Some(name) if q.parent_pid != 0 => format!("{name} ({})", q.parent_pid),
                _ if q.parent_pid != 0 => format!("Process {} (ended)", q.parent_pid),
                _ => String::new(),
            },
        ));
    }
    out.push(("User", q.user.clone()));
    out.push((
        if count > 1 {
            "Oldest running for"
        } else {
            "Running for"
        },
        if q.running_s > 0 {
            util::fmt_duration(q.running_s)
        } else {
            String::new()
        },
    ));
    out.push(("Handles", util::fmt_count(u64::from(q.handles))));
    out.push((
        "Graphics memory",
        q.gpu_memory.map(util::fmt_bytes).unwrap_or_default(),
    ));
    out
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
        let grouped = self.settings.group_processes;
        // Grouped: one summed row per program name, and how many it stands for.
        let (merged, counts): (Vec<ProcessSample>, std::collections::HashMap<u32, u32>) = if grouped
        {
            let groups = group_by_name(&s.processes);
            let counts = groups.iter().map(|(g, n)| (g.pid, *n)).collect();
            (groups.into_iter().map(|(g, _)| g).collect(), counts)
        } else {
            Default::default()
        };
        let list: &[ProcessSample] = if grouped { &merged } else { &s.processes };
        let count_of = |pid: u32| counts.get(&pid).copied().unwrap_or(1);
        let rows = filter_sort(
            list,
            &self.st.process_search,
            self.st.process_sort,
            self.st.process_descending,
        );
        // The selection follows the process, not the row; it goes away with it.
        if let Some(pid) = self.st.process_selected {
            if !s.processes.is_empty() && !list.iter().any(|q| q.pid == pid) {
                self.st.process_selected = None;
            }
        }
        let selected = self
            .st
            .process_selected
            .and_then(|pid| list.iter().find(|q| q.pid == pid));
        let selected_count = selected.map_or(1, |q| count_of(q.pid));
        let subtitle = if s.processes.is_empty() {
            "Reading the process list...".to_string()
        } else if grouped {
            format!("{} programs, {} processes", rows.len(), s.processes.len())
        } else if rows.len() == s.processes.len() {
            format!("{} processes", s.processes.len())
        } else {
            format!("{} of {} processes", rows.len(), s.processes.len())
        };
        let mut end = None;
        let mut group = grouped;
        let focus_search = std::mem::take(&mut self.st.focus_search);
        App::page_header(ui, "Processes", &subtitle, |ui| {
            let label = Icon::CLOSE.with("End task");
            let can_end = selected.is_some() && selected_count == 1;
            let resp = widgets::danger_button(ui, &label, can_end);
            if selected.is_none() {
                resp.on_disabled_hover_text("Select a process first.");
            } else if selected_count > 1 {
                resp.on_disabled_hover_text(format!(
                    "This row stands for {selected_count} processes. Untick \"Group by name\" to end one of them."
                ));
            } else if resp.clicked() {
                end = selected.map(|q| (q.pid, q.name.clone()));
            }
            ui.add_space(6.0);
            let search = egui::TextEdit::singleline(&mut self.st.process_search)
                .hint_text(Icon::SEARCH.with("Search by name or PID"))
                .desired_width(220.0)
                .margin(Margin::symmetric(10, 7));
            let search = ui.add(search).on_hover_text("Ctrl+F");
            if focus_search {
                search.request_focus();
            }
            ui.add_space(6.0);
            widgets::checkbox(ui, &mut group, "Group by name")
                .on_hover_text("One row per program, with the use of all its processes added up.");
        });
        if group != grouped {
            self.settings.group_processes = group;
            // A group and a process are different things: start afresh.
            self.st.process_selected = None;
        }
        if end.is_some() {
            self.st.end_task = end;
        }
        let details = selected.map(|q| {
            let parent = s
                .processes
                .iter()
                .find(|x| x.pid == q.parent_pid && q.parent_pid != 0)
                .map(|x| x.name.as_str());
            detail_facts(q, selected_count, parent)
        });

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
                // The selected row's details sit under the table.
                const DETAILS_H: f32 = 62.0;
                let table_h = if details.is_some() {
                    (ui.available_height() - DETAILS_H).max(row_h * 3.0)
                } else {
                    ui.available_height()
                };
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(table_h)
                    .show_rows(ui, row_h, rows.len(), |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for q in &rows[range] {
                            let idle = q.pid == 0;
                            let members = count_of(q.pid);
                            let cells = [
                                Cell::new(q.name.clone()).strong(),
                                if members > 1 {
                                    Cell::new(format!("\u{d7}{members}")).color(p.muted)
                                } else {
                                    Cell::new(q.pid.to_string()).color(p.muted)
                                },
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
                if let Some(facts) = &details {
                    ui.add_space(6.0);
                    widgets::divider(ui);
                    ui.add_space(8.0);
                    pages::facts(ui, facts.len(), facts);
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
    fn grouping_adds_up_processes_with_the_same_name() {
        let mut list = sample();
        list[0].threads = 10;
        list[4].threads = 5;
        list[0].user = "ada".into();
        list[4].user = "ada".into();
        list[0].running_s = 50;
        list[4].running_s = 900;
        list[4].gpu_memory = Some(64);
        list.push(proc(900, "WEB BROWSER", 0.5, 100));
        list[5].user = "SYSTEM".into();
        let groups = group_by_name(&list);
        // Five names stay four rows, in the order they first appear.
        assert_eq!(groups.len(), 4);
        let (web, count) = &groups[0];
        assert_eq!(*count, 3);
        assert_eq!(web.name, "Web Browser");
        assert_eq!(web.pid, 40);
        assert!((web.cpu_pct - 8.0).abs() < 1e-4);
        assert_eq!(web.memory, 900);
        assert_eq!(web.threads, 15);
        assert_eq!(web.running_s, 900);
        // One member had a graphics reading: the sum is of those that have one.
        assert_eq!(web.gpu_pct, Some(0.0));
        assert_eq!(web.gpu_memory, Some(64));
        // Different owners: no single user to name.
        assert_eq!(web.user, "");
        // Single processes pass through unchanged, bad numbers are cleaned.
        assert_eq!(groups[1].1, 1);
        assert_eq!(groups[1].0.pid, 8);
        assert_eq!(groups[1].0.disk_read_bps, 0.0);
        // Grouped rows sort and filter like any others.
        let merged: Vec<ProcessSample> = groups.into_iter().map(|(g, _)| g).collect();
        assert_eq!(
            pids(&filter_sort(&merged, "", ProcessColumn::Memory, true)),
            [8, 40, 300, 4]
        );
        assert!(group_by_name(&[]).is_empty());
    }

    #[test]
    fn the_demo_has_programs_with_several_processes() {
        let mut source = crate::demo::DemoSource::new();
        let sample = source.sample(0.0, 0, true);
        let groups = group_by_name(&sample.processes);
        assert!(groups.len() < sample.processes.len());
        assert!(groups
            .iter()
            .any(|(g, n)| g.name == "Web Browser" && *n >= 4));
        // Grouping loses nothing: the processor shares still add up.
        let before: f32 = sample.processes.iter().map(|q| q.cpu_pct).sum();
        let after: f32 = groups.iter().map(|(g, _)| g.cpu_pct).sum();
        assert!((before - after).abs() < 0.01);
    }

    #[test]
    fn details_of_the_selected_row() {
        let mut q = proc(40, "Web Browser", 1.0, 100);
        q.parent_pid = 8;
        q.user = "ada".into();
        q.running_s = 3700;
        q.handles = 1234;
        let facts = detail_facts(&q, 1, Some("code editor"));
        assert_eq!(facts[0], ("Started by", "code editor (8)".to_string()));
        assert_eq!(facts[1], ("User", "ada".to_string()));
        assert_eq!(facts[2].0, "Running for");
        assert!(!facts[2].1.is_empty());
        assert_eq!(facts[3], ("Handles", "1,234".to_string()));
        // No graphics reading: left empty, which the page shows as not available.
        assert_eq!(facts[4], ("Graphics memory", String::new()));
        // The parent has gone; a group says how many processes it stands for.
        assert_eq!(detail_facts(&q, 1, None)[0].1, "Process 8 (ended)");
        assert_eq!(detail_facts(&q, 3, None)[0], ("Processes", "3".to_string()));
        assert_eq!(detail_facts(&q, 3, None)[2].0, "Oldest running for");
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
