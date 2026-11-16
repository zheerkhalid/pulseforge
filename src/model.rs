//! The data model shared by the collectors, the demo data, the reports and the
//! user interface.
//!
//! Conventions:
//! * sizes are bytes (`u64`), rates are bytes per second (`f64`);
//! * temperatures are degrees Celsius (`f32`), converted for display only;
//! * percentages are `0.0..=100.0`;
//! * `Option::None` means "this machine does not expose the reading"; the
//!   matching `*_note` string (where present) says why in plain language;
//! * every struct has an `extra` list of `(label, value)` rows for anything
//!   a collector can read that has no dedicated field. The UI and the reports
//!   show these rows as they are.

use serde::{Deserialize, Serialize};

/// The honest answer for readings that Windows only exposes through a kernel driver.
pub const NO_DRIVER: &str = "not available without a driver";
/// For readings that need the program to run elevated.
pub const NEEDS_ADMIN: &str = "needs administrator rights";

pub type Extra = Vec<(String, String)>;

// ----------------------------------------------------------------------------------
// CPU
// ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheInfo {
    /// 1, 2 or 3.
    pub level: u8,
    /// "Data", "Instruction" or "Unified".
    pub kind: String,
    /// Size of one cache of this kind.
    pub size_bytes: u64,
    /// How many caches of this kind the machine has.
    pub count: u32,
    pub line_size: u32,
    pub associativity: u32,
}

/// Facts about the processor that do not change while the program runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuInfo {
    /// The brand string, trimmed ("Example Core 7 1470K").
    pub name: String,
    pub vendor: String,
    pub architecture: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub packages: u32,
    pub cores: u32,
    pub threads: u32,
    /// Hybrid processors: number of performance / efficiency cores.
    pub performance_cores: Option<u32>,
    pub efficiency_cores: Option<u32>,
    pub base_mhz: Option<u32>,
    pub max_mhz: Option<u32>,
    pub caches: Vec<CacheInfo>,
    /// Instruction-set features in display form ("AVX2", "AES", "SHA", ...).
    pub features: Vec<String>,
    /// Hardware virtualisation support (VT-x / AMD-V) as reported by Windows.
    pub virtualization: Option<bool>,
    /// True when Windows itself runs under a hypervisor.
    pub hypervisor: bool,
    /// Socket designation from SMBIOS, if known.
    pub socket: String,
    pub microcode: String,
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuSample {
    pub total_pct: f32,
    /// Share of `total_pct` spent in the kernel.
    pub kernel_pct: f32,
    /// One entry per logical processor.
    pub per_core_pct: Vec<f32>,
    /// Current effective clock per logical processor, when Windows reports it.
    pub per_core_mhz: Vec<f32>,
    /// Average effective clock.
    pub current_mhz: Option<f32>,
    /// Package / ACPI temperature when exposed without a driver.
    pub temperature_c: Option<f32>,
    /// Why `temperature_c` is empty, or where it comes from.
    pub temperature_note: String,
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
    pub context_switches_per_s: Option<f64>,
    pub interrupts_per_s: Option<f64>,
}

// ----------------------------------------------------------------------------------
// Memory
// ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySample {
    pub total: u64,
    /// In use (total minus available).
    pub used: u64,
    pub available: u64,
    /// File cache and standby pages.
    pub cached: u64,
    pub free: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub pagefile_used: u64,
    pub pagefile_total: u64,
    pub kernel_paged: u64,
    pub kernel_nonpaged: u64,
    pub compressed: Option<u64>,
    pub modified: Option<u64>,
    pub standby: Option<u64>,
    pub hardware_reserved: Option<u64>,
    pub page_faults_per_s: Option<f64>,
}

impl MemorySample {
    pub fn used_pct(&self) -> f32 {
        pct(self.used, self.total)
    }
    pub fn commit_pct(&self) -> f32 {
        pct(self.commit_used, self.commit_limit)
    }
    pub fn pagefile_pct(&self) -> f32 {
        pct(self.pagefile_used, self.pagefile_total)
    }
}

/// `part / whole` as a percentage, `0.0` when `whole` is zero.
pub fn pct(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64 * 100.0).clamp(0.0, 100.0) as f32
    }
}

/// One memory slot from SMBIOS (type 17).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryModule {
    /// "DIMM A1", "ChannelB-DIMM0", ...
    pub slot: String,
    pub bank: String,
    /// False for an empty slot (then only `slot` and `bank` are meaningful).
    pub populated: bool,
    pub size_bytes: u64,
    /// "DDR4", "DDR5", "LPDDR5", ...
    pub kind: String,
    /// "DIMM", "SODIMM", ...
    pub form_factor: String,
    /// Rated speed in MT/s (the number commonly quoted as MHz).
    pub speed_mhz: Option<u32>,
    pub configured_mhz: Option<u32>,
    pub manufacturer: String,
    pub part_number: String,
    pub serial: String,
    pub voltage_mv: Option<u32>,
    pub rank: Option<u8>,
    pub data_width: Option<u16>,
    pub extra: Extra,
}

// ----------------------------------------------------------------------------------
// GPU
// ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuInfo {
    /// Stable key that joins a `GpuSample` to its adapter (the adapter LUID as hex).
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub dedicated_bytes: u64,
    pub shared_bytes: u64,
    pub driver_version: String,
    pub driver_date: String,
    /// True for software adapters (the basic render driver).
    pub software: bool,
    pub integrated: Option<bool>,
    /// PCI location or bus description if known.
    pub location: String,
    pub extra: Extra,
}

/// Load of one GPU engine type ("3D", "Copy", "Video Decode", ...).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuEngine {
    pub name: String,
    pub pct: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuSample {
    /// Matches `GpuInfo::id`.
    pub id: String,
    /// The busiest engine, which is what people mean by "GPU usage".
    pub load_pct: f32,
    pub engines: Vec<GpuEngine>,
    pub dedicated_used: u64,
    pub shared_used: u64,
    pub temperature_c: Option<f32>,
    pub temperature_note: String,
    pub power_w: Option<f32>,
    pub fan_pct: Option<f32>,
    pub fan_rpm: Option<u32>,
    pub clock_mhz: Option<f32>,
}

// ----------------------------------------------------------------------------------
// Storage
// ----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    Good,
    /// Worth watching: reallocated sectors, high wear, high temperature, ...
    Caution,
    /// The drive reports failure or has exhausted its spare capacity.
    Bad,
    #[default]
    Unknown,
}

impl HealthStatus {
    pub fn label(self) -> &'static str {
        match self {
            HealthStatus::Good => "Good",
            HealthStatus::Caution => "Caution",
            HealthStatus::Bad => "Bad",
            HealthStatus::Unknown => "Unknown",
        }
    }
    /// The worse of two states (`Unknown` never hides a known state).
    pub fn worst(self, other: HealthStatus) -> HealthStatus {
        fn rank(h: HealthStatus) -> u8 {
            match h {
                HealthStatus::Unknown => 0,
                HealthStatus::Good => 1,
                HealthStatus::Caution => 2,
                HealthStatus::Bad => 3,
            }
        }
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// One SMART attribute or NVMe health-log field, already decoded.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartAttribute {
    /// ATA attribute id; `None` for NVMe fields.
    pub id: Option<u8>,
    pub name: String,
    /// Normalised current / worst / threshold (ATA only).
    pub current: Option<u8>,
    pub worst: Option<u8>,
    pub threshold: Option<u8>,
    /// The raw value as a number where that makes sense.
    pub raw: u64,
    /// The value as a person would read it ("41 °C", "12,480 hours", "3.2 TB").
    pub display: String,
    pub status: HealthStatus,
    /// One plain sentence: what the attribute means and whether this value is fine.
    pub explanation: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Partition {
    pub number: u32,
    /// "EFI system", "Basic data", "Recovery", ...
    pub kind: String,
    pub offset: u64,
    pub size: u64,
    /// "C:" when the partition is mounted with a letter.
    pub letter: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Volume {
    /// "C:" or a mount path.
    pub mount: String,
    pub label: String,
    pub filesystem: String,
    pub total: u64,
    pub free: u64,
    /// Index of the physical drive that holds it, if known.
    pub drive_index: Option<u32>,
    pub bitlocker: Option<bool>,
    /// "Fixed", "Removable", "Network", "Optical".
    pub kind: String,
}

impl Volume {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
    pub fn used_pct(&self) -> f32 {
        pct(self.used(), self.total)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DriveInfo {
    /// N in `\\.\PhysicalDriveN`.
    pub index: u32,
    pub model: String,
    pub firmware: String,
    pub serial: String,
    /// "NVMe", "SATA", "USB", "SD", "RAID", "Virtual", ...
    pub interface: String,
    /// "SSD", "HDD" or "" when unknown.
    pub media: String,
    pub size_bytes: u64,
    pub removable: bool,
    /// "GPT", "MBR" or "RAW".
    pub partition_style: String,
    pub partitions: Vec<Partition>,
    pub rotation_rpm: Option<u32>,

    /// Overall verdict, in plain words.
    pub health: HealthStatus,
    /// Remaining life in percent where the drive reports it (100 = new).
    pub health_pct: Option<u8>,
    /// One or two plain sentences that summarise the state of the drive.
    pub health_summary: String,
    pub temperature_c: Option<f32>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub total_written_bytes: Option<u64>,
    pub total_read_bytes: Option<u64>,
    /// "NVMe health log", "ATA SMART", "Windows storage health" or "".
    pub smart_source: String,
    /// Why SMART details are missing (for example `NEEDS_ADMIN`), empty if present.
    pub smart_note: String,
    pub attributes: Vec<SmartAttribute>,
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskSample {
    /// Matches `DriveInfo::index`.
    pub index: u32,
    pub read_bps: f64,
    pub write_bps: f64,
    /// Share of the interval the drive was busy.
    pub active_pct: f32,
    pub queue_depth: Option<f32>,
}

// ----------------------------------------------------------------------------------
// Network
// ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetSample {
    /// Friendly name ("Ethernet", "Wi-Fi").
    pub name: String,
    /// Hardware description.
    pub description: String,
    /// "Ethernet", "Wi-Fi", "Cellular", "Virtual", ...
    pub kind: String,
    pub up: bool,
    pub link_bps: u64,
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// Totals since boot.
    pub rx_total: u64,
    pub tx_total: u64,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub mac: String,
    pub gateway: String,
    pub dns: Vec<String>,
    pub extra: Extra,
}

// ----------------------------------------------------------------------------------
// Battery
// ----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatteryState {
    Charging,
    Discharging,
    /// On mains power and neither charging nor discharging.
    Idle,
    Full,
    #[default]
    Unknown,
}

impl BatteryState {
    pub fn label(self) -> &'static str {
        match self {
            BatteryState::Charging => "Charging",
            BatteryState::Discharging => "On battery",
            BatteryState::Idle => "Plugged in",
            BatteryState::Full => "Fully charged",
            BatteryState::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BatteryInfo {
    pub name: String,
    pub manufacturer: String,
    pub serial: String,
    /// "Lithium-ion", "Lithium polymer", ...
    pub chemistry: String,
    pub design_mwh: Option<u32>,
    pub full_mwh: Option<u32>,
    pub remaining_mwh: Option<u32>,
    pub percent: f32,
    pub state: BatteryState,
    pub ac_online: bool,
    /// Positive while charging, negative while discharging.
    pub rate_mw: Option<i32>,
    pub voltage_mv: Option<u32>,
    pub cycle_count: Option<u32>,
    /// Capacity lost against the design capacity, in percent.
    pub wear_pct: Option<f32>,
    /// Estimated seconds until empty (discharging) or full (charging).
    pub seconds_left: Option<u32>,
    pub temperature_c: Option<f32>,
    pub manufacture_date: String,
    pub extra: Extra,
}

// ----------------------------------------------------------------------------------
// System inventory
// ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInfo {
    // Computer (SMBIOS type 1)
    pub manufacturer: String,
    pub product: String,
    pub family: String,
    pub sku: String,
    pub system_serial: String,
    pub uuid: String,
    /// "Desktop", "Notebook", "Mini PC", ...
    pub chassis: String,
    // Baseboard (SMBIOS type 2)
    pub board_manufacturer: String,
    pub board_product: String,
    pub board_version: String,
    pub board_serial: String,
    // Firmware (SMBIOS type 0)
    pub bios_vendor: String,
    pub bios_version: String,
    pub bios_date: String,
    /// "UEFI" or "Legacy BIOS".
    pub firmware_mode: String,
    pub secure_boot: Option<bool>,
    pub tpm_present: Option<bool>,
    /// "2.0", "1.2".
    pub tpm_version: String,
    pub tpm_manufacturer: String,
    // Windows
    pub os_name: String,
    pub os_edition: String,
    /// "24H2".
    pub os_display_version: String,
    /// "26100.2314".
    pub os_build: String,
    pub os_arch: String,
    /// ISO date, "2025-03-14".
    pub os_install_date: String,
    pub computer_name: String,
    pub user_name: String,
    /// "2026-11-16 08:12".
    pub boot_time: String,
    pub time_zone: String,
    pub locale: String,
    pub memory_slots_total: Option<u32>,
    pub memory_slots_used: Option<u32>,
    pub memory_max_bytes: Option<u64>,
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayInfo {
    /// Monitor name from EDID ("Studio 27Q") or the device name.
    pub name: String,
    pub manufacturer: String,
    /// The adapter it is connected to.
    pub adapter: String,
    pub primary: bool,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f32,
    pub bits_per_pixel: u32,
    /// Scaling in percent (100, 125, 150, ...).
    pub scale_pct: u32,
    pub hdr_supported: Option<bool>,
    pub hdr_enabled: Option<bool>,
    /// "DisplayPort", "HDMI", "Internal", ...
    pub connection: String,
    pub position: (i32, i32),
    /// "Landscape", "Portrait", ...
    pub orientation: String,
    /// Physical size in millimetres from EDID.
    pub size_mm: Option<(u32, u32)>,
    pub diagonal_inches: Option<f32>,
    pub extra: Extra,
}

// ----------------------------------------------------------------------------------
// Sensors
// ----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SensorKind {
    #[default]
    Temperature,
    Fan,
    Voltage,
    Power,
    Load,
    Clock,
    Current,
    Other,
}

impl SensorKind {
    pub fn label(self) -> &'static str {
        match self {
            SensorKind::Temperature => "Temperature",
            SensorKind::Fan => "Fan",
            SensorKind::Voltage => "Voltage",
            SensorKind::Power => "Power",
            SensorKind::Load => "Load",
            SensorKind::Clock => "Clock",
            SensorKind::Current => "Current",
            SensorKind::Other => "Other",
        }
    }
    /// The unit readings of this kind are stored in.
    pub fn unit(self) -> &'static str {
        match self {
            SensorKind::Temperature => "°C",
            SensorKind::Fan => "RPM",
            SensorKind::Voltage => "V",
            SensorKind::Power => "W",
            SensorKind::Load => "%",
            SensorKind::Clock => "MHz",
            SensorKind::Current => "A",
            SensorKind::Other => "",
        }
    }
}

/// One line in the sensors list. Readings that the machine does not expose are
/// listed too, with `value: None` and the reason in `note`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SensorReading {
    /// The device it belongs to ("Processor", "Drive 0", "Battery", "ACPI").
    pub group: String,
    pub name: String,
    pub kind: SensorKind,
    /// In the unit of `kind` (see [`SensorKind::unit`]).
    pub value: Option<f64>,
    /// Where it comes from ("ACPI thermal zone", "NVMe health log", "NVML", ...).
    pub source: String,
    /// Why it is missing, or a remark about its accuracy.
    pub note: String,
}

// ----------------------------------------------------------------------------------
// Processes
// ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessSample {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    /// Share of the whole machine (all cores = 100).
    pub cpu_pct: f32,
    /// Private working set.
    pub memory: u64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    /// Not measurable per process without a driver or ETW session: stays `None`.
    pub net_bps: Option<f64>,
    pub gpu_pct: Option<f32>,
    pub gpu_memory: Option<u64>,
    pub threads: u32,
    pub handles: u32,
    /// Seconds since the process started.
    pub running_s: u64,
    pub user: String,
}

// ----------------------------------------------------------------------------------
// Snapshots
// ----------------------------------------------------------------------------------

/// Everything that is read once (and again on request).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inventory {
    pub cpu: CpuInfo,
    pub memory_modules: Vec<MemoryModule>,
    pub gpus: Vec<GpuInfo>,
    pub system: SystemInfo,
    pub displays: Vec<DisplayInfo>,
}

/// Everything that is read slowly (about once a minute): drive health,
/// volumes and their usage.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slow {
    pub drives: Vec<DriveInfo>,
    pub volumes: Vec<Volume>,
}

/// Everything that is read on every tick.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sample {
    /// Seconds since the Unix epoch (UTC).
    pub time: u64,
    pub uptime_s: u64,
    pub cpu: CpuSample,
    pub memory: MemorySample,
    pub gpus: Vec<GpuSample>,
    pub disks: Vec<DiskSample>,
    pub net: Vec<NetSample>,
    pub battery: Option<BatteryInfo>,
    pub sensors: Vec<SensorReading>,
    pub processes: Vec<ProcessSample>,
}

impl Sample {
    /// Download / upload rate summed over all real adapters.
    pub fn net_totals(&self) -> (f64, f64) {
        self.net
            .iter()
            .filter(|n| n.kind != "Virtual" && n.kind != "Loopback")
            .fold((0.0, 0.0), |(rx, tx), n| (rx + n.rx_bps, tx + n.tx_bps))
    }

    /// Read / write rate summed over all drives.
    pub fn disk_totals(&self) -> (f64, f64) {
        self.disks
            .iter()
            .fold((0.0, 0.0), |(r, w), d| (r + d.read_bps, w + d.write_bps))
    }

    /// The busiest hardware GPU.
    pub fn gpu_load(&self) -> Option<f32> {
        self.gpus
            .iter()
            .map(|g| g.load_pct)
            .fold(None, |a: Option<f32>, b| Some(a.map_or(b, |a| a.max(b))))
    }

    /// The hottest GPU reading.
    pub fn gpu_temp(&self) -> Option<f32> {
        self.gpus
            .iter()
            .filter_map(|g| g.temperature_c)
            .fold(None, |a: Option<f32>, b| Some(a.map_or(b, |a| a.max(b))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_are_safe() {
        assert_eq!(pct(0, 0), 0.0);
        assert_eq!(pct(50, 100), 50.0);
        assert_eq!(pct(200, 100), 100.0);
        let m = MemorySample {
            total: 16 << 30,
            used: 4 << 30,
            ..Default::default()
        };
        assert_eq!(m.used_pct(), 25.0);
        assert_eq!(m.commit_pct(), 0.0);
    }

    #[test]
    fn worst_health_wins() {
        use HealthStatus::*;
        assert_eq!(Good.worst(Caution), Caution);
        assert_eq!(Bad.worst(Good), Bad);
        assert_eq!(Unknown.worst(Good), Good);
        assert_eq!(Good.worst(Unknown), Good);
    }

    #[test]
    fn totals() {
        let s = Sample {
            net: vec![
                NetSample {
                    kind: "Ethernet".into(),
                    rx_bps: 10.0,
                    tx_bps: 1.0,
                    ..Default::default()
                },
                NetSample {
                    kind: "Virtual".into(),
                    rx_bps: 99.0,
                    ..Default::default()
                },
            ],
            gpus: vec![
                GpuSample {
                    load_pct: 3.0,
                    ..Default::default()
                },
                GpuSample {
                    load_pct: 40.0,
                    temperature_c: Some(61.0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(s.net_totals(), (10.0, 1.0));
        assert_eq!(s.gpu_load(), Some(40.0));
        assert_eq!(s.gpu_temp(), Some(61.0));
        assert_eq!(Sample::default().gpu_load(), None);
    }
}
