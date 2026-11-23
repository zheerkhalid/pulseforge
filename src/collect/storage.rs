//! Physical drives, their health, partitions, volumes and live activity.
//!
//! Everything here is a read-only query. No command that writes, trims,
//! erases or changes a drive setting is ever sent.

use super::smart::{self, DriveKind};
use crate::model::{DiskSample, DriveInfo, HealthStatus, Partition, Volume, NEEDS_ADMIN};
use crate::util::{from_wide, wide};
use std::ffi::c_void;
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW,
    GetVolumeInformationW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Diagnostics::Debug::{
    SetThreadErrorMode, SEM_FAILCRITICALERRORS, THREAD_ERROR_MODE,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::UI::Shell::PropertiesSystem::{PSGetPropertyKeyFromName, PROPERTYKEY};
use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};

/// Highest `PhysicalDriveN` index that is probed.
const MAX_DRIVE_INDEX: u32 = 63;
/// Seconds between rescans of the drive list by [`DiskSampler`].
const RESCAN_S: f64 = 10.0;

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_STORAGE_PREDICT_FAILURE: u32 = 0x002D_1100;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
const IOCTL_DISK_GET_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;
const IOCTL_DISK_PERFORMANCE: u32 = 0x0007_0020;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const IOCTL_ATA_PASS_THROUGH: u32 = 0x0004_D02C;
const SMART_RCV_DRIVE_DATA: u32 = 0x0007_C088;

const PROPERTY_DEVICE: u32 = 0;
const PROPERTY_SEEK_PENALTY: u32 = 7;
const PROPERTY_TRIM: u32 = 8;
const PROPERTY_PROTOCOL_SPECIFIC: u32 = 50;
const PROPERTY_TEMPERATURE: u32 = 52;

const GENERIC_READ_WRITE: u32 = 0xC000_0000;
/// `HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)`.
pub(crate) const ACCESS_DENIED: u32 = 0x8007_0005;

/// The two read-only SMART sub-commands that are used.
const SMART_READ_DATA: u8 = 0xD0;
const SMART_READ_THRESHOLDS: u8 = 0xD1;

// ----------------------------------------------------------------------------------
// Handles and control codes
// ----------------------------------------------------------------------------------

/// An open device handle that is closed when dropped.
pub(crate) struct Handle(HANDLE);

// SAFETY: a kernel handle is not tied to the thread that opened it.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateFileW and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Opens a device path. `access` 0 asks for no access at all, which is
/// enough for property queries and works without administrator rights.
pub(crate) fn open_device(path: &str, access: u32) -> Result<Handle, u32> {
    let w = wide(path);
    // SAFETY: the path is NUL-terminated and outlives the call.
    let result = unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            HANDLE::default(),
        )
    };
    match result {
        Ok(h) if !h.is_invalid() => Ok(Handle(h)),
        Ok(_) => Err(0),
        Err(e) => Err(e.code().0 as u32),
    }
}

/// Sends a control code. Returns the number of bytes written to `output`,
/// or the error code.
pub(crate) fn ioctl(
    handle: &Handle,
    code: u32,
    input: &[u8],
    output: &mut [u8],
) -> Result<usize, u32> {
    let mut returned = 0u32;
    // SAFETY: both buffers are live slices and their true lengths are passed.
    let result = unsafe {
        DeviceIoControl(
            handle.0,
            code,
            if input.is_empty() {
                None
            } else {
                Some(input.as_ptr() as *const c_void)
            },
            input.len() as u32,
            if output.is_empty() {
                None
            } else {
                Some(output.as_mut_ptr() as *mut c_void)
            },
            output.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    match result {
        Ok(()) => Ok((returned as usize).min(output.len())),
        Err(e) => Err(e.code().0 as u32),
    }
}

pub(crate) fn le_u16(buf: &[u8], offset: usize) -> Option<u16> {
    let b = buf.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn le_u32(buf: &[u8], offset: usize) -> Option<u32> {
    let b = buf.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub(crate) fn le_u64(buf: &[u8], offset: usize) -> Option<u64> {
    let b = buf.get(offset..offset.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Some(u64::from_le_bytes(a))
}

/// A size or offset the system reports as a signed 64-bit number.
fn le_size(buf: &[u8], offset: usize) -> Option<u64> {
    le_u64(buf, offset).filter(|&v| v <= i64::MAX as u64)
}

// ----------------------------------------------------------------------------------
// Pure decoding
// ----------------------------------------------------------------------------------

/// What `StorageDeviceProperty` says about a drive.
#[derive(Clone, Debug, Default, PartialEq)]
struct DeviceDescriptor {
    model: String,
    firmware: String,
    serial: String,
    bus: u32,
    removable: bool,
}

/// The NUL-terminated ASCII string at `offset` of a descriptor buffer.
fn descriptor_string(buf: &[u8], offset: u32) -> String {
    let offset = offset as usize;
    if offset == 0 || offset >= buf.len() {
        return String::new();
    }
    let rest = &buf[offset..];
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    rest[..end.min(256)]
        .iter()
        .filter(|b| (0x20..0x7F).contains(*b))
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

fn parse_device_descriptor(buf: &[u8]) -> Option<DeviceDescriptor> {
    if buf.len() < 36 {
        return None;
    }
    let vendor = descriptor_string(buf, le_u32(buf, 12)?);
    let product = descriptor_string(buf, le_u32(buf, 16)?);
    let model = if vendor.is_empty() || product.starts_with(&vendor) {
        product
    } else if product.is_empty() {
        vendor
    } else {
        format!("{vendor} {product}")
    };
    Some(DeviceDescriptor {
        model: model.split_whitespace().collect::<Vec<_>>().join(" "),
        firmware: descriptor_string(buf, le_u32(buf, 20)?),
        serial: normalize_serial(&descriptor_string(buf, le_u32(buf, 24)?)),
        bus: le_u32(buf, 28)?,
        removable: buf[10] != 0,
    })
}

/// Tidies a serial number. Some drivers hand out the ATA serial as text in
/// hexadecimal with every pair of characters swapped: that form is decoded.
pub fn normalize_serial(raw: &str) -> String {
    let t = raw.trim().trim_end_matches('.').trim();
    if t.len() >= 16 && t.len().is_multiple_of(2) && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes: Vec<u8> = (0..t.len() / 2)
            .filter_map(|i| u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).ok())
            .collect();
        if bytes.len() == t.len() / 2 && bytes.iter().all(|b| (0x20..0x7F).contains(b)) {
            let mut swapped = bytes.clone();
            for pair in swapped.as_chunks_mut::<2>().0 {
                pair.swap(0, 1);
            }
            let text: String = swapped.iter().map(|&b| b as char).collect();
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    t.to_string()
}

fn bus_name(bus: u32) -> &'static str {
    match bus {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "FireWire",
        6 => "Fibre Channel",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 | 15 => "Virtual",
        16 => "Storage Spaces",
        17 => "NVMe",
        18 => "Storage-class memory",
        19 => "UFS",
        _ => "",
    }
}

/// A GUID stored in the usual mixed-endian layout, as upper-case text.
fn guid_text(b: &[u8]) -> String {
    if b.len() < 16 {
        return String::new();
    }
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

fn gpt_type_name(guid: &str) -> &'static str {
    match guid {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI system",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reserved",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Basic data",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Recovery",
        "5808C8AA-7E8F-42E0-85D2-E1E90434CFB3" => "Dynamic disk metadata",
        "AF9B60A0-1431-4F62-BC68-3311714A69AD" => "Dynamic disk data",
        "E75CAF8F-F680-4CEE-AFA3-B001E56EFC2D" => "Storage Spaces",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux file system",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Linux swap",
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "Linux LVM",
        "A19D880F-05FC-4D3B-A006-743F0F84911E" => "Linux RAID",
        "21686148-6449-6E6F-744E-656564454649" => "BIOS boot",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "Apple APFS",
        _ => "Other",
    }
}

fn mbr_type_name(kind: u8) -> &'static str {
    match kind {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x05 | 0x0F => "Extended",
        0x07 => "NTFS / exFAT",
        0x0B | 0x0C => "FAT32",
        0x12 | 0x27 => "Recovery",
        0x42 => "Dynamic disk",
        0x82 => "Linux swap",
        0x83 => "Linux file system",
        0x8E => "Linux LVM",
        0xEE => "GPT protective",
        0xEF => "EFI system",
        _ => "Other",
    }
}

/// Decodes a `DRIVE_LAYOUT_INFORMATION_EX` buffer into the partition style
/// and the partitions in use.
fn parse_layout(buf: &[u8]) -> (String, Vec<Partition>) {
    const HEADER: usize = 48;
    const ENTRY: usize = 144;
    let (Some(style), Some(count)) = (le_u32(buf, 0), le_u32(buf, 4)) else {
        return (String::new(), Vec::new());
    };
    let style_name = match style {
        0 => "MBR",
        1 => "GPT",
        2 => "RAW",
        _ => "",
    };
    let mut partitions = Vec::new();
    for i in 0..(count as usize).min(256) {
        let Some(entry) = buf.get(HEADER + i * ENTRY..HEADER + (i + 1) * ENTRY) else {
            break;
        };
        let (Some(offset), Some(size), Some(number)) =
            (le_size(entry, 8), le_size(entry, 16), le_u32(entry, 24))
        else {
            continue;
        };
        if size == 0 {
            continue;
        }
        let kind = match le_u32(entry, 0) {
            Some(0) => {
                if entry[32] == 0 {
                    continue;
                }
                mbr_type_name(entry[32])
            }
            Some(1) => gpt_type_name(&guid_text(&entry[32..48])),
            _ => "Other",
        };
        partitions.push(Partition {
            number,
            kind: kind.to_string(),
            offset,
            size,
            letter: String::new(),
        });
    }
    partitions.sort_by_key(|p| p.offset);
    (style_name.to_string(), partitions)
}

/// The first believable temperature (°C) in a
/// `STORAGE_TEMPERATURE_DATA_DESCRIPTOR`.
fn parse_temperature(buf: &[u8]) -> Option<f32> {
    let count = usize::from(le_u16(buf, 12)?);
    (0..count.min(16))
        .filter_map(|i| le_u16(buf, 24 + i * 16 + 2))
        .map(|raw| raw as i16)
        .find(|&t| (1..=125).contains(&t))
        .map(f32::from)
}

/// The counters of one `DISK_PERFORMANCE` reading.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Perf {
    read: u64,
    written: u64,
    idle: u64,
    query: u64,
    queue: u32,
}

fn parse_perf(buf: &[u8]) -> Option<Perf> {
    Some(Perf {
        read: le_size(buf, 0)?,
        written: le_size(buf, 8)?,
        idle: le_size(buf, 32)?,
        queue: le_u32(buf, 48)?,
        query: le_size(buf, 56)?,
    })
}

/// Rates between two readings of the same drive.
fn rates(index: u32, prev: &Perf, now: &Perf) -> DiskSample {
    let ticks = now.query.saturating_sub(prev.query);
    let mut sample = DiskSample {
        index,
        queue_depth: Some(now.queue.min(100_000) as f32),
        ..Default::default()
    };
    if ticks == 0 {
        return sample;
    }
    // Times are in units of 100 nanoseconds.
    let seconds = ticks as f64 / 1e7;
    sample.read_bps = now.read.saturating_sub(prev.read) as f64 / seconds;
    sample.write_bps = now.written.saturating_sub(prev.written) as f64 / seconds;
    if now.idle > 0 && now.idle >= prev.idle {
        let idle = (now.idle - prev.idle) as f64 / ticks as f64;
        sample.active_pct = ((1.0 - idle) * 100.0).clamp(0.0, 100.0) as f32;
    }
    sample
}

// ----------------------------------------------------------------------------------
// Device queries
// ----------------------------------------------------------------------------------

fn drive_path(index: u32) -> String {
    format!(r"\\.\PhysicalDrive{index}")
}

/// `IOCTL_STORAGE_QUERY_PROPERTY` for a standard descriptor.
fn query_property(handle: &Handle, property: u32, size: usize) -> Option<Vec<u8>> {
    let mut input = [0u8; 12];
    input[..4].copy_from_slice(&property.to_le_bytes());
    let mut out = vec![0u8; size];
    let n = ioctl(handle, IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut out).ok()?;
    out.truncate(n);
    Some(out)
}

/// The NVMe SMART / Health Information log page through the storage
/// protocol query (a read-only request handled by the Windows NVMe driver).
fn nvme_health_log(handle: &Handle) -> Result<Vec<u8>, u32> {
    const QUERY: usize = 8;
    const SPECIFIC: usize = 40;
    let mut buf = vec![0u8; QUERY + SPECIFIC + 4096];
    let fields: [u32; 8] = [
        PROPERTY_PROTOCOL_SPECIFIC,
        0,                   // PropertyStandardQuery
        3,                   // ProtocolTypeNvme
        2,                   // NVMeDataTypeLogPage
        2,                   // log page: SMART / health information
        0,                   // sub value
        SPECIFIC as u32,     // data offset
        smart::BLOCK as u32, // data length
    ];
    for (i, v) in fields.iter().enumerate() {
        buf[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    let input = buf.clone();
    let n = ioctl(handle, IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut buf)?;
    let out = &buf[..n];
    let offset = le_u32(out, QUERY + 16).ok_or(0u32)? as usize;
    let start = QUERY.checked_add(offset).ok_or(0u32)?;
    out.get(start..start + smart::BLOCK)
        .map(<[u8]>::to_vec)
        .ok_or(0)
}

/// `ATA_PASS_THROUGH_EX` followed by one sector of data.
#[repr(C)]
struct AtaRequest {
    length: u16,
    flags: u16,
    path_id: u8,
    target_id: u8,
    lun: u8,
    reserved: u8,
    data_transfer_length: u32,
    timeout_s: u32,
    reserved2: u32,
    data_buffer_offset: usize,
    previous_task_file: [u8; 8],
    current_task_file: [u8; 8],
    data: [u8; 512],
}

/// One SMART read (data or thresholds) through the ATA pass-through.
/// Only the two read sub-commands are ever sent.
fn ata_smart_read(handle: &Handle, feature: u8) -> Result<Vec<u8>, u32> {
    debug_assert!(feature == SMART_READ_DATA || feature == SMART_READ_THRESHOLDS);
    const DRDY_REQUIRED: u16 = 0x01;
    const DATA_IN: u16 = 0x02;
    let header = std::mem::offset_of!(AtaRequest, data);
    let mut request = AtaRequest {
        length: header as u16,
        flags: DRDY_REQUIRED | DATA_IN,
        path_id: 0,
        target_id: 0,
        lun: 0,
        reserved: 0,
        data_transfer_length: 512,
        timeout_s: 5,
        reserved2: 0,
        data_buffer_offset: header,
        previous_task_file: [0; 8],
        // features, sector count, LBA low, LBA mid, LBA high, device, command (SMART).
        current_task_file: [feature, 1, 0, 0x4F, 0xC2, 0xA0, 0xB0, 0],
        data: [0; 512],
    };
    let size = std::mem::size_of::<AtaRequest>() as u32;
    let pointer = &mut request as *mut AtaRequest as *mut c_void;
    let mut returned = 0u32;
    // SAFETY: the request is a plain repr(C) value that lives across the call;
    // the same buffer is used for input and output with its exact size.
    let result = unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_ATA_PASS_THROUGH,
            Some(pointer as *const c_void),
            size,
            Some(pointer),
            size,
            Some(&mut returned),
            None,
        )
    };
    result.map_err(|e| e.code().0 as u32)?;
    // Bit 0 of the status register: the drive rejected the command.
    if request.current_task_file[6] & 0x01 != 0 || (returned as usize) < header + 512 {
        return Err(0);
    }
    Ok(request.data.to_vec())
}

/// The same read through the older `SMART_RCV_DRIVE_DATA` request.
fn legacy_smart_read(handle: &Handle, feature: u8) -> Result<Vec<u8>, u32> {
    debug_assert!(feature == SMART_READ_DATA || feature == SMART_READ_THRESHOLDS);
    // SENDCMDINPARAMS: buffer size, the eight task-file registers, drive number.
    let mut input = [0u8; 32];
    input[..4].copy_from_slice(&512u32.to_le_bytes());
    input[4..12].copy_from_slice(&[feature, 1, 1, 0x4F, 0xC2, 0xA0, 0xB0, 0]);
    // SENDCMDOUTPARAMS: buffer size, driver status (12 bytes), data.
    let mut out = vec![0u8; 16 + 512];
    let n = ioctl(handle, SMART_RCV_DRIVE_DATA, &input, &mut out)?;
    if n < 16 + 512 || out[4] != 0 {
        return Err(0);
    }
    Ok(out[16..16 + 512].to_vec())
}

/// `IOCTL_STORAGE_PREDICT_FAILURE`: the drive's own yes/no, plus the
/// vendor data block (often the SMART data sector on ATA drives).
fn predict_failure(handle: &Handle) -> Result<(bool, Vec<u8>), u32> {
    let mut out = vec![0u8; 4 + 512];
    let n = ioctl(handle, IOCTL_STORAGE_PREDICT_FAILURE, &[], &mut out)?;
    if n < 4 {
        return Err(0);
    }
    let failing = le_u32(&out, 0).unwrap_or(0) != 0;
    out.truncate(n);
    Ok((failing, out.split_off(4)))
}

fn disk_size(handle: &Handle) -> (u64, Option<u32>) {
    let mut out = [0u8; 256];
    if let Ok(n) = ioctl(handle, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut out) {
        let buf = &out[..n];
        if let Some(size) = le_size(buf, 24) {
            return (size, le_u32(buf, 20).filter(|&s| s > 0));
        }
    }
    let mut out = [0u8; 8];
    match ioctl(handle, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut out) {
        Ok(n) => (le_size(&out[..n], 0).unwrap_or(0), None),
        Err(_) => (0, None),
    }
}

fn layout(handle: &Handle) -> (String, Vec<Partition>) {
    let mut size = 48 + 144 * 16;
    // Grow the buffer until the partition table fits.
    for _ in 0..5 {
        let mut out = vec![0u8; size];
        match ioctl(handle, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, &[], &mut out) {
            Ok(n) => return parse_layout(&out[..n]),
            // ERROR_INSUFFICIENT_BUFFER / ERROR_MORE_DATA.
            Err(0x8007_007A) | Err(0x8007_00EA) => size *= 4,
            Err(_) => break,
        }
    }
    (String::new(), Vec::new())
}

/// Where a lettered volume lives: (letter, drive index, offset on the drive).
fn letter_extents() -> Vec<(String, u32, u64)> {
    let mut found = Vec::new();
    for root in logical_roots() {
        let letter = root.trim_end_matches('\\').to_string();
        let w = wide(&root);
        // SAFETY: NUL-terminated string that outlives the call.
        let kind = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
        // Only removable (2) and fixed (3) drives sit on a physical drive.
        if kind != 2 && kind != 3 {
            continue;
        }
        let Ok(handle) = open_device(&format!(r"\\.\{letter}"), 0) else {
            continue;
        };
        let mut out = [0u8; 8 + 24 * 8];
        let Ok(n) = ioctl(&handle, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut out) else {
            continue;
        };
        let buf = &out[..n];
        let count = le_u32(buf, 0).unwrap_or(0) as usize;
        for i in 0..count.min(8) {
            if let (Some(disk), Some(offset)) = (le_u32(buf, 8 + i * 24), le_size(buf, 16 + i * 24))
            {
                found.push((letter.clone(), disk, offset));
            }
        }
    }
    found
}

/// "C:\", "D:\", ...
fn logical_roots() -> Vec<String> {
    let mut buf = [0u16; 512];
    // SAFETY: the slice carries its own length.
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return Vec::new();
    }
    buf[..n]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Reads whatever health information the drive gives up and fills the
/// health fields of `d`.
fn read_health(handle: &Handle, bus: u32, d: &mut DriveInfo) {
    let kind = if bus == 17 {
        DriveKind::Nvme
    } else if d.media == "HDD" {
        DriveKind::AtaHdd
    } else {
        DriveKind::AtaSsd
    };
    let property_temperature =
        query_property(handle, PROPERTY_TEMPERATURE, 512).and_then(|b| parse_temperature(&b));
    let mut denied = false;
    let mut attributes = Vec::new();
    let mut failing: Option<bool> = None;

    if bus == 17 {
        let mut log = nvme_health_log(handle);
        if log.is_err() {
            // Some drivers only answer on a handle with read/write access.
            match open_device(&drive_path(d.index), GENERIC_READ_WRITE) {
                Ok(rw) => log = nvme_health_log(&rw),
                Err(code) => denied |= code == ACCESS_DENIED,
            }
        }
        match log {
            Ok(page) => attributes = smart::parse_nvme(&page),
            Err(code) => denied |= code == ACCESS_DENIED,
        }
        if !attributes.is_empty() {
            d.smart_source = "NVMe health log".into();
        }
    } else if matches!(bus, 2 | 3 | 11) {
        match open_device(&drive_path(d.index), GENERIC_READ_WRITE) {
            Ok(rw) => {
                let read = |feature: u8| {
                    ata_smart_read(&rw, feature).or_else(|_| legacy_smart_read(&rw, feature))
                };
                match read(SMART_READ_DATA) {
                    Ok(data) => {
                        let thresholds = read(SMART_READ_THRESHOLDS).unwrap_or_default();
                        attributes = smart::parse_ata(&data, &thresholds, kind);
                    }
                    Err(code) => denied |= code == ACCESS_DENIED,
                }
            }
            Err(code) => denied |= code == ACCESS_DENIED,
        }
        if !attributes.is_empty() {
            d.smart_source = "ATA SMART".into();
        }
    }

    if attributes.is_empty() {
        // Last resort: the drive's own yes/no through Windows.
        match predict_failure(handle) {
            Ok((fail, vendor)) => {
                failing = Some(fail);
                if matches!(bus, 2 | 3 | 11) {
                    let found = smart::parse_ata(&vendor, &[], kind);
                    // Only believe the block when it carries the two counters
                    // every SMART drive has.
                    let has = |id: u8| found.iter().any(|a| a.id == Some(id));
                    if has(9) && has(12) {
                        attributes = found;
                    }
                }
                d.smart_source = "Windows storage health".into();
            }
            Err(code) => denied |= code == ACCESS_DENIED,
        }
    }

    let summary = smart::summarize(&attributes, kind);
    d.health = summary.health;
    d.health_pct = summary.health_pct;
    d.health_summary = summary.health_summary;
    d.temperature_c = summary.temperature_c.or(property_temperature);
    d.power_on_hours = summary.power_on_hours;
    d.power_cycles = summary.power_cycles;
    d.total_written_bytes = summary.total_written_bytes;
    d.total_read_bytes = summary.total_read_bytes;
    d.attributes = attributes;

    let elevated = crate::sys::is_elevated();
    if d.smart_source != "NVMe health log" && d.smart_source != "ATA SMART" {
        d.smart_note = if denied || (!elevated && matches!(bus, 2 | 3 | 11 | 17)) {
            NEEDS_ADMIN.to_string()
        } else {
            "this kind of connection does not pass health details through".to_string()
        };
    }
    match failing {
        Some(true) => {
            d.health = HealthStatus::Bad;
            d.health_summary = "This drive itself predicts that it will fail. Back up your data now and plan to replace it.".into();
        }
        Some(false) if d.attributes.is_empty() => {
            d.health = HealthStatus::Good;
            d.health_summary = format!(
                "Windows reports that this drive does not predict a failure. Detailed health data: {}.",
                d.smart_note
            );
        }
        Some(false) if d.health == HealthStatus::Good || d.health == HealthStatus::Unknown => {
            // Attributes without their limits, read through Windows.
            d.extra
                .push(("SMART limits (thresholds)".into(), d.smart_note.clone()));
        }
        _ => {}
    }
    if d.health == HealthStatus::Unknown && d.health_summary.is_empty() {
        d.health_summary = format!(
            "The health of this drive could not be read: {}.",
            d.smart_note
        );
    }
}

fn read_drive(index: u32, letters: &[(String, u32, u64)]) -> Option<DriveInfo> {
    let handle = open_device(&drive_path(index), 0).ok()?;
    let mut d = DriveInfo {
        index,
        ..Default::default()
    };
    let descriptor = query_property(&handle, PROPERTY_DEVICE, 1024)
        .and_then(|b| parse_device_descriptor(&b))
        .unwrap_or_default();
    let bus = descriptor.bus;
    d.model = descriptor.model;
    d.firmware = descriptor.firmware;
    d.serial = descriptor.serial;
    d.removable = descriptor.removable;
    d.interface = bus_name(bus).to_string();

    let seek_penalty = query_property(&handle, PROPERTY_SEEK_PENALTY, 16)
        .filter(|b| b.len() >= 9)
        .map(|b| b[8] != 0);
    let trim = query_property(&handle, PROPERTY_TRIM, 16)
        .filter(|b| b.len() >= 9)
        .map(|b| b[8] != 0);
    d.media = match (seek_penalty, trim, bus) {
        (_, _, 17) => "SSD",
        // Memory cards and virtual disks are neither.
        (_, _, 12..=16) => "",
        (Some(true), _, _) => "HDD",
        (Some(false), _, _) => "SSD",
        (None, Some(true), _) => "SSD",
        _ => "",
    }
    .to_string();
    if let Some(trim) = trim {
        d.extra.push((
            "TRIM".into(),
            if trim { "Supported" } else { "Not supported" }.to_string(),
        ));
    }

    let (size, sector) = disk_size(&handle);
    d.size_bytes = size;
    if let Some(sector) = sector {
        d.extra
            .push(("Sector size".into(), format!("{sector} bytes")));
    }
    let (style, mut partitions) = layout(&handle);
    for p in &mut partitions {
        if let Some((letter, ..)) = letters.iter().find(|(_, disk, offset)| {
            *disk == index && *offset >= p.offset && *offset < p.offset.saturating_add(p.size)
        }) {
            p.letter = letter.clone();
        }
    }
    d.partition_style = style;
    d.partitions = partitions;

    read_health(&handle, bus, &mut d);
    Some(d)
}

/// Every physical drive with identification, partitions and health.
/// Takes tens of milliseconds per drive: call it rarely (about once a minute).
pub fn drives() -> Vec<DriveInfo> {
    let letters = letter_extents();
    (0..=MAX_DRIVE_INDEX)
        .filter_map(|index| read_drive(index, &letters))
        .collect()
}

/// Mounted volumes with their usage.
pub fn volumes() -> Vec<Volume> {
    let extents = letter_extents();
    let mut old_mode = THREAD_ERROR_MODE(0);
    // SAFETY: only affects this thread; stops "no disk in drive" dialogs for
    // empty card readers. The previous mode is restored below.
    let mode_set =
        unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old_mode)) }.is_ok();
    // SAFETY: initialises COM for this thread (the shell is asked for the
    // encryption state); balanced below when it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let key = bitlocker_key();
    let mut out = Vec::new();
    for root in logical_roots() {
        let w = wide(&root);
        let path = PCWSTR(w.as_ptr());
        // SAFETY: NUL-terminated string that outlives the calls below.
        let kind = match unsafe { GetDriveTypeW(path) } {
            2 => "Removable",
            3 => "Fixed",
            4 => "Network",
            5 => "Optical",
            6 => "RAM disk",
            _ => continue,
        };
        let mount = root.trim_end_matches('\\').to_string();
        let mut label = [0u16; 261];
        let mut filesystem = [0u16; 261];
        // SAFETY: the slices carry their own lengths.
        let mounted = unsafe {
            GetVolumeInformationW(
                path,
                Some(&mut label),
                None,
                None,
                None,
                Some(&mut filesystem),
            )
        }
        .is_ok();
        let (mut total, mut free) = (0u64, 0u64);
        if mounted {
            // SAFETY: plain out-parameters.
            let _ = unsafe { GetDiskFreeSpaceExW(path, None, Some(&mut total), Some(&mut free)) };
        } else if kind != "Fixed" {
            // An empty card reader or optical drive.
            continue;
        }
        out.push(Volume {
            drive_index: extents
                .iter()
                .find(|(letter, ..)| *letter == mount)
                .map(|(_, disk, _)| *disk),
            mount,
            label: from_wide(&label),
            filesystem: from_wide(&filesystem),
            total,
            free: free.min(total),
            bitlocker: if kind == "Fixed" || kind == "Removable" {
                key.as_ref().and_then(|key| bitlocker_state(&root, key))
            } else {
                None
            },
            kind: kind.to_string(),
        });
    }
    if mode_set {
        // SAFETY: restores the mode saved above.
        let _ = unsafe { SetThreadErrorMode(old_mode, None) };
    }
    if com.is_ok() {
        // SAFETY: balances the successful initialisation above.
        unsafe { CoUninitialize() };
    }
    out
}

/// What the shell's "BitLocker protection" number means: `Some(true)` for a
/// volume whose contents are encrypted (protection on, locked, suspended or
/// still being encrypted or decrypted), `Some(false)` for one that is not,
/// `None` when Windows does not say.
pub fn bitlocker_from_code(code: i32) -> Option<bool> {
    match code {
        1 | 3 | 4 | 5 | 6 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// The key of the shell property that holds a volume's BitLocker state.
/// Windows is asked for it by name, so no number is taken for granted.
fn bitlocker_key() -> Option<PROPERTYKEY> {
    let mut key = PROPERTYKEY::default();
    // SAFETY: a constant NUL-terminated name and a valid out-parameter.
    unsafe {
        PSGetPropertyKeyFromName(
            windows::core::w!("System.Volume.BitLockerProtection"),
            &mut key,
        )
    }
    .ok()?;
    Some(key)
}

/// The encryption state of a volume, read from the same property Explorer
/// uses for its padlock. A read-only query that needs no administrator
/// rights; the calling thread must have COM initialised.
fn bitlocker_state(root: &str, key: &PROPERTYKEY) -> Option<bool> {
    let w = wide(root);
    // SAFETY: NUL-terminated path; the interface is released when dropped.
    unsafe {
        let item: IShellItem2 = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        bitlocker_from_code(item.GetInt32(key).ok()?)
    }
}

struct Tracked {
    index: u32,
    handle: Handle,
    last: Option<Perf>,
}

/// Live read / write rates per physical drive.
#[derive(Default)]
pub struct DiskSampler {
    drives: Vec<Tracked>,
    scanned: Option<Instant>,
}

impl DiskSampler {
    pub fn new() -> DiskSampler {
        DiskSampler::default()
    }

    /// Opens the drives that are not tracked yet.
    fn rescan(&mut self) {
        for index in 0..=MAX_DRIVE_INDEX {
            if self.drives.iter().any(|t| t.index == index) {
                continue;
            }
            if let Ok(handle) = open_device(&drive_path(index), 0) {
                self.drives.push(Tracked {
                    index,
                    handle,
                    last: None,
                });
            }
        }
        self.drives.sort_by_key(|t| t.index);
        self.scanned = Some(Instant::now());
    }

    pub fn sample(&mut self) -> Vec<DiskSample> {
        let due = self
            .scanned
            .map(|at| at.elapsed().as_secs_f64() >= RESCAN_S)
            .unwrap_or(true);
        if due {
            self.rescan();
        }
        let mut out = Vec::with_capacity(self.drives.len());
        let mut gone = Vec::new();
        for t in &mut self.drives {
            let mut buf = [0u8; 128];
            let now = ioctl(&t.handle, IOCTL_DISK_PERFORMANCE, &[], &mut buf)
                .ok()
                .and_then(|n| parse_perf(&buf[..n]));
            match now {
                Some(now) => {
                    out.push(match &t.last {
                        Some(prev) => rates(t.index, prev, &now),
                        None => DiskSample {
                            index: t.index,
                            ..Default::default()
                        },
                    });
                    t.last = Some(now);
                }
                // The drive went away (or does not count): drop the handle
                // and look again at the next rescan.
                None => gone.push(t.index),
            }
        }
        self.drives.retain(|t| !gone.contains(&t.index));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(vendor: &str, product: &str, firmware: &str, serial: &str, bus: u32) -> Vec<u8> {
        let mut buf = vec![0u8; 36];
        buf[10] = 1;
        buf[28..32].copy_from_slice(&bus.to_le_bytes());
        for (field, text) in [
            (12usize, vendor),
            (16, product),
            (20, firmware),
            (24, serial),
        ] {
            if text.is_empty() {
                continue;
            }
            let at = buf.len() as u32;
            buf[field..field + 4].copy_from_slice(&at.to_le_bytes());
            buf.extend_from_slice(text.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn device_descriptor() {
        let buf = descriptor(
            "",
            "Fabrikam  Rocket 2TB ",
            "FW10.2 ",
            "  TESTSN00042  ",
            17,
        );
        let d = parse_device_descriptor(&buf).expect("parses");
        assert_eq!(d.model, "Fabrikam Rocket 2TB");
        assert_eq!(d.firmware, "FW10.2");
        assert_eq!(d.serial, "TESTSN00042");
        assert_eq!(bus_name(d.bus), "NVMe");
        assert!(d.removable);
        let d =
            parse_device_descriptor(&descriptor("Contoso ", "Stick", "", "", 7)).expect("parses");
        assert_eq!(d.model, "Contoso Stick");
        assert_eq!(d.serial, "");
        assert_eq!(bus_name(d.bus), "USB");
        // Corrupt: too short, offsets past the end, no terminator, all 0xFF.
        assert!(parse_device_descriptor(&[]).is_none());
        assert!(parse_device_descriptor(&[0u8; 35]).is_none());
        let mut bad = descriptor("", "Model", "", "", 11);
        bad[16..20].copy_from_slice(&9_999u32.to_le_bytes());
        assert_eq!(parse_device_descriptor(&bad).expect("parses").model, "");
        let mut open = descriptor("", "NoEnd", "", "", 11);
        open.pop();
        assert_eq!(
            parse_device_descriptor(&open).expect("parses").model,
            "NoEnd"
        );
        let d = parse_device_descriptor(&[0xFFu8; 64]).expect("parses");
        assert_eq!(d.model, "");
        assert_eq!(bus_name(d.bus), "");
        assert_eq!(
            parse_device_descriptor(&[0u8; 64]),
            Some(DeviceDescriptor::default())
        );
    }

    #[test]
    fn serial_numbers() {
        assert_eq!(normalize_serial("  AB12CD34  "), "AB12CD34");
        assert_eq!(
            normalize_serial("0011_2233_4455_6677."),
            "0011_2233_4455_6677"
        );
        // "TEST1234" with each pair swapped, written as hexadecimal text.
        let swapped = "ETTS2143";
        let hex: String = swapped.bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(normalize_serial(&hex), "TEST1234");
        // Space padding in front is dropped too.
        let padded: String = "    ETTS2143".bytes().map(|b| format!("{b:02X}")).collect();
        assert_eq!(normalize_serial(&padded), "TEST1234");
        // A real hexadecimal identifier stays as it is.
        assert_eq!(normalize_serial("5000C500A1B2C3D4"), "5000C500A1B2C3D4");
        assert_eq!(normalize_serial(""), "");
        assert_eq!(normalize_serial("séríal"), "séríal");
    }

    fn layout_buffer(style: u32, entries: &[(u32, u64, u64, u32, [u8; 16])]) -> Vec<u8> {
        let mut buf = vec![0u8; 48];
        buf[..4].copy_from_slice(&style.to_le_bytes());
        buf[4..8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        for (entry_style, offset, size, number, kind) in entries {
            let mut e = vec![0u8; 144];
            e[..4].copy_from_slice(&entry_style.to_le_bytes());
            e[8..16].copy_from_slice(&offset.to_le_bytes());
            e[16..24].copy_from_slice(&size.to_le_bytes());
            e[24..28].copy_from_slice(&number.to_le_bytes());
            e[32..48].copy_from_slice(kind);
            buf.extend(e);
        }
        buf
    }

    const EFI: [u8; 16] = [
        0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9,
        0x3B,
    ];
    const BASIC: [u8; 16] = [
        0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99,
        0xC7,
    ];

    #[test]
    fn partition_tables() {
        let buf = layout_buffer(
            1,
            &[
                (1, 105 << 20, 500 << 30, 2, BASIC),
                (1, 1 << 20, 100 << 20, 1, EFI),
                (1, 0, 0, 0, [0; 16]),
                (1, 600 << 30, 1 << 30, 3, [7; 16]),
            ],
        );
        let (style, parts) = parse_layout(&buf);
        assert_eq!(style, "GPT");
        assert_eq!(parts.len(), 3, "the empty entry is skipped");
        assert_eq!(parts[0].kind, "EFI system");
        assert_eq!(parts[0].number, 1);
        assert_eq!(parts[1].kind, "Basic data");
        assert_eq!(parts[1].size, 500 << 30);
        assert_eq!(parts[2].kind, "Other");

        let mut ntfs = [0u8; 16];
        ntfs[0] = 0x07;
        let buf = layout_buffer(0, &[(0, 1 << 20, 80 << 30, 1, ntfs), (0, 0, 5, 0, [0; 16])]);
        let (style, parts) = parse_layout(&buf);
        assert_eq!(style, "MBR");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, "NTFS / exFAT");

        // Corrupt: empty, cut in the header, cut in an entry, absurd count, 0xFF.
        assert_eq!(parse_layout(&[]), (String::new(), Vec::new()));
        assert_eq!(parse_layout(&[1, 0, 0]).1.len(), 0);
        let full = layout_buffer(
            1,
            &[
                (1, 1 << 20, 1 << 30, 1, EFI),
                (1, 2 << 30, 1 << 30, 2, BASIC),
            ],
        );
        for cut in 0..full.len() {
            let _ = parse_layout(&full[..cut]);
        }
        assert_eq!(parse_layout(&full[..48 + 144 + 20]).1.len(), 1);
        let mut absurd = full.clone();
        absurd[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_layout(&absurd).1.len(), 2);
        let (style, parts) = parse_layout(&[0xFFu8; 4096]);
        assert_eq!(style, "");
        assert!(parts.is_empty(), "negative sizes are rejected");
        assert_eq!(parse_layout(&[0u8; 4096]).1.len(), 0);
    }

    #[test]
    fn type_names() {
        assert_eq!(guid_text(&EFI), "C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
        assert_eq!(guid_text(&[1, 2]), "");
        assert_eq!(
            gpt_type_name("DE94BBA4-06D1-4D40-A16A-BFD50179D6AC"),
            "Recovery"
        );
        assert_eq!(mbr_type_name(0x0C), "FAT32");
        assert_eq!(mbr_type_name(0xEE), "GPT protective");
        assert_eq!(bus_name(11), "SATA");
        assert_eq!(bus_name(12), "SD");
        assert_eq!(bus_name(8), "RAID");
        assert_eq!(bus_name(10), "SAS");
        assert_eq!(bus_name(14), "Virtual");
        assert_eq!(bus_name(999), "");
    }

    #[test]
    fn temperature_descriptor() {
        let mut buf = vec![0u8; 24 + 32];
        buf[12] = 2;
        buf[24 + 2..24 + 4].copy_from_slice(&0i16.to_le_bytes());
        buf[40 + 2..40 + 4].copy_from_slice(&37i16.to_le_bytes());
        assert_eq!(parse_temperature(&buf), Some(37.0));
        buf[40 + 2..40 + 4].copy_from_slice(&(-5i16).to_le_bytes());
        assert_eq!(parse_temperature(&buf), None);
        assert_eq!(parse_temperature(&[]), None);
        assert_eq!(parse_temperature(&[0u8; 24]), None);
        assert_eq!(parse_temperature(&[0xFFu8; 600]), None);
        // Count says 2, data holds only one entry.
        let mut short = vec![0u8; 24 + 16];
        short[12] = 2;
        short[26] = 41;
        assert_eq!(parse_temperature(&short), Some(41.0));
    }

    fn perf_buffer(p: &Perf) -> Vec<u8> {
        let mut buf = vec![0u8; 88];
        buf[..8].copy_from_slice(&p.read.to_le_bytes());
        buf[8..16].copy_from_slice(&p.written.to_le_bytes());
        buf[32..40].copy_from_slice(&p.idle.to_le_bytes());
        buf[48..52].copy_from_slice(&p.queue.to_le_bytes());
        buf[56..64].copy_from_slice(&p.query.to_le_bytes());
        buf
    }

    #[test]
    fn activity_rates() {
        let a = Perf {
            read: 1_000,
            written: 5_000,
            idle: 10_000_000,
            query: 20_000_000,
            queue: 0,
        };
        let b = Perf {
            read: 1_000 + 4_000_000,
            written: 5_000 + 1_000_000,
            idle: 10_000_000 + 15_000_000,
            query: 20_000_000 + 20_000_000,
            queue: 3,
        };
        assert_eq!(parse_perf(&perf_buffer(&b)), Some(b));
        let s = rates(4, &a, &b);
        assert_eq!(s.index, 4);
        assert_eq!(s.read_bps, 2_000_000.0);
        assert_eq!(s.write_bps, 500_000.0);
        assert!((s.active_pct - 25.0).abs() < 0.01);
        assert_eq!(s.queue_depth, Some(3.0));
        // No time passed, counters running backwards, idle not reported.
        let s = rates(0, &b, &b);
        assert_eq!((s.read_bps, s.write_bps, s.active_pct), (0.0, 0.0, 0.0));
        let s = rates(0, &b, &a);
        assert_eq!((s.read_bps, s.write_bps, s.active_pct), (0.0, 0.0, 0.0));
        let no_idle = Perf { idle: 0, ..b };
        assert_eq!(rates(0, &Perf { idle: 0, ..a }, &no_idle).active_pct, 0.0);
        // Idle time larger than the interval must not go negative.
        let odd = Perf {
            idle: a.idle + 90_000_000,
            ..b
        };
        assert_eq!(rates(0, &a, &odd).active_pct, 0.0);
        // Corrupt buffers.
        assert_eq!(parse_perf(&[]), None);
        assert_eq!(parse_perf(&[0u8; 63]), None);
        assert_eq!(parse_perf(&[0xFFu8; 88]), None);
        assert_eq!(parse_perf(&[0u8; 88]), Some(Perf::default()));
    }

    #[test]
    fn live_drives_and_volumes() {
        let list = drives();
        println!("drives: {}", list.len());
        for d in &list {
            println!(
                "  drive: interface={} media={:?} model known={} size>0={} style={} partitions={} lettered={} health={} source={:?} note={:?} attributes={} temperature={} life={}",
                d.interface,
                d.media,
                !d.model.is_empty(),
                d.size_bytes > 0,
                d.partition_style,
                d.partitions.len(),
                d.partitions.iter().filter(|p| !p.letter.is_empty()).count(),
                d.health.label(),
                d.smart_source,
                d.smart_note,
                d.attributes.len(),
                d.temperature_c.is_some(),
                d.health_pct.is_some(),
            );
            assert!(d.health_pct.unwrap_or(0) <= 100);
            if let Some(t) = d.temperature_c {
                assert!((0.0..=125.0).contains(&t));
            }
        }
        assert!(list.iter().any(|d| !d.model.is_empty() && d.size_bytes > 0));
        let vols = volumes();
        println!(
            "volumes: {} ({} mapped to a drive)",
            vols.len(),
            vols.iter().filter(|v| v.drive_index.is_some()).count()
        );
        assert!(!vols.is_empty());
        for v in &vols {
            assert!(v.free <= v.total);
            assert!((0.0..=100.0).contains(&v.used_pct()));
        }
        // Windows knows the property by name, and says for the system volume
        // whether it is encrypted (as a standard user, too).
        assert!(bitlocker_key().is_some());
        println!(
            "volumes: encryption state known for {}",
            vols.iter().filter(|v| v.bitlocker.is_some()).count()
        );
        assert!(vols
            .iter()
            .any(|v| v.kind == "Fixed" && v.bitlocker.is_some()));
        // Reading it twice leaves COM balanced and gives the same answer.
        let again = volumes();
        assert_eq!(
            vols.iter().map(|v| v.bitlocker).collect::<Vec<_>>(),
            again.iter().map(|v| v.bitlocker).collect::<Vec<_>>()
        );
    }

    #[test]
    fn bitlocker_codes() {
        // On, being encrypted, being decrypted, suspended, locked.
        for code in [1, 3, 4, 5, 6] {
            assert_eq!(bitlocker_from_code(code), Some(true), "{code}");
        }
        assert_eq!(bitlocker_from_code(2), Some(false));
        for code in [0, 7, -1, 100] {
            assert_eq!(bitlocker_from_code(code), None, "{code}");
        }
    }

    #[test]
    fn live_disk_sampler() {
        let mut sampler = DiskSampler::new();
        let first = sampler.sample();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let started = Instant::now();
        let second = sampler.sample();
        println!(
            "disk sampler: {} drives, second reading took {} µs, any activity: {}",
            second.len(),
            started.elapsed().as_micros(),
            second.iter().any(|d| d.read_bps > 0.0 || d.write_bps > 0.0)
        );
        assert_eq!(first.len(), second.len());
        for d in &second {
            assert!((0.0..=100.0).contains(&d.active_pct));
            assert!(d.read_bps >= 0.0 && d.write_bps >= 0.0);
        }
    }
}
