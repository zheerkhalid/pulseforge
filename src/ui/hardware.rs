//! The Processor, Memory, Graphics and Battery pages.

use super::app::App;
use super::charts::{self, Unit};
use super::pages::{self, col, col_right, Cell};
use super::theme::{pal, regular, semibold, Icon};
use super::widgets;
use crate::history::keys;
use crate::model::{BatteryState, ProcessSample, NO_DRIVER};
use crate::util;
use eframe::egui::{
    pos2, vec2, Align2, Color32, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};

/// What the "top processes" tables rank by.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TopBy {
    Cpu,
    Memory,
    Gpu,
}

/// The `count` heaviest processes by the chosen measure, heaviest first.
/// The idle pseudo-process (id 0) is never listed.
pub(super) fn top_processes(
    list: &[ProcessSample],
    by: TopBy,
    count: usize,
) -> Vec<&ProcessSample> {
    let key = |q: &ProcessSample| -> f64 {
        let v = match by {
            TopBy::Cpu => f64::from(q.cpu_pct),
            TopBy::Memory => q.memory as f64,
            TopBy::Gpu => f64::from(q.gpu_pct.unwrap_or(0.0)),
        };
        if v.is_finite() {
            v
        } else {
            0.0
        }
    };
    let mut v: Vec<&ProcessSample> = list
        .iter()
        .filter(|q| q.pid != 0 && (by != TopBy::Gpu || q.gpu_pct.is_some()))
        .collect();
    v.sort_by(|a, b| key(b).total_cmp(&key(a)).then_with(|| a.name.cmp(&b.name)));
    v.truncate(count);
    v
}

/// A big number with a caption beside it.
fn headline(ui: &mut Ui, value: &str, caption: &str) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(RichText::new(value).font(semibold(30.0)));
        ui.vertical(|ui| {
            ui.add_space(15.0);
            ui.add(eframe::egui::Label::new(RichText::new(caption).color(p.muted)).truncate());
        });
    });
}

/// A labelled usage bar: "Dedicated memory   1.2 GB of 8.0 GB".
fn labelled_meter(ui: &mut Ui, label: &str, text: &str, fraction: f32, color: Color32) {
    let p = pal(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    widgets::text_in(ui, rect, label, regular(12.5), p.muted);
    widgets::text_in_right(ui, rect, text, regular(12.5), p.text);
    ui.add_space(-4.0);
    charts::meter(ui, fraction, color, width, 8.0);
}

impl App {
    /// A compact table of the heaviest processes.
    pub(super) fn top_table(&self, ui: &mut Ui, title: &str, by: TopBy, count: usize) {
        let p = pal(ui);
        let series = charts::series_ui(ui);
        widgets::section(ui, title);
        let list = top_processes(&self.data.sample.processes, by, count);
        if list.is_empty() {
            pages::note(
                ui,
                match by {
                    TopBy::Gpu => "No process is using the graphics adapters right now, or Windows does not report it.",
                    _ => "The process list is being read. It is only collected while this window is visible.",
                },
            );
            return;
        }
        let (cols, rows): (Vec<pages::Col>, Vec<Vec<Cell>>) = match by {
            TopBy::Cpu => (
                vec![
                    col("Process", 1.0),
                    col_right("PID", 52.0),
                    col("Processor", 150.0),
                ],
                list.iter()
                    .map(|q| {
                        vec![
                            Cell::new(q.name.clone()),
                            Cell::new(q.pid.to_string()).color(p.muted),
                            Cell::new(format!("{:.1}%", q.cpu_pct))
                                .bar(q.cpu_pct / 100.0, series.cpu),
                        ]
                    })
                    .collect(),
            ),
            TopBy::Memory => {
                let total = self.data.sample.memory.total.max(1) as f32;
                (
                    vec![
                        col("Process", 1.0),
                        col_right("PID", 52.0),
                        col("Memory", 170.0),
                    ],
                    list.iter()
                        .map(|q| {
                            vec![
                                Cell::new(q.name.clone()),
                                Cell::new(q.pid.to_string()).color(p.muted),
                                Cell::new(util::fmt_bytes(q.memory))
                                    .bar(q.memory as f32 / total * 4.0, series.memory),
                            ]
                        })
                        .collect(),
                )
            }
            TopBy::Gpu => (
                vec![
                    col("Process", 1.0),
                    col_right("PID", 52.0),
                    col("Graphics", 150.0),
                    col_right("Graphics memory", 120.0),
                ],
                list.iter()
                    .map(|q| {
                        let g = q.gpu_pct.unwrap_or(0.0);
                        vec![
                            Cell::new(q.name.clone()),
                            Cell::new(q.pid.to_string()).color(p.muted),
                            Cell::new(format!("{g:.1}%")).bar(g / 100.0, series.gpu),
                            match q.gpu_memory {
                                Some(m) => Cell::new(util::fmt_bytes(m)),
                                None => Cell::new("n/a").color(p.faint),
                            },
                        ]
                    })
                    .collect(),
            ),
        };
        pages::table(ui, &cols, &rows, 30.0);
    }

    // ------------------------------------------------------------------------------
    // Processor
    // ------------------------------------------------------------------------------

    pub(super) fn cpu_ui(&mut self, ui: &mut Ui) {
        let inv = self.data.inventory.clone();
        let s = self.data.sample.clone();
        let cpu = &inv.cpu;
        let subtitle = if cpu.name.is_empty() {
            "Reading hardware...".to_string()
        } else {
            cpu.name.clone()
        };
        App::page_header(ui, "Processor", &subtitle, |_| {});
        let hist = self.hist();
        let time = self.time_axis();
        let series = charts::series_ui(ui);
        let f = self.settings.fahrenheit;
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            if !self.data.inventory_ready {
                pages::loading(ui, "Model, caches and features appear in a moment.");
            }
            let mut left_height = 0.0_f32;
            pages::card_row(ui, &[1.0, 1.3], &mut |ui, i| {
                if i == 0 {
                    headline(
                        ui,
                        &util::fmt_pct(s.cpu.total_pct),
                        &match s.cpu.current_mhz {
                            Some(mhz) => format!("in use at {}", util::fmt_mhz(mhz)),
                            None => "in use".to_string(),
                        },
                    );
                    let cores = match (cpu.performance_cores, cpu.efficiency_cores) {
                        (Some(perf), Some(eff)) if eff > 0 => {
                            format!("{} ({perf} fast + {eff} efficient)", cpu.cores)
                        }
                        _ if cpu.cores > 0 => cpu.cores.to_string(),
                        _ => String::new(),
                    };
                    let mhz =
                        |v: Option<u32>| v.map(|m| util::fmt_mhz(m as f32)).unwrap_or_default();
                    let number = |n: u32| if n > 0 { n.to_string() } else { String::new() };
                    let mut items: Vec<(&str, String)> = vec![
                        ("Vendor", cpu.vendor.clone()),
                        ("Cores", cores),
                        ("Threads", number(cpu.threads)),
                        ("Socket", cpu.socket.clone()),
                        ("Base clock", mhz(cpu.base_mhz)),
                        ("Maximum clock", mhz(cpu.max_mhz)),
                        ("Architecture", cpu.architecture.clone()),
                        (
                            "Family / model / stepping",
                            if cpu.family + cpu.model + cpu.stepping > 0 {
                                format!("{} / {} / {}", cpu.family, cpu.model, cpu.stepping)
                            } else {
                                String::new()
                            },
                        ),
                        (
                            "Virtualisation",
                            match cpu.virtualization {
                                Some(true) => "Supported and enabled".to_string(),
                                Some(false) => "Not enabled".to_string(),
                                None => String::new(),
                            },
                        ),
                        (
                            "Hypervisor",
                            if cpu.hypervisor {
                                "Windows runs under one".to_string()
                            } else {
                                "None detected".to_string()
                            },
                        ),
                        ("Microcode", cpu.microcode.clone()),
                        ("Packages", number(cpu.packages)),
                    ];
                    for (label, value) in &cpu.extra {
                        items.push((label.as_str(), value.clone()));
                    }
                    pages::facts(ui, 2, &items);
                    left_height = ui.min_rect().height();
                } else {
                    // The graph takes the height the facts beside it leave.
                    let plot = (left_height - charts::chrome_height(2) - 92.0).clamp(150.0, 420.0);
                    charts::chart(
                        ui,
                        "cpu-total",
                        "Usage",
                        Unit::Percent,
                        &[
                            hist.line("Total", series.cpu, keys::CPU_TOTAL),
                            hist.line("Kernel", series.second, keys::CPU_KERNEL),
                        ],
                        plot,
                        &time,
                        "",
                    );
                    let mut live: Vec<(&str, String)> = Vec::new();
                    if s.cpu.processes > 0 {
                        live.push(("Processes", util::fmt_count(u64::from(s.cpu.processes))));
                        live.push(("Threads", util::fmt_count(u64::from(s.cpu.threads))));
                        live.push(("Handles", util::fmt_count(u64::from(s.cpu.handles))));
                    }
                    if let Some(v) = s.cpu.context_switches_per_s {
                        live.push((
                            "Context switches",
                            format!("{} /s", util::fmt_count(v as u64)),
                        ));
                    }
                    if let Some(v) = s.cpu.interrupts_per_s {
                        live.push(("Interrupts", format!("{} /s", util::fmt_count(v as u64))));
                    }
                    live.push(("Uptime", util::fmt_duration(s.uptime_s)));
                    pages::facts(ui, 3, &live);
                }
            });

            if !s.cpu.per_core_pct.is_empty() {
                self.core_strip(ui, &hist, "Logical processors", 62.0);
            }

            pages::card_row(ui, &[1.0, 1.0], &mut |ui, i| {
                if i == 0 {
                    let line = hist.line("Clock", series.cpu, keys::CPU_MHZ);
                    let missing = line.points.is_empty();
                    charts::chart(
                        ui,
                        "cpu-mhz",
                        "Clock speed",
                        Unit::Frequency,
                        &[line],
                        120.0,
                        &time,
                        "Windows does not report the current clock speed here",
                    );
                    if missing {
                        pages::unavailable(
                            ui,
                            "Current clock speed",
                            "Windows does not report it on this computer",
                        );
                    }
                } else {
                    let line = hist.line("Temperature", series.cpu, keys::CPU_TEMP);
                    let missing = line.points.is_empty();
                    let reason = if s.cpu.temperature_note.is_empty() {
                        NO_DRIVER
                    } else {
                        s.cpu.temperature_note.as_str()
                    };
                    charts::chart(
                        ui,
                        "cpu-temp",
                        "Temperature",
                        Unit::Temperature,
                        &[line],
                        120.0,
                        &time,
                        "Not available without a driver",
                    );
                    if missing || s.cpu.temperature_c.is_none() {
                        pages::unavailable(ui, "Processor temperature", reason);
                    } else {
                        pages::note(
                            ui,
                            &format!(
                                "Now {}. Source: {}.",
                                s.cpu
                                    .temperature_c
                                    .map(|t| util::fmt_temp(t, f))
                                    .unwrap_or_default(),
                                reason
                            ),
                        );
                    }
                }
            });

            pages::card_row(ui, &[1.0, 1.0], &mut |ui, i| {
                if i == 0 {
                    widgets::section(ui, "Caches");
                    if cpu.caches.is_empty() {
                        pages::note(ui, "Cache details are not available.");
                    } else {
                        let rows: Vec<Vec<Cell>> = cpu
                            .caches
                            .iter()
                            .map(|c| {
                                vec![
                                    Cell::new(format!("L{}", c.level)).strong(),
                                    Cell::new(c.kind.clone()),
                                    Cell::new(if c.count > 1 {
                                        format!("{} x {}", c.count, util::fmt_bytes(c.size_bytes))
                                    } else {
                                        util::fmt_bytes(c.size_bytes)
                                    }),
                                    Cell::new(if c.associativity > 0 {
                                        format!("{}-way", c.associativity)
                                    } else {
                                        String::new()
                                    })
                                    .color(p.muted),
                                    Cell::new(if c.line_size > 0 {
                                        format!("{} B", c.line_size)
                                    } else {
                                        String::new()
                                    })
                                    .color(p.muted),
                                ]
                            })
                            .collect();
                        pages::table(
                            ui,
                            &[
                                col("Level", 44.0),
                                col("Kind", 1.0),
                                col("Size", 110.0),
                                col("Ways", 64.0),
                                col_right("Line", 48.0),
                            ],
                            &rows,
                            28.0,
                        );
                    }
                    ui.add_space(6.0);
                    widgets::section(ui, "Instruction sets");
                    if cpu.features.is_empty() {
                        pages::note(ui, "The feature list is not available.");
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                            for feature in &cpu.features {
                                widgets::badge(ui, feature, p.muted);
                            }
                        });
                    }
                } else {
                    self.top_table(ui, "Top processes by processor use", TopBy::Cpu, 9);
                }
            });
        });
    }

    // ------------------------------------------------------------------------------
    // Memory
    // ------------------------------------------------------------------------------

    pub(super) fn memory_ui(&mut self, ui: &mut Ui) {
        let inv = self.data.inventory.clone();
        let s = self.data.sample.clone();
        let m = s.memory.clone();
        let populated: Vec<_> = inv.memory_modules.iter().filter(|x| x.populated).collect();
        let subtitle = if m.total == 0 {
            "Reading memory...".to_string()
        } else if populated.is_empty() {
            format!("{} installed", util::fmt_bytes(m.total))
        } else {
            format!(
                "{} usable · {} {} in {} of {} slots",
                util::fmt_bytes(m.total),
                populated.len(),
                if populated.len() == 1 {
                    "module"
                } else {
                    "modules"
                },
                populated.len(),
                inv.memory_modules.len()
            )
        };
        App::page_header(ui, "Memory", &subtitle, |_| {});
        let hist = self.hist();
        let time = self.time_axis();
        let series = charts::series_ui(ui);
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            let free_color = if p.dark {
                p.border
            } else {
                p.faint.gamma_multiply(0.55)
            };
            pages::card_row(ui, &[1.0, 1.2], &mut |ui, i| {
                if i == 0 {
                    if m.total == 0 {
                        pages::note(ui, "No reading yet.");
                        return;
                    }
                    headline(
                        ui,
                        &util::fmt_bytes(m.used),
                        &format!(
                            "in use of {} ({})",
                            util::fmt_bytes(m.total),
                            util::fmt_pct(m.used_pct())
                        ),
                    );
                    // In use / modified / standby / free add up to the total.
                    let modified = m.modified.unwrap_or(0).min(m.used);
                    let standby = m.standby.unwrap_or(m.cached.min(m.available));
                    let free = m.available.saturating_sub(standby);
                    let total = m.total as f32;
                    charts::segmented_bar(
                        ui,
                        &[
                            ((m.used - modified) as f32 / total, series.memory),
                            (modified as f32 / total, series.second),
                            (standby as f32 / total, series.gpu),
                            (free as f32 / total, free_color),
                        ],
                        12.0,
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(14.0, 2.0);
                        charts::legend_item(
                            ui,
                            series.memory,
                            "In use",
                            &util::fmt_bytes(m.used - modified),
                        );
                        if m.modified.is_some() {
                            charts::legend_item(
                                ui,
                                series.second,
                                "Modified",
                                &util::fmt_bytes(modified),
                            );
                        }
                        charts::legend_item(
                            ui,
                            series.gpu,
                            "Standby (cached)",
                            &util::fmt_bytes(standby),
                        );
                        charts::legend_item(ui, free_color, "Free", &util::fmt_bytes(free));
                    });
                    ui.add_space(4.0);
                    let opt = |v: Option<u64>| v.map(util::fmt_bytes).unwrap_or_default();
                    let sys = &inv.system;
                    pages::facts(
                        ui,
                        2,
                        &[
                            (
                                "Committed",
                                format!(
                                    "{} of {} ({})",
                                    util::fmt_bytes(m.commit_used),
                                    util::fmt_bytes(m.commit_limit),
                                    util::fmt_pct(m.commit_pct())
                                ),
                            ),
                            (
                                "Page file",
                                if m.pagefile_total > 0 {
                                    format!(
                                        "{} of {}",
                                        util::fmt_bytes(m.pagefile_used),
                                        util::fmt_bytes(m.pagefile_total)
                                    )
                                } else {
                                    "None".to_string()
                                },
                            ),
                            ("Available", util::fmt_bytes(m.available)),
                            ("Cached", util::fmt_bytes(m.cached)),
                            ("Kernel paged pool", util::fmt_bytes(m.kernel_paged)),
                            ("Kernel non-paged pool", util::fmt_bytes(m.kernel_nonpaged)),
                            ("Compressed", opt(m.compressed)),
                            ("Hardware reserved", opt(m.hardware_reserved)),
                            (
                                "Page faults",
                                m.page_faults_per_s
                                    .map(|v| format!("{} /s", util::fmt_count(v as u64)))
                                    .unwrap_or_default(),
                            ),
                            (
                                "Largest supported",
                                sys.memory_max_bytes
                                    .map(util::fmt_bytes)
                                    .unwrap_or_default(),
                            ),
                        ],
                    );
                } else {
                    charts::chart(
                        ui,
                        "mem-used",
                        "Usage",
                        Unit::Percent,
                        &[
                            hist.line("In use", series.memory, keys::MEM_USED_PCT),
                            hist.line("Committed", series.second, keys::MEM_COMMIT_PCT),
                        ],
                        150.0,
                        &time,
                        "",
                    );
                    ui.add_space(4.0);
                    charts::chart(
                        ui,
                        "mem-cache",
                        "Cache and page file",
                        Unit::Percent,
                        &[
                            hist.line("Cached", series.gpu, keys::MEM_CACHED_PCT),
                            hist.line("Page file", series.second, keys::MEM_PAGEFILE_PCT),
                        ],
                        96.0,
                        &time,
                        "",
                    );
                }
            });

            widgets::card(ui, |ui| {
                widgets::section(ui, "Memory modules");
                if !self.data.inventory_ready {
                    pages::note(ui, "Reading hardware...");
                } else if inv.memory_modules.is_empty() {
                    pages::unavailable(
                        ui,
                        "Module details",
                        "the firmware does not list the memory slots",
                    );
                } else {
                    let rows: Vec<Vec<Cell>> = inv
                        .memory_modules
                        .iter()
                        .map(|x| {
                            if !x.populated {
                                return vec![
                                    Cell::new(x.slot.clone()).strong().sub(x.bank.clone()),
                                    Cell::new("Empty").color(p.faint),
                                    Cell::default(),
                                    Cell::default(),
                                    Cell::default(),
                                    Cell::default(),
                                    Cell::default(),
                                ];
                            }
                            let speed = match (x.configured_mhz, x.speed_mhz) {
                                (Some(c), Some(r)) if c != r => format!("{c} MT/s (rated {r})"),
                                (Some(c), _) => format!("{c} MT/s"),
                                (None, Some(r)) => format!("{r} MT/s"),
                                (None, None) => String::new(),
                            };
                            let or_dash = |v: &str| {
                                if v.trim().is_empty() {
                                    Cell::new("Not reported").color(p.faint)
                                } else {
                                    Cell::new(v.trim())
                                }
                            };
                            vec![
                                Cell::new(x.slot.clone()).strong().sub(x.bank.clone()),
                                Cell::new(util::fmt_bytes(x.size_bytes)),
                                or_dash(&[x.kind.as_str(), x.form_factor.as_str()].join(" ")),
                                or_dash(&speed),
                                or_dash(&x.manufacturer),
                                or_dash(&x.part_number),
                                or_dash(&self.serial(&x.serial)),
                            ]
                        })
                        .collect();
                    pages::table(
                        ui,
                        &[
                            col("Slot", 120.0),
                            col("Size", 80.0),
                            col("Type", 110.0),
                            col("Speed", 0.9),
                            col("Manufacturer", 0.8),
                            col("Part number", 1.0),
                            col("Serial", 110.0),
                        ],
                        &rows,
                        44.0,
                    );
                }
            });

            widgets::card(ui, |ui| {
                self.top_table(ui, "Top processes by memory", TopBy::Memory, 8);
            });
        });
    }

    // ------------------------------------------------------------------------------
    // Graphics
    // ------------------------------------------------------------------------------

    pub(super) fn gpu_ui(&mut self, ui: &mut Ui) {
        let inv = self.data.inventory.clone();
        let s = self.data.sample.clone();
        let hardware = inv.gpus.iter().filter(|g| !g.software).count();
        let subtitle = if !self.data.inventory_ready {
            "Reading hardware...".to_string()
        } else {
            match hardware {
                0 => "No graphics adapter found".to_string(),
                1 => "1 adapter".to_string(),
                n => format!("{n} adapters"),
            }
        };
        App::page_header(ui, "Graphics", &subtitle, |_| {});
        let hist = self.hist();
        let time = self.time_axis();
        let series = charts::series_ui(ui);
        let f = self.settings.fahrenheit;
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            if !self.data.inventory_ready {
                pages::loading(ui, "Adapters and drivers appear in a moment.");
            } else if inv.gpus.is_empty() {
                widgets::card(ui, |ui| {
                    widgets::empty_state(
                        ui,
                        Icon::GPU,
                        p.muted,
                        "No graphics adapter found",
                        "Windows did not list a graphics adapter.",
                    );
                });
            }
            for (index, info) in inv.gpus.iter().enumerate() {
                let sample = s.gpus.iter().find(|g| g.id == info.id);
                pages::card_row(ui, &[1.0, 1.25], &mut |ui, i| {
                    if i == 0 {
                        ui.horizontal(|ui| {
                            ui.add(
                                eframe::egui::Label::new(
                                    RichText::new(&info.name).font(semibold(16.0)),
                                )
                                .truncate(),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            match info.integrated {
                                Some(true) => {
                                    widgets::badge(ui, "Integrated", p.info);
                                }
                                Some(false) => {
                                    widgets::badge(ui, "Discrete", p.info);
                                }
                                None => {}
                            }
                            if info.software {
                                widgets::badge(ui, "Software renderer", p.muted);
                            }
                            if let Some(g) = sample {
                                widgets::badge(
                                    ui,
                                    &format!("{} in use", util::fmt_pct(g.load_pct)),
                                    p.muted,
                                );
                            }
                        });
                        pages::facts(
                            ui,
                            2,
                            &[
                                ("Vendor", info.vendor.clone()),
                                ("Driver version", info.driver_version.clone()),
                                ("Driver date", info.driver_date.clone()),
                                ("Location", info.location.clone()),
                            ],
                        );
                        if let Some(g) = sample {
                            if info.dedicated_bytes > 0 {
                                let frac = g.dedicated_used as f32 / info.dedicated_bytes as f32;
                                labelled_meter(
                                    ui,
                                    "Dedicated memory",
                                    &format!(
                                        "{} of {}",
                                        util::fmt_bytes(g.dedicated_used),
                                        util::fmt_bytes(info.dedicated_bytes)
                                    ),
                                    frac,
                                    pages::usage_color(&p, series.gpu, frac * 100.0),
                                );
                            }
                            if info.shared_bytes > 0 {
                                labelled_meter(
                                    ui,
                                    "Shared memory",
                                    &format!(
                                        "{} of {}",
                                        util::fmt_bytes(g.shared_used),
                                        util::fmt_bytes(info.shared_bytes)
                                    ),
                                    g.shared_used as f32 / info.shared_bytes as f32,
                                    series.gpu,
                                );
                            }
                            ui.add_space(2.0);
                            let mut sensors: Vec<(&str, String)> = Vec::new();
                            if let Some(t) = g.temperature_c {
                                sensors.push(("Temperature", util::fmt_temp(t, f)));
                            }
                            if let Some(w) = g.power_w {
                                sensors.push(("Power", format!("{w:.0} W")));
                            }
                            match (g.fan_rpm, g.fan_pct) {
                                (Some(rpm), Some(pct)) => sensors.push((
                                    "Fan",
                                    format!("{} RPM ({pct:.0}%)", util::fmt_count(u64::from(rpm))),
                                )),
                                (Some(rpm), None) => sensors.push((
                                    "Fan",
                                    format!("{} RPM", util::fmt_count(u64::from(rpm))),
                                )),
                                (None, Some(pct)) => sensors.push(("Fan", format!("{pct:.0}%"))),
                                (None, None) => {}
                            }
                            if let Some(c) = g.clock_mhz {
                                sensors.push(("Core clock", util::fmt_mhz(c)));
                            }
                            for (label, value) in &info.extra {
                                sensors.push((label.as_str(), value.clone()));
                            }
                            if !sensors.is_empty() {
                                pages::facts(ui, 2, &sensors);
                            }
                            if g.temperature_c.is_none() {
                                pages::unavailable(
                                    ui,
                                    "Temperature, fan and power",
                                    if g.temperature_note.is_empty() {
                                        NO_DRIVER
                                    } else {
                                        &g.temperature_note
                                    },
                                );
                            }
                        } else {
                            pages::note(ui, "No live readings for this adapter yet.");
                        }
                    } else {
                        let short = super::dashboard::gpu_short(Some(info), index);
                        charts::chart(
                            ui,
                            ("gpu-load", index),
                            "Load and memory",
                            Unit::Percent,
                            &[
                                hist.line("Load", series.gpu, &keys::gpu_load(&info.id)),
                                hist.line("Memory", series.second, &keys::gpu_mem_pct(&info.id)),
                            ],
                            124.0,
                            &time,
                            "",
                        );
                        let temp = hist.line(&short, series.gpu, &keys::gpu_temp(&info.id));
                        if !temp.points.is_empty() {
                            ui.add_space(4.0);
                            charts::chart(
                                ui,
                                ("gpu-temp", index),
                                "Temperature",
                                Unit::Temperature,
                                &[temp],
                                84.0,
                                &time,
                                "",
                            );
                        }
                        if let Some(g) = sample.filter(|g| !g.engines.is_empty()) {
                            ui.add_space(4.0);
                            widgets::section(ui, "Engines");
                            let width = ui.available_width();
                            let columns = if width > 420.0 { 2 } else { 1 };
                            let cell = (width - 20.0 * (columns as f32 - 1.0)) / columns as f32;
                            for row in g.engines.chunks(columns) {
                                let (rect, _) =
                                    ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
                                for (c, engine) in row.iter().enumerate() {
                                    let r = Rect::from_min_size(
                                        rect.min + vec2(c as f32 * (cell + 20.0), 0.0),
                                        vec2(cell, 22.0),
                                    );
                                    let name = Rect::from_min_size(
                                        r.min,
                                        vec2(112.0_f32.min(cell * 0.45), 22.0),
                                    );
                                    widgets::text_in(
                                        ui,
                                        name,
                                        &engine.name,
                                        regular(12.5),
                                        p.muted,
                                    );
                                    let bar = Rect::from_min_max(
                                        pos2(name.right() + 8.0, r.center().y - 3.0),
                                        pos2(r.right() - 44.0, r.center().y + 3.0),
                                    );
                                    charts::paint_meter(ui, bar, engine.pct / 100.0, series.gpu);
                                    widgets::text_in_right(
                                        ui,
                                        Rect::from_min_max(pos2(r.right() - 40.0, r.top()), r.max),
                                        &util::fmt_pct(engine.pct),
                                        regular(12.5),
                                        p.text,
                                    );
                                }
                            }
                        }
                    }
                });
            }
            if !inv.gpus.is_empty() {
                widgets::card(ui, |ui| {
                    self.top_table(ui, "Processes using graphics", TopBy::Gpu, 8);
                });
            }
        });
    }

    // ------------------------------------------------------------------------------
    // Battery
    // ------------------------------------------------------------------------------

    pub(super) fn battery_ui(&mut self, ui: &mut Ui) {
        let s = self.data.sample.clone();
        let Some(b) = s.battery.as_ref() else {
            App::page_header(ui, "Battery", "No battery", |_| {});
            let p = pal(ui);
            widgets::empty_state(
                ui,
                Icon::BATTERY,
                p.muted,
                "This computer has no battery",
                "Nothing to show here: Windows reports no battery.",
            );
            return;
        };
        let subtitle = [b.manufacturer.as_str(), b.name.as_str()]
            .iter()
            .filter(|x| !x.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        App::page_header(ui, "Battery", &subtitle, |_| {});
        let hist = self.hist();
        let time = self.time_axis();
        let series = charts::series_ui(ui);
        let f = self.settings.fahrenheit;
        pages::page_scroll(ui, |ui| {
            let p = pal(ui);
            pages::card_row(ui, &[1.0, 1.25], &mut |ui, i| {
                if i == 0 {
                    // Battery outline with the charge, the percentage beside it.
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
                    let body = Rect::from_min_size(rect.min + vec2(1.0, 8.0), vec2(96.0, 46.0));
                    let level_color = if b.percent < 15.0 {
                        p.danger
                    } else if b.percent < 30.0 {
                        p.warn
                    } else {
                        series.battery
                    };
                    ui.painter().rect_stroke(
                        body,
                        CornerRadius::same(9),
                        Stroke::new(2.0_f32, p.faint),
                        StrokeKind::Inside,
                    );
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            pos2(body.right() + 2.0, body.center().y - 8.0),
                            vec2(5.0, 16.0),
                        ),
                        CornerRadius::same(2),
                        p.faint,
                    );
                    let inner = body.shrink(6.0);
                    let fill = Rect::from_min_size(
                        inner.min,
                        vec2((inner.width() * b.percent / 100.0).max(4.0), inner.height()),
                    );
                    ui.painter()
                        .rect_filled(fill, CornerRadius::same(4), level_color);
                    ui.painter().text(
                        pos2(body.right() + 24.0, rect.top() + 4.0),
                        Align2::LEFT_TOP,
                        util::fmt_pct(b.percent),
                        semibold(34.0),
                        p.text,
                    );
                    let mut state = b.state.label().to_string();
                    if let Some(left) = b.seconds_left {
                        state.push_str(&format!(
                            " · {} {}",
                            util::fmt_duration(u64::from(left) / 60 * 60),
                            if b.state == BatteryState::Charging {
                                "until full"
                            } else {
                                "left"
                            }
                        ));
                    }
                    widgets::text_in(
                        ui,
                        Rect::from_min_max(
                            pos2(body.right() + 26.0, rect.bottom() - 20.0),
                            rect.max,
                        ),
                        &state,
                        regular(13.5),
                        p.muted,
                    );
                    ui.add_space(2.0);
                    if let (Some(design), Some(full)) = (b.design_mwh, b.full_mwh) {
                        let wh = |mwh: u32| format!("{:.1} Wh", f64::from(mwh) / 1000.0);
                        let mut text = format!("{} of {} design", wh(full), wh(design));
                        if let Some(w) = b.wear_pct {
                            text.push_str(&format!(" · {w:.0}% wear"));
                        }
                        let frac = full as f32 / design.max(1) as f32;
                        labelled_meter(
                            ui,
                            "Full charge capacity",
                            &text,
                            frac,
                            if frac < 0.6 { p.warn } else { series.battery },
                        );
                        ui.add_space(4.0);
                    }
                    let wh = |mwh: Option<u32>| {
                        mwh.map(|m| format!("{:.1} Wh", f64::from(m) / 1000.0))
                            .unwrap_or_default()
                    };
                    let mut items: Vec<(&str, String)> = vec![
                        (
                            match b.rate_mw {
                                Some(mw) if mw > 0 => "Charge rate",
                                Some(mw) if mw < 0 => "Discharge rate",
                                _ => "Rate",
                            },
                            match b.rate_mw {
                                Some(0) => "Not charging or discharging".to_string(),
                                Some(mw) => format!("{:.1} W", f64::from(mw.abs()) / 1000.0),
                                None => String::new(),
                            },
                        ),
                        (
                            "Voltage",
                            b.voltage_mv
                                .map(|mv| format!("{:.2} V", f64::from(mv) / 1000.0))
                                .unwrap_or_default(),
                        ),
                        ("Remaining", wh(b.remaining_mwh)),
                        (
                            "Power source",
                            if b.ac_online {
                                "Mains".into()
                            } else {
                                "Battery".into()
                            },
                        ),
                        (
                            "Charge cycles",
                            b.cycle_count.map(|c| c.to_string()).unwrap_or_default(),
                        ),
                        (
                            "Wear",
                            b.wear_pct.map(|w| format!("{w:.1}%")).unwrap_or_default(),
                        ),
                        ("Chemistry", b.chemistry.clone()),
                        ("Manufacturer", b.manufacturer.clone()),
                        ("Serial number", self.serial(&b.serial)),
                        ("Manufactured", b.manufacture_date.clone()),
                        (
                            "Temperature",
                            b.temperature_c
                                .map(|t| util::fmt_temp(t, f))
                                .unwrap_or_default(),
                        ),
                    ];
                    for (label, value) in &b.extra {
                        items.push((label.as_str(), value.clone()));
                    }
                    pages::facts(ui, 2, &items);
                } else {
                    charts::chart(
                        ui,
                        "battery-pct",
                        "Charge",
                        Unit::Percent,
                        &[hist.line("Charge", series.battery, keys::BATTERY_PCT)],
                        150.0,
                        &time,
                        "",
                    );
                    ui.add_space(4.0);
                    charts::chart(
                        ui,
                        "battery-rate",
                        "Charge (+) and discharge (-) rate",
                        Unit::Watts,
                        &[hist.line("Rate", series.battery, keys::BATTERY_RATE_W)],
                        120.0,
                        &time,
                        "The battery does not report its rate",
                    );
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, name: &str, cpu: f32, memory: u64, gpu: Option<f32>) -> ProcessSample {
        ProcessSample {
            pid,
            name: name.into(),
            cpu_pct: cpu,
            memory,
            gpu_pct: gpu,
            ..Default::default()
        }
    }

    #[test]
    fn top_lists_rank_and_skip_the_idle_process() {
        let list = vec![
            proc(0, "System Idle Process", 90.0, 0, None),
            proc(10, "b", 2.0, 500, Some(1.0)),
            proc(11, "a", 2.0, 900, None),
            proc(12, "c", 7.5, 100, Some(40.0)),
            proc(13, "d", f32::NAN, 50, None),
        ];
        let names = |by, n| -> Vec<String> {
            top_processes(&list, by, n)
                .iter()
                .map(|q| q.name.clone())
                .collect()
        };
        assert_eq!(names(TopBy::Cpu, 3), ["c", "a", "b"]);
        assert_eq!(names(TopBy::Memory, 2), ["a", "b"]);
        // Only processes with a graphics reading are ranked by graphics use.
        assert_eq!(names(TopBy::Gpu, 9), ["c", "b"]);
        assert!(top_processes(&[], TopBy::Cpu, 5).is_empty());
    }
}
