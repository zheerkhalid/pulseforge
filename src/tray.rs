//! The notification-area icon: shows a live number, a tooltip with the main
//! readings, alert notifications and a small menu.
//!
//! The icon lives on its own thread with a hidden window, so it keeps working
//! while the main window is hidden and its event loop is asleep.

use crate::sys;
use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
    DrawTextW, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    DT_CENTER, DT_NOCLIP, DT_SINGLELINE, DT_VCENTER, HDC, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_SET_ENABLED: u32 = WM_APP + 2;
const WM_UPDATE: u32 = WM_APP + 3;
const WM_NOTIFY_USER: u32 = WM_APP + 4;
const WM_QUIT_THREAD: u32 = WM_APP + 5;

const MENU_SHOW: usize = 1;
const MENU_MINI: usize = 2;
const MENU_EXIT: usize = 3;

/// What the user did with the tray icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// Show the main window (it has already been made visible).
    Show,
    ToggleMini,
    Exit,
}

struct State {
    tx: Sender<TrayEvent>,
    wake: Box<dyn Fn() + Send>,
    main_window: isize,
    enabled: bool,
    added: bool,
    /// Up to three characters drawn into the icon; empty = the program icon.
    value: String,
    tooltip: String,
    mini_on: bool,
    pending: Vec<(String, String)>,
    icon: isize,
    taskbar_created: u32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.lock().ok().and_then(|mut g| g.as_mut().map(f))
}

fn emit(event: TrayEvent) {
    with_state(|s| {
        let _ = s.tx.send(event);
        (s.wake)();
    });
}

/// The pixels of a tray icon with `text` on the brand gradient: BGRA,
/// premultiplied, top-down, `size` x `size`. The text itself is drawn by GDI
/// afterwards; this is the background with rounded corners (alpha 0 outside).
pub fn icon_background(size: usize) -> Vec<u8> {
    let mut px = vec![0u8; size * size * 4];
    let radius = size as f32 * 0.22;
    let max = size as f32 - 1.0;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32, y as f32);
            // Distance to the nearest corner centre decides the coverage.
            let cx = fx.clamp(radius, max - radius);
            let cy = fy.clamp(radius, max - radius);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let alpha = (radius + 1.0 - d).clamp(0.0, 1.0);
            let t = (fx + fy) / (2.0 * max.max(1.0));
            let r = 0xff as f32 + (0x7a as f32 - 0xff as f32) * t;
            let g = 0x3d as f32 + (0x3c as f32 - 0x3d as f32) * t;
            let b = 0x8b as f32 + (0xf0 as f32 - 0x8b as f32) * t;
            // Darkened a little so white digits stay readable.
            let k = 0.82 * alpha;
            let i = (y * size + x) * 4;
            px[i] = (b * k) as u8;
            px[i + 1] = (g * k) as u8;
            px[i + 2] = (r * k) as u8;
            px[i + 3] = (alpha * 255.0) as u8;
        }
    }
    px
}

/// Renders an icon with the value on it. Returns 0 on failure.
unsafe fn render_icon(text: &str) -> isize {
    let size = GetSystemMetrics(SM_CXSMICON).clamp(16, 64);
    let n = size as usize;
    let dc = CreateCompatibleDC(HDC::default());
    if dc.is_invalid() {
        return 0;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let Ok(color) = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
    else {
        let _ = DeleteDC(dc);
        return 0;
    };
    if bits.is_null() {
        let _ = DeleteObject(color);
        let _ = DeleteDC(dc);
        return 0;
    }
    let background = icon_background(n);
    let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, n * n * 4);
    pixels.copy_from_slice(&background);

    let old_bitmap = SelectObject(dc, color);
    let chars = text.chars().count().max(1) as i32;
    let height = if chars >= 3 {
        size * 62 / 100
    } else {
        size * 82 / 100
    };
    // ANTIALIASED_QUALITY (4): grey-scale smoothing, no coloured fringes.
    let font = CreateFontW(
        -height,
        0,
        0,
        0,
        700,
        0,
        0,
        0,
        0,
        0,
        0,
        4,
        0,
        w!("Segoe UI"),
    );
    let old_font = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, COLORREF(0x00ff_ffff));
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT {
        left: 0,
        top: -(size / 16),
        right: size,
        bottom: size - size / 16,
    };
    DrawTextW(
        dc,
        &mut buf,
        &mut rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP,
    );
    // GDI does not write alpha: restore it from the background.
    for i in 0..n * n {
        pixels[i * 4 + 3] = background[i * 4 + 3];
    }
    SelectObject(dc, old_font);
    SelectObject(dc, old_bitmap);
    let _ = DeleteObject(font);
    let _ = DeleteDC(dc);

    let mask = CreateBitmap(size, size, 1, 1, None);
    let icon_info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    let icon = CreateIconIndirect(&icon_info)
        .map(|i| i.0 as isize)
        .unwrap_or(0);
    let _ = DeleteObject(mask);
    let _ = DeleteObject(color);
    icon
}

unsafe fn app_icon() -> HICON {
    let module = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
    // Icon resource 1 is the application icon embedded by the build script.
    LoadIconW(HINSTANCE(module.0), PCWSTR(std::ptr::without_provenance(1)))
        .or_else(|_| LoadIconW(HINSTANCE::default(), IDI_APPLICATION))
        .unwrap_or_default()
}

fn copy_wide(dst: &mut [u16], text: &str) {
    let max = dst.len() - 1;
    let mut i = 0;
    for c in text.encode_utf16() {
        if i >= max {
            break;
        }
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

/// Adds, updates or removes the icon so that it matches the state.
unsafe fn sync(window: HWND) {
    let Some((enabled, added, value, tooltip, old_icon)) = with_state(|s| {
        (
            s.enabled,
            s.added,
            s.value.clone(),
            s.tooltip.clone(),
            s.icon,
        )
    }) else {
        return;
    };
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        ..Default::default()
    };
    if !enabled {
        if added {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            with_state(|s| s.added = false);
        }
        return;
    }
    let new_icon = if value.is_empty() {
        0
    } else {
        render_icon(&value)
    };
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = if new_icon != 0 {
        HICON(new_icon as *mut c_void)
    } else {
        app_icon()
    };
    copy_wide(&mut data.szTip, &tooltip);
    let ok = Shell_NotifyIconW(if added { NIM_MODIFY } else { NIM_ADD }, &data).as_bool();
    with_state(|s| {
        if ok {
            s.added = true;
        }
        s.icon = new_icon;
    });
    if old_icon != 0 {
        let _ = DestroyIcon(HICON(old_icon as *mut c_void));
    }
}

unsafe fn show_pending(window: HWND) {
    let Some((items, added)) = with_state(|s| (std::mem::take(&mut s.pending), s.added)) else {
        return;
    };
    if !added {
        return;
    }
    for (title, text) in items {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: 1,
            uFlags: NIF_INFO,
            dwInfoFlags: NIIF_WARNING,
            ..Default::default()
        };
        copy_wide(&mut data.szInfoTitle, &title);
        copy_wide(&mut data.szInfo, &text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

unsafe fn tray_menu(window: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let mini_on = with_state(|s| s.mini_on).unwrap_or(false);
    let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Open PulseForge"));
    let _ = AppendMenuW(
        menu,
        if mini_on {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        },
        MENU_MINI,
        w!("Mini widget"),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Needed so the menu closes when the user clicks elsewhere.
    let _ = SetForegroundWindow(window);
    let choice = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        window,
        None,
    );
    let _ = DestroyMenu(menu);
    match choice.0 as usize {
        MENU_SHOW => show_main(),
        MENU_MINI => {
            wake_main();
            emit(TrayEvent::ToggleMini);
        }
        MENU_EXIT => {
            // The main window must be awake to process the exit.
            wake_main();
            emit(TrayEvent::Exit);
        }
        _ => {}
    }
}

fn main_window() -> isize {
    with_state(|s| s.main_window).unwrap_or(0)
}

/// A hidden window does not run its event loop: nudge it with a harmless message.
fn wake_main() {
    let main = main_window();
    if main != 0 {
        // SAFETY: posting to a window of this process; a stale handle just fails.
        unsafe {
            let _ = PostMessageW(HWND(main as *mut c_void), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

fn show_main() {
    sys::show_window(main_window());
    emit(TrayEvent::Show);
}

unsafe extern "system" fn wnd_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_main(),
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(window),
                _ => {}
            }
            LRESULT(0)
        }
        WM_SET_ENABLED => {
            with_state(|s| s.enabled = wparam.0 != 0);
            sync(window);
            LRESULT(0)
        }
        WM_UPDATE => {
            sync(window);
            LRESULT(0)
        }
        WM_NOTIFY_USER => {
            show_pending(window);
            LRESULT(0)
        }
        // Sent by a second copy of the program that was started while this
        // one runs: show the window instead of opening another.
        sys::WM_SHOW_MAIN => {
            show_main();
            LRESULT(0)
        }
        WM_QUIT_THREAD => {
            with_state(|s| s.enabled = false);
            sync(window);
            let _ = DestroyWindow(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            // Explorer restarted: the icon has to be added again.
            let created = with_state(|s| s.taskbar_created).unwrap_or(0);
            if created != 0 && msg == created {
                with_state(|s| s.added = false);
                sync(window);
                return LRESULT(0);
            }
            DefWindowProcW(window, msg, wparam, lparam)
        }
    }
}

/// Handle to the tray thread. Cheap to clone; all methods just post a message.
#[derive(Clone)]
pub struct Tray {
    window: isize,
}

impl Tray {
    /// Starts the tray thread (the icon itself appears with [`Tray::set_enabled`]).
    /// `wake` is called after every event so the user interface picks it up.
    pub fn start(
        main_window: isize,
        tx: Sender<TrayEvent>,
        wake: Box<dyn Fn() + Send>,
    ) -> Option<Tray> {
        if let Ok(mut g) = STATE.lock() {
            *g = Some(State {
                tx,
                wake,
                main_window,
                enabled: false,
                added: false,
                value: String::new(),
                tooltip: "PulseForge".into(),
                mini_on: false,
                pending: Vec::new(),
                icon: 0,
                taskbar_created: 0,
            });
        }
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<isize>();
        std::thread::Builder::new()
            .name("pulseforge-tray".into())
            .spawn(move || {
                // SAFETY: a standard window class, window and message loop,
                // all owned by this thread.
                unsafe {
                    let module = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(wnd_proc),
                        hInstance: HINSTANCE(module.0),
                        lpszClassName: w!("PulseForgeTrayWindow"),
                        ..Default::default()
                    };
                    RegisterClassW(&class);
                    let window = CreateWindowExW(
                        WS_EX_TOOLWINDOW,
                        w!("PulseForgeTrayWindow"),
                        w!("PulseForge tray"),
                        WS_POPUP,
                        0,
                        0,
                        0,
                        0,
                        HWND::default(),
                        HMENU::default(),
                        HINSTANCE(module.0),
                        None,
                    );
                    let Ok(window) = window else {
                        let _ = ready_tx.send(0);
                        return;
                    };
                    let created = RegisterWindowMessageW(w!("TaskbarCreated"));
                    with_state(|s| s.taskbar_created = created);
                    let _ = ready_tx.send(window.0 as isize);
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .ok()?;
        let window = ready_rx.recv().ok().filter(|w| *w != 0)?;
        Some(Tray { window })
    }

    fn post(&self, msg: u32, wparam: usize) {
        // SAFETY: posting to our own window; a stale handle just fails.
        unsafe {
            let _ = PostMessageW(
                HWND(self.window as *mut c_void),
                msg,
                WPARAM(wparam),
                LPARAM(0),
            );
        }
    }

    /// Shows or removes the icon.
    pub fn set_enabled(&self, on: bool) {
        self.post(WM_SET_ENABLED, usize::from(on));
    }

    /// Sets the number in the icon (up to three characters; empty shows the
    /// program icon) and the tooltip. Does nothing if both are unchanged.
    pub fn set_value(&self, value: &str, tooltip: &str) {
        let changed = with_state(|s| {
            let value: String = value.chars().take(3).collect();
            let tooltip: String = tooltip.chars().take(120).collect();
            if s.value == value && s.tooltip == tooltip {
                false
            } else {
                s.value = value;
                s.tooltip = tooltip;
                true
            }
        })
        .unwrap_or(false);
        if changed {
            self.post(WM_UPDATE, 0);
        }
    }

    /// Shows a notification bubble (only while the icon is visible).
    pub fn notify(&self, title: &str, text: &str) {
        with_state(|s| {
            if s.pending.len() < 8 {
                s.pending.push((title.to_string(), text.to_string()));
            }
        });
        self.post(WM_NOTIFY_USER, 0);
    }

    /// Tells the menu whether the mini widget is on (for its check mark).
    pub fn set_mini(&self, on: bool) {
        with_state(|s| s.mini_on = on);
    }

    /// Removes the icon and ends the thread.
    pub fn stop(&self) {
        self.post(WM_QUIT_THREAD, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_background_has_round_transparent_corners() {
        for size in [16usize, 20, 24, 32] {
            let px = icon_background(size);
            assert_eq!(px.len(), size * size * 4);
            let alpha = |x: usize, y: usize| px[(y * size + x) * 4 + 3];
            assert_eq!(alpha(0, 0), 0, "corner must be transparent at {size}");
            assert_eq!(alpha(size - 1, size - 1), 0);
            assert_eq!(alpha(size / 2, size / 2), 255);
            assert_eq!(alpha(size / 2, 0), 255, "edges are opaque");
            // Premultiplied: no channel may exceed alpha.
            assert!(px
                .chunks(4)
                .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        }
    }

    #[test]
    fn wide_copy_truncates_and_terminates() {
        let mut buf = [0xffffu16; 6];
        copy_wide(&mut buf, "PulseForge");
        assert_eq!(&buf[..5], &[80, 117, 108, 115, 101]);
        assert_eq!(buf[5], 0);
        let mut buf = [0xffffu16; 6];
        copy_wide(&mut buf, "ab");
        assert_eq!(&buf[..3], &[97, 98, 0]);
    }
}
