//! Application state and the window frame: header, navigation rail, dialogs,
//! toasts, the tray icon plumbing and screenshots.

use super::charts::{Hist, TimeAxis};
use super::pages::PageState;
use super::theme::{self, pal, pal_ctx, regular, semibold, Icon};
use super::widgets;
use crate::alerts::{AlertEvent, Metric};
use crate::model::{Inventory, Sample, Slow};
use crate::sampler::{Monitor, Overhead, Source};
use crate::settings::Settings;
use crate::sys;
use crate::tray::{Tray, TrayEvent};
use eframe::egui::{
    self, vec2, Align, Align2, Color32, CornerRadius, Frame, Key, Layout, Margin, Rect, RichText,
    Sense, Stroke, TextureHandle, Ui,
};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Instant;

/// How the app was started.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Show invented hardware and readings; nothing on this PC is read or changed.
    pub demo: bool,
    /// Render one frame of the chosen view to this PNG, then exit.
    pub screenshot: Option<PathBuf>,
    /// Which page / dialog to open first (see [`Tab::from_name`]; also `about`).
    pub view: Option<String>,
    /// Force the light theme (for screenshots).
    pub light: bool,
    /// Start hidden in the tray (used by the "start with Windows" entry).
    pub start_hidden: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Dashboard,
    Cpu,
    Memory,
    Gpu,
    Storage,
    Network,
    Battery,
    System,
    Displays,
    Sensors,
    Processes,
    Alerts,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 13] = [
        Tab::Dashboard,
        Tab::Cpu,
        Tab::Memory,
        Tab::Gpu,
        Tab::Storage,
        Tab::Network,
        Tab::Battery,
        Tab::System,
        Tab::Displays,
        Tab::Sensors,
        Tab::Processes,
        Tab::Alerts,
        Tab::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Cpu => "Processor",
            Tab::Memory => "Memory",
            Tab::Gpu => "Graphics",
            Tab::Storage => "Storage",
            Tab::Network => "Network",
            Tab::Battery => "Battery",
            Tab::System => "System",
            Tab::Displays => "Displays",
            Tab::Sensors => "Sensors",
            Tab::Processes => "Processes",
            Tab::Alerts => "Alerts",
            Tab::Settings => "Settings",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Tab::Dashboard => Icon::HOME,
            Tab::Cpu => Icon::CPU,
            Tab::Memory => Icon::MEMORY,
            Tab::Gpu => Icon::GPU,
            Tab::Storage => Icon::DRIVE,
            Tab::Network => Icon::NETWORK,
            Tab::Battery => Icon::BATTERY,
            Tab::System => Icon::SYSTEM,
            Tab::Displays => Icon::DISPLAY,
            Tab::Sensors => Icon::SENSOR,
            Tab::Processes => Icon::PROCESSES,
            Tab::Alerts => Icon::BELL,
            Tab::Settings => Icon::SETTINGS,
        }
    }

    /// The page's own name, as [`Tab::from_name`] reads it back (this is how
    /// the last page is remembered in the settings).
    pub fn name(self) -> &'static str {
        match self {
            Tab::Dashboard => "dashboard",
            Tab::Cpu => "cpu",
            Tab::Memory => "memory",
            Tab::Gpu => "gpu",
            Tab::Storage => "storage",
            Tab::Network => "network",
            Tab::Battery => "battery",
            Tab::System => "system",
            Tab::Displays => "displays",
            Tab::Sensors => "sensors",
            Tab::Processes => "processes",
            Tab::Alerts => "alerts",
            Tab::Settings => "settings",
        }
    }

    /// The pages in the navigation rail, top to bottom. A computer without a
    /// battery has no Battery page.
    pub fn visible(has_battery: bool) -> Vec<Tab> {
        Tab::ALL
            .into_iter()
            .filter(|t| has_battery || *t != Tab::Battery)
            .collect()
    }

    /// The page behind Ctrl+1 ... Ctrl+9 and Ctrl+0 (the tenth): its place
    /// in the navigation rail.
    pub fn for_digit(digit: u8, has_battery: bool) -> Option<Tab> {
        let place = match digit {
            1..=9 => usize::from(digit) - 1,
            0 => 9,
            _ => return None,
        };
        Tab::visible(has_battery).get(place).copied()
    }

    /// "Ctrl+3" for the pages that have a number key.
    pub fn shortcut(self, has_battery: bool) -> Option<String> {
        let place = Tab::visible(has_battery).iter().position(|t| *t == self)?;
        match place {
            0..=8 => Some(format!("Ctrl+{}", place + 1)),
            9 => Some("Ctrl+0".to_string()),
            _ => None,
        }
    }

    /// The next (or previous) page in the rail, wrapping round at the ends.
    pub fn step(self, forward: bool, has_battery: bool) -> Tab {
        let pages = Tab::visible(has_battery);
        let n = pages.len();
        let at = pages.iter().position(|t| *t == self).unwrap_or(0);
        pages[if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        }]
    }

    /// The names accepted by `--view`.
    pub fn from_name(name: &str) -> Option<Tab> {
        Some(match name {
            "dashboard" => Tab::Dashboard,
            "cpu" | "processor" => Tab::Cpu,
            "memory" => Tab::Memory,
            "gpu" | "graphics" => Tab::Gpu,
            "storage" => Tab::Storage,
            "network" => Tab::Network,
            "battery" => Tab::Battery,
            "system" => Tab::System,
            "displays" => Tab::Displays,
            "sensors" => Tab::Sensors,
            "processes" => Tab::Processes,
            "alerts" => Tab::Alerts,
            "settings" => Tab::Settings,
            // Views that open a page in a particular state (see `App::apply_view`).
            "storage-health" | "storage-volumes" => Tab::Storage,
            "end-task" | "processes-grouped" | "processes-details" => Tab::Processes,
            "export" | "export-report" => Tab::Settings,
            "mini" | "mini-widget" | "mini-live" => Tab::Dashboard,
            _ => return None,
        })
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub born: Instant,
}

struct Shot {
    path: PathBuf,
    frames: u32,
    requested: bool,
    started: Instant,
}

/// What the pages draw from: refreshed from the monitor at the start of
/// every frame (cheap: three `Arc` clones and a few small copies).
#[derive(Clone, Default)]
pub struct Data {
    pub inventory: Arc<Inventory>,
    pub inventory_ready: bool,
    pub slow: Arc<Slow>,
    pub slow_ready: bool,
    pub sample: Arc<Sample>,
    pub tick: u64,
    /// The history clock at the newest reading.
    pub now: f64,
    pub events: Vec<AlertEvent>,
    pub active_alerts: Vec<(Metric, String, f32)>,
    pub overhead: Overhead,
    pub log_file: Option<PathBuf>,
    pub log_error: Option<String>,
    pub log_rows: u64,
}

pub struct App {
    pub(super) demo: bool,
    pub(super) elevated: bool,
    pub(super) settings: Settings,
    saved_settings: Settings,
    pub(super) tab: Tab,
    pub(super) monitor: Monitor,
    pub(super) data: Data,
    /// While the graphs are paused: the history time they are frozen at.
    pub(super) paused_at: Option<f64>,
    pub(super) startup_on: bool,
    pub(super) about_open: bool,
    pub(super) toasts: Vec<Toast>,
    pub(super) logo: Option<TextureHandle>,
    pub(super) hwnd: isize,
    tray: Option<Tray>,
    tray_rx: Receiver<TrayEvent>,
    pub(super) exiting: bool,
    /// Frames left in which a `--tray` start hides the window again.
    hide_frames: u8,
    shot: Option<Shot>,
    /// The window size seen last, and since when it has been that size: it
    /// is written to the settings once it has stopped changing.
    size_seen: Option<((f32, f32), Instant)>,
    /// What the pages remember between frames (selection, sorting, dialogs).
    pub(super) st: PageState,
}

fn window_handle(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

/// Starts the window.
pub fn run(options: Options, icon: egui::IconData) -> eframe::Result<()> {
    sys::set_demo(options.demo);
    let hidden = options.start_hidden && options.screenshot.is_none() && !options.demo;
    // The window opens at the size it was closed with (never for screenshots
    // and the demo, which have their own fixed size).
    let size = if options.screenshot.is_none() && !options.demo {
        Settings::load().window_size
    } else {
        None
    }
    .unwrap_or((1280.0, 800.0));
    let viewport = egui::ViewportBuilder::default()
        .with_title(if options.demo {
            "PulseForge (demo)"
        } else {
            "PulseForge"
        })
        .with_inner_size([size.0.max(1040.0), size.1.max(660.0)])
        .with_min_inner_size([1040.0, 660.0])
        .with_visible(!hidden)
        .with_icon(icon.clone());
    let native = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "PulseForge",
        native,
        Box::new(move |cc| Ok(Box::new(App::new(cc, options, icon)))),
    )
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, options: Options, icon: egui::IconData) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        let mut settings = if options.demo {
            Settings::default()
        } else {
            Settings::load()
        };
        if options.light {
            settings.theme = crate::settings::ThemeChoice::Light;
        }
        theme::apply_choice(ctx, settings.theme);

        let logo = (icon.width > 0).then(|| {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            ctx.load_texture("logo", image, egui::TextureOptions::LINEAR)
        });

        let wake = ctx.clone();
        let monitor = Monitor::start(
            if options.demo {
                Source::Demo
            } else {
                Source::Real
            },
            &settings,
            Box::new(move || wake.request_repaint()),
        );

        // The tray icon: never for screenshots or the demo.
        let hwnd = window_handle(cc);
        let (tray_tx, tray_rx) = channel();
        let tray = if options.screenshot.is_some() || options.demo || hwnd == 0 {
            None
        } else {
            let wake = ctx.clone();
            Tray::start(hwnd, tray_tx, Box::new(move || wake.request_repaint()))
        };
        if let Some(tray) = &tray {
            tray.set_enabled(settings.tray_icon || options.start_hidden);
            tray.set_mini(settings.mini_widget);
            monitor.set_tray(Some(tray.clone()));
        }
        if options.start_hidden && tray.is_some() {
            monitor.ui_visible.store(false, Ordering::Relaxed);
        }

        let mut app = App {
            demo: options.demo,
            elevated: if options.demo {
                false
            } else {
                sys::is_elevated()
            },
            saved_settings: settings.clone(),
            settings,
            tab: Tab::Dashboard,
            monitor,
            data: Data::default(),
            paused_at: None,
            startup_on: sys::startup_enabled(),
            about_open: false,
            toasts: Vec::new(),
            logo,
            hwnd,
            tray,
            tray_rx,
            exiting: false,
            hide_frames: 0,
            shot: options.screenshot.clone().map(|path| Shot {
                path,
                frames: 0,
                requested: false,
                started: Instant::now(),
            }),
            size_seen: None,
            st: PageState::default(),
        };
        app.st.screenshot = options.screenshot.is_some();
        if options.demo {
            // The invented machine has a past: what the alert rules would have
            // reported over its last hours.
            app.st.demo_events =
                crate::demo::past_events(&app.settings.alerts, crate::util::now_unix());
        }
        if options.start_hidden && app.tray.is_some() {
            sys::cloak_window(app.hwnd);
            app.hide_frames = 3;
        }
        // Without a page named on the command line, the page that was open
        // when the program was closed comes back.
        let remembered = (app.settings.remember_page && !options.demo)
            .then(|| app.settings.last_page.clone())
            .and_then(|name| Tab::from_name(&name).map(|tab| tab.name()));
        app.apply_view(
            options
                .view
                .as_deref()
                .or(remembered)
                .unwrap_or("dashboard"),
        );
        app.refresh_data();
        app
    }

    /// Opens the page / dialog named on the command line.
    ///
    /// Besides the page names: `about`, `storage-health`, `storage-volumes`,
    /// `end-task`, `export-report`, `mini` (the widget drawn inside the window),
    /// `mini-live` (the widget's own window switched on), and any page name followed by a
    /// history window (`dashboard-24h`, `cpu-1h`, `network-6h`, ...).
    fn apply_view(&mut self, view: &str) {
        let mut view = view.trim().to_ascii_lowercase();
        const WINDOW_NAMES: [&str; 7] = ["1m", "5m", "10m", "1h", "2h", "6h", "24h"];
        for (i, name) in WINDOW_NAMES.iter().enumerate() {
            if let Some(rest) = view.strip_suffix(&format!("-{name}")) {
                self.settings.history_window = i;
                view = rest.to_string();
                break;
            }
        }
        if view == "about" {
            self.about_open = true;
        } else if let Some(tab) = Tab::from_name(&view) {
            self.tab = tab;
        }
        self.open_view_state(&view);
        // A view is a starting state, not a change of the user's settings.
        self.saved_settings = self.settings.clone();
    }

    pub(super) fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast {
            text: text.into(),
            error,
            born: Instant::now(),
        });
    }

    /// Copies the latest readings out of the monitor.
    fn refresh_data(&mut self) {
        let Ok(s) = self.monitor.shared.lock() else {
            return;
        };
        if s.tick == self.data.tick
            && s.inventory_ready == self.data.inventory_ready
            && Arc::ptr_eq(&s.slow, &self.data.slow)
            && Arc::ptr_eq(&s.inventory, &self.data.inventory)
        {
            return;
        }
        self.data.inventory = s.inventory.clone();
        self.data.inventory_ready = s.inventory_ready;
        self.data.slow = s.slow.clone();
        self.data.slow_ready = s.slow_ready;
        self.data.sample = s.sample.clone();
        self.data.tick = s.tick;
        self.data.now = s.history.now;
        self.data.events = self
            .st
            .demo_events
            .iter()
            .chain(s.events.iter())
            .cloned()
            .collect();
        self.data.active_alerts = s.active_alerts.clone();
        self.data.overhead = s.overhead;
        self.data.log_file = s.log_file.clone();
        self.data.log_error = s.log_error.clone();
        self.data.log_rows = s.log_rows;
    }

    /// The time the graphs end at: the newest reading, or the moment of pausing.
    pub(super) fn graph_now(&self) -> f64 {
        self.paused_at.unwrap_or(self.data.now)
    }

    /// The graph history for the selected window, ending at [`App::graph_now`].
    pub(super) fn hist(&self) -> Hist {
        Hist {
            shared: self.monitor.shared.clone(),
            now: self.graph_now(),
            window_s: self.window_s(),
        }
    }

    /// The time scale the charts are drawn on.
    pub(super) fn time_axis(&self) -> TimeAxis {
        let behind = (self.data.now - self.graph_now()).max(0.0);
        TimeAxis {
            window_s: self.window_s(),
            end_unix: self.data.sample.time.saturating_sub(behind as u64),
            live: self.paused_at.is_none(),
            fahrenheit: self.settings.fahrenheit,
            utc_offset_s: crate::util::local_offset_minutes() * 60,
        }
    }

    /// The selected history window in seconds.
    pub(super) fn window_s(&self) -> f64 {
        crate::history::WINDOWS
            .get(self.settings.history_window)
            .map(|w| w.1)
            .unwrap_or(300.0)
    }

    pub(super) fn toggle_pause(&mut self) {
        self.paused_at = match self.paused_at {
            Some(_) => None,
            None => Some(self.data.now),
        };
    }

    /// Saves changed settings and passes them on to the monitor and the tray.
    fn sync_settings(&mut self, ctx: &egui::Context) {
        if self.settings == self.saved_settings {
            return;
        }
        if self.settings.theme != self.saved_settings.theme {
            theme::apply_choice(ctx, self.settings.theme);
        }
        self.monitor.apply_settings(&self.settings);
        if let Some(tray) = &self.tray {
            // While the window is hidden the icon is the only way back to it,
            // so it stays whatever the setting says.
            tray.set_enabled(self.settings.tray_icon || sys::window_hidden());
            tray.set_mini(self.settings.mini_widget);
        }
        if !self.demo {
            if let Err(e) = self.settings.save() {
                self.toast(e, true);
            }
        }
        self.saved_settings = self.settings.clone();
    }

    // ----------------------------------------------------------------------------------
    // Tray, window visibility, keyboard, screenshots
    // ----------------------------------------------------------------------------------

    fn handle_tray(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.tray_rx.try_recv() {
            match event {
                TrayEvent::Show => self.bring_up(ctx),
                TrayEvent::ToggleMini => self.settings.mini_widget = !self.settings.mini_widget,
                TrayEvent::Exit => {
                    self.exiting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // Started with --tray: the window was cloaked before its first frame;
        // once the toolkit has shown it (after that frame) it goes to the tray.
        if self.hide_frames > 0 {
            self.hide_frames -= 1;
            if self.hide_frames == 0 {
                sys::hide_window(self.hwnd);
            } else {
                ctx.request_repaint();
            }
            self.monitor.ui_visible.store(false, Ordering::Relaxed);
            return;
        }
        // Closing the window keeps the program in the tray when asked to.
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested
            && !self.exiting
            && self.settings.close_to_tray
            && self.settings.tray_icon
            && self.tray.is_some()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            sys::hide_window(self.hwnd);
            self.monitor.ui_visible.store(false, Ordering::Relaxed);
            return;
        }
        if self.shot.is_none() && self.tray.is_some() {
            // While hidden or minimised the monitor stops waking the window.
            let minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
            let on_screen = !minimized && sys::window_on_screen(self.hwnd);
            self.monitor.ui_visible.store(on_screen, Ordering::Relaxed);
        }
    }

    /// The window is on screen again (from the tray icon, the mini widget or
    /// a second start of the program).
    pub(super) fn bring_up(&mut self, ctx: &egui::Context) {
        self.hide_frames = 0;
        self.monitor.ui_visible.store(true, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        if let Some(tray) = &self.tray {
            // The icon may have been kept only because the window was hidden.
            tray.set_enabled(self.settings.tray_icon);
        }
    }

    /// True while a dialog covers the pages.
    fn dialog_open(&self) -> bool {
        self.about_open || self.st.export_open || self.st.end_task.is_some()
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let (escape, space) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Space) && !i.modifiers.any(),
            )
        });
        if escape {
            if self.about_open {
                self.about_open = false;
            } else {
                self.close_dialogs();
            }
        }
        if space && !ctx.wants_keyboard_input() {
            self.toggle_pause();
        }
        if self.dialog_open() || self.st.mini_preview {
            return;
        }
        const DIGITS: [Key; 10] = [
            Key::Num0,
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ];
        let has_battery = self.data.sample.battery.is_some();
        // (digit, next page, previous page, find, export, mini widget, read again, copy)
        let keys = ctx.input(|i| {
            let ctrl = i.modifiers.command && !i.modifiers.alt;
            let plain = ctrl && !i.modifiers.shift;
            (
                DIGITS
                    .iter()
                    .position(|k| plain && i.key_pressed(*k))
                    .map(|d| d as u8),
                ctrl && !i.modifiers.shift && i.key_pressed(Key::Tab),
                ctrl && i.modifiers.shift && i.key_pressed(Key::Tab),
                plain && i.key_pressed(Key::F),
                plain && i.key_pressed(Key::E),
                plain && i.key_pressed(Key::M),
                i.key_pressed(Key::F5) && !i.modifiers.any(),
                // The toolkit turns Ctrl+C (with or without Shift) into a
                // "copy" event instead of a key press.
                ctrl && i.modifiers.shift
                    && i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
            )
        });
        let (digit, next, previous, find, export, mini, reread, copy) = keys;
        if let Some(tab) = digit.and_then(|d| Tab::for_digit(d, has_battery)) {
            self.tab = tab;
        }
        if next || previous {
            self.tab = self.tab.step(next, has_battery);
        }
        if find {
            self.tab = Tab::Processes;
            self.st.focus_search = true;
        }
        if export {
            self.open_export();
        }
        if mini {
            self.settings.mini_widget = !self.settings.mini_widget;
        }
        if reread {
            self.read_again();
        }
        if copy {
            self.copy_summary();
        }
    }

    /// F5: reads the hardware list, the drives and their health again now.
    pub(super) fn read_again(&mut self) {
        self.monitor.refresh_inventory();
        self.monitor.refresh_slow();
        self.toast(
            if self.demo {
                "Demo mode: the invented hardware stays as it is."
            } else {
                "Reading the hardware and the drives again..."
            },
            false,
        );
    }

    /// Puts a short description of the computer on the clipboard.
    pub(super) fn copy_summary(&mut self) {
        let (inventory, slow, sample) = self.monitor.snapshot();
        let text = crate::report::summary(&inventory, &slow, &sample);
        if text.is_empty() {
            self.toast(
                "The hardware is still being read. Try again in a moment.",
                true,
            );
            return;
        }
        match sys::copy_to_clipboard(&text) {
            Ok(()) => self.toast("A summary of this computer is on the clipboard.", false),
            Err(e) => self.toast(e, !self.demo),
        }
    }

    /// Remembers the window size (once it has stopped changing) and the page
    /// that is open, so that the next start looks like this one.
    fn remember_window(&mut self, ctx: &egui::Context) {
        if self.demo || self.shot.is_some() {
            return;
        }
        if !self.st.mini_preview && self.settings.last_page != self.tab.name() {
            self.settings.last_page = self.tab.name().to_string();
        }
        let (size, maximized, minimized, fullscreen) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.inner_rect
                    .map(|r| (r.width().round(), r.height().round())),
                v.maximized.unwrap_or(false),
                v.minimized.unwrap_or(false),
                v.fullscreen.unwrap_or(false),
            )
        });
        let Some(size) = size else { return };
        // Only an ordinary window has a size worth coming back to.
        if maximized || minimized || fullscreen || sys::window_hidden() || size.0 < 640.0 {
            return;
        }
        match self.size_seen {
            Some((seen, since)) if seen == size => {
                if self.settings.window_size != Some(size) {
                    if since.elapsed().as_millis() >= 800 {
                        self.settings.window_size = Some(size);
                    } else {
                        ctx.request_repaint_after(std::time::Duration::from_millis(300));
                    }
                }
            }
            _ => {
                self.size_seen = Some((size, Instant::now()));
                ctx.request_repaint_after(std::time::Duration::from_millis(900));
            }
        }
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        ctx.request_repaint();
        shot.frames += 1;
        if shot.frames == 1 {
            ctx.set_pixels_per_point(1.0);
        }
        if shot.frames == 3 {
            // Now that one point is one pixel, this gives an exact 1280 x 800 image
            // (or the size asked for with PULSEFORGE_SHOT_SIZE=<width>x<height>,
            // which is how the layout is checked at other window sizes).
            let size = std::env::var("PULSEFORGE_SHOT_SIZE")
                .ok()
                .and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some(vec2(w.trim().parse().ok()?, h.trim().parse().ok()?))
                })
                .filter(|s| s.x >= 640.0 && s.y >= 480.0 && s.x <= 7680.0 && s.y <= 4320.0)
                .unwrap_or(vec2(1280.0, 800.0));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            if let Some(parent) = shot.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let result = image::save_buffer(
                &shot.path,
                image.as_raw(),
                w as u32,
                h as u32,
                image::ColorType::Rgba8,
            );
            if let Err(e) = result {
                eprintln!("screenshot failed: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.shot = None;
            return;
        }
        // PULSEFORGE_SHOT_DELAY_MS waits for real readings before the picture is
        // taken (the demo is ready at once and needs no delay).
        let delay_ms = std::env::var("PULSEFORGE_SHOT_DELAY_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0)
            .min(60_000);
        let waited = shot.started.elapsed().as_millis() as u64 >= delay_ms;
        if !shot.requested && shot.frames > 40 && waited {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }

    // ----------------------------------------------------------------------------------
    // Frame: header and navigation
    // ----------------------------------------------------------------------------------

    fn header(&mut self, ctx: &egui::Context) {
        let p = pal_ctx(ctx);
        egui::TopBottomPanel::top("header")
            .exact_height(64.0)
            .frame(
                Frame::new()
                    .fill(p.rail)
                    .inner_margin(Margin::symmetric(18, 0)),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().hline(
                    full.x_range().expand(18.0),
                    full.bottom(),
                    Stroke::new(1.0_f32, p.border),
                );
                ui.horizontal_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                    if let Some(logo) = &self.logo {
                        ui.painter().image(
                            logo.id(),
                            rect,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.add_space(2.0);
                    ui.label(RichText::new("PulseForge").font(semibold(19.0)));
                    if self.demo {
                        widgets::badge(ui, "DEMO", p.info)
                            .on_hover_text("Invented hardware and readings. Nothing on this PC is read or changed.");
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, Icon::INFO, "About PulseForge").clicked() {
                            self.about_open = true;
                        }
                        let (icon, tip) = if self.paused_at.is_some() {
                            (Icon::PLAY, "Resume the graphs (Space)")
                        } else {
                            (Icon::PAUSE, "Pause the graphs (Space)")
                        };
                        if widgets::icon_button(ui, icon, tip).clicked() {
                            self.toggle_pause();
                        }
                        ui.add_space(4.0);
                        if widgets::ghost_button(ui, &Icon::EXPORT.with("Export report"))
                            .on_hover_text("Save a full report of this computer as HTML, text or JSON.")
                            .clicked()
                        {
                            self.open_export();
                        }
                        ui.add_space(4.0);
                        let labels: Vec<&str> =
                            crate::history::WINDOWS.iter().map(|w| w.0).collect();
                        if let Some(i) =
                            widgets::segmented(ui, &labels, self.settings.history_window)
                        {
                            self.settings.history_window = i;
                        }
                        ui.add_space(6.0);
                        let roomy = ui.available_width() > 330.0;
                        if self.elevated {
                            widgets::badge(ui, &Icon::SHIELD.with("Administrator"), p.good)
                                .on_hover_text("PulseForge runs with administrator rights: drive health details and all sensors Windows exposes are available.");
                        } else if !self.demo {
                            if widgets::ghost_button(ui, &Icon::SHIELD.with("Restart as administrator"))
                                .on_hover_text("Unlocks SMART / NVMe health details and some sensors. PulseForge itself never changes any setting.")
                                .clicked()
                            {
                                self.restart_as_admin(ctx);
                            }
                            if roomy {
                                widgets::badge(ui, "Standard rights", p.muted).on_hover_text(
                                    "Some drive health details and sensors need administrator rights.",
                                );
                            }
                        }
                        if self.paused_at.is_some() && roomy {
                            widgets::badge(ui, "PAUSED", p.warn);
                        }
                    });
                });
            });
    }

    fn rail(&mut self, ctx: &egui::Context) {
        let p = pal_ctx(ctx);
        egui::SidePanel::left("rail")
            .exact_width(208.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(p.rail)
                    .inner_margin(Margin::symmetric(10, 14)),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().vline(
                    full.right() + 10.0,
                    full.y_range().expand(14.0),
                    Stroke::new(1.0_f32, p.border),
                );
                ui.spacing_mut().item_spacing.y = 4.0;
                let has_battery = self.data.sample.battery.is_some();
                for tab in Tab::ALL {
                    if tab == Tab::Battery && !has_battery && self.tab != Tab::Battery {
                        continue;
                    }
                    if tab == Tab::Alerts {
                        ui.add_space(6.0);
                        widgets::divider(ui);
                        ui.add_space(6.0);
                    }
                    let active = self.tab == tab;
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
                    if active {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(9), p.selected);
                        let bar = Rect::from_min_size(
                            rect.left_center() - vec2(0.0, 9.0),
                            vec2(3.0, 18.0),
                        );
                        ui.painter()
                            .rect_filled(bar, CornerRadius::same(2), p.accent);
                    } else if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(9), p.hover);
                    }
                    let fg = if active { p.text } else { p.muted };
                    ui.painter().text(
                        rect.left_center() + vec2(24.0, 0.0),
                        Align2::CENTER_CENTER,
                        tab.icon().glyph(),
                        regular(16.0),
                        if active { p.accent } else { fg },
                    );
                    ui.painter().text(
                        rect.left_center() + vec2(46.0, -1.0),
                        Align2::LEFT_CENTER,
                        tab.label(),
                        if active {
                            semibold(14.5)
                        } else {
                            regular(14.5)
                        },
                        fg,
                    );
                    if tab == Tab::Alerts && !self.data.active_alerts.is_empty() {
                        ui.painter().text(
                            rect.right_center() - vec2(12.0, 1.0),
                            Align2::RIGHT_CENTER,
                            self.data.active_alerts.len().to_string(),
                            semibold(12.5),
                            p.warn,
                        );
                    }
                    let clicked = resp.clicked();
                    if let Some(keys) = tab.shortcut(has_battery) {
                        resp.on_hover_text(keys);
                    }
                    if clicked {
                        self.tab = tab;
                    }
                }
                // On a short window the list reaches the bottom: no room for the footer.
                if ui.available_height() < 34.0 {
                    return;
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                                .size(12.0)
                                .color(p.faint),
                        );
                    });
                });
            });
    }

    // ----------------------------------------------------------------------------------
    // Dialogs and toasts
    // ----------------------------------------------------------------------------------

    fn about_dialog(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let p = pal_ctx(ctx);
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("about"))
            .frame(
                Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::same(28)),
            )
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(84.0, 84.0), Sense::hover());
                    if let Some(logo) = &self.logo {
                        ui.painter().image(
                            logo.id(),
                            rect,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("PulseForge").font(semibold(26.0)));
                    ui.label(
                        RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                            .color(p.muted),
                    );
                    ui.add_space(10.0);
                    ui.label("A clear view of your hardware, with no driver to install.");
                    ui.add_space(10.0);
                    ui.label(RichText::new("Developed by Zheer Khalid").font(semibold(15.0)));
                    ui.add_space(2.0);
                    ui.hyperlink_to(
                        RichText::new("zheer.dev").color(p.accent),
                        "https://zheer.dev",
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Free and open source under the MIT License.")
                            .size(12.0)
                            .color(p.faint),
                    );
                    ui.add_space(14.0);
                    if widgets::primary_button(ui, "Close", true).clicked() {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            self.about_open = false;
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|t| t.born.elapsed().as_secs_f32() < if t.error { 8.0 } else { 3.5 });
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let p = pal_ctx(ctx);
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-18.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                    for t in self.toasts.iter().rev().take(4) {
                        let color = if t.error { p.danger } else { p.accent };
                        Frame::new()
                            .fill(p.card)
                            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.7)))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.set_max_width(420.0);
                                ui.horizontal(|ui| {
                                    let icon = if t.error { Icon::ERROR } else { Icon::DONE };
                                    ui.label(RichText::new(icon.glyph()).color(color));
                                    ui.add(egui::Label::new(t.text.as_str()).wrap());
                                });
                            });
                    }
                });
            });
    }

    /// Starts an elevated copy and closes this one (Windows asks for consent).
    pub(super) fn restart_as_admin(&mut self, ctx: &egui::Context) {
        match sys::restart_as_admin() {
            Ok(()) => {
                self.exiting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.toast(e, true),
        }
    }

    /// The standard page header: title, subtitle and right-aligned actions.
    pub(super) fn page_header(
        ui: &mut Ui,
        title: &str,
        subtitle: &str,
        actions: impl FnOnce(&mut Ui),
    ) {
        let p = pal(ui);
        // A fixed height: a right-to-left row would otherwise take all the
        // height it can get.
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 52.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).font(semibold(24.0)));
                    ui.add(egui::Label::new(RichText::new(subtitle).color(p.muted)).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), actions);
            },
        );
        ui.add_space(8.0);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.st.dark = ctx.style().visuals.dark_mode;
        self.refresh_data();
        self.handle_tray(ctx);
        self.handle_keys(ctx);
        self.handle_screenshot(ctx);

        self.header(ctx);
        self.rail(ctx);

        let p = pal_ctx(ctx);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| self.page_ui(ui));

        self.about_dialog(ctx);
        self.export_dialog(ctx);
        self.end_task_dialog(ctx);
        self.mini_viewport(ctx);
        self.toasts_ui(ctx);
        self.remember_window(ctx);
        self.sync_settings(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Screenshots must not depend on where the mouse happens to be:
        // no hover highlights, no tooltips.
        if self.shot.is_some() {
            raw_input.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::PointerMoved(_)
                        | egui::Event::MouseMoved(_)
                        | egui::Event::PointerButton { .. }
                        | egui::Event::MouseWheel { .. }
                )
            });
            // PULSEFORGE_SHOT_POINTER=<x>,<y> puts the pointer at a fixed place
            // instead, to take a picture of a hover read-out.
            let fixed = std::env::var("PULSEFORGE_SHOT_POINTER").ok().and_then(|v| {
                let (x, y) = v.split_once(',')?;
                Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
            });
            raw_input.events.push(match fixed {
                Some(pos) => egui::Event::PointerMoved(pos),
                None => egui::Event::PointerGone,
            });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(tray) = &self.tray {
            tray.stop();
        }
        self.monitor.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_is_remembered_by_its_name() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_name(tab.name()), Some(tab));
        }
        assert_eq!(Tab::from_name(""), None);
    }

    #[test]
    fn number_keys_follow_the_navigation_rail() {
        // With a battery: thirteen pages, the first ten have a key.
        assert_eq!(Tab::for_digit(1, true), Some(Tab::Dashboard));
        assert_eq!(Tab::for_digit(7, true), Some(Tab::Battery));
        assert_eq!(Tab::for_digit(0, true), Some(Tab::Sensors));
        assert_eq!(Tab::Battery.shortcut(true).as_deref(), Some("Ctrl+7"));
        assert_eq!(Tab::Sensors.shortcut(true).as_deref(), Some("Ctrl+0"));
        assert_eq!(Tab::Settings.shortcut(true), None);
        // Without one the Battery page is not there and the rest moves up.
        assert_eq!(Tab::for_digit(7, false), Some(Tab::System));
        assert_eq!(Tab::for_digit(0, false), Some(Tab::Processes));
        assert_eq!(Tab::Battery.shortcut(false), None);
        assert_eq!(Tab::for_digit(42, true), None);
        // What the tooltip promises is what the key does.
        for battery in [true, false] {
            for (i, tab) in Tab::visible(battery).into_iter().enumerate().take(10) {
                let digit = ((i + 1) % 10) as u8;
                assert_eq!(Tab::for_digit(digit, battery), Some(tab));
                assert_eq!(
                    tab.shortcut(battery),
                    Some(format!("Ctrl+{digit}")),
                    "{tab:?}"
                );
            }
        }
    }

    #[test]
    fn stepping_through_the_pages_wraps_round() {
        assert_eq!(Tab::Dashboard.step(true, true), Tab::Cpu);
        assert_eq!(Tab::Dashboard.step(false, true), Tab::Settings);
        assert_eq!(Tab::Settings.step(true, false), Tab::Dashboard);
        // The Battery page is skipped on a computer without a battery.
        assert_eq!(Tab::Network.step(true, false), Tab::System);
        assert_eq!(Tab::System.step(false, false), Tab::Network);
        assert_eq!(Tab::Network.step(true, true), Tab::Battery);
    }
}
