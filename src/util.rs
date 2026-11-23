//! Formatting helpers shared by the user interface, the reports and the logs.

use std::time::{SystemTime, UNIX_EPOCH};

/// "1.5 GB", "512 MB", "0 B". Binary multiples, as Windows shows them.
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// A transfer rate: "12.4 MB/s".
pub fn fmt_rate(bytes_per_s: f64) -> String {
    let v = if bytes_per_s.is_finite() && bytes_per_s > 0.0 {
        bytes_per_s
    } else {
        0.0
    };
    format!("{}/s", fmt_bytes(v as u64))
}

/// A network rate in bits: "940 Mbps".
pub fn fmt_bits(bits_per_s: u64) -> String {
    let v = bits_per_s as f64;
    if v >= 1e9 {
        format!("{:.1} Gbps", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.0} Mbps", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.0} Kbps", v / 1e3)
    } else {
        format!("{bits_per_s} bps")
    }
}

/// "3 d 4 h 12 min", "4 h 12 min", "12 min 5 s", "42 s".
pub fn fmt_duration(seconds: u64) -> String {
    let d = seconds / 86_400;
    let h = seconds % 86_400 / 3600;
    let m = seconds % 3600 / 60;
    let s = seconds % 60;
    if d > 0 {
        format!("{d} d {h} h {m} min")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else if m > 0 {
        format!("{m} min {s} s")
    } else {
        format!("{s} s")
    }
}

/// Hours as "1 y 5 mo", "23 d 4 h" or "17 h": for power-on time.
pub fn fmt_hours(hours: u64) -> String {
    let days = hours / 24;
    if days >= 365 {
        format!("{} y {} mo", days / 365, days % 365 / 30)
    } else if days > 0 {
        format!("{} d {} h", days, hours % 24)
    } else {
        format!("{hours} h")
    }
}

pub fn c_to_f(celsius: f32) -> f32 {
    celsius * 9.0 / 5.0 + 32.0
}

/// "54 °C" or "129 °F".
pub fn fmt_temp(celsius: f32, fahrenheit: bool) -> String {
    if fahrenheit {
        format!("{:.0} °F", c_to_f(celsius))
    } else {
        format!("{celsius:.0} °C")
    }
}

/// "4.20 GHz" or "800 MHz".
pub fn fmt_mhz(mhz: f32) -> String {
    if mhz >= 1000.0 {
        format!("{:.2} GHz", mhz / 1000.0)
    } else {
        format!("{mhz:.0} MHz")
    }
}

/// "12%": whole numbers, as a tile shows them.
pub fn fmt_pct(pct: f32) -> String {
    format!("{:.0}%", pct.clamp(0.0, 100.0))
}

/// Thousands separators: 1234567 -> "1,234,567".
pub fn fmt_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Seconds since the Unix epoch.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days since 1970-01-01 to (year, month, day). Proleptic Gregorian.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Minutes that local time is ahead of UTC right now.
/// Days since 1970-01-01 for a calendar date (the inverse of [`civil_from_days`]).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn local_offset_minutes() -> i64 {
    #[cfg(windows)]
    {
        use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        let mut tz = TIME_ZONE_INFORMATION::default();
        // SAFETY: plain out-parameter call.
        let id = unsafe { GetTimeZoneInformation(&mut tz) };
        let bias = match id {
            2 => tz.Bias + tz.DaylightBias,
            1 => tz.Bias + tz.StandardBias,
            _ => tz.Bias,
        };
        -i64::from(bias)
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// (year, month, day, hour, minute, second) in local time.
pub fn local_parts(unix: u64) -> (i64, u32, u32, u32, u32, u32) {
    let t = unix as i64 + local_offset_minutes() * 60;
    let (y, mo, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    (
        y,
        mo,
        d,
        (s / 3600) as u32,
        (s % 3600 / 60) as u32,
        (s % 60) as u32,
    )
}

/// "2026-11-16 08:12:45" in local time.
pub fn local_datetime(unix: u64) -> String {
    let (y, mo, d, h, mi, s) = local_parts(unix);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// "08:12:45" in local time.
pub fn local_time(unix: u64) -> String {
    let (_, _, _, h, mi, s) = local_parts(unix);
    format!("{h:02}:{mi:02}:{s:02}")
}

/// "2026-11-16" in local time.
pub fn local_date(unix: u64) -> String {
    let (y, mo, d, ..) = local_parts(unix);
    format!("{y:04}-{mo:02}-{d:02}")
}

/// "20261116-081245": safe in file names.
pub fn file_stamp(unix: u64) -> String {
    let (y, mo, d, h, mi, s) = local_parts(unix);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// UTF-16, NUL-terminated, for Win32 calls.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a NUL-terminated (or full-length) UTF-16 buffer.
pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_and_rates() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(1023), "1023 B");
        assert_eq!(fmt_bytes(1536), "1.50 KB");
        assert_eq!(fmt_bytes(16 << 30), "16.0 GB");
        assert_eq!(fmt_bytes(512 << 20), "512 MB");
        assert_eq!(fmt_rate(-5.0), "0 B/s");
        assert_eq!(fmt_rate(f64::NAN), "0 B/s");
        assert_eq!(fmt_rate(2.0 * 1024.0 * 1024.0), "2.00 MB/s");
        assert_eq!(fmt_bits(1_000_000_000), "1.0 Gbps");
        assert_eq!(fmt_bits(866_000_000), "866 Mbps");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(42), "42 s");
        assert_eq!(fmt_duration(725), "12 min 5 s");
        assert_eq!(fmt_duration(4 * 3600 + 12 * 60), "4 h 12 min");
        assert_eq!(
            fmt_duration(3 * 86_400 + 4 * 3600 + 12 * 60),
            "3 d 4 h 12 min"
        );
        assert_eq!(fmt_hours(17), "17 h");
        assert_eq!(fmt_hours(23 * 24 + 4), "23 d 4 h");
        assert_eq!(fmt_hours(12_480), "1 y 5 mo");
    }

    #[test]
    fn temperatures_and_numbers() {
        assert_eq!(fmt_temp(54.4, false), "54 °C");
        assert_eq!(fmt_temp(100.0, true), "212 °F");
        assert_eq!(fmt_mhz(4200.0), "4.20 GHz");
        assert_eq!(fmt_mhz(800.0), "800 MHz");
        assert_eq!(fmt_pct(123.0), "100%");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
        assert_eq!(fmt_count(12), "12");
    }

    #[test]
    fn calendar() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        for days in [-800_000, -1, 0, 59, 11_016, 20_833, 400_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn wide_strings() {
        assert_eq!(from_wide(&wide("héllo")), "héllo");
        assert_eq!(from_wide(&[104, 105]), "hi");
    }
}
