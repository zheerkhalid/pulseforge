//! Connected displays: name, mode, scaling and HDR state.

use crate::model::DisplayInfo;
use crate::util::{from_wide, wide};
use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, EnumDisplaySettingsExW, GetMonitorInfoW, DEVMODEW,
    DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
use winreg::RegKey;

/// The parts of an EDID block that are shown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edid {
    /// Three-letter manufacturer id ("ABC").
    pub manufacturer: String,
    /// The monitor name descriptor, if the block has one.
    pub name: String,
    /// Width and height of the picture in millimetres.
    pub size_mm: Option<(u32, u32)>,
    pub year: Option<u32>,
}

/// Parses the 128-byte base block of an EDID. `None` when the header or the
/// checksum is wrong.
pub fn parse_edid(edid: &[u8]) -> Option<Edid> {
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    let block = edid.get(..128)?;
    if block[..8] != HEADER {
        return None;
    }
    if block.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) != 0 {
        return None;
    }
    // Three letters of five bits each, 1 = 'A'.
    let id = u16::from_be_bytes([block[8], block[9]]);
    let letters: Vec<u8> = [10u32, 5, 0]
        .iter()
        .map(|&shift| ((id >> shift) & 0x1F) as u8)
        .collect();
    let manufacturer = if id & 0x8000 == 0 && letters.iter().all(|l| (1..=26).contains(l)) {
        letters.iter().map(|&l| (b'A' + l - 1) as char).collect()
    } else {
        String::new()
    };

    let mut out = Edid {
        manufacturer,
        year: (block[17] > 0 && block[16] != 0xFF).then(|| 1990 + u32::from(block[17])),
        ..Default::default()
    };
    // Whole centimetres from the basic parameters ...
    if block[21] > 0 && block[22] > 0 {
        out.size_mm = Some((u32::from(block[21]) * 10, u32::from(block[22]) * 10));
    }
    for index in 0..4 {
        let d = &block[54 + index * 18..54 + (index + 1) * 18];
        if d[0] != 0 || d[1] != 0 {
            // ... or millimetres from the first detailed timing, which is finer.
            let width = u32::from(d[12]) | (u32::from(d[14] >> 4) << 8);
            let height = u32::from(d[13]) | (u32::from(d[14] & 0x0F) << 8);
            let finer = match out.size_mm {
                // Some timings carry an aspect ratio instead of a size: only
                // believe values close to the centimetre figures.
                Some((w, h)) => width.abs_diff(w) <= 10 && height.abs_diff(h) <= 10,
                None => width >= 50 && height >= 50,
            };
            if finer && index == 0 {
                out.size_mm = Some((width, height));
            }
        } else if d[3] == 0xFC && out.name.is_empty() {
            out.name = d[5..18]
                .iter()
                .take_while(|&&b| b != 0x0A && b != 0)
                .filter(|b| (0x20..0x7F).contains(*b))
                .map(|&b| b as char)
                .collect::<String>()
                .trim()
                .to_string();
        }
    }
    Some(out)
}

/// The screen diagonal in inches, rounded to one decimal.
pub fn diagonal_inches(size_mm: (u32, u32)) -> Option<f32> {
    let (w, h) = (f64::from(size_mm.0), f64::from(size_mm.1));
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let inches = (w * w + h * h).sqrt() / 25.4;
    Some(((inches * 10.0).round() / 10.0) as f32)
}

/// The registry key (below `HKEY_LOCAL_MACHINE`) that holds the EDID of a
/// monitor, from its device interface path
/// (`\\?\DISPLAY#ABC1234#5&1a2b3c&0&UID4352#{...}`).
pub fn edid_registry_path(device_path: &str) -> Option<String> {
    let rest = device_path
        .strip_prefix(r"\\?\")
        .or_else(|| device_path.strip_prefix(r"\\.\"))?;
    let instance = match rest.find("#{") {
        Some(end) => &rest[..end],
        None => rest,
    };
    let parts: Vec<&str> = instance.split('#').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || p.contains('\\')) {
        return None;
    }
    Some(format!(
        r"SYSTEM\CurrentControlSet\Enum\{}\{}\{}\Device Parameters",
        parts[0], parts[1], parts[2]
    ))
}

/// The video output technology code in words.
pub fn connection_name(technology: i32) -> &'static str {
    match technology {
        0 => "VGA",
        1 => "S-Video",
        2 => "Composite",
        3 => "Component",
        4 => "DVI",
        5 => "HDMI",
        6 | 11 | 13 => "Internal",
        8 => "D-Terminal",
        9 => "SDI",
        10 => "DisplayPort",
        12 => "UDI",
        14 => "TV adapter",
        15 => "Wireless (Miracast)",
        16 => "Indirect (wired)",
        17 => "Virtual",
        18 => "DisplayPort (USB-C)",
        i32::MIN => "Internal",
        _ => "Other",
    }
}

pub fn orientation_name(code: u32) -> &'static str {
    match code {
        0 => "Landscape",
        1 => "Portrait",
        2 => "Landscape (flipped)",
        3 => "Portrait (flipped)",
        _ => "",
    }
}

/// Scaling in percent from the effective DPI and, for a process that is not
/// DPI aware (where the DPI always reads 96), from the ratio between the real
/// and the virtualised width.
pub fn scale_percent(dpi: u32, physical_width: u32, logical_width: u32) -> u32 {
    let by_dpi = if (48..=960).contains(&dpi) {
        (f64::from(dpi) / 96.0 * 100.0).round() as u32
    } else {
        100
    };
    let by_ratio = if logical_width > 0 && physical_width > 0 {
        (f64::from(physical_width) / f64::from(logical_width) * 100.0).round() as u32
    } else {
        100
    };
    let scale = if by_dpi != 100 { by_dpi } else { by_ratio };
    if (50..=1000).contains(&scale) {
        scale
    } else {
        100
    }
}

/// A refresh rate from a rational, rounded to two decimals.
pub fn refresh_hz(numerator: u32, denominator: u32, fallback: u32) -> f32 {
    if denominator > 0 && numerator > 0 {
        let hz = f64::from(numerator) / f64::from(denominator);
        if (1.0..=2000.0).contains(&hz) {
            return ((hz * 100.0).round() / 100.0) as f32;
        }
    }
    // 0 and 1 mean "the hardware default".
    if (2..=2000).contains(&fallback) {
        fallback as f32
    } else {
        0.0
    }
}

// ----------------------------------------------------------------------------------
// Reading this machine
// ----------------------------------------------------------------------------------

struct Monitor {
    device: String,
    logical_width: u32,
    primary: bool,
    dpi: u32,
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the address of the Vec owned by `monitors()`, which
    // outlives the enumeration; the callback runs on the calling thread.
    let list = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: the structure is large enough for the size announced in cbSize.
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) };
    if ok.as_bool() {
        let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
        // SAFETY: plain out-parameters.
        let dpi =
            match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } {
                Ok(()) => dpi_x,
                Err(_) => 96,
            };
        let rect = info.monitorInfo.rcMonitor;
        list.push(Monitor {
            device: from_wide(&info.szDevice),
            logical_width: rect.right.saturating_sub(rect.left).max(0) as u32,
            primary: info.monitorInfo.dwFlags & 1 != 0,
            dpi,
        });
    }
    BOOL(1)
}

fn monitors() -> Vec<Monitor> {
    let mut list: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only touches the Vec passed through LPARAM, which
    // lives until the call returns.
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_callback),
            LPARAM(&mut list as *mut Vec<Monitor> as isize),
        );
    }
    list
}

/// The graphics adapter behind a GDI device name such as `\\.\DISPLAY1`.
fn adapter_name(gdi_device: &str) -> String {
    for index in 0..64u32 {
        let mut device = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: the structure has its size set and outlives the call.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) }.as_bool() {
            break;
        }
        if from_wide(&device.DeviceName).eq_ignore_ascii_case(gdi_device) {
            return from_wide(&device.DeviceString).trim().to_string();
        }
    }
    String::new()
}

fn header(
    kind: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: windows::Win32::Foundation::LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: adapter,
        id,
    }
}

fn active_paths() -> Vec<DISPLAYCONFIG_PATH_INFO> {
    // The configuration can change between the two calls: try a few times.
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        // SAFETY: plain out-parameters.
        let sized = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if sized.0 != 0 || path_count == 0 || path_count > 1024 || mode_count > 4096 {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); (mode_count as usize).max(1)];
        // SAFETY: both arrays hold as many elements as the counts say.
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if result.0 == 0 {
            paths.truncate(path_count as usize);
            return paths;
        }
        // ERROR_INSUFFICIENT_BUFFER: the layout changed, ask again.
        if result.0 != 122 {
            break;
        }
    }
    Vec::new()
}

fn read_edid(device_path: &str) -> Option<Edid> {
    let key_path = edid_registry_path(device_path)?;
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(key_path, KEY_READ)
        .ok()?;
    let value = key.get_raw_value("EDID").ok()?;
    parse_edid(&value.bytes)
}

pub fn list() -> Vec<DisplayInfo> {
    let monitors = monitors();
    let mut out = Vec::new();
    for (number, path) in active_paths().iter().enumerate() {
        let mut d = DisplayInfo::default();

        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
                path.targetInfo.adapterId,
                path.targetInfo.id,
            ),
            ..Default::default()
        };
        let mut technology = path.targetInfo.outputTechnology.0;
        let mut device_path = String::new();
        // SAFETY: the header announces the true size of the structure.
        if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } == 0 {
            d.name = from_wide(&target.monitorFriendlyDeviceName)
                .trim()
                .to_string();
            technology = target.outputTechnology.0;
            device_path = from_wide(&target.monitorDevicePath);
        }
        d.connection = connection_name(technology).to_string();

        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                path.sourceInfo.adapterId,
                path.sourceInfo.id,
            ),
            ..Default::default()
        };
        let mut gdi = String::new();
        // SAFETY: as above.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } == 0 {
            gdi = from_wide(&source.viewGdiDeviceName);
        }

        let mut colour = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
                path.targetInfo.adapterId,
                path.targetInfo.id,
            ),
            ..Default::default()
        };
        // SAFETY: as above.
        if unsafe { DisplayConfigGetDeviceInfo(&mut colour.header) } == 0 {
            // SAFETY: the union is a plain 32-bit flag word.
            let flags = unsafe { colour.Anonymous.value };
            d.hdr_supported = Some(flags & 1 != 0);
            d.hdr_enabled = Some(flags & 2 != 0);
            if (1..=32).contains(&colour.bitsPerColorChannel) {
                d.extra.push((
                    "Bits per colour channel".into(),
                    colour.bitsPerColorChannel.to_string(),
                ));
            }
        }

        let mut frequency = 0u32;
        if !gdi.is_empty() {
            d.adapter = adapter_name(&gdi);
            let mut mode = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            let name = wide(&gdi);
            // SAFETY: the name is NUL-terminated; the structure has its size set.
            let ok = unsafe {
                EnumDisplaySettingsExW(
                    PCWSTR(name.as_ptr()),
                    ENUM_CURRENT_SETTINGS,
                    &mut mode,
                    ENUM_DISPLAY_SETTINGS_FLAGS(0),
                )
            };
            if ok.as_bool() {
                d.width = mode.dmPelsWidth;
                d.height = mode.dmPelsHeight;
                d.bits_per_pixel = mode.dmBitsPerPel;
                frequency = mode.dmDisplayFrequency;
                // SAFETY: for a display device this union holds the position
                // and orientation.
                let (position, orientation) = unsafe {
                    (
                        mode.Anonymous1.Anonymous2.dmPosition,
                        mode.Anonymous1.Anonymous2.dmDisplayOrientation.0,
                    )
                };
                d.position = (position.x, position.y);
                d.orientation = orientation_name(orientation).to_string();
            }
        }
        let rate = path.targetInfo.refreshRate;
        d.refresh_hz = refresh_hz(rate.Numerator, rate.Denominator, frequency);

        let monitor = monitors
            .iter()
            .find(|m| m.device.eq_ignore_ascii_case(&gdi));
        d.primary = monitor.map(|m| m.primary).unwrap_or(d.position == (0, 0));
        d.scale_pct = monitor
            .map(|m| scale_percent(m.dpi, d.width, m.logical_width))
            .unwrap_or(100);

        if let Some(edid) = read_edid(&device_path) {
            d.manufacturer = edid.manufacturer;
            d.size_mm = edid.size_mm;
            d.diagonal_inches = edid.size_mm.and_then(diagonal_inches);
            if d.name.is_empty() {
                d.name = edid.name;
            }
            if let Some(year) = edid.year {
                d.extra
                    .push(("Year of manufacture".into(), year.to_string()));
            }
        }
        if d.name.is_empty() {
            d.name = if d.connection == "Internal" {
                "Built-in display".to_string()
            } else {
                format!("Display {}", number + 1)
            };
        }
        out.push(d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid base block: maker "PFG", 60 x 34 cm, name "Studio 27Q".
    fn edid_block() -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // P = 16, F = 6, G = 7.
        let id: u16 = (16 << 10) | (6 << 5) | 7;
        e[8..10].copy_from_slice(&id.to_be_bytes());
        e[16] = 12;
        e[17] = 33;
        e[21] = 60;
        e[22] = 34;
        // Detailed timing with 597 x 336 mm.
        let d = 54;
        e[d] = 0x56;
        e[d + 1] = 0x5E;
        e[d + 12] = (597 & 0xFF) as u8;
        e[d + 13] = (336 & 0xFF) as u8;
        e[d + 14] = (((597 >> 8) as u8) << 4) | ((336 >> 8) as u8);
        // Monitor name descriptor.
        let n = 54 + 18;
        e[n + 3] = 0xFC;
        let name = b"Studio 27Q\n  ";
        e[n + 5..n + 5 + name.len()].copy_from_slice(name);
        fix_checksum(&mut e);
        e
    }

    fn fix_checksum(e: &mut [u8]) {
        let sum = e[..127].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        e[127] = 0u8.wrapping_sub(sum);
    }

    #[test]
    fn edid_basics() {
        let edid = parse_edid(&edid_block()).expect("valid block");
        assert_eq!(edid.manufacturer, "PFG");
        assert_eq!(edid.name, "Studio 27Q");
        assert_eq!(edid.size_mm, Some((597, 336)));
        assert_eq!(edid.year, Some(2023));
        assert_eq!(diagonal_inches((597, 336)), Some(27.0));
        // An extension block after the base block does not matter.
        let mut long = edid_block();
        long.extend_from_slice(&[0xAB; 128]);
        assert!(parse_edid(&long).is_some());
        // Without a detailed timing the centimetre values are used.
        let mut coarse = edid_block();
        for b in &mut coarse[54..72] {
            *b = 0;
        }
        fix_checksum(&mut coarse);
        assert_eq!(
            parse_edid(&coarse).expect("valid").size_mm,
            Some((600, 340))
        );
        // A timing that carries an aspect ratio (16:9) instead of a size.
        let mut ratio = edid_block();
        ratio[54 + 12] = 16;
        ratio[54 + 13] = 9;
        ratio[54 + 14] = 0;
        fix_checksum(&mut ratio);
        assert_eq!(parse_edid(&ratio).expect("valid").size_mm, Some((600, 340)));
        // A projector: no size at all.
        let mut none = coarse.clone();
        none[21] = 0;
        none[22] = 0;
        fix_checksum(&mut none);
        let p = parse_edid(&none).expect("valid");
        assert_eq!(p.size_mm, None);
        assert_eq!(diagonal_inches((0, 0)), None);
    }

    #[test]
    fn corrupt_edid_is_rejected() {
        assert_eq!(parse_edid(&[]), None);
        assert_eq!(parse_edid(&[0u8; 128]), None);
        assert_eq!(parse_edid(&[0xFFu8; 128]), None);
        assert_eq!(parse_edid(&[0xFFu8; 4096]), None);
        let good = edid_block();
        for cut in 0..128 {
            assert_eq!(parse_edid(&good[..cut]), None);
        }
        let mut bad_sum = good.clone();
        bad_sum[127] = bad_sum[127].wrapping_add(1);
        assert_eq!(parse_edid(&bad_sum), None);
        let mut bad_header = good.clone();
        bad_header[1] = 0;
        fix_checksum(&mut bad_header);
        assert_eq!(parse_edid(&bad_header), None);
        // A maker id outside A..Z gives no letters, and an unterminated name
        // made of odd bytes is filtered.
        let mut odd = good.clone();
        odd[8] = 0xFF;
        odd[9] = 0xFF;
        for b in &mut odd[54 + 18 + 5..54 + 36] {
            *b = 0xC3;
        }
        fix_checksum(&mut odd);
        let p = parse_edid(&odd).expect("still a valid block");
        assert_eq!(p.manufacturer, "");
        assert_eq!(p.name, "");
        // Pseudo-random blocks with a valid header and checksum.
        let mut state = 0xDEAD_BEEFu32;
        for _ in 0..200 {
            let mut e: Vec<u8> = (0..128)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
            fix_checksum(&mut e);
            let p = parse_edid(&e).expect("header and checksum are fine");
            assert!(p.manufacturer.is_empty() || p.manufacturer.len() == 3);
            assert!(p.name.len() <= 13);
        }
    }

    #[test]
    fn registry_paths() {
        assert_eq!(
            edid_registry_path(
                r"\\?\DISPLAY#PFG27A1#5&1a2b3c4d&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}"
            )
            .as_deref(),
            Some(
                r"SYSTEM\CurrentControlSet\Enum\DISPLAY\PFG27A1\5&1a2b3c4d&0&UID4352\Device Parameters"
            )
        );
        assert_eq!(edid_registry_path(""), None);
        assert_eq!(edid_registry_path(r"\\?\DISPLAY#ONLYTWO"), None);
        assert_eq!(edid_registry_path(r"DISPLAY#A#B"), None);
        assert_eq!(edid_registry_path(r"\\?\A#B\..\C#D#{x}"), None);
        assert_eq!(edid_registry_path(r"\\?\A##C#{x}"), None);
    }

    #[test]
    fn names_and_numbers() {
        assert_eq!(connection_name(10), "DisplayPort");
        assert_eq!(connection_name(5), "HDMI");
        assert_eq!(connection_name(4), "DVI");
        assert_eq!(connection_name(0), "VGA");
        assert_eq!(connection_name(11), "Internal");
        assert_eq!(connection_name(i32::MIN), "Internal");
        assert_eq!(connection_name(-1), "Other");
        assert_eq!(connection_name(777), "Other");
        assert_eq!(orientation_name(0), "Landscape");
        assert_eq!(orientation_name(1), "Portrait");
        assert_eq!(orientation_name(9), "");
        assert_eq!(scale_percent(144, 2560, 2560), 150);
        assert_eq!(scale_percent(96, 2560, 2560), 100);
        assert_eq!(scale_percent(96, 2560, 1707), 150, "DPI-unaware process");
        assert_eq!(scale_percent(96, 1920, 1536), 125);
        assert_eq!(scale_percent(0, 0, 0), 100);
        assert_eq!(scale_percent(u32::MAX, u32::MAX, 1), 100);
        assert_eq!(refresh_hz(60_000, 1_001, 59), 59.94);
        assert_eq!(refresh_hz(144, 1, 0), 144.0);
        assert_eq!(refresh_hz(0, 0, 60), 60.0);
        assert_eq!(refresh_hz(5, 0, 1), 0.0);
        assert_eq!(refresh_hz(u32::MAX, 1, u32::MAX), 0.0);
    }

    #[test]
    fn live_displays() {
        let list = list();
        println!("displays: {}", list.len());
        for d in &list {
            println!(
                "  {}x{} @ {} Hz, {} bpp, scale {} %, connection {}, primary {}, name known {}, adapter known {}, maker id known {}, size known {}, hdr known {}, orientation {}",
                d.width,
                d.height,
                d.refresh_hz,
                d.bits_per_pixel,
                d.scale_pct,
                d.connection,
                d.primary,
                !d.name.is_empty(),
                !d.adapter.is_empty(),
                !d.manufacturer.is_empty(),
                d.size_mm.is_some(),
                d.hdr_supported.is_some(),
                d.orientation,
            );
            assert!((50..=1000).contains(&d.scale_pct));
            assert!(!d.name.is_empty());
        }
        // A session without a desktop (a service, some remote sessions) has none.
        if !list.is_empty() {
            assert!(list.iter().all(|d| d.width >= 320 && d.height >= 200));
            assert!(list.iter().filter(|d| d.primary).count() <= 1);
        }
    }
}
