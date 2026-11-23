//! Running processes and the one action in the program that changes
//! anything: ending a task.
//!
//! The list comes from a single system call per reading (the same snapshot
//! the system's own task list uses). Two columns need an explanation:
//!
//! * `disk_read_bps` / `disk_write_bps` are the process's *total* I/O rates
//!   (files, pipes, sockets and devices together). Windows only splits disk
//!   I/O per process through an event-tracing session, which this program
//!   does not start; total I/O is the closest reading available without it.
//! * `net_bps` stays `None`: network traffic per process cannot be measured
//!   without a kernel driver or an event-tracing session.
//!
//! The "System Idle Process" entry (process id 0) is left out: it is the
//! absence of load, not a program.

use super::gpu::GpuProcess;
use super::memory::{publish_process_facts, SystemCounts, COMPRESSION_STORE};
use super::pdh::{le_u16, le_u32, le_u64};
use crate::model::ProcessSample;
use std::collections::HashMap;
use std::time::Instant;
use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS};
use windows::Win32::Foundation::{CloseHandle, BOOL, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetCurrentProcessId, IsProcessCritical, OpenProcess, TerminateProcess,
    ALL_PROCESSOR_GROUPS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

// ----------------------------------------------------------------------------------
// System information queries shared with the other collectors
// ----------------------------------------------------------------------------------

const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023_u32 as i32;
const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005_u32 as i32;

/// `SystemProcessInformation`.
const CLASS_PROCESSES: i32 = 5;
/// The process list never needs more than this; a larger request is refused.
const MAX_SNAPSHOT_BYTES: usize = 256 << 20;

/// One system-information query into `buf`. Returns the number of bytes
/// written, or the status code and the size the system asked for. The buffer
/// is made of `u64` so that it has the alignment the kernel insists on.
pub(crate) fn nt_query(class: i32, buf: &mut [u64]) -> Result<usize, (i32, usize)> {
    let len = (buf.len() * 8).min(u32::MAX as usize) as u32;
    let mut returned = 0u32;
    // SAFETY: `buf` is writable for `len` bytes and 8-byte aligned; `returned`
    // is a valid out-pointer.
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_INFORMATION_CLASS(class),
            buf.as_mut_ptr().cast(),
            len,
            &mut returned,
        )
    };
    if status.0 >= 0 {
        let written = if returned == 0 {
            len
        } else {
            returned.min(len)
        };
        Ok(written as usize)
    } else {
        Err((status.0, returned as usize))
    }
}

/// Like [`nt_query`], growing `buf` until the answer fits (up to `max_bytes`).
pub(crate) fn nt_query_grow(class: i32, buf: &mut Vec<u64>, max_bytes: usize) -> Option<usize> {
    if buf.is_empty() {
        buf.resize(64, 0);
    }
    for _ in 0..8 {
        match nt_query(class, buf) {
            Ok(written) => return Some(written),
            Err((status, wanted))
                if status == STATUS_INFO_LENGTH_MISMATCH
                    || status == STATUS_BUFFER_TOO_SMALL
                    || status == STATUS_BUFFER_OVERFLOW =>
            {
                let current = buf.len() * 8;
                // Room to spare: the list can grow between the two calls.
                let target = wanted.max(current * 2) + wanted / 8 + 4096;
                if target > max_bytes {
                    return None;
                }
                buf.resize(target / 8 + 1, 0);
            }
            Err(_) => return None,
        }
    }
    None
}

/// The first `len` bytes of a `u64` buffer.
pub(crate) fn as_bytes(buf: &[u64], len: usize) -> &[u8] {
    let len = len.min(buf.len() * 8);
    // SAFETY: any initialised `u64` slice is valid as bytes; `len` is clamped to its size.
    unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len) }
}

// ----------------------------------------------------------------------------------
// Parsing the process snapshot
// ----------------------------------------------------------------------------------

/// Size of the fixed part of one process entry on 64-bit Windows.
const ENTRY_SIZE: usize = 256;

/// The fields this program reads from one entry of the process snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RawProcess {
    pub pid: u32,
    pub parent: u32,
    pub threads: u32,
    pub handles: u32,
    pub private_working_set: u64,
    pub working_set: u64,
    /// 100 ns units since 1601 (UTC).
    pub create_time: u64,
    /// Kernel plus user time in 100 ns units.
    pub cpu_time: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Walks the entries of a process snapshot. `base` is the address the buffer
/// had when the system filled it (image names are stored as absolute
/// pointers into the same buffer). `each` receives the entry and the image
/// name as UTF-16LE bytes (empty when the name is missing or its pointer does
/// not lie inside the buffer). Truncated or corrupt input ends the walk.
pub(crate) fn for_each_process(
    bytes: &[u8],
    base: usize,
    mut each: impl FnMut(&RawProcess, &[u8]),
) {
    let mut offset = 0usize;
    while let Some(end) = offset.checked_add(ENTRY_SIZE) {
        if end > bytes.len() {
            break;
        }
        let entry = &bytes[offset..end];
        let u32_at = |o: usize| le_u32(entry, o).unwrap_or(0);
        let u64_at = |o: usize| le_u64(entry, o).unwrap_or(0);
        let raw = RawProcess {
            pid: u64_at(80) as u32,
            parent: u64_at(88) as u32,
            threads: u32_at(4),
            handles: u32_at(96),
            private_working_set: u64_at(8),
            working_set: u64_at(144),
            create_time: u64_at(32),
            cpu_time: u64_at(40).saturating_add(u64_at(48)),
            read_bytes: u64_at(232),
            write_bytes: u64_at(240),
        };
        let name_len = usize::from(le_u16(entry, 56).unwrap_or(0)) & !1;
        let name_ptr = u64_at(64) as usize;
        let name = name_ptr
            .checked_sub(base)
            .and_then(|start| Some(start..start.checked_add(name_len)?))
            .and_then(|range| bytes.get(range))
            .unwrap_or(&[]);
        each(&raw, name);

        let next = u32_at(0) as usize;
        if next == 0 || next < ENTRY_SIZE {
            break;
        }
        match offset.checked_add(next) {
            Some(n) => offset = n,
            None => break,
        }
    }
}

/// UTF-16LE bytes to a string.
pub(crate) fn utf16le(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or('\u{FFFD}'))
        .collect()
}

/// True when the UTF-16LE `name` equals the ASCII text `ascii`, ignoring case.
pub(crate) fn utf16le_eq_ascii(name: &[u8], ascii: &str) -> bool {
    name.len() == ascii.len() * 2
        && name
            .as_chunks::<2>()
            .0
            .iter()
            .zip(ascii.bytes())
            .all(|(pair, b)| pair[1] == 0 && pair[0].eq_ignore_ascii_case(&b))
}

/// Share of the whole machine a process used between two readings.
pub(crate) fn cpu_percent(prev_cpu: u64, now_cpu: u64, seconds: f64, processors: u32) -> f32 {
    if now_cpu < prev_cpu || !seconds.is_finite() || seconds <= 0.0 || processors == 0 {
        return 0.0;
    }
    let used_s = (now_cpu - prev_cpu) as f64 / 1e7;
    (used_s / (seconds * f64::from(processors)) * 100.0).clamp(0.0, 100.0) as f32
}

/// The name shown for an entry: the image name, or the well-known names of
/// the two entries that have none.
fn display_name(pid: u32, name: &[u8]) -> String {
    if !name.is_empty() {
        utf16le(name)
    } else if pid == 0 {
        "System Idle Process".to_string()
    } else if pid == 4 {
        "System".to_string()
    } else {
        String::new()
    }
}

fn filetime_now() -> u64 {
    // SAFETY: no arguments, cannot fail.
    let ft = unsafe { GetSystemTimeAsFileTime() };
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

fn processor_count() -> u32 {
    // SAFETY: no pointers involved.
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(1)
}

// ----------------------------------------------------------------------------------
// The sampler
// ----------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Previous {
    create_time: u64,
    cpu_time: u64,
    read_bytes: u64,
    write_bytes: u64,
}

#[derive(Default)]
pub struct ProcessSampler {
    buf: Vec<u64>,
    previous: HashMap<u32, Previous>,
    scratch: HashMap<u32, Previous>,
    last: Option<Instant>,
}

impl ProcessSampler {
    pub fn new() -> ProcessSampler {
        ProcessSampler::default()
    }

    /// All processes with their load since the previous call. `gpu` is the
    /// per-process GPU use from the GPU sampler's latest reading; when it is
    /// empty the GPU columns stay `None`.
    ///
    /// The first call (and the first reading of a new process) reports zero
    /// load, because a rate needs two readings. `disk_*_bps` cover all I/O of
    /// the process and `net_bps` is always `None`; see the module comment.
    pub fn sample(&mut self, gpu: &HashMap<u32, GpuProcess>) -> Vec<ProcessSample> {
        let now = Instant::now();
        let seconds = self
            .last
            .map(|at| now.duration_since(at).as_secs_f64())
            .unwrap_or(0.0);
        let Some(written) = nt_query_grow(CLASS_PROCESSES, &mut self.buf, MAX_SNAPSHOT_BYTES)
        else {
            return Vec::new();
        };
        self.last = Some(now);
        let processors = processor_count();
        let wall = filetime_now();
        let base = self.buf.as_ptr() as usize;
        let bytes = as_bytes(&self.buf, written);
        let previous = &self.previous;
        let scratch = &mut self.scratch;
        scratch.clear();
        let mut out = Vec::with_capacity(previous.len().max(64));
        let mut counts = SystemCounts::default();
        let mut compression_store = None;
        for_each_process(bytes, base, |raw, name| {
            counts.processes += 1;
            counts.threads = counts.threads.saturating_add(raw.threads);
            counts.handles = counts.handles.saturating_add(raw.handles);
            if compression_store.is_none() && utf16le_eq_ascii(name, COMPRESSION_STORE) {
                compression_store = Some(raw.working_set);
            }
            if raw.pid == 0 {
                return;
            }
            let before = previous
                .get(&raw.pid)
                .filter(|p| p.create_time == raw.create_time);
            let (cpu_pct, read_bps, write_bps) = match before {
                Some(p) => (
                    cpu_percent(p.cpu_time, raw.cpu_time, seconds, processors),
                    super::pdh::rate(p.read_bytes, raw.read_bytes, seconds),
                    super::pdh::rate(p.write_bytes, raw.write_bytes, seconds),
                ),
                None => (0.0, 0.0, 0.0),
            };
            scratch.insert(
                raw.pid,
                Previous {
                    create_time: raw.create_time,
                    cpu_time: raw.cpu_time,
                    read_bytes: raw.read_bytes,
                    write_bytes: raw.write_bytes,
                },
            );
            let used = gpu.get(&raw.pid);
            let running_s = if raw.create_time == 0 {
                0
            } else {
                wall.saturating_sub(raw.create_time) / 10_000_000
            };
            out.push(ProcessSample {
                pid: raw.pid,
                parent_pid: raw.parent,
                name: display_name(raw.pid, name),
                cpu_pct,
                memory: raw.private_working_set,
                disk_read_bps: read_bps,
                disk_write_bps: write_bps,
                net_bps: None,
                gpu_pct: (!gpu.is_empty()).then(|| used.map_or(0.0, |g| g.pct)),
                gpu_memory: (!gpu.is_empty()).then(|| used.map_or(0, |g| g.memory)),
                threads: raw.threads,
                handles: raw.handles,
                running_s,
                user: String::new(),
            });
        });
        std::mem::swap(&mut self.previous, &mut self.scratch);
        // The other collectors need these figures too; hand them over so
        // they do not have to walk the process list themselves.
        publish_process_facts(counts, compression_store);
        out
    }
}

/// Working set of the first process with this image name (ASCII, case
/// ignored), from a fresh snapshot into `buf`. Used for the memory
/// compression store, which Windows presents as a process.
pub(crate) fn working_set_of(image: &str, buf: &mut Vec<u64>) -> Option<u64> {
    let written = nt_query_grow(CLASS_PROCESSES, buf, MAX_SNAPSHOT_BYTES)?;
    let base = buf.as_ptr() as usize;
    let mut found = None;
    for_each_process(as_bytes(buf, written), base, |raw, name| {
        if found.is_none() && utf16le_eq_ascii(name, image) {
            found = Some(raw.working_set);
        }
    });
    found
}

/// The image name of a running process, `None` when there is no such process.
fn name_of(pid: u32) -> Option<String> {
    let mut buf = Vec::new();
    let written = nt_query_grow(CLASS_PROCESSES, &mut buf, MAX_SNAPSHOT_BYTES)?;
    let base = buf.as_ptr() as usize;
    let mut found = None;
    for_each_process(as_bytes(&buf, written), base, |raw, name| {
        if raw.pid == pid && found.is_none() {
            found = Some(display_name(raw.pid, name));
        }
    });
    found
}

// ----------------------------------------------------------------------------------
// Ending a task
// ----------------------------------------------------------------------------------

/// Processes Windows cannot run without. Ending one of them stops the
/// session or the whole system, so the program never tries.
const CRITICAL_NAMES: [&str; 14] = [
    "system",
    "system idle process",
    "registry",
    "memory compression",
    "secure system",
    "smss",
    "csrss",
    "wininit",
    "winlogon",
    "services",
    "lsass",
    "lsaiso",
    "fontdrvhost",
    "dwm",
];

/// True when `name` (with or without ".exe", any case) is on the list of
/// processes Windows cannot run without.
pub fn is_critical_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    CRITICAL_NAMES.contains(&stem)
}

/// Why a process must not be ended, or `None` when ending it may be tried.
/// `name` is the image name if known, `own_pid` the id of this program.
pub fn refusal_reason(pid: u32, name: &str, own_pid: u32) -> Option<String> {
    if pid == 0 || pid == 4 {
        return Some("This is the Windows kernel itself. It cannot be ended.".to_string());
    }
    if pid == own_pid {
        return Some(
            "This is PulseForge itself. Close the window or use Exit instead.".to_string(),
        );
    }
    if is_critical_name(name) {
        return Some(format!(
            "{} is a part of Windows that the system needs to keep running. Ending it would \
             crash or lock this PC, so it is not ended.",
            name.trim()
        ));
    }
    None
}

/// Closes a process handle when dropped.
struct ProcessHandle(windows::Win32::Foundation::HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Ends the process with this id. Refuses system-critical processes and this
/// program itself.
pub fn end_task(pid: u32) -> Result<(), String> {
    // SAFETY: no arguments, cannot fail.
    let own_pid = unsafe { GetCurrentProcessId() };
    if let Some(reason) = refusal_reason(pid, "", own_pid) {
        return Err(reason);
    }
    let Some(name) = name_of(pid) else {
        return Err(format!(
            "There is no process with the id {pid} any more. It has probably ended already."
        ));
    };
    if let Some(reason) = refusal_reason(pid, &name, own_pid) {
        return Err(reason);
    }

    // Windows marks some processes as critical regardless of their name.
    // SAFETY: plain call; the returned handle is owned by the guard.
    if let Ok(handle) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        let handle = ProcessHandle(handle);
        let mut critical = BOOL(0);
        // SAFETY: the handle is open with the access this call needs; `critical`
        // is a valid out-pointer.
        if unsafe { IsProcessCritical(handle.0, &mut critical) }.is_ok() && critical.as_bool() {
            return Err(format!(
                "{name} is marked by Windows as critical. Ending it would crash this PC, so it \
                 is not ended."
            ));
        }
    }

    // SAFETY: plain call; the returned handle is owned by the guard.
    let handle = match unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
        Ok(handle) => ProcessHandle(handle),
        Err(error) => return Err(open_error_text(&name, pid, error.code().0 as u32)),
    };
    // SAFETY: the handle is open with PROCESS_TERMINATE.
    match unsafe { TerminateProcess(handle.0, 1) } {
        Ok(()) => Ok(()),
        Err(error) => Err(open_error_text(&name, pid, error.code().0 as u32)),
    }
}

/// Plain words for a failed attempt to open or end a process. `hresult` is
/// the error as an HRESULT (Win32 errors are wrapped as 0x8007xxxx).
fn open_error_text(name: &str, pid: u32, hresult: u32) -> String {
    let win32 = if hresult & 0xFFFF_0000 == 0x8007_0000 {
        hresult & 0xFFFF
    } else {
        hresult
    };
    if win32 == ERROR_ACCESS_DENIED.0 {
        format!(
            "Access denied: {name} belongs to Windows or to another user. Start PulseForge as \
             administrator to end it."
        )
    } else if win32 == ERROR_INVALID_PARAMETER.0 {
        format!("There is no process with the id {pid} any more. It has probably ended already.")
    } else {
        format!("{name} could not be ended (Windows error {win32}).")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one snapshot entry at `offset` of `buf`; the name is stored
    /// right after the fixed part, the way the system lays it out.
    #[allow(clippy::too_many_arguments)]
    fn put_entry(
        buf: &mut Vec<u8>,
        base: usize,
        pid: u32,
        parent: u32,
        name: &str,
        cpu: (u64, u64),
        io: (u64, u64),
        last: bool,
    ) {
        let start = buf.len();
        let name_bytes: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let total = (ENTRY_SIZE + name_bytes.len() + 2 + 7) & !7;
        buf.resize(start + total, 0);
        let e = &mut buf[start..start + total];
        let next = if last { 0u32 } else { total as u32 };
        e[0..4].copy_from_slice(&next.to_le_bytes());
        e[4..8].copy_from_slice(&7u32.to_le_bytes());
        e[8..16].copy_from_slice(&(5u64 << 20).to_le_bytes());
        e[32..40].copy_from_slice(&1_000_000u64.to_le_bytes());
        e[40..48].copy_from_slice(&cpu.0.to_le_bytes());
        e[48..56].copy_from_slice(&cpu.1.to_le_bytes());
        e[56..58].copy_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        let ptr = if name.is_empty() {
            0u64
        } else {
            (base + start + ENTRY_SIZE) as u64
        };
        e[64..72].copy_from_slice(&ptr.to_le_bytes());
        e[80..88].copy_from_slice(&u64::from(pid).to_le_bytes());
        e[88..96].copy_from_slice(&u64::from(parent).to_le_bytes());
        e[96..100].copy_from_slice(&33u32.to_le_bytes());
        e[144..152].copy_from_slice(&(9u64 << 20).to_le_bytes());
        e[232..240].copy_from_slice(&io.0.to_le_bytes());
        e[240..248].copy_from_slice(&io.1.to_le_bytes());
        e[ENTRY_SIZE..ENTRY_SIZE + name_bytes.len()].copy_from_slice(&name_bytes);
    }

    fn collect(bytes: &[u8], base: usize) -> Vec<(RawProcess, String)> {
        let mut out = Vec::new();
        for_each_process(bytes, base, |raw, name| out.push((*raw, utf16le(name))));
        out
    }

    #[test]
    fn parses_a_synthetic_snapshot() {
        let base = 0x10_0000;
        let mut buf = Vec::new();
        put_entry(&mut buf, base, 0, 0, "", (0, 0), (0, 0), false);
        put_entry(&mut buf, base, 4, 0, "System", (10, 20), (1, 2), false);
        put_entry(
            &mut buf,
            base,
            4321,
            4,
            "example.exe",
            (300, 700),
            (5000, 9000),
            true,
        );
        let list = collect(&buf, base);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].0.pid, 0);
        assert_eq!(list[0].1, "");
        assert_eq!(list[1].1, "System");
        let (raw, name) = &list[2];
        assert_eq!(name, "example.exe");
        assert_eq!(raw.pid, 4321);
        assert_eq!(raw.parent, 4);
        assert_eq!(raw.threads, 7);
        assert_eq!(raw.handles, 33);
        assert_eq!(raw.cpu_time, 1000);
        assert_eq!(raw.private_working_set, 5 << 20);
        assert_eq!(raw.working_set, 9 << 20);
        assert_eq!(raw.read_bytes, 5000);
        assert_eq!(raw.write_bytes, 9000);
        assert_eq!(raw.create_time, 1_000_000);
    }

    #[test]
    fn corrupt_snapshots_do_not_panic() {
        let base = 0x10_0000;
        let mut buf = Vec::new();
        put_entry(&mut buf, base, 8, 4, "one.exe", (1, 1), (1, 1), false);
        put_entry(&mut buf, base, 12, 4, "two.exe", (1, 1), (1, 1), true);

        // Truncated at every length.
        for len in 0..buf.len() {
            let _ = collect(&buf[..len], base);
        }
        assert_eq!(collect(&buf[..ENTRY_SIZE - 1], base).len(), 0);

        // Wrong base: names fall outside and come back empty, entries survive.
        let moved = collect(&buf, base + 0x1000_0000);
        assert_eq!(moved.len(), 2);
        assert!(moved.iter().all(|(_, name)| name.is_empty()));
        let low = collect(&buf, 0);
        assert!(low.iter().all(|(_, name)| name.is_empty()));

        // A next-offset that points nowhere, backwards or into itself.
        for next in [1u32, 8, 255, u32::MAX, 0x7FFF_FFFF] {
            let mut bad = buf.clone();
            bad[0..4].copy_from_slice(&next.to_le_bytes());
            assert_eq!(collect(&bad, base).len(), 1, "next = {next}");
        }

        // A name length that runs past the end.
        let mut bad = buf.clone();
        bad[56..58].copy_from_slice(&0xFFFEu16.to_le_bytes());
        let list = collect(&bad, base);
        assert_eq!(list[0].1, "");

        // All zeros and all ones.
        assert_eq!(collect(&vec![0u8; 4096], base).len(), 1);
        let ones = collect(&vec![0xFFu8; 4096], base);
        assert_eq!(ones.len(), 1);
        assert_eq!(ones[0].1, "");
        assert_eq!(collect(&[], base).len(), 0);
    }

    #[test]
    fn cpu_share_maths() {
        // One full second of CPU time in one second on 4 processors: 25 %.
        assert_eq!(cpu_percent(0, 10_000_000, 1.0, 4), 25.0);
        assert_eq!(cpu_percent(0, 80_000_000, 1.0, 4), 100.0, "clamped");
        assert_eq!(cpu_percent(500, 100, 1.0, 4), 0.0, "time went backwards");
        assert_eq!(cpu_percent(0, 100, 0.0, 4), 0.0);
        assert_eq!(cpu_percent(0, 100, f64::NAN, 4), 0.0);
        assert_eq!(cpu_percent(0, 100, 1.0, 0), 0.0);
    }

    #[test]
    fn utf16_helpers() {
        let name: Vec<u8> = "Memory Compression"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(utf16le_eq_ascii(&name, "memory compression"));
        assert!(!utf16le_eq_ascii(&name, "memory compressio"));
        assert!(!utf16le_eq_ascii(&name[..5], "me"));
        assert!(!utf16le_eq_ascii(&[], "x"));
        assert_eq!(utf16le(&name), "Memory Compression");
        assert_eq!(
            utf16le(&[0x41, 0x00, 0x42]),
            "A",
            "odd trailing byte ignored"
        );
        assert_eq!(
            utf16le(&[0x00, 0xD8]),
            "\u{FFFD}",
            "lone surrogate replaced"
        );
    }

    #[test]
    fn critical_processes_are_refused() {
        for name in [
            "csrss.exe",
            "CSRSS.EXE",
            "wininit.exe",
            "winlogon.exe",
            "smss.exe",
            "services.exe",
            "lsass.exe",
            "System",
            "Registry",
            "Memory Compression",
            " lsass.exe ",
        ] {
            assert!(is_critical_name(name), "{name}");
            let reason = refusal_reason(999, name, 1).expect("must refuse");
            assert!(reason.contains("Windows"), "{reason}");
        }
        for name in [
            "notepad.exe",
            "example.exe",
            "",
            "service.exe",
            "csrss2.exe",
        ] {
            assert!(!is_critical_name(name), "{name}");
            assert!(refusal_reason(999, name, 1).is_none());
        }
        assert!(refusal_reason(0, "anything.exe", 1).is_some());
        assert!(refusal_reason(4, "anything.exe", 1).is_some());
        let own = refusal_reason(77, "example.exe", 77).expect("own process");
        assert!(own.contains("PulseForge"));
    }

    #[test]
    fn error_texts_are_plain() {
        assert!(open_error_text("example.exe", 5, 0x8007_0005).starts_with("Access denied"));
        assert!(open_error_text("example.exe", 5, 5).starts_with("Access denied"));
        assert!(open_error_text("example.exe", 5, 0x8007_0057).contains("no process"));
        assert!(open_error_text("example.exe", 5, 0x8007_0006).contains("Windows error 6"));
    }

    #[test]
    fn end_task_refuses_protected_targets() {
        assert!(end_task(0).is_err());
        assert!(end_task(4).is_err());
        assert!(end_task(std::process::id()).is_err());
        // An id that cannot exist (process ids are multiples of four).
        let gone = end_task(0xFFFF_FFF3).expect_err("no such process");
        assert!(gone.contains("no process"), "{gone}");

        // A real critical process, found by name: must be refused before any
        // attempt to open it.
        let mut buf = Vec::new();
        let written = nt_query_grow(CLASS_PROCESSES, &mut buf, MAX_SNAPSHOT_BYTES).unwrap_or(0);
        let base = buf.as_ptr() as usize;
        let mut critical_pid = None;
        for_each_process(as_bytes(&buf, written), base, |raw, name| {
            if utf16le_eq_ascii(name, "csrss.exe") {
                critical_pid = Some(raw.pid);
            }
        });
        if let Some(pid) = critical_pid {
            let reason = end_task(pid).expect_err("critical process must be refused");
            assert!(reason.contains("needs to keep running"), "{reason}");
        }
        println!(
            "process: critical process found: {}",
            critical_pid.is_some()
        );
    }

    #[test]
    fn end_task_ends_a_child_started_by_the_test() {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let ping = format!(r"{system_root}\System32\ping.exe");
        let spawned = Command::new(ping)
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        let Ok(mut child) = spawned else {
            println!("process: could not start the helper child, skipping");
            return;
        };
        let result = end_task(child.id());
        let mut ended = false;
        for _ in 0..100 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                ended = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !ended {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(result.is_ok(), "end_task failed: {result:?}");
        assert!(ended, "the child was still running after end_task");
        println!("process: child ended: yes");
    }

    #[test]
    fn live_smoke() {
        let mut sampler = ProcessSampler::new();
        let gpu = HashMap::new();
        let first = sampler.sample(&gpu);
        assert!(!first.is_empty());
        assert!(first.iter().all(|p| p.cpu_pct == 0.0));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let started = Instant::now();
        let mut list = Vec::new();
        for _ in 0..20 {
            list = sampler.sample(&gpu);
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        assert!(list.len() > 5);
        assert!(list.iter().all(|p| p.pid != 0));
        assert!(list.iter().all(|p| (0.0..=100.0).contains(&p.cpu_pct)));
        assert!(list
            .iter()
            .all(|p| p.disk_read_bps >= 0.0 && p.disk_write_bps >= 0.0));
        assert!(list
            .iter()
            .all(|p| p.net_bps.is_none() && p.gpu_pct.is_none()));
        let me = list.iter().find(|p| p.pid == std::process::id());
        assert!(me.is_some_and(|p| p.threads > 0 && p.memory > 0 && !p.name.is_empty()));
        let named = list.iter().filter(|p| !p.name.is_empty()).count();
        // The walk also refreshed the shared counts (the idle entry included).
        let counts = super::super::memory::system_counts().expect("counts were published");
        assert!(counts.processes as usize > list.len() / 2);
        assert!(counts.threads >= counts.processes && counts.handles > 0);
        // The shares are judged over a real interval: over the fraction of a
        // millisecond between the back-to-back calls above, a busy computer
        // makes them meaningless.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let settled = sampler.sample(&gpu);
        let total: f32 = settled.iter().map(|p| p.cpu_pct).sum();
        assert!(total <= 101.0 * 1.5, "sum of shares is {total}");
        println!(
            "process: {} entries, {named} named, avg {ms:.3} ms per call",
            list.len()
        );

        // With a GPU map the columns are filled for every process.
        let mut gpu = HashMap::new();
        gpu.insert(
            std::process::id(),
            GpuProcess {
                pct: 12.5,
                memory: 1 << 20,
            },
        );
        let with_gpu = sampler.sample(&gpu);
        let me = with_gpu.iter().find(|p| p.pid == std::process::id());
        assert_eq!(me.and_then(|p| p.gpu_pct), Some(12.5));
        assert!(with_gpu.iter().all(|p| p.gpu_pct.is_some()));
    }
}
