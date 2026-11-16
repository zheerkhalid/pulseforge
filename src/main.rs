#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use pulseforge::report::{self, Format, ReportData};
use pulseforge::ui::{self, Options};
use std::path::PathBuf;

const USAGE: &str = "PulseForge

  PulseForge.exe                      open the program
  PulseForge.exe --tray               start hidden in the notification area
  PulseForge.exe --demo               show invented hardware and readings; nothing on this PC
                                      is read or changed
  PulseForge.exe --screenshot <png>   render the window to a PNG and exit
        [--view dashboard|cpu|memory|gpu|storage|network|battery|system|displays|sensors|
                processes|alerts|settings|about|...] [--light]
        (the view can also be set with the PULSEFORGE_VIEW environment variable)
  PulseForge.exe --report <file> [--format html|txt|json] [--no-mask] [--demo]
                                      write a full system report and exit; serial numbers
                                      and names are masked unless --no-mask is given
  PulseForge.exe --selftest           run every collector once and print which ones
                                      delivered data (no identifying values are printed)";

/// A windowed program has no console of its own: borrow the parent's, so
/// that text output shows up when started from a terminal.
fn attach_console() {
    // SAFETY: plain call; failing (no parent console, output redirected) is fine.
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    if has("--help") || has("-h") || has("/?") {
        attach_console();
        println!("{USAGE}");
        return Ok(());
    }
    if has("--selftest") {
        attach_console();
        let (checks, ok) = pulseforge::selftest::run();
        for c in &checks {
            println!(
                "[{}] {:<22} {}",
                if c.ok {
                    " ok "
                } else if c.required {
                    "FAIL"
                } else {
                    "none"
                },
                c.name,
                c.detail
            );
        }
        println!(
            "{}",
            if ok {
                "SELFTEST PASSED"
            } else {
                "SELFTEST FAILED"
            }
        );
        std::process::exit(if ok { 0 } else { 1 });
    }
    if let Some(path) = value("--report") {
        attach_console();
        let format = match value("--format").as_deref() {
            Some("txt") | Some("text") => Format::Text,
            Some("json") => Format::Json,
            Some("html") | None => Format::Html,
            Some(other) => {
                println!("Unknown format \"{other}\": use html, txt or json.");
                std::process::exit(2);
            }
        };
        let (inventory, slow, sample) = if has("--demo") {
            let mut source = pulseforge::demo::DemoSource::new();
            (
                pulseforge::demo::inventory(),
                pulseforge::demo::slow(),
                source.sample(0.0, pulseforge::util::now_unix(), true),
            )
        } else {
            let slow = pulseforge::sampler::collect_slow();
            let mut source = pulseforge::sampler::RealSource::new();
            // Rates need two readings.
            source.sample(true, &slow);
            std::thread::sleep(std::time::Duration::from_millis(1000));
            let sample = source.sample(true, &slow);
            (pulseforge::sampler::collect_inventory(), slow, sample)
        };
        let data = ReportData {
            inventory: &inventory,
            slow: &slow,
            sample: &sample,
            generated: pulseforge::util::now_unix(),
            fahrenheit: false,
        };
        let text = report::render(&data, format, !has("--no-mask"));
        match std::fs::write(&path, text) {
            Ok(()) => {
                println!("Report written.");
                std::process::exit(0);
            }
            Err(e) => {
                println!("Could not write the report: {e}");
                std::process::exit(1);
            }
        }
    }

    let options = Options {
        demo: has("--demo"),
        screenshot: value("--screenshot").map(PathBuf::from),
        view: value("--view").or_else(|| std::env::var("PULSEFORGE_VIEW").ok()),
        light: has("--light"),
        start_hidden: has("--tray"),
    };
    ui::run(options, window_icon())
}

fn window_icon() -> egui::IconData {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.png"));
    match image::load_from_memory(bytes) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            egui::IconData {
                rgba: rgba.into_raw(),
                width,
                height,
            }
        }
        Err(_) => egui::IconData::default(),
    }
}
