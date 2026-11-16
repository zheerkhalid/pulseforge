//! Processor: identification and live load.
//!
//! Identification comes from CPUID, the logical-processor map of Windows and
//! the registry. Load comes from the per-processor time counters of the
//! kernel; the effective clock is the "processor performance" counter of
//! Windows (a percentage of the nominal clock) multiplied by the nominal
//! clock. Per-core temperatures need a kernel driver and are not read here.

use super::memory::{kernel_counters, system_counts};
use super::pdh::{self, le_u16, le_u32, le_u64};
use super::process::{as_bytes, nt_query};
use crate::model::{CacheInfo, CpuInfo, CpuSample};
use crate::util::wide;
use std::ffi::c_void;
use std::time::Instant;
use windows::core::{s, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Power::{
    CallNtPowerInformation, ProcessorInformation, PROCESSOR_POWER_INFORMATION,
};
use windows::Win32::System::SystemInformation::{GetLogicalProcessorInformationEx, RelationAll};
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, IsProcessorFeaturePresent,
    ALL_PROCESSOR_GROUPS, PF_VIRT_FIRMWARE_ENABLED,
};

// ----------------------------------------------------------------------------------
// CPUID decoding (pure)
// ----------------------------------------------------------------------------------

/// The CPUID registers that carry feature bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureRegs {
    /// Leaf 1.
    pub l1_ecx: u32,
    pub l1_edx: u32,
    /// Leaf 7, sub-leaf 0.
    pub l7_ebx: u32,
    pub l7_ecx: u32,
    pub l7_edx: u32,
    /// Leaf 7, sub-leaf 1.
    pub l7s1_eax: u32,
    /// Leaf 0x8000_0001.
    pub e1_ecx: u32,
    pub e1_edx: u32,
}

#[derive(Clone, Copy)]
enum Reg {
    L1Ecx,
    L1Edx,
    L7Ebx,
    L7Ecx,
    L7Edx,
    L7S1Eax,
    E1Ecx,
    E1Edx,
}

/// Feature bits in the order they are shown.
const FEATURES: [(Reg, u32, &str); 48] = [
    (Reg::E1Edx, 29, "x86-64"),
    (Reg::L1Edx, 23, "MMX"),
    (Reg::L1Edx, 25, "SSE"),
    (Reg::L1Edx, 26, "SSE2"),
    (Reg::L1Ecx, 0, "SSE3"),
    (Reg::L1Ecx, 9, "SSSE3"),
    (Reg::L1Ecx, 19, "SSE4.1"),
    (Reg::L1Ecx, 20, "SSE4.2"),
    (Reg::E1Ecx, 6, "SSE4A"),
    (Reg::L1Ecx, 28, "AVX"),
    (Reg::L7Ebx, 5, "AVX2"),
    (Reg::L7S1Eax, 4, "AVX-VNNI"),
    (Reg::L7S1Eax, 23, "AVX-IFMA"),
    (Reg::L7Ebx, 16, "AVX-512F"),
    (Reg::L7Ebx, 17, "AVX-512DQ"),
    (Reg::L7Ebx, 21, "AVX-512IFMA"),
    (Reg::L7Ebx, 28, "AVX-512CD"),
    (Reg::L7Ebx, 30, "AVX-512BW"),
    (Reg::L7Ebx, 31, "AVX-512VL"),
    (Reg::L7Ecx, 1, "AVX-512VBMI"),
    (Reg::L7Ecx, 6, "AVX-512VBMI2"),
    (Reg::L7Ecx, 11, "AVX-512VNNI"),
    (Reg::L7Ecx, 12, "AVX-512BITALG"),
    (Reg::L7Ecx, 14, "AVX-512VPOPCNTDQ"),
    (Reg::L7Edx, 8, "AVX-512VP2INTERSECT"),
    (Reg::L7Edx, 23, "AVX-512FP16"),
    (Reg::L7S1Eax, 5, "AVX-512BF16"),
    (Reg::L7Edx, 24, "AMX"),
    (Reg::L1Ecx, 12, "FMA"),
    (Reg::E1Ecx, 16, "FMA4"),
    (Reg::E1Ecx, 11, "XOP"),
    (Reg::L1Ecx, 29, "F16C"),
    (Reg::L1Ecx, 25, "AES"),
    (Reg::L7Ecx, 9, "VAES"),
    (Reg::L1Ecx, 1, "PCLMULQDQ"),
    (Reg::L7Ecx, 10, "VPCLMULQDQ"),
    (Reg::L7Ecx, 8, "GFNI"),
    (Reg::L7Ebx, 29, "SHA"),
    (Reg::L7Ebx, 3, "BMI1"),
    (Reg::L7Ebx, 8, "BMI2"),
    (Reg::L7Ebx, 19, "ADX"),
    (Reg::L1Ecx, 23, "POPCNT"),
    (Reg::E1Ecx, 5, "LZCNT"),
    (Reg::L1Ecx, 22, "MOVBE"),
    (Reg::L1Ecx, 30, "RDRAND"),
    (Reg::L7Ebx, 18, "RDSEED"),
    (Reg::L1Ecx, 5, "VT-x"),
    (Reg::E1Ecx, 2, "AMD-V"),
];

/// The instruction-set features in display form, in a fixed order.
pub fn decode_features(regs: &FeatureRegs) -> Vec<String> {
    FEATURES
        .iter()
        .filter(|(reg, bit, _)| {
            let value = match reg {
                Reg::L1Ecx => regs.l1_ecx,
                Reg::L1Edx => regs.l1_edx,
                Reg::L7Ebx => regs.l7_ebx,
                Reg::L7Ecx => regs.l7_ecx,
                Reg::L7Edx => regs.l7_edx,
                Reg::L7S1Eax => regs.l7s1_eax,
                Reg::E1Ecx => regs.e1_ecx,
                Reg::E1Edx => regs.e1_edx,
            };
            value & (1 << bit) != 0
        })
        .map(|(_, _, name)| (*name).to_string())
        .collect()
}

/// (family, model, stepping) as they are conventionally displayed, from the
/// processor signature (CPUID leaf 1, EAX).
pub fn decode_signature(eax: u32) -> (u32, u32, u32) {
    let stepping = eax & 0xF;
    let base_model = (eax >> 4) & 0xF;
    let base_family = (eax >> 8) & 0xF;
    let ext_model = (eax >> 16) & 0xF;
    let ext_family = (eax >> 20) & 0xFF;
    let family = if base_family == 0xF {
        base_family + ext_family
    } else {
        base_family
    };
    let model = if base_family == 0x6 || base_family == 0xF {
        (ext_model << 4) | base_model
    } else {
        base_model
    };
    (family, model, stepping)
}

/// The twelve-character vendor identification from leaf 0 (EBX, EDX, ECX).
pub fn vendor_id(ebx: u32, edx: u32, ecx: u32) -> String {
    let mut bytes = Vec::with_capacity(12);
    for reg in [ebx, edx, ecx] {
        bytes.extend_from_slice(&reg.to_le_bytes());
    }
    printable(&bytes)
}

/// The maker's name for a vendor identification, or the identification itself.
pub fn vendor_name(id: &str) -> String {
    match id {
        "GenuineIntel" => "Intel".to_string(),
        "AuthenticAMD" => "AMD".to_string(),
        "HygonGenuine" => "Hygon".to_string(),
        "CentaurHauls" | "  Shanghai  " => "Zhaoxin".to_string(),
        other => other.trim().to_string(),
    }
}

/// The brand string from the twelve registers of leaves 0x8000_0002..=4,
/// trimmed and with runs of spaces collapsed.
pub fn brand_string(regs: &[u32; 12]) -> String {
    let mut bytes = Vec::with_capacity(48);
    for reg in regs {
        bytes.extend_from_slice(&reg.to_le_bytes());
    }
    let text = printable(&bytes);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// ASCII text up to the first NUL; anything unprintable becomes a space.
fn printable(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect()
}

/// Microcode revision from the registry's 8-byte "Update Revision" value.
/// One maker stores it in the upper half, the other in the lower half.
pub fn microcode_text(raw: &[u8]) -> String {
    let low = le_u32(raw, 0).unwrap_or(0);
    let high = le_u32(raw, 4).unwrap_or(0);
    let revision = if high != 0 { high } else { low };
    if revision == 0 {
        String::new()
    } else {
        format!("0x{revision:X}")
    }
}

// ----------------------------------------------------------------------------------
// Topology decoding (pure)
// ----------------------------------------------------------------------------------

/// What the logical-processor map of Windows says about the machine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Topology {
    pub packages: u32,
    pub cores: u32,
    pub threads: u32,
    pub caches: Vec<CacheInfo>,
    /// (efficiency class, number of cores), ascending by class.
    pub efficiency_classes: Vec<(u8, u32)>,
}

impl Topology {
    /// (performance cores, efficiency cores) on a hybrid processor.
    pub fn hybrid_split(&self) -> Option<(u32, u32)> {
        if self.efficiency_classes.len() < 2 {
            return None;
        }
        let (_, performance) = *self.efficiency_classes.last()?;
        let all: u32 = self.efficiency_classes.iter().map(|(_, n)| n).sum();
        Some((performance, all - performance))
    }
}

const RELATION_CORE: u32 = 0;
const RELATION_CACHE: u32 = 2;
const RELATION_PACKAGE: u32 = 3;

/// Decodes the variable-length records of `GetLogicalProcessorInformationEx`.
/// A record that does not fit ends the walk; what was read so far is kept.
pub fn parse_topology(bytes: &[u8]) -> Topology {
    let mut topo = Topology::default();
    let mut offset = 0usize;
    while let (Some(relation), Some(size)) = (
        le_u32(bytes, offset),
        offset.checked_add(4).and_then(|o| le_u32(bytes, o)),
    ) {
        let size = size as usize;
        let Some(end) = offset.checked_add(size) else {
            break;
        };
        if size < 8 || end > bytes.len() {
            break;
        }
        let record = &bytes[offset..end];
        match relation {
            RELATION_CORE => {
                if let (Some(&class), Some(groups)) = (record.get(9), le_u16(record, 30)) {
                    topo.cores += 1;
                    let fitting = record.len().saturating_sub(32) / 16;
                    let groups = usize::from(groups).min(fitting);
                    for g in 0..groups {
                        if let Some(mask) = le_u64(record, 32 + g * 16) {
                            topo.threads += mask.count_ones();
                        }
                    }
                    match topo
                        .efficiency_classes
                        .iter_mut()
                        .find(|(c, _)| *c == class)
                    {
                        Some((_, n)) => *n += 1,
                        None => topo.efficiency_classes.push((class, 1)),
                    }
                }
            }
            RELATION_PACKAGE => topo.packages += 1,
            RELATION_CACHE => {
                if let (
                    Some(&level),
                    Some(&associativity),
                    Some(line),
                    Some(cache_size),
                    Some(kind),
                ) = (
                    record.get(8),
                    record.get(9),
                    le_u16(record, 10),
                    le_u32(record, 12),
                    le_u32(record, 16),
                ) {
                    let kind = match kind {
                        0 => "Unified",
                        1 => "Instruction",
                        2 => "Data",
                        _ => "Trace",
                    };
                    let found = topo.caches.iter_mut().find(|c| {
                        c.level == level
                            && c.kind == kind
                            && c.size_bytes == u64::from(cache_size)
                            && c.line_size == u32::from(line)
                    });
                    match found {
                        Some(cache) => cache.count += 1,
                        None => topo.caches.push(CacheInfo {
                            level,
                            kind: kind.to_string(),
                            size_bytes: u64::from(cache_size),
                            count: 1,
                            line_size: u32::from(line),
                            associativity: u32::from(associativity),
                        }),
                    }
                }
            }
            _ => {}
        }
        offset = end;
    }
    topo.efficiency_classes.sort_unstable();
    topo.caches.sort_by(|a, b| {
        (a.level, &a.kind, std::cmp::Reverse(a.size_bytes)).cmp(&(
            b.level,
            &b.kind,
            std::cmp::Reverse(b.size_bytes),
        ))
    });
    topo
}

// ----------------------------------------------------------------------------------
// Load maths (pure)
// ----------------------------------------------------------------------------------

/// Running times of one logical processor in 100 ns units. As the kernel
/// reports them, `kernel` includes `idle`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

/// Size of one per-processor record of the kernel.
const CORE_RECORD: usize = 48;

/// Decodes per-processor records and appends them to `times` and the
/// interrupt totals to `interrupts`. Returns how many were appended; a
/// trailing partial record is ignored.
pub fn parse_core_times(
    bytes: &[u8],
    times: &mut Vec<CoreTimes>,
    interrupts: &mut Vec<u32>,
) -> usize {
    let mut added = 0;
    for record in bytes.as_chunks::<CORE_RECORD>().0 {
        times.push(CoreTimes {
            idle: le_u64(record, 0).unwrap_or(0),
            kernel: le_u64(record, 8).unwrap_or(0),
            user: le_u64(record, 16).unwrap_or(0),
        });
        interrupts.push(le_u32(record, 40).unwrap_or(0));
        added += 1;
    }
    added
}

/// Load between two readings: fills `per_core` (percent busy per logical
/// processor) and returns (total percent, kernel percent). When the number
/// of processors changed, everything is zero for this reading.
pub fn load_between(prev: &[CoreTimes], now: &[CoreTimes], per_core: &mut Vec<f32>) -> (f32, f32) {
    per_core.clear();
    if prev.len() != now.len() || now.is_empty() {
        per_core.resize(now.len(), 0.0);
        return (0.0, 0.0);
    }
    let (mut all, mut busy_all, mut kernel_all) = (0u64, 0u64, 0u64);
    for (p, n) in prev.iter().zip(now) {
        let idle = n.idle.saturating_sub(p.idle);
        let kernel = n.kernel.saturating_sub(p.kernel);
        let user = n.user.saturating_sub(p.user);
        let total = kernel.saturating_add(user);
        let busy = total.saturating_sub(idle);
        per_core.push(percent(busy, total));
        all = all.saturating_add(total);
        busy_all = busy_all.saturating_add(busy);
        kernel_all = kernel_all.saturating_add(kernel.saturating_sub(idle).min(busy));
    }
    (percent(busy_all, all), percent(kernel_all, all))
}

fn percent(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64 * 100.0).clamp(0.0, 100.0) as f32
    }
}

/// Parses the instance name of a per-processor counter: "0,12" is processor
/// 12 of group 0. The totals ("_Total", "0,_Total") give `None`.
pub fn parse_core_instance(name: &str) -> Option<(u16, u16)> {
    let (group, index) = name.split_once(',')?;
    Some((group.trim().parse().ok()?, index.trim().parse().ok()?))
}

/// Effective clock from the "processor performance" percentage and the
/// nominal clock. Nonsense input gives `None`.
pub fn effective_mhz(performance_pct: f64, nominal_mhz: u32) -> Option<f32> {
    if !performance_pct.is_finite() || performance_pct <= 0.0 || nominal_mhz == 0 {
        return None;
    }
    let mhz = performance_pct / 100.0 * f64::from(nominal_mhz);
    // Far above any real clock: a counter glitch, not a reading.
    (mhz <= 20_000.0).then_some(mhz as f32)
}

// ----------------------------------------------------------------------------------
// Reading this machine: identification
// ----------------------------------------------------------------------------------

/// One CPUID query: [EAX, EBX, ECX, EDX].
fn cpuid(leaf: u32, sub_leaf: u32) -> [u32; 4] {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: CPUID is available on every x86-64 processor and has no side effects.
        #[allow(unused_unsafe)]
        let r = unsafe { core::arch::x86_64::__cpuid_count(leaf, sub_leaf) };
        [r.eax, r.ebx, r.ecx, r.edx]
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (leaf, sub_leaf);
        [0; 4]
    }
}

/// The nominal clock of every logical processor Windows reports, in MHz.
fn nominal_clocks() -> Vec<u32> {
    // SAFETY: no pointers involved.
    let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.clamp(1, 4096) as usize;
    let mut info = vec![PROCESSOR_POWER_INFORMATION::default(); count];
    let bytes = (count * std::mem::size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    // SAFETY: the output buffer is writable for `bytes` bytes; no input buffer.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            None,
            0,
            Some(info.as_mut_ptr().cast::<c_void>()),
            bytes,
        )
    };
    if status.0 < 0 {
        return Vec::new();
    }
    // Entries the call did not fill stay zero and are dropped from the end.
    while info.last().is_some_and(|p| p.MaxMhz == 0) {
        info.pop();
    }
    info.iter().map(|p| p.MaxMhz).collect()
}

/// (~MHz, microcode revision) of processor 0 from the registry.
fn registry_facts() -> (Option<u32>, String) {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
    else {
        return (None, String::new());
    };
    let mhz = key.get_value::<u32, _>("~MHz").ok().filter(|&m| m > 0);
    let microcode = key
        .get_raw_value("Update Revision")
        .map(|v| microcode_text(&v.bytes))
        .unwrap_or_default();
    (mhz, microcode)
}

fn topology() -> Topology {
    let mut needed = 0u32;
    // SAFETY: a null buffer with a length of zero asks for the needed size.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationAll, None, &mut needed) };
    if needed == 0 || needed > (64 << 20) {
        return Topology::default();
    }
    let mut buf = vec![0u64; needed as usize / 8 + 1];
    let mut length = needed;
    // SAFETY: the buffer is writable for at least `length` bytes and 8-byte aligned.
    let ok = unsafe {
        GetLogicalProcessorInformationEx(RelationAll, Some(buf.as_mut_ptr().cast()), &mut length)
    };
    if ok.is_err() {
        return Topology::default();
    }
    parse_topology(as_bytes(&buf, length as usize))
}

/// Static facts about the processor (CPUID, topology, caches, features).
pub fn info() -> CpuInfo {
    let mut out = CpuInfo {
        architecture: if cfg!(target_arch = "x86_64") {
            "x86-64".to_string()
        } else {
            std::env::consts::ARCH.to_string()
        },
        ..Default::default()
    };

    // CPUID.
    let leaf0 = cpuid(0, 0);
    let max_leaf = leaf0[0];
    let vendor = vendor_id(leaf0[1], leaf0[3], leaf0[2]);
    out.vendor = vendor_name(&vendor);
    let leaf1 = if max_leaf >= 1 { cpuid(1, 0) } else { [0; 4] };
    (out.family, out.model, out.stepping) = decode_signature(leaf1[0]);
    let leaf7 = if max_leaf >= 7 { cpuid(7, 0) } else { [0; 4] };
    let leaf7_1 = if max_leaf >= 7 && leaf7[0] >= 1 {
        cpuid(7, 1)
    } else {
        [0; 4]
    };
    let max_ext = cpuid(0x8000_0000, 0)[0];
    let ext1 = if max_ext >= 0x8000_0001 {
        cpuid(0x8000_0001, 0)
    } else {
        [0; 4]
    };
    let regs = FeatureRegs {
        l1_ecx: leaf1[2],
        l1_edx: leaf1[3],
        l7_ebx: leaf7[1],
        l7_ecx: leaf7[2],
        l7_edx: leaf7[3],
        l7s1_eax: leaf7_1[0],
        e1_ecx: ext1[2],
        e1_edx: ext1[3],
    };
    out.features = decode_features(&regs);
    out.hypervisor = leaf1[2] & (1 << 31) != 0;
    if max_ext >= 0x8000_0004 {
        let mut brand = [0u32; 12];
        for (i, leaf) in (0x8000_0002u32..=0x8000_0004).enumerate() {
            brand[i * 4..i * 4 + 4].copy_from_slice(&cpuid(leaf, 0));
        }
        out.name = brand_string(&brand);
    }
    if out.name.is_empty() {
        out.name = format!("{} family {} model {}", out.vendor, out.family, out.model);
    }
    if out.hypervisor {
        let hv = cpuid(0x4000_0000, 0);
        let id = vendor_id(hv[1], hv[2], hv[3]);
        if !id.trim().is_empty() {
            out.extra
                .push(("Hypervisor".to_string(), id.trim().to_string()));
        }
    }
    if max_ext >= 0x8000_0008 {
        let widths = cpuid(0x8000_0008, 0)[0];
        let (physical, linear) = (widths & 0xFF, (widths >> 8) & 0xFF);
        if physical > 0 && linear > 0 {
            out.extra.push((
                "Address width".to_string(),
                format!("{physical}-bit physical, {linear}-bit virtual"),
            ));
        }
    }
    let hybrid_flag = leaf7[3] & (1 << 15) != 0;

    // Topology and caches.
    let topo = topology();
    out.packages = topo.packages;
    out.cores = topo.cores;
    out.threads = topo.threads;
    if out.threads == 0 {
        // SAFETY: no pointers involved.
        out.threads = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    }
    if let Some((performance, efficiency)) = topo.hybrid_split() {
        out.performance_cores = Some(performance);
        out.efficiency_cores = Some(efficiency);
    } else if hybrid_flag {
        out.extra.push((
            "Hybrid design".to_string(),
            "Yes (core types not reported by Windows)".to_string(),
        ));
    }
    out.caches = topo.caches;
    // SAFETY: no pointers involved.
    let groups = unsafe { GetActiveProcessorGroupCount() };
    if groups > 1 {
        out.extra
            .push(("Processor groups".to_string(), groups.to_string()));
    }

    // Clocks.
    let (registry_mhz, microcode) = registry_facts();
    out.microcode = microcode;
    let nominal = nominal_clocks();
    out.base_mhz = registry_mhz.or_else(|| nominal.first().copied().filter(|&m| m > 0));
    // Leaf 0x16 (one maker only) states the base and the maximum clock.
    if max_leaf >= 0x16 && vendor == "GenuineIntel" {
        let clocks = cpuid(0x16, 0);
        let (base, max) = (clocks[0] & 0xFFFF, clocks[1] & 0xFFFF);
        if out.base_mhz.is_none() && base > 0 {
            out.base_mhz = Some(base);
        }
        if max > 0 && max >= base {
            out.max_mhz = Some(max);
        }
    }
    if out.max_mhz.is_none() {
        // On hybrid processors the fastest core type has the highest nominal clock.
        let top = nominal.iter().copied().max().unwrap_or(0);
        if top > 0 && out.base_mhz.is_some_and(|base| top > base) {
            out.max_mhz = Some(top);
        }
    }

    // Virtualisation as Windows reports it. Under a hypervisor Windows no
    // longer sees the firmware switch, so the answer is left open there.
    // SAFETY: no pointers involved.
    let firmware_enabled = unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) }.as_bool();
    out.virtualization = if firmware_enabled {
        Some(true)
    } else if out.hypervisor {
        out.extra.push((
            "Virtualisation".to_string(),
            "In use by the running hypervisor".to_string(),
        ));
        None
    } else {
        Some(false)
    };
    out
}

// ----------------------------------------------------------------------------------
// Reading this machine: live load
// ----------------------------------------------------------------------------------

/// `SystemProcessorPerformanceInformation`.
const CLASS_CORE_TIMES: i32 = 8;

/// `NtQuerySystemInformationEx`: like the plain query with an input buffer,
/// here the processor group to report.
type QueryEx =
    unsafe extern "system" fn(i32, *const c_void, u32, *mut c_void, u32, *mut u32) -> i32;

fn find_query_ex() -> Option<QueryEx> {
    let module = wide("ntdll.dll");
    // SAFETY: the name is NUL-terminated; ntdll is loaded in every process, so
    // this only looks it up and takes no reference that would need releasing.
    let handle = unsafe { GetModuleHandleW(PCWSTR(module.as_ptr())) }.ok()?;
    // SAFETY: the module handle is valid and the name is a NUL-terminated literal.
    let address = unsafe { GetProcAddress(handle, s!("NtQuerySystemInformationEx")) }?;
    // SAFETY: the export has exactly this signature on every supported Windows version.
    Some(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryEx>(address) })
}

/// The clock counter and what is needed to turn it into MHz.
struct ClockCounter {
    query: pdh::Query,
    counter: pdh::Counter,
}

/// Live processor load. Keeps the previous counters between calls.
pub struct CpuSampler {
    query_ex: Option<QueryEx>,
    buf: Vec<u64>,
    kernel_buf: Vec<u64>,
    previous: Vec<CoreTimes>,
    current: Vec<CoreTimes>,
    interrupts_prev: Vec<u32>,
    interrupts_now: Vec<u32>,
    last: Option<Instant>,
    context_switches: Option<u32>,
    clock: Option<ClockCounter>,
    clock_tried: bool,
    nominal: Vec<u32>,
    base_mhz: u32,
    /// (group, index, MHz) of the latest clock reading.
    clocks: Vec<(u16, u16, f32)>,
    clock_period: std::time::Duration,
    clock_at: Option<Instant>,
    /// Per-core and average clock of the latest reading.
    clock_last: (Vec<f32>, Option<f32>),
}

impl Default for CpuSampler {
    fn default() -> Self {
        CpuSampler::new()
    }
}

impl CpuSampler {
    pub fn new() -> CpuSampler {
        CpuSampler {
            query_ex: find_query_ex(),
            buf: Vec::new(),
            kernel_buf: Vec::new(),
            previous: Vec::new(),
            current: Vec::new(),
            interrupts_prev: Vec::new(),
            interrupts_now: Vec::new(),
            last: None,
            context_switches: None,
            clock: None,
            clock_tried: false,
            nominal: Vec::new(),
            base_mhz: 0,
            clocks: Vec::new(),
            clock_period: std::time::Duration::ZERO,
            clock_at: None,
            clock_last: (Vec::new(), None),
        }
    }

    /// Reads the time counters of every logical processor, group by group,
    /// into `self.current` and `self.interrupts_now`.
    fn read_core_times(&mut self) {
        self.current.clear();
        self.interrupts_now.clear();
        // SAFETY: no pointers involved.
        let groups = unsafe { GetActiveProcessorGroupCount() }.max(1);
        if let Some(query_ex) = self.query_ex {
            for group in 0..groups {
                // SAFETY: no pointers involved.
                let count = unsafe { GetActiveProcessorCount(group) } as usize;
                let words = (count.max(1) * CORE_RECORD).div_ceil(8);
                if self.buf.len() < words {
                    self.buf.resize(words, 0);
                }
                let mut returned = 0u32;
                // SAFETY: the input is one u16 that outlives the call; the output
                // buffer is writable for the length passed and 8-byte aligned.
                let status = unsafe {
                    query_ex(
                        CLASS_CORE_TIMES,
                        (&group as *const u16).cast(),
                        std::mem::size_of::<u16>() as u32,
                        self.buf.as_mut_ptr().cast(),
                        (self.buf.len() * 8) as u32,
                        &mut returned,
                    )
                };
                if status < 0 {
                    self.current.clear();
                    self.interrupts_now.clear();
                    break;
                }
                parse_core_times(
                    as_bytes(&self.buf, returned as usize),
                    &mut self.current,
                    &mut self.interrupts_now,
                );
            }
            if !self.current.is_empty() {
                return;
            }
        }
        // The plain query: the processors of this thread's group only.
        // SAFETY: no pointers involved.
        let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(1) as usize;
        let words = (count * CORE_RECORD).div_ceil(8);
        if self.buf.len() < words {
            self.buf.resize(words, 0);
        }
        if let Ok(written) = nt_query(CLASS_CORE_TIMES, &mut self.buf) {
            parse_core_times(
                as_bytes(&self.buf, written),
                &mut self.current,
                &mut self.interrupts_now,
            );
        }
    }

    fn read_clocks(&mut self, sample: &mut CpuSample) {
        if !self.clock_tried {
            self.clock_tried = true;
            self.nominal = nominal_clocks();
            self.base_mhz = registry_facts()
                .0
                .or_else(|| self.nominal.first().copied())
                .unwrap_or(0);
            self.clock = pdh::Query::open().ok().and_then(|mut query| {
                let counter = query
                    .add(r"\Processor Information(*)\% Processor Performance")
                    .ok()?;
                Some(ClockCounter { query, counter })
            });
        }
        let Some(clock) = &mut self.clock else {
            return;
        };
        if !clock.query.collect() {
            return;
        }
        let clocks = &mut self.clocks;
        let nominal = &self.nominal;
        let base = self.base_mhz;
        clocks.clear();
        let single_group = nominal.len() == sample.per_core_pct.len();
        let read = clock.query.read_array(clock.counter, |name, value| {
            if let Some((group, index)) = parse_core_instance(name) {
                // The per-processor nominal clock is only known to line up with
                // the processor numbers when there is a single group.
                let own = if single_group && group == 0 {
                    nominal.get(usize::from(index)).copied().unwrap_or(0)
                } else {
                    0
                };
                let nominal_mhz = if own > 0 { own } else { base };
                if let Some(mhz) = effective_mhz(value, nominal_mhz) {
                    clocks.push((group, index, mhz));
                }
            }
        });
        if read.is_err() || clocks.is_empty() {
            return;
        }
        clocks.sort_unstable_by_key(|&(group, index, _)| (group, index));
        sample.per_core_mhz.extend(clocks.iter().map(|c| c.2));
        let sum: f32 = sample.per_core_mhz.iter().sum();
        sample.current_mhz = Some(sum / sample.per_core_mhz.len() as f32);
    }

    /// How often the effective clocks are read; between readings the last
    /// values are repeated. The monitor lengthens this while nobody is looking.
    pub fn set_clock_period(&mut self, period: std::time::Duration) {
        self.clock_period = period;
    }

    /// Load since the previous call (the first call returns zeros).
    pub fn sample(&mut self) -> CpuSample {
        let mut sample = CpuSample::default();
        let now = Instant::now();
        let seconds = self
            .last
            .map(|at| now.duration_since(at).as_secs_f64())
            .unwrap_or(0.0);
        self.last = Some(now);

        self.read_core_times();
        let (total, kernel) = load_between(&self.previous, &self.current, &mut sample.per_core_pct);
        sample.total_pct = total;
        sample.kernel_pct = kernel;

        if seconds > 0.0 && self.interrupts_prev.len() == self.interrupts_now.len() {
            let interrupts: u64 = self
                .interrupts_prev
                .iter()
                .zip(&self.interrupts_now)
                .map(|(&before, &after)| u64::from(after.wrapping_sub(before)))
                .sum();
            if !self.interrupts_now.is_empty() {
                sample.interrupts_per_s = Some(interrupts as f64 / seconds);
            }
        }
        std::mem::swap(&mut self.previous, &mut self.current);
        std::mem::swap(&mut self.interrupts_prev, &mut self.interrupts_now);

        if let Some(counters) = kernel_counters(&mut self.kernel_buf) {
            if let Some(before) = self.context_switches {
                if seconds > 0.0 {
                    sample.context_switches_per_s = Some(pdh::rate_wrapping_u32(
                        before,
                        counters.context_switches,
                        seconds,
                    ));
                }
            }
            self.context_switches = Some(counters.context_switches);
        }

        // Reading the clocks is the most expensive part of this sampler (the
        // counter provider computes every per-processor counter), so it is
        // done every `clock_period` and the last values are reused in between.
        let due = self
            .clock_at
            .map(|at| now.duration_since(at) >= self.clock_period)
            .unwrap_or(true);
        if due {
            self.read_clocks(&mut sample);
            self.clock_at = Some(now);
            self.clock_last = (sample.per_core_mhz.clone(), sample.current_mhz);
        } else {
            sample.per_core_mhz = self.clock_last.0.clone();
            sample.current_mhz = self.clock_last.1;
        }

        // Cached for a few seconds: counting needs a walk over all processes.
        if let Some(counts) = system_counts() {
            sample.processes = counts.processes;
            sample.threads = counts.threads;
            sample.handles = counts.handles;
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_from_synthetic_registers() {
        assert!(decode_features(&FeatureRegs::default()).is_empty());

        let regs = FeatureRegs {
            l1_edx: (1 << 23) | (1 << 25) | (1 << 26),
            l1_ecx: (1 << 0) | (1 << 9) | (1 << 19) | (1 << 20) | (1 << 28) | (1 << 25) | (1 << 12),
            l7_ebx: (1 << 5) | (1 << 3) | (1 << 8) | (1 << 29),
            e1_edx: 1 << 29,
            ..Default::default()
        };
        let names = decode_features(&regs);
        assert_eq!(
            names,
            [
                "x86-64", "MMX", "SSE", "SSE2", "SSE3", "SSSE3", "SSE4.1", "SSE4.2", "AVX", "AVX2",
                "FMA", "AES", "SHA", "BMI1", "BMI2"
            ]
        );
        assert!(!names.iter().any(|n| n.starts_with("AVX-512")));

        let avx512 = FeatureRegs {
            l7_ebx: (1 << 16) | (1 << 17) | (1 << 30) | (1 << 31),
            l7_ecx: 1 << 11,
            ..Default::default()
        };
        assert_eq!(
            decode_features(&avx512),
            [
                "AVX-512F",
                "AVX-512DQ",
                "AVX-512BW",
                "AVX-512VL",
                "AVX-512VNNI"
            ]
        );

        // Every bit set: every known feature exactly once, no panic.
        let all = FeatureRegs {
            l1_ecx: u32::MAX,
            l1_edx: u32::MAX,
            l7_ebx: u32::MAX,
            l7_ecx: u32::MAX,
            l7_edx: u32::MAX,
            l7s1_eax: u32::MAX,
            e1_ecx: u32::MAX,
            e1_edx: u32::MAX,
        };
        let everything = decode_features(&all);
        assert_eq!(everything.len(), FEATURES.len());
        let mut unique = everything.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), everything.len(), "no feature listed twice");
        for name in ["RDRAND", "RDSEED", "POPCNT", "VT-x", "AMD-V", "F16C"] {
            assert!(everything.iter().any(|n| n == name), "{name}");
        }
    }

    #[test]
    fn signature_vendor_and_brand() {
        // Base family 6 with an extended model.
        assert_eq!(decode_signature(0x000B_06A2), (6, 0xBA, 2));
        // Base family 15 adds the extended family.
        assert_eq!(decode_signature(0x00A6_0F12), (0x19, 0x61, 2));
        // Old family 5: extended fields are ignored.
        assert_eq!(decode_signature(0x000F_0543), (5, 4, 3));
        assert_eq!(decode_signature(0), (0, 0, 0));
        assert_eq!(decode_signature(u32::MAX), (0xF + 0xFF, 0xFF, 0xF));

        let id = vendor_id(0x756E_6547, 0x4965_6E69, 0x6C65_746E);
        assert_eq!(id, "GenuineIntel");
        assert_eq!(vendor_name(&id), "Intel");
        assert_eq!(vendor_name("AuthenticAMD"), "AMD");
        assert_eq!(vendor_name("SomethingNew"), "SomethingNew");
        assert_eq!(vendor_id(0, 0, 0), "");
        assert_eq!(vendor_id(u32::MAX, u32::MAX, u32::MAX).trim(), "");

        let mut regs = [0u32; 12];
        let text = b"   Example  Core 7   1470K  \0garbage";
        for (i, chunk) in text.chunks(4).enumerate().take(12) {
            let mut four = [0u8; 4];
            four[..chunk.len()].copy_from_slice(chunk);
            regs[i] = u32::from_le_bytes(four);
        }
        assert_eq!(brand_string(&regs), "Example Core 7 1470K");
        assert_eq!(brand_string(&[0; 12]), "");
        assert_eq!(brand_string(&[u32::MAX; 12]), "");
    }

    #[test]
    fn microcode_revision() {
        assert_eq!(microcode_text(&[0, 0, 0, 0, 0x2B, 0x01, 0, 0]), "0x12B");
        assert_eq!(
            microcode_text(&[0x05, 0x02, 0x60, 0x0A, 0, 0, 0, 0]),
            "0xA600205"
        );
        assert_eq!(microcode_text(&[0x34, 0x12, 0, 0]), "0x1234");
        assert_eq!(microcode_text(&[]), "");
        assert_eq!(microcode_text(&[0; 8]), "");
        assert_eq!(microcode_text(&[1, 2]), "");
    }

    fn core_record(class: u8, masks: &[u64]) -> Vec<u8> {
        let size = 32 + masks.len() * 16;
        let mut r = vec![0u8; size];
        r[0..4].copy_from_slice(&RELATION_CORE.to_le_bytes());
        r[4..8].copy_from_slice(&(size as u32).to_le_bytes());
        r[9] = class;
        r[30..32].copy_from_slice(&(masks.len() as u16).to_le_bytes());
        for (i, mask) in masks.iter().enumerate() {
            r[32 + i * 16..40 + i * 16].copy_from_slice(&mask.to_le_bytes());
        }
        r
    }

    fn cache_record(level: u8, kind: u32, size_bytes: u32) -> Vec<u8> {
        let mut r = vec![0u8; 56];
        r[0..4].copy_from_slice(&RELATION_CACHE.to_le_bytes());
        r[4..8].copy_from_slice(&56u32.to_le_bytes());
        r[8] = level;
        r[9] = 8;
        r[10..12].copy_from_slice(&64u16.to_le_bytes());
        r[12..16].copy_from_slice(&size_bytes.to_le_bytes());
        r[16..20].copy_from_slice(&kind.to_le_bytes());
        r
    }

    fn package_record() -> Vec<u8> {
        let mut r = vec![0u8; 48];
        r[0..4].copy_from_slice(&RELATION_PACKAGE.to_le_bytes());
        r[4..8].copy_from_slice(&48u32.to_le_bytes());
        r
    }

    fn synthetic_topology() -> Vec<u8> {
        let mut bytes = Vec::new();
        // Two performance cores with two threads each, four efficiency cores.
        bytes.extend(core_record(1, &[0b11]));
        bytes.extend(core_record(1, &[0b1100]));
        for i in 0..4 {
            bytes.extend(core_record(0, &[1 << (4 + i)]));
        }
        bytes.extend(package_record());
        for _ in 0..2 {
            bytes.extend(cache_record(1, 2, 48 << 10));
            bytes.extend(cache_record(1, 1, 32 << 10));
            bytes.extend(cache_record(2, 0, 2 << 20));
        }
        bytes.extend(cache_record(3, 0, 24 << 20));
        bytes
    }

    #[test]
    fn topology_from_synthetic_records() {
        let bytes = synthetic_topology();
        let topo = parse_topology(&bytes);
        assert_eq!(topo.packages, 1);
        assert_eq!(topo.cores, 6);
        assert_eq!(topo.threads, 8);
        assert_eq!(topo.efficiency_classes, [(0, 4), (1, 2)]);
        assert_eq!(topo.hybrid_split(), Some((2, 4)));
        assert_eq!(topo.caches.len(), 4);
        let l1d = &topo.caches[0];
        assert_eq!((l1d.level, l1d.kind.as_str(), l1d.count), (1, "Data", 2));
        assert_eq!(l1d.size_bytes, 48 << 10);
        assert_eq!((l1d.line_size, l1d.associativity), (64, 8));
        assert_eq!(topo.caches[1].kind, "Instruction");
        assert_eq!((topo.caches[2].level, topo.caches[2].count), (2, 2));
        assert_eq!((topo.caches[3].level, topo.caches[3].count), (3, 1));

        // A uniform processor is not hybrid.
        let mut uniform = core_record(0, &[0b11]);
        uniform.extend(core_record(0, &[0b1100]));
        assert_eq!(parse_topology(&uniform).hybrid_split(), None);

        // A core spanning two processor groups.
        let two_groups = parse_topology(&core_record(0, &[u64::MAX, 0b111]));
        assert_eq!((two_groups.cores, two_groups.threads), (1, 67));
    }

    #[test]
    fn corrupt_topology_does_not_panic() {
        let bytes = synthetic_topology();
        for len in 0..bytes.len() {
            let topo = parse_topology(&bytes[..len]);
            assert!(topo.cores <= 6 && topo.threads <= 8);
        }
        assert_eq!(parse_topology(&[]), Topology::default());
        assert_eq!(
            parse_topology(&[0u8; 256]),
            Topology::default(),
            "size 0 stops the walk"
        );
        assert_eq!(parse_topology(&[0xFFu8; 256]), Topology::default());

        // A record that claims more group masks than it holds.
        let mut lying = core_record(0, &[0b1]);
        lying[30..32].copy_from_slice(&u16::MAX.to_le_bytes());
        let topo = parse_topology(&lying);
        assert_eq!((topo.cores, topo.threads), (1, 1));

        // A size smaller than a header, and one past the end.
        let mut tiny = core_record(0, &[0b1]);
        tiny[4..8].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(parse_topology(&tiny).cores, 0);
        let mut huge = core_record(0, &[0b1]);
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_topology(&huge).cores, 0);
    }

    #[test]
    fn load_maths() {
        let t = |idle, kernel, user| CoreTimes { idle, kernel, user };
        let mut per_core = Vec::new();

        // Core 0: 1000 ticks, 250 idle -> 75 % busy, of which 250 kernel.
        // Core 1: fully idle.
        let prev = [t(0, 0, 0), t(0, 0, 0)];
        let now = [t(250, 500, 500), t(1000, 1000, 0)];
        let (total, kernel) = load_between(&prev, &now, &mut per_core);
        assert_eq!(per_core, [75.0, 0.0]);
        assert_eq!(total, 37.5);
        assert_eq!(kernel, 12.5);

        // No time passed.
        let (total, kernel) = load_between(&now, &now, &mut per_core);
        assert_eq!((total, kernel), (0.0, 0.0));
        assert_eq!(per_core, [0.0, 0.0]);

        // Counters that went backwards never give negative or huge values.
        let (total, _) = load_between(&now, &prev, &mut per_core);
        assert_eq!(total, 0.0);

        // Idle larger than kernel + user (a torn reading) stays in range.
        let (total, kernel) = load_between(&[t(0, 0, 0)], &[t(5000, 100, 100)], &mut per_core);
        assert!((0.0..=100.0).contains(&total) && (0.0..=100.0).contains(&kernel));

        // First reading or a changed processor count: zeros, right length.
        let (total, _) = load_between(&[], &now, &mut per_core);
        assert_eq!(total, 0.0);
        assert_eq!(per_core, [0.0, 0.0]);
        let (total, _) = load_between(&[], &[], &mut per_core);
        assert_eq!(total, 0.0);
        assert!(per_core.is_empty());

        // Extremes.
        let max = [t(u64::MAX, u64::MAX, u64::MAX)];
        let (total, kernel) = load_between(&[t(0, 0, 0)], &max, &mut per_core);
        assert!((0.0..=100.0).contains(&total) && (0.0..=100.0).contains(&kernel));
    }

    #[test]
    fn core_time_records() {
        let mut bytes = Vec::new();
        for i in 0..3u64 {
            let mut record = [0u8; CORE_RECORD];
            record[0..8].copy_from_slice(&(100 * i).to_le_bytes());
            record[8..16].copy_from_slice(&(200 * i).to_le_bytes());
            record[16..24].copy_from_slice(&(300 * i).to_le_bytes());
            record[40..44].copy_from_slice(&(7 * i as u32).to_le_bytes());
            bytes.extend(record);
        }
        let (mut times, mut interrupts) = (Vec::new(), Vec::new());
        assert_eq!(parse_core_times(&bytes, &mut times, &mut interrupts), 3);
        assert_eq!(
            times[2],
            CoreTimes {
                idle: 200,
                kernel: 400,
                user: 600
            }
        );
        assert_eq!(interrupts, [0, 7, 14]);
        // A trailing partial record is ignored; appending keeps earlier groups.
        assert_eq!(
            parse_core_times(&bytes[..CORE_RECORD + 10], &mut times, &mut interrupts),
            1
        );
        assert_eq!(times.len(), 4);
        assert_eq!(parse_core_times(&[], &mut times, &mut interrupts), 0);
        assert_eq!(
            parse_core_times(&[0xFF; 47], &mut times, &mut interrupts),
            0
        );
    }

    #[test]
    fn clock_instances_and_values() {
        assert_eq!(parse_core_instance("0,0"), Some((0, 0)));
        assert_eq!(parse_core_instance("1,63"), Some((1, 63)));
        assert_eq!(parse_core_instance("0,_Total"), None);
        assert_eq!(parse_core_instance("_Total"), None);
        assert_eq!(parse_core_instance(""), None);
        assert_eq!(parse_core_instance("0,"), None);
        assert_eq!(parse_core_instance(",3"), None);
        assert_eq!(parse_core_instance("0,99999999"), None);
        assert_eq!(parse_core_instance("-1,2"), None);

        assert_eq!(effective_mhz(100.0, 3000), Some(3000.0));
        assert_eq!(effective_mhz(150.0, 3000), Some(4500.0));
        assert_eq!(effective_mhz(0.0, 3000), None);
        assert_eq!(effective_mhz(-5.0, 3000), None);
        assert_eq!(effective_mhz(f64::NAN, 3000), None);
        assert_eq!(effective_mhz(100.0, 0), None);
        assert_eq!(effective_mhz(1e9, 3000), None);
    }

    #[test]
    fn live_info_smoke() {
        let cpu = info();
        assert!(!cpu.name.is_empty());
        assert!(!cpu.vendor.is_empty());
        assert!(cpu.threads >= 1 && cpu.cores >= 1 && cpu.packages >= 1);
        assert!(cpu.threads >= cpu.cores);
        assert!(
            cpu.features.iter().any(|f| f == "SSE2"),
            "every x86-64 CPU has SSE2"
        );
        assert!(
            cpu.socket.is_empty(),
            "the socket comes from the firmware tables"
        );
        assert!(cpu.caches.iter().all(|c| c.size_bytes > 0 && c.count > 0));
        if let (Some(base), Some(max)) = (cpu.base_mhz, cpu.max_mhz) {
            assert!(max >= base);
        }
        println!(
            "cpu info: {} features, {} cache kinds, base clock {}, max clock {}, hybrid {}, microcode {}, virtualisation known {}, hypervisor {}",
            cpu.features.len(),
            cpu.caches.len(),
            if cpu.base_mhz.is_some() { "yes" } else { "no" },
            if cpu.max_mhz.is_some() { "yes" } else { "no" },
            if cpu.performance_cores.is_some() { "yes" } else { "no" },
            if cpu.microcode.is_empty() { "no" } else { "yes" },
            if cpu.virtualization.is_some() { "yes" } else { "no" },
            if cpu.hypervisor { "yes" } else { "no" },
        );
    }

    #[test]
    fn live_sampler_smoke() {
        let mut sampler = CpuSampler::new();
        let first = sampler.sample();
        assert_eq!(first.total_pct, 0.0, "the first reading has no interval");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let second = sampler.sample();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let started = Instant::now();
        let mut s = CpuSample::default();
        for _ in 0..20 {
            s = sampler.sample();
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;

        for sample in [&second, &s] {
            assert!((0.0..=100.0).contains(&sample.total_pct));
            assert!((0.0..=100.0).contains(&sample.kernel_pct));
            assert!(sample.kernel_pct <= sample.total_pct + 0.01);
            assert!(!sample.per_core_pct.is_empty());
            assert!(sample
                .per_core_pct
                .iter()
                .all(|p| (0.0..=100.0).contains(p)));
            assert!(sample.temperature_c.is_none());
        }
        let threads = info().threads as usize;
        assert_eq!(second.per_core_pct.len(), threads);
        let mean = second.per_core_pct.iter().sum::<f32>() / second.per_core_pct.len() as f32;
        assert!(
            (mean - second.total_pct).abs() < 15.0,
            "total and per-core average disagree: {} vs {mean}",
            second.total_pct
        );
        assert!(second.processes > 0 && second.threads > 0 && second.handles > 0);
        assert!(second.context_switches_per_s.is_some_and(|r| r >= 0.0));
        assert!(second.interrupts_per_s.is_some_and(|r| r >= 0.0));
        if let Some(mhz) = second.current_mhz {
            assert!((100.0..=20_000.0).contains(&mhz));
            assert_eq!(second.per_core_mhz.len(), threads);
        }
        println!(
            "cpu sample: clock counter {}, context switches {}, interrupts {}, total in range yes, avg {ms:.3} ms per call",
            if second.current_mhz.is_some() { "yes" } else { "no" },
            if second.context_switches_per_s.is_some() { "yes" } else { "no" },
            if second.interrupts_per_s.is_some() { "yes" } else { "no" },
        );
    }
}
