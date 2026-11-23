//! Small pieces of Windows: elevation, the "start with Windows" entry, the
//! program's own resource use, opening links and folders, window helpers.

use crate::util::wide;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    SW_SHOWNORMAL,
};

static DEMO: AtomicBool = AtomicBool::new(false);

/// In demo mode nothing is written outside the screenshot file and nothing on
/// the machine is touched.
pub fn set_demo(on: bool) {
    DEMO.store(on, Ordering::SeqCst);
}

pub fn is_demo() -> bool {
    DEMO.load(Ordering::SeqCst)
}

/// `%LOCALAPPDATA%\PulseForge`.
pub fn app_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("PulseForge")
}

/// True when the program runs with administrator rights.
pub fn is_elevated() -> bool {
    // SAFETY: the token handle is opened and closed here; the buffer is a local.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

fn shell_execute(verb: &str, file: &str, params: &str) -> Result<(), String> {
    let (v, f, p) = (wide(verb), wide(file), wide(params));
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let r = unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(v.as_ptr()),
            PCWSTR(f.as_ptr()),
            if params.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(p.as_ptr())
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r.0 as usize > 32 {
        Ok(())
    } else {
        Err("Windows could not open it.".to_string())
    }
}

/// Starts a second, elevated copy of the program (Windows asks for consent).
/// The caller closes this copy when the call succeeds.
pub fn restart_as_admin() -> Result<(), String> {
    if is_demo() {
        return Err("Demo mode: nothing on this PC is changed.".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // This copy is still running while the elevated one starts: tell the new
    // one not to hand over to it.
    shell_execute("runas", &exe.to_string_lossy(), NEW_INSTANCE_FLAG)
}

// ----------------------------------------------------------------------------------
// One copy at a time
// ----------------------------------------------------------------------------------

/// Command-line switch that starts a second copy on purpose.
pub const NEW_INSTANCE_FLAG: &str = "--new-instance";
/// Posted to the tray thread's window to bring the main window up.
pub const WM_SHOW_MAIN: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 6;

/// What a starting copy should do about a copy that may already be running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Startup {
    /// No other copy: carry on.
    Run,
    /// Another copy is running and has been told what it needs to know: exit.
    HandOver,
}

/// The decision behind [`claim_instance`], separated from the Windows calls.
/// `exists`: another copy holds the name. `quiet`: this start was the silent
/// start with Windows. `shown`: the other copy accepted the request to show
/// its window.
pub fn startup_decision(exists: bool, quiet: bool, shown: bool) -> Startup {
    if !exists {
        Startup::Run
    } else if quiet || shown {
        Startup::HandOver
    } else {
        // The other copy cannot be reached (it is still starting, is closing,
        // or runs with higher rights): better two windows than none.
        Startup::Run
    }
}

/// Makes this the one running copy, or asks the copy that already runs to
/// show its window. `quiet` is the start with Windows (`--tray`), which must
/// never pop a window up.
pub fn claim_instance(quiet: bool) -> Startup {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WPARAM};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
    };
    // SAFETY: plain calls. The mutex handle is kept for the life of the
    // process on purpose: it is what marks this copy as running.
    unsafe {
        let mutex = CreateMutexW(None, false, w!("Local\\PulseForge.OneCopy"));
        let exists = mutex.is_ok() && GetLastError() == ERROR_ALREADY_EXISTS;
        let mut shown = false;
        if exists && !quiet {
            if let Ok(window) = FindWindowW(w!("PulseForgeTrayWindow"), PCWSTR::null()) {
                let mut pid = 0u32;
                GetWindowThreadProcessId(window, Some(&mut pid));
                if pid != 0 {
                    let _ = AllowSetForegroundWindow(pid);
                }
                shown = PostMessageW(window, WM_SHOW_MAIN, WPARAM(0), LPARAM(0)).is_ok();
            }
        }
        let decision = startup_decision(exists, quiet, shown);
        if decision == Startup::HandOver {
            if let Ok(handle) = mutex {
                let _ = CloseHandle(handle);
            }
        }
        decision
    }
}

/// Opens a web link in the default browser. Only http(s) links are accepted.
pub fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains('"') {
        return Err("Not a web link.".into());
    }
    shell_execute("open", url, "")
}

/// Opens a folder in File Explorer.
pub fn open_folder(path: &std::path::Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err("The folder does not exist yet.".into());
    }
    shell_execute("open", &path.to_string_lossy(), "")
}

// ----------------------------------------------------------------------------------
// Start with Windows
// ----------------------------------------------------------------------------------

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "PulseForge";

/// The command line stored in the Run key for this executable.
pub fn startup_command(exe: &std::path::Path) -> String {
    format!("\"{}\" --tray", exe.display())
}

/// True when the per-user "start with Windows" entry exists.
pub fn startup_enabled() -> bool {
    if is_demo() {
        return false;
    }
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

/// Adds or removes the per-user "start with Windows" entry (HKCU Run key;
/// no administrator rights needed, nothing machine-wide is touched).
pub fn set_startup(on: bool) -> Result<(), String> {
    if is_demo() {
        return Err("Demo mode: nothing on this PC is changed.".into());
    }
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(RUN_KEY)
        .map_err(|e| format!("Could not open the startup key: {e}"))?;
    if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_value(RUN_VALUE, &startup_command(&exe))
            .map_err(|e| format!("Could not add the startup entry: {e}"))
    } else {
        match key.delete_value(RUN_VALUE) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Could not remove the startup entry: {e}")),
        }
    }
}

// ----------------------------------------------------------------------------------
// The program's own resource use
// ----------------------------------------------------------------------------------

fn filetime_100ns(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Processor time this process has used so far (kernel + user, in 100 ns
/// units) and its private memory in bytes.
pub fn self_usage() -> (u64, u64) {
    // SAFETY: out-parameters are locals; the pseudo handle needs no closing.
    unsafe {
        let process = GetCurrentProcess();
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let time = if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user)
            .is_ok()
        {
            filetime_100ns(kernel) + filetime_100ns(user)
        } else {
            0
        };
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let memory = if K32GetProcessMemoryInfo(
            process,
            &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            size,
        )
        .as_bool()
        {
            counters.PrivateUsage as u64
        } else {
            0
        };
        (time, memory)
    }
}

/// Number of logical processors (at least 1).
pub fn logical_processors() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

// ----------------------------------------------------------------------------------
// Window helpers (the main window, by raw handle)
// ----------------------------------------------------------------------------------

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

static HIDDEN: AtomicBool = AtomicBool::new(false);

/// Adds or removes the window's taskbar button.
fn taskbar_button(window: HWND, show: bool) {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
    // SAFETY: standard COM use; the interface is released when it goes out
    // of scope, before the matching CoUninitialize.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        {
            let list: windows::core::Result<ITaskbarList> =
                CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER);
            if let Ok(list) = list {
                if list.HrInit().is_ok() {
                    let _ = if show {
                        list.AddTab(window)
                    } else {
                        list.DeleteTab(window)
                    };
                }
            }
        }
        if init.is_ok() {
            CoUninitialize();
        }
    }
}

fn cloak(window: HWND, on: bool) {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    let value = BOOL::from(on);
    // SAFETY: the attribute is a BOOL of the size passed.
    unsafe {
        let _ = DwmSetWindowAttribute(
            window,
            DWMWA_CLOAK,
            &value as *const BOOL as *const c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
    }
}

/// Makes the window invisible before it is first shown (for a start in the
/// tray, so that it never flashes up). Follow with [`hide_window`].
pub fn cloak_window(raw: isize) {
    if raw != 0 {
        cloak(hwnd(raw), true);
        HIDDEN.store(true, Ordering::SeqCst);
    }
}

/// Sends the window to the tray: it disappears from the screen, the taskbar
/// and the task switcher.
///
/// The window is minimised, cloaked and its taskbar button removed rather
/// than hidden with `SW_HIDE`: a truly hidden window never receives paint
/// messages, and the window toolkit then spins a processor core waiting to
/// deliver any repaint that is still pending.
pub fn hide_window(raw: isize) {
    if raw != 0 {
        let window = hwnd(raw);
        HIDDEN.store(true, Ordering::SeqCst);
        // SAFETY: a window handle of this process; a stale handle just fails.
        unsafe {
            let _ = ShowWindow(window, SW_MINIMIZE);
        }
        cloak(window, true);
        taskbar_button(window, false);
    }
}

/// Shows the window again, restores it if minimised and brings it to the front.
pub fn show_window(raw: isize) {
    if raw != 0 {
        let window = hwnd(raw);
        let was_hidden = HIDDEN.swap(false, Ordering::SeqCst);
        cloak(window, false);
        // SAFETY: as above.
        unsafe {
            if was_hidden {
                taskbar_button(window, true);
            }
            if IsIconic(window).as_bool() {
                let _ = ShowWindow(window, SW_RESTORE);
            } else {
                let _ = ShowWindow(window, SW_SHOW);
            }
            let _ = SetForegroundWindow(window);
        }
    }
}

/// Puts `text` on the clipboard as plain text.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    if is_demo() {
        return Err("Demo mode: nothing is copied.".into());
    }
    const CF_UNICODETEXT: u32 = 13;
    let data = wide(text);
    let failed = || "Windows did not accept the text for the clipboard.".to_string();
    // SAFETY: the block is allocated with room for the whole NUL-terminated
    // string and filled while locked. Once SetClipboardData succeeds the
    // clipboard owns it; on any earlier failure it is freed here.
    unsafe {
        let block =
            GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(&data[..])).map_err(|_| failed())?;
        let target = GlobalLock(block) as *mut u16;
        if target.is_null() {
            let _ = GlobalFree(block);
            return Err(failed());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        let _ = GlobalUnlock(block);
        if OpenClipboard(HWND::default()).is_err() {
            let _ = GlobalFree(block);
            return Err("The clipboard is in use by another program. Try again.".into());
        }
        let _ = EmptyClipboard();
        let set = SetClipboardData(CF_UNICODETEXT, HANDLE(block.0));
        let _ = CloseClipboard();
        if set.is_err() {
            let _ = GlobalFree(block);
            return Err(failed());
        }
    }
    Ok(())
}

/// The whole desktop (all displays together) as left, top, width and height
/// in pixels.
pub fn desktop_rect() -> (i32, i32, i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    // SAFETY: plain queries without side effects.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// True while the window has been sent to the tray with [`hide_window`].
pub fn window_hidden() -> bool {
    HIDDEN.load(Ordering::SeqCst)
}

/// True when the window is neither in the tray nor minimised.
pub fn window_on_screen(raw: isize) -> bool {
    if raw == 0 {
        return true;
    }
    if window_hidden() {
        return false;
    }
    // SAFETY: as above.
    unsafe { IsWindowVisible(hwnd(raw)).as_bool() && !IsIconic(hwnd(raw)).as_bool() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_command_is_quoted() {
        let cmd = startup_command(std::path::Path::new(
            "C:\\Program Files\\PulseForge\\PulseForge.exe",
        ));
        assert_eq!(
            cmd,
            "\"C:\\Program Files\\PulseForge\\PulseForge.exe\" --tray"
        );
    }

    /// Writes the real per-user Run value and removes it again. Not part of
    /// the normal run: `cargo test --lib sys::tests::startup_entry -- --ignored`.
    #[test]
    #[ignore = "touches the real HKCU Run key (and cleans up)"]
    fn startup_entry_round_trip() {
        let before = startup_enabled();
        if before {
            // An entry of an installed copy exists: leave it alone.
            return;
        }
        set_startup(true).unwrap();
        assert!(startup_enabled());
        set_startup(false).unwrap();
        assert!(!startup_enabled());
        // Removing a missing entry is not an error.
        set_startup(false).unwrap();
    }

    #[test]
    fn a_second_copy_hands_over_only_when_the_first_can_be_reached() {
        assert_eq!(startup_decision(false, false, false), Startup::Run);
        assert_eq!(startup_decision(false, true, false), Startup::Run);
        // A normal second start shows the running window and exits.
        assert_eq!(startup_decision(true, false, true), Startup::HandOver);
        // The start with Windows never opens a window: it just exits.
        assert_eq!(startup_decision(true, true, false), Startup::HandOver);
        // The running copy could not be told: start anyway.
        assert_eq!(startup_decision(true, false, false), Startup::Run);
    }

    #[test]
    fn urls_are_filtered() {
        assert!(open_url("file:///C:/Windows").is_err());
        assert!(open_url("https://zheer.dev\" & calc").is_err());
        assert!(open_url("calc.exe").is_err());
    }

    #[test]
    fn the_desktop_has_a_size() {
        let (_, _, width, height) = desktop_rect();
        assert!(width > 0 && height > 0);
    }

    #[test]
    fn own_usage_is_readable() {
        let (time, memory) = self_usage();
        assert!(memory > 0);
        let _ = time;
        assert!(logical_processors() >= 1);
    }
}
