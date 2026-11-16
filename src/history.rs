//! Graph history: one [`Series`] per metric, kept in three resolutions so that
//! 24 hours of history for a hundred metrics stays within a few megabytes.
//!
//! * raw points for the most recent minutes ([`RAW_CAP`] points);
//! * 5-second buckets for two hours;
//! * 1-minute buckets for 24 hours.
//!
//! Time is a plain `f64` number of seconds on any monotonic clock the caller
//! likes (the sampler uses seconds since it started; demo data starts in the
//! negative to pre-fill the graphs).

use std::collections::{HashMap, VecDeque};

/// Raw points kept per series: ten minutes at the fastest update interval (0.5 s).
pub const RAW_CAP: usize = 1200;
/// Windows up to this many seconds are drawn from raw points.
pub const RAW_WINDOW_S: f64 = 600.0;
/// Windows up to this many seconds are drawn from the fine buckets.
pub const FINE_WINDOW_S: f64 = 7200.0;
const FINE_WIDTH_S: f64 = 5.0;
const COARSE_WIDTH_S: f64 = 60.0;
const TIER_CAP: usize = 1440;

/// The history windows offered in the user interface: label and seconds.
pub const WINDOWS: [(&str, f64); 7] = [
    ("1 min", 60.0),
    ("5 min", 300.0),
    ("10 min", 600.0),
    ("1 h", 3600.0),
    ("2 h", 7200.0),
    ("6 h", 21_600.0),
    ("24 h", 86_400.0),
];

/// One point of a graph. For raw points `min == max == v`; for buckets `v` is
/// the average and `min` / `max` the extremes inside the bucket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Seconds relative to "now": zero or negative.
    pub t: f64,
    pub v: f32,
    pub min: f32,
    pub max: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub min: f32,
    pub max: f32,
    pub avg: f32,
    /// The newest value in the window.
    pub last: f32,
    /// Number of samples that went into the figures.
    pub count: u64,
}

#[derive(Clone, Copy, Debug)]
struct Agg {
    min: f32,
    max: f32,
    sum: f64,
    n: u32,
}

impl Agg {
    const EMPTY: Agg = Agg {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
        sum: 0.0,
        n: 0,
    };
    fn add(&mut self, v: f32) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += f64::from(v);
        self.n += 1;
    }
}

#[derive(Clone, Debug)]
struct Tier {
    width: f64,
    /// Bucket index (`floor(t / width)`) of `buckets[0]`.
    first: i64,
    buckets: VecDeque<Agg>,
}

impl Tier {
    fn new(width: f64) -> Tier {
        Tier {
            width,
            first: 0,
            buckets: VecDeque::new(),
        }
    }

    fn push(&mut self, t: f64, v: f32) {
        let idx = (t / self.width).floor() as i64;
        if self.buckets.is_empty() {
            self.first = idx;
            self.buckets.push_back(Agg::EMPTY);
        }
        let mut last = self.first + self.buckets.len() as i64 - 1;
        if idx > last {
            if idx - last > TIER_CAP as i64 {
                // A gap longer than the whole tier: start again.
                self.buckets.clear();
                self.first = idx;
                self.buckets.push_back(Agg::EMPTY);
            } else {
                while last < idx {
                    self.buckets.push_back(Agg::EMPTY);
                    last += 1;
                }
            }
            while self.buckets.len() > TIER_CAP {
                self.buckets.pop_front();
                self.first += 1;
            }
        }
        // Samples that arrive late (clock went backwards) fall into the newest bucket.
        if let Some(b) = self.buckets.back_mut() {
            b.add(v);
        }
    }

    fn visit(&self, now: f64, window_s: f64, mut f: impl FnMut(f64, &Agg)) {
        for (i, b) in self.buckets.iter().enumerate() {
            if b.n == 0 {
                continue;
            }
            let start = (self.first + i as i64) as f64 * self.width;
            let center = start + self.width / 2.0;
            // A bucket belongs to the window if any part of it does.
            if start + self.width < now - window_s || start > now {
                continue;
            }
            f(center.min(now), b);
        }
    }
}

/// The history of one metric.
#[derive(Clone, Debug)]
pub struct Series {
    raw_t: VecDeque<f64>,
    raw_v: VecDeque<f32>,
    fine: Tier,
    coarse: Tier,
}

impl Default for Series {
    fn default() -> Self {
        Series::new()
    }
}

impl Series {
    pub fn new() -> Series {
        Series {
            raw_t: VecDeque::new(),
            raw_v: VecDeque::new(),
            fine: Tier::new(FINE_WIDTH_S),
            coarse: Tier::new(COARSE_WIDTH_S),
        }
    }

    /// Adds a sample. Values that are not finite are ignored.
    pub fn push(&mut self, t: f64, v: f32) {
        if !v.is_finite() || !t.is_finite() {
            return;
        }
        self.raw_t.push_back(t);
        self.raw_v.push_back(v);
        while self.raw_t.len() > RAW_CAP {
            self.raw_t.pop_front();
            self.raw_v.pop_front();
        }
        self.fine.push(t, v);
        self.coarse.push(t, v);
    }

    /// The newest value.
    pub fn last(&self) -> Option<f32> {
        self.raw_v.back().copied()
    }

    /// Number of raw points held.
    pub fn len(&self) -> usize {
        self.raw_v.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw_v.is_empty()
    }

    /// The points inside `now - window_s ..= now`, oldest first, at the
    /// resolution that suits the window.
    pub fn points(&self, now: f64, window_s: f64) -> Vec<Point> {
        let mut out = Vec::new();
        if window_s <= RAW_WINDOW_S {
            for (t, v) in self.raw_t.iter().zip(self.raw_v.iter()) {
                if *t >= now - window_s && *t <= now {
                    out.push(Point {
                        t: t - now,
                        v: *v,
                        min: *v,
                        max: *v,
                    });
                }
            }
        } else {
            let tier = if window_s <= FINE_WINDOW_S {
                &self.fine
            } else {
                &self.coarse
            };
            tier.visit(now, window_s, |t, b| {
                out.push(Point {
                    t: t - now,
                    v: (b.sum / f64::from(b.n)) as f32,
                    min: b.min,
                    max: b.max,
                });
            });
        }
        out
    }

    /// Minimum, maximum and average over the window; `None` when it holds no samples.
    pub fn stats(&self, now: f64, window_s: f64) -> Option<Stats> {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum = 0.0_f64;
        let mut count = 0_u64;
        let mut last = None;
        if window_s <= RAW_WINDOW_S {
            for (t, v) in self.raw_t.iter().zip(self.raw_v.iter()) {
                if *t >= now - window_s && *t <= now {
                    min = min.min(*v);
                    max = max.max(*v);
                    sum += f64::from(*v);
                    count += 1;
                    last = Some(*v);
                }
            }
        } else {
            let tier = if window_s <= FINE_WINDOW_S {
                &self.fine
            } else {
                &self.coarse
            };
            tier.visit(now, window_s, |_, b| {
                min = min.min(b.min);
                max = max.max(b.max);
                sum += b.sum;
                count += u64::from(b.n);
                last = Some((b.sum / f64::from(b.n)) as f32);
            });
            // The newest raw value is a better "last" than a bucket average.
            if let (Some(t), Some(v)) = (self.raw_t.back(), self.raw_v.back()) {
                if *t <= now && *t >= now - window_s && last.is_some() {
                    last = Some(*v);
                }
            }
        }
        let last = last?;
        Some(Stats {
            min,
            max,
            avg: (sum / count as f64) as f32,
            last,
            count,
        })
    }
}

/// All series, by key (see [`keys`]).
#[derive(Clone, Debug, Default)]
pub struct History {
    series: HashMap<String, Series>,
    /// Time of the newest sample pushed.
    pub now: f64,
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    pub fn push(&mut self, key: &str, t: f64, v: f32) {
        if t > self.now || self.series.is_empty() {
            self.now = t;
        }
        match self.series.get_mut(key) {
            Some(s) => s.push(t, v),
            None => {
                let mut s = Series::new();
                s.push(t, v);
                self.series.insert(key.to_string(), s);
            }
        }
    }

    /// Like [`History::push`], for readings that may be missing.
    pub fn push_opt(&mut self, key: &str, t: f64, v: Option<f32>) {
        if let Some(v) = v {
            self.push(key, t, v);
        }
    }

    pub fn get(&self, key: &str) -> Option<&Series> {
        self.series.get(key)
    }

    pub fn points(&self, key: &str, now: f64, window_s: f64) -> Vec<Point> {
        self.get(key)
            .map(|s| s.points(now, window_s))
            .unwrap_or_default()
    }

    pub fn stats(&self, key: &str, now: f64, window_s: f64) -> Option<Stats> {
        self.get(key).and_then(|s| s.stats(now, window_s))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.series.keys().map(|k| k.as_str())
    }

    pub fn series_count(&self) -> usize {
        self.series.len()
    }

    /// Rough memory held, for the overhead figures in Settings.
    pub fn approx_bytes(&self) -> usize {
        self.series
            .iter()
            .map(|(k, s)| {
                k.len()
                    + s.raw_t.len() * 12
                    + (s.fine.buckets.len() + s.coarse.buckets.len()) * std::mem::size_of::<Agg>()
            })
            .sum()
    }
}

/// The names under which the sampler stores each metric. The user interface
/// uses the same functions, so the two cannot drift apart.
pub mod keys {
    pub const CPU_TOTAL: &str = "cpu.total";
    pub const CPU_KERNEL: &str = "cpu.kernel";
    pub const CPU_MHZ: &str = "cpu.mhz";
    pub const CPU_TEMP: &str = "cpu.temp";
    pub const MEM_USED_PCT: &str = "mem.used_pct";
    pub const MEM_COMMIT_PCT: &str = "mem.commit_pct";
    pub const MEM_CACHED_PCT: &str = "mem.cached_pct";
    pub const MEM_PAGEFILE_PCT: &str = "mem.pagefile_pct";
    /// Bytes per second over all drives.
    pub const DISK_READ: &str = "disk.read";
    pub const DISK_WRITE: &str = "disk.write";
    /// Bytes per second over all physical network adapters.
    pub const NET_RX: &str = "net.rx";
    pub const NET_TX: &str = "net.tx";
    pub const BATTERY_PCT: &str = "battery.pct";
    /// Watts: positive while charging, negative while discharging.
    pub const BATTERY_RATE_W: &str = "battery.rate_w";
    /// This program's own processor use, percent of the whole machine.
    pub const SELF_CPU: &str = "self.cpu";

    pub fn cpu_core(index: usize) -> String {
        format!("cpu.core.{index}")
    }
    /// `id` is `GpuInfo::id`.
    pub fn gpu_load(id: &str) -> String {
        format!("gpu.{id}.load")
    }
    /// Dedicated memory in use, percent of the adapter's dedicated memory.
    pub fn gpu_mem_pct(id: &str) -> String {
        format!("gpu.{id}.mem_pct")
    }
    pub fn gpu_temp(id: &str) -> String {
        format!("gpu.{id}.temp")
    }
    pub fn gpu_engine(id: &str, engine: &str) -> String {
        format!("gpu.{id}.engine.{engine}")
    }
    /// `index` is `DriveInfo::index`. Bytes per second.
    pub fn disk_read(index: u32) -> String {
        format!("disk.{index}.read")
    }
    pub fn disk_write(index: u32) -> String {
        format!("disk.{index}.write")
    }
    pub fn disk_active(index: u32) -> String {
        format!("disk.{index}.active")
    }
    pub fn disk_temp(index: u32) -> String {
        format!("disk.{index}.temp")
    }
    /// Per adapter, by friendly name. Bytes per second.
    pub fn net_rx(name: &str) -> String {
        format!("net.{name}.rx")
    }
    pub fn net_tx(name: &str) -> String {
        format!("net.{name}.tx")
    }
    /// Any line of the sensors list, by group and name.
    pub fn sensor(group: &str, name: &str) -> String {
        format!("sensor.{group}.{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_ring_is_bounded_and_ordered() {
        let mut s = Series::new();
        for i in 0..(RAW_CAP + 500) {
            s.push(i as f64, i as f32);
        }
        assert_eq!(s.len(), RAW_CAP);
        assert_eq!(s.last(), Some((RAW_CAP + 499) as f32));
        let now = (RAW_CAP + 499) as f64;
        let pts = s.points(now, 60.0);
        assert_eq!(pts.len(), 61);
        assert_eq!(pts.first().unwrap().t, -60.0);
        assert_eq!(pts.last().unwrap().t, 0.0);
        assert!(pts.windows(2).all(|w| w[0].t < w[1].t));
    }

    #[test]
    fn statistics_over_a_window() {
        let mut s = Series::new();
        for (i, v) in [10.0, 20.0, 30.0, 40.0].iter().enumerate() {
            s.push(i as f64, *v);
        }
        let st = s.stats(3.0, 60.0).unwrap();
        assert_eq!(
            (st.min, st.max, st.avg, st.last, st.count),
            (10.0, 40.0, 25.0, 40.0, 4)
        );
        // Only the last two seconds.
        let st = s.stats(3.0, 1.0).unwrap();
        assert_eq!((st.min, st.max, st.avg, st.count), (30.0, 40.0, 35.0, 2));
        // A window with nothing in it.
        assert!(s.stats(1000.0, 5.0).is_none());
        assert!(Series::new().stats(0.0, 60.0).is_none());
    }

    #[test]
    fn pausing_hides_newer_points() {
        let mut s = Series::new();
        for i in 0..100 {
            s.push(i as f64, i as f32);
        }
        // "Now" frozen at t = 50: nothing newer may show.
        let pts = s.points(50.0, 10.0);
        assert_eq!(pts.len(), 11);
        assert_eq!(pts.last().unwrap().v, 50.0);
        assert_eq!(s.stats(50.0, 10.0).unwrap().max, 50.0);
    }

    #[test]
    fn non_finite_values_are_dropped() {
        let mut s = Series::new();
        s.push(0.0, f32::NAN);
        s.push(1.0, f32::INFINITY);
        s.push(f64::NAN, 1.0);
        assert!(s.is_empty());
        s.push(2.0, 5.0);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn long_windows_use_buckets() {
        let mut s = Series::new();
        // Two hours at one sample per second, a sawtooth 0..59.
        for i in 0..7200 {
            s.push(i as f64, (i % 60) as f32);
        }
        let now = 7199.0;
        // One hour: 5-second buckets.
        let pts = s.points(now, 3600.0);
        assert!(pts.len() >= 720 && pts.len() <= 722, "{}", pts.len());
        assert!(pts.iter().all(|p| p.min <= p.v && p.v <= p.max));
        assert!(pts.iter().all(|p| p.t <= 0.0 && p.t >= -3605.0));
        // Six hours: 1-minute buckets, each a full sawtooth.
        let pts = s.points(now, 21_600.0);
        assert_eq!(pts.len(), 120);
        let p = pts[10];
        assert_eq!((p.min, p.max), (0.0, 59.0));
        assert!((p.v - 29.5).abs() < 0.01);
        let st = s.stats(now, 21_600.0).unwrap();
        assert_eq!((st.min, st.max, st.count), (0.0, 59.0, 7200));
        assert!((st.avg - 29.5).abs() < 0.01);
        assert_eq!(st.last, 59.0);
    }

    #[test]
    fn tiers_are_bounded_and_survive_gaps() {
        let mut s = Series::new();
        // 30 hours at one sample every 10 seconds.
        for i in 0..(30 * 360) {
            s.push(i as f64 * 10.0, 1.0);
        }
        assert!(s.fine.buckets.len() <= TIER_CAP);
        assert!(s.coarse.buckets.len() <= TIER_CAP);
        let now = (30 * 360 - 1) as f64 * 10.0;
        assert!(s.points(now, 86_400.0).len() <= TIER_CAP);
        // The machine sleeps for a week.
        let later = now + 7.0 * 86_400.0;
        s.push(later, 2.0);
        assert!(s.coarse.buckets.len() <= TIER_CAP);
        let pts = s.points(later, 86_400.0);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].v, 2.0);
        // Time going backwards does not panic or grow anything.
        s.push(0.0, 3.0);
        assert!(s.coarse.buckets.len() <= TIER_CAP);
    }

    #[test]
    fn history_by_key() {
        let mut h = History::new();
        h.push(keys::CPU_TOTAL, -5.0, 10.0);
        h.push(keys::CPU_TOTAL, 0.0, 30.0);
        h.push_opt(keys::CPU_TEMP, 0.0, None);
        h.push(&keys::cpu_core(3), 0.0, 7.0);
        assert_eq!(h.now, 0.0);
        assert_eq!(h.series_count(), 2);
        assert!(h.get(keys::CPU_TEMP).is_none());
        assert_eq!(h.stats(keys::CPU_TOTAL, 0.0, 60.0).unwrap().avg, 20.0);
        assert_eq!(h.points("cpu.core.3", 0.0, 60.0).len(), 1);
        assert!(h.points("nope", 0.0, 60.0).is_empty());
        assert!(h.approx_bytes() > 0);
    }
}
