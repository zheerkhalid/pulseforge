//! SMBIOS (DMI) tables: computer, baseboard, firmware and memory modules.

use crate::model::{MemoryModule, SystemInfo};

/// What the SMBIOS tables say. Parsing is separate from reading so that it
/// can be tested with synthetic and corrupt tables.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Smbios {
    /// Only the SMBIOS-derived fields are filled (manufacturer .. bios_date, chassis, memory slots).
    pub system: SystemInfo,
    pub modules: Vec<MemoryModule>,
    /// Socket designation of the first processor (type 4).
    pub cpu_socket: String,
}

/// Upper bound on the structures walked, as a second guard against odd tables.
const MAX_STRUCTURES: usize = 8192;

/// One structure: the formatted area (including its 4-byte header) and the
/// string set that follows it.
struct Structure<'a> {
    kind: u8,
    data: &'a [u8],
    strings: &'a [u8],
}

impl Structure<'_> {
    fn u8(&self, offset: usize) -> Option<u8> {
        self.data.get(offset).copied()
    }

    fn u16(&self, offset: usize) -> Option<u16> {
        let b = self.data.get(offset..offset.checked_add(2)?)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&self, offset: usize) -> Option<u32> {
        let b = self.data.get(offset..offset.checked_add(4)?)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&self, offset: usize) -> Option<u64> {
        let b = self.data.get(offset..offset.checked_add(8)?)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Some(u64::from_le_bytes(a))
    }

    /// The string a 1-based index byte at `offset` refers to, exactly as stored.
    fn raw_str(&self, offset: usize) -> String {
        let index = match self.u8(offset) {
            Some(i) if i > 0 => usize::from(i),
            _ => return String::new(),
        };
        match self.strings.split(|&b| b == 0).nth(index - 1) {
            Some(bytes) => String::from_utf8_lossy(bytes)
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
                .trim()
                .to_string(),
            None => String::new(),
        }
    }

    /// Like [`Structure::raw_str`], with firmware placeholders turned into "".
    fn str(&self, offset: usize) -> String {
        clean(&self.raw_str(offset))
    }
}

/// Splits the table into structures. Stops at the end-of-table structure
/// (type 127), at the end of the data or at the first malformed header.
fn structures(table: &[u8]) -> Vec<Structure<'_>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while out.len() < MAX_STRUCTURES {
        let Some(header) = table.get(pos..pos.saturating_add(4)) else {
            break;
        };
        let kind = header[0];
        let length = usize::from(header[1]);
        if length < 4 {
            break;
        }
        let Some(data) = table.get(pos..pos + length) else {
            break;
        };
        let start = pos + length;
        let rest = table.get(start..).unwrap_or(&[]);
        // The string set ends with two NUL bytes in a row.
        let end = rest.windows(2).position(|w| w == [0, 0]);
        let (strings, next) = match end {
            Some(i) => (&rest[..i], Some(start + i + 2)),
            // Cut off inside the strings: keep what is there and stop.
            None => (rest, None),
        };
        out.push(Structure {
            kind,
            data,
            strings,
        });
        match next {
            Some(n) if kind != 127 => pos = n,
            _ => break,
        }
    }
    out
}

/// How many structures the table holds (for diagnostics).
pub fn structure_count(table: &[u8]) -> usize {
    structures(table).len()
}

/// Turns the placeholder texts firmware vendors leave in unused fields into
/// an empty string and trims the rest.
pub fn clean(text: &str) -> String {
    const PLACEHOLDERS: [&str; 38] = [
        "to be filled by o.e.m.",
        "to be filled by o.e.m",
        "to be filled by oem",
        "default string",
        "default",
        "not specified",
        "not applicable",
        "not available",
        "not provided",
        "not present",
        "none",
        "n/a",
        "na",
        "unknown",
        "undefined",
        "invalid",
        "empty",
        "[empty]",
        "null",
        "oem",
        "o.e.m.",
        "system product name",
        "system manufacturer",
        "system version",
        "system serial number",
        "system family",
        "system sku",
        "sku",
        "sku number",
        "base board serial number",
        "chassis serial number",
        "no asset tag",
        "no dimm",
        "no module installed",
        "type1productconfigid",
        "serial",
        "0123456789",
        "123456789",
    ];
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    let lower = t.to_ascii_lowercase();
    if PLACEHOLDERS.contains(&lower.as_str()) || lower == "1234567890" {
        return String::new();
    }
    // "00000000", "FFFFFFFF", "xxxxxxxx", "........", "0000-0000" and the like.
    let filler = |c: char| matches!(c, '0' | 'f' | 'F' | 'x' | 'X' | '-' | '.' | '_' | ' ' | '*');
    if t.chars().all(filler) {
        let mut letters = t.chars().filter(|c| c.is_ascii_alphanumeric());
        let all_same = match letters.next() {
            Some(first) => letters.all(|c| c.eq_ignore_ascii_case(&first)),
            None => true,
        };
        if all_same {
            return String::new();
        }
    }
    t.to_string()
}

/// "03/14/2025" (or the old "03/14/99") as an ISO date; other texts unchanged.
fn iso_date(text: &str) -> String {
    let parts: Vec<&str> = text.split('/').collect();
    if parts.len() == 3 {
        let numbers: Vec<Option<u32>> = parts.iter().map(|p| p.trim().parse().ok()).collect();
        if let (Some(month), Some(day), Some(year)) = (numbers[0], numbers[1], numbers[2]) {
            let year = if parts[2].trim().len() <= 2 {
                1900 + year
            } else {
                year
            };
            if (1..=12).contains(&month) && (1..=31).contains(&day) && (1900..=9999).contains(&year)
            {
                return format!("{year:04}-{month:02}-{day:02}");
            }
        }
    }
    text.to_string()
}

/// The 16 UUID bytes in the usual text form. SMBIOS 2.6 and later store the
/// first three fields little-endian. All zeros / all 0xFF mean "not set".
fn uuid_text(bytes: &[u8]) -> String {
    if bytes.len() != 16 || bytes.iter().all(|&b| b == 0) || bytes.iter().all(|&b| b == 0xFF) {
        return String::new();
    }
    let b = bytes;
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

fn chassis_name(code: u8) -> &'static str {
    // Bit 7 is the "chassis lock present" flag.
    match code & 0x7F {
        0x03 => "Desktop",
        0x04 => "Low-profile desktop",
        0x05 => "Pizza box",
        0x06 => "Mini tower",
        0x07 => "Tower",
        0x08 => "Portable",
        0x09 => "Laptop",
        0x0A => "Notebook",
        0x0B => "Handheld",
        0x0C => "Docking station",
        0x0D => "All-in-one",
        0x0E => "Sub-notebook",
        0x0F => "Space-saving desktop",
        0x10 => "Lunch box",
        0x11 => "Server",
        0x12 => "Expansion chassis",
        0x13 => "Sub-chassis",
        0x14 => "Bus expansion chassis",
        0x15 => "Peripheral chassis",
        0x16 => "Storage chassis",
        0x17 => "Rack-mount",
        0x18 => "Sealed-case PC",
        0x19 => "Multi-system chassis",
        0x1A => "Compact PCI",
        0x1B => "Advanced TCA",
        0x1C => "Blade",
        0x1D => "Blade enclosure",
        0x1E => "Tablet",
        0x1F => "Convertible",
        0x20 => "Detachable",
        0x21 => "IoT gateway",
        0x22 => "Embedded PC",
        0x23 => "Mini PC",
        0x24 => "Stick PC",
        _ => "",
    }
}

fn form_factor_name(code: u8) -> &'static str {
    match code {
        0x03 => "SIMM",
        0x04 => "SIP",
        0x05 => "Chip",
        0x06 => "DIP",
        0x07 => "ZIP",
        0x08 => "Proprietary card",
        0x09 => "DIMM",
        0x0A => "TSOP",
        0x0B => "Row of chips",
        0x0C => "RIMM",
        0x0D => "SODIMM",
        0x0E => "SRIMM",
        0x0F => "FB-DIMM",
        0x10 => "Die",
        _ => "",
    }
}

fn memory_type_name(code: u8) -> &'static str {
    match code {
        0x03 => "DRAM",
        0x04 => "EDRAM",
        0x05 => "VRAM",
        0x06 => "SRAM",
        0x07 => "RAM",
        0x08 => "ROM",
        0x09 => "Flash",
        0x0A => "EEPROM",
        0x0B => "FEPROM",
        0x0C => "EPROM",
        0x0D => "CDRAM",
        0x0E => "3DRAM",
        0x0F => "SDRAM",
        0x10 => "SGRAM",
        0x11 => "RDRAM",
        0x12 => "DDR",
        0x13 => "DDR2",
        0x14 => "DDR2 FB-DIMM",
        0x18 => "DDR3",
        0x19 => "FBD2",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x1F => "Logical non-volatile",
        0x20 => "HBM",
        0x21 => "HBM2",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        0x24 => "HBM3",
        _ => "",
    }
}

/// Decodes one memory device structure (type 17).
fn memory_module(s: &Structure) -> MemoryModule {
    let mut m = MemoryModule {
        slot: s.str(0x10),
        bank: s.str(0x11),
        ..Default::default()
    };
    let size = s.u16(0x0C).unwrap_or(0);
    if size == 0 {
        return m;
    }
    m.populated = true;
    m.size_bytes = match size {
        // Size unknown.
        0xFFFF => 0,
        // The real size is in the extended field, in megabytes.
        0x7FFF => u64::from(s.u32(0x1C).unwrap_or(0) & 0x7FFF_FFFF) << 20,
        // Bit 15 set: the value is in kilobytes, otherwise megabytes.
        v if v & 0x8000 != 0 => u64::from(v & 0x7FFF) << 10,
        v => u64::from(v) << 20,
    };
    m.form_factor = form_factor_name(s.u8(0x0E).unwrap_or(0)).to_string();
    m.kind = memory_type_name(s.u8(0x12).unwrap_or(0)).to_string();
    let speed = |short: usize, extended: usize| -> Option<u32> {
        match s.u16(short)? {
            0 => None,
            0xFFFF => s.u32(extended).filter(|&v| v > 0 && v < 1_000_000),
            v => Some(u32::from(v)),
        }
    };
    m.speed_mhz = speed(0x15, 0x54);
    m.configured_mhz = speed(0x20, 0x58);
    m.manufacturer = s.str(0x17);
    m.serial = s.str(0x18);
    m.part_number = s.str(0x1A);
    m.rank = s.u8(0x1B).map(|a| a & 0x0F).filter(|&r| r > 0);
    m.voltage_mv = s.u16(0x26).filter(|&v| v > 0 && v != 0xFFFF).map(u32::from);
    let total_width = s.u16(0x08).filter(|&w| w > 0 && w != 0xFFFF);
    m.data_width = s.u16(0x0A).filter(|&w| w > 0 && w != 0xFFFF);
    if let (Some(total), Some(data)) = (total_width, m.data_width) {
        if total > data {
            m.extra.push(("Error correction".into(), "ECC".to_string()));
        }
    }
    if let Some(detail) = s.u16(0x13) {
        let kind = if detail & 0x8000 != 0 {
            "Load-reduced (LRDIMM)"
        } else if detail & 0x2000 != 0 {
            "Registered (buffered)"
        } else if detail & 0x4000 != 0 {
            "Unbuffered"
        } else {
            ""
        };
        if !kind.is_empty() {
            m.extra.push(("Module type".into(), kind.to_string()));
        }
    }
    m
}

/// Parses a raw SMBIOS table blob (the structure table itself, without the
/// 8-byte `RawSMBIOSData` header). Never panics, whatever the input.
pub fn parse(table: &[u8]) -> Smbios {
    let mut out = Smbios::default();
    let mut seen = [false; 5];
    let mut slots_declared: u32 = 0;
    let mut slots_declared_any: u32 = 0;
    let mut max_bytes: u64 = 0;
    let mut have_array = false;
    for s in structures(table) {
        let sys = &mut out.system;
        match s.kind {
            0 if !seen[0] => {
                seen[0] = true;
                sys.bios_vendor = s.str(0x04);
                sys.bios_version = s.str(0x05);
                sys.bios_date = iso_date(&s.str(0x08));
            }
            1 if !seen[1] => {
                seen[1] = true;
                sys.manufacturer = s.str(0x04);
                sys.product = s.str(0x05);
                sys.system_serial = s.str(0x07);
                sys.uuid = s.data.get(0x08..0x18).map(uuid_text).unwrap_or_default();
                sys.sku = s.str(0x19);
                sys.family = s.str(0x1A);
                let version = s.str(0x06);
                if !version.is_empty() {
                    sys.extra.push(("System version".into(), version));
                }
            }
            2 if !seen[2] => {
                seen[2] = true;
                sys.board_manufacturer = s.str(0x04);
                sys.board_product = s.str(0x05);
                sys.board_version = s.str(0x06);
                sys.board_serial = s.str(0x07);
            }
            3 if !seen[3] => {
                seen[3] = true;
                sys.chassis = chassis_name(s.u8(0x05).unwrap_or(0)).to_string();
            }
            4 if !seen[4] => {
                seen[4] = true;
                out.cpu_socket = s.str(0x04);
            }
            16 => {
                have_array = true;
                let devices = u32::from(s.u16(0x0D).unwrap_or(0));
                slots_declared_any = slots_declared_any.saturating_add(devices);
                // Use 3 = system memory (other arrays describe video or cache memory).
                if s.u8(0x05) == Some(0x03) {
                    slots_declared = slots_declared.saturating_add(devices);
                    let capacity = match s.u32(0x07) {
                        Some(0x8000_0000) => s.u64(0x0F).unwrap_or(0),
                        Some(kb) => u64::from(kb) << 10,
                        None => 0,
                    };
                    max_bytes = max_bytes.saturating_add(capacity);
                }
            }
            17 => out.modules.push(memory_module(&s)),
            _ => {}
        }
    }
    let used = out.modules.iter().filter(|m| m.populated).count() as u32;
    let declared = if slots_declared > 0 {
        slots_declared
    } else {
        slots_declared_any
    };
    let total = declared.max(out.modules.len() as u32);
    if have_array || !out.modules.is_empty() {
        out.system.memory_slots_total = Some(total);
        out.system.memory_slots_used = Some(used);
    }
    if max_bytes > 0 {
        out.system.memory_max_bytes = Some(max_bytes);
    }
    out
}

/// The raw structure table of this machine (without the 8-byte header).
pub fn read_raw() -> Result<Vec<u8>, String> {
    use windows::Win32::System::SystemInformation::{
        GetSystemFirmwareTable, FIRMWARE_TABLE_PROVIDER,
    };
    // 'RSMB': the raw SMBIOS firmware table provider.
    let provider = FIRMWARE_TABLE_PROVIDER(0x5253_4D42);
    // SAFETY: a call without a buffer only returns the size needed.
    let size = unsafe { GetSystemFirmwareTable(provider, 0, None) } as usize;
    if size == 0 || size > 16 << 20 {
        return Err("The firmware tables could not be read.".into());
    }
    let mut buf = vec![0u8; size];
    // SAFETY: the slice carries its own length; the call writes at most that many bytes.
    let got = unsafe { GetSystemFirmwareTable(provider, 0, Some(&mut buf)) } as usize;
    if got == 0 || got > buf.len() {
        return Err("The firmware tables could not be read.".into());
    }
    buf.truncate(got);
    if buf.len() < 8 {
        return Err("The firmware tables are empty.".into());
    }
    let declared = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let end = 8usize.saturating_add(declared).min(buf.len());
    Ok(buf[8..end].to_vec())
}

/// Reads and parses the tables of this machine.
pub fn read() -> Result<Smbios, String> {
    read_raw().map(|table| parse(&table))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one structure: header, formatted bytes after the header, strings.
    fn structure(kind: u8, handle: u16, body: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut v = vec![kind, (body.len() + 4) as u8];
        v.extend_from_slice(&handle.to_le_bytes());
        v.extend_from_slice(body);
        if strings.is_empty() {
            v.extend_from_slice(&[0, 0]);
        } else {
            for s in strings {
                v.extend_from_slice(s.as_bytes());
                v.push(0);
            }
            v.push(0);
        }
        v
    }

    fn put16(body: &mut [u8], offset: usize, value: u16) {
        body[offset - 4..offset - 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(body: &mut [u8], offset: usize, value: u32) {
        body[offset - 4..offset].copy_from_slice(&value.to_le_bytes());
    }

    /// A type 17 body (0x5C bytes long including the header).
    fn module_body() -> Vec<u8> {
        vec![0u8; 0x5C - 4]
    }

    fn sample_table() -> Vec<u8> {
        let mut t = Vec::new();
        // Type 0: vendor 1, version 2, date 3.
        let mut b = vec![0u8; 0x18 - 4];
        b[0] = 1;
        b[1] = 2;
        b[4] = 3;
        t.extend(structure(
            0,
            0,
            &b,
            &["Northwind Firmware", "F.42", "03/14/2025"],
        ));
        // Type 1.
        let mut b = vec![0u8; 0x1B - 4];
        b[0] = 1;
        b[1] = 2;
        b[2] = 3;
        b[3] = 4;
        let uuid: [u8; 16] = [
            0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        b[4..20].copy_from_slice(&uuid);
        b[0x19 - 4] = 5;
        b[0x1A - 4] = 6;
        t.extend(structure(
            1,
            1,
            &b,
            &[
                "Contoso Computers",
                "Falcon 15",
                "To Be Filled By O.E.M.",
                "CN-TEST-0001",
                "SKU-778",
                "Falcon",
            ],
        ));
        // Type 2.
        let mut b = vec![0u8; 0x0F - 4];
        b[0] = 1;
        b[1] = 2;
        b[2] = 3;
        b[3] = 4;
        t.extend(structure(
            2,
            2,
            &b,
            &["Contoso Boards", "FX-B550", "Rev 1.02", "Default string"],
        ));
        // Type 3: notebook, with the lock bit set.
        let mut b = vec![0u8; 0x15 - 4];
        b[0] = 1;
        b[1] = 0x8A;
        t.extend(structure(3, 3, &b, &["Contoso Computers"]));
        // Type 4.
        let mut b = vec![0u8; 0x30 - 4];
        b[0] = 1;
        t.extend(structure(4, 4, &b, &["SKT-9000"]));
        // Type 16: system memory, 4 devices, 128 GB via the extended field.
        let mut b = vec![0u8; 0x17 - 4];
        b[0] = 3;
        b[1] = 3;
        put32(&mut b, 0x07, 0x8000_0000);
        put16(&mut b, 0x0D, 4);
        b[0x0F - 4..0x17 - 4].copy_from_slice(&(128u64 << 30).to_le_bytes());
        t.extend(structure(16, 16, &b, &[]));
        // Module 1: 16 GB DDR5 SODIMM.
        let mut b = module_body();
        put16(&mut b, 0x0A, 64);
        put16(&mut b, 0x08, 72);
        put16(&mut b, 0x0C, 16 * 1024);
        b[0x0E - 4] = 0x0D;
        b[0x10 - 4] = 1;
        b[0x11 - 4] = 2;
        b[0x12 - 4] = 0x22;
        put16(&mut b, 0x13, 0x4000);
        put16(&mut b, 0x15, 5600);
        b[0x17 - 4] = 3;
        b[0x18 - 4] = 4;
        b[0x1A - 4] = 5;
        b[0x1B - 4] = 0x12;
        put16(&mut b, 0x20, 4800);
        put16(&mut b, 0x26, 1100);
        t.extend(structure(
            17,
            17,
            &b,
            &[
                "DIMM A1",
                "BANK 0",
                "Fabrikam Memory",
                "TESTSER123",
                "FM5-16G-5600   ",
            ],
        ));
        // Module 2: 64 GB through the extended size field, DDR4.
        let mut b = module_body();
        put16(&mut b, 0x0C, 0x7FFF);
        put32(&mut b, 0x1C, 64 * 1024);
        b[0x0E - 4] = 0x09;
        b[0x10 - 4] = 1;
        b[0x12 - 4] = 0x1A;
        put16(&mut b, 0x15, 0xFFFF);
        put32(&mut b, 0x54, 70_000);
        b[0x18 - 4] = 2;
        t.extend(structure(17, 18, &b, &["DIMM A2", "00000000"]));
        // Module 3: 512 KB (unit bit set).
        let mut b = module_body();
        put16(&mut b, 0x0C, 0x8000 | 512);
        b[0x10 - 4] = 1;
        t.extend(structure(17, 19, &b, &["DIMM B1"]));
        // Module 4: empty slot.
        let mut b = module_body();
        b[0x10 - 4] = 1;
        b[0x11 - 4] = 2;
        t.extend(structure(17, 20, &b, &["DIMM B2", "BANK 3"]));
        // End of table, followed by junk that must be ignored.
        t.extend(structure(127, 127, &[], &[]));
        t.extend(structure(1, 99, &[1, 0, 0, 0], &["Ignored Maker"]));
        t
    }

    #[test]
    fn decodes_a_full_table() {
        let s = parse(&sample_table());
        let y = &s.system;
        assert_eq!(y.bios_vendor, "Northwind Firmware");
        assert_eq!(y.bios_version, "F.42");
        assert_eq!(y.bios_date, "2025-03-14");
        assert_eq!(y.manufacturer, "Contoso Computers");
        assert_eq!(y.product, "Falcon 15");
        assert_eq!(y.system_serial, "CN-TEST-0001");
        assert_eq!(y.sku, "SKU-778");
        assert_eq!(y.family, "Falcon");
        assert_eq!(y.uuid, "00112233-4455-6677-8899-AABBCCDDEEFF");
        assert!(y.extra.is_empty(), "placeholder version is dropped");
        assert_eq!(y.board_manufacturer, "Contoso Boards");
        assert_eq!(y.board_product, "FX-B550");
        assert_eq!(y.board_version, "Rev 1.02");
        assert_eq!(y.board_serial, "");
        assert_eq!(y.chassis, "Notebook");
        assert_eq!(s.cpu_socket, "SKT-9000");
        assert_eq!(y.memory_slots_total, Some(4));
        assert_eq!(y.memory_slots_used, Some(3));
        assert_eq!(y.memory_max_bytes, Some(128 << 30));

        assert_eq!(s.modules.len(), 4);
        let m = &s.modules[0];
        assert!(m.populated);
        assert_eq!(m.slot, "DIMM A1");
        assert_eq!(m.bank, "BANK 0");
        assert_eq!(m.size_bytes, 16 << 30);
        assert_eq!(m.kind, "DDR5");
        assert_eq!(m.form_factor, "SODIMM");
        assert_eq!(m.speed_mhz, Some(5600));
        assert_eq!(m.configured_mhz, Some(4800));
        assert_eq!(m.manufacturer, "Fabrikam Memory");
        assert_eq!(m.part_number, "FM5-16G-5600");
        assert_eq!(m.serial, "TESTSER123");
        assert_eq!(m.voltage_mv, Some(1100));
        assert_eq!(m.rank, Some(2));
        assert_eq!(m.data_width, Some(64));
        assert!(m
            .extra
            .iter()
            .any(|(k, v)| k == "Error correction" && v == "ECC"));
        assert!(m.extra.iter().any(|(_, v)| v == "Unbuffered"));

        let m = &s.modules[1];
        assert_eq!(m.size_bytes, 64 << 30);
        assert_eq!(m.kind, "DDR4");
        assert_eq!(m.form_factor, "DIMM");
        assert_eq!(m.speed_mhz, Some(70_000));
        assert_eq!(m.serial, "", "all-zero serial is a placeholder");
        assert_eq!(m.rank, None);

        assert_eq!(s.modules[2].size_bytes, 512 << 10);
        let m = &s.modules[3];
        assert!(!m.populated);
        assert_eq!(m.slot, "DIMM B2");
        assert_eq!(m.bank, "BANK 3");
        assert_eq!(m.size_bytes, 0);
    }

    #[test]
    fn placeholders_are_empty() {
        for p in [
            "To Be Filled By O.E.M.",
            "Default string",
            "Not Specified",
            "None",
            "System Product Name",
            "00000000",
            "FFFFFFFF",
            "ffff-ffff",
            "  ",
            "0",
            "xxxxxxxx",
            "N/A",
        ] {
            assert_eq!(clean(p), "", "{p}");
        }
        assert_eq!(clean("  FX-100 "), "FX-100");
        assert_eq!(clean("F0F0"), "F0F0");
        assert_eq!(clean("00F1"), "00F1");
    }

    #[test]
    fn dates_and_uuids() {
        assert_eq!(iso_date("12/01/2023"), "2023-12-01");
        assert_eq!(iso_date("07/04/99"), "1999-07-04");
        assert_eq!(iso_date("13/40/2023"), "13/40/2023");
        assert_eq!(iso_date("2023.12.01"), "2023.12.01");
        assert_eq!(iso_date(""), "");
        assert_eq!(uuid_text(&[0; 16]), "");
        assert_eq!(uuid_text(&[0xFF; 16]), "");
        assert_eq!(uuid_text(&[1, 2, 3]), "");
    }

    #[test]
    fn corrupt_tables_do_not_panic() {
        assert_eq!(parse(&[]), Smbios::default());
        let full = sample_table();
        // Every possible truncation: in a header, a formatted area, a string set.
        for cut in 0..full.len() {
            let _ = parse(&full[..cut]);
        }
        // Cut inside the strings of the first structure: fields that are
        // complete are still read.
        let s = parse(&full[..0x18 + 20]);
        assert_eq!(s.system.bios_vendor, "Northwind Firmware");
        // Cut inside the formatted area of the first structure: nothing.
        assert_eq!(parse(&full[..10]), Smbios::default());
        // Length byte smaller than the header.
        assert_eq!(parse(&[1, 3, 0, 0, 1, 2, 3, 0, 0]), Smbios::default());
        assert_eq!(parse(&[1, 0, 0, 0, 0, 0]), Smbios::default());
        // String index past the string set.
        let mut b = vec![0u8; 0x1B - 4];
        b[0] = 9;
        b[1] = 1;
        let s = parse(&structure(1, 1, &b, &["Only One"]));
        assert_eq!(s.system.manufacturer, "");
        assert_eq!(s.system.product, "Only One");
        // No terminator at all.
        let mut no_end = structure(0, 0, &[1, 0, 0, 0, 0], &["Vendor Without End"]);
        no_end.truncate(no_end.len() - 2);
        assert_eq!(parse(&no_end).system.bios_vendor, "Vendor Without End");
        // 64 KB of 0xFF and of zeros.
        let _ = parse(&vec![0xFFu8; 65_536]);
        let _ = parse(&vec![0u8; 65_536]);
        assert_eq!(structure_count(&vec![0u8; 65_536]), 0);
        // Pseudo-random bytes.
        let mut state = 0x1234_5678u32;
        for round in 0..200 {
            let len = 1 + (round * 37) % 3000;
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let _ = parse(&bytes);
        }
        // A short type 17 (old firmware) must not read past its length.
        let s = parse(&structure(17, 5, &[0u8; 0x15 - 4], &[]));
        assert_eq!(s.modules.len(), 1);
        assert!(!s.modules[0].populated);
    }

    #[test]
    fn live_table_is_readable() {
        match read_raw() {
            Ok(table) => {
                let count = structure_count(&table);
                let parsed = parse(&table);
                println!(
                    "SMBIOS: {} bytes, {} structures, {} memory slots, maker known: {}",
                    table.len(),
                    count,
                    parsed.modules.len(),
                    !parsed.system.manufacturer.is_empty()
                );
                assert!(count > 0);
            }
            Err(e) => println!("SMBIOS not readable: {e}"),
        }
    }
}
