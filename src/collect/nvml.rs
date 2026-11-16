//! Optional readings from an installed graphics vendor library, loaded at run time.
//!
//! When the graphics driver of the machine ships its management library in
//! the Windows system directory, it is loaded from there (and only from
//! there) and asked for temperature, fan duty, power draw and core clock.
//! Nothing is bundled with the program, and a machine without the library
//! simply has no such readings: [`Library::load`] then returns the reason.

use super::pdh::le_u32;
use crate::util::wide;
use std::ffi::{c_char, c_void};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

/// File name of the management library. Searched in System32 only.
const LIBRARY_FILE: &str = "nvml.dll";
/// The honest note for adapters that have no library to ask.
pub const NO_LIBRARY_NOTE: &str = "no vendor library found; not available without a driver";

type Device = *mut c_void;
type FnInit = unsafe extern "C" fn() -> i32;
type FnShutdown = unsafe extern "C" fn() -> i32;
type FnCount = unsafe extern "C" fn(*mut u32) -> i32;
type FnHandle = unsafe extern "C" fn(u32, *mut Device) -> i32;
type FnName = unsafe extern "C" fn(Device, *mut c_char, u32) -> i32;
type FnPci = unsafe extern "C" fn(Device, *mut c_void) -> i32;
type FnTyped = unsafe extern "C" fn(Device, u32, *mut u32) -> i32;
type FnValue = unsafe extern "C" fn(Device, *mut u32) -> i32;

/// What identifies one device of the library, for matching it to an adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    /// PCI bus number, when the library reports it.
    pub bus: Option<u32>,
    /// PCI device id (the upper half of the combined id), when reported.
    pub device_id: Option<u32>,
}

/// One reading of one device. Values the library cannot give are `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    pub temperature_c: Option<f32>,
    pub fan_pct: Option<f32>,
    pub power_w: Option<f32>,
    pub clock_mhz: Option<f32>,
}

/// The loaded library with its devices. Shut down and unloaded on drop.
pub struct Library {
    /// The module handle as an integer, so the type can move between threads.
    module: usize,
    shutdown: FnShutdown,
    temperature: FnTyped,
    fan: Option<FnValue>,
    power: Option<FnValue>,
    clock: Option<FnTyped>,
    /// Device handles as integers (they are opaque tokens of the library).
    handles: Vec<usize>,
    devices: Vec<DeviceInfo>,
}

/// Frees the module if loading stops half way.
struct ModuleGuard(usize);

impl Drop for ModuleGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the handle came from LoadLibraryExW and is released once.
            unsafe {
                let _ = FreeLibrary(HMODULE(self.0 as *mut c_void));
            }
        }
    }
}

/// Looks up an export. `name` must be NUL-terminated.
fn export(module: usize, name: &'static [u8]) -> Option<unsafe extern "system" fn() -> isize> {
    debug_assert!(name.ends_with(&[0]));
    // SAFETY: the module handle is valid while the caller holds it and the
    // name is a NUL-terminated byte string.
    unsafe { GetProcAddress(HMODULE(module as *mut c_void), PCSTR(name.as_ptr())) }
}

macro_rules! symbol {
    ($module:expr, $name:literal, $ty:ty) => {
        // SAFETY: the export is documented with exactly this signature; all
        // function pointers have the same size.
        export($module, concat!($name, "\0").as_bytes())
            .map(|f| unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f) })
    };
}

impl Library {
    /// Loads the library from the system directory and lists its devices.
    /// The error is a plain-language reason, suitable as a sensor note.
    pub fn load() -> Result<Library, String> {
        let file = wide(LIBRARY_FILE);
        // SAFETY: the name is NUL-terminated; the flag restricts the search to
        // System32, so no other directory can supply the file.
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(file.as_ptr()),
                HANDLE::default(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map_err(|_| NO_LIBRARY_NOTE.to_string())?;
        let module = module.0 as usize;
        let mut guard = ModuleGuard(module);

        let incomplete = || "vendor library found but not usable".to_string();
        let init = symbol!(module, "nvmlInit_v2", FnInit).ok_or_else(incomplete)?;
        let shutdown = symbol!(module, "nvmlShutdown", FnShutdown).ok_or_else(incomplete)?;
        let count_fn = symbol!(module, "nvmlDeviceGetCount_v2", FnCount).ok_or_else(incomplete)?;
        let handle_fn =
            symbol!(module, "nvmlDeviceGetHandleByIndex_v2", FnHandle).ok_or_else(incomplete)?;
        let temperature =
            symbol!(module, "nvmlDeviceGetTemperature", FnTyped).ok_or_else(incomplete)?;
        let name_fn = symbol!(module, "nvmlDeviceGetName", FnName);
        let pci_fn = symbol!(module, "nvmlDeviceGetPciInfo_v3", FnPci);
        let fan = symbol!(module, "nvmlDeviceGetFanSpeed", FnValue);
        let power = symbol!(module, "nvmlDeviceGetPowerUsage", FnValue);
        let clock = symbol!(module, "nvmlDeviceGetClockInfo", FnTyped);

        // SAFETY: the function takes no arguments.
        if unsafe { init() } != 0 {
            return Err("vendor library found but its driver did not answer".to_string());
        }
        let mut library = Library {
            module,
            shutdown,
            temperature,
            fan,
            power,
            clock,
            handles: Vec::new(),
            devices: Vec::new(),
        };
        // From here on dropping `library` shuts down and unloads.
        guard.0 = 0;

        let mut count = 0u32;
        // SAFETY: `count` is a valid out-pointer.
        if unsafe { count_fn(&mut count) } != 0 {
            return Err("vendor library found but it lists no devices".to_string());
        }
        for index in 0..count.min(64) {
            let mut device: Device = std::ptr::null_mut();
            // SAFETY: `device` is a valid out-pointer.
            if unsafe { handle_fn(index, &mut device) } != 0 || device.is_null() {
                continue;
            }
            let mut info = DeviceInfo::default();
            if let Some(name_fn) = name_fn {
                let mut text = [0u8; 128];
                // SAFETY: the buffer is writable for the length passed.
                if unsafe { name_fn(device, text.as_mut_ptr().cast(), text.len() as u32) } == 0 {
                    info.name = c_text(&text);
                }
            }
            if let Some(pci_fn) = pci_fn {
                // Larger than any version of the structure, 8-byte aligned.
                let mut raw = [0u64; 64];
                // SAFETY: the buffer is writable and larger than the structure.
                if unsafe { pci_fn(device, raw.as_mut_ptr().cast()) } == 0 {
                    let bytes: Vec<u8> = raw.iter().flat_map(|w| w.to_le_bytes()).collect();
                    if let Some((bus, device_id)) = parse_pci_info(&bytes) {
                        info.bus = Some(bus);
                        info.device_id = Some(device_id);
                    }
                }
            }
            library.handles.push(device as usize);
            library.devices.push(info);
        }
        if library.devices.is_empty() {
            return Err("vendor library found but it lists no devices".to_string());
        }
        Ok(library)
    }

    /// The devices the library knows, in its own order.
    pub fn devices(&self) -> &[DeviceInfo] {
        &self.devices
    }

    /// Reads one device. An index out of range gives an empty reading.
    pub fn read(&self, index: usize) -> Reading {
        let Some(&handle) = self.handles.get(index) else {
            return Reading::default();
        };
        let device = handle as Device;
        let typed = |f: FnTyped, kind: u32| {
            let mut value = 0u32;
            // SAFETY: the device handle came from this library, which is still
            // loaded and initialised; `value` is a valid out-pointer.
            (unsafe { f(device, kind, &mut value) } == 0).then_some(value)
        };
        let plain = |f: FnValue| {
            let mut value = 0u32;
            // SAFETY: as above.
            (unsafe { f(device, &mut value) } == 0).then_some(value)
        };
        sanitize(
            typed(self.temperature, 0),
            self.fan.and_then(plain),
            self.power.and_then(plain),
            self.clock.and_then(|f| typed(f, 0)),
        )
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the library was initialised by `load`; after the shutdown no
        // handle of it is used again, and the module is released exactly once.
        unsafe {
            let _ = (self.shutdown)();
            let _ = FreeLibrary(HMODULE(self.module as *mut c_void));
        }
    }
}

/// Text from a NUL-terminated byte buffer.
fn c_text(bytes: &[u8]) -> String {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).trim().to_string()
}

/// (bus, device id) from the library's PCI structure: a 16-byte legacy text,
/// then domain, bus, device and the combined id (device id in the upper half).
pub fn parse_pci_info(bytes: &[u8]) -> Option<(u32, u32)> {
    let bus = le_u32(bytes, 20)?;
    let combined = le_u32(bytes, 28)?;
    (bus <= 0xFF && combined != 0 && combined != u32::MAX).then_some((bus, combined >> 16))
}

/// Turns raw library values (degrees, percent, milliwatts, megahertz) into a
/// reading, dropping anything outside what hardware can report.
pub fn sanitize(
    temperature: Option<u32>,
    fan: Option<u32>,
    power_mw: Option<u32>,
    clock: Option<u32>,
) -> Reading {
    Reading {
        temperature_c: temperature.filter(|&t| t > 0 && t <= 150).map(|t| t as f32),
        fan_pct: fan.filter(|&f| f <= 200).map(|f| f.min(100) as f32),
        power_w: power_mw
            .filter(|&p| p > 0 && p <= 2_000_000)
            .map(|p| p as f32 / 1000.0),
        clock_mhz: clock.filter(|&c| c > 0 && c <= 20_000).map(|c| c as f32),
    }
}

/// Finds the library device that is the given display adapter: by PCI bus,
/// else by a name only one device has, else by a device id only one device
/// has, else (one device, one adapter of that maker) the only candidate.
pub fn match_device(
    devices: &[DeviceInfo],
    adapter_name: &str,
    adapter_bus: Option<u32>,
    adapter_device_id: u32,
    adapters_of_vendor: usize,
) -> Option<usize> {
    if let Some(bus) = adapter_bus {
        let mut on_bus = devices
            .iter()
            .enumerate()
            .filter(|(_, d)| d.bus == Some(bus));
        if let (Some((index, _)), None) = (on_bus.next(), on_bus.next()) {
            return Some(index);
        }
    }
    let only = |pred: &dyn Fn(&DeviceInfo) -> bool| {
        let mut hits = devices.iter().enumerate().filter(|(_, d)| pred(d));
        match (hits.next(), hits.next()) {
            (Some((index, _)), None) => Some(index),
            _ => None,
        }
    };
    let wanted = adapter_name.trim();
    if !wanted.is_empty() {
        if let Some(index) = only(&|d| d.name.eq_ignore_ascii_case(wanted)) {
            return Some(index);
        }
    }
    if let Some(index) = only(&|d| d.device_id == Some(adapter_device_id)) {
        return Some(index);
    }
    (devices.len() == 1 && adapters_of_vendor == 1).then_some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, bus: Option<u32>, id: Option<u32>) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            bus,
            device_id: id,
        }
    }

    #[test]
    fn pci_structure_decoding() {
        let mut raw = vec![0u8; 68];
        raw[16..20].copy_from_slice(&0u32.to_le_bytes());
        raw[20..24].copy_from_slice(&3u32.to_le_bytes());
        raw[24..28].copy_from_slice(&0u32.to_le_bytes());
        raw[28..32].copy_from_slice(&0x1ABC_10DEu32.to_le_bytes());
        assert_eq!(parse_pci_info(&raw), Some((3, 0x1ABC)));
        assert_eq!(parse_pci_info(&raw[..31]), None, "truncated");
        assert_eq!(parse_pci_info(&[]), None);
        assert_eq!(
            parse_pci_info(&[0u8; 68]),
            None,
            "all zeros is not a device"
        );
        assert_eq!(
            parse_pci_info(&[0xFFu8; 68]),
            None,
            "all ones is not a device"
        );
    }

    #[test]
    fn readings_are_sanitised() {
        let r = sanitize(Some(61), Some(35), Some(187_500), Some(2100));
        assert_eq!(r.temperature_c, Some(61.0));
        assert_eq!(r.fan_pct, Some(35.0));
        assert_eq!(r.power_w, Some(187.5));
        assert_eq!(r.clock_mhz, Some(2100.0));

        let bogus = sanitize(Some(0), Some(4_000_000), Some(u32::MAX), Some(0));
        assert_eq!(bogus, Reading::default());
        assert_eq!(
            sanitize(Some(151), None, Some(0), Some(50_000)),
            Reading::default()
        );
        assert_eq!(sanitize(None, None, None, None), Reading::default());
        // A fan slightly above 100 % is a rounding artefact of the library.
        assert_eq!(sanitize(None, Some(104), None, None).fan_pct, Some(100.0));
        // A stopped fan is a real reading.
        assert_eq!(sanitize(None, Some(0), None, None).fan_pct, Some(0.0));
    }

    #[test]
    fn devices_match_adapters() {
        let two = [
            device("Example Graphics 9000", Some(1), Some(0x1111)),
            device("Example Graphics 9000", Some(5), Some(0x1111)),
        ];
        // Identical cards are told apart by the bus only.
        assert_eq!(
            match_device(&two, "Example Graphics 9000", Some(5), 0x1111, 2),
            Some(1)
        );
        assert_eq!(
            match_device(&two, "Example Graphics 9000", Some(1), 0x1111, 2),
            Some(0)
        );
        assert_eq!(
            match_device(&two, "Example Graphics 9000", None, 0x1111, 2),
            None
        );
        assert_eq!(
            match_device(&two, "Example Graphics 9000", Some(9), 0x1111, 2),
            None
        );

        let mixed = [
            device("Example Graphics 9000", None, Some(0x1111)),
            device("Example Graphics 7000", None, Some(0x2222)),
        ];
        assert_eq!(
            match_device(&mixed, "example graphics 7000", None, 0, 2),
            Some(1)
        );
        assert_eq!(match_device(&mixed, "Renamed", None, 0x1111, 2), Some(0));
        assert_eq!(match_device(&mixed, "Renamed", None, 0x3333, 2), None);

        let one = [device("", None, None)];
        assert_eq!(match_device(&one, "Anything", None, 0x4444, 1), Some(0));
        assert_eq!(match_device(&one, "Anything", None, 0x4444, 2), None);
        assert_eq!(match_device(&[], "Anything", Some(1), 0x4444, 1), None);
    }

    #[test]
    fn text_helpers() {
        assert_eq!(c_text(b"Example 9000\0junk"), "Example 9000");
        assert_eq!(c_text(b"no terminator"), "no terminator");
        assert_eq!(c_text(&[]), "");
        assert_eq!(c_text(&[0xFF, 0xFE, 0]), "\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn live_load_smoke() {
        match Library::load() {
            Ok(library) => {
                assert!(!library.devices().is_empty());
                let started = std::time::Instant::now();
                for _ in 0..20 {
                    for index in 0..library.devices().len() {
                        let r = library.read(index);
                        assert!(r.temperature_c.is_none_or(|t| (0.0..=150.0).contains(&t)));
                        assert!(r.fan_pct.is_none_or(|f| (0.0..=100.0).contains(&f)));
                    }
                }
                let ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
                assert_eq!(library.read(usize::MAX), Reading::default());
                println!(
                    "vendor library: present yes, devices {}, avg {ms:.3} ms per read",
                    library.devices().len()
                );
            }
            Err(reason) => {
                assert!(!reason.is_empty());
                println!("vendor library: present no ({reason})");
            }
        }
    }
}
