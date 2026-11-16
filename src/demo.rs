//! Invented hardware and readings for `--demo` and the screenshots. Nothing
//! here is read from the machine the program runs on: every name, serial
//! number and address below is made up.
//!
//! The readings are a pure function of the history time `t` (seconds; zero
//! is the moment the program started, negative values are "earlier today"),
//! so the same `t` always gives the same picture. The invented day:
//!
//! * the evening before: light use on battery with a video stream, then charging;
//! * the night: idle, plugged in;
//! * the morning: office work, a backup to the external drive;
//! * a lunch break, then work on battery;
//! * a one-hour graphics session on mains power;
//! * the last two hours: work with build bursts, a large download, one short
//!   processor-temperature excursion, and the battery discharging again.

use crate::alerts::{self, AlertEvent, AlertRule, Engine};
use crate::model::*;

const KB: u64 = 1 << 10;
const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;
/// Decimal gigabyte, as drive makers count.
const GB10: u64 = 1_000_000_000;

/// Identifier of the integrated adapter.
pub const GPU_INTEGRATED: &str = "demo-igpu";
/// Identifier of the discrete adapter.
pub const GPU_DISCRETE: &str = "demo-dgpu";

const THREADS: usize = 16;
const MEMORY_TOTAL: u64 = 32 * GB - 182 * MB;
const BATTERY_DESIGN_MWH: u32 = 75_000;
const BATTERY_FULL_MWH: u32 = 66_000;
/// Uptime at `t == 0`.
const UPTIME_AT_START_S: f64 = (3 * 86_400 + 4 * 3600 + 17 * 60) as f64;

fn s(text: &str) -> String {
    text.to_string()
}

fn rows(pairs: &[(&str, &str)]) -> Extra {
    pairs.iter().map(|(a, b)| (s(a), s(b))).collect()
}

// ----------------------------------------------------------------------------------
// The invented machine
// ----------------------------------------------------------------------------------

/// The invented machine.
pub fn inventory() -> Inventory {
    let cache = |level, kind: &str, size_bytes, count, associativity| CacheInfo {
        level,
        kind: s(kind),
        size_bytes,
        count,
        line_size: 64,
        associativity,
    };
    let module = |slot: &str, bank: &str, serial: &str| MemoryModule {
        slot: s(slot),
        bank: s(bank),
        populated: true,
        size_bytes: 16 * GB,
        kind: s("DDR5"),
        form_factor: s("SODIMM"),
        speed_mhz: Some(5600),
        configured_mhz: Some(5200),
        manufacturer: s("Corvid Memory"),
        part_number: s("CM5S16G56C46-N"),
        serial: s(serial),
        voltage_mv: Some(1100),
        rank: Some(1),
        data_width: Some(64),
        extra: Vec::new(),
    };
    let empty = |slot: &str, bank: &str| MemoryModule {
        slot: s(slot),
        bank: s(bank),
        ..Default::default()
    };
    Inventory {
        cpu: CpuInfo {
            name: s("Vertex V7 Pro 840H"),
            vendor: s("Vertex Semiconductor"),
            architecture: s("x64"),
            family: 25,
            model: 68,
            stepping: 1,
            packages: 1,
            cores: 12,
            threads: THREADS as u32,
            performance_cores: Some(4),
            efficiency_cores: Some(8),
            base_mhz: Some(2700),
            max_mhz: Some(5100),
            caches: vec![
                cache(1, "Data", 48 * KB, 4, 12),
                cache(1, "Instruction", 32 * KB, 4, 8),
                cache(1, "Data", 32 * KB, 8, 8),
                cache(1, "Instruction", 64 * KB, 8, 8),
                cache(2, "Unified", 1280 * KB, 4, 10),
                cache(2, "Unified", 2 * MB, 2, 16),
                cache(3, "Unified", 18 * MB, 1, 12),
            ],
            features: [
                "SSE4.2", "AVX", "AVX2", "FMA3", "AES", "SHA", "BMI1", "BMI2", "F16C", "POPCNT",
                "RDRAND", "RDSEED", "CLMUL", "ADX", "VAES", "x2APIC",
            ]
            .iter()
            .map(|f| s(f))
            .collect(),
            virtualization: Some(true),
            hypervisor: false,
            socket: s("BGA 1680 (soldered)"),
            microcode: s("0x411C"),
            extra: rows(&[
                ("Lithography", "7 nm class"),
                ("Design power", "45 W"),
                ("Second-level address translation", "Supported"),
            ]),
        },
        memory_modules: vec![
            module("DIMM A1", "Channel A", "4C1F90A7"),
            empty("DIMM A2", "Channel A"),
            module("DIMM B1", "Channel B", "4C1F90B2"),
            empty("DIMM B2", "Channel B"),
        ],
        gpus: vec![
            GpuInfo {
                id: s(GPU_INTEGRATED),
                name: s("Vertex Integrated Graphics G7"),
                vendor: s("Vertex Semiconductor"),
                dedicated_bytes: 128 * MB,
                shared_bytes: 16 * GB - 91 * MB,
                driver_version: s("32.0.101.5522"),
                driver_date: s("2026-08-19"),
                software: false,
                integrated: Some(true),
                location: s("PCI bus 0, device 2, function 0"),
                extra: rows(&[("Feature level", "12_1"), ("Driver model", "WDDM 3.2")]),
            },
            GpuInfo {
                id: s(GPU_DISCRETE),
                name: s("Lumen Graphics LX 760M"),
                vendor: s("Lumen Graphics"),
                dedicated_bytes: 8 * GB,
                shared_bytes: 16 * GB - 91 * MB,
                driver_version: s("24.9.1.0"),
                driver_date: s("2026-10-02"),
                software: false,
                integrated: Some(false),
                location: s("PCI bus 1, device 0, function 0"),
                extra: rows(&[
                    ("Feature level", "12_2"),
                    ("Driver model", "WDDM 3.2"),
                    ("Memory type", "GDDR6"),
                ]),
            },
        ],
        system: SystemInfo {
            manufacturer: s("Northwind Devices"),
            product: s("Atlas Pro 16"),
            family: s("Atlas Pro"),
            sku: s("AP16-7H-32"),
            system_serial: s("NWD7K2M40913"),
            uuid: s("6F1D2A40-93B7-4C55-A1E8-70C2D94B5E13"),
            chassis: s("Notebook"),
            board_manufacturer: s("Northwind Devices"),
            board_product: s("AP16-MB7"),
            board_version: s("Rev 1.3"),
            board_serial: s("MB7Q41K08255"),
            bios_vendor: s("Keystone Firmware"),
            bios_version: s("AP16.1.18.0"),
            bios_date: s("2026-07-22"),
            firmware_mode: s("UEFI"),
            secure_boot: Some(true),
            tpm_present: Some(true),
            tpm_version: s("2.0"),
            tpm_manufacturer: s("Halden Trust Modules"),
            os_name: s("Windows 11 Pro"),
            os_edition: s("Professional"),
            os_display_version: s("25H2"),
            os_build: s("26200.7019"),
            os_arch: s("64-bit"),
            os_install_date: s("2026-03-14"),
            computer_name: s("ATLAS-WS"),
            user_name: s("Alex"),
            boot_time: s("2026-11-13 09:28"),
            time_zone: s("UTC+01:00 (Central European Time)"),
            locale: s("en-GB"),
            memory_slots_total: Some(4),
            memory_slots_used: Some(2),
            memory_max_bytes: Some(96 * GB),
            extra: rows(&[
                ("Embedded controller", "1.12"),
                ("Device encryption", "On"),
                ("Virtualisation-based security", "Running"),
                ("Power plan", "Balanced"),
            ]),
        },
        displays: vec![
            DisplayInfo {
                name: s("Built-in display"),
                manufacturer: s("Northwind Devices"),
                adapter: s("Vertex Integrated Graphics G7"),
                primary: true,
                width: 2560,
                height: 1600,
                refresh_hz: 165.0,
                bits_per_pixel: 32,
                scale_pct: 150,
                hdr_supported: Some(true),
                hdr_enabled: Some(false),
                connection: s("Internal"),
                position: (0, 0),
                orientation: s("Landscape"),
                size_mm: Some((345, 215)),
                diagonal_inches: Some(16.0),
                extra: rows(&[
                    ("Colour depth", "10 bits per channel"),
                    ("Panel", "NWD160Q7"),
                ]),
            },
            DisplayInfo {
                name: s("Meridian 27Q"),
                manufacturer: s("Meridian Displays"),
                adapter: s("Lumen Graphics LX 760M"),
                primary: false,
                width: 2560,
                height: 1440,
                refresh_hz: 75.0,
                bits_per_pixel: 32,
                scale_pct: 100,
                hdr_supported: Some(false),
                hdr_enabled: Some(false),
                connection: s("DisplayPort"),
                position: (2560, -120),
                orientation: s("Landscape"),
                size_mm: Some((597, 336)),
                diagonal_inches: Some(27.0),
                extra: rows(&[
                    ("Colour depth", "8 bits per channel"),
                    ("Made", "2024, week 31"),
                ]),
            },
        ],
    }
}

fn nvme_attributes() -> Vec<SmartAttribute> {
    let a = |name: &str, raw: u64, display: &str, explanation: &str| SmartAttribute {
        id: None,
        name: s(name),
        current: None,
        worst: None,
        threshold: None,
        raw,
        display: s(display),
        status: HealthStatus::Good,
        explanation: s(explanation),
    };
    vec![
        a(
            "Critical warning",
            0,
            "None",
            "The drive raises this flag when something needs attention. No flag is set.",
        ),
        a(
            "Temperature",
            43,
            "43 °C",
            "The drive's own temperature sensor. Well inside the normal range.",
        ),
        a(
            "Available spare",
            100,
            "100%",
            "Reserve blocks that replace worn-out ones. The full reserve is still there.",
        ),
        a(
            "Spare threshold",
            10,
            "10%",
            "The drive warns when the reserve falls below this level.",
        ),
        a(
            "Percentage used",
            4,
            "4%",
            "How much of the rated write endurance has been used. 96% of the rated life remains.",
        ),
        a(
            "Data read",
            21_480_000,
            "10.0 TB",
            "Everything read from the drive since it was new.",
        ),
        a(
            "Data written",
            17_840_000,
            "8.31 TB",
            "Everything written to the drive since it was new. Far below its rated endurance.",
        ),
        a(
            "Host read commands",
            312_904_117,
            "312,904,117",
            "Number of read requests the computer has sent.",
        ),
        a(
            "Host write commands",
            268_114_902,
            "268,114,902",
            "Number of write requests the computer has sent.",
        ),
        a(
            "Controller busy time",
            1_932,
            "1,932 min",
            "Time the controller spent working on requests.",
        ),
        a(
            "Power cycles",
            1_204,
            "1,204",
            "How often the drive has been switched on.",
        ),
        a(
            "Power-on time",
            5_816,
            "5,816 hours",
            "Total time the drive has been powered.",
        ),
        a(
            "Unsafe shutdowns",
            17,
            "17",
            "Times the power went away without a clean shutdown. A few are normal for a notebook.",
        ),
        a(
            "Media and data integrity errors",
            0,
            "0",
            "Data the drive could not read back correctly. Zero is what you want.",
        ),
        a(
            "Error log entries",
            0,
            "0",
            "Errors the controller has recorded. None so far.",
        ),
        a(
            "Warning temperature time",
            0,
            "0 min",
            "Time spent above the warning temperature. The drive has never run too hot.",
        ),
        a(
            "Critical temperature time",
            0,
            "0 min",
            "Time spent above the critical temperature. Never.",
        ),
    ]
}

fn sata_attributes() -> Vec<SmartAttribute> {
    #[allow(clippy::too_many_arguments)]
    fn a(
        id: u8,
        name: &str,
        current: u8,
        worst: u8,
        threshold: u8,
        raw: u64,
        display: &str,
        status: HealthStatus,
        explanation: &str,
    ) -> SmartAttribute {
        SmartAttribute {
            id: Some(id),
            name: s(name),
            current: Some(current),
            worst: Some(worst),
            threshold: Some(threshold),
            raw,
            display: s(display),
            status,
            explanation: s(explanation),
        }
    }
    use HealthStatus::{Caution, Good};
    vec![
        a(
            5,
            "Reallocated sectors",
            96,
            96,
            10,
            12,
            "12",
            Caution,
            "Damaged areas the drive has replaced with spares. 12 so far: keep a backup and watch whether the number grows.",
        ),
        a(
            9,
            "Power-on time",
            93,
            93,
            0,
            31_220,
            "31,220 hours",
            Good,
            "Total time the drive has been powered. About three and a half years.",
        ),
        a(
            12,
            "Power cycles",
            97,
            97,
            0,
            2_871,
            "2,871",
            Good,
            "How often the drive has been switched on.",
        ),
        a(
            177,
            "Wear levelling count",
            29,
            29,
            5,
            1_068,
            "71% used",
            Caution,
            "How much of the flash memory's write endurance is used up. 29% remains: plan a replacement.",
        ),
        a(
            179,
            "Used reserved blocks",
            96,
            96,
            10,
            12,
            "12",
            Good,
            "Spare blocks already taken into use. Plenty of reserve is left.",
        ),
        a(
            181,
            "Program failures",
            100,
            100,
            10,
            0,
            "0",
            Good,
            "Failed attempts to write flash memory. None.",
        ),
        a(
            182,
            "Erase failures",
            100,
            100,
            10,
            0,
            "0",
            Good,
            "Failed attempts to erase flash memory. None.",
        ),
        a(
            187,
            "Uncorrectable errors",
            100,
            100,
            0,
            0,
            "0",
            Good,
            "Read errors the drive could not repair by itself. None.",
        ),
        a(
            190,
            "Airflow temperature",
            64,
            48,
            0,
            36,
            "36 °C",
            Good,
            "Temperature of the air around the drive. Normal.",
        ),
        a(
            194,
            "Temperature",
            64,
            48,
            0,
            36,
            "36 °C",
            Good,
            "The drive's own temperature sensor. Normal; the highest ever recorded was 52 °C.",
        ),
        a(
            195,
            "Error correction rate",
            200,
            200,
            0,
            0,
            "0",
            Good,
            "Read errors repaired on the fly. Nothing unusual.",
        ),
        a(
            199,
            "Cable errors",
            100,
            100,
            0,
            0,
            "0",
            Good,
            "Transfer errors between drive and computer, usually a cable or connector fault. None.",
        ),
        a(
            241,
            "Total written",
            99,
            99,
            0,
            151_190_000_000,
            "70.4 TB",
            Good,
            "Everything written to the drive since it was new.",
        ),
        a(
            242,
            "Total read",
            99,
            99,
            0,
            124_340_000_000,
            "57.9 TB",
            Good,
            "Everything read from the drive since it was new.",
        ),
    ]
}

/// Invented drives and volumes.
pub fn slow() -> Slow {
    let part = |number, kind: &str, offset, size, letter: &str| Partition {
        number,
        kind: s(kind),
        offset,
        size,
        letter: s(letter),
    };
    let volume = |mount: &str, label: &str, fs: &str, total, free, drive, bitlocker| Volume {
        mount: s(mount),
        label: s(label),
        filesystem: s(fs),
        total,
        free,
        drive_index: Some(drive),
        bitlocker,
        kind: s("Fixed"),
    };
    let nvme_size = 1000 * GB10 + 204 * MB;
    let sata_size = 2000 * GB10 + 398 * MB;
    let usb_size = 4000 * GB10 + 787 * MB;
    Slow {
        drives: vec![
            DriveInfo {
                index: 0,
                model: s("Borealis NV7 Pro 1TB"),
                firmware: s("4B2QNV07"),
                serial: s("BNV7X41M902713K"),
                interface: s("NVMe"),
                media: s("SSD"),
                size_bytes: nvme_size,
                removable: false,
                partition_style: s("GPT"),
                partitions: vec![
                    part(1, "EFI system", MB, 260 * MB, ""),
                    part(2, "Reserved", 261 * MB, 16 * MB, ""),
                    part(3, "Basic data", 277 * MB, nvme_size - 1300 * MB, "C:"),
                    part(4, "Recovery", nvme_size - 1023 * MB, 1022 * MB, ""),
                ],
                rotation_rpm: None,
                health: HealthStatus::Good,
                health_pct: Some(96),
                health_summary: s(
                    "This drive is healthy. It has used 4% of its rated write endurance, its spare capacity is untouched and it has recorded no errors.",
                ),
                temperature_c: Some(43.0),
                power_on_hours: Some(5_816),
                power_cycles: Some(1_204),
                total_written_bytes: Some(9_134 * GB),
                total_read_bytes: Some(10_998 * GB),
                smart_source: s("NVMe health log"),
                smart_note: String::new(),
                attributes: nvme_attributes(),
                extra: rows(&[
                    ("NVMe version", "2.0"),
                    ("Link", "PCIe 4.0 x4"),
                    ("Warning temperature", "81 °C"),
                    ("Critical temperature", "85 °C"),
                ]),
            },
            DriveInfo {
                index: 1,
                model: s("Kestrel S500 2TB"),
                firmware: s("KS5.21"),
                serial: s("KS500T2A7716420"),
                interface: s("SATA"),
                media: s("SSD"),
                size_bytes: sata_size,
                removable: false,
                partition_style: s("GPT"),
                partitions: vec![
                    part(1, "Reserved", MB, 16 * MB, ""),
                    part(2, "Basic data", 17 * MB, 1500 * GB10, "D:"),
                    part(3, "Basic data", 17 * MB + 1500 * GB10, sata_size - 1500 * GB10 - 18 * MB, "E:"),
                ],
                rotation_rpm: None,
                health: HealthStatus::Caution,
                health_pct: Some(29),
                health_summary: s(
                    "This drive still works but is wearing out. It has used 71% of its write endurance and has replaced 12 damaged sectors. Keep a backup and plan a replacement.",
                ),
                temperature_c: Some(36.0),
                power_on_hours: Some(31_220),
                power_cycles: Some(2_871),
                total_written_bytes: Some(72_090 * GB),
                total_read_bytes: Some(59_290 * GB),
                smart_source: s("ATA SMART"),
                smart_note: String::new(),
                attributes: sata_attributes(),
                extra: rows(&[("Link", "SATA 6 Gb/s"), ("TRIM", "Supported")]),
            },
            DriveInfo {
                index: 2,
                model: s("Tundra Portable 4TB"),
                firmware: s("1.07"),
                serial: s("TP4U0092GH5511"),
                interface: s("USB"),
                media: s("HDD"),
                size_bytes: usb_size,
                removable: true,
                partition_style: s("GPT"),
                partitions: vec![part(1, "Basic data", MB, usb_size - 2 * MB, "F:")],
                rotation_rpm: Some(5400),
                health: HealthStatus::Unknown,
                health_pct: None,
                health_summary: s(
                    "The health of this drive is unknown: its USB enclosure does not pass health data on to the computer.",
                ),
                temperature_c: None,
                power_on_hours: None,
                power_cycles: None,
                total_written_bytes: None,
                total_read_bytes: None,
                smart_source: String::new(),
                smart_note: s("the USB enclosure does not pass health data through"),
                attributes: Vec::new(),
                extra: rows(&[("Connection", "USB 3.2 Gen 1 (5 Gb/s)")]),
            },
        ],
        volumes: vec![
            volume("C:", "Windows", "NTFS", 931 * GB, 389 * GB, 0, Some(true)),
            volume("D:", "Projects", "NTFS", 1397 * GB, 44 * GB, 1, Some(false)),
            volume("E:", "Scratch", "NTFS", 465 * GB, 301 * GB, 1, Some(false)),
            volume("F:", "Backup", "exFAT", 3725 * GB, 2190 * GB, 2, None),
        ],
    }
}

// ----------------------------------------------------------------------------------
// The invented day
// ----------------------------------------------------------------------------------

fn hash(seed: u64, n: i64) -> f64 {
    let mut z = (n as u64)
        .wrapping_add(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth value noise in `0.0..=1.0`: a different curve for every `seed`.
fn noise(seed: u64, x: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let f = f * f * (3.0 - 2.0 * f);
    let a = hash(seed, i as i64);
    let b = hash(seed, i as i64 + 1);
    a + (b - a) * f
}

fn smoothstep(x: f64, a: f64, b: f64) -> f64 {
    if b <= a {
        return if x >= a { 1.0 } else { 0.0 };
    }
    let u = ((x - a) / (b - a)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// One while `x` is between `start` and `end`, with soft edges of `ramp`.
fn block(x: f64, start: f64, end: f64, ramp: f64) -> f64 {
    smoothstep(x, start, start + ramp) * (1.0 - smoothstep(x, end - ramp, end))
}

/// Battery charge over the day as (hour, percent) knots; after the last knot
/// the battery keeps discharging.
const BATTERY_KNOTS: [(f64, f64); 7] = [
    (-24.0, 78.0),
    (-21.5, 34.0),
    (-19.6, 100.0),
    (-4.7, 100.0),
    (-3.3, 61.0),
    (-1.25, 96.0),
    (0.0, 71.0),
];
const BATTERY_FLOOR_PCT: f64 = 22.0;

/// Battery percent and its slope in percent per hour at hour `x`.
fn battery_curve(x: f64) -> (f64, f64) {
    let knots = &BATTERY_KNOTS;
    if x <= knots[0].0 {
        return (knots[0].1, 0.0);
    }
    for pair in knots.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        if x <= x1 {
            let slope = (y1 - y0) / (x1 - x0);
            return (y0 + slope * (x - x0), slope);
        }
    }
    let (x0, y0) = knots[knots.len() - 2];
    let (x1, y1) = knots[knots.len() - 1];
    let slope = (y1 - y0) / (x1 - x0);
    let pct = y1 + slope * (x - x1);
    if pct <= BATTERY_FLOOR_PCT {
        (BATTERY_FLOOR_PCT, 0.0)
    } else {
        (pct, slope)
    }
}

/// The slowly changing "what is the person doing" curves at time `t`.
#[derive(Clone, Copy, Debug)]
struct Scene {
    /// Office and development work, 0..1.
    work: f64,
    /// A video stream on the integrated adapter, 0..1.
    video: f64,
    /// The graphics session on the discrete adapter, 0..1.
    session: f64,
    /// The same with slow edges, for temperatures.
    session_heat: f64,
    /// Build bursts during work, 0..1.
    burst: f64,
    /// The short full-load excursion a few minutes ago, 0..1.
    excursion: f64,
    /// The backup to the external drive, 0..1.
    backup: f64,
    /// A large download, 0..1.
    download: f64,
}

fn scene(t: f64) -> Scene {
    let x = t / 3600.0;
    let work = 0.75 * block(x, -24.5, -21.3, 0.2)
        + block(x, -9.5, -5.5, 0.25)
        + block(x, -4.7, -3.25, 0.1)
        + 0.9 * smoothstep(x, -2.2, -2.05);
    let work = work.min(1.0);
    // A build every few minutes while working; some slots stay quiet.
    let period = 410.0;
    let shifted = t + 235.0;
    let slot = (shifted / period).floor();
    let phase = shifted / period - slot;
    // The build that finished a few minutes ago is always a big one.
    let strength = if slot == 0.0 {
        0.92
    } else {
        hash(11, slot as i64)
    };
    let length = 0.14 + 0.1 * hash(12, slot as i64);
    let burst = if strength > 0.35 {
        block(phase, 0.05, 0.05 + length, 0.02) * (0.55 + 0.45 * strength)
    } else {
        0.0
    };
    Scene {
        work,
        video: block(x, -23.6, -21.7, 0.05),
        session: block(x, -3.2, -2.2, 0.03),
        session_heat: block(x, -3.2, -2.17, 0.14),
        burst: burst * smoothstep(work, 0.2, 0.6),
        excursion: block(t, -478.0, -418.0, 7.0),
        backup: block(x, -8.2, -7.55, 0.02),
        download: block(x, -1.62, -1.46, 0.01),
    }
}

fn total_cpu(t: f64, sc: &Scene) -> f64 {
    let idle = 2.2 + 1.6 * noise(1, t / 9.0);
    let work = sc.work * (9.0 + 13.0 * noise(2, t / 41.0) + 7.0 * noise(3, t / 6.0));
    let video = sc.video * (6.0 + 3.0 * noise(4, t / 5.0));
    let session = sc.session * (34.0 + 14.0 * noise(5, t / 23.0));
    let burst = sc.burst * (52.0 + 14.0 * noise(6, t / 4.0));
    let v = idle + work + video + session + burst;
    let v = v + (100.0 - v) * sc.excursion * (0.97 + 0.03 * noise(7, t / 3.0));
    v.clamp(0.5, 100.0)
}

struct ProcessSpec {
    name: &'static str,
    /// Share of the machine's processor load this process takes.
    weight: f32,
    memory_mb: u32,
    threads: u32,
    /// Megabytes per second read / written when the machine is busy.
    disk: (f32, f32),
    /// 0 = no graphics use, 1 = integrated, 2 = discrete session.
    gpu: u8,
    system: bool,
}

const fn p(
    name: &'static str,
    weight: f32,
    memory_mb: u32,
    threads: u32,
    disk: (f32, f32),
    gpu: u8,
    system: bool,
) -> ProcessSpec {
    ProcessSpec {
        name,
        weight,
        memory_mb,
        threads,
        disk,
        gpu,
        system,
    }
}

const PROCESSES: [ProcessSpec; 46] = [
    p("System Idle Process", 0.0, 0, 16, (0.0, 0.0), 0, true),
    p("System", 1.2, 14, 268, (0.3, 0.9), 0, true),
    p("Registry", 0.0, 62, 4, (0.0, 0.0), 0, true),
    p("Session Manager", 0.0, 1, 2, (0.0, 0.0), 0, true),
    p("Client Server Runtime", 0.3, 5, 14, (0.0, 0.0), 0, true),
    p("Windows Logon", 0.0, 3, 5, (0.0, 0.0), 0, true),
    p("Services Host", 0.2, 9, 8, (0.0, 0.0), 0, true),
    p("Security Authority", 0.2, 12, 9, (0.0, 0.0), 0, true),
    p("Service Host: Network", 0.4, 21, 17, (0.0, 0.0), 0, true),
    p(
        "Service Host: Local System",
        0.6,
        44,
        31,
        (0.1, 0.1),
        0,
        true,
    ),
    p("Service Host: Update", 0.3, 38, 12, (0.2, 0.3), 0, true),
    p("Service Host: Audio", 0.3, 16, 9, (0.0, 0.0), 0, true),
    p("Desktop Window Manager", 2.4, 148, 24, (0.0, 0.0), 1, true),
    p("File Explorer", 0.9, 112, 71, (0.1, 0.0), 1, false),
    p("Shell Experience Host", 0.2, 58, 22, (0.0, 0.0), 1, false),
    p("Start Menu", 0.1, 46, 18, (0.0, 0.0), 0, false),
    p("Search Indexer", 0.7, 73, 19, (0.9, 0.4), 0, true),
    p("Security Scanner", 2.6, 286, 37, (1.8, 0.2), 0, true),
    p("Print Spooler", 0.0, 7, 9, (0.0, 0.0), 0, true),
    p("Audio Device Graph", 0.4, 18, 11, (0.0, 0.0), 0, true),
    p("Task Scheduler Host", 0.1, 11, 6, (0.0, 0.0), 0, true),
    p("Runtime Broker", 0.1, 14, 7, (0.0, 0.0), 0, false),
    p("Text Input Host", 0.1, 31, 12, (0.0, 0.0), 0, false),
    p("Web Browser", 6.5, 612, 44, (0.2, 0.6), 1, false),
    p(
        "Web Browser (tab: documentation)",
        2.2,
        388,
        21,
        (0.0, 0.0),
        0,
        false,
    ),
    p(
        "Web Browser (tab: mail)",
        1.4,
        296,
        19,
        (0.0, 0.0),
        0,
        false,
    ),
    p(
        "Web Browser (tab: video)",
        3.1,
        471,
        23,
        (0.0, 0.0),
        1,
        false,
    ),
    p("Web Browser (graphics)", 1.8, 214, 14, (0.0, 0.0), 1, false),
    p("Web Browser (network)", 0.5, 64, 12, (0.0, 0.2), 0, false),
    p("Code Editor", 5.2, 742, 58, (0.6, 0.4), 1, false),
    p(
        "Code Editor (language server)",
        7.4,
        1284,
        36,
        (1.1, 0.2),
        0,
        false,
    ),
    p("Code Editor (terminal)", 0.6, 96, 11, (0.0, 0.0), 0, false),
    p("Build Tool", 24.0, 918, 34, (9.5, 14.0), 0, false),
    p("Compiler Driver", 16.0, 644, 18, (6.2, 7.5), 0, false),
    p("Container Engine", 3.2, 1508, 62, (0.8, 1.2), 0, false),
    p("Database Server", 1.6, 902, 47, (0.5, 0.9), 0, false),
    p("Terminal", 0.4, 82, 15, (0.0, 0.0), 1, false),
    p("Chat", 1.3, 356, 41, (0.0, 0.1), 1, false),
    p("Mail Client", 0.5, 228, 27, (0.0, 0.1), 0, false),
    p("Music Player", 0.9, 184, 26, (0.1, 0.0), 0, false),
    p("Notes Desk", 0.3, 142, 17, (0.0, 0.0), 0, false),
    p("Cloud Sync", 0.8, 126, 22, (0.4, 0.3), 0, false),
    p("Password Vault", 0.1, 88, 12, (0.0, 0.0), 0, false),
    p("Render Studio", 9.0, 2260, 52, (1.5, 0.4), 2, false),
    p("Screenshot Tool", 0.2, 54, 9, (0.0, 0.0), 0, false),
    p("PulseForge", 0.3, 46, 9, (0.0, 0.0), 1, false),
];

fn processes(
    t: f64,
    sc: &Scene,
    cpu_total: f64,
    dgpu_load: f64,
    igpu_load: f64,
) -> Vec<ProcessSample> {
    let busy = (sc.burst + sc.excursion).min(1.0);
    // How strongly each process takes part right now.
    let level = |i: usize, spec: &ProcessSpec| -> f64 {
        let wobble = 0.55 + 0.9 * noise(100 + i as u64, t / 5.0);
        let role = match spec.name {
            "Build Tool" | "Compiler Driver" => 0.02 + busy,
            "Render Studio" => 0.03 + sc.session * 2.5,
            "Web Browser (tab: video)" => 0.15 + sc.video * 2.0,
            _ if spec.system => 0.5 + 0.2 * sc.work,
            _ => 0.25 + 0.75 * sc.work,
        };
        f64::from(spec.weight) * wobble * role
    };
    let levels: Vec<f64> = PROCESSES
        .iter()
        .enumerate()
        .map(|(i, spec)| level(i, spec))
        .collect();
    let sum: f64 = levels.iter().sum::<f64>().max(0.001);
    let scale = cpu_total * 0.97 / sum;
    PROCESSES
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let idle_process = spec.name == "System Idle Process";
            let cpu = if idle_process {
                100.0 - cpu_total
            } else {
                levels[i] * scale
            };
            let active = match spec.name {
                "Build Tool" | "Compiler Driver" => busy,
                _ => 0.08 + 0.3 * sc.work * noise(200 + i as u64, t / 7.0),
            };
            let memory = f64::from(spec.memory_mb)
                * (0.94 + 0.12 * noise(300 + i as u64, t / 90.0))
                * if spec.name == "Render Studio" {
                    0.35 + 0.65 * sc.session
                } else {
                    1.0
                };
            let gpu_pct = match spec.gpu {
                0 => None,
                2 => Some((dgpu_load * 0.96) as f32),
                _ => Some((igpu_load * f64::from(spec.weight) / 22.0).min(igpu_load) as f32),
            };
            ProcessSample {
                pid: match i {
                    0 => 0,
                    1 => 4,
                    _ => 400 + i as u32 * 52 + (hash(400, i as i64) * 12.0) as u32 * 4,
                },
                parent_pid: if i < 8 { 4 } else { 812 },
                name: s(spec.name),
                cpu_pct: cpu.clamp(0.0, 100.0) as f32,
                memory: (memory * MB as f64) as u64,
                disk_read_bps: f64::from(spec.disk.0) * MB as f64 * active,
                disk_write_bps: f64::from(spec.disk.1) * MB as f64 * active,
                net_bps: None,
                gpu_pct,
                gpu_memory: match spec.gpu {
                    0 => None,
                    2 => Some(((0.3 + 4.9 * sc.session) * GB as f64) as u64),
                    _ => Some(u64::from(spec.memory_mb) * MB / 6),
                },
                threads: spec.threads,
                handles: spec.threads * 23 + (hash(500, i as i64) * 400.0) as u32,
                running_s: if spec.system {
                    (UPTIME_AT_START_S + t).max(30.0) as u64
                } else {
                    (UPTIME_AT_START_S + t - hash(600, i as i64) * 86_000.0).max(30.0) as u64
                },
                user: s(if spec.system { "SYSTEM" } else { "Alex" }),
            }
        })
        .collect()
}

/// Generates readings as a smooth, deterministic function of time.
#[derive(Default)]
pub struct DemoSource {}

impl DemoSource {
    pub fn new() -> DemoSource {
        DemoSource::default()
    }

    /// The readings at time `t` (seconds on the history clock; negative values
    /// are used to pre-fill the graphs). `unix` is the wall-clock time to stamp.
    /// With `detail` false the process list and the descriptive texts of the
    /// network adapters are left empty (pre-filling).
    pub fn sample(&mut self, t: f64, unix: u64, detail: bool) -> Sample {
        let sc = scene(t);
        let x = t / 3600.0;

        // Processor.
        let total = total_cpu(t, &sc);
        let per_core: Vec<f32> = (0..THREADS)
            .map(|i| {
                // Performance threads (0..8) carry more than efficiency cores.
                let share = if i < 8 {
                    1.18 - 0.22 * (i % 2) as f64
                } else {
                    0.62
                };
                let wobble = 0.45 + 1.1 * noise(20 + i as u64, t / (3.0 + i as f64 * 0.37));
                let v = total * share * wobble;
                (v + (100.0 - v) * sc.excursion * 0.96).clamp(0.0, 100.0) as f32
            })
            .collect();
        let demand = (total / 60.0).min(1.0);
        let mhz =
            1350.0 + 3350.0 * demand.powf(0.6) + 260.0 * noise(40, t / 4.0) - 600.0 * sc.excursion;
        let per_core_mhz: Vec<f32> = per_core
            .iter()
            .enumerate()
            .map(|(i, load)| {
                let top = if i < 8 { 5100.0 } else { 3900.0 };
                (900.0 + (top - 900.0) * (f64::from(*load) / 70.0).min(1.0).powf(0.5)) as f32
            })
            .collect();
        let heat = sc.work * 9.0 + sc.session_heat * 24.0 + sc.burst * 19.0 + sc.video * 5.0;
        let cpu_temp = 41.0 + heat + 2.0 * noise(41, t / 15.0);
        let cpu_temp = cpu_temp + (93.6 - cpu_temp) * smoothstep(sc.excursion, 0.0, 0.9);

        // Memory.
        let in_use_gb = 9.6
            + 6.4 * sc.work
            + 0.8 * noise(50, t / 600.0)
            + 4.6 * sc.session
            + 1.6 * sc.burst
            + 0.9 * sc.video;
        let used = (in_use_gb * GB as f64) as u64;
        let modified = ((0.35 + 0.5 * sc.work + 0.2 * noise(51, t / 200.0)) * GB as f64) as u64;
        let available = MEMORY_TOTAL - used;
        let standby = (available as f64 * (0.52 + 0.2 * noise(52, t / 900.0))) as u64;
        let free = available - standby;
        let pagefile_total = 6 * GB;
        let pagefile_used = ((0.5 + 0.9 * sc.work + 0.5 * sc.session) * GB as f64) as u64;
        let memory = MemorySample {
            total: MEMORY_TOTAL,
            used,
            available,
            cached: standby + modified,
            free,
            commit_used: used + pagefile_used + (7.2 * GB as f64) as u64,
            commit_limit: MEMORY_TOTAL + pagefile_total,
            pagefile_used,
            pagefile_total,
            kernel_paged: ((0.61 + 0.05 * noise(53, t / 300.0)) * GB as f64) as u64,
            kernel_nonpaged: ((0.42 + 0.03 * noise(54, t / 300.0)) * GB as f64) as u64,
            compressed: Some(((0.2 + 0.5 * sc.work) * GB as f64) as u64),
            modified: Some(modified),
            standby: Some(standby),
            hardware_reserved: Some(32 * GB - MEMORY_TOTAL),
            page_faults_per_s: Some(
                900.0 + 5200.0 * sc.work * noise(55, t / 5.0) + 14_000.0 * sc.burst,
            ),
        };

        // Graphics.
        let igpu_load = (2.0
            + sc.work * (5.0 + 9.0 * noise(60, t / 8.0))
            + sc.video * (21.0 + 6.0 * noise(61, t / 4.0))
            + sc.session * 6.0)
            .clamp(0.0, 100.0);
        let dgpu_load = (0.4
            + 1.2 * noise(62, t / 11.0) * sc.work
            + sc.session * (89.0 + 9.0 * noise(63, t / 9.0)))
        .clamp(0.0, 100.0);
        let engine = |name: &str, pct: f64| GpuEngine {
            name: s(name),
            pct: pct.clamp(0.0, 100.0) as f32,
        };
        let gpus = vec![
            GpuSample {
                id: s(GPU_INTEGRATED),
                load_pct: igpu_load as f32,
                engines: vec![
                    engine("3D", igpu_load - sc.video * 14.0),
                    engine("Copy", 0.4 + 1.5 * sc.work * noise(64, t / 6.0)),
                    engine("Video Decode", sc.video * (19.0 + 5.0 * noise(65, t / 4.0))),
                    engine("Video Processing", sc.video * 6.0),
                ],
                dedicated_used: 44 * MB,
                shared_used: ((0.7 + 0.8 * sc.work + 0.5 * sc.video) * GB as f64) as u64,
                temperature_c: None,
                temperature_note: s(NO_DRIVER),
                power_w: None,
                fan_pct: None,
                fan_rpm: None,
                clock_mhz: None,
            },
            GpuSample {
                id: s(GPU_DISCRETE),
                load_pct: dgpu_load as f32,
                engines: vec![
                    engine("3D", dgpu_load),
                    engine("Copy", sc.session * (7.0 + 5.0 * noise(66, t / 5.0))),
                    engine("Compute", sc.session * (22.0 + 12.0 * noise(67, t / 13.0))),
                    engine("Video Encode", sc.session * 11.0 * noise(68, t / 30.0)),
                ],
                dedicated_used: ((0.55 + 5.3 * sc.session + 0.2 * noise(69, t / 60.0)) * GB as f64)
                    as u64,
                shared_used: ((0.08 + 0.4 * sc.session) * GB as f64) as u64,
                // A demanding scene in the middle of the session pushes it close to its limit.
                temperature_c: Some(
                    (43.0
                        + 31.0 * sc.session_heat
                        + 15.5 * block(x, -2.78, -2.6, 0.03)
                        + 1.5 * noise(70, t / 20.0)) as f32,
                ),
                temperature_note: String::new(),
                power_w: Some((7.0 + 98.0 * sc.session * (0.9 + 0.1 * noise(71, t / 9.0))) as f32),
                fan_pct: Some((sc.session_heat * 78.0) as f32),
                fan_rpm: Some((sc.session_heat * 4350.0) as u32),
                clock_mhz: Some((420.0 + 1980.0 * smoothstep(sc.session, 0.0, 0.5)) as f32),
            },
        ];

        // Drives.
        let mb = MB as f64;
        let busy = (sc.burst + sc.excursion).min(1.0);
        let d0_read = mb
            * (0.05
                + 1.4 * sc.work * noise(80, t / 7.0).powi(3)
                + 62.0 * busy * noise(81, t / 3.0)
                + 30.0 * sc.session * noise(82, t / 40.0).powi(4));
        let d0_write = mb
            * (0.12
                + 0.9 * sc.work * noise(83, t / 5.0).powi(2)
                + 88.0 * busy * noise(84, t / 2.5)
                + 41.0 * sc.download);
        let d1_read = mb
            * (0.6 * sc.work * noise(85, t / 9.0).powi(4)
                + 24.0 * busy * noise(86, t / 4.0)
                + 118.0 * sc.backup);
        let d1_write =
            mb * (0.3 * sc.work * noise(87, t / 9.0).powi(4) + 9.0 * busy * noise(88, t / 4.0));
        let d2_write = mb * sc.backup * (104.0 + 14.0 * noise(89, t / 6.0));
        let disk = |index, read: f64, write: f64, ceiling: f64| DiskSample {
            index,
            read_bps: read,
            write_bps: write,
            active_pct: (((read + write) / (ceiling * mb)) * 100.0).clamp(0.0, 100.0) as f32,
            queue_depth: Some((((read + write) / (ceiling * mb)) * 3.5) as f32),
        };
        let disks = vec![
            disk(0, d0_read, d0_write, 170.0),
            disk(1, d1_read, d1_write, 130.0),
            disk(2, 0.0, d2_write, 125.0),
        ];

        // Network.
        let wifi_rx = 1024.0
            * (6.0
                + sc.work * (40.0 + 900.0 * noise(90, t / 6.0).powi(5))
                + sc.video * (3900.0 + 900.0 * noise(91, t / 3.0))
                + sc.download * (39_000.0 + 4000.0 * noise(92, t / 2.0)));
        let wifi_tx = 1024.0
            * (2.0
                + sc.work * (14.0 + 260.0 * noise(93, t / 6.0).powi(5))
                + sc.video * 60.0
                + sc.download * 420.0
                + block(x, -6.52, -6.3, 0.01) * (5200.0 + 600.0 * noise(94, t / 3.0)));
        let virt_rx = 1024.0 * sc.work * 38.0 * noise(95, t / 5.0).powi(3);
        let virt_tx = 1024.0 * sc.work * 22.0 * noise(96, t / 5.0).powi(3);
        let since_boot = UPTIME_AT_START_S + t;
        let list = |items: &[&str]| -> Vec<String> {
            if detail {
                items.iter().map(|i| s(i)).collect()
            } else {
                Vec::new()
            }
        };
        let text = |value: &str| if detail { s(value) } else { String::new() };
        let net = vec![
            NetSample {
                name: s("Wi-Fi"),
                description: text("Corvid Wireless AX-2400 (Wi-Fi 6E)"),
                kind: s("Wi-Fi"),
                up: true,
                link_bps: 1_201_000_000,
                rx_bps: wifi_rx,
                tx_bps: wifi_tx,
                rx_total: (since_boot * 96_000.0) as u64,
                tx_total: (since_boot * 11_500.0) as u64,
                ipv4: list(&["192.168.50.42"]),
                ipv6: list(&["2001:db8:42:50::a1c", "fe80::4c1f:90ff:fea7:2b10"]),
                mac: text("02:4C:1F:90:A7:2B"),
                gateway: text("192.168.50.1"),
                dns: list(&["192.168.50.1", "2001:db8:42:50::1"]),
                extra: if detail {
                    rows(&[
                        ("Network name", "Lighthouse-5G"),
                        ("Band", "5 GHz, channel 44"),
                        ("Signal", "Excellent (-48 dBm)"),
                        ("Security", "WPA3-Personal"),
                    ])
                } else {
                    Vec::new()
                },
            },
            NetSample {
                name: s("Ethernet"),
                description: text("Northwind 2.5 Gigabit Ethernet Controller"),
                kind: s("Ethernet"),
                up: false,
                link_bps: 0,
                rx_bps: 0.0,
                tx_bps: 0.0,
                rx_total: 0,
                tx_total: 0,
                ipv4: Vec::new(),
                ipv6: Vec::new(),
                mac: text("02:4C:1F:90:A7:2A"),
                gateway: String::new(),
                dns: Vec::new(),
                extra: if detail {
                    rows(&[("Cable", "Not plugged in")])
                } else {
                    Vec::new()
                },
            },
            NetSample {
                name: s("vEthernet (Sandbox)"),
                description: text("Virtual Ethernet Adapter"),
                kind: s("Virtual"),
                up: true,
                link_bps: 10_000_000_000,
                rx_bps: virt_rx,
                tx_bps: virt_tx,
                rx_total: (since_boot * 2100.0) as u64,
                tx_total: (since_boot * 1300.0) as u64,
                ipv4: list(&["172.24.96.1"]),
                ipv6: list(&["fe80::15:5dff:fe42:9a01"]),
                mac: text("02:15:5D:42:9A:01"),
                gateway: String::new(),
                dns: Vec::new(),
                extra: Vec::new(),
            },
        ];

        // Battery.
        let (percent, slope) = battery_curve(x);
        let (state, ac_online) = if slope < -0.01 {
            (BatteryState::Discharging, false)
        } else if slope > 0.01 {
            (BatteryState::Charging, true)
        } else if percent >= 99.5 {
            (BatteryState::Full, true)
        } else {
            (BatteryState::Discharging, false)
        };
        let wobble = 0.88 + 0.24 * noise(97, t / 30.0);
        let rate_mw = if slope.abs() > 0.01 {
            (slope / 100.0 * f64::from(BATTERY_FULL_MWH) * wobble) as i32
        } else if ac_online {
            0
        } else {
            // Resting on the floor of the demo curve: still drawing a little.
            (-6200.0 * wobble) as i32
        };
        let remaining = (percent / 100.0 * f64::from(BATTERY_FULL_MWH)) as u32;
        let seconds_left = if slope < -0.01 {
            Some((percent / -slope * 3600.0) as u32)
        } else if slope > 0.01 {
            Some(((100.0 - percent) / slope * 3600.0) as u32)
        } else {
            None
        };
        let battery = BatteryInfo {
            name: s("AP16-BT75"),
            manufacturer: s("Halden Energy"),
            serial: s("HE75B20417"),
            chemistry: s("Lithium polymer"),
            design_mwh: Some(BATTERY_DESIGN_MWH),
            full_mwh: Some(BATTERY_FULL_MWH),
            remaining_mwh: Some(remaining),
            percent: percent as f32,
            state,
            ac_online,
            rate_mw: Some(rate_mw),
            voltage_mv: Some((14_200.0 + 26.0 * percent + 180.0 * slope.signum()) as u32),
            cycle_count: Some(318),
            wear_pct: Some(
                (1.0 - f64::from(BATTERY_FULL_MWH) / f64::from(BATTERY_DESIGN_MWH)) as f32 * 100.0,
            ),
            seconds_left,
            temperature_c: Some((29.0 + 4.0 * sc.session_heat + slope.abs() * 0.12) as f32),
            manufacture_date: s("2025-01-20"),
            extra: if detail {
                rows(&[("Charge limit", "Off"), ("Adapter", "140 W USB-C")])
            } else {
                Vec::new()
            },
        };

        // Sensors the machine exposes by itself, and the honest gaps.
        let reading =
            |group: &str, name: &str, kind, value: Option<f64>, source: &str, note: &str| {
                SensorReading {
                    group: s(group),
                    name: s(name),
                    kind,
                    value,
                    source: s(source),
                    note: s(note),
                }
            };
        let fan_rpm = 0.0_f64.max(
            1750.0 * smoothstep(heat, 6.0, 12.0)
                + 2600.0 * smoothstep(heat + 30.0 * sc.excursion, 12.0, 34.0),
        );
        let zone = "ACPI thermal zone";
        let sensors = vec![
            reading(
                "Processor",
                "Package temperature",
                SensorKind::Temperature,
                Some(cpu_temp),
                zone,
                "the firmware's processor zone",
            ),
            reading(
                "Processor",
                "Core voltage",
                SensorKind::Voltage,
                None,
                "",
                NO_DRIVER,
            ),
            reading(
                "Processor",
                "Package power",
                SensorKind::Power,
                None,
                "",
                NO_DRIVER,
            ),
            reading(
                "Mainboard",
                "Processor zone (TZ00)",
                SensorKind::Temperature,
                Some(cpu_temp),
                zone,
                "",
            ),
            reading(
                "Mainboard",
                "Chassis (TZ01)",
                SensorKind::Temperature,
                Some(34.0 + heat * 0.34 + 1.0 * noise(42, t / 60.0)),
                zone,
                "",
            ),
            reading(
                "Mainboard",
                "Charger (TZ02)",
                SensorKind::Temperature,
                Some(31.0 + slope.max(0.0) * 0.33 + heat * 0.1),
                zone,
                "",
            ),
            reading(
                "Mainboard",
                "Wireless module (TZ03)",
                SensorKind::Temperature,
                Some(37.0 + 6.0 * sc.video + 9.0 * sc.download + heat * 0.12),
                zone,
                "",
            ),
            reading(
                "Mainboard",
                "System fan",
                SensorKind::Fan,
                Some((fan_rpm / 10.0).round() * 10.0),
                "ACPI fan device",
                "",
            ),
            reading(
                "Mainboard",
                "Second fan",
                SensorKind::Fan,
                None,
                "",
                NO_DRIVER,
            ),
            reading(
                "Mainboard",
                "+12 V rail",
                SensorKind::Voltage,
                None,
                "",
                NO_DRIVER,
            ),
            reading(
                "Mainboard",
                "+5 V rail",
                SensorKind::Voltage,
                None,
                "",
                NO_DRIVER,
            ),
        ];

        let processes = if detail {
            processes(t, &sc, total, dgpu_load, igpu_load)
        } else {
            Vec::new()
        };

        Sample {
            time: unix,
            uptime_s: (UPTIME_AT_START_S + t).max(0.0) as u64,
            cpu: CpuSample {
                total_pct: total as f32,
                kernel_pct: (total * (0.16 + 0.08 * noise(8, t / 20.0))) as f32,
                per_core_pct: per_core,
                per_core_mhz,
                current_mhz: Some(mhz.clamp(800.0, 5100.0) as f32),
                temperature_c: Some(cpu_temp as f32),
                temperature_note: s("ACPI thermal zone"),
                processes: 212 + (6.0 * noise(9, t / 120.0)) as u32 + (8.0 * sc.burst) as u32,
                threads: 3080 + (160.0 * noise(10, t / 90.0)) as u32,
                handles: 96_400 + (4200.0 * noise(13, t / 150.0)) as u32,
                context_switches_per_s: Some(9000.0 + total * 620.0),
                interrupts_per_s: Some(5200.0 + total * 310.0),
            },
            memory,
            gpus,
            disks,
            net,
            battery: Some(battery),
            sensors,
            processes,
        }
    }
}

/// Alert events of the invented last four hours, oldest first: the alert engine run
/// over the demo readings with these rules. Alerts that were already active
/// four hours ago (and still are) are left out; the live monitor reports those.
pub fn past_events(rules: &[AlertRule], now_unix: u64) -> Vec<AlertEvent> {
    let slow = slow();
    let mut source = DemoSource::new();
    let mut engine = Engine::new();
    let mut out = Vec::new();
    let mut t = -4.0 * 3600.0_f64;
    let first = t;
    while t < -1.0 {
        let sample = source.sample(t, (now_unix as f64 + t) as u64, false);
        let events = engine.evaluate(rules, &alerts::readings(&sample, &slow), t, sample.time);
        if t > first {
            out.extend(events);
        }
        t += 5.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::Metric;

    fn times() -> Vec<f64> {
        let mut v: Vec<f64> = (0..=288)
            .map(|i| -86_400.0 + f64::from(i) * 300.0)
            .collect();
        v.extend([-455.0, -450.0, -1.0, 0.0, 0.5, 17.0, 3600.0, 40_000.0]);
        v
    }

    #[test]
    fn readings_stay_in_range() {
        let mut src = DemoSource::new();
        for t in times() {
            let sm = src.sample(t, 1_800_000_000, true);
            let in_pct = |v: f32| (0.0..=100.0).contains(&v);
            assert!(in_pct(sm.cpu.total_pct), "cpu at {t}");
            assert!(sm.cpu.kernel_pct <= sm.cpu.total_pct);
            assert_eq!(sm.cpu.per_core_pct.len(), THREADS);
            assert_eq!(sm.cpu.per_core_mhz.len(), THREADS);
            assert!(sm.cpu.per_core_pct.iter().all(|v| in_pct(*v)));
            let temp = sm.cpu.temperature_c.unwrap();
            assert!((30.0..=100.0).contains(&temp), "temp {temp} at {t}");
            for g in &sm.gpus {
                assert!(in_pct(g.load_pct));
                assert!(g
                    .engines
                    .iter()
                    .all(|e| in_pct(e.pct) && !e.name.is_empty()));
            }
            assert!(sm.gpus[1].dedicated_used <= 8 * GB);
            for d in &sm.disks {
                assert!(in_pct(d.active_pct));
                assert!(d.read_bps >= 0.0 && d.write_bps >= 0.0);
            }
            for n in &sm.net {
                assert!(n.rx_bps >= 0.0 && n.tx_bps >= 0.0);
            }
            let b = sm.battery.as_ref().unwrap();
            assert!((20.0..=100.0).contains(&b.percent), "battery at {t}");
            assert!(b.remaining_mwh.unwrap() <= b.full_mwh.unwrap());
            assert!(sm.processes.iter().all(|p| in_pct(p.cpu_pct)));
            let busy: f32 = sm
                .processes
                .iter()
                .filter(|p| p.pid != 0)
                .map(|p| p.cpu_pct)
                .sum();
            assert!(
                busy <= sm.cpu.total_pct + 0.5,
                "processes exceed total at {t}"
            );
        }
    }

    #[test]
    fn memory_parts_add_up() {
        let mut src = DemoSource::new();
        for t in times() {
            let m = src.sample(t, 0, false).memory;
            assert_eq!(m.used + m.available, m.total);
            assert_eq!(m.standby.unwrap() + m.free, m.available);
            assert!(m.modified.unwrap() < m.used);
            assert_eq!(m.cached, m.standby.unwrap() + m.modified.unwrap());
            assert!(m.commit_used <= m.commit_limit);
            assert!(m.pagefile_used <= m.pagefile_total);
            assert_eq!(m.total + m.hardware_reserved.unwrap(), 32 * GB);
        }
    }

    #[test]
    fn deterministic_for_the_same_time() {
        let mut a = DemoSource::new();
        let mut b = DemoSource::new();
        for t in [-80_000.0, -450.0, 0.0, 12.5] {
            assert_eq!(a.sample(t, 7, true), b.sample(t, 7, true));
            // Asking again, or after other times, changes nothing.
            let _ = a.sample(t + 1000.0, 7, false);
            assert_eq!(a.sample(t, 7, false), b.sample(t, 7, false));
        }
    }

    #[test]
    fn prefill_path_is_light() {
        let mut src = DemoSource::new();
        let quick = src.sample(-5000.0, 0, false);
        assert!(quick.processes.is_empty());
        assert!(quick
            .net
            .iter()
            .all(|n| n.description.is_empty() && n.ipv4.is_empty()));
        let full = src.sample(-5000.0, 0, true);
        assert!(full.processes.len() >= 40);
        // The numbers that go into the graphs are the same either way.
        assert_eq!(quick.cpu, full.cpu);
        assert_eq!(quick.memory, full.memory);
        assert_eq!(quick.gpus, full.gpus);
        assert_eq!(quick.disks, full.disks);
        assert_eq!(quick.sensors, full.sensors);
    }

    #[test]
    fn nothing_is_left_unnamed() {
        let inv = inventory();
        assert!(!inv.cpu.name.is_empty() && !inv.cpu.vendor.is_empty());
        assert_eq!(inv.cpu.threads as usize, THREADS);
        assert_eq!(
            inv.cpu.performance_cores.unwrap() * 2 + inv.cpu.efficiency_cores.unwrap(),
            inv.cpu.threads
        );
        assert_eq!(inv.memory_modules.iter().filter(|m| m.populated).count(), 2);
        assert_eq!(inv.memory_modules.len(), 4);
        assert!(inv
            .gpus
            .iter()
            .all(|g| !g.name.is_empty() && !g.id.is_empty()));
        assert_eq!(inv.displays.len(), 2);
        assert!(inv
            .displays
            .iter()
            .all(|d| !d.name.is_empty() && d.width > 0));
        let slow = slow();
        assert_eq!(slow.drives.len(), 3);
        assert_eq!(slow.volumes.len(), 4);
        for d in &slow.drives {
            assert!(!d.model.is_empty() && !d.serial.is_empty());
            assert!(d
                .attributes
                .iter()
                .all(|a| !a.name.is_empty() && !a.display.is_empty() && !a.explanation.is_empty()));
            let parts: u64 = d.partitions.iter().map(|p| p.size).sum();
            assert!(parts <= d.size_bytes);
            assert_eq!(d.attributes.is_empty(), !d.smart_note.is_empty());
        }
        assert!(slow.volumes.iter().all(|v| v.free < v.total));
        let sample = DemoSource::new().sample(0.0, 0, true);
        assert!(sample.processes.iter().all(|p| !p.name.is_empty()));
        let mut pids: Vec<u32> = sample.processes.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        assert_eq!(pids.len(), sample.processes.len(), "process ids are unique");
        assert!(sample
            .net
            .iter()
            .all(|n| !n.name.is_empty() && !n.kind.is_empty()));
        assert!(sample
            .sensors
            .iter()
            .all(|r| !r.group.is_empty() && !r.name.is_empty()));
        assert!(sample
            .sensors
            .iter()
            .all(|r| r.value.is_some() || !r.note.is_empty()));
        // The sample's adapters and drives match the inventory.
        assert!(sample
            .gpus
            .iter()
            .all(|g| inv.gpus.iter().any(|i| i.id == g.id)));
        assert!(sample
            .disks
            .iter()
            .all(|d| slow.drives.iter().any(|i| i.index == d.index)));
    }

    #[test]
    fn the_demo_triggers_alerts() {
        let rules = alerts::default_rules();
        let slow = slow();
        // Now: the nearly full volume is past the default limit.
        let sample = DemoSource::new().sample(0.0, 0, true);
        let mut engine = Engine::new();
        let events = engine.evaluate(&rules, &alerts::readings(&sample, &slow), 0.0, 0);
        assert!(events
            .iter()
            .any(|e| e.metric == Metric::DiskFree && e.subject == "D:" && e.raised));
        // The battery is discharging now, without being low.
        let b = sample.battery.unwrap();
        assert_eq!(b.state, BatteryState::Discharging);
        assert!(b.percent > 50.0 && b.rate_mw.unwrap() < 0);
        // Earlier: the processor ran hot for a moment and recovered.
        let past = past_events(&rules, 1_800_000_000);
        let hot: Vec<_> = past
            .iter()
            .filter(|e| e.metric == Metric::CpuTemp)
            .collect();
        assert_eq!(hot.len(), 2, "{past:?}");
        assert!(hot[0].raised && !hot[1].raised);
        assert!(hot[0].time < hot[1].time && hot[1].time < 1_800_000_000);
        assert!(past.iter().all(|e| e.metric != Metric::DiskFree));
        assert!(past.windows(2).all(|w| w[0].time <= w[1].time));
    }

    #[test]
    fn battery_curve_is_continuous() {
        let mut last = battery_curve(-24.0).0;
        let mut x = -24.0;
        while x < 6.0 {
            let (pct, _) = battery_curve(x);
            assert!((pct - last).abs() < 1.0, "jump at {x}");
            assert!((BATTERY_FLOOR_PCT..=100.0).contains(&pct));
            last = pct;
            x += 0.01;
        }
    }
}
