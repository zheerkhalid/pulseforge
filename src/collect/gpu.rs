//! Graphics adapters: identification, load, memory and (where a vendor
//! library is installed) temperature.
//!
//! Identification comes from DXGI and the display class key of the registry.
//! Load and memory come from the GPU performance counters of Windows, which
//! have one instance per process and engine; they are added up per adapter
//! here. Temperature, fan, power and clock are only available through a
//! vendor library (see [`super::nvml`]); without one they stay empty with a
//! note that says so.

use super::nvml;
use super::pdh;
use crate::model::{GpuEngine, GpuInfo, GpuSample};
use crate::util::from_wide;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_DESC1};

/// GPU use of one process, summed over all adapters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuProcess {
    pub pct: f32,
    pub memory: u64,
}

/// One reading of all adapters.
#[derive(Clone, Debug, Default)]
pub struct GpuReading {
    /// One entry per hardware adapter, in the order of [`adapters`].
    pub gpus: Vec<GpuSample>,
    /// By process id.
    pub per_process: HashMap<u32, GpuProcess>,
}

/// An adapter LUID as (high part, low part).
pub type Luid = (u32, u32);

/// The stable adapter key: the LUID the way the performance counters print
/// it, for example `0x00000000_0x0000ABCD`.
pub fn luid_id(luid: Luid) -> String {
    format!("0x{:08X}_0x{:08X}", luid.0, luid.1)
}

// ----------------------------------------------------------------------------------
// Counter instance names (pure)
// ----------------------------------------------------------------------------------

/// The parts of a `GPU Engine` instance name such as
/// `pid_1234_luid_0x00000000_0x0000ABCD_phys_0_eng_3_engtype_VideoDecode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineInstance<'a> {
    pub pid: u32,
    pub luid: Luid,
    pub phys: u32,
    pub engine: u32,
    /// The engine type as Windows names it ("3D", "VideoDecode", "Compute_0").
    pub kind: &'a str,
}

/// Strips `key` and the underscore after it.
fn after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    s.strip_prefix(key)?.strip_prefix('_')
}

/// Splits at the next underscore (or the end).
fn token(s: &str) -> (&str, &str) {
    match s.split_once('_') {
        Some((head, rest)) => (head, rest),
        None => (s, ""),
    }
}

fn decimal(s: &str) -> Option<(u32, &str)> {
    let (head, rest) = token(s);
    if head.is_empty() || !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head.parse().ok()?, rest))
}

fn hex(s: &str) -> Option<(u32, &str)> {
    let (head, rest) = token(s);
    let digits = head
        .strip_prefix("0x")
        .or_else(|| head.strip_prefix("0X"))?;
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    Some((u32::from_str_radix(digits, 16).ok()?, rest))
}

fn luid_part(s: &str) -> Option<(Luid, &str)> {
    let s = after(s, "luid")?;
    let (high, s) = hex(s)?;
    let (low, s) = hex(s)?;
    Some(((high, low), s))
}

/// Parses a `GPU Engine` instance name. Anything malformed gives `None`.
pub fn parse_engine_instance(name: &str) -> Option<EngineInstance<'_>> {
    let (pid, s) = decimal(after(name, "pid")?)?;
    let (luid, s) = luid_part(s)?;
    let (phys, s) = decimal(after(s, "phys")?)?;
    let (engine, s) = decimal(after(s, "eng")?)?;
    let kind = after(s, "engtype")?;
    (!kind.is_empty()).then_some(EngineInstance {
        pid,
        luid,
        phys,
        engine,
        kind,
    })
}

/// Parses a `GPU Adapter Memory` instance name: `luid_0x..._0x..._phys_0`.
pub fn parse_adapter_instance(name: &str) -> Option<(Luid, u32)> {
    let (luid, s) = luid_part(name)?;
    let (phys, rest) = decimal(after(s, "phys")?)?;
    rest.is_empty().then_some((luid, phys))
}

/// Parses a `GPU Process Memory` instance name: `pid_12_luid_0x..._0x..._phys_0`.
pub fn parse_process_instance(name: &str) -> Option<(u32, Luid)> {
    let (pid, s) = decimal(after(name, "pid")?)?;
    let (luid, s) = luid_part(s)?;
    let (_phys, rest) = decimal(after(s, "phys")?)?;
    rest.is_empty().then_some((pid, luid))
}

/// The engine type as people know it: "VideoDecode" becomes "Video Decode",
/// "Compute_0" becomes "Compute".
pub fn engine_label(kind: &str) -> String {
    // A trailing "_<number>" only tells identical engines apart.
    let base = match kind.rsplit_once('_') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => kind,
    };
    let base = base.trim();
    if base.is_empty() {
        return "Other".to_string();
    }
    if base.eq_ignore_ascii_case("cuda") {
        return "CUDA".to_string();
    }
    let mut out = String::with_capacity(base.len() + 4);
    let mut prev: Option<char> = None;
    for c in base.chars() {
        let c = if c == '_' { ' ' } else { c };
        if let Some(p) = prev {
            if c.is_ascii_uppercase() && p.is_ascii_lowercase() {
                out.push(' ');
            }
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// Display order of the engine types: the well-known ones first.
fn label_rank(label: &str) -> usize {
    const ORDER: [&str; 7] = [
        "3D",
        "Copy",
        "Video Decode",
        "Video Encode",
        "Video Processing",
        "Compute",
        "CUDA",
    ];
    ORDER
        .iter()
        .position(|&o| o == label)
        .unwrap_or(ORDER.len())
}

// ----------------------------------------------------------------------------------
// Adding the instances up (pure)
// ----------------------------------------------------------------------------------

struct Node {
    adapter: usize,
    phys: u32,
    engine: u32,
    label: usize,
    pct: f32,
}

/// Adds the per-process, per-engine counter instances up into per-adapter
/// and per-process figures. All buffers are kept between readings.
#[derive(Default)]
pub struct Aggregator {
    /// Hardware adapters, in output order.
    luids: Vec<Luid>,
    /// Adapters that are known but not reported (software renderers).
    ignored: Vec<Luid>,
    nodes: Vec<Node>,
    /// (raw engine type, display label), interned.
    kinds: Vec<(String, usize)>,
    labels: Vec<String>,
    /// Busiest engine per (process, adapter).
    process_use: HashMap<(u32, usize), f32>,
    process_memory: HashMap<u32, u64>,
    dedicated: Vec<u64>,
    shared: Vec<u64>,
    /// Set when a counter names an adapter that is in neither list.
    pub unknown_adapter: bool,
}

impl Aggregator {
    /// Starts a reading for these hardware adapters.
    pub fn begin(&mut self, luids: &[Luid], ignored: &[Luid]) {
        self.luids.clear();
        self.luids.extend_from_slice(luids);
        self.ignored.clear();
        self.ignored.extend_from_slice(ignored);
        self.nodes.clear();
        self.process_use.clear();
        self.process_memory.clear();
        self.dedicated.clear();
        self.dedicated.resize(luids.len(), 0);
        self.shared.clear();
        self.shared.resize(luids.len(), 0);
        self.unknown_adapter = false;
    }

    fn adapter_index(&mut self, luid: Luid) -> Option<usize> {
        let found = self.luids.iter().position(|&l| l == luid);
        if found.is_none() && !self.ignored.contains(&luid) {
            self.unknown_adapter = true;
        }
        found
    }

    fn label_index(&mut self, kind: &str) -> usize {
        if let Some((_, label)) = self.kinds.iter().find(|(raw, _)| raw == kind) {
            return *label;
        }
        let text = engine_label(kind);
        let label = match self.labels.iter().position(|l| *l == text) {
            Some(index) => index,
            None => {
                self.labels.push(text);
                self.labels.len() - 1
            }
        };
        self.kinds.push((kind.to_string(), label));
        label
    }

    /// One instance of `GPU Engine \ Utilization Percentage`.
    pub fn engine(&mut self, name: &str, value: f64) {
        let Some(instance) = parse_engine_instance(name) else {
            return;
        };
        let Some(adapter) = self.adapter_index(instance.luid) else {
            return;
        };
        let pct = if value.is_finite() {
            value.clamp(0.0, 100.0) as f32
        } else {
            0.0
        };
        let position = self.nodes.iter().position(|n| {
            n.adapter == adapter && n.engine == instance.engine && n.phys == instance.phys
        });
        match position {
            Some(index) => self.nodes[index].pct += pct,
            None => {
                let label = self.label_index(instance.kind);
                self.nodes.push(Node {
                    adapter,
                    phys: instance.phys,
                    engine: instance.engine,
                    label,
                    pct,
                });
            }
        }
        if pct > 0.0 {
            let busiest = self
                .process_use
                .entry((instance.pid, adapter))
                .or_insert(0.0);
            if pct > *busiest {
                *busiest = pct;
            }
        }
    }

    /// One instance of `GPU Adapter Memory`, dedicated or shared usage in bytes.
    pub fn adapter_memory(&mut self, name: &str, value: f64, shared: bool) {
        let Some((luid, _phys)) = parse_adapter_instance(name) else {
            return;
        };
        let Some(adapter) = self.adapter_index(luid) else {
            return;
        };
        let bytes = bytes_from(value);
        let slot = if shared {
            &mut self.shared[adapter]
        } else {
            &mut self.dedicated[adapter]
        };
        *slot = slot.saturating_add(bytes);
    }

    /// One instance of `GPU Process Memory \ Dedicated Usage` in bytes.
    pub fn process_memory(&mut self, name: &str, value: f64) {
        let Some((pid, luid)) = parse_process_instance(name) else {
            return;
        };
        if self.adapter_index(luid).is_none() {
            return;
        }
        let total = self.process_memory.entry(pid).or_insert(0);
        *total = total.saturating_add(bytes_from(value));
    }

    /// Load of one adapter: (busiest engine type, all engine types).
    pub fn adapter_load(&self, adapter: usize) -> (f32, Vec<GpuEngine>) {
        let mut by_label: Vec<(usize, f32)> = Vec::new();
        for node in self.nodes.iter().filter(|n| n.adapter == adapter) {
            let pct = node.pct.clamp(0.0, 100.0);
            match by_label.iter_mut().find(|(label, _)| *label == node.label) {
                Some((_, busiest)) => *busiest = busiest.max(pct),
                None => by_label.push((node.label, pct)),
            }
        }
        let mut engines: Vec<GpuEngine> = by_label
            .into_iter()
            .map(|(label, pct)| GpuEngine {
                name: self.labels[label].clone(),
                pct,
            })
            .collect();
        engines.sort_by(|a, b| {
            (label_rank(&a.name), a.name.as_str()).cmp(&(label_rank(&b.name), b.name.as_str()))
        });
        let load = engines.iter().map(|e| e.pct).fold(0.0f32, f32::max);
        (load, engines)
    }

    /// (dedicated, shared) bytes in use on one adapter.
    pub fn adapter_bytes(&self, adapter: usize) -> (u64, u64) {
        (
            self.dedicated.get(adapter).copied().unwrap_or(0),
            self.shared.get(adapter).copied().unwrap_or(0),
        )
    }

    /// Use per process: the busiest engine on each adapter, added over the
    /// adapters, and the dedicated memory. Every process that holds graphics
    /// memory is listed, also with zero load.
    pub fn per_process(&self) -> HashMap<u32, GpuProcess> {
        let mut out: HashMap<u32, GpuProcess> =
            HashMap::with_capacity(self.process_memory.len().max(self.process_use.len()));
        for (&pid, &memory) in &self.process_memory {
            out.entry(pid).or_default().memory = memory;
        }
        for (&(pid, _), &pct) in &self.process_use {
            let entry = out.entry(pid).or_default();
            entry.pct = (entry.pct + pct).min(100.0);
        }
        out
    }
}

fn bytes_from(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

// ----------------------------------------------------------------------------------
// Identification
// ----------------------------------------------------------------------------------

/// The maker for a PCI vendor id.
pub fn vendor_from_id(id: u32) -> String {
    match id {
        0x10DE => "NVIDIA".to_string(),
        0x1002 | 0x1022 => "AMD".to_string(),
        0x8086 => "Intel".to_string(),
        0x1414 => "Microsoft".to_string(),
        0x5143 | 0x4D4F_4351 => "Qualcomm".to_string(),
        0x106B => "Apple".to_string(),
        0x13B5 => "Arm".to_string(),
        0x1010 => "Imagination".to_string(),
        other => format!("Vendor 0x{other:04X}"),
    }
}

/// "9-12-2024" (month-day-year, as the registry stores driver dates) to
/// "2024-09-12". Anything else is returned trimmed.
pub fn iso_date(text: &str) -> String {
    let trimmed = text.trim();
    let parts: Vec<&str> = trimmed.split(['-', '/']).collect();
    if let [month, day, year] = parts[..] {
        if let (Ok(m), Ok(d), Ok(y)) = (
            month.parse::<u32>(),
            day.parse::<u32>(),
            year.parse::<u32>(),
        ) {
            if (1..=12).contains(&m) && (1..=31).contains(&d) && (1980..=2200).contains(&y) {
                return format!("{y:04}-{m:02}-{d:02}");
            }
        }
    }
    trimmed.to_string()
}

/// (bus, device, function) from a PCI location value. The registry stores it
/// either as plain text ("PCI bus 1, device 0, function 0") or as a resource
/// reference that ends in "(1,0,0)".
pub fn parse_pci_location(text: &str) -> Option<(u32, u32, u32)> {
    let text = text.trim();
    let numbers: Vec<u32> = if let Some(open) = text.rfind('(') {
        let inner = text[open + 1..].strip_suffix(')')?;
        inner
            .split(',')
            .map(|n| n.trim().parse::<u32>())
            .collect::<Result<_, _>>()
            .ok()?
    } else {
        let lower = text.to_ascii_lowercase();
        let rest = lower.strip_prefix("pci bus ")?;
        let (bus, rest) = rest.split_once(", device ")?;
        let (device, function) = rest.split_once(", function ")?;
        vec![
            bus.trim().parse().ok()?,
            device.trim().parse().ok()?,
            function.trim().parse().ok()?,
        ]
    };
    match numbers[..] {
        [bus, device, function] if bus <= 255 && device <= 31 && function <= 7 => {
            Some((bus, device, function))
        }
        _ => None,
    }
}

/// True when a hardware id such as `PCI\VEN_10DE&DEV_1ABC&SUBSYS_...` names
/// this vendor and device.
pub fn hardware_id_matches(hardware_id: &str, vendor: u32, device: u32) -> bool {
    let upper = hardware_id.to_ascii_uppercase();
    upper.contains(&format!("VEN_{vendor:04X}")) && upper.contains(&format!("DEV_{device:04X}"))
}

/// Whether the adapter is part of the processor. Only answered when the
/// facts are clear; otherwise `None`.
pub fn guess_integrated(vendor: u32, dedicated_bytes: u64, software: bool) -> Option<bool> {
    const GIB: u64 = 1 << 30;
    if software {
        return None;
    }
    if vendor == 0x10DE || dedicated_bytes >= 2 * GIB {
        Some(false)
    } else if matches!(vendor, 0x8086 | 0x1002 | 0x1022) && dedicated_bytes <= GIB / 2 {
        Some(true)
    } else {
        None
    }
}

const DISPLAY_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

#[derive(Default)]
struct DriverFacts {
    version: String,
    date: String,
    /// The value devices use to point at their class entry.
    driver_ref: String,
}

fn driver_facts(vendor: u32, device: u32, name: &str) -> DriverFacts {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    let Ok(class) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(DISPLAY_CLASS) else {
        return DriverFacts::default();
    };
    let mut by_name = None;
    for sub in class.enum_keys().flatten().take(256) {
        // Entries are numbered ("0000"); other subkeys are not devices.
        if sub.len() != 4 || !sub.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(key) = class.open_subkey(&sub) else {
            continue;
        };
        let facts = || DriverFacts {
            version: key
                .get_value::<String, _>("DriverVersion")
                .unwrap_or_default(),
            date: iso_date(&key.get_value::<String, _>("DriverDate").unwrap_or_default()),
            driver_ref: format!("{{4d36e968-e325-11ce-bfc1-08002be10318}}\\{sub}"),
        };
        let matching: String = key.get_value("MatchingDeviceId").unwrap_or_default();
        if hardware_id_matches(&matching, vendor, device) {
            return facts();
        }
        let description: String = key.get_value("DriverDesc").unwrap_or_default();
        if by_name.is_none() && !name.is_empty() && description.eq_ignore_ascii_case(name) {
            by_name = Some(facts());
        }
    }
    by_name.unwrap_or_default()
}

/// The PCI location of the device that uses the class entry `driver_ref`.
fn pci_location(vendor: u32, device: u32, driver_ref: &str) -> Option<(u32, u32, u32)> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    if driver_ref.is_empty() {
        return None;
    }
    let pci = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Enum\PCI")
        .ok()?;
    for id in pci.enum_keys().flatten().take(4096) {
        if !hardware_id_matches(&id, vendor, device) {
            continue;
        }
        let Ok(id_key) = pci.open_subkey(&id) else {
            continue;
        };
        for instance in id_key.enum_keys().flatten().take(64) {
            let Ok(key) = id_key.open_subkey(&instance) else {
                continue;
            };
            let driver: String = key.get_value("Driver").unwrap_or_default();
            if !driver.eq_ignore_ascii_case(driver_ref) {
                continue;
            }
            let location: String = key.get_value("LocationInformation").unwrap_or_default();
            if let Some(found) = parse_pci_location(&location) {
                return Some(found);
            }
        }
    }
    None
}

/// An adapter with the facts the sampler needs beyond the public model.
struct Adapter {
    info: GpuInfo,
    luid: Luid,
    vendor_id: u32,
    device_id: u32,
    bus: Option<u32>,
}

fn describe(desc: &DXGI_ADAPTER_DESC1) -> Adapter {
    let luid: Luid = (desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart);
    let name = from_wide(&desc.Description).trim().to_string();
    // Flag 2 marks a software adapter; the basic render driver also has a fixed id.
    let software = desc.Flags & 2 != 0 || (desc.VendorId == 0x1414 && desc.DeviceId == 0x8C);
    let mut info = GpuInfo {
        id: luid_id(luid),
        name,
        vendor: vendor_from_id(desc.VendorId),
        dedicated_bytes: desc.DedicatedVideoMemory as u64,
        shared_bytes: desc.SharedSystemMemory as u64,
        software,
        integrated: guess_integrated(desc.VendorId, desc.DedicatedVideoMemory as u64, software),
        ..Default::default()
    };
    let mut bus = None;
    if !software {
        let facts = driver_facts(desc.VendorId, desc.DeviceId, &info.name);
        if let Some((b, d, f)) = pci_location(desc.VendorId, desc.DeviceId, &facts.driver_ref) {
            info.location = format!("PCI bus {b}, device {d}, function {f}");
            bus = Some(b);
        }
        info.driver_version = facts.version;
        info.driver_date = facts.date;
        info.extra.push((
            "Hardware id".to_string(),
            format!(
                "VEN_{:04X} DEV_{:04X} REV_{:02X}",
                desc.VendorId, desc.DeviceId, desc.Revision
            ),
        ));
        if desc.DedicatedSystemMemory > 0 {
            info.extra.push((
                "Dedicated system memory".to_string(),
                crate::util::fmt_bytes(desc.DedicatedSystemMemory as u64),
            ));
        }
    }
    Adapter {
        info,
        luid,
        vendor_id: desc.VendorId,
        device_id: desc.DeviceId,
        bus,
    }
}

fn enumerate() -> Vec<Adapter> {
    // SAFETY: plain factory creation; the interface is released when dropped.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut out: Vec<Adapter> = Vec::new();
    for index in 0..64 {
        // SAFETY: enumeration by index; an error means the end of the list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: fills a plain structure.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let described = describe(&desc);
        if !out.iter().any(|a| a.luid == described.luid) {
            out.push(described);
        }
    }
    out
}

/// The graphics adapters of this machine (software adapters included, flagged).
pub fn adapters() -> Vec<GpuInfo> {
    enumerate().into_iter().map(|a| a.info).collect()
}

// ----------------------------------------------------------------------------------
// Live readings
// ----------------------------------------------------------------------------------

/// How often the adapter list may be re-read when a counter names an
/// adapter that is not in it (a driver update or a hot-plugged adapter).
const RESCAN_PERIOD: Duration = Duration::from_secs(60);
/// A vendor library that takes longer than this per reading is asked less often.
const SLOW_LIBRARY: Duration = Duration::from_millis(2);
const SLOW_LIBRARY_PERIOD: Duration = Duration::from_secs(5);

struct Counters {
    query: pdh::Query,
    engine: Option<pdh::Counter>,
    adapter_dedicated: Option<pdh::Counter>,
    adapter_shared: Option<pdh::Counter>,
    process_dedicated: Option<pdh::Counter>,
}

fn open_counters() -> Option<Counters> {
    let mut query = pdh::Query::open().ok()?;
    let engine = query.add(r"\GPU Engine(*)\Utilization Percentage").ok();
    let adapter_dedicated = query.add(r"\GPU Adapter Memory(*)\Dedicated Usage").ok();
    let adapter_shared = query.add(r"\GPU Adapter Memory(*)\Shared Usage").ok();
    let process_dedicated = query.add(r"\GPU Process Memory(*)\Dedicated Usage").ok();
    if engine.is_none() && adapter_dedicated.is_none() {
        return None;
    }
    Some(Counters {
        query,
        engine,
        adapter_dedicated,
        adapter_shared,
        process_dedicated,
    })
}

struct State {
    /// Hardware adapters only, in the order of [`adapters`].
    hardware: Vec<Adapter>,
    luids: Vec<Luid>,
    ignored: Vec<Luid>,
    scanned_at: Instant,
    counters: Option<Counters>,
    aggregator: Aggregator,
    library: Option<nvml::Library>,
    library_note: String,
    /// Library device per hardware adapter.
    library_map: Vec<Option<usize>>,
    library_readings: Vec<nvml::Reading>,
    library_at: Option<Instant>,
    library_period: Duration,
    last: GpuReading,
}

impl State {
    fn new() -> State {
        let (library, library_note) = match nvml::Library::load() {
            Ok(library) => (Some(library), String::new()),
            Err(reason) => (None, reason),
        };
        let mut state = State {
            hardware: Vec::new(),
            luids: Vec::new(),
            ignored: Vec::new(),
            scanned_at: Instant::now(),
            counters: open_counters(),
            aggregator: Aggregator::default(),
            library,
            library_note,
            library_map: Vec::new(),
            library_readings: Vec::new(),
            library_at: None,
            library_period: Duration::ZERO,
            last: GpuReading::default(),
        };
        state.scan();
        state
    }

    /// Reads the adapter list and matches the vendor library's devices to it.
    fn scan(&mut self) {
        let all = enumerate();
        self.scanned_at = Instant::now();
        self.ignored = all
            .iter()
            .filter(|a| a.info.software)
            .map(|a| a.luid)
            .collect();
        self.hardware = all.into_iter().filter(|a| !a.info.software).collect();
        self.luids = self.hardware.iter().map(|a| a.luid).collect();
        self.library_map = self
            .hardware
            .iter()
            .map(|adapter| {
                let library = self.library.as_ref()?;
                // The library only knows the adapters of its own maker.
                if adapter.vendor_id != 0x10DE {
                    return None;
                }
                let same_vendor = self
                    .hardware
                    .iter()
                    .filter(|a| a.vendor_id == adapter.vendor_id)
                    .count();
                nvml::match_device(
                    library.devices(),
                    &adapter.info.name,
                    adapter.bus,
                    adapter.device_id,
                    same_vendor,
                )
            })
            .collect();
        self.library_readings.clear();
        self.library_at = None;
    }

    fn read_library(&mut self) {
        let Some(library) = &self.library else {
            return;
        };
        let due = self
            .library_at
            .is_none_or(|at| at.elapsed() >= self.library_period);
        if !due && self.library_readings.len() == self.hardware.len() {
            return;
        }
        let started = Instant::now();
        self.library_readings.clear();
        self.library_readings.extend(
            self.library_map
                .iter()
                .map(|slot| slot.map(|index| library.read(index)).unwrap_or_default()),
        );
        self.library_at = Some(started);
        self.library_period = if started.elapsed() > SLOW_LIBRARY {
            SLOW_LIBRARY_PERIOD
        } else {
            Duration::ZERO
        };
    }

    fn sample(&mut self) -> GpuReading {
        if self.aggregator.unknown_adapter && self.scanned_at.elapsed() >= RESCAN_PERIOD {
            self.scan();
        }
        self.aggregator.begin(&self.luids, &self.ignored);
        let mut load_known = false;
        if let Some(counters) = &mut self.counters {
            if counters.query.collect() {
                let aggregator = &mut self.aggregator;
                if let Some(counter) = counters.engine {
                    load_known = counters
                        .query
                        .read_array(counter, |name, value| aggregator.engine(name, value))
                        .is_ok();
                }
                if let Some(counter) = counters.adapter_dedicated {
                    let _ = counters.query.read_array(counter, |name, value| {
                        aggregator.adapter_memory(name, value, false);
                    });
                }
                if let Some(counter) = counters.adapter_shared {
                    let _ = counters.query.read_array(counter, |name, value| {
                        aggregator.adapter_memory(name, value, true);
                    });
                }
                if let Some(counter) = counters.process_dedicated {
                    let _ = counters.query.read_array(counter, |name, value| {
                        aggregator.process_memory(name, value);
                    });
                }
            }
        }
        self.read_library();

        let mut reading = GpuReading {
            gpus: Vec::with_capacity(self.hardware.len()),
            per_process: self.aggregator.per_process(),
        };
        for (index, adapter) in self.hardware.iter().enumerate() {
            let (dedicated_used, shared_used) = self.aggregator.adapter_bytes(index);
            let mut sample = GpuSample {
                id: adapter.info.id.clone(),
                dedicated_used,
                shared_used,
                ..Default::default()
            };
            if load_known {
                (sample.load_pct, sample.engines) = self.aggregator.adapter_load(index);
            } else if let Some(before) = self.last.gpus.iter().find(|g| g.id == sample.id) {
                // The load counter gave nothing this time (it does that now and
                // then): keep the previous figures rather than drop to zero.
                sample.load_pct = before.load_pct;
                sample.engines.clone_from(&before.engines);
            }
            let from_library = self
                .library_map
                .get(index)
                .copied()
                .flatten()
                .and_then(|_| self.library_readings.get(index));
            match from_library {
                Some(values) => {
                    sample.temperature_c = values.temperature_c;
                    sample.fan_pct = values.fan_pct;
                    sample.power_w = values.power_w;
                    sample.clock_mhz = values.clock_mhz;
                    if values.temperature_c.is_none() {
                        sample.temperature_note =
                            "the vendor library reports no temperature for this adapter"
                                .to_string();
                    }
                }
                None => {
                    sample.temperature_note = if self.library_note.is_empty() {
                        nvml::NO_LIBRARY_NOTE.to_string()
                    } else {
                        self.library_note.clone()
                    };
                }
            }
            reading.gpus.push(sample);
        }
        if !load_known {
            // Same for the per-process load.
            for (pid, before) in &self.last.per_process {
                if let Some(now) = reading.per_process.get_mut(pid) {
                    now.pct = before.pct;
                }
            }
        }
        self.last.gpus.clone_from(&reading.gpus);
        self.last.per_process.clone_from(&reading.per_process);
        reading
    }
}

/// Live adapter readings. The counters, the adapter list and the vendor
/// library are opened on the first call and kept.
#[derive(Default)]
pub struct GpuSampler {
    state: Option<State>,
}

impl GpuSampler {
    pub fn new() -> GpuSampler {
        GpuSampler::default()
    }

    pub fn sample(&mut self) -> GpuReading {
        self.state.get_or_insert_with(State::new).sample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Luid = (0, 0xABCD);
    const B: Luid = (0x1, 0x0002_0000);

    #[test]
    fn engine_instance_names() {
        let parsed = parse_engine_instance(
            "pid_1234_luid_0x00000000_0x0000ABCD_phys_0_eng_3_engtype_VideoDecode",
        )
        .expect("valid name");
        assert_eq!(
            parsed,
            EngineInstance {
                pid: 1234,
                luid: A,
                phys: 0,
                engine: 3,
                kind: "VideoDecode",
            }
        );
        // Engine types may contain underscores and spaces.
        let odd = parse_engine_instance(
            "pid_8_luid_0x00000001_0x00020000_phys_1_eng_12_engtype_Compute_0",
        )
        .expect("valid name");
        assert_eq!(
            (odd.luid, odd.phys, odd.engine, odd.kind),
            (B, 1, 12, "Compute_0")
        );
        let spaced = parse_engine_instance("pid_8_luid_0x0_0xa_phys_0_eng_0_engtype_GDI Render")
            .expect("valid name");
        assert_eq!((spaced.luid, spaced.kind), ((0, 10), "GDI Render"));

        for bad in [
            "",
            "pid",
            "pid_",
            "pid_x_luid_0x00000000_0x0000ABCD_phys_0_eng_3_engtype_3D",
            "pid_-1_luid_0x00000000_0x0000ABCD_phys_0_eng_3_engtype_3D",
            "pid_99999999999_luid_0x00000000_0x0000ABCD_phys_0_eng_3_engtype_3D",
            "pid_1_luid_00000000_0x0000ABCD_phys_0_eng_3_engtype_3D",
            "pid_1_luid_0x00000000_phys_0_eng_3_engtype_3D",
            "pid_1_luid_0x00000000_0xZZZZ_phys_0_eng_3_engtype_3D",
            "pid_1_luid_0x000000000_0x0000ABCD_phys_0_eng_3_engtype_3D",
            "pid_1_luid_0x00000000_0x0000ABCD_phys_0_eng_3",
            "pid_1_luid_0x00000000_0x0000ABCD_phys_0_eng_3_engtype_",
            "pid_1_luid_0x00000000_0x0000ABCD_phys_0_eng__engtype_3D",
            "pid_1_luid_0x00000000_0x0000ABCD_eng_3_engtype_3D",
            "luid_0x00000000_0x0000ABCD_phys_0",
            "_Total",
            "pid_1_luid_0x_0x_phys_0_eng_3_engtype_3D",
            "pid_1_LUID_0x00000000_0x0000ABCD_phys_0_eng_3_engtype_3D",
            "\u{FFFD}\u{FFFD}\u{FFFD}",
        ] {
            assert_eq!(parse_engine_instance(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn memory_instance_names() {
        assert_eq!(
            parse_adapter_instance("luid_0x00000000_0x0000ABCD_phys_0"),
            Some((A, 0))
        );
        assert_eq!(
            parse_adapter_instance("luid_0x00000001_0x00020000_phys_2"),
            Some((B, 2))
        );
        assert_eq!(
            parse_process_instance("pid_77_luid_0x00000000_0x0000ABCD_phys_0"),
            Some((77, A))
        );
        for bad in [
            "",
            "luid",
            "luid_0x00000000",
            "luid_0x00000000_0x0000ABCD",
            "luid_0x00000000_0x0000ABCD_phys_",
            "luid_0x00000000_0x0000ABCD_phys_0_extra",
            "pid_77_luid_0x00000000_0x0000ABCD_phys_0",
        ] {
            assert_eq!(parse_adapter_instance(bad), None, "{bad:?}");
        }
        for bad in [
            "",
            "pid_77",
            "pid_77_luid_0x00000000_0x0000ABCD",
            "pid_77_luid_0x00000000_0x0000ABCD_phys_0_eng_1_engtype_3D",
            "luid_0x00000000_0x0000ABCD_phys_0",
        ] {
            assert_eq!(parse_process_instance(bad), None, "{bad:?}");
        }
        assert_eq!(luid_id(A), "0x00000000_0x0000ABCD");
        assert_eq!(luid_id(B), "0x00000001_0x00020000");
        // The id is what the counters print, so it parses back.
        let name = format!("luid_{}_phys_0", luid_id(B));
        assert_eq!(parse_adapter_instance(&name), Some((B, 0)));
    }

    #[test]
    fn engine_labels() {
        assert_eq!(engine_label("3D"), "3D");
        assert_eq!(engine_label("Copy"), "Copy");
        assert_eq!(engine_label("VideoDecode"), "Video Decode");
        assert_eq!(engine_label("VideoEncode"), "Video Encode");
        assert_eq!(engine_label("VideoProcessing"), "Video Processing");
        assert_eq!(engine_label("Compute_0"), "Compute");
        assert_eq!(engine_label("Compute_12"), "Compute");
        assert_eq!(engine_label("Cuda"), "CUDA");
        assert_eq!(engine_label("LegacyOverlay"), "Legacy Overlay");
        assert_eq!(engine_label("GDI Render"), "GDI Render");
        assert_eq!(engine_label("High_Priority3D"), "High Priority3D");
        assert_eq!(engine_label(""), "Other");
        assert_eq!(engine_label("_7"), "Other");
        assert!(label_rank("3D") < label_rank("Copy"));
        assert!(label_rank("Compute") < label_rank("Something Else"));
    }

    fn engine_name(pid: u32, luid: Luid, engine: u32, kind: &str) -> String {
        format!(
            "pid_{pid}_luid_{}_phys_0_eng_{engine}_engtype_{kind}",
            luid_id(luid)
        )
    }

    #[test]
    fn instances_add_up_per_adapter_and_process() {
        let mut agg = Aggregator::default();
        agg.begin(&[A, B], &[(9, 9)]);
        // Adapter A: two processes on the same 3D engine, one on video decode,
        // a second 3D engine that is nearly idle.
        agg.engine(&engine_name(100, A, 0, "3D"), 30.0);
        agg.engine(&engine_name(200, A, 0, "3D"), 12.5);
        agg.engine(&engine_name(100, A, 1, "3D"), 2.0);
        agg.engine(&engine_name(100, A, 5, "VideoDecode"), 8.0);
        agg.engine(&engine_name(300, A, 2, "Copy"), 0.0);
        // Adapter B: one busy compute engine.
        agg.engine(&engine_name(100, B, 3, "Compute_0"), 55.0);
        agg.engine(&engine_name(100, B, 4, "Compute_1"), 5.0);
        // Ignored and unknown adapters, malformed names, bad values.
        agg.engine(&engine_name(100, (9, 9), 0, "3D"), 99.0);
        agg.engine("nonsense", 50.0);
        agg.engine(&engine_name(400, A, 5, "VideoDecode"), f64::NAN);
        assert!(!agg.unknown_adapter);
        agg.engine(&engine_name(100, (7, 7), 0, "3D"), 99.0);
        assert!(agg.unknown_adapter);

        agg.adapter_memory("luid_0x00000000_0x0000ABCD_phys_0", 1000.0, false);
        agg.adapter_memory("luid_0x00000000_0x0000ABCD_phys_1", 500.0, false);
        agg.adapter_memory("luid_0x00000000_0x0000ABCD_phys_0", 70.0, true);
        agg.adapter_memory("luid_0x00000001_0x00020000_phys_0", -5.0, false);
        agg.adapter_memory("garbage", 1e12, false);
        agg.process_memory("pid_100_luid_0x00000000_0x0000ABCD_phys_0", 300.0);
        agg.process_memory("pid_100_luid_0x00000001_0x00020000_phys_0", 200.0);
        agg.process_memory("pid_555_luid_0x00000000_0x0000ABCD_phys_0", 40.0);
        agg.process_memory("pid_555_luid_0x00000009_0x00000009_phys_0", 4000.0);

        let (load_a, engines_a) = agg.adapter_load(0);
        assert_eq!(load_a, 42.5);
        let names: Vec<&str> = engines_a.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["3D", "Copy", "Video Decode"]);
        assert_eq!(engines_a[0].pct, 42.5);
        assert_eq!(engines_a[1].pct, 0.0);
        assert_eq!(engines_a[2].pct, 8.0);
        let (load_b, engines_b) = agg.adapter_load(1);
        assert_eq!(load_b, 55.0);
        assert_eq!(engines_b.len(), 1, "identical engine types share one line");
        assert_eq!(engines_b[0].name, "Compute");
        assert_eq!(agg.adapter_load(7), (0.0, Vec::new()));

        assert_eq!(agg.adapter_bytes(0), (1500, 70));
        assert_eq!(agg.adapter_bytes(1), (0, 0));
        assert_eq!(agg.adapter_bytes(9), (0, 0));

        let per_process = agg.per_process();
        // Process 100: busiest engine on A (30) plus busiest on B (55).
        assert_eq!(
            per_process[&100],
            GpuProcess {
                pct: 85.0,
                memory: 500
            }
        );
        assert_eq!(
            per_process[&200],
            GpuProcess {
                pct: 12.5,
                memory: 0
            }
        );
        assert_eq!(
            per_process[&555],
            GpuProcess {
                pct: 0.0,
                memory: 40
            }
        );
        assert!(!per_process.contains_key(&300), "no load and no memory");
        assert!(!per_process.contains_key(&400));

        // A new reading starts from nothing.
        agg.begin(&[A], &[]);
        assert_eq!(agg.adapter_load(0), (0.0, Vec::new()));
        assert!(agg.per_process().is_empty());
        assert!(!agg.unknown_adapter);
    }

    #[test]
    fn sums_never_leave_the_range() {
        let mut agg = Aggregator::default();
        agg.begin(&[A], &[]);
        for pid in 0..50 {
            agg.engine(&engine_name(pid, A, 0, "3D"), 90.0);
            agg.engine(&engine_name(pid, A, 0, "3D"), 1e300);
            agg.engine(&engine_name(pid, A, 1, "3D"), -40.0);
        }
        let (load, engines) = agg.adapter_load(0);
        assert_eq!(load, 100.0);
        assert!(engines.iter().all(|e| (0.0..=100.0).contains(&e.pct)));
        assert!(agg
            .per_process()
            .values()
            .all(|p| (0.0..=100.0).contains(&p.pct)));
        agg.adapter_memory("luid_0x00000000_0x0000ABCD_phys_0", 1e300, false);
        agg.adapter_memory("luid_0x00000000_0x0000ABCD_phys_0", 1e300, false);
        assert_eq!(agg.adapter_bytes(0).0, u64::MAX);
    }

    #[test]
    fn identification_helpers() {
        assert_eq!(vendor_from_id(0x10DE), "NVIDIA");
        assert_eq!(vendor_from_id(0x1002), "AMD");
        assert_eq!(vendor_from_id(0x8086), "Intel");
        assert_eq!(vendor_from_id(0xBEEF), "Vendor 0xBEEF");

        assert_eq!(iso_date("9-12-2024"), "2024-09-12");
        assert_eq!(iso_date(" 11/3/2025 "), "2025-11-03");
        assert_eq!(iso_date("13-40-2024"), "13-40-2024");
        assert_eq!(iso_date(""), "");
        assert_eq!(iso_date("yesterday"), "yesterday");
        assert_eq!(iso_date("1-2-3-4"), "1-2-3-4");

        assert_eq!(
            parse_pci_location(
                r"@system32\drivers\pci.sys,#65536;PCI bus %1, device %2, function %3;(1,0,0)"
            ),
            Some((1, 0, 0))
        );
        assert_eq!(
            parse_pci_location("PCI bus 3, device 2, function 1"),
            Some((3, 2, 1))
        );
        assert_eq!(parse_pci_location("(1,0)"), None);
        assert_eq!(parse_pci_location("(1,0,0,0)"), None);
        assert_eq!(parse_pci_location("(999,0,0)"), None);
        assert_eq!(parse_pci_location("(a,b,c)"), None);
        assert_eq!(parse_pci_location("(1,0,0"), None);
        assert_eq!(parse_pci_location(""), None);
        assert_eq!(parse_pci_location("somewhere on the board"), None);

        assert!(hardware_id_matches(
            r"pci\ven_10de&dev_1abc&subsys_00000000",
            0x10DE,
            0x1ABC
        ));
        assert!(!hardware_id_matches(
            r"PCI\VEN_10DE&DEV_1ABD",
            0x10DE,
            0x1ABC
        ));
        assert!(!hardware_id_matches("", 0x10DE, 0x1ABC));

        let gib = 1u64 << 30;
        assert_eq!(guess_integrated(0x10DE, 8 * gib, false), Some(false));
        assert_eq!(guess_integrated(0x8086, 128 << 20, false), Some(true));
        assert_eq!(guess_integrated(0x8086, 8 * gib, false), Some(false));
        assert_eq!(
            guess_integrated(0x1002, gib, false),
            None,
            "could be either"
        );
        assert_eq!(guess_integrated(0x1414, 0, true), None);
    }

    #[test]
    fn live_adapters_smoke() {
        let list = adapters();
        let hardware = list.iter().filter(|a| !a.software).count();
        for adapter in &list {
            assert!(!adapter.id.is_empty());
            assert!(!adapter.vendor.is_empty());
            // The id must be what the counter instance names carry.
            let name = format!("luid_{}_phys_0", adapter.id);
            assert!(parse_adapter_instance(&name).is_some());
        }
        let mut ids: Vec<&str> = list.iter().map(|a| a.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), list.len(), "adapter ids are unique");
        println!(
            "gpu adapters: {} total, {hardware} hardware, driver version known {}, driver date known {}, location known {}",
            list.len(),
            list.iter().filter(|a| !a.software && !a.driver_version.is_empty()).count(),
            list.iter().filter(|a| !a.software && !a.driver_date.is_empty()).count(),
            list.iter().filter(|a| !a.software && !a.location.is_empty()).count(),
        );
    }

    #[test]
    fn live_sampler_smoke() {
        let hardware: Vec<GpuInfo> = adapters().into_iter().filter(|a| !a.software).collect();
        let mut sampler = GpuSampler::new();
        let _ = sampler.sample();
        std::thread::sleep(Duration::from_millis(1100));
        let second = sampler.sample();
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        let mut reading = GpuReading::default();
        for _ in 0..20 {
            reading = sampler.sample();
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;

        for r in [&second, &reading] {
            assert_eq!(r.gpus.len(), hardware.len());
            for (sample, info) in r.gpus.iter().zip(&hardware) {
                assert_eq!(sample.id, info.id, "same order as the adapter list");
                assert!((0.0..=100.0).contains(&sample.load_pct));
                assert!(sample
                    .engines
                    .iter()
                    .all(|e| (0.0..=100.0).contains(&e.pct)));
                assert!(sample.engines.iter().all(|e| !e.name.is_empty()));
                assert!(sample.temperature_c.is_some() || !sample.temperature_note.is_empty());
                assert!(sample
                    .temperature_c
                    .is_none_or(|t| (0.0..=150.0).contains(&t)));
            }
            assert!(r
                .per_process
                .values()
                .all(|p| (0.0..=100.0).contains(&p.pct)));
        }
        let state = sampler.state.as_ref().expect("state exists after sampling");
        println!(
            "gpu sample: adapters {}, counter set {}, engine types {}, memory counters {}, processes with use {}, temperature {}, avg {ms:.3} ms per call",
            second.gpus.len(),
            if state.counters.is_some() { "yes" } else { "no" },
            second.gpus.iter().map(|g| g.engines.len()).sum::<usize>(),
            if second.gpus.iter().any(|g| g.dedicated_used + g.shared_used > 0) { "yes" } else { "no" },
            second.per_process.len(),
            if second.gpus.iter().any(|g| g.temperature_c.is_some()) { "yes" } else { "no" },
        );
    }
}
