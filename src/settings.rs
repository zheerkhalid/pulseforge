//! User settings, stored as JSON in `%LOCALAPPDATA%\PulseForge\settings.json`.

use crate::alerts::{self, AlertRule};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    Dark,
    Light,
    System,
}

/// What the tray icon shows as a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayValue {
    CpuUsage,
    CpuTemp,
    MemoryUsage,
    GpuUsage,
    GpuTemp,
    Battery,
    /// Just the program icon.
    None,
}

impl TrayValue {
    pub const ALL: [TrayValue; 7] = [
        TrayValue::CpuUsage,
        TrayValue::CpuTemp,
        TrayValue::MemoryUsage,
        TrayValue::GpuUsage,
        TrayValue::GpuTemp,
        TrayValue::Battery,
        TrayValue::None,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TrayValue::CpuUsage => "Processor usage",
            TrayValue::CpuTemp => "Processor temperature",
            TrayValue::MemoryUsage => "Memory usage",
            TrayValue::GpuUsage => "Graphics usage",
            TrayValue::GpuTemp => "Graphics temperature",
            TrayValue::Battery => "Battery level",
            TrayValue::None => "Program icon only",
        }
    }
}

/// The update intervals offered, in milliseconds.
pub const INTERVALS_MS: [u32; 5] = [500, 1000, 2000, 5000, 10_000];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub fahrenheit: bool,
    /// How often readings are taken.
    pub interval_ms: u32,
    /// Index into [`crate::history::WINDOWS`].
    pub history_window: usize,
    /// Show serial numbers only partly (in the window; reports have their own switch).
    pub mask_serials: bool,

    pub tray_icon: bool,
    pub tray_value: TrayValue,
    /// Closing the window keeps the program running in the tray.
    pub close_to_tray: bool,
    /// When started with Windows, start in the tray.
    pub start_minimized: bool,
    pub mini_widget: bool,
    pub mini_pos: Option<(f32, f32)>,

    pub alerts_enabled: bool,
    pub alerts: Vec<AlertRule>,

    pub log_csv: bool,
    /// Empty = the default folder (`logs` next to the settings file).
    pub log_dir: String,
    /// Seconds between rows in the CSV log.
    pub log_interval_s: u32,
    /// Days a CSV log file is kept before it is deleted; 0 keeps them for ever.
    pub log_keep_days: u32,

    /// Open the page that was showing when the program was closed.
    pub remember_page: bool,
    /// Name of that page (see `Tab::name`).
    pub last_page: String,
    /// Size of the window when it was last closed, in points.
    pub window_size: Option<(f32, f32)>,
    /// One row per program name on the Processes page.
    pub group_processes: bool,

    /// Mask serial numbers and names in exported reports.
    pub report_mask: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::Dark,
            fahrenheit: false,
            interval_ms: 1000,
            history_window: 1,
            mask_serials: true,
            tray_icon: true,
            tray_value: TrayValue::CpuUsage,
            close_to_tray: false,
            start_minimized: true,
            mini_widget: false,
            mini_pos: None,
            alerts_enabled: true,
            alerts: alerts::default_rules(),
            log_csv: false,
            log_dir: String::new(),
            log_interval_s: 5,
            log_keep_days: 0,
            remember_page: true,
            last_page: String::new(),
            window_size: None,
            group_processes: false,
            report_mask: true,
        }
    }
}

impl Settings {
    /// Brings loaded values back into their valid ranges.
    pub fn sanitize(&mut self) {
        if !INTERVALS_MS.contains(&self.interval_ms) {
            self.interval_ms = 1000;
        }
        if self.history_window >= crate::history::WINDOWS.len() {
            self.history_window = 1;
        }
        self.log_interval_s = self.log_interval_s.clamp(1, 3600);
        self.log_keep_days = self.log_keep_days.min(3650);
        // A window size that makes no sense (a damaged file, a display that
        // is gone) falls back to the default size.
        if let Some((w, h)) = self.window_size {
            let sane = w.is_finite() && h.is_finite() && w >= 640.0 && h >= 480.0;
            self.window_size = sane.then_some((w.min(7680.0), h.min(4320.0)));
        }
        if let Some((x, y)) = self.mini_pos {
            if !(x.is_finite() && y.is_finite()) {
                self.mini_pos = None;
            }
        }
        alerts::normalize(&mut self.alerts);
    }

    pub fn load_from(path: &Path) -> Settings {
        let mut s: Settings = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.sanitize();
        s
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("Could not save settings: {e}"))
    }

    pub fn path() -> PathBuf {
        crate::sys::app_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        Settings::load_from(&Settings::path())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Settings::path())
    }

    /// Where CSV logs go.
    pub fn log_folder(&self) -> PathBuf {
        if self.log_dir.trim().is_empty() {
            crate::sys::app_dir().join("logs")
        } else {
            PathBuf::from(self.log_dir.trim())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tolerant_loading() {
        let dir =
            std::env::temp_dir().join(format!("pulseforge-settings-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(Settings::load_from(&path), Settings::default());
        let mut s = Settings {
            theme: ThemeChoice::Light,
            fahrenheit: true,
            interval_ms: 2000,
            tray_value: TrayValue::GpuTemp,
            ..Default::default()
        };
        s.alerts[1].threshold = 75.0;
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);
        // Partial files keep defaults for the rest; out-of-range values are repaired.
        std::fs::write(
            &path,
            r#"{"theme":"System","interval_ms":7,"history_window":99,"alerts":[],"future":1}"#,
        )
        .unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.theme, ThemeChoice::System);
        assert_eq!(loaded.interval_ms, 1000);
        assert_eq!(loaded.history_window, 1);
        assert_eq!(loaded.alerts, alerts::default_rules());
        // New options keep their defaults when an older file is loaded.
        assert_eq!(loaded.log_keep_days, 0);
        assert!(loaded.remember_page);
        assert_eq!(loaded.window_size, None);
        // Nonsense sizes and positions are dropped, big ones are capped.
        std::fs::write(
            &path,
            r#"{"window_size":[10.0,5.0],"log_keep_days":999999}"#,
        )
        .unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.window_size, None);
        assert_eq!(loaded.log_keep_days, 3650);
        let mut big = Settings {
            window_size: Some((99_999.0, 900.0)),
            mini_pos: Some((f32::NAN, 3.0)),
            ..Default::default()
        };
        big.sanitize();
        assert_eq!(big.window_size, Some((7680.0, 900.0)));
        assert_eq!(big.mini_pos, None);
        // Broken files fall back entirely.
        std::fs::write(&path, "{{{").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
