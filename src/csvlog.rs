//! Logging of the main readings to CSV files, one file per day.

use crate::model::{Sample, Slow};
use crate::util;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Column names, in the order [`row`] writes them.
pub const COLUMNS: [&str; 20] = [
    "time",
    "cpu_pct",
    "cpu_temp_c",
    "cpu_mhz",
    "mem_used_pct",
    "mem_used_mb",
    "commit_pct",
    "gpu_pct",
    "gpu_temp_c",
    "gpu_mem_mb",
    "disk_read_bps",
    "disk_write_bps",
    "disk_temp_max_c",
    "net_down_bps",
    "net_up_bps",
    "battery_pct",
    "battery_rate_w",
    "processes",
    "threads",
    "uptime_s",
];

pub fn header() -> String {
    COLUMNS.join(",")
}

fn opt(v: Option<f64>, decimals: usize) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:.decimals$}"),
        _ => String::new(),
    }
}

/// One CSV line (without the line break). Missing readings are empty cells.
pub fn row(sample: &Sample, slow: &Slow) -> String {
    let (disk_r, disk_w) = sample.disk_totals();
    let (rx, tx) = sample.net_totals();
    let gpu_mem: u64 = sample.gpus.iter().map(|g| g.dedicated_used).sum();
    let disk_temp = slow
        .drives
        .iter()
        .filter_map(|d| d.temperature_c)
        .fold(None, |a: Option<f32>, b| Some(a.map_or(b, |a| a.max(b))));
    let battery = sample.battery.as_ref();
    let cells = [
        util::local_datetime(sample.time),
        format!("{:.1}", sample.cpu.total_pct),
        opt(sample.cpu.temperature_c.map(f64::from), 1),
        opt(sample.cpu.current_mhz.map(f64::from), 0),
        format!("{:.1}", sample.memory.used_pct()),
        format!("{}", sample.memory.used / (1024 * 1024)),
        format!("{:.1}", sample.memory.commit_pct()),
        opt(sample.gpu_load().map(f64::from), 1),
        opt(sample.gpu_temp().map(f64::from), 1),
        if sample.gpus.is_empty() {
            String::new()
        } else {
            format!("{}", gpu_mem / (1024 * 1024))
        },
        format!("{disk_r:.0}"),
        format!("{disk_w:.0}"),
        opt(disk_temp.map(f64::from), 1),
        format!("{rx:.0}"),
        format!("{tx:.0}"),
        opt(battery.map(|b| f64::from(b.percent)), 1),
        opt(
            battery
                .and_then(|b| b.rate_mw)
                .map(|mw| f64::from(mw) / 1000.0),
            2,
        ),
        sample.cpu.processes.to_string(),
        sample.cpu.threads.to_string(),
        sample.uptime_s.to_string(),
    ];
    cells.join(",")
}

/// The file for the day of `unix`: `pulseforge-20261116.csv`.
pub fn file_name(unix: u64) -> String {
    let (y, m, d, ..) = util::local_parts(unix);
    format!("pulseforge-{y:04}{m:02}{d:02}.csv")
}

/// Appends rows to the file of the day, starting a new file at midnight.
pub struct Logger {
    dir: PathBuf,
    interval_s: u32,
    file: Option<(String, File)>,
    last: Option<u64>,
}

impl Logger {
    pub fn new(dir: &Path, interval_s: u32) -> Logger {
        Logger {
            dir: dir.to_path_buf(),
            interval_s: interval_s.max(1),
            file: None,
            last: None,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn interval_s(&self) -> u32 {
        self.interval_s
    }

    /// The file being written, once a row has gone out.
    pub fn current_file(&self) -> Option<PathBuf> {
        self.file.as_ref().map(|(name, _)| self.dir.join(name))
    }

    /// Writes a row if the interval since the previous one has passed.
    /// Returns whether a row was written.
    pub fn log(&mut self, sample: &Sample, slow: &Slow) -> Result<bool, String> {
        if let Some(last) = self.last {
            if sample.time < last.saturating_add(u64::from(self.interval_s)) && sample.time >= last
            {
                return Ok(false);
            }
        }
        let name = file_name(sample.time);
        if self.file.as_ref().map(|(n, _)| n != &name).unwrap_or(true) {
            std::fs::create_dir_all(&self.dir)
                .map_err(|e| format!("Could not create the log folder: {e}"))?;
            let path = self.dir.join(&name);
            let is_new = std::fs::metadata(&path)
                .map(|m| m.len() == 0)
                .unwrap_or(true);
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("Could not open the log file: {e}"))?;
            if is_new {
                writeln!(file, "{}", header()).map_err(|e| e.to_string())?;
            }
            self.file = Some((name, file));
        }
        if let Some((_, file)) = &mut self.file {
            writeln!(file, "{}", row(sample, slow))
                .map_err(|e| format!("Could not write to the log file: {e}"))?;
        }
        self.last = Some(sample.time);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BatteryInfo, DriveInfo, GpuSample};

    #[test]
    fn rows_match_the_header() {
        let empty = row(&Sample::default(), &Slow::default());
        assert_eq!(empty.split(',').count(), COLUMNS.len());
        assert_eq!(header().split(',').count(), COLUMNS.len());
        let mut s = Sample::default();
        s.cpu.total_pct = 12.34;
        s.cpu.temperature_c = Some(51.26);
        s.memory.total = 16 << 30;
        s.memory.used = 4 << 30;
        s.gpus.push(GpuSample {
            load_pct: 30.0,
            dedicated_used: 512 << 20,
            ..Default::default()
        });
        s.battery = Some(BatteryInfo {
            percent: 80.0,
            rate_mw: Some(-12_500),
            ..Default::default()
        });
        let slow = Slow {
            drives: vec![
                DriveInfo {
                    temperature_c: Some(35.0),
                    ..Default::default()
                },
                DriveInfo {
                    temperature_c: Some(44.0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let cells: Vec<String> = row(&s, &slow).split(',').map(String::from).collect();
        assert_eq!(cells.len(), COLUMNS.len());
        let get = |name: &str| cells[COLUMNS.iter().position(|c| *c == name).unwrap()].as_str();
        assert_eq!(get("cpu_pct"), "12.3");
        assert_eq!(get("cpu_temp_c"), "51.3");
        assert_eq!(get("cpu_mhz"), "");
        assert_eq!(get("mem_used_pct"), "25.0");
        assert_eq!(get("mem_used_mb"), "4096");
        assert_eq!(get("gpu_pct"), "30.0");
        assert_eq!(get("gpu_temp_c"), "");
        assert_eq!(get("gpu_mem_mb"), "512");
        assert_eq!(get("disk_temp_max_c"), "44.0");
        assert_eq!(get("battery_rate_w"), "-12.50");
    }

    #[test]
    fn logger_respects_interval_and_writes_one_header() {
        let dir = std::env::temp_dir().join(format!("pulseforge-csv-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut logger = Logger::new(&dir, 5);
        // Midday UTC, so that no time zone puts the test across midnight.
        let mut s = Sample {
            time: 1_800_014_400,
            ..Default::default()
        };
        let slow = Slow::default();
        assert!(logger.log(&s, &slow).unwrap());
        s.time += 2;
        assert!(!logger.log(&s, &slow).unwrap());
        s.time += 3;
        assert!(logger.log(&s, &slow).unwrap());
        let path = logger.current_file().unwrap();
        drop(logger);
        // A second logger appends to the same file without a second header.
        let mut logger = Logger::new(&dir, 1);
        s.time += 1;
        assert!(logger.log(&s, &slow).unwrap());
        drop(logger);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], header());
        assert!(lines[1..]
            .iter()
            .all(|l| l.split(',').count() == COLUMNS.len()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unwritable_folder_is_an_error_not_a_panic() {
        let mut logger = Logger::new(Path::new("Z:\\no\\such\\<folder>"), 1);
        assert!(logger.log(&Sample::default(), &Slow::default()).is_err());
    }
}
