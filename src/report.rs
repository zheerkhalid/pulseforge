//! The exported system report (HTML, plain text or JSON) and the masking of
//! serial numbers and names for privacy.

use crate::model::{
    BatteryInfo, DriveInfo, Extra, HealthStatus, Inventory, Sample, SensorKind, SensorReading,
    Slow, SmartAttribute,
};
use crate::util::{
    fmt_bits, fmt_bytes, fmt_count, fmt_duration, fmt_hours, fmt_mhz, fmt_pct, fmt_rate, fmt_temp,
    local_datetime,
};

/// Everything a report is built from.
#[derive(Clone, Copy)]
pub struct ReportData<'a> {
    pub inventory: &'a Inventory,
    pub slow: &'a Slow,
    pub sample: &'a Sample,
    /// Unix time the report is made.
    pub generated: u64,
    pub fahrenheit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Html,
    Text,
    Json,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Html => "html",
            Format::Text => "txt",
            Format::Json => "json",
        }
    }
}

const GENERATOR: &str = "PulseForge";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BULLET: char = '\u{2022}';
/// Processes included in the JSON report.
const JSON_PROCESSES: usize = 50;
/// Processes listed in the HTML and text reports.
const LISTED_PROCESSES: usize = 25;
const MASK_NOTE: &str =
    "Serial numbers, the computer name, user names and network addresses are masked in this report.";

// ----------------------------------------------------------------------------------
// Masking
// ----------------------------------------------------------------------------------

/// Hides most of a serial number: keeps the last four characters.
/// Also used by the user interface when "mask serial numbers" is on.
pub fn mask_serial(serial: &str) -> String {
    let count = serial.chars().count();
    if count <= 4 {
        return BULLET.to_string().repeat(count);
    }
    let mut out = BULLET.to_string().repeat(count - 4);
    out.extend(serial.chars().skip(count - 4));
    out
}

/// Hides a value completely (names and addresses give too much away even in
/// part). The length is hidden as well; empty stays empty.
fn hide(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        BULLET.to_string().repeat(6)
    }
}

/// True when the label of an extra row suggests that its value identifies
/// the machine or its user.
fn label_is_identifying(label: &str) -> bool {
    const PARTS: [&str; 13] = [
        "serial",
        "uuid",
        "guid",
        "address",
        "asset",
        "computer name",
        "name of user",
        "user name",
        "product key",
        "licen",
        "identifier",
        "owner",
        "ssid",
    ];
    const WORDS: [&str; 9] = [
        "mac", "user", "username", "host", "hostname", "id", "imei", "login", "account",
    ];
    let lower = label.to_lowercase();
    PARTS.iter().any(|p| lower.contains(p))
        || lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| WORDS.contains(&word))
}

fn mask_extra(extra: &mut Extra, names: &[String]) {
    for (label, value) in extra.iter_mut() {
        if label_is_identifying(label) {
            *value = mask_serial(value);
        } else {
            scrub(value, names);
        }
    }
}

/// Removes the computer and user name from free text (a network adapter or
/// a volume may be named after its owner).
fn scrub(text: &mut String, names: &[String]) {
    for name in names {
        if text.contains(name.as_str()) {
            *text = text.replace(name.as_str(), &hide(name));
        }
    }
}

/// Copies of the data with serial numbers, the computer name, the user name,
/// network addresses and similar identifying values masked.
pub fn masked(data: &ReportData) -> (Inventory, Slow, Sample) {
    let mut inventory = data.inventory.clone();
    let mut slow = data.slow.clone();
    let mut sample = data.sample.clone();

    // Names worth removing from free text. Very short ones would hit
    // ordinary words.
    let names: Vec<String> = [&inventory.system.computer_name, &inventory.system.user_name]
        .iter()
        .filter(|n| n.chars().count() >= 4)
        .map(|n| n.to_string())
        .collect();

    let system = &mut inventory.system;
    system.system_serial = mask_serial(&system.system_serial);
    system.board_serial = mask_serial(&system.board_serial);
    system.uuid = mask_serial(&system.uuid);
    system.computer_name = hide(&system.computer_name);
    system.user_name = hide(&system.user_name);
    mask_extra(&mut system.extra, &names);
    mask_extra(&mut inventory.cpu.extra, &names);
    for module in &mut inventory.memory_modules {
        module.serial = mask_serial(&module.serial);
        mask_extra(&mut module.extra, &names);
    }
    for gpu in &mut inventory.gpus {
        mask_extra(&mut gpu.extra, &names);
    }
    for display in &mut inventory.displays {
        scrub(&mut display.name, &names);
        mask_extra(&mut display.extra, &names);
    }

    for drive in &mut slow.drives {
        drive.serial = mask_serial(&drive.serial);
        mask_extra(&mut drive.extra, &names);
    }
    for volume in &mut slow.volumes {
        scrub(&mut volume.label, &names);
    }

    if let Some(battery) = &mut sample.battery {
        battery.serial = mask_serial(&battery.serial);
        mask_extra(&mut battery.extra, &names);
    }
    for net in &mut sample.net {
        net.mac = hide(&net.mac);
        net.gateway = hide(&net.gateway);
        for address in net
            .ipv4
            .iter_mut()
            .chain(net.ipv6.iter_mut())
            .chain(net.dns.iter_mut())
        {
            *address = hide(address);
        }
        scrub(&mut net.name, &names);
        scrub(&mut net.description, &names);
        mask_extra(&mut net.extra, &names);
    }
    for process in &mut sample.processes {
        process.user = hide(&process.user);
    }
    for sensor in &mut sample.sensors {
        scrub(&mut sensor.group, &names);
        scrub(&mut sensor.name, &names);
    }
    (inventory, slow, sample)
}

// ----------------------------------------------------------------------------------
// The report as sections of labelled values and tables
// ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Cell {
    text: String,
    badge: Option<HealthStatus>,
    /// A usage bar, 0..=100.
    bar: Option<f32>,
}

impl Cell {
    fn text(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            ..Default::default()
        }
    }
    fn badge(status: HealthStatus) -> Cell {
        Cell {
            text: status.label().to_string(),
            badge: Some(status),
            bar: None,
        }
    }
}

enum Block {
    Heading(String),
    Pairs(Vec<(String, Cell)>),
    Table {
        head: Vec<&'static str>,
        rows: Vec<Vec<Cell>>,
    },
    Note(String),
}

struct Section {
    id: &'static str,
    title: &'static str,
    blocks: Vec<Block>,
}

/// One tile of the summary at the top.
struct Tile {
    label: String,
    value: String,
    detail: String,
    badge: Option<HealthStatus>,
}

/// A list of label / value rows; empty values are left out.
#[derive(Default)]
struct Pairs(Vec<(String, Cell)>);

impl Pairs {
    fn add(&mut self, label: &str, value: impl Into<String>) {
        let value = value.into();
        if !value.trim().is_empty() {
            self.0.push((label.to_string(), Cell::text(value)));
        }
    }
    fn opt(&mut self, label: &str, value: Option<String>) {
        if let Some(value) = value {
            self.add(label, value);
        }
    }
    fn cell(&mut self, label: &str, cell: Cell) {
        self.0.push((label.to_string(), cell));
    }
    fn extra(&mut self, extra: &Extra) {
        for (label, value) in extra {
            self.add(label, value.clone());
        }
    }
    fn block(self) -> Option<Block> {
        (!self.0.is_empty()).then_some(Block::Pairs(self.0))
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "Yes"
    } else {
        "No"
    }
}

/// "59.94", "60", "27.5": at most two decimals, no trailing zeros.
fn number(value: f64) -> String {
    if !value.is_finite() {
        return String::new();
    }
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn memory_summary(inventory: &Inventory) -> String {
    let filled: Vec<_> = inventory
        .memory_modules
        .iter()
        .filter(|m| m.populated)
        .collect();
    let Some(first) = filled.first() else {
        return String::new();
    };
    let same = filled
        .iter()
        .all(|m| m.size_bytes == first.size_bytes && m.kind == first.kind);
    if same {
        joined(&[
            &format!("{} \u{d7} {}", filled.len(), fmt_bytes(first.size_bytes)),
            &first.kind,
            &first
                .configured_mhz
                .or(first.speed_mhz)
                .map(|s| format!("at {s} MT/s"))
                .unwrap_or_default(),
        ])
    } else {
        format!("{} modules", filled.len())
    }
}

fn drive_title(drive: &DriveInfo) -> String {
    if drive.model.trim().is_empty() {
        format!("Drive {}", drive.index)
    } else {
        drive.model.clone()
    }
}

fn battery_line(battery: &BatteryInfo) -> String {
    format!(
        "{} \u{b7} {}",
        fmt_pct(battery.percent),
        battery.state.label()
    )
}

fn tiles(inventory: &Inventory, slow: &Slow, sample: &Sample) -> Vec<Tile> {
    let sys = &inventory.system;
    let mut out = Vec::new();
    let mut push = |label: &str, value: String, detail: String, badge: Option<HealthStatus>| {
        if !value.trim().is_empty() {
            out.push(Tile {
                label: label.to_string(),
                value,
                detail,
                badge,
            });
        }
    };
    let mut computer = joined(&[&sys.manufacturer, &sys.product]);
    if computer.is_empty() {
        computer = joined(&[&sys.board_manufacturer, &sys.board_product]);
    }
    push(
        "Computer",
        computer,
        joined(&[&sys.os_name, &sys.os_edition, &sys.os_display_version]),
        None,
    );
    let cpu = &inventory.cpu;
    push(
        "Processor",
        cpu.name.clone(),
        if cpu.cores > 0 {
            format!("{} cores, {} threads", cpu.cores, cpu.threads)
        } else {
            String::new()
        },
        None,
    );
    let total = sample.memory.total;
    push(
        "Memory",
        if total > 0 {
            fmt_bytes(total)
        } else {
            let sum: u64 = inventory.memory_modules.iter().map(|m| m.size_bytes).sum();
            if sum > 0 {
                fmt_bytes(sum)
            } else {
                String::new()
            }
        },
        memory_summary(inventory),
        None,
    );
    for gpu in inventory.gpus.iter().filter(|g| !g.software) {
        push(
            "Graphics",
            gpu.name.clone(),
            if gpu.dedicated_bytes > 0 {
                format!("{} dedicated", fmt_bytes(gpu.dedicated_bytes))
            } else {
                String::new()
            },
            None,
        );
    }
    for drive in &slow.drives {
        push(
            &format!("Drive {}", drive.index),
            drive_title(drive),
            joined(&[
                &fmt_bytes(drive.size_bytes),
                &drive.interface,
                &drive.media,
                &drive
                    .health_pct
                    .map(|p| format!("\u{b7} {p} % life left"))
                    .unwrap_or_default(),
            ]),
            Some(drive.health),
        );
    }
    if let Some(battery) = &sample.battery {
        push(
            "Battery",
            battery_line(battery),
            battery
                .wear_pct
                .map(|w| format!("{} % wear", number(f64::from(w))))
                .unwrap_or_default(),
            None,
        );
    }
    out
}

/// The computer in a few lines, for pasting into a message or a forum post:
/// what it is, not how it is doing. It holds no serial number, computer or
/// user name and no network address, so it needs no masking.
pub fn summary(inventory: &Inventory, slow: &Slow, sample: &Sample) -> String {
    let sys = &inventory.system;
    let mut lines = Vec::new();
    for tile in tiles(inventory, slow, sample) {
        match tile.label.as_str() {
            // The charge changes by the minute: not part of a specification.
            "Battery" => {}
            "Computer" => {
                lines.push(format!("Computer: {}", tile.value));
                let mut windows = tile.detail.clone();
                if !sys.os_build.trim().is_empty() {
                    windows = joined(&[&windows, &format!("(build {})", sys.os_build.trim())]);
                }
                if !windows.trim().is_empty() {
                    lines.push(format!("Windows: {windows}"));
                }
            }
            label => {
                // "Drive 0" and "Drive 1" are both simply storage to a reader.
                let label = if label.starts_with("Drive ") {
                    "Storage"
                } else {
                    label
                };
                let detail = tile.detail.replace("\u{b7} ", "");
                if detail.trim().is_empty() {
                    lines.push(format!("{label}: {}", tile.value));
                } else {
                    lines.push(format!("{label}: {} ({})", tile.value, detail.trim()));
                }
            }
        }
    }
    for d in inventory
        .displays
        .iter()
        .filter(|d| d.width > 0 && d.height > 0)
    {
        let mut line = format!("Display: {} x {}", d.width, d.height);
        if d.refresh_hz > 0.0 {
            line.push_str(&format!(", {} Hz", number(f64::from(d.refresh_hz))));
        }
        if let Some(inches) = d.diagonal_inches.filter(|i| *i > 0.0) {
            line.push_str(&format!(", {inches:.1} inch"));
        }
        lines.push(line);
    }
    lines.join("\r\n")
}

fn sensor_value(sensor: &SensorReading, fahrenheit: bool) -> String {
    match sensor.value {
        None => "not available".to_string(),
        Some(v) if sensor.kind == SensorKind::Temperature => fmt_temp(v as f32, fahrenheit),
        Some(v) => joined(&[&number(v), sensor.kind.unit()]),
    }
}

/// The attribute value as shown; temperatures follow the chosen unit.
fn attribute_display(attribute: &SmartAttribute, fahrenheit: bool) -> String {
    let is_temperature = attribute.display.ends_with("\u{b0}C")
        && attribute.status != HealthStatus::Unknown
        && attribute.raw <= 200;
    if fahrenheit && is_temperature {
        fmt_temp(attribute.raw as f32, true)
    } else {
        attribute.display.clone()
    }
}

fn processor_section(inventory: &Inventory, sample: &Sample, fahrenheit: bool) -> Section {
    let cpu = &inventory.cpu;
    let mut blocks = Vec::new();
    let mut p = Pairs::default();
    p.add("Name", cpu.name.clone());
    p.add("Vendor", cpu.vendor.clone());
    p.add("Architecture", cpu.architecture.clone());
    if cpu.cores > 0 {
        p.add("Sockets", cpu.packages.max(1).to_string());
        p.add("Cores", cpu.cores.to_string());
        p.add("Threads", cpu.threads.to_string());
    }
    p.opt(
        "Performance cores",
        cpu.performance_cores.map(|n| n.to_string()),
    );
    p.opt(
        "Efficiency cores",
        cpu.efficiency_cores.map(|n| n.to_string()),
    );
    p.opt("Base speed", cpu.base_mhz.map(|m| fmt_mhz(m as f32)));
    p.opt("Maximum speed", cpu.max_mhz.map(|m| fmt_mhz(m as f32)));
    if cpu.family > 0 || cpu.model > 0 {
        p.add(
            "Family / model / stepping",
            format!("{} / {} / {}", cpu.family, cpu.model, cpu.stepping),
        );
    }
    p.add("Socket", cpu.socket.clone());
    p.add("Microcode", cpu.microcode.clone());
    p.opt(
        "Hardware virtualisation",
        cpu.virtualization
            .map(|v| if v { "Supported" } else { "Not supported" }.to_string()),
    );
    if !cpu.name.is_empty() {
        p.add("Running under a hypervisor", yes_no(cpu.hypervisor));
    }
    p.add("Instruction sets", cpu.features.join(", "));
    p.extra(&cpu.extra);
    blocks.extend(p.block());

    if !cpu.caches.is_empty() {
        blocks.push(Block::Heading("Caches".into()));
        blocks.push(Block::Table {
            head: vec!["Level", "Kind", "Size", "Count", "Line size", "Ways"],
            rows: cpu
                .caches
                .iter()
                .map(|c| {
                    vec![
                        Cell::text(format!("L{}", c.level)),
                        Cell::text(c.kind.clone()),
                        Cell::text(fmt_bytes(c.size_bytes)),
                        Cell::text(c.count.to_string()),
                        Cell::text(format!("{} B", c.line_size)),
                        Cell::text(c.associativity.to_string()),
                    ]
                })
                .collect(),
        });
    }

    let s = &sample.cpu;
    let mut p = Pairs::default();
    if sample.time > 0 || s.total_pct > 0.0 {
        p.add("Load", fmt_pct(s.total_pct));
        p.opt("Current speed", s.current_mhz.map(fmt_mhz));
        p.add(
            "Temperature",
            match s.temperature_c {
                Some(t) => joined(&[
                    &fmt_temp(t, fahrenheit),
                    &if s.temperature_note.is_empty() {
                        String::new()
                    } else {
                        format!("({})", s.temperature_note)
                    },
                ]),
                None if s.temperature_note.is_empty() => "not available".to_string(),
                None => format!("not available: {}", s.temperature_note),
            },
        );
        p.add("Processes", fmt_count(u64::from(s.processes)));
        p.add("Threads", fmt_count(u64::from(s.threads)));
        p.add("Handles", fmt_count(u64::from(s.handles)));
    }
    if let Some(block) = p.block() {
        blocks.push(Block::Heading("Right now".into()));
        blocks.push(block);
    }
    Section {
        id: "processor",
        title: "Processor",
        blocks,
    }
}

fn memory_section(inventory: &Inventory, sample: &Sample) -> Section {
    let m = &sample.memory;
    let sys = &inventory.system;
    let mut blocks = Vec::new();
    let mut p = Pairs::default();
    if m.total > 0 {
        p.add("Installed (usable)", fmt_bytes(m.total));
        p.cell(
            "In use",
            Cell {
                text: format!("{} ({})", fmt_bytes(m.used), fmt_pct(m.used_pct())),
                badge: None,
                bar: Some(m.used_pct()),
            },
        );
        p.add("Available", fmt_bytes(m.available));
        p.add("Cached", fmt_bytes(m.cached));
        if m.commit_limit > 0 {
            p.add(
                "Committed",
                format!(
                    "{} of {}",
                    fmt_bytes(m.commit_used),
                    fmt_bytes(m.commit_limit)
                ),
            );
        }
        if m.pagefile_total > 0 {
            p.add(
                "Page file",
                format!(
                    "{} of {}",
                    fmt_bytes(m.pagefile_used),
                    fmt_bytes(m.pagefile_total)
                ),
            );
        }
        p.opt("Compressed", m.compressed.map(fmt_bytes));
        p.opt("Hardware reserved", m.hardware_reserved.map(fmt_bytes));
    }
    if let (Some(used), Some(total)) = (sys.memory_slots_used, sys.memory_slots_total) {
        p.add("Slots in use", format!("{used} of {total}"));
    }
    p.opt(
        "Maximum the board supports",
        sys.memory_max_bytes.map(fmt_bytes),
    );
    blocks.extend(p.block());

    if !inventory.memory_modules.is_empty() {
        blocks.push(Block::Heading("Modules".into()));
        blocks.push(Block::Table {
            head: vec![
                "Slot",
                "Size",
                "Type",
                "Form",
                "Rated",
                "Running at",
                "Manufacturer",
                "Part number",
                "Serial",
                "Voltage",
                "Rank",
            ],
            rows: inventory
                .memory_modules
                .iter()
                .map(|module| {
                    let slot = if module.bank.is_empty() || module.bank == module.slot {
                        module.slot.clone()
                    } else {
                        format!("{} ({})", module.slot, module.bank)
                    };
                    if !module.populated {
                        let mut row = vec![Cell::text(slot), Cell::text("empty")];
                        row.resize(11, Cell::default());
                        return row;
                    }
                    let speed = |s: Option<u32>| s.map(|s| format!("{s} MT/s")).unwrap_or_default();
                    vec![
                        Cell::text(slot),
                        Cell::text(if module.size_bytes > 0 {
                            fmt_bytes(module.size_bytes)
                        } else {
                            "unknown".to_string()
                        }),
                        Cell::text(module.kind.clone()),
                        Cell::text(module.form_factor.clone()),
                        Cell::text(speed(module.speed_mhz)),
                        Cell::text(speed(module.configured_mhz)),
                        Cell::text(module.manufacturer.clone()),
                        Cell::text(module.part_number.clone()),
                        Cell::text(module.serial.clone()),
                        Cell::text(
                            module
                                .voltage_mv
                                .map(|v| format!("{} V", number(f64::from(v) / 1000.0)))
                                .unwrap_or_default(),
                        ),
                        Cell::text(module.rank.map(|r| r.to_string()).unwrap_or_default()),
                    ]
                })
                .collect(),
        });
    }
    Section {
        id: "memory",
        title: "Memory",
        blocks,
    }
}

fn graphics_section(inventory: &Inventory, sample: &Sample, fahrenheit: bool) -> Section {
    let mut blocks = Vec::new();
    for gpu in &inventory.gpus {
        blocks.push(Block::Heading(if gpu.name.is_empty() {
            "Graphics adapter".to_string()
        } else {
            gpu.name.clone()
        }));
        let mut p = Pairs::default();
        p.add("Vendor", gpu.vendor.clone());
        if gpu.software {
            p.add("Kind", "Software renderer");
        } else {
            p.opt(
                "Kind",
                gpu.integrated
                    .map(|i| if i { "Integrated" } else { "Dedicated" }.to_string()),
            );
        }
        if gpu.dedicated_bytes > 0 {
            p.add("Dedicated memory", fmt_bytes(gpu.dedicated_bytes));
        }
        if gpu.shared_bytes > 0 {
            p.add("Shared memory", fmt_bytes(gpu.shared_bytes));
        }
        p.add("Driver version", gpu.driver_version.clone());
        p.add("Driver date", gpu.driver_date.clone());
        p.add("Location", gpu.location.clone());
        if let Some(live) = sample.gpus.iter().find(|s| s.id == gpu.id) {
            p.add("Load", fmt_pct(live.load_pct));
            if live.dedicated_used > 0 {
                p.add("Dedicated memory in use", fmt_bytes(live.dedicated_used));
            }
            if live.shared_used > 0 {
                p.add("Shared memory in use", fmt_bytes(live.shared_used));
            }
            p.add(
                "Temperature",
                match live.temperature_c {
                    Some(t) => fmt_temp(t, fahrenheit),
                    None if live.temperature_note.is_empty() => String::new(),
                    None => format!("not available: {}", live.temperature_note),
                },
            );
            p.opt(
                "Power",
                live.power_w.map(|w| format!("{} W", number(f64::from(w)))),
            );
            p.opt("Fan", live.fan_rpm.map(|r| format!("{r} RPM")));
            p.opt("Fan duty", live.fan_pct.map(fmt_pct));
            p.opt("Clock", live.clock_mhz.map(fmt_mhz));
            for engine in live.engines.iter().filter(|e| e.pct >= 0.5) {
                p.add(&format!("Engine: {}", engine.name), fmt_pct(engine.pct));
            }
        }
        p.extra(&gpu.extra);
        blocks.extend(p.block());
    }
    Section {
        id: "graphics",
        title: "Graphics",
        blocks,
    }
}

fn volume_rows(slow: &Slow, drive: Option<u32>) -> Vec<Vec<Cell>> {
    slow.volumes
        .iter()
        .filter(|v| drive.is_none() || v.drive_index == drive)
        .map(|v| {
            vec![
                Cell::text(v.mount.clone()),
                Cell::text(v.label.clone()),
                Cell::text(v.filesystem.clone()),
                Cell::text(v.kind.clone()),
                Cell::text(if v.total > 0 {
                    fmt_bytes(v.total)
                } else {
                    String::new()
                }),
                Cell::text(if v.total > 0 {
                    fmt_bytes(v.free)
                } else {
                    String::new()
                }),
                if v.total > 0 {
                    Cell {
                        text: format!("{} used", fmt_pct(v.used_pct())),
                        badge: None,
                        bar: Some(v.used_pct()),
                    }
                } else {
                    Cell::default()
                },
                Cell::text(match v.bitlocker {
                    Some(true) => "Encrypted",
                    Some(false) => "Not encrypted",
                    None => "",
                }),
                Cell::text(
                    v.drive_index
                        .map(|i| format!("Drive {i}"))
                        .unwrap_or_default(),
                ),
            ]
        })
        .collect()
}

const VOLUME_HEAD: [&str; 9] = [
    "Volume",
    "Label",
    "File system",
    "Kind",
    "Size",
    "Free",
    "Usage",
    "Encryption",
    "On",
];

fn storage_section(slow: &Slow, sample: &Sample, fahrenheit: bool) -> Section {
    let mut blocks = Vec::new();
    for drive in &slow.drives {
        blocks.push(Block::Heading(format!(
            "Drive {}: {}",
            drive.index,
            drive_title(drive)
        )));
        let mut p = Pairs::default();
        p.add("Model", drive.model.clone());
        p.add("Firmware", drive.firmware.clone());
        p.add("Serial number", drive.serial.clone());
        p.add("Connection", drive.interface.clone());
        p.add("Kind", drive.media.clone());
        if drive.size_bytes > 0 {
            p.add("Size", fmt_bytes(drive.size_bytes));
        }
        p.add("Removable", yes_no(drive.removable));
        p.add("Partition table", drive.partition_style.clone());
        p.opt("Rotation", drive.rotation_rpm.map(|r| format!("{r} RPM")));
        p.cell("Health", Cell::badge(drive.health));
        p.opt("Life left", drive.health_pct.map(|h| format!("{h} %")));
        p.add("In plain words", drive.health_summary.clone());
        p.opt(
            "Temperature",
            drive.temperature_c.map(|t| fmt_temp(t, fahrenheit)),
        );
        p.opt(
            "Powered on for",
            drive
                .power_on_hours
                .map(|h| format!("{} hours ({})", fmt_count(h), fmt_hours(h))),
        );
        p.opt("Power cycles", drive.power_cycles.map(fmt_count));
        p.opt("Written in total", drive.total_written_bytes.map(fmt_bytes));
        p.opt("Read in total", drive.total_read_bytes.map(fmt_bytes));
        p.add("Health data from", drive.smart_source.clone());
        p.add("Health details", drive.smart_note.clone());
        if let Some(live) = sample.disks.iter().find(|d| d.index == drive.index) {
            p.add(
                "Activity right now",
                format!(
                    "read {}, write {}, busy {}",
                    fmt_rate(live.read_bps),
                    fmt_rate(live.write_bps),
                    fmt_pct(live.active_pct)
                ),
            );
        }
        p.extra(&drive.extra);
        blocks.extend(p.block());

        if !drive.attributes.is_empty() {
            blocks.push(Block::Table {
                head: vec![
                    "ID",
                    "Attribute",
                    "Value",
                    "Current",
                    "Worst",
                    "Limit",
                    "Status",
                    "What it means",
                ],
                rows: drive
                    .attributes
                    .iter()
                    .map(|a| {
                        let small = |v: Option<u8>| v.map(|v| v.to_string()).unwrap_or_default();
                        vec![
                            Cell::text(small(a.id)),
                            Cell::text(a.name.clone()),
                            Cell::text(attribute_display(a, fahrenheit)),
                            Cell::text(small(a.current)),
                            Cell::text(small(a.worst)),
                            Cell::text(small(a.threshold)),
                            Cell::badge(a.status),
                            Cell::text(a.explanation.clone()),
                        ]
                    })
                    .collect(),
            });
        }
        if !drive.partitions.is_empty() {
            blocks.push(Block::Table {
                head: vec!["Partition", "Type", "Size", "Starts at", "Letter"],
                rows: drive
                    .partitions
                    .iter()
                    .map(|part| {
                        vec![
                            Cell::text(part.number.to_string()),
                            Cell::text(part.kind.clone()),
                            Cell::text(fmt_bytes(part.size)),
                            Cell::text(fmt_bytes(part.offset)),
                            Cell::text(part.letter.clone()),
                        ]
                    })
                    .collect(),
            });
        }
        let rows = volume_rows(slow, Some(drive.index));
        if !rows.is_empty() {
            blocks.push(Block::Table {
                head: VOLUME_HEAD.to_vec(),
                rows,
            });
        }
    }
    // Volumes that are not on a known drive (network shares, optical media).
    let loose: Vec<Vec<Cell>> = slow
        .volumes
        .iter()
        .filter(|v| {
            v.drive_index
                .is_none_or(|i| !slow.drives.iter().any(|d| d.index == i))
        })
        .flat_map(|v| {
            volume_rows(
                &Slow {
                    drives: Vec::new(),
                    volumes: vec![v.clone()],
                },
                None,
            )
        })
        .collect();
    if !loose.is_empty() {
        blocks.push(Block::Heading("Other volumes".into()));
        blocks.push(Block::Table {
            head: VOLUME_HEAD.to_vec(),
            rows: loose,
        });
    }
    Section {
        id: "storage",
        title: "Storage",
        blocks,
    }
}

fn battery_section(sample: &Sample, fahrenheit: bool) -> Section {
    let mut blocks = Vec::new();
    match &sample.battery {
        None => blocks.push(Block::Note("This computer has no battery.".into())),
        Some(b) => {
            let mwh = |v: Option<u32>| v.map(|v| format!("{} mWh", fmt_count(u64::from(v))));
            let mut p = Pairs::default();
            p.add("Name", b.name.clone());
            p.add("Manufacturer", b.manufacturer.clone());
            p.add("Serial number", b.serial.clone());
            p.add("Chemistry", b.chemistry.clone());
            p.cell(
                "Charge",
                Cell {
                    text: fmt_pct(b.percent),
                    badge: None,
                    bar: Some(b.percent),
                },
            );
            p.add("State", b.state.label());
            p.add(
                "Mains power",
                if b.ac_online {
                    "Connected"
                } else {
                    "Not connected"
                },
            );
            p.opt("Design capacity", mwh(b.design_mwh));
            p.opt("Full-charge capacity", mwh(b.full_mwh));
            p.opt("Remaining", mwh(b.remaining_mwh));
            p.opt(
                "Wear",
                b.wear_pct
                    .map(|w| format!("{} % of the design capacity is lost", number(f64::from(w)))),
            );
            p.opt(
                "Rate",
                b.rate_mw.map(|r| {
                    let watts = number(f64::from(r.unsigned_abs()) / 1000.0);
                    match r {
                        0 => "0 W".to_string(),
                        r if r > 0 => format!("{watts} W (charging)"),
                        _ => format!("{watts} W (discharging)"),
                    }
                }),
            );
            p.opt(
                "Voltage",
                b.voltage_mv
                    .map(|v| format!("{} V", number(f64::from(v) / 1000.0))),
            );
            p.opt("Charge cycles", b.cycle_count.map(|c| c.to_string()));
            p.opt(
                "Time left",
                b.seconds_left.map(|s| fmt_duration(u64::from(s))),
            );
            p.opt(
                "Temperature",
                b.temperature_c.map(|t| fmt_temp(t, fahrenheit)),
            );
            p.add("Made on", b.manufacture_date.clone());
            p.extra(&b.extra);
            blocks.extend(p.block());
        }
    }
    Section {
        id: "battery",
        title: "Battery",
        blocks,
    }
}

fn network_section(sample: &Sample) -> Section {
    let mut blocks = Vec::new();
    for net in &sample.net {
        blocks.push(Block::Heading(if net.name.is_empty() {
            "Network adapter".to_string()
        } else {
            net.name.clone()
        }));
        let mut p = Pairs::default();
        p.add("Hardware", net.description.clone());
        p.add("Kind", net.kind.clone());
        p.add("Status", if net.up { "Connected" } else { "Not connected" });
        if net.link_bps > 0 {
            p.add("Link speed", fmt_bits(net.link_bps));
        }
        if net.up {
            p.add("Download", fmt_rate(net.rx_bps));
            p.add("Upload", fmt_rate(net.tx_bps));
        }
        if net.rx_total > 0 || net.tx_total > 0 {
            p.add("Received since start", fmt_bytes(net.rx_total));
            p.add("Sent since start", fmt_bytes(net.tx_total));
        }
        p.add("IPv4 address", net.ipv4.join(", "));
        p.add("IPv6 address", net.ipv6.join(", "));
        p.add("Hardware (MAC) address", net.mac.clone());
        p.add("Gateway", net.gateway.clone());
        p.add("DNS servers", net.dns.join(", "));
        p.extra(&net.extra);
        blocks.extend(p.block());
    }
    Section {
        id: "network",
        title: "Network",
        blocks,
    }
}

fn displays_section(inventory: &Inventory) -> Section {
    let mut blocks = Vec::new();
    for (i, d) in inventory.displays.iter().enumerate() {
        blocks.push(Block::Heading(if d.name.is_empty() {
            format!("Display {}", i + 1)
        } else {
            d.name.clone()
        }));
        let mut p = Pairs::default();
        p.add("Manufacturer id", d.manufacturer.clone());
        p.add("Connected to", d.adapter.clone());
        if d.width > 0 {
            p.add("Resolution", format!("{} \u{d7} {}", d.width, d.height));
        }
        if d.refresh_hz > 0.0 {
            p.add(
                "Refresh rate",
                format!("{} Hz", number(f64::from(d.refresh_hz))),
            );
        }
        if d.bits_per_pixel > 0 {
            p.add(
                "Colour depth",
                format!("{} bits per pixel", d.bits_per_pixel),
            );
        }
        if d.scale_pct > 0 {
            p.add("Scaling", format!("{} %", d.scale_pct));
        }
        p.opt(
            "HDR",
            d.hdr_supported.map(|supported| {
                match (supported, d.hdr_enabled.unwrap_or(false)) {
                    (true, true) => "Supported, switched on",
                    (true, false) => "Supported, switched off",
                    _ => "Not supported",
                }
                .to_string()
            }),
        );
        p.add("Connection", d.connection.clone());
        p.add("Orientation", d.orientation.clone());
        if d.width > 0 {
            p.add("Position", format!("{}, {}", d.position.0, d.position.1));
            p.add("Main display", yes_no(d.primary));
        }
        p.opt(
            "Physical size",
            d.size_mm.map(|(w, h)| {
                joined(&[
                    &format!("{w} \u{d7} {h} mm"),
                    &d.diagonal_inches
                        .map(|i| format!("({} inches)", number(f64::from(i))))
                        .unwrap_or_default(),
                ])
            }),
        );
        p.extra(&d.extra);
        blocks.extend(p.block());
    }
    Section {
        id: "displays",
        title: "Displays",
        blocks,
    }
}

fn system_section(inventory: &Inventory, sample: &Sample) -> Section {
    let s = &inventory.system;
    let mut blocks = Vec::new();
    let mut add = |title: &str, p: Pairs| {
        if let Some(block) = p.block() {
            blocks.push(Block::Heading(title.to_string()));
            blocks.push(block);
        }
    };

    let mut p = Pairs::default();
    p.add("Manufacturer", s.manufacturer.clone());
    p.add("Model", s.product.clone());
    p.add("Family", s.family.clone());
    p.add("SKU", s.sku.clone());
    p.add("Serial number", s.system_serial.clone());
    p.add("UUID", s.uuid.clone());
    p.add("Chassis", s.chassis.clone());
    add("Computer", p);

    let mut p = Pairs::default();
    p.add("Manufacturer", s.board_manufacturer.clone());
    p.add("Model", s.board_product.clone());
    p.add("Version", s.board_version.clone());
    p.add("Serial number", s.board_serial.clone());
    add("Mainboard", p);

    let mut p = Pairs::default();
    p.add("Vendor", s.bios_vendor.clone());
    p.add("Version", s.bios_version.clone());
    p.add("Date", s.bios_date.clone());
    p.add("Mode", s.firmware_mode.clone());
    p.opt(
        "Secure Boot",
        s.secure_boot
            .map(|on| if on { "On" } else { "Off" }.to_string()),
    );
    p.opt(
        "Security chip (TPM)",
        s.tpm_present.map(|present| {
            if !present {
                "Not present".to_string()
            } else if s.tpm_version.is_empty() {
                "Present".to_string()
            } else {
                format!("Present, version {}", s.tpm_version)
            }
        }),
    );
    p.add("TPM manufacturer", s.tpm_manufacturer.clone());
    add("Firmware and security", p);

    let mut p = Pairs::default();
    p.add("Windows", joined(&[&s.os_name, &s.os_edition]));
    p.add("Version", s.os_display_version.clone());
    p.add("Build", s.os_build.clone());
    p.add("Architecture", s.os_arch.clone());
    p.add("Installed on", s.os_install_date.clone());
    p.add("Computer name", s.computer_name.clone());
    p.add("User name", s.user_name.clone());
    p.add("Started", s.boot_time.clone());
    if sample.uptime_s > 0 {
        p.add("Running for", fmt_duration(sample.uptime_s));
    }
    p.add("Time zone", s.time_zone.clone());
    p.add("Language and region", s.locale.clone());
    p.extra(&s.extra);
    add("Windows", p);

    Section {
        id: "system",
        title: "System",
        blocks,
    }
}

fn sensors_section(sample: &Sample, fahrenheit: bool) -> Section {
    let mut blocks = Vec::new();
    if !sample.sensors.is_empty() {
        blocks.push(Block::Table {
            head: vec!["Device", "Sensor", "Kind", "Reading", "Source", "Note"],
            rows: sample
                .sensors
                .iter()
                .map(|s| {
                    vec![
                        Cell::text(s.group.clone()),
                        Cell::text(s.name.clone()),
                        Cell::text(s.kind.label()),
                        Cell::text(sensor_value(s, fahrenheit)),
                        Cell::text(s.source.clone()),
                        Cell::text(s.note.clone()),
                    ]
                })
                .collect(),
        });
        if sample.sensors.iter().any(|s| s.value.is_none()) {
            blocks.push(Block::Note(
                "Readings marked \"not available\" are not offered by Windows on this computer; the note says why. Nothing is estimated.".into(),
            ));
        }
    }
    Section {
        id: "sensors",
        title: "Sensors",
        blocks,
    }
}

/// The processes ordered by processor use, busiest first.
fn top_processes(sample: &Sample, limit: usize) -> Vec<&crate::model::ProcessSample> {
    let mut list: Vec<_> = sample.processes.iter().collect();
    list.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.memory.cmp(&a.memory))
    });
    list.truncate(limit);
    list
}

fn processes_section(sample: &Sample) -> Section {
    let mut blocks = Vec::new();
    if !sample.processes.is_empty() {
        blocks.push(Block::Table {
            head: vec![
                "Process",
                "PID",
                "Processor",
                "Memory",
                "Disk",
                "Threads",
                "Running for",
                "User",
            ],
            rows: top_processes(sample, LISTED_PROCESSES)
                .iter()
                .map(|p| {
                    vec![
                        Cell::text(p.name.clone()),
                        Cell::text(p.pid.to_string()),
                        Cell::text(format!("{:.1}%", p.cpu_pct.clamp(0.0, 100.0))),
                        Cell::text(fmt_bytes(p.memory)),
                        Cell::text(fmt_rate(p.disk_read_bps + p.disk_write_bps)),
                        Cell::text(p.threads.to_string()),
                        Cell::text(fmt_duration(p.running_s)),
                        Cell::text(p.user.clone()),
                    ]
                })
                .collect(),
        });
        if sample.processes.len() > LISTED_PROCESSES {
            blocks.push(Block::Note(format!(
                "The {LISTED_PROCESSES} busiest of {} processes are listed.",
                sample.processes.len()
            )));
        }
    }
    Section {
        id: "processes",
        title: "Top processes",
        blocks,
    }
}

fn sections(inventory: &Inventory, slow: &Slow, sample: &Sample, fahrenheit: bool) -> Vec<Section> {
    let mut all = vec![
        processor_section(inventory, sample, fahrenheit),
        memory_section(inventory, sample),
        graphics_section(inventory, sample, fahrenheit),
        storage_section(slow, sample, fahrenheit),
        battery_section(sample, fahrenheit),
        network_section(sample),
        displays_section(inventory),
        system_section(inventory, sample),
        sensors_section(sample, fahrenheit),
        processes_section(sample),
    ];
    for section in &mut all {
        if section.blocks.is_empty() {
            section.blocks.push(Block::Note(
                "No information was collected for this area.".into(),
            ));
        }
    }
    all
}

// ----------------------------------------------------------------------------------
// Plain text
// ----------------------------------------------------------------------------------

fn width(text: &str) -> usize {
    text.chars().count()
}

fn pad(text: &str, to: usize) -> String {
    let mut out = text.to_string();
    out.extend(std::iter::repeat_n(' ', to.saturating_sub(width(text))));
    out
}

fn render_text(tiles: &[Tile], sections: &[Section], generated: &str, mask: bool) -> String {
    let mut out = String::new();
    let line = |out: &mut String, text: &str| {
        out.push_str(text.trim_end());
        out.push_str("\r\n");
    };
    line(&mut out, &format!("{GENERATOR} system report"));
    line(&mut out, &format!("Generated {generated}"));
    if mask {
        line(&mut out, MASK_NOTE);
    }
    line(&mut out, "");
    line(&mut out, "SUMMARY");
    line(&mut out, "=======");
    let label_width = tiles.iter().map(|t| width(&t.label)).max().unwrap_or(0);
    for tile in tiles {
        let mut value = tile.value.clone();
        if !tile.detail.is_empty() {
            value.push_str(&format!(" ({})", tile.detail));
        }
        if let Some(badge) = tile.badge {
            value.push_str(&format!(" [{}]", badge.label()));
        }
        line(
            &mut out,
            &format!("{}  {}", pad(&tile.label, label_width), value),
        );
    }
    for section in sections {
        line(&mut out, "");
        line(&mut out, &section.title.to_uppercase());
        line(&mut out, &"=".repeat(width(section.title)));
        for block in &section.blocks {
            match block {
                Block::Heading(text) => {
                    line(&mut out, "");
                    line(&mut out, text);
                    line(&mut out, &"-".repeat(width(text).min(78)));
                }
                Block::Note(text) => line(&mut out, text),
                Block::Pairs(pairs) => {
                    let w = pairs.iter().map(|(l, _)| width(l)).max().unwrap_or(0);
                    for (label, cell) in pairs {
                        line(&mut out, &format!("{}  {}", pad(label, w), cell.text));
                    }
                }
                Block::Table { head, rows } => {
                    let columns = head.len();
                    let mut widths: Vec<usize> = head.iter().map(|h| width(h)).collect();
                    for row in rows {
                        for (i, cell) in row.iter().enumerate().take(columns) {
                            widths[i] = widths[i].max(width(&cell.text));
                        }
                    }
                    let print = |out: &mut String, cells: Vec<&str>| {
                        let mut text = String::new();
                        for (i, cell) in cells.iter().enumerate() {
                            if i + 1 == cells.len() {
                                text.push_str(cell);
                            } else {
                                text.push_str(&pad(cell, widths[i] + 2));
                            }
                        }
                        line(out, &text);
                    };
                    line(&mut out, "");
                    print(&mut out, head.to_vec());
                    let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
                    print(&mut out, rule.iter().map(String::as_str).collect());
                    for row in rows {
                        print(
                            &mut out,
                            row.iter().take(columns).map(|c| c.text.as_str()).collect(),
                        );
                    }
                }
            }
        }
    }
    line(&mut out, "");
    line(
        &mut out,
        &format!("Generated by {GENERATOR} {VERSION} \u{b7} Zheer Khalid (zheer.dev)"),
    );
    out
}

// ----------------------------------------------------------------------------------
// HTML
// ----------------------------------------------------------------------------------

/// Escapes text for use in HTML content and attribute values.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

const CSS: &str = "\
:root{--bg:#111218;--card:#191a23;--inset:#14151d;--border:#2b2d3b;--text:#ececf4;--muted:#a2a4b8;\
--faint:#6b6e84;--pink:#ff3d8b;--purple:#7a3cf0;--good:#2fc98a;--warn:#f2a93b;--bad:#ff5a4e}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--text);font:14px/1.5 \"Segoe UI\",system-ui,-apple-system,sans-serif}\
.accent{height:6px;background:linear-gradient(90deg,var(--pink),var(--purple))}\
.wrap{max-width:1120px;margin:0 auto;padding:0 20px 48px}\
header{padding:28px 0 6px}\
h1{margin:0;font-size:28px;line-height:1.2;font-weight:700}\
h1 span{background:linear-gradient(90deg,var(--pink),var(--purple));-webkit-background-clip:text;background-clip:text;color:transparent}\
.sub{color:var(--muted);margin:6px 0 0}\
.note{color:var(--muted);margin:8px 0}\
nav{margin:16px 0 0;display:flex;flex-wrap:wrap;gap:8px}\
nav a{color:var(--text);text-decoration:none;border:1px solid var(--border);border-radius:999px;padding:3px 12px;font-size:13px;background:var(--card)}\
nav a:hover{border-color:var(--pink)}\
.card{background:var(--card);border:1px solid var(--border);border-radius:14px;padding:18px 20px;margin:16px 0}\
h2{margin:0 0 12px;font-size:18px}\
h2::before{content:\"\";display:inline-block;width:10px;height:10px;border-radius:50%;margin-right:10px;background:linear-gradient(135deg,var(--pink),var(--purple))}\
h3{margin:20px 0 8px;font-size:15px;color:var(--text)}\
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:12px}\
.tile{background:var(--inset);border:1px solid var(--border);border-radius:12px;padding:12px 14px;min-width:0}\
.tile .l{color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.07em}\
.tile .v{font-size:15px;font-weight:600;margin:2px 0;overflow-wrap:anywhere}\
.tile .d{color:var(--muted);font-size:13px}\
.scroll{overflow-x:auto}\
table{border-collapse:collapse;width:100%;margin:4px 0 8px}\
th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--border);vertical-align:top}\
th{color:var(--muted);font-weight:600;font-size:12px;white-space:nowrap}\
tr:last-child td{border-bottom:none}\
table.kv td:first-child{color:var(--muted);width:30%;min-width:170px}\
td{overflow-wrap:anywhere}\
.badge{display:inline-block;padding:1px 10px;border-radius:999px;font-size:12px;font-weight:600;white-space:nowrap}\
.good{background:rgba(47,201,138,.16);color:var(--good)}\
.caution{background:rgba(242,169,59,.16);color:var(--warn)}\
.bad{background:rgba(255,90,78,.18);color:var(--bad)}\
.unknown{background:rgba(162,164,184,.14);color:var(--muted)}\
.meter{display:inline-block;width:110px;height:8px;border-radius:4px;background:var(--border);vertical-align:middle;margin-right:8px;overflow:hidden}\
.meter i{display:block;height:100%;background:linear-gradient(90deg,var(--pink),var(--purple))}\
footer{color:var(--faint);text-align:center;margin-top:28px;font-size:13px}\
@media print{:root{--bg:#fff;--card:#fff;--inset:#fff;--border:#ccc;--text:#111;--muted:#444;--faint:#666}nav{display:none}.card{break-inside:avoid}}";

fn badge_html(status: HealthStatus) -> String {
    let class = match status {
        HealthStatus::Good => "good",
        HealthStatus::Caution => "caution",
        HealthStatus::Bad => "bad",
        HealthStatus::Unknown => "unknown",
    };
    format!(
        "<span class=\"badge {class}\">{}</span>",
        esc(status.label())
    )
}

fn cell_html(cell: &Cell) -> String {
    if let Some(status) = cell.badge {
        return badge_html(status);
    }
    let mut out = String::new();
    if let Some(bar) = cell.bar {
        let pct = if bar.is_finite() {
            bar.clamp(0.0, 100.0)
        } else {
            0.0
        };
        out.push_str(&format!(
            "<span class=\"meter\"><i style=\"width:{pct:.0}%\"></i></span>"
        ));
    }
    out.push_str(&esc(&cell.text));
    out
}

fn render_html(tiles: &[Tile], sections: &[Section], generated: &str, mask: bool) -> String {
    let mut h = String::with_capacity(64 * 1024);
    h.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    h.push_str(&format!("<title>{GENERATOR} system report</title>\n"));
    h.push_str("<style>");
    h.push_str(CSS);
    h.push_str("</style>\n</head>\n<body>\n<div class=\"accent\"></div>\n<div class=\"wrap\">\n");
    h.push_str(&format!(
        "<header>\n<h1><span>{GENERATOR}</span> system report</h1>\n<p class=\"sub\">Generated {}</p>\n",
        esc(generated)
    ));
    if mask {
        h.push_str(&format!("<p class=\"note\">{}</p>\n", esc(MASK_NOTE)));
    }
    h.push_str("<nav>");
    for section in sections {
        h.push_str(&format!(
            "<a href=\"#{}\">{}</a>",
            section.id,
            esc(section.title)
        ));
    }
    h.push_str("</nav>\n</header>\n");

    h.push_str("<section class=\"card\" id=\"summary\">\n<h2>Summary</h2>\n<div class=\"grid\">\n");
    if tiles.is_empty() {
        h.push_str("<p class=\"note\">No information was collected.</p>\n");
    }
    for tile in tiles {
        h.push_str(&format!(
            "<div class=\"tile\"><div class=\"l\">{}</div><div class=\"v\">{}</div><div class=\"d\">{}{}</div></div>\n",
            esc(&tile.label),
            esc(&tile.value),
            tile.badge
                .map(|b| format!("{} ", badge_html(b)))
                .unwrap_or_default(),
            esc(&tile.detail),
        ));
    }
    h.push_str("</div>\n</section>\n");

    for section in sections {
        h.push_str(&format!(
            "<section class=\"card\" id=\"{}\">\n<h2>{}</h2>\n",
            section.id,
            esc(section.title)
        ));
        for block in &section.blocks {
            match block {
                Block::Heading(text) => h.push_str(&format!("<h3>{}</h3>\n", esc(text))),
                Block::Note(text) => {
                    h.push_str(&format!("<p class=\"note\">{}</p>\n", esc(text)));
                }
                Block::Pairs(pairs) => {
                    h.push_str("<table class=\"kv\">\n");
                    for (label, cell) in pairs {
                        h.push_str(&format!(
                            "<tr><td>{}</td><td>{}</td></tr>\n",
                            esc(label),
                            cell_html(cell)
                        ));
                    }
                    h.push_str("</table>\n");
                }
                Block::Table { head, rows } => {
                    h.push_str("<div class=\"scroll\"><table>\n<tr>");
                    for name in head {
                        h.push_str(&format!("<th>{}</th>", esc(name)));
                    }
                    h.push_str("</tr>\n");
                    for row in rows {
                        h.push_str("<tr>");
                        for cell in row.iter().take(head.len()) {
                            h.push_str(&format!("<td>{}</td>", cell_html(cell)));
                        }
                        h.push_str("</tr>\n");
                    }
                    h.push_str("</table></div>\n");
                }
            }
        }
        h.push_str("</section>\n");
    }
    h.push_str(&format!(
        "<footer>Generated by {GENERATOR} {} \u{b7} Zheer Khalid (zheer.dev)</footer>\n</div>\n</body>\n</html>\n",
        esc(VERSION)
    ));
    h
}

// ----------------------------------------------------------------------------------
// JSON and the entry point
// ----------------------------------------------------------------------------------

fn render_json(
    inventory: &Inventory,
    slow: &Slow,
    sample: &Sample,
    generated: &str,
    mask: bool,
) -> String {
    let mut sample = sample.clone();
    sample.processes = top_processes(&sample, JSON_PROCESSES)
        .into_iter()
        .cloned()
        .collect();
    use serde_json::{to_value, Map, Value};
    let mut map = Map::new();
    map.insert("generator".into(), Value::from(GENERATOR));
    map.insert("version".into(), Value::from(VERSION));
    map.insert("generated".into(), Value::from(generated));
    map.insert("masked".into(), Value::from(mask));
    map.insert(
        "inventory".into(),
        to_value(inventory).unwrap_or(Value::Null),
    );
    map.insert("slow".into(), to_value(slow).unwrap_or(Value::Null));
    map.insert("sample".into(), to_value(&sample).unwrap_or(Value::Null));
    serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_else(|_| "{}".to_string())
}

/// Renders the report. With `mask` the identifying values are masked first.
pub fn render(data: &ReportData, format: Format, mask: bool) -> String {
    let (inventory, slow, sample) = if mask {
        masked(data)
    } else {
        (
            data.inventory.clone(),
            data.slow.clone(),
            data.sample.clone(),
        )
    };
    let generated = local_datetime(data.generated);
    match format {
        Format::Json => render_json(&inventory, &slow, &sample, &generated, mask),
        Format::Text | Format::Html => {
            let tiles = tiles(&inventory, &slow, &sample);
            let sections = sections(&inventory, &slow, &sample, data.fahrenheit);
            if format == Format::Text {
                render_text(&tiles, &sections, &generated, mask)
            } else {
                render_html(&tiles, &sections, &generated, mask)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    const SYSTEM_SERIAL: &str = "ZX81QQ7788KL";
    const BOARD_SERIAL: &str = "BRD-4471-XYZW";
    const UUID: &str = "00112233-4455-6677-8899-AABBCCDDEEFF";
    const COMPUTER: &str = "ORBITAL-DESK";
    const USER: &str = "marisol.quint";
    const RAM_SERIAL: &str = "RAMSER90210";
    const DRIVE_SERIAL: &str = "DRV55501234ABCD";
    const BATTERY_SERIAL: &str = "BATT-778899";
    const MAC: &str = "A4:5E:60:11:22:33";
    const IPV4: &str = "192.0.2.44";
    const IPV6: &str = "2001:db8::beef:1";
    const GATEWAY: &str = "192.0.2.1";
    const DNS: &str = "198.51.100.53";
    const ASSET: &str = "ASSET-55-KILO";
    const EXTRA_MAC: &str = "0A:1B:2C:3D:4E:5F";

    const SECRETS: [&str; 15] = [
        SYSTEM_SERIAL,
        BOARD_SERIAL,
        UUID,
        COMPUTER,
        USER,
        RAM_SERIAL,
        DRIVE_SERIAL,
        BATTERY_SERIAL,
        MAC,
        IPV4,
        IPV6,
        GATEWAY,
        DNS,
        ASSET,
        EXTRA_MAC,
    ];

    fn fixture() -> (Inventory, Slow, Sample) {
        let inventory = Inventory {
            cpu: CpuInfo {
                name: "Example Core 9 1490X".into(),
                vendor: "Example Semiconductor".into(),
                cores: 8,
                threads: 16,
                base_mhz: Some(3600),
                caches: vec![CacheInfo {
                    level: 1,
                    kind: "Data".into(),
                    size_bytes: 48 << 10,
                    count: 8,
                    line_size: 64,
                    associativity: 12,
                }],
                features: vec!["AVX2".into(), "AES".into()],
                ..Default::default()
            },
            memory_modules: vec![
                MemoryModule {
                    slot: "DIMM A1".into(),
                    populated: true,
                    size_bytes: 16 << 30,
                    kind: "DDR5".into(),
                    speed_mhz: Some(5600),
                    serial: RAM_SERIAL.into(),
                    ..Default::default()
                },
                MemoryModule {
                    slot: "DIMM A2".into(),
                    ..Default::default()
                },
            ],
            gpus: vec![GpuInfo {
                id: "g0".into(),
                name: "Example Graphics RX 990".into(),
                dedicated_bytes: 12 << 30,
                ..Default::default()
            }],
            system: SystemInfo {
                manufacturer: "Contoso Computers".into(),
                product: "Falcon 15".into(),
                system_serial: SYSTEM_SERIAL.into(),
                uuid: UUID.into(),
                board_serial: BOARD_SERIAL.into(),
                os_name: "Windows 11".into(),
                os_edition: "Pro".into(),
                computer_name: COMPUTER.into(),
                user_name: USER.into(),
                secure_boot: Some(true),
                tpm_present: Some(true),
                tpm_version: "2.0".into(),
                extra: vec![
                    ("Asset tag".into(), ASSET.into()),
                    ("Chassis colour".into(), "Graphite".into()),
                ],
                ..Default::default()
            },
            displays: vec![DisplayInfo {
                name: "Studio 27Q".into(),
                width: 2560,
                height: 1440,
                refresh_hz: 59.94,
                scale_pct: 125,
                hdr_supported: Some(true),
                hdr_enabled: Some(false),
                size_mm: Some((597, 336)),
                diagonal_inches: Some(27.0),
                ..Default::default()
            }],
        };
        let slow = Slow {
            drives: vec![DriveInfo {
                index: 0,
                model: "Fabrikam Rocket 2TB".into(),
                serial: DRIVE_SERIAL.into(),
                interface: "NVMe".into(),
                media: "SSD".into(),
                size_bytes: 2_000_398_934_016,
                partition_style: "GPT".into(),
                partitions: vec![Partition {
                    number: 1,
                    kind: "Basic data".into(),
                    offset: 1 << 20,
                    size: 1 << 40,
                    letter: "C:".into(),
                }],
                health: HealthStatus::Caution,
                health_pct: Some(88),
                health_summary: "This drive works, but it shows warning signs.".into(),
                temperature_c: Some(41.0),
                power_on_hours: Some(5_300),
                smart_source: "NVMe health log".into(),
                attributes: vec![SmartAttribute {
                    name: "Temperature".into(),
                    raw: 41,
                    display: "41 \u{b0}C".into(),
                    status: HealthStatus::Good,
                    explanation:
                        "The overall temperature of the drive: 41 \u{b0}C is a normal value.".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            volumes: vec![
                Volume {
                    mount: "C:".into(),
                    label: format!("{USER} files"),
                    filesystem: "NTFS".into(),
                    total: 1 << 40,
                    free: 1 << 38,
                    drive_index: Some(0),
                    kind: "Fixed".into(),
                    ..Default::default()
                },
                Volume {
                    mount: "Z:".into(),
                    kind: "Network".into(),
                    ..Default::default()
                },
            ],
        };
        let sample = Sample {
            time: 1_800_000_000,
            uptime_s: 90_000,
            cpu: CpuSample {
                total_pct: 23.0,
                temperature_note: NO_DRIVER.into(),
                ..Default::default()
            },
            memory: MemorySample {
                total: 32 << 30,
                used: 12 << 30,
                available: 20 << 30,
                ..Default::default()
            },
            gpus: vec![GpuSample {
                id: "g0".into(),
                load_pct: 12.0,
                temperature_c: Some(55.0),
                ..Default::default()
            }],
            disks: vec![DiskSample {
                index: 0,
                read_bps: 1e6,
                ..Default::default()
            }],
            net: vec![NetSample {
                name: "Ethernet".into(),
                description: format!("{COMPUTER} virtual link"),
                up: true,
                ipv4: vec![IPV4.into()],
                ipv6: vec![IPV6.into()],
                mac: MAC.into(),
                gateway: GATEWAY.into(),
                dns: vec![DNS.into()],
                extra: vec![("Permanent MAC address".into(), EXTRA_MAC.into())],
                ..Default::default()
            }],
            battery: Some(BatteryInfo {
                name: "Example Cell".into(),
                serial: BATTERY_SERIAL.into(),
                percent: 64.0,
                state: BatteryState::Discharging,
                wear_pct: Some(10.0),
                rate_mw: Some(-9_000),
                seconds_left: Some(7_200),
                ..Default::default()
            }),
            sensors: vec![
                SensorReading {
                    group: "Processor".into(),
                    name: "Core temperatures".into(),
                    kind: SensorKind::Temperature,
                    value: None,
                    source: String::new(),
                    note: NO_DRIVER.into(),
                },
                SensorReading {
                    group: "Drive 0".into(),
                    name: "Temperature".into(),
                    kind: SensorKind::Temperature,
                    value: Some(41.0),
                    source: "NVMe health log".into(),
                    note: String::new(),
                },
            ],
            processes: (0..80)
                .map(|i| ProcessSample {
                    pid: 1000 + i,
                    name: format!("worker{i}.exe"),
                    cpu_pct: i as f32 / 10.0,
                    memory: u64::from(i) << 20,
                    user: format!("{COMPUTER}\\{USER}"),
                    ..Default::default()
                })
                .collect(),
        };
        (inventory, slow, sample)
    }

    fn render_fixture(format: Format, mask: bool, fahrenheit: bool) -> String {
        let (inventory, slow, sample) = fixture();
        let data = ReportData {
            inventory: &inventory,
            slow: &slow,
            sample: &sample,
            generated: 1_800_000_000,
            fahrenheit,
        };
        render(&data, format, mask)
    }

    #[test]
    fn serial_masking() {
        assert_eq!(
            mask_serial("ABCDEF1A2B"),
            "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}1A2B"
        );
        assert_eq!(mask_serial(""), "");
        assert_eq!(mask_serial("A"), "\u{2022}");
        assert_eq!(mask_serial("1234"), "\u{2022}\u{2022}\u{2022}\u{2022}");
        assert_eq!(mask_serial("12345"), "\u{2022}2345");
        // Multi-byte characters: no slicing inside a character.
        assert_eq!(
            mask_serial("序列号äöü12"),
            "\u{2022}\u{2022}\u{2022}\u{2022}öü12"
        );
        assert_eq!(mask_serial("ключ"), "\u{2022}\u{2022}\u{2022}\u{2022}");
        assert_eq!(mask_serial("🙂🙂🙂🙂🙂"), "\u{2022}🙂🙂🙂🙂");
        assert_eq!(hide(""), "");
        assert_eq!(hide("x"), hide("a much longer value"));
    }

    #[test]
    fn identifying_labels() {
        for label in [
            "Serial number",
            "Asset tag",
            "UUID",
            "MAC",
            "Permanent MAC address",
            "IP address",
            "Name of user",
            "Device ID",
            "Wi-Fi SSID",
            "User",
            "Registered owner",
        ] {
            assert!(label_is_identifying(label), "{label}");
        }
        for label in [
            "Machine type",
            "Sector size",
            "TRIM",
            "Module type",
            "Year of manufacture",
            "Width",
            "Chassis colour",
        ] {
            assert!(!label_is_identifying(label), "{label}");
        }
    }

    #[test]
    fn masking_hides_every_identifying_value() {
        for format in [Format::Html, Format::Text, Format::Json] {
            let open = render_fixture(format, false, false);
            let hidden = render_fixture(format, true, false);
            for secret in SECRETS {
                assert!(
                    open.contains(secret),
                    "{format:?}: the unmasked report should show {secret}"
                );
                assert!(
                    !hidden.contains(secret),
                    "{format:?}: the masked report leaks {secret}"
                );
            }
            // What is not identifying stays readable.
            for kept in [
                "Fabrikam Rocket 2TB",
                "Example Core 9 1490X",
                "Graphite",
                "Falcon 15",
            ] {
                assert!(hidden.contains(kept), "{format:?}: {kept} is missing");
            }
            assert!(hidden.contains(&mask_serial(DRIVE_SERIAL)));
        }
        let (inventory, slow, sample) = fixture();
        // The short summary for the clipboard names the parts and gives
        // nothing identifying away, without any masking.
        let short = summary(&inventory, &slow, &sample);
        for kept in [
            "Computer: ",
            "Falcon 15",
            "Processor: Example Core 9 1490X",
            "Storage: Fabrikam Rocket 2TB",
            "Display: ",
        ] {
            assert!(short.contains(kept), "summary: {kept} is missing\n{short}");
        }
        for secret in [SYSTEM_SERIAL, DRIVE_SERIAL, COMPUTER, USER, IPV4] {
            assert!(!short.contains(secret), "summary gives {secret} away");
        }
        assert!(!short.contains("Battery"));
        assert!(short.lines().count() >= 5);
        assert_eq!(
            summary(&Inventory::default(), &Slow::default(), &Sample::default()),
            ""
        );
        let data = ReportData {
            inventory: &inventory,
            slow: &slow,
            sample: &sample,
            generated: 0,
            fahrenheit: false,
        };
        let (i, s, m) = masked(&data);
        assert_eq!(i.system.system_serial, mask_serial(SYSTEM_SERIAL));
        assert_eq!(i.system.computer_name, hide(COMPUTER));
        assert_eq!(i.memory_modules[1].serial, "");
        assert_eq!(s.drives[0].serial, mask_serial(DRIVE_SERIAL));
        assert!(!s.volumes[0].label.contains(USER));
        assert!(m.processes.iter().all(|p| !p.user.contains(USER)));
        assert_eq!(m.net[0].ipv4, vec![hide(IPV4)]);
        // The originals are untouched.
        assert_eq!(inventory.system.system_serial, SYSTEM_SERIAL);
    }

    #[test]
    fn html_is_self_contained_and_escaped() {
        let (mut inventory, mut slow, mut sample) = fixture();
        let attack = "<script>alert('x')</script>";
        slow.drives[0].model = attack.into();
        inventory.cpu.name = "Core \"Quoted\" & <b>bold</b>".into();
        inventory.displays[0].name = attack.into();
        sample.processes[79].name = "<img src=x onerror=alert(1)>".into();
        sample.net[0]
            .extra
            .push(("<i>label</i>".into(), "<u>value</u>".into()));
        slow.drives[0].attributes[0].explanation = "</td><script>bad()</script>".into();
        let data = ReportData {
            inventory: &inventory,
            slow: &slow,
            sample: &sample,
            generated: 1_800_000_000,
            fahrenheit: false,
        };
        for mask in [false, true] {
            let html = render(&data, Format::Html, mask);
            assert!(html.starts_with("<!DOCTYPE html>"));
            assert!(html.trim_end().ends_with("</html>"));
            let lower = html.to_lowercase();
            assert!(!lower.contains("<script"), "no script element may appear");
            assert!(!lower.contains("<img"));
            assert!(!lower.contains("<b>bold"));
            assert!(!lower.contains("<i>label"));
            assert!(!lower.contains("<u>value"));
            assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
            assert!(html.contains("Core &quot;Quoted&quot; &amp; &lt;b&gt;bold&lt;/b&gt;"));
            // Self-contained: nothing is loaded from anywhere.
            for external in ["http://", "https://", "src=\"", "<link", "@import", "url("] {
                assert!(!lower.contains(external), "{external}");
            }
            assert!(html.contains("#ff3d8b") && html.contains("#7a3cf0"));
            assert!(html.contains(&format!(
                "Generated by PulseForge {VERSION} \u{b7} Zheer Khalid (zheer.dev)"
            )));
            // Every element that is opened is closed.
            for tag in [
                "section", "table", "tr", "td", "th", "div", "span", "h2", "h3", "p", "nav",
            ] {
                let opened = html.matches(&format!("<{tag}>")).count()
                    + html.matches(&format!("<{tag} ")).count();
                let closed = html.matches(&format!("</{tag}>")).count();
                assert_eq!(opened, closed, "<{tag}> is unbalanced");
            }
            for id in [
                "summary",
                "processor",
                "memory",
                "graphics",
                "storage",
                "battery",
                "network",
                "displays",
                "system",
                "sensors",
                "processes",
            ] {
                assert!(html.contains(&format!("id=\"{id}\"")), "{id}");
            }
            assert!(html.contains("badge caution"));
            assert!(html.contains("class=\"meter\""));
            assert!(html.contains("not available without a driver"));
        }
    }

    #[test]
    fn text_report_is_aligned_and_complete() {
        let text = render_fixture(Format::Text, true, false);
        assert!(text.starts_with("PulseForge system report\r\n"));
        for title in [
            "SUMMARY",
            "PROCESSOR",
            "MEMORY",
            "GRAPHICS",
            "STORAGE",
            "BATTERY",
            "NETWORK",
            "DISPLAYS",
            "SYSTEM",
            "SENSORS",
            "TOP PROCESSES",
        ] {
            assert!(text.contains(&format!("\r\n{title}\r\n")), "{title}");
        }
        assert!(text.contains("[Caution]"));
        assert!(text.contains("41 \u{b0}C"));
        assert!(text.contains("2560 \u{d7} 1440"));
        assert!(text.contains("59.94 Hz"));
        assert!(text.contains("The 25 busiest of 80 processes are listed."));
        assert!(text.contains("worker79.exe"));
        assert!(!text.contains("worker10.exe"));
        assert!(text.lines().all(|l| l == l.trim_end()));
        assert!(text.trim_end().ends_with("Zheer Khalid (zheer.dev)"));
        // Label and value columns line up inside a block.
        let model = text
            .lines()
            .find(|l| l.starts_with("Model "))
            .expect("model row");
        let firmware = text
            .lines()
            .find(|l| l.starts_with("Serial number "))
            .expect("serial row");
        let column = |l: &str| {
            l.find("  ")
                .map(|i| l[i..].len() - l[i..].trim_start().len() + i)
        };
        assert_eq!(column(model), column(firmware));
        // Fahrenheit is honoured everywhere a temperature is shown.
        let f = render_fixture(Format::Text, true, true);
        assert!(f.contains("106 \u{b0}F"));
        assert!(!f.contains("41 \u{b0}C is") || f.contains("106 \u{b0}F"));
        let html = render_fixture(Format::Html, true, true);
        assert!(html.contains("106 \u{b0}F") && html.contains("131 \u{b0}F"));
    }

    #[test]
    fn json_parses_back() {
        for mask in [false, true] {
            let json = render_fixture(Format::Json, mask, false);
            let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
            assert_eq!(value["generator"], "PulseForge");
            assert_eq!(value["version"], VERSION);
            assert_eq!(value["masked"], mask);
            assert_eq!(value["generated"].as_str().map(str::len), Some(19));
            assert_eq!(value["inventory"]["cpu"]["name"], "Example Core 9 1490X");
            let processes = value["sample"]["processes"].as_array().expect("array");
            assert_eq!(processes.len(), 50);
            assert_eq!(processes[0]["name"], "worker79.exe");
            let inventory: Inventory =
                serde_json::from_value(value["inventory"].clone()).expect("inventory");
            let slow: Slow = serde_json::from_value(value["slow"].clone()).expect("slow");
            let sample: Sample = serde_json::from_value(value["sample"].clone()).expect("sample");
            let (original, original_slow, _) = fixture();
            if mask {
                assert_eq!(inventory.system.user_name, hide(USER));
                assert_eq!(slow.drives[0].serial, mask_serial(DRIVE_SERIAL));
            } else {
                assert_eq!(inventory, original);
                assert_eq!(slow, original_slow);
            }
            assert_eq!(sample.net.len(), 1);
        }
    }

    #[test]
    fn empty_data_renders() {
        let (inventory, slow, sample) = (Inventory::default(), Slow::default(), Sample::default());
        let data = ReportData {
            inventory: &inventory,
            slow: &slow,
            sample: &sample,
            generated: 0,
            fahrenheit: true,
        };
        for mask in [false, true] {
            let html = render(&data, Format::Html, mask);
            assert!(html.contains("</html>"));
            assert!(html.contains("This computer has no battery."));
            assert!(html.contains("No information was collected"));
            let text = render(&data, Format::Text, mask);
            assert!(text.contains("STORAGE"));
            let json = render(&data, Format::Json, mask);
            let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
            assert!(value["sample"]["battery"].is_null());
        }
        // Odd numbers must not break the output either.
        let mut sample = Sample::default();
        sample.memory.total = u64::MAX;
        sample.memory.used = u64::MAX;
        sample.cpu.total_pct = f32::NAN;
        sample.battery = Some(BatteryInfo {
            percent: f32::INFINITY,
            wear_pct: Some(f32::NAN),
            rate_mw: Some(i32::MIN),
            ..Default::default()
        });
        sample.sensors.push(SensorReading {
            value: Some(f64::NAN),
            kind: SensorKind::Fan,
            ..Default::default()
        });
        let data = ReportData {
            sample: &sample,
            ..data
        };
        for format in [Format::Html, Format::Text, Format::Json] {
            let _ = render(&data, format, true);
        }
    }
}
