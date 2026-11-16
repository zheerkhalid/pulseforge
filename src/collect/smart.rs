//! Decoding of ATA SMART attributes and the NVMe health log into plain
//! language. Pure functions: no device access here.

use crate::model::{HealthStatus, SmartAttribute};
use crate::util::{fmt_bytes, fmt_count, fmt_hours};

/// What kind of drive the attributes come from. It decides how a few
/// attributes are named and whether a "life left" figure makes sense.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Nvme,
    AtaSsd,
    AtaHdd,
}

/// Names of the NVMe health-log rows that [`summarize`] looks for.
pub const NVME_CRITICAL_WARNING: &str = "Critical warning";
pub const NVME_TEMPERATURE: &str = "Temperature";
pub const NVME_AVAILABLE_SPARE: &str = "Available spare";
pub const NVME_SPARE_THRESHOLD: &str = "Available spare threshold";
pub const NVME_PERCENTAGE_USED: &str = "Percentage used";
pub const NVME_DATA_READ: &str = "Data read";
pub const NVME_DATA_WRITTEN: &str = "Data written";
pub const NVME_POWER_CYCLES: &str = "Power cycles";
pub const NVME_POWER_ON_HOURS: &str = "Power-on hours";

/// Bytes in one sector of a SMART data block or one NVMe health log page.
pub const BLOCK: usize = 512;

/// The overall picture of a drive, derived from its attribute list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub health: HealthStatus,
    /// Remaining life in percent where the drive reports it.
    pub health_pct: Option<u8>,
    pub health_summary: String,
    pub temperature_c: Option<f32>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub total_written_bytes: Option<u64>,
    pub total_read_bytes: Option<u64>,
}

// ----------------------------------------------------------------------------------
// ATA
// ----------------------------------------------------------------------------------

/// How the raw value of an ATA attribute is read and judged.
#[derive(Clone, Copy, PartialEq)]
enum Fmt {
    /// A plain counter shown for information.
    Count,
    /// A counter where any non-zero value is a warning sign: (caution from, bad from).
    Critical(u64, u64),
    /// Interface CRC errors: a cable hint.
    Crc,
    Hours,
    /// Temperature in the low byte of the raw value.
    Temp,
    /// Logical blocks of 512 bytes.
    Lbas,
    /// The normalised value is the percentage of life left.
    Life,
    Millis,
    /// Vendor-specific encoding: judged by the threshold only.
    Opaque,
}

struct Spec {
    name: &'static str,
    fmt: Fmt,
    meaning: &'static str,
}

fn ata_spec(id: u8, kind: DriveKind) -> Spec {
    let hdd = kind == DriveKind::AtaHdd;
    let (name, fmt, meaning) = match id {
        1 => (
            "Read error rate",
            Fmt::Opaque,
            "How often the drive had to correct errors while reading (the raw number is vendor-specific)",
        ),
        2 => (
            "Throughput performance",
            Fmt::Opaque,
            "The drive's own rating of its overall throughput",
        ),
        3 => (
            "Spin-up time",
            Fmt::Millis,
            "How long the platters take to reach full speed",
        ),
        4 => (
            "Start/stop count",
            Fmt::Count,
            "How many times the drive has started and stopped spinning",
        ),
        5 => (
            "Reallocated sectors",
            Fmt::Critical(1, 200),
            "Damaged sectors the drive has replaced with spare ones",
        ),
        7 => (
            "Seek error rate",
            Fmt::Opaque,
            "How often the heads missed their target track (the raw number is vendor-specific)",
        ),
        8 => (
            "Seek time performance",
            Fmt::Opaque,
            "The drive's own rating of how fast its heads move",
        ),
        9 => (
            "Power-on hours",
            Fmt::Hours,
            "The total time the drive has been switched on",
        ),
        10 => (
            "Spin retry count",
            Fmt::Critical(1, u64::MAX),
            "Times the drive needed more than one attempt to spin up",
        ),
        11 => (
            "Calibration retries",
            Fmt::Count,
            "Times the drive had to repeat its head calibration",
        ),
        12 => (
            "Power cycle count",
            Fmt::Count,
            "How many times the drive has been switched on",
        ),
        13 => (
            "Soft read error rate",
            Fmt::Opaque,
            "Read errors the drive corrected on its own",
        ),
        169 => (
            "Remaining life",
            Fmt::Life,
            "The share of the drive's rated write endurance that is left",
        ),
        170 => (
            "Available reserved space",
            Fmt::Opaque,
            "Spare flash blocks kept for replacing worn ones",
        ),
        171 => (
            "Program fail count",
            Fmt::Count,
            "Times writing to a flash block failed",
        ),
        172 => (
            "Erase fail count",
            Fmt::Count,
            "Times erasing a flash block failed",
        ),
        173 => (
            "Wear leveling count",
            Fmt::Opaque,
            "How evenly and how often the flash blocks have been erased",
        ),
        174 => (
            "Unexpected power loss",
            Fmt::Count,
            "Times the power was cut without a clean shutdown",
        ),
        175 => (
            "Program fail count (chip)",
            Fmt::Count,
            "Times writing to a flash block failed, counted per chip",
        ),
        176 => (
            "Erase fail count (chip)",
            Fmt::Count,
            "Times erasing a flash block failed, counted per chip",
        ),
        177 => (
            "Wear leveling (life left)",
            Fmt::Life,
            "The share of the drive's rated erase cycles that is left",
        ),
        178 => (
            "Used reserved blocks (chip)",
            Fmt::Count,
            "Spare flash blocks already used on the most worn chip",
        ),
        179 => (
            "Used reserved blocks",
            Fmt::Count,
            "Spare flash blocks already used to replace worn ones",
        ),
        180 => (
            "Unused reserved blocks",
            Fmt::Count,
            "Spare flash blocks still available",
        ),
        181 => (
            "Program fail count (total)",
            Fmt::Count,
            "Times writing to a flash block failed over the drive's life",
        ),
        182 => (
            "Erase fail count (total)",
            Fmt::Count,
            "Times erasing a flash block failed over the drive's life",
        ),
        183 => (
            "Runtime bad blocks",
            Fmt::Count,
            "Blocks that went bad during use (on some drives: interface speed downshifts)",
        ),
        184 => (
            "End-to-end errors",
            Fmt::Critical(1, u64::MAX),
            "Data that was damaged inside the drive between its cache and the media",
        ),
        187 => (
            "Reported uncorrectable errors",
            Fmt::Critical(1, 1000),
            "Read errors the drive could not correct and had to report",
        ),
        188 => (
            "Command timeouts",
            Fmt::Count,
            "Commands that were aborted because the drive did not answer in time",
        ),
        189 => (
            "High fly writes",
            Fmt::Count,
            "Writes made while a head was flying higher than it should",
        ),
        190 => (
            "Airflow temperature",
            Fmt::Temp,
            "The temperature of the air inside the drive",
        ),
        191 => (
            "Shock sensor events",
            Fmt::Count,
            "Errors caused by knocks or vibration",
        ),
        192 => (
            "Unsafe shutdowns",
            Fmt::Count,
            "Times the drive lost power without being shut down properly",
        ),
        193 => (
            "Load cycle count",
            Fmt::Count,
            "How many times the heads have been parked and released",
        ),
        194 => ("Temperature", Fmt::Temp, "The temperature of the drive"),
        195 => (
            "Hardware ECC recovered",
            Fmt::Opaque,
            "Errors fixed by the built-in error correction (the raw number is vendor-specific)",
        ),
        196 => (
            "Reallocation events",
            Fmt::Critical(1, u64::MAX),
            "Attempts to move data from a damaged sector to a spare one",
        ),
        197 => (
            "Pending sectors",
            Fmt::Critical(1, 100),
            "Unstable sectors waiting to be checked and possibly replaced",
        ),
        198 => (
            "Uncorrectable sectors",
            Fmt::Critical(1, 100),
            "Sectors that could not be read or written even after retries",
        ),
        199 => (
            "Interface CRC errors",
            Fmt::Crc,
            "Transfers between drive and computer that arrived damaged",
        ),
        200 => (
            "Write error rate",
            Fmt::Opaque,
            "Errors found while writing",
        ),
        201 => (
            "Soft read error rate",
            Fmt::Opaque,
            "Read errors the drive corrected on its own",
        ),
        202 if hdd => (
            "Data address mark errors",
            Fmt::Count,
            "Errors finding the start of a data block on the platter",
        ),
        202 => (
            "Lifetime remaining",
            Fmt::Life,
            "The share of the drive's rated write endurance that is left",
        ),
        206 => (
            "Flying height",
            Fmt::Opaque,
            "How high the heads fly above the platters",
        ),
        225 => (
            "Host writes",
            Fmt::Count,
            "Data written by the computer, in a vendor-specific unit",
        ),
        226 => (
            "Workload media wear",
            Fmt::Opaque,
            "Wear caused by the recent workload (vendor-specific)",
        ),
        227 => (
            "Workload read/write ratio",
            Fmt::Opaque,
            "The mix of reading and writing in the recent workload (vendor-specific)",
        ),
        228 => (
            "Power-off retract count",
            Fmt::Count,
            "Times the heads were pulled back because the power went away",
        ),
        229 => (
            "Vendor-specific 229",
            Fmt::Opaque,
            "A manufacturer-specific value",
        ),
        230 => (
            "Drive life protection status",
            Fmt::Opaque,
            "Whether the drive is slowing writes down to protect its lifespan (vendor-specific)",
        ),
        231 => (
            "SSD life left",
            Fmt::Life,
            "The share of the drive's rated write endurance that is left",
        ),
        232 => (
            "Available reserved space",
            Fmt::Opaque,
            "Spare flash capacity still available for replacing worn blocks",
        ),
        233 => (
            "Media wearout indicator",
            Fmt::Life,
            "The share of the drive's rated erase cycles that is left",
        ),
        234 => (
            "Vendor-specific 234",
            Fmt::Opaque,
            "A manufacturer-specific value (often erase counts or thermal throttling)",
        ),
        235 => (
            "Power-loss recovery count",
            Fmt::Count,
            "Times the drive had to recover after losing power (vendor-specific)",
        ),
        241 => (
            "Total data written",
            Fmt::Lbas,
            "All the data the computer has written to this drive",
        ),
        242 => (
            "Total data read",
            Fmt::Lbas,
            "All the data the computer has read from this drive",
        ),
        243 => (
            "Total data written (extended)",
            Fmt::Count,
            "Upper part of the written-data counter (vendor-specific)",
        ),
        249 => (
            "Flash writes",
            Fmt::Count,
            "Data written to the flash cells, in a vendor-specific unit",
        ),
        250 => (
            "Read error retry rate",
            Fmt::Opaque,
            "How often a read had to be repeated",
        ),
        251 => (
            "Minimum spares remaining",
            Fmt::Opaque,
            "The lowest share of spare blocks left on any part of the drive",
        ),
        252 => (
            "Newly added bad blocks",
            Fmt::Count,
            "Flash blocks that went bad after the drive left the factory",
        ),
        254 => (
            "Free-fall events",
            Fmt::Count,
            "Times the drive detected that it was falling and parked its heads",
        ),
        _ => (
            "",
            Fmt::Opaque,
            "A manufacturer-specific value without an agreed meaning",
        ),
    };
    Spec { name, fmt, meaning }
}

/// A temperature byte that can be believed.
fn plausible_celsius(value: u64) -> Option<u8> {
    let v = (value & 0xFF) as u8;
    (1..=125).contains(&v).then_some(v)
}

fn sentence(meaning: &str, verdict: &str) -> String {
    format!("{meaning}: {verdict}.")
}

/// Decodes the SMART READ DATA sector and the matching thresholds sector of
/// an ATA drive. Returns an empty list when the data is not a SMART block.
pub fn parse_ata(data: &[u8], thresholds: &[u8], kind: DriveKind) -> Vec<SmartAttribute> {
    if data.len() != BLOCK {
        return Vec::new();
    }
    let mut out: Vec<SmartAttribute> = Vec::new();
    for slot in 0..30 {
        let Some(entry) = data.get(2 + slot * 12..2 + slot * 12 + 12) else {
            break;
        };
        let id = entry[0];
        if id == 0 {
            continue;
        }
        if out.iter().any(|a| a.id == Some(id)) {
            // The same id twice: this is not a SMART data block.
            return Vec::new();
        }
        let current = entry[3];
        let worst = entry[4];
        let mut raw_bytes = [0u8; 8];
        raw_bytes[..6].copy_from_slice(&entry[5..11]);
        let raw = u64::from_le_bytes(raw_bytes);
        let threshold = if thresholds.len() == BLOCK {
            (0..30)
                .filter_map(|i| thresholds.get(2 + i * 12..2 + i * 12 + 2))
                .find(|t| t[0] == id)
                .map(|t| t[1])
        } else {
            None
        };
        out.push(ata_attribute(id, current, worst, threshold, raw, kind));
    }
    out
}

fn ata_attribute(
    id: u8,
    current: u8,
    worst: u8,
    threshold: Option<u8>,
    raw: u64,
    kind: DriveKind,
) -> SmartAttribute {
    let spec = ata_spec(id, kind);
    let name = if spec.name.is_empty() {
        format!("Vendor-specific {id}")
    } else {
        spec.name.to_string()
    };
    let low32 = raw & 0xFFFF_FFFF;
    let failed =
        spec.fmt != Fmt::Temp && matches!(threshold, Some(t) if t > 0 && t != 0xFF && current <= t);
    let (number, display, mut status, mut verdict) = match spec.fmt {
        Fmt::Count => (
            raw,
            fmt_count(raw),
            HealthStatus::Good,
            "shown for information, not a sign of trouble".to_string(),
        ),
        Fmt::Opaque => (
            raw,
            fmt_count(raw),
            HealthStatus::Good,
            "the drive rates this as within its limits".to_string(),
        ),
        Fmt::Critical(caution, bad) => {
            let (status, verdict) = if low32 >= bad {
                (
                    HealthStatus::Bad,
                    format!(
                        "{} so far, which is a lot; back up your data and replace the drive",
                        fmt_count(low32)
                    ),
                )
            } else if low32 >= caution {
                (
                    HealthStatus::Caution,
                    format!(
                        "{} so far; keep a backup and watch whether the number grows",
                        fmt_count(low32)
                    ),
                )
            } else {
                (HealthStatus::Good, "none, which is ideal".to_string())
            };
            (low32, fmt_count(low32), status, verdict)
        }
        Fmt::Crc => {
            let (status, verdict) = if low32 > 0 {
                (
                    HealthStatus::Caution,
                    format!(
                        "{} so far, which usually points to a loose or faulty data cable rather than the drive itself",
                        fmt_count(low32)
                    ),
                )
            } else {
                (HealthStatus::Good, "none, which is ideal".to_string())
            };
            (low32, fmt_count(low32), status, verdict)
        }
        Fmt::Hours => (
            low32,
            format!("{} hours ({})", fmt_count(low32), fmt_hours(low32)),
            HealthStatus::Good,
            "shown for information, not a sign of trouble".to_string(),
        ),
        Fmt::Millis => {
            let ms = raw & 0xFFFF;
            (
                ms,
                format!("{} ms", fmt_count(ms)),
                HealthStatus::Good,
                "the drive rates this as within its limits".to_string(),
            )
        }
        Fmt::Lbas => {
            let bytes = raw.saturating_mul(512);
            (
                raw,
                fmt_bytes(bytes),
                HealthStatus::Good,
                "shown for information, assuming the usual 512-byte unit".to_string(),
            )
        }
        Fmt::Temp => {
            match plausible_celsius(raw) {
                Some(c) => {
                    let limit = if kind == DriveKind::AtaHdd { 55 } else { 70 };
                    let (status, verdict) = if c >= limit {
                        (
                        HealthStatus::Caution,
                        format!("{c} °C is hot for this kind of drive; improve the airflow around it"),
                    )
                    } else {
                        (HealthStatus::Good, format!("{c} °C is a normal value"))
                    };
                    (u64::from(c), format!("{c} °C"), status, verdict)
                }
                None => (
                    raw,
                    "no reading".to_string(),
                    HealthStatus::Unknown,
                    "the drive does not report a believable value".to_string(),
                ),
            }
        }
        Fmt::Life => {
            if current <= 100 {
                let (status, verdict) = if current <= 10 {
                    (
                        HealthStatus::Caution,
                        format!("only about {current} % is left; plan to replace the drive"),
                    )
                } else {
                    (HealthStatus::Good, format!("about {current} % is left"))
                };
                (raw, format!("{current} %"), status, verdict)
            } else {
                (
                    raw,
                    fmt_count(raw),
                    HealthStatus::Good,
                    "the drive rates this as within its limits".to_string(),
                )
            }
        }
    };
    if failed {
        status = HealthStatus::Bad;
        verdict = "the value has fallen to the limit set by the manufacturer, which the drive itself counts as a failure".to_string();
    }
    SmartAttribute {
        id: Some(id),
        name,
        current: Some(current),
        worst: Some(worst),
        threshold,
        raw: number,
        display,
        status,
        explanation: sentence(spec.meaning, &verdict),
    }
}

// ----------------------------------------------------------------------------------
// NVMe
// ----------------------------------------------------------------------------------

fn le128(log: &[u8], offset: usize) -> u128 {
    let mut a = [0u8; 16];
    if let Some(b) = log.get(offset..offset + 16) {
        a.copy_from_slice(b);
    }
    u128::from_le_bytes(a)
}

fn clamp64(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

fn kelvin_to_celsius(kelvin: u16) -> Option<i32> {
    // Anything outside -60..=150 °C is "not reported" or garbage.
    (213..=423)
        .contains(&kelvin)
        .then(|| i32::from(kelvin) - 273)
}

fn nvme_row(
    name: &str,
    raw: u64,
    display: String,
    status: HealthStatus,
    text: String,
) -> SmartAttribute {
    SmartAttribute {
        id: None,
        name: name.to_string(),
        current: None,
        worst: None,
        threshold: None,
        raw,
        display,
        status,
        explanation: text,
    }
}

/// Decodes the NVMe SMART / Health Information log page (log identifier 2).
/// Returns an empty list when the data is not a believable health log.
pub fn parse_nvme(log: &[u8]) -> Vec<SmartAttribute> {
    use HealthStatus::{Bad, Caution, Good};
    if log.len() != BLOCK || log.iter().all(|&b| b == 0) {
        return Vec::new();
    }
    let warning = log[0];
    let kelvin = u16::from_le_bytes([log[1], log[2]]);
    let spare = log[3];
    let spare_threshold = log[4];
    let used = log[5];
    if spare > 100 || spare_threshold > 100 {
        return Vec::new();
    }
    let info = "shown for information, not a sign of trouble";
    let mut out = Vec::new();

    // Critical warning bits.
    let mut problems: Vec<&str> = Vec::new();
    let mut warning_status = Good;
    for (bit, text, status) in [
        (0u8, "spare capacity is below its limit", Bad),
        (1, "temperature is outside its limits", Caution),
        (2, "reliability is degraded", Bad),
        (3, "the drive has switched to read-only", Bad),
        (4, "the backup power for cached data has failed", Bad),
        (5, "a persistent memory region is unreliable", Caution),
    ] {
        if warning & (1 << bit) != 0 {
            problems.push(text);
            warning_status = warning_status.worst(status);
        }
    }
    out.push(nvme_row(
        NVME_CRITICAL_WARNING,
        u64::from(warning),
        if problems.is_empty() {
            "None".to_string()
        } else {
            let mut text = problems.join("; ");
            if let Some(first) = text.get(..1) {
                text = first.to_uppercase() + &text[1..];
            }
            text
        },
        warning_status,
        sentence(
            "Warnings the drive raises itself when something needs attention",
            if problems.is_empty() {
                "none are raised, which is ideal"
            } else {
                "at least one is raised, so back up your data and check this drive"
            },
        ),
    ));

    match kelvin_to_celsius(kelvin) {
        Some(c) => {
            let (status, verdict) = if c >= 70 {
                (
                    Caution,
                    format!("{c} °C is hot; the drive may slow down to protect itself, so improve the airflow around it"),
                )
            } else {
                (Good, format!("{c} °C is a normal value"))
            };
            out.push(nvme_row(
                NVME_TEMPERATURE,
                c.max(0) as u64,
                format!("{c} °C"),
                status,
                sentence("The overall temperature of the drive", &verdict),
            ));
        }
        None => out.push(nvme_row(
            NVME_TEMPERATURE,
            0,
            "no reading".to_string(),
            HealthStatus::Unknown,
            sentence(
                "The overall temperature of the drive",
                "the drive does not report a believable value",
            ),
        )),
    }

    let (status, verdict) = if spare < spare_threshold {
        (
            Bad,
            format!("{spare} % is below the drive's own limit of {spare_threshold} %; back up your data and replace the drive"),
        )
    } else if spare_threshold > 0 && spare < spare_threshold.saturating_add(10) {
        (
            Caution,
            format!("{spare} % is close to the drive's own limit of {spare_threshold} %; plan to replace the drive"),
        )
    } else {
        (Good, format!("{spare} % is left, which is fine"))
    };
    out.push(nvme_row(
        NVME_AVAILABLE_SPARE,
        u64::from(spare),
        format!("{spare} %"),
        status,
        sentence(
            "Spare flash capacity kept for replacing worn-out cells",
            &verdict,
        ),
    ));
    out.push(nvme_row(
        NVME_SPARE_THRESHOLD,
        u64::from(spare_threshold),
        format!("{spare_threshold} %"),
        Good,
        sentence(
            "The spare capacity below which the drive raises a warning",
            "a fixed limit set by the manufacturer",
        ),
    ));

    let (status, verdict) = if used >= 100 {
        (
            Caution,
            format!("{used} % means the rated write endurance is used up; the drive may keep working, but plan to replace it"),
        )
    } else if used >= 90 {
        (
            Caution,
            format!("{used} % of the rated write endurance is used; plan to replace the drive"),
        )
    } else {
        (
            Good,
            format!("{used} % of the rated write endurance is used, which is fine"),
        )
    };
    out.push(nvme_row(
        NVME_PERCENTAGE_USED,
        u64::from(used),
        format!("{used} %"),
        status,
        sentence(
            "How much of the life the manufacturer rates the drive for has been used",
            &verdict,
        ),
    ));

    // Data units are thousands of 512-byte blocks.
    for (name, offset, meaning) in [
        (
            NVME_DATA_READ,
            32usize,
            "All the data the computer has read from this drive",
        ),
        (
            NVME_DATA_WRITTEN,
            48,
            "All the data the computer has written to this drive",
        ),
    ] {
        let bytes = clamp64(le128(log, offset).saturating_mul(512_000));
        out.push(nvme_row(
            name,
            bytes,
            fmt_bytes(bytes),
            Good,
            sentence(meaning, info),
        ));
    }

    let counters: [(&str, usize, &str); 5] = [
        (
            "Host read commands",
            64,
            "How many read requests the drive has completed",
        ),
        (
            "Host write commands",
            80,
            "How many write requests the drive has completed",
        ),
        (
            "Controller busy time",
            96,
            "Minutes the drive's controller has spent working on requests",
        ),
        (
            NVME_POWER_CYCLES,
            112,
            "How many times the drive has been switched on",
        ),
        (
            NVME_POWER_ON_HOURS,
            128,
            "The total time the drive has been switched on",
        ),
    ];
    for (name, offset, meaning) in counters {
        let v = clamp64(le128(log, offset));
        let display = match name {
            NVME_POWER_ON_HOURS => format!("{} hours ({})", fmt_count(v), fmt_hours(v)),
            "Controller busy time" => format!("{} min", fmt_count(v)),
            _ => fmt_count(v),
        };
        out.push(nvme_row(name, v, display, Good, sentence(meaning, info)));
    }

    let unsafe_shutdowns = clamp64(le128(log, 144));
    out.push(nvme_row(
        "Unsafe shutdowns",
        unsafe_shutdowns,
        fmt_count(unsafe_shutdowns),
        Good,
        sentence(
            "Times the drive lost power without being shut down properly",
            info,
        ),
    ));

    let media = clamp64(le128(log, 160));
    let (status, verdict) = if media >= 1000 {
        (
            Bad,
            format!(
                "{} so far, which is a lot; back up your data and replace the drive",
                fmt_count(media)
            ),
        )
    } else if media > 0 {
        (
            Caution,
            format!(
                "{} so far; keep a backup and watch whether the number grows",
                fmt_count(media)
            ),
        )
    } else {
        (Good, "none, which is ideal".to_string())
    };
    out.push(nvme_row(
        "Media errors",
        media,
        fmt_count(media),
        status,
        sentence(
            "Times the drive could not read back data it had stored",
            &verdict,
        ),
    ));

    let errors = clamp64(le128(log, 176));
    out.push(nvme_row(
        "Error log entries",
        errors,
        fmt_count(errors),
        Good,
        sentence(
            "Entries in the drive's internal error log, which also records harmless rejected commands",
            info,
        ),
    ));

    let warm = u64::from(u32::from_le_bytes([log[192], log[193], log[194], log[195]]));
    out.push(nvme_row(
        "Warning temperature time",
        warm,
        format!("{} min", fmt_count(warm)),
        Good,
        sentence(
            "Minutes the drive has spent above its warning temperature",
            if warm > 0 {
                "it has been hot at times, so check the airflow around it"
            } else {
                "never, which is ideal"
            },
        ),
    ));
    let critical = u64::from(u32::from_le_bytes([log[196], log[197], log[198], log[199]]));
    out.push(nvme_row(
        "Critical temperature time",
        critical,
        format!("{} min", fmt_count(critical)),
        if critical > 0 { Caution } else { Good },
        sentence(
            "Minutes the drive has spent above its critical temperature",
            if critical > 0 {
                "it has overheated before, so improve the cooling around it"
            } else {
                "never, which is ideal"
            },
        ),
    ));

    for sensor in 0..8usize {
        let at = 200 + sensor * 2;
        let k = u16::from_le_bytes([log[at], log[at + 1]]);
        if k == 0 {
            continue;
        }
        if let Some(c) = kelvin_to_celsius(k) {
            out.push(nvme_row(
                &format!("Temperature sensor {}", sensor + 1),
                c.max(0) as u64,
                format!("{c} °C"),
                Good,
                sentence("An additional temperature sensor inside the drive", info),
            ));
        }
    }
    out
}

// ----------------------------------------------------------------------------------
// Summary
// ----------------------------------------------------------------------------------

/// Turns an attribute list into the overall verdict and the handful of
/// headline numbers. An empty list gives `Unknown` and nothing else.
pub fn summarize(attributes: &[SmartAttribute], kind: DriveKind) -> Summary {
    let mut s = Summary::default();
    if attributes.is_empty() {
        return s;
    }
    let by_id = |id: u8| attributes.iter().find(|a| a.id == Some(id));
    let by_name = |name: &str| attributes.iter().find(|a| a.id.is_none() && a.name == name);
    let known = |a: &&SmartAttribute| a.status != HealthStatus::Unknown;

    match kind {
        DriveKind::Nvme => {
            s.temperature_c = by_name(NVME_TEMPERATURE)
                .filter(known)
                .map(|a| a.raw as f32);
            s.power_on_hours = by_name(NVME_POWER_ON_HOURS).map(|a| a.raw);
            s.power_cycles = by_name(NVME_POWER_CYCLES).map(|a| a.raw);
            s.total_written_bytes = by_name(NVME_DATA_WRITTEN).map(|a| a.raw);
            s.total_read_bytes = by_name(NVME_DATA_READ).map(|a| a.raw);
            s.health_pct =
                by_name(NVME_PERCENTAGE_USED).map(|a| 100u8.saturating_sub(a.raw.min(100) as u8));
        }
        DriveKind::AtaSsd | DriveKind::AtaHdd => {
            s.temperature_c = [194u8, 190]
                .iter()
                .filter_map(|&id| by_id(id))
                .find(known)
                .map(|a| a.raw as f32);
            s.power_on_hours = by_id(9).map(|a| a.raw);
            s.power_cycles = by_id(12).map(|a| a.raw);
            s.total_written_bytes = by_id(241).map(|a| a.raw.saturating_mul(512));
            s.total_read_bytes = by_id(242).map(|a| a.raw.saturating_mul(512));
            if kind == DriveKind::AtaSsd {
                s.health_pct = [231u8, 233, 169, 177, 202]
                    .iter()
                    .filter_map(|&id| by_id(id))
                    .filter_map(|a| a.current)
                    .find(|&c| c <= 100);
            }
        }
    }

    s.health = attributes
        .iter()
        .fold(HealthStatus::Good, |h, a| h.worst(a.status));
    let names = |status: HealthStatus| -> String {
        let list: Vec<String> = attributes
            .iter()
            .filter(|a| a.status == status)
            .take(3)
            .map(|a| a.name.to_lowercase())
            .collect();
        list.join(", ")
    };
    s.health_summary = match s.health {
        HealthStatus::Bad => format!(
            "This drive reports a serious problem ({}). Back up your data now and plan to replace it.",
            names(HealthStatus::Bad)
        ),
        HealthStatus::Caution => format!(
            "This drive works, but it shows warning signs ({}). Keep a current backup of important files.",
            names(HealthStatus::Caution)
        ),
        _ => {
            let mut text = "This drive reports no problems.".to_string();
            if let Some(left) = s.health_pct {
                text.push_str(&format!(
                    " It has used about {} % of its rated life.",
                    100 - left.min(100)
                ));
            }
            text
        }
    };
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (id, current, worst, threshold, raw)
    type Row = (u8, u8, u8, u8, u64);

    fn ata_blocks(rows: &[Row]) -> (Vec<u8>, Vec<u8>) {
        let mut data = vec![0u8; BLOCK];
        let mut thresholds = vec![0u8; BLOCK];
        data[0] = 0x10;
        for (i, &(id, current, worst, threshold, raw)) in rows.iter().enumerate() {
            let at = 2 + i * 12;
            data[at] = id;
            data[at + 1] = 0x03;
            data[at + 3] = current;
            data[at + 4] = worst;
            data[at + 5..at + 11].copy_from_slice(&raw.to_le_bytes()[..6]);
            thresholds[at] = id;
            thresholds[at + 1] = threshold;
        }
        (data, thresholds)
    }

    fn find(attrs: &[SmartAttribute], id: u8) -> &SmartAttribute {
        attrs
            .iter()
            .find(|a| a.id == Some(id))
            .expect("attribute present")
    }

    fn named<'a>(attrs: &'a [SmartAttribute], name: &str) -> &'a SmartAttribute {
        attrs
            .iter()
            .find(|a| a.name == name)
            .expect("attribute present")
    }

    #[test]
    fn healthy_sata_ssd() {
        let (data, thresholds) = ata_blocks(&[
            (5, 100, 100, 10, 0),
            (9, 99, 99, 0, 12_480),
            (12, 99, 99, 0, 1_520),
            (177, 97, 97, 0, 61),
            (194, 67, 52, 0, 0x0030_0014_0021),
            (199, 100, 100, 0, 0),
            (231, 97, 97, 10, 0),
            (241, 99, 99, 0, 20_000_000_000),
            (242, 99, 99, 0, 10_000_000_000),
        ]);
        let attrs = parse_ata(&data, &thresholds, DriveKind::AtaSsd);
        assert_eq!(attrs.len(), 9);
        assert!(attrs.iter().all(|a| a.status == HealthStatus::Good));
        assert!(attrs
            .iter()
            .all(|a| !a.explanation.is_empty() && !a.name.is_empty() && !a.display.is_empty()));
        let t = find(&attrs, 194);
        assert_eq!(t.display, "33 °C");
        assert_eq!(t.raw, 33);
        assert_eq!(find(&attrs, 9).display, "12,480 hours (1 y 5 mo)");
        assert_eq!(find(&attrs, 5).threshold, Some(10));
        assert_eq!(find(&attrs, 231).display, "97 %");
        assert_eq!(find(&attrs, 241).display, fmt_bytes(20_000_000_000 * 512));

        let s = summarize(&attrs, DriveKind::AtaSsd);
        assert_eq!(s.health, HealthStatus::Good);
        assert_eq!(s.health_pct, Some(97));
        assert_eq!(s.temperature_c, Some(33.0));
        assert_eq!(s.power_on_hours, Some(12_480));
        assert_eq!(s.power_cycles, Some(1_520));
        assert_eq!(s.total_written_bytes, Some(20_000_000_000 * 512));
        assert_eq!(s.total_read_bytes, Some(10_000_000_000 * 512));
        assert_eq!(
            s.health_summary,
            "This drive reports no problems. It has used about 3 % of its rated life."
        );
    }

    #[test]
    fn hard_disk_with_bad_sectors() {
        let (data, thresholds) = ata_blocks(&[
            (1, 118, 99, 6, 170_000_000),
            (3, 97, 97, 0, 4_250),
            (5, 98, 98, 36, 24),
            (9, 71, 71, 0, 25_400),
            (10, 100, 100, 97, 0),
            (194, 38, 50, 0, 38),
            (197, 100, 100, 0, 8),
            (198, 100, 100, 0, 0),
            (199, 200, 200, 0, 3),
            (202, 100, 253, 0, 0),
        ]);
        let attrs = parse_ata(&data, &thresholds, DriveKind::AtaHdd);
        assert_eq!(find(&attrs, 5).status, HealthStatus::Caution);
        assert_eq!(find(&attrs, 197).status, HealthStatus::Caution);
        assert_eq!(find(&attrs, 198).status, HealthStatus::Good);
        assert_eq!(find(&attrs, 199).status, HealthStatus::Caution);
        assert!(find(&attrs, 199).explanation.contains("cable"));
        assert_eq!(find(&attrs, 1).status, HealthStatus::Good);
        assert_eq!(find(&attrs, 3).display, "4,250 ms");
        assert_eq!(find(&attrs, 202).name, "Data address mark errors");
        let s = summarize(&attrs, DriveKind::AtaHdd);
        assert_eq!(s.health, HealthStatus::Caution);
        assert_eq!(s.health_pct, None, "no invented life figure for hard disks");
        assert!(s.health_summary.contains("reallocated sectors"));
        assert!(s.health_summary.contains("backup"));

        // Many more, and one attribute at its threshold: bad.
        let (data, thresholds) = ata_blocks(&[(5, 30, 30, 36, 2_400), (197, 100, 100, 0, 350)]);
        let attrs = parse_ata(&data, &thresholds, DriveKind::AtaHdd);
        assert_eq!(find(&attrs, 5).status, HealthStatus::Bad);
        assert!(find(&attrs, 5).explanation.contains("limit"));
        assert_eq!(find(&attrs, 197).status, HealthStatus::Bad);
        let s = summarize(&attrs, DriveKind::AtaHdd);
        assert_eq!(s.health, HealthStatus::Bad);
        assert!(s.health_summary.contains("Back up your data now"));
    }

    #[test]
    fn hot_and_worn_ata() {
        let (data, thresholds) = ata_blocks(&[(194, 40, 40, 0, 60), (233, 4, 4, 0, 0)]);
        let hdd = parse_ata(&data, &thresholds, DriveKind::AtaHdd);
        assert_eq!(find(&hdd, 194).status, HealthStatus::Caution);
        let ssd = parse_ata(&data, &thresholds, DriveKind::AtaSsd);
        assert_eq!(find(&ssd, 194).status, HealthStatus::Good);
        assert_eq!(find(&ssd, 233).status, HealthStatus::Caution);
        assert_eq!(summarize(&ssd, DriveKind::AtaSsd).health_pct, Some(4));
        // Without a thresholds sector the attributes still decode.
        let alone = parse_ata(&data, &[], DriveKind::AtaSsd);
        assert_eq!(alone.len(), 2);
        assert_eq!(alone[0].threshold, None);
        // An unknown id gets a neutral name.
        let (data, thresholds) = ata_blocks(&[(99, 100, 100, 0, 7)]);
        let odd = parse_ata(&data, &thresholds, DriveKind::AtaSsd);
        assert_eq!(odd[0].name, "Vendor-specific 99");
        // A temperature of zero is "no reading", not a measurement.
        let (data, thresholds) = ata_blocks(&[(194, 100, 100, 0, 0)]);
        let cold = parse_ata(&data, &thresholds, DriveKind::AtaSsd);
        assert_eq!(cold[0].status, HealthStatus::Unknown);
        assert_eq!(summarize(&cold, DriveKind::AtaSsd).temperature_c, None);
    }

    fn nvme_log(warning: u8, kelvin: u16, spare: u8, threshold: u8, used: u8) -> Vec<u8> {
        let mut log = vec![0u8; BLOCK];
        log[0] = warning;
        log[1..3].copy_from_slice(&kelvin.to_le_bytes());
        log[3] = spare;
        log[4] = threshold;
        log[5] = used;
        log
    }

    fn put128(log: &mut [u8], offset: usize, value: u128) {
        log[offset..offset + 16].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn healthy_nvme() {
        let mut log = nvme_log(0, 273 + 41, 100, 10, 3);
        put128(&mut log, 32, 10_000_000);
        put128(&mut log, 48, 20_000_000);
        put128(&mut log, 64, 123_456_789);
        put128(&mut log, 112, 840);
        put128(&mut log, 128, 5_300);
        put128(&mut log, 144, 12);
        log[200..202].copy_from_slice(&(273u16 + 44).to_le_bytes());
        log[202..204].copy_from_slice(&(273u16 + 39).to_le_bytes());
        let attrs = parse_nvme(&log);
        assert!(attrs.iter().all(|a| a.status == HealthStatus::Good));
        assert!(attrs
            .iter()
            .all(|a| a.id.is_none() && !a.explanation.is_empty()));
        assert_eq!(named(&attrs, NVME_TEMPERATURE).display, "41 °C");
        assert_eq!(named(&attrs, NVME_CRITICAL_WARNING).display, "None");
        assert_eq!(named(&attrs, "Temperature sensor 1").display, "44 °C");
        assert_eq!(named(&attrs, "Temperature sensor 2").display, "39 °C");
        assert!(!attrs.iter().any(|a| a.name == "Temperature sensor 3"));
        assert_eq!(named(&attrs, "Host read commands").display, "123,456,789");
        let s = summarize(&attrs, DriveKind::Nvme);
        assert_eq!(s.health, HealthStatus::Good);
        assert_eq!(s.health_pct, Some(97));
        assert_eq!(s.temperature_c, Some(41.0));
        assert_eq!(s.power_on_hours, Some(5_300));
        assert_eq!(s.power_cycles, Some(840));
        assert_eq!(s.total_read_bytes, Some(10_000_000 * 512_000));
        assert_eq!(s.total_written_bytes, Some(20_000_000 * 512_000));
        assert_eq!(
            s.health_summary,
            "This drive reports no problems. It has used about 3 % of its rated life."
        );
    }

    #[test]
    fn worn_nvme() {
        let mut log = nvme_log(0b0000_0101, 273 + 78, 4, 10, 131);
        put128(&mut log, 160, 17);
        log[196..200].copy_from_slice(&9u32.to_le_bytes());
        put128(&mut log, 48, u128::MAX);
        let attrs = parse_nvme(&log);
        assert_eq!(
            named(&attrs, NVME_CRITICAL_WARNING).status,
            HealthStatus::Bad
        );
        assert!(named(&attrs, NVME_CRITICAL_WARNING)
            .display
            .starts_with("Spare capacity"));
        assert_eq!(
            named(&attrs, NVME_AVAILABLE_SPARE).status,
            HealthStatus::Bad
        );
        assert_eq!(
            named(&attrs, NVME_TEMPERATURE).status,
            HealthStatus::Caution
        );
        assert_eq!(
            named(&attrs, NVME_PERCENTAGE_USED).status,
            HealthStatus::Caution
        );
        assert_eq!(named(&attrs, "Media errors").status, HealthStatus::Caution);
        assert_eq!(
            named(&attrs, "Critical temperature time").status,
            HealthStatus::Caution
        );
        let s = summarize(&attrs, DriveKind::Nvme);
        assert_eq!(s.health, HealthStatus::Bad);
        assert_eq!(s.health_pct, Some(0), "clamped, not negative");
        assert_eq!(s.total_written_bytes, Some(u64::MAX), "saturates");
        assert!(s.health_summary.contains("serious problem"));
        // Spare close to the limit is a caution.
        let attrs = parse_nvme(&nvme_log(0, 300, 15, 10, 50));
        assert_eq!(
            named(&attrs, NVME_AVAILABLE_SPARE).status,
            HealthStatus::Caution
        );
    }

    #[test]
    fn corrupt_input_is_rejected_quietly() {
        for kind in [DriveKind::Nvme, DriveKind::AtaSsd, DriveKind::AtaHdd] {
            assert!(parse_ata(&[], &[], kind).is_empty());
            assert!(parse_ata(&[0u8; 100], &[0u8; 100], kind).is_empty());
            assert!(parse_ata(&[0u8; 513], &[0u8; 512], kind).is_empty());
            assert!(parse_ata(&[0u8; BLOCK], &[0u8; BLOCK], kind).is_empty());
            assert!(parse_ata(&[0xFFu8; BLOCK], &[0xFFu8; BLOCK], kind).is_empty());
            let s = summarize(&[], kind);
            assert_eq!(s, Summary::default());
            assert_eq!(s.health, HealthStatus::Unknown);
        }
        assert!(parse_nvme(&[]).is_empty());
        assert!(parse_nvme(&[1u8; 511]).is_empty());
        assert!(parse_nvme(&[1u8; 4096]).is_empty());
        assert!(parse_nvme(&[0u8; BLOCK]).is_empty());
        assert!(parse_nvme(&[0xFFu8; BLOCK]).is_empty());
        // Pseudo-random blocks: any result is acceptable, a panic is not.
        let mut state = 0x9E37_79B9u32;
        for _ in 0..300 {
            let block: Vec<u8> = (0..BLOCK)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            for kind in [DriveKind::Nvme, DriveKind::AtaSsd, DriveKind::AtaHdd] {
                let a = parse_ata(&block, &block, kind);
                let _ = summarize(&a, kind);
                let n = parse_nvme(&block);
                let s = summarize(&n, kind);
                assert!(s.health_pct.unwrap_or(0) <= 100);
            }
        }
        // A health log whose temperature is nonsense keeps the rest.
        let attrs = parse_nvme(&nvme_log(0, 0xFFFF, 100, 5, 1));
        assert_eq!(
            named(&attrs, NVME_TEMPERATURE).status,
            HealthStatus::Unknown
        );
        let s = summarize(&attrs, DriveKind::Nvme);
        assert_eq!(s.temperature_c, None);
        assert_eq!(s.health, HealthStatus::Good);
    }
}
