//! `--selftest`: runs every collector once against this machine and reports
//! which ones delivered data. Only counts and yes/no facts are printed, never
//! identifying values (no serial numbers, names, addresses or process lists).

use crate::collect;
use crate::model::{HealthStatus, SensorKind};
use crate::report::{self, Format, ReportData};
use crate::sampler::{self, RealSource};
use crate::util;
use std::time::{Duration, Instant};

/// One line of the self-test.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: &'static str,
    /// True when the collector delivered data.
    pub ok: bool,
    /// True when a miss means the program is broken (as opposed to hardware
    /// that simply does not expose the reading).
    pub required: bool,
    pub detail: String,
}

fn check(name: &'static str, required: bool, ok: bool, detail: String) -> Check {
    Check {
        name,
        ok,
        required,
        detail,
    }
}

fn in_range(v: f32) -> bool {
    (0.0..=100.0).contains(&v)
}

/// Runs all checks. The second value is false when a required check failed.
pub fn run() -> (Vec<Check>, bool) {
    let mut out = Vec::new();
    let elevated = crate::sys::is_elevated();
    out.push(check(
        "rights",
        false,
        elevated,
        if elevated {
            "running as administrator".into()
        } else {
            "standard rights (drive health details may need administrator rights)".into()
        },
    ));

    // Processor.
    let cpu = collect::cpu::info();
    out.push(check(
        "processor",
        true,
        !cpu.name.is_empty() && cpu.threads > 0,
        format!(
            "model {}, {} cores / {} threads, {} cache levels, {} features, clocks {}",
            if cpu.name.is_empty() {
                "missing"
            } else {
                "read"
            },
            cpu.cores,
            cpu.threads,
            cpu.caches.len(),
            cpu.features.len(),
            if cpu.base_mhz.is_some() || cpu.max_mhz.is_some() {
                "read"
            } else {
                "missing"
            }
        ),
    ));

    // Inventory pieces.
    let smbios = collect::smbios::read();
    out.push(match &smbios {
        Ok(s) => check(
            "firmware tables",
            false,
            true,
            format!(
                "computer {}, board {}, firmware {}, {} memory slots ({} filled)",
                yes_no(!s.system.product.is_empty() || !s.system.manufacturer.is_empty()),
                yes_no(!s.system.board_product.is_empty()),
                yes_no(!s.system.bios_version.is_empty()),
                s.modules.len(),
                s.modules.iter().filter(|m| m.populated).count()
            ),
        ),
        Err(e) => check("firmware tables", false, false, e.clone()),
    });
    let system = collect::system::info();
    out.push(check(
        "system",
        true,
        !system.os_name.is_empty() && !system.os_build.is_empty(),
        format!(
            "Windows version {}, install date {}, Secure Boot {}, TPM {}, firmware mode {}",
            yes_no(!system.os_build.is_empty()),
            yes_no(!system.os_install_date.is_empty()),
            known(system.secure_boot.is_some()),
            known(system.tpm_present.is_some()),
            known(!system.firmware_mode.is_empty())
        ),
    ));
    let displays = collect::displays::list();
    out.push(check(
        "displays",
        false,
        !displays.is_empty() && displays.iter().all(|d| d.width > 0 && d.height > 0),
        format!(
            "{} found, names {}, refresh {}, scaling {}, HDR state {}",
            displays.len(),
            yes_no(displays.iter().any(|d| !d.name.is_empty())),
            yes_no(displays.iter().any(|d| d.refresh_hz > 0.0)),
            yes_no(displays.iter().any(|d| d.scale_pct > 0)),
            known(displays.iter().any(|d| d.hdr_supported.is_some()))
        ),
    ));
    let gpus = collect::gpu::adapters();
    let hardware_gpus = gpus.iter().filter(|g| !g.software).count();
    out.push(check(
        "graphics adapters",
        false,
        hardware_gpus > 0,
        format!(
            "{} hardware, {} software, driver version {}",
            hardware_gpus,
            gpus.len() - hardware_gpus,
            yes_no(gpus.iter().any(|g| !g.driver_version.is_empty()))
        ),
    ));

    // Drives and volumes.
    let slow = sampler::collect_slow();
    let drives = &slow.drives;
    out.push(check(
        "drives",
        true,
        !drives.is_empty() && drives.iter().any(|d| d.size_bytes > 0),
        format!(
            "{} found, model {}, partitions {}",
            drives.len(),
            yes_no(drives.iter().any(|d| !d.model.is_empty())),
            yes_no(drives.iter().any(|d| !d.partitions.is_empty()))
        ),
    ));
    let with_attributes = drives.iter().filter(|d| !d.attributes.is_empty()).count();
    let with_verdict = drives
        .iter()
        .filter(|d| d.health != HealthStatus::Unknown)
        .count();
    let with_temp = drives.iter().filter(|d| d.temperature_c.is_some()).count();
    let noted = drives.iter().filter(|d| !d.smart_note.is_empty()).count();
    out.push(check(
        "drive health",
        false,
        with_attributes > 0 || with_verdict > 0,
        format!(
            "attributes for {with_attributes} of {n}, verdict for {with_verdict}, temperature for {with_temp}, {noted} with a note (rights or unsupported)",
            n = drives.len()
        ),
    ));
    out.push(check(
        "volumes",
        true,
        slow.volumes.iter().any(|v| v.total > 0),
        format!(
            "{} found, {} mapped to a drive, encryption state known for {}",
            slow.volumes.len(),
            slow.volumes
                .iter()
                .filter(|v| v.drive_index.is_some())
                .count(),
            slow.volumes
                .iter()
                .filter(|v| v.bitlocker.is_some())
                .count()
        ),
    ));

    // Live readings: two samples, so that rates and loads exist.
    let mut source = RealSource::new();
    source.sample(true, &slow);
    std::thread::sleep(Duration::from_millis(600));
    let s = source.sample(true, &slow);
    out.push(check(
        "processor load",
        true,
        in_range(s.cpu.total_pct)
            && !s.cpu.per_core_pct.is_empty()
            && s.cpu.per_core_pct.iter().all(|v| in_range(*v)),
        format!(
            "total and {} per-core values in range, counts {}",
            s.cpu.per_core_pct.len(),
            yes_no(s.cpu.processes > 0 && s.cpu.threads > 0)
        ),
    ));
    out.push(check(
        "processor clock",
        false,
        s.cpu.current_mhz.is_some(),
        if s.cpu.current_mhz.is_some() {
            format!("live, {} per-core values", s.cpu.per_core_mhz.len())
        } else {
            "not reported".into()
        },
    ));
    out.push(check(
        "processor temperature",
        false,
        s.cpu.temperature_c.is_some(),
        if s.cpu.temperature_c.is_some() {
            format!("available ({})", s.cpu.temperature_note)
        } else {
            s.cpu.temperature_note.clone()
        },
    ));
    let m = &s.memory;
    out.push(check(
        "memory",
        true,
        m.total > 0 && m.used <= m.total && m.commit_limit > 0,
        format!(
            "usage and commit read, page file {}, standby/modified lists {}, compressed {}",
            yes_no(m.pagefile_total > 0),
            known(m.standby.is_some()),
            known(m.compressed.is_some())
        ),
    ));
    out.push(check(
        "graphics load",
        false,
        !s.gpus.is_empty() && s.gpus.iter().all(|g| in_range(g.load_pct)),
        format!(
            "{} adapters, engines {}, memory use {}, per-process use {}",
            s.gpus.len(),
            yes_no(s.gpus.iter().any(|g| !g.engines.is_empty())),
            yes_no(s.gpus.iter().any(|g| g.dedicated_used + g.shared_used > 0)),
            yes_no(s.processes.iter().any(|p| p.gpu_pct.is_some()))
        ),
    ));
    let gpu_temp = s.gpus.iter().any(|g| g.temperature_c.is_some());
    out.push(check(
        "graphics temperature",
        false,
        gpu_temp,
        if gpu_temp {
            "available from a vendor library".into()
        } else {
            s.gpus
                .first()
                .map(|g| g.temperature_note.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| crate::model::NO_DRIVER.into())
        },
    ));
    out.push(check(
        "drive activity",
        false,
        !s.disks.is_empty()
            && s.disks
                .iter()
                .all(|d| d.read_bps >= 0.0 && d.write_bps >= 0.0 && in_range(d.active_pct)),
        format!("{} drives with live rates", s.disks.len()),
    ));
    out.push(check(
        "network",
        false,
        !s.net.is_empty() && s.net.iter().all(|n| n.rx_bps >= 0.0 && n.tx_bps >= 0.0),
        format!(
            "{} adapters ({} up), addresses {}",
            s.net.len(),
            s.net.iter().filter(|n| n.up).count(),
            yes_no(
                s.net
                    .iter()
                    .any(|n| !n.ipv4.is_empty() || !n.ipv6.is_empty())
            )
        ),
    ));
    out.push(check(
        "processes",
        true,
        s.processes.len() > 5
            && s.processes.iter().all(|p| in_range(p.cpu_pct))
            && s.processes.iter().any(|p| p.memory > 0),
        format!(
            "{} listed, processor share, memory and I/O rates read",
            s.processes.len()
        ),
    ));
    out.push(match &s.battery {
        Some(b) => check(
            "battery",
            false,
            in_range(b.percent),
            format!(
                "present, capacities {}, cycle count {}, rate {}, wear {}",
                known(b.design_mwh.is_some() && b.full_mwh.is_some()),
                known(b.cycle_count.is_some()),
                known(b.rate_mw.is_some()),
                known(b.wear_pct.is_some())
            ),
        ),
        None => check(
            "battery",
            false,
            false,
            "no battery in this computer".into(),
        ),
    });
    let count = |kind: SensorKind, present: bool| {
        s.sensors
            .iter()
            .filter(|r| r.kind == kind && r.value.is_some() == present)
            .count()
    };
    out.push(check(
        "sensors",
        false,
        s.sensors.iter().any(|r| r.value.is_some()),
        format!(
            "temperatures {} read / {} unavailable, fans {} / {}, voltages {} / {}",
            count(SensorKind::Temperature, true),
            count(SensorKind::Temperature, false),
            count(SensorKind::Fan, true),
            count(SensorKind::Fan, false),
            count(SensorKind::Voltage, true),
            count(SensorKind::Voltage, false)
        ),
    ));

    // Report: all three formats, and masking must hide the names.
    let inventory = sampler::collect_inventory();
    let data = ReportData {
        inventory: &inventory,
        slow: &slow,
        sample: &s,
        generated: util::now_unix(),
        fahrenheit: false,
    };
    let html = report::render(&data, Format::Html, true);
    let text = report::render(&data, Format::Text, true);
    let json = report::render(&data, Format::Json, true);
    let json_ok = serde_json::from_str::<serde_json::Value>(&json).is_ok();
    let secrets: Vec<&str> = [
        inventory.system.computer_name.as_str(),
        inventory.system.user_name.as_str(),
        inventory.system.system_serial.as_str(),
        inventory.system.uuid.as_str(),
    ]
    .into_iter()
    .chain(slow.drives.iter().map(|d| d.serial.as_str()))
    .filter(|v| v.chars().count() >= 5)
    .collect();
    // The fixed author credit in the footer is not machine data: take it out
    // before looking for leaks (it may share a word with a user name).
    let strip = |t: &str| t.replace("Zheer Khalid", "").replace("zheer.dev", "");
    let (html_body, text_body, json_body) = (strip(&html), strip(&text), strip(&json));
    let leaked = secrets
        .iter()
        .filter(|v| html_body.contains(**v) || text_body.contains(**v) || json_body.contains(**v))
        .count();
    out.push(check(
        "report",
        true,
        !html.is_empty() && !text.is_empty() && json_ok && leaked == 0,
        format!(
            "HTML {}, text {}, JSON {}, masking {} ({} identifying values checked)",
            yes_no(!html.is_empty()),
            yes_no(!text.is_empty()),
            if json_ok { "valid" } else { "invalid" },
            if leaked == 0 { "holds" } else { "LEAKS" },
            secrets.len()
        ),
    ));

    // Cost of one reading without the process list (what runs while minimised),
    // taken at a realistic spacing: wall time and processor time of the whole process.
    let rounds = 10u32;
    let mut wall_ms = 0.0_f64;
    let (cpu_before, _) = crate::sys::self_usage();
    for _ in 0..rounds {
        std::thread::sleep(Duration::from_millis(250));
        let began = Instant::now();
        source.sample(false, &slow);
        wall_ms += began.elapsed().as_secs_f64() * 1000.0;
    }
    let (cpu_after, memory) = crate::sys::self_usage();
    let wall_ms = wall_ms / f64::from(rounds);
    let cpu_ms = cpu_after.saturating_sub(cpu_before) as f64 / 10_000.0 / f64::from(rounds);
    let began = Instant::now();
    source.sample(true, &slow);
    let detail_ms = began.elapsed().as_secs_f64() * 1000.0;
    let machine_pct = cpu_ms / 10.0 / f64::from(crate::sys::logical_processors());
    out.push(check(
        "cost per reading",
        false,
        cpu_ms < 20.0,
        format!(
            "{cpu_ms:.2} ms of processor time ({wall_ms:.2} ms elapsed) without the process list, about {machine_pct:.3}% of this machine at one reading per second; {detail_ms:.1} ms elapsed with the process list; {} private memory in this run",
            util::fmt_bytes(memory)
        ),
    ));

    let passed = out.iter().all(|c| c.ok || !c.required);
    (out, passed)
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

fn known(v: bool) -> &'static str {
    if v {
        "known"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_checks_decide_the_result() {
        let a = check("a", true, true, String::new());
        let b = check("b", false, false, String::new());
        let c = check("c", true, false, String::new());
        assert!([a.clone(), b.clone()].iter().all(|c| c.ok || !c.required));
        assert!(![a, b, c].iter().all(|c| c.ok || !c.required));
        assert_eq!(yes_no(true), "yes");
        assert_eq!(known(false), "unknown");
        assert!(in_range(0.0) && in_range(100.0) && !in_range(100.5) && !in_range(f32::NAN));
    }
}
