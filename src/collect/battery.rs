//! Battery: charge, rate, capacities, wear and cycle count.

use super::storage::{ioctl, le_u16, le_u32, open_device, Handle};
use crate::model::{BatteryInfo, BatteryState};
use std::sync::Mutex;
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, GUID_DEVCLASS_BATTERY,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

const IOCTL_BATTERY_QUERY_TAG: u32 = 0x0029_4040;
const IOCTL_BATTERY_QUERY_INFORMATION: u32 = 0x0029_4044;
const IOCTL_BATTERY_QUERY_STATUS: u32 = 0x0029_404C;

// Information levels of IOCTL_BATTERY_QUERY_INFORMATION.
const LEVEL_INFORMATION: u32 = 0;
const LEVEL_TEMPERATURE: u32 = 2;
const LEVEL_DEVICE_NAME: u32 = 4;
const LEVEL_MANUFACTURE_DATE: u32 = 5;
const LEVEL_MANUFACTURE_NAME: u32 = 6;
const LEVEL_SERIAL_NUMBER: u32 = 8;

const POWER_ON_LINE: u32 = 1;
const DISCHARGING: u32 = 2;
const CHARGING: u32 = 4;
const CRITICAL: u32 = 8;
/// Capability flag: capacities are in an unknown relative unit, not mWh.
const CAPACITY_RELATIVE: u32 = 0x4000_0000;
const UNKNOWN: u32 = 0xFFFF_FFFF;
const UNKNOWN_RATE: i32 = i32::MIN;

/// Largest capacity believed (1 kWh) and largest voltage believed (100 V).
const MAX_MWH: u32 = 1_000_000;
const MAX_MV: u32 = 100_000;
/// Estimates longer than this are noise from a near-zero rate.
const MAX_SECONDS: f64 = 14.0 * 86_400.0;
/// How long a failed search for the battery device is remembered.
const RETRY_S: f64 = 60.0;

/// The numbers the battery driver hands out, before interpretation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawBattery {
    pub capabilities: u32,
    pub chemistry: [u8; 4],
    pub design: u32,
    pub full: u32,
    pub cycle_count: u32,
    pub power_state: u32,
    pub remaining: u32,
    pub voltage: u32,
    pub rate: i32,
    /// What `GetSystemPowerStatus` says about the mains, if known.
    pub ac_online: Option<bool>,
    /// The charge percentage Windows shows, if known.
    pub system_percent: Option<u8>,
}

impl Default for RawBattery {
    fn default() -> Self {
        RawBattery {
            capabilities: 0,
            chemistry: [0; 4],
            design: UNKNOWN,
            full: UNKNOWN,
            cycle_count: 0,
            power_state: 0,
            remaining: UNKNOWN,
            voltage: UNKNOWN,
            rate: UNKNOWN_RATE,
            ac_online: None,
            system_percent: None,
        }
    }
}

/// The chemistry code of the battery driver in words.
pub fn chemistry_name(code: &[u8]) -> String {
    let text: String = code
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|&b| b as char)
        .collect();
    let text = text.trim();
    match text.to_ascii_uppercase().as_str() {
        "LION" | "LI-I" | "LIIO" | "LI" => "Lithium-ion".into(),
        "LIP" | "LIPO" | "LI-P" => "Lithium polymer".into(),
        "NIMH" => "Nickel-metal hydride".into(),
        "NICD" => "Nickel-cadmium".into(),
        "NIZN" => "Nickel-zinc".into(),
        "PBAC" => "Lead-acid".into(),
        "RAM" => "Rechargeable alkaline".into(),
        _ => text.to_string(),
    }
}

/// Charge in percent, `None` when the capacities are unknown or zero.
pub fn percent(remaining: Option<u32>, full: Option<u32>) -> Option<f32> {
    match (remaining, full) {
        (Some(r), Some(f)) if f > 0 => Some((r as f32 / f as f32 * 100.0).clamp(0.0, 100.0)),
        _ => None,
    }
}

/// Capacity lost against the design capacity, in percent. A battery that
/// holds more than its design capacity has 0 % wear, not a negative one.
pub fn wear(design: Option<u32>, full: Option<u32>) -> Option<f32> {
    match (design, full) {
        (Some(d), Some(f)) if d > 0 => {
            Some(((d as f32 - f as f32) / d as f32 * 100.0).clamp(0.0, 100.0))
        }
        _ => None,
    }
}

/// Seconds until empty (discharging) or full (charging).
pub fn seconds_left(
    state: BatteryState,
    remaining: Option<u32>,
    full: Option<u32>,
    rate_mw: Option<i32>,
) -> Option<u32> {
    let rate = f64::from(rate_mw?.unsigned_abs());
    if rate <= 0.0 {
        return None;
    }
    let mwh = match state {
        BatteryState::Discharging => f64::from(remaining?),
        BatteryState::Charging => f64::from(full?.saturating_sub(remaining?)),
        _ => return None,
    };
    let seconds = mwh / rate * 3600.0;
    (seconds.is_finite() && seconds <= MAX_SECONDS).then_some(seconds as u32)
}

fn state_from_bits(bits: u32, percent: Option<f32>, ac_online: Option<bool>) -> BatteryState {
    let full = percent.is_some_and(|p| p >= 99.5);
    if bits & CHARGING != 0 {
        BatteryState::Charging
    } else if bits & DISCHARGING != 0 {
        BatteryState::Discharging
    } else if bits & POWER_ON_LINE != 0 || ac_online == Some(true) {
        if full {
            BatteryState::Full
        } else {
            BatteryState::Idle
        }
    } else if ac_online == Some(false) {
        BatteryState::Discharging
    } else {
        BatteryState::Unknown
    }
}

/// Interprets the raw numbers: sentinels become `None`, relative-unit
/// batteries get a percentage only, absurd values are dropped.
pub fn interpret(raw: &RawBattery) -> BatteryInfo {
    let relative = raw.capabilities & CAPACITY_RELATIVE != 0;
    let capacity = |v: u32| (v != UNKNOWN && v > 0 && (relative || v <= MAX_MWH)).then_some(v);
    let design = capacity(raw.design);
    let full = capacity(raw.full);
    // Zero is a valid remaining charge.
    let remaining = (raw.remaining != UNKNOWN && (relative || raw.remaining <= MAX_MWH))
        .then_some(raw.remaining);
    let pct = percent(remaining, full).or(raw.system_percent.filter(|&p| p <= 100).map(f32::from));
    let state = state_from_bits(raw.power_state, pct, raw.ac_online);
    let rate =
        (raw.rate != UNKNOWN_RATE && raw.rate.unsigned_abs() <= MAX_MWH && !relative).then(|| {
            match state {
                BatteryState::Discharging => -raw.rate.abs(),
                BatteryState::Charging => raw.rate.abs(),
                _ => raw.rate,
            }
        });
    let mut info = BatteryInfo {
        chemistry: chemistry_name(&raw.chemistry),
        percent: pct.unwrap_or(0.0),
        state,
        ac_online: raw
            .ac_online
            .unwrap_or(raw.power_state & POWER_ON_LINE != 0),
        rate_mw: rate,
        voltage_mv: (raw.voltage != UNKNOWN && raw.voltage > 0 && raw.voltage <= MAX_MV)
            .then_some(raw.voltage),
        cycle_count: (raw.cycle_count > 0 && raw.cycle_count <= 100_000).then_some(raw.cycle_count),
        ..Default::default()
    };
    if relative {
        info.extra.push((
            "Capacity".into(),
            "the battery reports a percentage only, not milliwatt-hours".into(),
        ));
    } else {
        info.design_mwh = design;
        info.full_mwh = full;
        info.remaining_mwh = remaining;
        info.wear_pct = wear(design, full);
        info.seconds_left = seconds_left(state, remaining, full, rate);
    }
    if raw.power_state & CRITICAL != 0 {
        info.extra
            .push(("Charge level".into(), "critically low".into()));
    }
    info
}

/// Tenths of a Kelvin to °C, `None` for values no battery survives.
pub fn temperature_c(tenths_kelvin: u32) -> Option<f32> {
    let c = tenths_kelvin as f32 / 10.0 - 273.15;
    (tenths_kelvin != UNKNOWN && (-40.0..=120.0).contains(&c)).then_some(c)
}

/// The manufacture date structure (day, month, year) as an ISO date.
pub fn manufacture_date(buf: &[u8]) -> String {
    let (Some(&day), Some(&month), Some(year)) = (buf.first(), buf.get(1), le_u16(buf, 2)) else {
        return String::new();
    };
    if (1..=31).contains(&day) && (1..=12).contains(&month) && (1980..=2100).contains(&year) {
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        String::new()
    }
}

// ----------------------------------------------------------------------------------
// Reading the device
// ----------------------------------------------------------------------------------

#[derive(Default)]
struct Cache {
    path: Option<String>,
    missing_since: Option<Instant>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    path: None,
    missing_since: None,
});

/// Device interface paths of every battery that is present.
fn battery_paths() -> Vec<String> {
    let mut paths = Vec::new();
    // SAFETY: the device information set is destroyed before returning; every
    // structure passed has its size field set and outlives the call it is used in.
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_BATTERY),
            PCWSTR::null(),
            HWND::default(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        ) else {
            return paths;
        };
        for index in 0..8u32 {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVCLASS_BATTERY, index, &mut data)
                .is_err()
            {
                break;
            }
            let mut needed = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut needed), None);
            if !(8..=8192).contains(&needed) {
                continue;
            }
            // 32-bit words keep the buffer aligned for the structure.
            let mut buf = vec![0u32; (needed as usize).div_ceil(4) + 1];
            buf[0] = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let ok = SetupDiGetDeviceInterfaceDetailW(
                set,
                &data,
                Some(buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W),
                needed,
                None,
                None,
            )
            .is_ok();
            if ok {
                // The path starts right after the 4-byte size field.
                let units: Vec<u16> = buf[1..]
                    .iter()
                    .flat_map(|w| [(w & 0xFFFF) as u16, (w >> 16) as u16])
                    .collect();
                let path = crate::util::from_wide(&units);
                if !path.is_empty() {
                    paths.push(path);
                }
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    paths
}

fn open_battery(path: &str) -> Option<Handle> {
    // Read/write access is what the battery driver documents; fall back to less.
    [0xC000_0000u32, 0x8000_0000, 0]
        .iter()
        .find_map(|&access| open_device(path, access).ok())
}

fn query_tag(handle: &Handle) -> Option<u32> {
    let mut out = [0u8; 4];
    let n = ioctl(
        handle,
        IOCTL_BATTERY_QUERY_TAG,
        &0u32.to_le_bytes(),
        &mut out,
    )
    .ok()?;
    le_u32(&out[..n], 0).filter(|&tag| tag != 0)
}

fn query_information(handle: &Handle, tag: u32, level: u32, size: usize) -> Option<Vec<u8>> {
    let mut input = [0u8; 12];
    input[..4].copy_from_slice(&tag.to_le_bytes());
    input[4..8].copy_from_slice(&level.to_le_bytes());
    let mut out = vec![0u8; size];
    let n = ioctl(handle, IOCTL_BATTERY_QUERY_INFORMATION, &input, &mut out).ok()?;
    out.truncate(n);
    Some(out)
}

fn query_text(handle: &Handle, tag: u32, level: u32) -> String {
    query_information(handle, tag, level, 512)
        .map(|bytes| {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            crate::util::from_wide(&units).trim().to_string()
        })
        .unwrap_or_default()
}

fn system_status() -> Option<SYSTEM_POWER_STATUS> {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: plain out-parameter call.
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    Some(status)
}

/// Reads one battery device completely. `None` when it has no battery in it.
fn read_device(path: &str, system: &SYSTEM_POWER_STATUS) -> Option<BatteryInfo> {
    let handle = open_battery(path)?;
    let tag = query_tag(&handle)?;
    let mut raw = RawBattery {
        ac_online: match system.ACLineStatus {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        system_percent: (system.BatteryLifePercent <= 100).then_some(system.BatteryLifePercent),
        ..Default::default()
    };
    if let Some(b) = query_information(&handle, tag, LEVEL_INFORMATION, 64) {
        if b.len() >= 36 {
            raw.capabilities = le_u32(&b, 0).unwrap_or(0);
            raw.chemistry.copy_from_slice(&b[8..12]);
            raw.design = le_u32(&b, 12).unwrap_or(UNKNOWN);
            raw.full = le_u32(&b, 16).unwrap_or(UNKNOWN);
            raw.cycle_count = le_u32(&b, 32).unwrap_or(0);
        }
    }
    // BATTERY_WAIT_STATUS with a zero timeout: answer immediately.
    let mut wait = [0u8; 20];
    wait[..4].copy_from_slice(&tag.to_le_bytes());
    let mut status = [0u8; 16];
    if let Ok(n) = ioctl(&handle, IOCTL_BATTERY_QUERY_STATUS, &wait, &mut status) {
        let s = &status[..n];
        raw.power_state = le_u32(s, 0).unwrap_or(0);
        raw.remaining = le_u32(s, 4).unwrap_or(UNKNOWN);
        raw.voltage = le_u32(s, 8).unwrap_or(UNKNOWN);
        raw.rate = le_u32(s, 12).map(|v| v as i32).unwrap_or(UNKNOWN_RATE);
    }
    let mut info = interpret(&raw);
    info.name = query_text(&handle, tag, LEVEL_DEVICE_NAME);
    info.manufacturer = query_text(&handle, tag, LEVEL_MANUFACTURE_NAME);
    info.serial = query_text(&handle, tag, LEVEL_SERIAL_NUMBER);
    info.manufacture_date = query_information(&handle, tag, LEVEL_MANUFACTURE_DATE, 8)
        .map(|b| manufacture_date(&b))
        .unwrap_or_default();
    info.temperature_c = query_information(&handle, tag, LEVEL_TEMPERATURE, 8)
        .and_then(|b| le_u32(&b, 0))
        .and_then(temperature_c);
    if info.seconds_left.is_none()
        && info.state == BatteryState::Discharging
        && system.BatteryLifeTime != UNKNOWN
        && system.BatteryLifeTime < MAX_SECONDS as u32
    {
        // The estimate Windows itself shows.
        info.seconds_left = Some(system.BatteryLifeTime);
    }
    Some(info)
}

/// What the power status alone says, for a battery whose driver does not answer.
fn from_system(system: &SYSTEM_POWER_STATUS) -> BatteryInfo {
    let percent =
        (system.BatteryLifePercent <= 100).then_some(f32::from(system.BatteryLifePercent));
    let ac = system.ACLineStatus == 1;
    let state = if system.BatteryFlag != 255 && system.BatteryFlag & 8 != 0 {
        BatteryState::Charging
    } else {
        state_from_bits(0, percent, (system.ACLineStatus <= 1).then_some(ac))
    };
    BatteryInfo {
        percent: percent.unwrap_or(0.0),
        state,
        ac_online: ac,
        seconds_left: (state == BatteryState::Discharging
            && system.BatteryLifeTime != UNKNOWN
            && system.BatteryLifeTime < MAX_SECONDS as u32)
            .then_some(system.BatteryLifeTime),
        ..Default::default()
    }
}

/// The (first) battery, or `None` on machines without one. Cheap enough to
/// call every few seconds.
pub fn read() -> Option<BatteryInfo> {
    let system = system_status()?;
    // 128: "no system battery". The cheap exit for desktops.
    if system.BatteryFlag != 255 && system.BatteryFlag & 128 != 0 {
        return None;
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = cache.path.clone() {
        if let Some(info) = read_device(&path, &system) {
            return Some(info);
        }
        cache.path = None;
    }
    let searched_recently = cache
        .missing_since
        .is_some_and(|at| at.elapsed().as_secs_f64() < RETRY_S);
    if !searched_recently {
        for path in battery_paths() {
            if let Some(info) = read_device(&path, &system) {
                cache.path = Some(path);
                cache.missing_since = None;
                return Some(info);
            }
        }
        cache.missing_since = Some(Instant::now());
    }
    // Windows says there is a battery, but no battery device answered.
    (system.BatteryFlag != 255 && system.BatteryLifePercent <= 100).then(|| from_system(&system))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw() -> RawBattery {
        RawBattery {
            capabilities: 0x8000_0000,
            chemistry: *b"LION",
            design: 60_000,
            full: 54_000,
            cycle_count: 212,
            power_state: DISCHARGING,
            remaining: 27_000,
            voltage: 11_400,
            rate: -9_000,
            ac_online: Some(false),
            system_percent: Some(50),
        }
    }

    #[test]
    fn discharging_battery() {
        let b = interpret(&raw());
        assert_eq!(b.chemistry, "Lithium-ion");
        assert_eq!(b.design_mwh, Some(60_000));
        assert_eq!(b.full_mwh, Some(54_000));
        assert_eq!(b.remaining_mwh, Some(27_000));
        assert_eq!(b.percent, 50.0);
        assert_eq!(b.state, BatteryState::Discharging);
        assert!(!b.ac_online);
        assert_eq!(b.rate_mw, Some(-9_000));
        assert_eq!(b.voltage_mv, Some(11_400));
        assert_eq!(b.cycle_count, Some(212));
        assert!((b.wear_pct.unwrap_or(0.0) - 10.0).abs() < 0.001);
        assert_eq!(b.seconds_left, Some(3 * 3600));
        // Some drivers report the discharge rate as a positive number.
        let b = interpret(&RawBattery {
            rate: 9_000,
            ..raw()
        });
        assert_eq!(b.rate_mw, Some(-9_000));
        assert_eq!(b.seconds_left, Some(3 * 3600));
    }

    #[test]
    fn charging_full_and_idle() {
        let b = interpret(&RawBattery {
            power_state: POWER_ON_LINE | CHARGING,
            rate: 13_500,
            ac_online: Some(true),
            ..raw()
        });
        assert_eq!(b.state, BatteryState::Charging);
        assert!(b.ac_online);
        assert_eq!(b.rate_mw, Some(13_500));
        assert_eq!(b.seconds_left, Some(2 * 3600));
        let b = interpret(&RawBattery {
            power_state: POWER_ON_LINE,
            remaining: 54_000,
            rate: 0,
            ac_online: Some(true),
            ..raw()
        });
        assert_eq!(b.state, BatteryState::Full);
        assert_eq!(b.percent, 100.0);
        assert_eq!(b.seconds_left, None);
        let b = interpret(&RawBattery {
            power_state: POWER_ON_LINE,
            remaining: 40_000,
            rate: 0,
            ac_online: Some(true),
            ..raw()
        });
        assert_eq!(b.state, BatteryState::Idle);
        // No state bits at all: the mains status decides.
        assert_eq!(
            state_from_bits(0, Some(40.0), Some(false)),
            BatteryState::Discharging
        );
        assert_eq!(
            state_from_bits(0, Some(40.0), Some(true)),
            BatteryState::Idle
        );
        assert_eq!(state_from_bits(0, None, None), BatteryState::Unknown);
    }

    #[test]
    fn sentinels_zero_and_absurd_values() {
        // Everything unknown.
        let b = interpret(&RawBattery::default());
        assert_eq!(b.design_mwh, None);
        assert_eq!(b.full_mwh, None);
        assert_eq!(b.remaining_mwh, None);
        assert_eq!(b.rate_mw, None);
        assert_eq!(b.voltage_mv, None);
        assert_eq!(b.cycle_count, None);
        assert_eq!(b.wear_pct, None);
        assert_eq!(b.seconds_left, None);
        assert_eq!(b.percent, 0.0);
        assert_eq!(b.state, BatteryState::Unknown);
        assert_eq!(b.chemistry, "");
        // Unknown capacities fall back to the percentage Windows shows.
        let b = interpret(&RawBattery {
            system_percent: Some(73),
            ..Default::default()
        });
        assert_eq!(b.percent, 73.0);
        assert_eq!(
            interpret(&RawBattery {
                system_percent: Some(255),
                ..Default::default()
            })
            .percent,
            0.0
        );
        // Zeros.
        let b = interpret(&RawBattery {
            design: 0,
            full: 0,
            remaining: 0,
            voltage: 0,
            rate: 0,
            cycle_count: 0,
            system_percent: None,
            ..raw()
        });
        assert_eq!(
            (b.design_mwh, b.full_mwh, b.remaining_mwh),
            (None, None, Some(0))
        );
        assert_eq!(b.percent, 0.0);
        assert_eq!(b.wear_pct, None);
        assert_eq!(b.seconds_left, None);
        assert_eq!(b.voltage_mv, None);
        // Absurd numbers are dropped rather than shown.
        let b = interpret(&RawBattery {
            design: 0x7FFF_FFFF,
            full: 3_000_000_000,
            remaining: 2_000_000_000,
            voltage: 4_000_000,
            rate: 2_000_000_000,
            cycle_count: 4_000_000,
            ..raw()
        });
        assert_eq!(
            (b.design_mwh, b.full_mwh, b.remaining_mwh),
            (None, None, None)
        );
        assert_eq!((b.voltage_mv, b.rate_mw, b.cycle_count), (None, None, None));
        assert_eq!(b.percent, 50.0, "falls back to the system percentage");
        // Full above design: no negative wear. Remaining above full: 100 %.
        let b = interpret(&RawBattery {
            full: 63_000,
            remaining: 70_000,
            ..raw()
        });
        assert_eq!(b.wear_pct, Some(0.0));
        assert_eq!(b.percent, 100.0);
        // A tiny rate gives no multi-week estimate.
        assert_eq!(
            interpret(&RawBattery { rate: -1, ..raw() }).seconds_left,
            None
        );
        assert_eq!(
            seconds_left(BatteryState::Charging, Some(60), Some(50), Some(10)),
            Some(0),
            "remaining above full while charging"
        );
        assert_eq!(
            seconds_left(BatteryState::Idle, Some(5), Some(50), Some(10)),
            None
        );
        assert_eq!(percent(Some(5), Some(0)), None);
        assert_eq!(wear(Some(0), Some(5)), None);
    }

    #[test]
    fn relative_unit_battery() {
        let b = interpret(&RawBattery {
            capabilities: 0x8000_0000 | CAPACITY_RELATIVE,
            design: 100,
            full: 100,
            remaining: 64,
            rate: -7,
            ..raw()
        });
        assert_eq!(b.percent, 64.0);
        assert_eq!(
            (b.design_mwh, b.full_mwh, b.remaining_mwh),
            (None, None, None)
        );
        assert_eq!(b.rate_mw, None);
        assert_eq!(b.wear_pct, None);
        assert_eq!(b.seconds_left, None);
        assert_eq!(b.extra.len(), 1);
    }

    #[test]
    fn names_dates_and_temperature() {
        assert_eq!(chemistry_name(b"LION"), "Lithium-ion");
        assert_eq!(chemistry_name(b"LiP\0"), "Lithium polymer");
        assert_eq!(chemistry_name(b"NiMH"), "Nickel-metal hydride");
        assert_eq!(chemistry_name(b"PbAc"), "Lead-acid");
        assert_eq!(chemistry_name(b"XYZ\0"), "XYZ");
        assert_eq!(chemistry_name(&[0, 0, 0, 0]), "");
        assert_eq!(chemistry_name(&[0xFF, 0xFF, 0xFF, 0xFF]), "");
        assert_eq!(chemistry_name(&[]), "");
        assert_eq!(manufacture_date(&[14, 3, 0xE8, 0x07]), "2024-03-14");
        assert_eq!(manufacture_date(&[0, 0, 0, 0]), "");
        assert_eq!(manufacture_date(&[0xFF; 4]), "");
        assert_eq!(manufacture_date(&[14, 3]), "");
        assert_eq!(manufacture_date(&[]), "");
        let t = temperature_c(3_031).unwrap_or(0.0);
        assert!((t - 29.95).abs() < 0.01);
        assert_eq!(temperature_c(0), None);
        assert_eq!(temperature_c(UNKNOWN), None);
        assert_eq!(temperature_c(90_000), None);
    }

    #[test]
    fn live_battery() {
        let started = Instant::now();
        let first = read();
        let second = read();
        println!(
            "battery present: {} (two reads took {} ms)",
            first.is_some(),
            started.elapsed().as_millis()
        );
        assert_eq!(first.is_some(), second.is_some());
        if let Some(b) = second {
            println!(
                "  capacities known: {}, rate known: {}, wear known: {}, cycles known: {}, temperature known: {}, state: {}",
                b.full_mwh.is_some(),
                b.rate_mw.is_some(),
                b.wear_pct.is_some(),
                b.cycle_count.is_some(),
                b.temperature_c.is_some(),
                b.state.label()
            );
            assert!((0.0..=100.0).contains(&b.percent));
            assert!(b.wear_pct.is_none_or(|w| (0.0..=100.0).contains(&w)));
        }
    }
}
