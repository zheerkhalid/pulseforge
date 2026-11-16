//! Thin helpers over the PDH performance-counter API, plus the small counter
//! maths (rates from running totals) that the live collectors share.

use crate::util::wide;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_MORE_DATA,
};

/// Do not clamp percentages at 100 (clock ratios above the base clock are real).
const PDH_FMT_NOCAP100: u32 = 0x8000;
/// `PDH_INVALID_HANDLE`, returned by this wrapper for a counter of another query.
const PDH_INVALID_HANDLE: u32 = 0xC000_0BBC;
/// Status codes of a single value that mean "this number is usable".
const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_CSTATUS_NEW_DATA: u32 = 1;
/// Upper bound for the item buffer (64 MB): a larger request is treated as a failure.
const MAX_BUFFER_BYTES: usize = 64 << 20;

/// One counter that was added to a [`Query`]. Only valid with that query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counter(isize);

/// An open performance-counter query. Counters are added once, then
/// [`Query::collect`] takes a reading and the `read_*` functions format it.
/// The query and all its counters are closed when it is dropped.
pub struct Query {
    handle: isize,
    counters: Vec<isize>,
    /// Item buffer, reused between readings. `u64` keeps it 8-byte aligned.
    buf: Vec<u64>,
    /// Scratch space for one instance name.
    name: String,
}

impl Query {
    /// Opens a query on the live counters of this machine.
    pub fn open() -> Result<Query, String> {
        let mut handle = 0isize;
        // SAFETY: a null data source means "live data"; `handle` is a valid out-pointer.
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) };
        if status != 0 || handle == 0 {
            return Err(format!("could not open a counter query (0x{status:08X})"));
        }
        Ok(Query {
            handle,
            counters: Vec::new(),
            buf: Vec::new(),
            name: String::new(),
        })
    }

    /// Adds a counter by its English path, for example
    /// `\Processor Information(*)\% Processor Performance`. English paths work
    /// on every display language.
    pub fn add(&mut self, path: &str) -> Result<Counter, String> {
        let path_w = wide(path);
        let mut counter = 0isize;
        // SAFETY: `path_w` is NUL-terminated and outlives the call; `counter` is a valid out-pointer.
        let status =
            unsafe { PdhAddEnglishCounterW(self.handle, PCWSTR(path_w.as_ptr()), 0, &mut counter) };
        if status != 0 || counter == 0 {
            return Err(format!("counter not available (0x{status:08X}): {path}"));
        }
        self.counters.push(counter);
        Ok(Counter(counter))
    }

    /// Takes one reading of every counter. Rate counters need two readings
    /// before they format to a value.
    pub fn collect(&mut self) -> bool {
        // SAFETY: the handle is open for as long as `self` lives.
        unsafe { PdhCollectQueryData(self.handle) == 0 }
    }

    /// The value of a single-instance counter from the latest reading.
    pub fn read_value(&self, counter: Counter) -> Option<f64> {
        if !self.counters.contains(&counter.0) {
            return None;
        }
        let mut value = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: the counter belongs to this open query; `value` is a valid out-pointer.
        let status = unsafe {
            PdhGetFormattedCounterValue(
                counter.0,
                PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100),
                None,
                &mut value,
            )
        };
        if status != 0 || !status_ok(value.CStatus) {
            return None;
        }
        // SAFETY: PDH_FMT_DOUBLE was requested, so the union holds a double.
        let v = unsafe { value.Anonymous.doubleValue };
        v.is_finite().then_some(v)
    }

    /// Calls `each(instance name, value)` for every instance of a wildcard
    /// counter in the latest reading and returns how many were delivered.
    /// Instances without a usable value are skipped. On failure the PDH status
    /// code is returned (the usual one right after start is "no data yet").
    pub fn read_array(
        &mut self,
        counter: Counter,
        mut each: impl FnMut(&str, f64),
    ) -> Result<usize, u32> {
        if !self.counters.contains(&counter.0) {
            return Err(PDH_INVALID_HANDLE);
        }
        const ITEM: usize = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let format = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100);
        let mut count = 0u32;
        let mut done = false;
        for _ in 0..4 {
            let mut bytes = (self.buf.len() * 8).min(u32::MAX as usize) as u32;
            let ptr = if self.buf.is_empty() {
                None
            } else {
                Some(self.buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>())
            };
            count = 0;
            // SAFETY: `ptr` points to `bytes` writable, 8-byte aligned bytes (or is
            // null with a size of zero, which asks for the needed size).
            let status = unsafe {
                PdhGetFormattedCounterArrayW(counter.0, format, &mut bytes, &mut count, ptr)
            };
            if status == PDH_MORE_DATA {
                let wanted = bytes as usize;
                if wanted > MAX_BUFFER_BYTES {
                    return Err(status);
                }
                // A quarter more than asked: the instance list grows between calls.
                let words = (wanted + wanted / 4) / 8 + 16;
                if words > self.buf.len() {
                    self.buf.resize(words, 0);
                }
                continue;
            }
            if status != 0 {
                return Err(status);
            }
            done = true;
            break;
        }
        if !done {
            return Err(PDH_MORE_DATA);
        }

        let Query { buf, name, .. } = self;
        let total_bytes = buf.len() * 8;
        let count = (count as usize).min(total_bytes / ITEM);
        let base = buf.as_ptr() as usize;
        let mut delivered = 0;
        for i in 0..count {
            // SAFETY: `i * ITEM + ITEM <= total_bytes` by the clamp above and the
            // buffer is 8-byte aligned, which is the alignment of the item type.
            let item = unsafe { &*buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>().add(i) };
            if !status_ok(item.FmtValue.CStatus) {
                continue;
            }
            // SAFETY: PDH_FMT_DOUBLE was requested, so the union holds a double.
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            if !value.is_finite() {
                continue;
            }
            if !read_name(item.szName, base, total_bytes, name) {
                continue;
            }
            each(name.as_str(), value);
            delivered += 1;
        }
        Ok(delivered)
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `open` and is closed exactly once;
        // closing the query also releases its counters.
        unsafe {
            let _ = PdhCloseQuery(self.handle);
        }
    }
}

fn status_ok(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

/// Copies the instance name that `ptr` points to into `out`. PDH stores the
/// names inside the same buffer as the items; anything else is rejected.
fn read_name(ptr: PWSTR, base: usize, total_bytes: usize, out: &mut String) -> bool {
    out.clear();
    let addr = ptr.0 as usize;
    if addr < base || addr >= base + total_bytes || !(addr - base).is_multiple_of(2) {
        return false;
    }
    let max_units = (base + total_bytes - addr) / 2;
    // SAFETY: the range `addr .. addr + max_units * 2` lies inside the buffer
    // (checked above) and `addr` is 2-byte aligned relative to an 8-byte aligned base.
    let units = unsafe { std::slice::from_raw_parts(addr as *const u16, max_units) };
    let len = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    out.extend(char::decode_utf16(units[..len].iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')));
    true
}

// ----------------------------------------------------------------------------------
// Counter maths
// ----------------------------------------------------------------------------------

/// Units per second between two readings of a running 64-bit total. A total
/// that went backwards (the device was reset) or a non-positive interval
/// gives zero rather than a spike.
pub fn rate(prev: u64, now: u64, seconds: f64) -> f64 {
    if now < prev || !seconds.is_finite() || seconds <= 0.0 {
        return 0.0;
    }
    (now - prev) as f64 / seconds
}

/// Like [`rate`] for a 32-bit total that wraps around at 2^32.
pub fn rate_wrapping_u32(prev: u32, now: u32, seconds: f64) -> f64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0.0;
    }
    f64::from(now.wrapping_sub(prev)) / seconds
}

/// Reads a little-endian `u32` at `offset`, `None` when out of bounds.
pub fn le_u32(buf: &[u8], offset: usize) -> Option<u32> {
    let bytes = buf.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// Reads a little-endian `u64` at `offset`, `None` when out of bounds.
pub fn le_u64(buf: &[u8], offset: usize) -> Option<u64> {
    let bytes = buf.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// Reads a little-endian `u16` at `offset`, `None` when out of bounds.
pub fn le_u16(buf: &[u8], offset: usize) -> Option<u16> {
    let bytes = buf.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_handle_reset_wrap_and_zero_time() {
        assert_eq!(rate(1000, 3000, 2.0), 1000.0);
        assert_eq!(rate(3000, 1000, 1.0), 0.0, "a reset total is not a rate");
        assert_eq!(rate(0, 100, 0.0), 0.0);
        assert_eq!(rate(0, 100, -1.0), 0.0);
        assert_eq!(rate(0, 100, f64::NAN), 0.0);
        assert_eq!(rate(u64::MAX - 1, u64::MAX, 1.0), 1.0);
        assert_eq!(rate_wrapping_u32(u32::MAX - 9, 10, 1.0), 20.0);
        assert_eq!(rate_wrapping_u32(5, 5, 1.0), 0.0);
        assert_eq!(rate_wrapping_u32(0, 500, 0.5), 1000.0);
        assert_eq!(rate_wrapping_u32(0, 500, 0.0), 0.0);
        assert_eq!(rate_wrapping_u32(0, 500, f64::INFINITY), 0.0);
    }

    #[test]
    fn little_endian_readers_check_bounds() {
        let buf = [1u8, 0, 0, 0, 2, 0, 0, 0, 3];
        assert_eq!(le_u32(&buf, 0), Some(1));
        assert_eq!(le_u32(&buf, 4), Some(2));
        assert_eq!(le_u32(&buf, 6), None);
        assert_eq!(le_u64(&buf, 0), Some(0x0000_0002_0000_0001));
        assert_eq!(le_u64(&buf, 2), None);
        assert_eq!(le_u16(&buf, 8), None);
        assert_eq!(le_u16(&buf, 7), Some(0x0300));
        assert_eq!(le_u32(&buf, usize::MAX), None);
        assert_eq!(le_u32(&[], 0), None);
    }

    #[test]
    fn names_outside_the_buffer_are_rejected() {
        let buf: Vec<u64> = vec![0x0042_0041, 0];
        let base = buf.as_ptr() as usize;
        let mut out = String::from("old");
        assert!(read_name(PWSTR(base as *mut u16), base, 16, &mut out));
        assert_eq!(out, "AB");
        assert!(!read_name(PWSTR(std::ptr::null_mut()), base, 16, &mut out));
        assert!(!read_name(
            PWSTR((base + 16) as *mut u16),
            base,
            16,
            &mut out
        ));
        assert!(!read_name(
            PWSTR((base + 1) as *mut u16),
            base,
            16,
            &mut out
        ));
        assert!(out.is_empty());
    }

    #[test]
    fn live_query_smoke() {
        let Ok(mut query) = Query::open() else {
            println!("pdh: query could not be opened");
            return;
        };
        assert!(query.add(r"\No Such Object(*)\No Such Counter").is_err());
        let Ok(counter) = query.add(r"\Processor Information(*)\% Processor Performance") else {
            println!("pdh: processor counter set not present");
            return;
        };
        query.collect();
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(query.collect());
        let mut seen = 0;
        let got = query.read_array(counter, |name, value| {
            assert!(!name.is_empty());
            assert!(value.is_finite());
            seen += 1;
        });
        println!("pdh: instances read: {:?}, delivered {seen}", got.is_ok());
        // A counter of another query must be refused, not passed to the API.
        let mut other = Query::open().expect("second query");
        assert!(other.read_array(counter, |_, _| {}).is_err());
        assert!(other.read_value(counter).is_none());
    }
}
