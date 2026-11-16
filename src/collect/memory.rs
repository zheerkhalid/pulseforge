//! Memory: live usage breakdown.
//!
//! Sources, all user mode: the global memory status, the kernel's performance
//! block, the page-file list and the kernel's page lists. The page lists
//! (standby, modified, free) are read directly where Windows allows it and
//! through the `\Memory` performance counters otherwise.
//!
//! Two figures need a walk over the whole process list, which is far too
//! expensive for every reading: the process / thread / handle counts and the
//! size of the memory compression store (Windows presents the store as a
//! process). Both are cached here. When the process list is being read anyway
//! (the process page is open) it refreshes them for free; otherwise the
//! counts are re-read every few seconds and the store once a minute.

use super::pdh::{self, le_u32, le_u64};
use super::process::{as_bytes, nt_query, nt_query_grow, working_set_of};
use crate::model::MemorySample;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::SystemInformation::{
    GetPhysicallyInstalledSystemMemory, GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX,
    SYSTEM_INFO,
};

/// `SystemPerformanceInformation`.
const CLASS_PERFORMANCE: i32 = 2;
/// `SystemPageFileInformation`.
const CLASS_PAGE_FILES: i32 = 18;
/// `SystemFileCacheInformation`.
const CLASS_FILE_CACHE: i32 = 21;
/// `SystemMemoryListInformation`.
const CLASS_MEMORY_LISTS: i32 = 80;

/// How long the process / thread / handle counts are reused.
const COUNTS_MAX_AGE: Duration = Duration::from_secs(5);
/// How long the size of the compression store is reused.
const COMPRESSED_MAX_AGE: Duration = Duration::from_secs(60);
/// Image name of the memory compression store.
pub(crate) const COMPRESSION_STORE: &str = "Memory Compression";

// ----------------------------------------------------------------------------------
// Figures shared between collectors
// ----------------------------------------------------------------------------------

/// How many processes, threads and handles exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemCounts {
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
}

static COUNTS: Mutex<Option<(Instant, SystemCounts)>> = Mutex::new(None);
static COMPRESSED: Mutex<Option<(Instant, Option<u64>)>> = Mutex::new(None);

/// Process, thread and handle counts, at most a few seconds old. Reading
/// them makes Windows walk the process list, so the result is cached.
pub fn system_counts() -> Option<SystemCounts> {
    let mut cache = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, counts)) = *cache {
        if at.elapsed() < COUNTS_MAX_AGE {
            return Some(counts);
        }
    }
    let mut raw = PERFORMANCE_INFORMATION::default();
    let size = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
    raw.cb = size;
    // SAFETY: `raw` is a valid out-structure of the size passed.
    unsafe { GetPerformanceInfo(&mut raw, size) }.ok()?;
    let counts = SystemCounts {
        processes: raw.ProcessCount,
        threads: raw.ThreadCount,
        handles: raw.HandleCount,
    };
    *cache = Some((Instant::now(), counts));
    Some(counts)
}

/// Called by the process collector after each walk over the process list:
/// stores the figures it has at hand so nobody else needs to walk the list.
pub(crate) fn publish_process_facts(counts: SystemCounts, compression_store: Option<u64>) {
    let now = Instant::now();
    *COUNTS.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, counts));
    *COMPRESSED.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, compression_store));
}

/// Size of the memory compression store, at most a minute old.
fn compressed_memory(buf: &mut Vec<u64>) -> Option<u64> {
    let mut cache = COMPRESSED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, value)) = *cache {
        if at.elapsed() < COMPRESSED_MAX_AGE {
            return value;
        }
    }
    let value = working_set_of(COMPRESSION_STORE, buf);
    *cache = Some((Instant::now(), value));
    value
}

/// The fields of the kernel's performance block that the collectors use.
/// The three running totals wrap at 2^32; the sizes are in pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelCounters {
    pub page_faults: u32,
    pub context_switches: u32,
    pub system_calls: u32,
    pub available_pages: u32,
    pub committed_pages: u32,
    pub commit_limit_pages: u32,
    pub paged_pool_pages: u32,
    pub nonpaged_pool_pages: u32,
}

/// Decodes a `SystemPerformanceInformation` block.
pub fn parse_kernel_counters(bytes: &[u8]) -> Option<KernelCounters> {
    Some(KernelCounters {
        available_pages: le_u32(bytes, 44)?,
        committed_pages: le_u32(bytes, 48)?,
        commit_limit_pages: le_u32(bytes, 52)?,
        page_faults: le_u32(bytes, 60)?,
        paged_pool_pages: le_u32(bytes, 112)?,
        nonpaged_pool_pages: le_u32(bytes, 116)?,
        context_switches: le_u32(bytes, 296)?,
        system_calls: le_u32(bytes, 308)?,
    })
}

/// Reads the kernel's performance block. `buf` is scratch space that is reused.
pub fn kernel_counters(buf: &mut Vec<u64>) -> Option<KernelCounters> {
    if buf.len() < 64 {
        buf.resize(64, 0);
    }
    let written = nt_query_grow(CLASS_PERFORMANCE, buf, 1 << 16)?;
    parse_kernel_counters(as_bytes(buf, written))
}

// ----------------------------------------------------------------------------------
// Page lists and page files
// ----------------------------------------------------------------------------------

/// Sizes of the kernel's page lists in pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageLists {
    pub zeroed: u64,
    pub free: u64,
    pub modified: u64,
    pub standby: u64,
}

/// Decodes a `SystemMemoryListInformation` block: five counts followed by
/// eight standby counts by priority, each one pointer-sized.
pub fn parse_page_lists(bytes: &[u8]) -> Option<PageLists> {
    let mut standby = 0u64;
    for priority in 0..8 {
        standby = standby.saturating_add(le_u64(bytes, 40 + priority * 8)?);
    }
    Some(PageLists {
        zeroed: le_u64(bytes, 0)?,
        free: le_u64(bytes, 8)?,
        modified: le_u64(bytes, 16)?,
        standby,
    })
}

/// Size of the system file cache including transition pages, in pages, from
/// a `SystemFileCacheInformation` block.
pub fn parse_file_cache(bytes: &[u8]) -> Option<u64> {
    le_u64(bytes, 40)
}

/// Sums the page files of a `SystemPageFileInformation` list: (total, in use)
/// in pages. A truncated or looping list ends the walk with what was read.
pub fn parse_page_files(bytes: &[u8]) -> (u64, u64) {
    let (mut total, mut used) = (0u64, 0u64);
    let mut offset = 0usize;
    for _ in 0..64 {
        let (Some(next), Some(size), Some(in_use)) = (
            le_u32(bytes, offset),
            offset.checked_add(4).and_then(|o| le_u32(bytes, o)),
            offset.checked_add(8).and_then(|o| le_u32(bytes, o)),
        ) else {
            break;
        };
        // An entry is at least 32 bytes; a shorter rest is not an entry.
        if offset.checked_add(32).is_none_or(|end| end > bytes.len()) {
            break;
        }
        total += u64::from(size);
        used += u64::from(in_use.min(size));
        if next < 32 {
            break;
        }
        match offset.checked_add(next as usize) {
            Some(n) => offset = n,
            None => break,
        }
    }
    (total, used)
}

/// Memory fitted in the machine minus what Windows can use, `None` when the
/// firmware does not say or the numbers make no sense.
pub fn hardware_reserved(installed_bytes: u64, usable_bytes: u64) -> Option<u64> {
    if installed_bytes == 0 || usable_bytes == 0 || installed_bytes < usable_bytes {
        return None;
    }
    // More than half "reserved" means the firmware figure is wrong.
    let reserved = installed_bytes - usable_bytes;
    (reserved <= installed_bytes / 2).then_some(reserved)
}

fn page_size() -> u64 {
    let mut info = SYSTEM_INFO::default();
    // SAFETY: `info` is a valid out-structure; the call cannot fail.
    unsafe { GetSystemInfo(&mut info) };
    if info.dwPageSize == 0 {
        4096
    } else {
        u64::from(info.dwPageSize)
    }
}

/// Where the page-list sizes come from on this machine.
enum ListSource {
    Untried,
    /// The kernel answers directly.
    Kernel,
    /// The `\Memory` performance counters (no special rights needed).
    Counters {
        query: pdh::Query,
        standby: Vec<pdh::Counter>,
        modified: pdh::Counter,
        free: pdh::Counter,
    },
    Unavailable,
}

pub struct MemorySampler {
    buf: Vec<u64>,
    page: u64,
    lists: ListSource,
    installed: Option<u64>,
    installed_read: bool,
    process_buf: Vec<u64>,
    faults: Option<(Instant, u32)>,
}

impl Default for MemorySampler {
    fn default() -> Self {
        MemorySampler {
            buf: Vec::new(),
            page: 0,
            lists: ListSource::Untried,
            installed: None,
            installed_read: false,
            process_buf: Vec::new(),
            faults: None,
        }
    }
}

impl MemorySampler {
    pub fn new() -> MemorySampler {
        MemorySampler::default()
    }

    pub fn sample(&mut self) -> MemorySample {
        let mut out = MemorySample::default();
        if self.page == 0 {
            self.page = page_size();
        }
        let page = self.page;

        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: `status` is a valid out-structure with its length set.
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
            out.total = status.ullTotalPhys;
            out.available = status.ullAvailPhys.min(status.ullTotalPhys);
            out.commit_limit = status.ullTotalPageFile;
            out.commit_used = status
                .ullTotalPageFile
                .saturating_sub(status.ullAvailPageFile);
        }

        let now = Instant::now();
        if let Some(kernel) = kernel_counters(&mut self.buf) {
            // System-wide commit; the figures above are capped by job limits.
            if kernel.commit_limit_pages > 0 {
                out.commit_used = u64::from(kernel.committed_pages) * page;
                out.commit_limit = u64::from(kernel.commit_limit_pages) * page;
            }
            out.kernel_paged = u64::from(kernel.paged_pool_pages) * page;
            out.kernel_nonpaged = u64::from(kernel.nonpaged_pool_pages) * page;
            if let Some((at, before)) = self.faults {
                let seconds = now.duration_since(at).as_secs_f64();
                if seconds > 0.0 {
                    out.page_faults_per_s =
                        Some(pdh::rate_wrapping_u32(before, kernel.page_faults, seconds));
                }
            }
            self.faults = Some((now, kernel.page_faults));
        }
        out.used = out.total.saturating_sub(out.available);

        match self.page_lists(page) {
            Some(lists) => {
                out.standby = Some(lists.standby * page);
                out.modified = Some(lists.modified * page);
                out.free = (lists.zeroed + lists.free) * page;
                out.cached = (lists.standby + lists.modified) * page;
            }
            None => {
                // Without the lists only the size of the file cache is known.
                if let Ok(written) = nt_query(CLASS_FILE_CACHE, &mut self.buf) {
                    out.cached = parse_file_cache(as_bytes(&self.buf, written))
                        .unwrap_or(0)
                        .saturating_mul(page)
                        .min(out.total);
                }
            }
        }

        self.page_files(&mut out, page);

        if !self.installed_read {
            self.installed_read = true;
            let mut kilobytes = 0u64;
            // SAFETY: `kilobytes` is a valid out-pointer.
            if unsafe { GetPhysicallyInstalledSystemMemory(&mut kilobytes) }.is_ok() {
                self.installed = Some(kilobytes.saturating_mul(1024));
            }
        }
        out.hardware_reserved = self
            .installed
            .and_then(|installed| hardware_reserved(installed, out.total));

        out.compressed = compressed_memory(&mut self.process_buf);
        out
    }

    /// The page lists from whichever source works on this machine. The
    /// source is chosen on the first call.
    fn page_lists(&mut self, page: u64) -> Option<PageLists> {
        if matches!(self.lists, ListSource::Untried) {
            self.lists = if self.kernel_lists().is_some() {
                ListSource::Kernel
            } else {
                open_list_counters().unwrap_or(ListSource::Unavailable)
            };
        }
        if matches!(self.lists, ListSource::Kernel) {
            return self.kernel_lists();
        }
        match &mut self.lists {
            ListSource::Counters {
                query,
                standby,
                modified,
                free,
            } => {
                if !query.collect() {
                    return None;
                }
                // These counters are in bytes; `PageLists` is in pages.
                let page = page.max(1);
                let standby_bytes: f64 = standby.iter().filter_map(|&c| query.read_value(c)).sum();
                Some(PageLists {
                    zeroed: 0,
                    free: query.read_value(*free)?.max(0.0) as u64 / page,
                    modified: query.read_value(*modified)?.max(0.0) as u64 / page,
                    standby: standby_bytes.max(0.0) as u64 / page,
                })
            }
            _ => None,
        }
    }

    fn kernel_lists(&mut self) -> Option<PageLists> {
        if self.buf.len() < 64 {
            self.buf.resize(64, 0);
        }
        let written = nt_query(CLASS_MEMORY_LISTS, &mut self.buf).ok()?;
        parse_page_lists(as_bytes(&self.buf, written))
    }

    fn page_files(&mut self, out: &mut MemorySample, page: u64) {
        if self.buf.len() < 64 {
            self.buf.resize(64, 0);
        }
        match nt_query_grow(CLASS_PAGE_FILES, &mut self.buf, 1 << 20) {
            Some(written) => {
                let (total, used) = parse_page_files(as_bytes(&self.buf, written));
                out.pagefile_total = total * page;
                out.pagefile_used = used * page;
            }
            None => {
                // Without the list only the size is known (commit limit minus memory).
                out.pagefile_total = out.commit_limit.saturating_sub(out.total);
            }
        }
    }
}

fn open_list_counters() -> Option<ListSource> {
    let mut query = pdh::Query::open().ok()?;
    let standby: Vec<pdh::Counter> = [
        r"\Memory\Standby Cache Reserve Bytes",
        r"\Memory\Standby Cache Normal Priority Bytes",
        r"\Memory\Standby Cache Core Bytes",
    ]
    .iter()
    .filter_map(|path| query.add(path).ok())
    .collect();
    if standby.is_empty() {
        return None;
    }
    let modified = query.add(r"\Memory\Modified Page List Bytes").ok()?;
    let free = query.add(r"\Memory\Free & Zero Page List Bytes").ok()?;
    Some(ListSource::Counters {
        query,
        standby,
        modified,
        free,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_counter_offsets() {
        let mut block = vec![0u8; 344];
        let mut put = |offset: usize, value: u32| {
            block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(44, 11);
        put(48, 22);
        put(52, 33);
        put(60, 1234);
        put(112, 44);
        put(116, 55);
        put(296, 987_654);
        put(308, 42);
        assert_eq!(
            parse_kernel_counters(&block),
            Some(KernelCounters {
                page_faults: 1234,
                context_switches: 987_654,
                system_calls: 42,
                available_pages: 11,
                committed_pages: 22,
                commit_limit_pages: 33,
                paged_pool_pages: 44,
                nonpaged_pool_pages: 55,
            })
        );
        assert_eq!(parse_kernel_counters(&block[..311]), None);
        assert_eq!(parse_kernel_counters(&[]), None);
        assert!(parse_kernel_counters(&[0xFF; 344]).is_some());

        let mut cache = vec![0u8; 64];
        cache[40..48].copy_from_slice(&777u64.to_le_bytes());
        assert_eq!(parse_file_cache(&cache), Some(777));
        assert_eq!(parse_file_cache(&cache[..47]), None);
    }

    #[test]
    fn page_lists_decode() {
        let mut block = vec![0u8; 176];
        let mut put = |index: usize, value: u64| {
            block[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
        };
        put(0, 100); // zeroed
        put(1, 20); // free
        put(2, 300); // modified
        for priority in 0..8 {
            put(5 + priority, 10 * (priority as u64 + 1));
        }
        assert_eq!(
            parse_page_lists(&block),
            Some(PageLists {
                zeroed: 100,
                free: 20,
                modified: 300,
                standby: 360,
            })
        );
        assert_eq!(parse_page_lists(&block[..103]), None, "truncated");
        assert_eq!(parse_page_lists(&[]), None);
        let ones = parse_page_lists(&[0xFF; 176]).expect("decodes");
        assert_eq!(ones.standby, u64::MAX, "saturates instead of overflowing");
    }

    #[test]
    fn page_file_list_decode() {
        fn entry(next: u32, total: u32, used: u32) -> Vec<u8> {
            let mut e = vec![0u8; 48];
            e[0..4].copy_from_slice(&next.to_le_bytes());
            e[4..8].copy_from_slice(&total.to_le_bytes());
            e[8..12].copy_from_slice(&used.to_le_bytes());
            e
        }
        let mut list = entry(48, 1000, 250);
        list.extend(entry(0, 500, 100));
        assert_eq!(parse_page_files(&list), (1500, 350));
        assert_eq!(
            parse_page_files(&list[..48]),
            (1000, 250),
            "second entry cut off"
        );
        assert_eq!(parse_page_files(&list[..31]), (0, 0));
        assert_eq!(parse_page_files(&[]), (0, 0));
        // In-use above the size is clamped, never a negative free amount.
        assert_eq!(parse_page_files(&entry(0, 10, 99)), (10, 10));
        // An entry that points at itself must not loop.
        assert_eq!(parse_page_files(&entry(4, 7, 1)), (7, 1));
        let all_ones = vec![0xFFu8; 96];
        let (total, used) = parse_page_files(&all_ones);
        assert_eq!((total, used), (u64::from(u32::MAX), u64::from(u32::MAX)));
        assert_eq!(parse_page_files(&[0u8; 96]), (0, 0));
    }

    #[test]
    fn hardware_reserved_is_sane() {
        let gib = 1u64 << 30;
        assert_eq!(
            hardware_reserved(16 * gib, 16 * gib - 300 * (1 << 20)),
            Some(300 << 20)
        );
        assert_eq!(hardware_reserved(16 * gib, 16 * gib), Some(0));
        assert_eq!(hardware_reserved(0, 16 * gib), None);
        assert_eq!(
            hardware_reserved(8 * gib, 16 * gib),
            None,
            "installed below usable"
        );
        assert_eq!(hardware_reserved(64 * gib, 16 * gib), None, "implausible");
        assert_eq!(hardware_reserved(16 * gib, 0), None);
    }

    /// True when two sizes differ by less than 3 % of the larger one plus 64 MB
    /// (the two readings are taken a moment apart).
    fn roughly(a: u64, b: u64) -> bool {
        a.abs_diff(b) <= a.max(b) / 33 + (64 << 20)
    }

    #[test]
    fn live_smoke() {
        let mut sampler = MemorySampler::new();
        let first = sampler.sample();
        assert!(
            first.page_faults_per_s.is_none(),
            "a rate needs two readings"
        );
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        let mut m = MemorySample::default();
        for _ in 0..20 {
            m = sampler.sample();
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;

        assert!(m.total > 0);
        assert!(m.available <= m.total);
        assert_eq!(m.used + m.available, m.total);
        assert!(m.commit_used <= m.commit_limit);
        assert!(m.commit_limit >= m.total / 2);
        assert!(m.pagefile_used <= m.pagefile_total);
        assert!(m.kernel_paged > 0 && m.kernel_nonpaged > 0);
        assert!(m.cached <= m.total);
        assert!(m.free <= m.total);
        if let (Some(standby), Some(modified)) = (m.standby, m.modified) {
            assert!(standby <= m.total && modified <= m.total);
            // Available memory is the standby list plus the free lists.
            let sum = standby + m.free;
            let diff = sum.abs_diff(m.available);
            assert!(
                diff <= m.total / 10,
                "standby + free differs from available"
            );
        }
        assert!(m.page_faults_per_s.is_some_and(|r| r >= 0.0));
        assert!((0.0..=100.0).contains(&m.used_pct()));

        // The fields decoded by offset must agree with what the documented
        // call reports for the same machine.
        let mut raw = PERFORMANCE_INFORMATION::default();
        let size = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
        // SAFETY: `raw` is a valid out-structure of the size passed.
        unsafe { GetPerformanceInfo(&mut raw, size) }.expect("performance info");
        let page = raw.PageSize as u64;
        assert_eq!(page, sampler.page);
        let m = sampler.sample();
        assert!(roughly(m.commit_used, raw.CommitTotal as u64 * page));
        assert!(roughly(m.commit_limit, raw.CommitLimit as u64 * page));
        assert!(roughly(m.kernel_paged, raw.KernelPaged as u64 * page));
        assert!(roughly(m.kernel_nonpaged, raw.KernelNonpaged as u64 * page));
        assert!(roughly(m.available, raw.PhysicalAvailable as u64 * page));
        let mut buf = vec![0u64; 64];
        let kernel = kernel_counters(&mut buf).expect("kernel counters");
        assert!(roughly(
            u64::from(kernel.available_pages) * page,
            raw.PhysicalAvailable as u64 * page
        ));
        let cache_matches = nt_query(CLASS_FILE_CACHE, &mut buf)
            .ok()
            .and_then(|written| parse_file_cache(as_bytes(&buf, written)))
            .map(|pages| roughly(pages * page, raw.SystemCache as u64 * page));

        let counts = system_counts().expect("system counts");
        assert!(counts.processes > 0 && counts.threads > 0 && counts.handles > 0);
        println!(
            "memory: page lists {}, compressed {}, hardware reserved {}, page file list {}, file cache size agrees {:?}, avg {ms:.3} ms per call",
            if m.standby.is_some() { "yes" } else { "no" },
            if m.compressed.is_some() { "yes" } else { "no" },
            if m.hardware_reserved.is_some() { "yes" } else { "no" },
            if m.pagefile_total > 0 { "yes" } else { "no" },
            cache_matches,
        );
        println!(
            "memory: list source {}",
            match sampler.lists {
                ListSource::Kernel => "kernel",
                ListSource::Counters { .. } => "counters",
                _ => "none",
            }
        );
    }
}
