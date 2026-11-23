//! The background monitor: one thread takes a reading on every tick, records
//! the graph history, checks the alert rules, writes the CSV log and keeps
//! the tray icon up to date. The user interface only reads the shared state.
//!
//! Hardware inventory (read once) and drive health (read about once a
//! minute) are collected on short-lived helper threads so that a slow drive
//! never delays a tick.

use crate::alerts::{self, AlertEvent, AlertRule, Engine, Metric};
use crate::collect;
use crate::csvlog::Logger;
use crate::demo::{self, DemoSource};
use crate::history::{keys, History};
use crate::model::*;
use crate::settings::{Settings, TrayValue};
use crate::tray::Tray;
use crate::{sys, util};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Seconds between drive-health and volume refreshes.
pub const SLOW_PERIOD_S: f64 = 60.0;
/// Seconds between battery readings.
const BATTERY_PERIOD_S: f64 = 4.0;
/// While the window is hidden: seconds between processor-clock readings.
const ECONOMY_CLOCK_PERIOD_S: f64 = 4.9;
/// While the window is hidden: seconds between graphics readings.
const ECONOMY_GPU_PERIOD_S: f64 = 2.9;
/// Seconds between process-list readings (slightly under two ticks at 1 s).
const PROCESS_PERIOD_S: f64 = 1.9;
use alerts::EVENTS_KEPT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// This machine.
    Real,
    /// Invented hardware and readings.
    Demo,
}

/// What the monitor costs, measured on itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overhead {
    /// Processor use of the whole program over the last tick, percent of the machine.
    pub cpu_pct: f32,
    /// The same, averaged since the program started.
    pub cpu_avg_pct: f32,
    /// Private memory of the program in bytes.
    pub memory: u64,
    /// Time the last reading took, in milliseconds.
    pub collect_ms: f32,
    /// Smoothed time per reading.
    pub collect_avg_ms: f32,
    /// Memory held by the graph history.
    pub history_bytes: usize,
}

/// The state the user interface reads. Lock it briefly: clone the `Arc`s and
/// query the history, then let go.
pub struct Shared {
    pub inventory: Arc<Inventory>,
    /// False until the first inventory has arrived.
    pub inventory_ready: bool,
    pub slow: Arc<Slow>,
    pub slow_ready: bool,
    pub sample: Arc<Sample>,
    pub history: History,
    /// Number of readings taken so far.
    pub tick: u64,
    /// Newest last.
    pub events: VecDeque<AlertEvent>,
    /// Metric, subject and latest value of every alert that is active now.
    pub active_alerts: Vec<(Metric, String, f32)>,
    pub overhead: Overhead,
    /// The CSV file being written, if logging is on.
    pub log_file: Option<PathBuf>,
    pub log_error: Option<String>,
    /// Rows written to the CSV log since the program started.
    pub log_rows: u64,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            inventory: Arc::new(Inventory::default()),
            inventory_ready: false,
            slow: Arc::new(Slow::default()),
            slow_ready: false,
            sample: Arc::new(Sample::default()),
            history: History::new(),
            tick: 0,
            events: VecDeque::new(),
            active_alerts: Vec::new(),
            overhead: Overhead::default(),
            log_file: None,
            log_error: None,
            log_rows: 0,
        }
    }
}

/// The part of the settings the monitor thread works with.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub interval_ms: u32,
    pub alerts_enabled: bool,
    pub rules: Vec<AlertRule>,
    /// Folder and row interval, when CSV logging is on.
    pub log: Option<(PathBuf, u32)>,
    /// Days a CSV log file is kept; 0 keeps them for ever.
    pub log_keep_days: u32,
    pub tray_value: TrayValue,
    pub fahrenheit: bool,
}

impl Config {
    pub fn from_settings(s: &Settings) -> Config {
        Config {
            interval_ms: s.interval_ms.clamp(250, 60_000),
            alerts_enabled: s.alerts_enabled,
            rules: s.alerts.clone(),
            log: s.log_csv.then(|| (s.log_folder(), s.log_interval_s)),
            log_keep_days: s.log_keep_days,
            tray_value: s.tray_value,
            fahrenheit: s.fahrenheit,
        }
    }
}

enum Cmd {
    Config(Box<Config>),
    Tray(Option<Tray>),
    RefreshInventory,
    RefreshSlow,
    ClearEvents,
    InventoryDone(Box<Inventory>),
    SlowDone(Box<Slow>),
    Stop,
}

/// Handle to the monitor thread.
pub struct Monitor {
    pub shared: Arc<Mutex<Shared>>,
    /// The user interface sets this to false while its window is hidden or
    /// minimised: the monitor then skips the process list and does not wake
    /// the window, which is what keeps the idle cost low.
    pub ui_visible: Arc<AtomicBool>,
    pub source: Source,
    tx: Sender<Cmd>,
}

impl Monitor {
    /// Starts monitoring. `wake` is called after every reading while
    /// `ui_visible` is set (typically `ctx.request_repaint()`).
    pub fn start(source: Source, settings: &Settings, wake: Box<dyn Fn() + Send>) -> Monitor {
        let shared = Arc::new(Mutex::new(Shared::new()));
        let ui_visible = Arc::new(AtomicBool::new(true));
        let (tx, rx) = channel();
        let config = Config::from_settings(settings);
        if source == Source::Real {
            // What the alerts reported in earlier runs.
            if let Ok(mut s) = shared.lock() {
                s.events = alerts::load_history(&history_path()).into();
            }
        }
        if source == Source::Demo {
            // Ready before the first frame, so screenshots never show a loading state.
            let mut source = DemoSource::new();
            if let Ok(mut s) = shared.lock() {
                s.inventory = Arc::new(demo::inventory());
                s.inventory_ready = true;
                s.slow = Arc::new(demo::slow());
                s.slow_ready = true;
                prefill_demo(&mut s, &mut source);
            }
        }
        let worker = Worker {
            shared: shared.clone(),
            ui_visible: ui_visible.clone(),
            source,
            config,
            rx,
            tx: tx.clone(),
            wake,
        };
        let _ = std::thread::Builder::new()
            .name("pulseforge-monitor".into())
            .spawn(move || worker.run());
        Monitor {
            shared,
            ui_visible,
            source,
            tx,
        }
    }

    /// Applies changed settings (interval, alert rules, logging, tray value).
    pub fn apply_settings(&self, settings: &Settings) {
        let _ = self
            .tx
            .send(Cmd::Config(Box::new(Config::from_settings(settings))));
    }

    /// Gives the monitor the tray icon to keep up to date (or takes it away).
    pub fn set_tray(&self, tray: Option<Tray>) {
        let _ = self.tx.send(Cmd::Tray(tray));
    }

    /// Reads the hardware inventory again (displays plugged in, and so on).
    pub fn refresh_inventory(&self) {
        let _ = self.tx.send(Cmd::RefreshInventory);
    }

    /// Reads drive health and volumes again now instead of at the next minute.
    pub fn refresh_slow(&self) {
        let _ = self.tx.send(Cmd::RefreshSlow);
    }

    /// Empties the alert history, in memory and on disk.
    pub fn clear_events(&self) {
        let _ = self.tx.send(Cmd::ClearEvents);
    }

    /// A consistent copy of the current data, for reports.
    pub fn snapshot(&self) -> (Arc<Inventory>, Arc<Slow>, Arc<Sample>) {
        match self.shared.lock() {
            Ok(s) => (s.inventory.clone(), s.slow.clone(), s.sample.clone()),
            Err(_) => Default::default(),
        }
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Where the alert history is kept between runs.
pub fn history_path() -> PathBuf {
    sys::app_dir().join("alert-history.json")
}

// ----------------------------------------------------------------------------------
// Reading this machine
// ----------------------------------------------------------------------------------

/// Reads the complete hardware inventory. Takes a moment (firmware tables,
/// registry, display configuration); meant for a helper thread.
pub fn collect_inventory() -> Inventory {
    let mut cpu = collect::cpu::info();
    let smbios = collect::smbios::read().unwrap_or_default();
    if cpu.socket.is_empty() {
        cpu.socket = smbios.cpu_socket.clone();
    }
    Inventory {
        cpu,
        memory_modules: smbios.modules,
        gpus: collect::gpu::adapters(),
        system: collect::system::info(),
        displays: collect::displays::list(),
    }
}

/// Reads drive health, partitions and volumes.
pub fn collect_slow() -> Slow {
    Slow {
        drives: collect::storage::drives(),
        volumes: collect::storage::volumes(),
    }
}

/// Seconds since Windows started.
pub fn uptime_seconds() -> u64 {
    // SAFETY: no arguments, no failure mode.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() / 1000 }
}

/// The live collectors with the state they keep between ticks.
pub struct RealSource {
    cpu: collect::cpu::CpuSampler,
    memory: collect::memory::MemorySampler,
    gpu: collect::gpu::GpuSampler,
    net: collect::network::NetSampler,
    process: collect::process::ProcessSampler,
    sensors: collect::sensors::SensorSampler,
    disk: collect::storage::DiskSampler,
    battery: Option<BatteryInfo>,
    battery_at: Option<Instant>,
    processes: Vec<ProcessSample>,
    processes_at: Option<Instant>,
    gpu_last: collect::gpu::GpuReading,
    gpu_at: Option<Instant>,
}

impl Default for RealSource {
    fn default() -> Self {
        RealSource::new()
    }
}

impl RealSource {
    pub fn new() -> RealSource {
        RealSource {
            cpu: collect::cpu::CpuSampler::new(),
            memory: collect::memory::MemorySampler::new(),
            gpu: collect::gpu::GpuSampler::new(),
            net: collect::network::NetSampler::new(),
            process: collect::process::ProcessSampler::new(),
            sensors: collect::sensors::SensorSampler::new(),
            disk: collect::storage::DiskSampler::new(),
            battery: None,
            battery_at: None,
            processes: Vec::new(),
            processes_at: None,
            gpu_last: Default::default(),
            gpu_at: None,
        }
    }

    /// One reading. With `detail` false the process list is skipped.
    pub fn sample(&mut self, detail: bool, slow: &Slow) -> Sample {
        // While nobody is looking (window hidden or minimised) the two most
        // expensive counter sets are read less often and repeated in between.
        self.cpu
            .set_clock_period(Duration::from_secs_f64(if detail {
                0.0
            } else {
                ECONOMY_CLOCK_PERIOD_S
            }));
        let mut cpu = self.cpu.sample();
        let memory = self.memory.sample();
        let gpu_due = detail
            || self
                .gpu_at
                .map(|at| at.elapsed().as_secs_f64() >= ECONOMY_GPU_PERIOD_S)
                .unwrap_or(true);
        if gpu_due {
            self.gpu_last = self.gpu.sample();
            self.gpu_at = Some(Instant::now());
        }
        let gpu = self.gpu_last.clone();
        let net = self.net.sample();
        let disks = self.disk.sample();
        let base_sensors = self.sensors.sample();
        if cpu.temperature_c.is_none() {
            match collect::sensors::cpu_temperature(&base_sensors) {
                Some((value, note)) => {
                    cpu.temperature_c = Some(value);
                    cpu.temperature_note = note;
                }
                None => {
                    if cpu.temperature_note.is_empty() {
                        cpu.temperature_note = NO_DRIVER.to_string();
                    }
                }
            }
        }
        let due = self
            .battery_at
            .map(|at| at.elapsed().as_secs_f64() >= BATTERY_PERIOD_S)
            .unwrap_or(true);
        if due {
            self.battery = collect::battery::read();
            self.battery_at = Some(Instant::now());
        }
        // The process list is the most expensive reading by far (one large
        // system call): take it at most every two seconds and only while
        // somebody can see it.
        let processes = if detail {
            let due = self
                .processes_at
                .map(|at| at.elapsed().as_secs_f64() >= PROCESS_PERIOD_S)
                .unwrap_or(true);
            if due || self.processes.is_empty() {
                self.processes = self.process.sample(&gpu.per_process);
                self.processes_at = Some(Instant::now());
            }
            self.processes.clone()
        } else {
            self.processes.clear();
            self.processes_at = None;
            Vec::new()
        };
        let mut sample = Sample {
            time: util::now_unix(),
            uptime_s: uptime_seconds(),
            cpu,
            memory,
            gpus: gpu.gpus,
            disks,
            net,
            battery: self.battery.clone(),
            sensors: Vec::new(),
            processes,
        };
        sample.sensors = compose_sensors(base_sensors, &sample, slow, &[]);
        sample
    }
}

/// The full sensors list: what the sensor collector found, followed by the
/// readings other collectors own (graphics, drives, battery). Readings that
/// are missing stay in the list with the reason, so the page can say so.
pub fn compose_sensors(
    base: Vec<SensorReading>,
    sample: &Sample,
    slow: &Slow,
    gpu_names: &[(String, String)],
) -> Vec<SensorReading> {
    let mut out = base;
    let mut add = |group: &str,
                   name: &str,
                   kind: SensorKind,
                   value: Option<f64>,
                   source: &str,
                   note: &str| {
        out.push(SensorReading {
            group: group.to_string(),
            name: name.to_string(),
            kind,
            value,
            source: source.to_string(),
            note: note.to_string(),
        });
    };
    for (i, g) in sample.gpus.iter().enumerate() {
        let group = gpu_names
            .iter()
            .find(|(id, _)| *id == g.id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| format!("Graphics {i}"));
        let source = if g.temperature_c.is_some() {
            "Vendor library"
        } else {
            ""
        };
        let note = if g.temperature_c.is_some() {
            ""
        } else if g.temperature_note.is_empty() {
            NO_DRIVER
        } else {
            g.temperature_note.as_str()
        };
        add(
            &group,
            "Temperature",
            SensorKind::Temperature,
            g.temperature_c.map(f64::from),
            source,
            note,
        );
        add(
            &group,
            "Load",
            SensorKind::Load,
            Some(f64::from(g.load_pct)),
            "Windows performance counters",
            "",
        );
        if let Some(rpm) = g.fan_rpm {
            add(
                &group,
                "Fan",
                SensorKind::Fan,
                Some(f64::from(rpm)),
                "Vendor library",
                "",
            );
        } else if let Some(pct) = g.fan_pct {
            add(
                &group,
                "Fan duty",
                SensorKind::Load,
                Some(f64::from(pct)),
                "Vendor library",
                "",
            );
        }
        if let Some(w) = g.power_w {
            add(
                &group,
                "Power",
                SensorKind::Power,
                Some(f64::from(w)),
                "Vendor library",
                "",
            );
        }
        if let Some(mhz) = g.clock_mhz {
            add(
                &group,
                "Core clock",
                SensorKind::Clock,
                Some(f64::from(mhz)),
                "Vendor library",
                "",
            );
        }
    }
    for d in &slow.drives {
        let group = if d.model.is_empty() {
            format!("Drive {}", d.index)
        } else {
            format!("Drive {} · {}", d.index, d.model)
        };
        let note = if d.temperature_c.is_some() {
            ""
        } else if d.smart_note.is_empty() {
            "the drive does not report a temperature"
        } else {
            d.smart_note.as_str()
        };
        add(
            &group,
            "Temperature",
            SensorKind::Temperature,
            d.temperature_c.map(f64::from),
            &d.smart_source,
            note,
        );
    }
    if let Some(b) = &sample.battery {
        if let Some(mv) = b.voltage_mv {
            add(
                "Battery",
                "Voltage",
                SensorKind::Voltage,
                Some(f64::from(mv) / 1000.0),
                "Battery driver",
                "",
            );
        }
        if let Some(mw) = b.rate_mw {
            add(
                "Battery",
                if mw >= 0 {
                    "Charge rate"
                } else {
                    "Discharge rate"
                },
                SensorKind::Power,
                Some(f64::from(mw.abs()) / 1000.0),
                "Battery driver",
                "",
            );
        }
        if let Some(t) = b.temperature_c {
            add(
                "Battery",
                "Temperature",
                SensorKind::Temperature,
                Some(f64::from(t)),
                "Battery driver",
                "",
            );
        }
    }
    out
}

/// Stores one reading in the graph history at time `t`.
pub fn record(history: &mut History, t: f64, sample: &Sample, inventory: &Inventory, slow: &Slow) {
    let c = &sample.cpu;
    history.push(keys::CPU_TOTAL, t, c.total_pct);
    history.push(keys::CPU_KERNEL, t, c.kernel_pct);
    history.push_opt(keys::CPU_MHZ, t, c.current_mhz);
    history.push_opt(keys::CPU_TEMP, t, c.temperature_c);
    for (i, v) in c.per_core_pct.iter().enumerate() {
        history.push(&keys::cpu_core(i), t, *v);
    }
    let m = &sample.memory;
    if m.total > 0 {
        history.push(keys::MEM_USED_PCT, t, m.used_pct());
        history.push(keys::MEM_COMMIT_PCT, t, m.commit_pct());
        history.push(keys::MEM_CACHED_PCT, t, pct(m.cached, m.total));
        history.push(keys::MEM_PAGEFILE_PCT, t, m.pagefile_pct());
    }
    for g in &sample.gpus {
        history.push(&keys::gpu_load(&g.id), t, g.load_pct);
        let dedicated = inventory
            .gpus
            .iter()
            .find(|i| i.id == g.id)
            .map(|i| i.dedicated_bytes)
            .unwrap_or(0);
        if dedicated > 0 {
            history.push(
                &keys::gpu_mem_pct(&g.id),
                t,
                pct(g.dedicated_used, dedicated),
            );
        }
        history.push_opt(&keys::gpu_temp(&g.id), t, g.temperature_c);
        for e in &g.engines {
            history.push(&keys::gpu_engine(&g.id, &e.name), t, e.pct);
        }
    }
    let (read, write) = sample.disk_totals();
    history.push(keys::DISK_READ, t, read as f32);
    history.push(keys::DISK_WRITE, t, write as f32);
    for d in &sample.disks {
        history.push(&keys::disk_read(d.index), t, d.read_bps as f32);
        history.push(&keys::disk_write(d.index), t, d.write_bps as f32);
        history.push(&keys::disk_active(d.index), t, d.active_pct);
    }
    for d in &slow.drives {
        history.push_opt(&keys::disk_temp(d.index), t, d.temperature_c);
    }
    let (rx, tx) = sample.net_totals();
    history.push(keys::NET_RX, t, rx as f32);
    history.push(keys::NET_TX, t, tx as f32);
    for n in &sample.net {
        history.push(&keys::net_rx(&n.name), t, n.rx_bps as f32);
        history.push(&keys::net_tx(&n.name), t, n.tx_bps as f32);
    }
    if let Some(b) = &sample.battery {
        history.push(keys::BATTERY_PCT, t, b.percent);
        history.push_opt(
            keys::BATTERY_RATE_W,
            t,
            b.rate_mw.map(|mw| mw as f32 / 1000.0),
        );
    }
    for s in &sample.sensors {
        if let Some(v) = s.value {
            if s.kind != SensorKind::Load {
                history.push(&keys::sensor(&s.group, &s.name), t, v as f32);
            }
        }
    }
}

/// The text for the tray icon (up to three characters) and its tooltip.
pub fn tray_text(sample: &Sample, value: TrayValue, fahrenheit: bool) -> (String, String) {
    let temp = |c: f32| {
        let v = if fahrenheit { util::c_to_f(c) } else { c };
        format!("{:.0}", v.clamp(-99.0, 999.0))
    };
    let whole = |p: f32| format!("{:.0}", p.clamp(0.0, 100.0));
    let icon = match value {
        TrayValue::CpuUsage => whole(sample.cpu.total_pct),
        TrayValue::CpuTemp => sample.cpu.temperature_c.map(temp).unwrap_or_default(),
        TrayValue::MemoryUsage => whole(sample.memory.used_pct()),
        TrayValue::GpuUsage => sample.gpu_load().map(whole).unwrap_or_default(),
        TrayValue::GpuTemp => sample.gpu_temp().map(temp).unwrap_or_default(),
        TrayValue::Battery => sample
            .battery
            .as_ref()
            .map(|b| whole(b.percent))
            .unwrap_or_default(),
        TrayValue::None => String::new(),
    };
    let mut tip = format!(
        "PulseForge\nProcessor {}",
        util::fmt_pct(sample.cpu.total_pct)
    );
    if let Some(t) = sample.cpu.temperature_c {
        tip.push_str(&format!(" · {}", util::fmt_temp(t, fahrenheit)));
    }
    tip.push_str(&format!(
        "\nMemory {}",
        util::fmt_pct(sample.memory.used_pct())
    ));
    if let Some(load) = sample.gpu_load() {
        tip.push_str(&format!("\nGraphics {}", util::fmt_pct(load)));
        if let Some(t) = sample.gpu_temp() {
            tip.push_str(&format!(" · {}", util::fmt_temp(t, fahrenheit)));
        }
    }
    if let Some(b) = &sample.battery {
        tip.push_str(&format!("\nBattery {}", util::fmt_pct(b.percent)));
    }
    (icon, tip)
}

/// Fills 24 hours of graph history with invented readings, ending at t = 0.
fn prefill_demo(shared: &mut Shared, source: &mut DemoSource) {
    let now = util::now_unix();
    let inventory = shared.inventory.clone();
    let slow = shared.slow.clone();
    let mut t = -86_400.0_f64;
    while t <= 0.0 {
        let detail = t == 0.0;
        let mut sample = source.sample(t, (now as f64 + t) as u64, detail);
        if t == 0.0 {
            let names = gpu_names(&inventory);
            sample.sensors =
                compose_sensors(std::mem::take(&mut sample.sensors), &sample, &slow, &names);
        }
        record(&mut shared.history, t, &sample, &inventory, &slow);
        if t == 0.0 {
            shared.sample = Arc::new(sample);
            shared.tick = 1;
        }
        // Coarse steps for the distant past, every second for the last ten minutes.
        t += if t < -7200.0 {
            20.0
        } else if t < -600.0 {
            5.0
        } else {
            1.0
        };
    }
}

fn gpu_names(inventory: &Inventory) -> Vec<(String, String)> {
    inventory
        .gpus
        .iter()
        .map(|g| (g.id.clone(), g.name.clone()))
        .collect()
}

// ----------------------------------------------------------------------------------
// The monitor thread
// ----------------------------------------------------------------------------------

struct Worker {
    shared: Arc<Mutex<Shared>>,
    ui_visible: Arc<AtomicBool>,
    source: Source,
    config: Config,
    rx: Receiver<Cmd>,
    tx: Sender<Cmd>,
    wake: Box<dyn Fn() + Send>,
}

impl Worker {
    fn spawn_inventory(&self) {
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new()
            .name("pulseforge-inventory".into())
            .spawn(move || {
                let _ = tx.send(Cmd::InventoryDone(Box::new(collect_inventory())));
            });
    }

    fn spawn_slow(&self) {
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new()
            .name("pulseforge-drives".into())
            .spawn(move || {
                let _ = tx.send(Cmd::SlowDone(Box::new(collect_slow())));
            });
    }

    fn run(mut self) {
        let started = Instant::now();
        let demo = self.source == Source::Demo;
        let mut real = (!demo).then(RealSource::new);
        let mut demo_source = DemoSource::new();
        let mut engine = Engine::new();
        let mut logger: Option<Logger> = None;
        let mut pruned_at: Option<Instant> = None;
        let mut tray: Option<Tray> = None;
        let mut slow_running = false;
        let mut slow_at = Instant::now();
        let mut inventory = Arc::new(Inventory::default());
        let mut slow = Arc::new(Slow::default());
        if demo {
            if let Ok(s) = self.shared.lock() {
                inventory = s.inventory.clone();
                slow = s.slow.clone();
            }
        } else {
            self.spawn_inventory();
            self.spawn_slow();
            slow_running = true;
        }
        let cpus = f64::from(sys::logical_processors());
        let (first_cpu, _) = sys::self_usage();
        let mut last_cpu = first_cpu;
        let mut last_wall = Instant::now();
        let mut collect_avg_ms = 0.0_f32;
        let mut next = Instant::now();

        loop {
            // Wait for the next tick, handling commands as they come.
            loop {
                let now = Instant::now();
                let wait = next.saturating_duration_since(now);
                let cmd = if wait.is_zero() {
                    self.rx.try_recv().ok()
                } else {
                    match self.rx.recv_timeout(wait) {
                        Ok(c) => Some(c),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                };
                match cmd {
                    Some(Cmd::Stop) => {
                        if let Some(t) = &tray {
                            t.stop();
                        }
                        return;
                    }
                    Some(Cmd::Config(c)) => {
                        if c.interval_ms != self.config.interval_ms {
                            next = Instant::now();
                        }
                        if c.log != self.config.log || c.log_keep_days != self.config.log_keep_days
                        {
                            pruned_at = None;
                        }
                        if c.log != self.config.log {
                            logger = None;
                            if let Ok(mut s) = self.shared.lock() {
                                s.log_file = None;
                                s.log_error = None;
                            }
                        }
                        self.config = *c;
                    }
                    Some(Cmd::Tray(t)) => tray = t,
                    Some(Cmd::RefreshInventory) => {
                        if !demo {
                            self.spawn_inventory();
                        }
                    }
                    Some(Cmd::RefreshSlow) => {
                        if !demo && !slow_running {
                            self.spawn_slow();
                            slow_running = true;
                        }
                    }
                    Some(Cmd::ClearEvents) => {
                        if let Ok(mut s) = self.shared.lock() {
                            s.events.clear();
                        }
                        if !demo {
                            let _ = alerts::save_history(&history_path(), [].iter());
                        }
                        (self.wake)();
                    }
                    Some(Cmd::InventoryDone(inv)) => {
                        inventory = Arc::new(*inv);
                        if let Ok(mut s) = self.shared.lock() {
                            s.inventory = inventory.clone();
                            s.inventory_ready = true;
                        }
                        (self.wake)();
                    }
                    Some(Cmd::SlowDone(new)) => {
                        slow = Arc::new(*new);
                        slow_running = false;
                        slow_at = Instant::now();
                        if let Ok(mut s) = self.shared.lock() {
                            s.slow = slow.clone();
                            s.slow_ready = true;
                        }
                        (self.wake)();
                    }
                    None => {
                        if Instant::now() >= next {
                            break;
                        }
                    }
                }
            }
            let interval = Duration::from_millis(u64::from(self.config.interval_ms));
            next += interval;
            if next < Instant::now() {
                // Fell behind (sleep, a slow reading): do not try to catch up.
                next = Instant::now() + interval;
            }

            let visible = self.ui_visible.load(Ordering::Relaxed);
            let t = started.elapsed().as_secs_f64();
            let began = Instant::now();
            let sample = match &mut real {
                Some(real) => {
                    let mut sample = real.sample(visible, &slow);
                    // Graphics lines are named after their adapter once the inventory is in.
                    if !inventory.gpus.is_empty() {
                        let names = gpu_names(&inventory);
                        for (i, g) in sample.gpus.iter().enumerate() {
                            let generic = format!("Graphics {i}");
                            if let Some((_, name)) = names.iter().find(|(id, _)| *id == g.id) {
                                for s in sample.sensors.iter_mut().filter(|s| s.group == generic) {
                                    s.group = name.clone();
                                }
                            }
                        }
                    }
                    sample
                }
                None => {
                    let mut sample = demo_source.sample(t, util::now_unix(), true);
                    sample.sensors = compose_sensors(
                        std::mem::take(&mut sample.sensors),
                        &sample,
                        &slow,
                        &gpu_names(&inventory),
                    );
                    sample
                }
            };
            let collect_ms = began.elapsed().as_secs_f32() * 1000.0;
            collect_avg_ms = if collect_avg_ms == 0.0 {
                collect_ms
            } else {
                collect_avg_ms * 0.9 + collect_ms * 0.1
            };

            // Alerts.
            let events = if self.config.alerts_enabled {
                engine.evaluate(
                    &self.config.rules,
                    &alerts::readings(&sample, &slow),
                    t,
                    sample.time,
                )
            } else {
                engine = Engine::new();
                Vec::new()
            };
            if let Some(tray) = &tray {
                for e in events.iter().filter(|e| e.raised) {
                    let message = if e.metric.is_temperature() && self.config.fahrenheit {
                        format!(
                            "{}, above your limit of {}.",
                            util::fmt_temp(e.value, true),
                            util::fmt_temp(e.threshold, true)
                        )
                    } else {
                        e.message.clone()
                    };
                    tray.notify(&e.title, &message);
                }
                let (icon, tip) =
                    tray_text(&sample, self.config.tray_value, self.config.fahrenheit);
                tray.set_value(&icon, &tip);
            }

            // CSV log (never in demo mode: nothing is written there).
            let mut log_result = None;
            if !demo {
                if let Some((dir, every)) = &self.config.log {
                    let l = logger.get_or_insert_with(|| Logger::new(dir, *every));
                    log_result = Some((l.log(&sample, &slow), l.current_file()));
                    // Old log files go once at the start and then once an hour.
                    let due = pruned_at.is_none_or(|at| at.elapsed().as_secs() >= 3600);
                    if due && self.config.log_keep_days > 0 {
                        pruned_at = Some(Instant::now());
                        crate::csvlog::prune(dir, sample.time, self.config.log_keep_days);
                    }
                }
            }

            // The program's own cost.
            let (cpu_now, memory) = sys::self_usage();
            let wall = last_wall.elapsed().as_secs_f64().max(0.001);
            let self_pct = ((cpu_now.saturating_sub(last_cpu)) as f64 / 1e7 / wall / cpus * 100.0)
                .clamp(0.0, 100.0) as f32;
            let total_wall = started.elapsed().as_secs_f64().max(0.001);
            let self_avg = ((cpu_now.saturating_sub(first_cpu)) as f64 / 1e7 / total_wall / cpus
                * 100.0)
                .clamp(0.0, 100.0) as f32;
            last_cpu = cpu_now;
            last_wall = Instant::now();

            let new_events = !events.is_empty();
            let mut to_save = None;
            if let Ok(mut s) = self.shared.lock() {
                record(&mut s.history, t, &sample, &inventory, &slow);
                s.history.push(keys::SELF_CPU, t, self_pct);
                s.sample = Arc::new(sample);
                s.tick += 1;
                for e in events {
                    s.events.push_back(e);
                }
                while s.events.len() > EVENTS_KEPT {
                    s.events.pop_front();
                }
                if new_events && !demo {
                    to_save = Some(s.events.clone());
                }
                s.active_alerts = engine.active();
                s.overhead = Overhead {
                    cpu_pct: self_pct,
                    cpu_avg_pct: self_avg,
                    memory,
                    collect_ms,
                    collect_avg_ms,
                    history_bytes: s.history.approx_bytes(),
                };
                if let Some((result, file)) = log_result {
                    match result {
                        Ok(wrote) => {
                            s.log_error = None;
                            s.log_file = file;
                            if wrote {
                                s.log_rows += 1;
                            }
                        }
                        Err(e) => s.log_error = Some(e),
                    }
                }
            }
            if let Some(events) = to_save {
                // Kept between runs; a failure here must never stop monitoring.
                let _ = alerts::save_history(&history_path(), events.iter());
            }
            if visible {
                (self.wake)();
            }
            if !demo && !slow_running && slow_at.elapsed().as_secs_f64() >= SLOW_PERIOD_S {
                self.spawn_slow();
                slow_running = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sample {
        let mut s = Sample::default();
        s.cpu.total_pct = 37.4;
        s.cpu.temperature_c = Some(61.0);
        s.cpu.per_core_pct = vec![10.0, 60.0];
        s.memory.total = 100;
        s.memory.used = 55;
        s.gpus.push(GpuSample {
            id: "a".into(),
            load_pct: 12.0,
            dedicated_used: 50,
            engines: vec![GpuEngine {
                name: "3D".into(),
                pct: 12.0,
            }],
            ..Default::default()
        });
        s.disks.push(DiskSample {
            index: 0,
            read_bps: 100.0,
            write_bps: 50.0,
            active_pct: 3.0,
            queue_depth: None,
        });
        s
    }

    #[test]
    fn tray_text_follows_the_setting() {
        let s = sample();
        assert_eq!(tray_text(&s, TrayValue::CpuUsage, false).0, "37");
        assert_eq!(tray_text(&s, TrayValue::CpuTemp, false).0, "61");
        assert_eq!(tray_text(&s, TrayValue::CpuTemp, true).0, "142");
        assert_eq!(tray_text(&s, TrayValue::MemoryUsage, false).0, "55");
        assert_eq!(tray_text(&s, TrayValue::GpuUsage, false).0, "12");
        // No reading: fall back to the program icon rather than a made-up number.
        assert_eq!(tray_text(&s, TrayValue::GpuTemp, false).0, "");
        assert_eq!(tray_text(&s, TrayValue::Battery, false).0, "");
        assert_eq!(tray_text(&s, TrayValue::None, false).0, "");
        let tip = tray_text(&s, TrayValue::None, false).1;
        assert!(tip.contains("Processor 37% · 61 °C"));
        assert!(tip.contains("Memory 55%"));
        assert!(tip.chars().count() < 120);
    }

    #[test]
    fn recording_fills_the_expected_series() {
        let s = sample();
        let inventory = Inventory {
            gpus: vec![GpuInfo {
                id: "a".into(),
                dedicated_bytes: 200,
                ..Default::default()
            }],
            ..Default::default()
        };
        let slow = Slow {
            drives: vec![DriveInfo {
                index: 0,
                temperature_c: Some(40.0),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut h = History::new();
        record(&mut h, 1.0, &s, &inventory, &slow);
        let last = |k: &str| h.get(k).and_then(|s| s.last());
        assert_eq!(last(keys::CPU_TOTAL), Some(37.4));
        assert_eq!(last(keys::CPU_TEMP), Some(61.0));
        assert_eq!(last(&keys::cpu_core(1)), Some(60.0));
        assert_eq!(last(keys::MEM_USED_PCT), Some(55.0));
        assert_eq!(last(&keys::gpu_load("a")), Some(12.0));
        assert_eq!(last(&keys::gpu_mem_pct("a")), Some(25.0));
        assert_eq!(last(&keys::gpu_engine("a", "3D")), Some(12.0));
        assert_eq!(last(&keys::gpu_temp("a")), None);
        assert_eq!(last(keys::DISK_READ), Some(100.0));
        assert_eq!(last(&keys::disk_write(0)), Some(50.0));
        assert_eq!(last(&keys::disk_temp(0)), Some(40.0));
        assert_eq!(last(keys::BATTERY_PCT), None);
        assert_eq!(last(keys::CPU_MHZ), None);
    }

    #[test]
    fn missing_readings_stay_in_the_sensor_list_with_a_reason() {
        let s = sample();
        let slow = Slow {
            drives: vec![DriveInfo {
                index: 2,
                model: "Example SSD".into(),
                smart_note: NEEDS_ADMIN.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let names = vec![("a".to_string(), "Example GPU".to_string())];
        let list = compose_sensors(Vec::new(), &s, &slow, &names);
        let gpu = list
            .iter()
            .find(|r| r.group == "Example GPU" && r.kind == SensorKind::Temperature)
            .unwrap();
        assert_eq!(gpu.value, None);
        assert_eq!(gpu.note, NO_DRIVER);
        let drive = list
            .iter()
            .find(|r| r.group == "Drive 2 · Example SSD")
            .unwrap();
        assert_eq!(drive.value, None);
        assert_eq!(drive.note, NEEDS_ADMIN);
        assert!(!list.iter().any(|r| r.group == "Battery"));
    }

    #[test]
    fn demo_monitor_is_ready_immediately_and_writes_nothing() {
        let settings = Settings {
            log_csv: true,
            ..Default::default()
        };
        let monitor = Monitor::start(Source::Demo, &settings, Box::new(|| {}));
        {
            let s = monitor.shared.lock().unwrap();
            assert!(s.inventory_ready && s.slow_ready);
            assert_eq!(s.tick, 1);
            assert!(!s.inventory.cpu.name.is_empty());
            // A full day of history is there from the start.
            assert!(s.history.points(keys::CPU_TOTAL, 0.0, 86_400.0).len() > 1000);
            assert!(s.history.points(keys::CPU_TOTAL, 0.0, 60.0).len() >= 60);
        }
        std::thread::sleep(Duration::from_millis(300));
        let s = monitor.shared.lock().unwrap();
        assert!(s.tick >= 2);
        assert!(s.log_file.is_none());
        assert_eq!(s.log_rows, 0);
    }
}
