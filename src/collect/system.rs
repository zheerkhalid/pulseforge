//! Computer, firmware and Windows facts.

use super::smbios;
use crate::model::SystemInfo;
use crate::util;
use std::ffi::c_void;
use windows::Win32::System::SystemInformation::{
    FirmwareTypeBios, FirmwareTypeUefi, GetFirmwareType, GetTickCount64, FIRMWARE_TYPE,
};
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows::Win32::System::TpmBaseServices::{Tbsi_GetDeviceInfo, TPM_DEVICE_INFO};
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
use winreg::RegKey;

/// First build number of Windows 11 (which still calls itself "Windows 10"
/// in the registry).
const WINDOWS_11_BUILD: u32 = 22_000;
/// `TBS_E_TPM_NOT_FOUND`.
const TPM_NOT_FOUND: u32 = 0x8028_400F;

fn machine_key(path: &str) -> Option<RegKey> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)
        .ok()
}

fn text(key: &RegKey, name: &str) -> String {
    key.get_value::<String, _>(name)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Splits the product name into the Windows name and the edition, and
/// corrects "Windows 10" to "Windows 11" from build 22000 on.
/// `("Windows 10 Pro", 26100)` gives `("Windows 11", "Pro")`.
pub fn split_product(product: &str, build: u32, edition_id: &str) -> (String, String) {
    let words: Vec<&str> = product.split_whitespace().collect();
    if words.is_empty() {
        return (String::new(), edition_id.to_string());
    }
    // "Windows Server 2022 Datacenter" keeps three words as its name.
    let name_len = if words.len() >= 3 && words[1].eq_ignore_ascii_case("Server") {
        3
    } else {
        2.min(words.len())
    };
    let mut name = words[..name_len].join(" ");
    if name == "Windows 10" && build >= WINDOWS_11_BUILD {
        name = "Windows 11".to_string();
    }
    let rest = words[name_len..].join(" ");
    let edition = if rest.is_empty() {
        edition_id.to_string()
    } else {
        rest
    };
    (name, edition)
}

/// "26100.2314", or just the build when the update revision is unknown.
pub fn build_text(build: &str, revision: Option<u32>) -> String {
    match (build.trim(), revision) {
        ("", _) => String::new(),
        (b, Some(r)) => format!("{b}.{r}"),
        (b, None) => b.to_string(),
    }
}

/// "UTC+03:00", "UTC-05:30", "UTC".
pub fn utc_offset_text(minutes: i64) -> String {
    if minutes == 0 {
        return "UTC".to_string();
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", m / 60 % 100, m % 60)
}

/// The architecture name Windows uses, in the usual short form.
pub fn arch_name(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "AMD64" | "X64" | "X86_64" => "x64".into(),
        "ARM64" | "AARCH64" => "ARM64".into(),
        "X86" | "I386" | "I686" => "x86".into(),
        "" => String::new(),
        other => other.to_string(),
    }
}

/// "2026-11-16 08:12" for the moment `uptime_s` seconds before `now`.
pub fn boot_time_text(now: u64, uptime_s: u64) -> String {
    let stamp = util::local_datetime(now.saturating_sub(uptime_s));
    stamp.chars().take(16).collect()
}

fn fill_windows(info: &mut SystemInfo) {
    if let Some(key) = machine_key(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") {
        let mut build = text(&key, "CurrentBuildNumber");
        if build.is_empty() {
            build = text(&key, "CurrentBuild");
        }
        let build_number = build.parse::<u32>().unwrap_or(0);
        let (name, edition) = split_product(
            &text(&key, "ProductName"),
            build_number,
            &text(&key, "EditionID"),
        );
        info.os_name = name;
        info.os_edition = edition;
        info.os_display_version = text(&key, "DisplayVersion");
        if info.os_display_version.is_empty() {
            info.os_display_version = text(&key, "ReleaseId");
        }
        info.os_build = build_text(&build, key.get_value::<u32, _>("UBR").ok());
        if let Ok(installed) = key.get_value::<u32, _>("InstallDate") {
            if installed > 0 {
                info.os_install_date = util::local_date(u64::from(installed));
            }
        }
    }
    if let Some(key) = machine_key(r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment")
    {
        info.os_arch = arch_name(&text(&key, "PROCESSOR_ARCHITECTURE"));
    }
    if info.os_arch.is_empty() {
        info.os_arch = arch_name(std::env::consts::ARCH);
    }
}

fn fill_firmware(info: &mut SystemInfo) {
    let mut kind = FIRMWARE_TYPE(0);
    // SAFETY: plain out-parameter call.
    if unsafe { GetFirmwareType(&mut kind) }.is_ok() {
        info.firmware_mode = if kind == FirmwareTypeUefi {
            "UEFI".to_string()
        } else if kind == FirmwareTypeBios {
            "Legacy BIOS".to_string()
        } else {
            String::new()
        };
    }
    info.secure_boot = machine_key(r"SYSTEM\CurrentControlSet\Control\SecureBoot\State")
        .and_then(|key| key.get_value::<u32, _>("UEFISecureBootEnabled").ok())
        .map(|v| v != 0);
    if info.secure_boot.is_none() && info.firmware_mode == "Legacy BIOS" {
        // Secure Boot does not exist on legacy firmware.
        info.secure_boot = Some(false);
    }

    let mut tpm = TPM_DEVICE_INFO::default();
    // SAFETY: the structure is a local of exactly the size that is passed.
    let result = unsafe {
        Tbsi_GetDeviceInfo(
            std::mem::size_of::<TPM_DEVICE_INFO>() as u32,
            &mut tpm as *mut TPM_DEVICE_INFO as *mut c_void,
        )
    };
    match result {
        0 => {
            info.tpm_present = Some(true);
            info.tpm_version = match tpm.tpmVersion {
                1 => "1.2".to_string(),
                2 => "2.0".to_string(),
                _ => String::new(),
            };
        }
        TPM_NOT_FOUND => info.tpm_present = Some(false),
        _ => {}
    }
}

fn fill_session(info: &mut SystemInfo) {
    info.computer_name = std::env::var("COMPUTERNAME").unwrap_or_default();
    info.user_name = std::env::var("USERNAME").unwrap_or_default();
    // SAFETY: no arguments, no failure mode.
    let uptime = unsafe { GetTickCount64() } / 1000;
    info.boot_time = boot_time_text(util::now_unix(), uptime);

    let mut zone = TIME_ZONE_INFORMATION::default();
    // SAFETY: plain out-parameter call.
    let id = unsafe { GetTimeZoneInformation(&mut zone) };
    let name = match id {
        2 => util::from_wide(&zone.DaylightName),
        0 | 1 => util::from_wide(&zone.StandardName),
        _ => String::new(),
    };
    let offset = utc_offset_text(util::local_offset_minutes());
    info.time_zone = if name.trim().is_empty() {
        offset
    } else {
        format!("{offset} ({})", name.trim())
    };

    info.locale = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Control Panel\International", KEY_READ)
        .map(|key| text(&key, "LocaleName"))
        .unwrap_or_default();
}

/// Everything in [`SystemInfo`]: SMBIOS fields (via `smbios::read`), Secure
/// Boot, TPM, Windows edition / build / install date, names and boot time.
pub fn info() -> SystemInfo {
    let mut info = smbios::read().map(|s| s.system).unwrap_or_default();
    fill_windows(&mut info);
    fill_firmware(&mut info);
    fill_session(&mut info);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_names() {
        let split = |p: &str, b: u32, e: &str| split_product(p, b, e);
        assert_eq!(
            split("Windows 10 Pro", 26_100, "Professional"),
            ("Windows 11".into(), "Pro".into())
        );
        assert_eq!(
            split("Windows 10 Pro", 19_045, "Professional"),
            ("Windows 10".into(), "Pro".into())
        );
        assert_eq!(
            split(
                "Windows 10 Home Single Language",
                22_000,
                "CoreSingleLanguage"
            ),
            ("Windows 11".into(), "Home Single Language".into())
        );
        assert_eq!(
            split("Windows Server 2022 Datacenter", 20_348, "ServerDatacenter"),
            ("Windows Server 2022".into(), "Datacenter".into())
        );
        assert_eq!(
            split("Windows 11", 26_100, "Enterprise"),
            ("Windows 11".into(), "Enterprise".into())
        );
        assert_eq!(split("", 0, ""), (String::new(), String::new()));
        assert_eq!(split("Windows", 0, ""), ("Windows".into(), String::new()));
        assert_eq!(split("   ", 30_000, "Core"), (String::new(), "Core".into()));
    }

    #[test]
    fn small_formatters() {
        assert_eq!(build_text("26100", Some(2314)), "26100.2314");
        assert_eq!(build_text("19045", None), "19045");
        assert_eq!(build_text("", Some(5)), "");
        assert_eq!(utc_offset_text(180), "UTC+03:00");
        assert_eq!(utc_offset_text(-330), "UTC-05:30");
        assert_eq!(utc_offset_text(0), "UTC");
        assert_eq!(utc_offset_text(i64::MIN).len(), 9);
        assert_eq!(arch_name("AMD64"), "x64");
        assert_eq!(arch_name("x86_64"), "x64");
        assert_eq!(arch_name("ARM64"), "ARM64");
        assert_eq!(arch_name("x86"), "x86");
        assert_eq!(arch_name(""), "");
        let boot = boot_time_text(1_700_000_000, 3_600);
        assert_eq!(boot.len(), 16);
        assert_eq!(&boot[4..5], "-");
        assert_eq!(&boot[13..14], ":");
        assert_eq!(boot_time_text(10, 99_999).len(), 16);
    }

    #[test]
    fn live_system_info() {
        let s = info();
        println!(
            "system: os name={} edition={} display version={} build={} arch={} install date={} firmware={:?} secure boot known={} tpm known={} tpm version={:?} computer name={} user name={} boot time={} time zone={} locale={} maker={} board={} bios={} chassis={:?}",
            !s.os_name.is_empty(),
            !s.os_edition.is_empty(),
            !s.os_display_version.is_empty(),
            !s.os_build.is_empty(),
            s.os_arch,
            s.os_install_date.len() == 10,
            s.firmware_mode,
            s.secure_boot.is_some(),
            s.tpm_present.is_some(),
            s.tpm_version,
            !s.computer_name.is_empty(),
            !s.user_name.is_empty(),
            s.boot_time.len() == 16,
            !s.time_zone.is_empty(),
            !s.locale.is_empty(),
            !s.manufacturer.is_empty(),
            !s.board_product.is_empty(),
            !s.bios_version.is_empty(),
            s.chassis,
        );
        assert!(s.os_name.starts_with("Windows"));
        assert!(!s.os_build.is_empty());
        assert!(!s.os_arch.is_empty());
        assert_eq!(s.boot_time.len(), 16);
        assert!(s.time_zone.starts_with("UTC"));
    }
}
