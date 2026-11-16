//! Temperature, fan and voltage sensors that Windows exposes without a
//! kernel driver (ACPI thermal zones and whatever else the firmware offers).
//!
//! Two sources are used. The thermal-zone performance counters are read on
//! every call; they work without administrator rights on many machines. WMI
//! is asked once, on a helper thread, for the ACPI zones (usually only
//! answered for administrators) and for the fan, temperature and voltage
//! probes of the firmware; a class that really delivers numbers is asked
//! again every ten seconds, never more often.
//!
//! Per-core temperatures, fan speeds and board voltages live behind chip
//! registers that only a kernel driver can read. They are listed as missing,
//! with the reason, instead of being guessed.

use super::pdh;
use super::wmi::{self, Row, Value};
use crate::model::{SensorKind, SensorReading, NEEDS_ADMIN, NO_DRIVER};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ACPI_GROUP: &str = "ACPI";
const ACPI_SOURCE: &str = "ACPI thermal zone";
const FIRMWARE_SOURCE: &str = "Firmware probe (WMI)";
/// How often a WMI class with live values is asked again.
const WMI_PERIOD: Duration = Duration::from_secs(10);

// ----------------------------------------------------------------------------------
// Pure helpers
// ----------------------------------------------------------------------------------

/// True for a temperature a PC part can really have.
pub fn plausible_celsius(celsius: f64) -> bool {
    celsius.is_finite() && (-20.0..=150.0).contains(&celsius)
}

/// Whole kelvin to degrees Celsius; `None` for zero and for bogus values.
pub fn kelvin_to_celsius(kelvin: f64) -> Option<f64> {
    if !kelvin.is_finite() || kelvin <= 0.0 {
        return None;
    }
    let celsius = kelvin - 273.15;
    plausible_celsius(celsius).then_some(celsius)
}

/// Tenths of a kelvin (the unit ACPI uses) to degrees Celsius.
pub fn tenth_kelvin_to_celsius(tenths: f64) -> Option<f64> {
    if !tenths.is_finite() || tenths <= 0.0 {
        return None;
    }
    kelvin_to_celsius(tenths / 10.0)
}

/// A short name for a thermal zone from its counter instance (`\_TZ.THM0`)
/// or its WMI instance name (`ACPI\ThermalZone\THM0_0`).
pub fn zone_name(instance: &str) -> String {
    let last = instance
        .trim()
        .rsplit(['\\', '.'])
        .find(|part| !part.is_empty())
        .unwrap_or("");
    // WMI appends an instance number.
    let last = match last.rsplit_once('_') {
        Some((head, tail))
            if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => last,
    };
    let last = last.trim_matches('_');
    if last.is_empty() {
        "Thermal zone".to_string()
    } else {
        format!("Thermal zone {last}")
    }
}

/// True when a zone name suggests it measures the processor.
pub fn is_cpu_zone(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["CPU", "PROC", "PKG", "CORE", "B0D4"]
        .iter()
        .any(|hint| upper.contains(hint))
}

fn acpi_reading(instance: &str, celsius: f64) -> SensorReading {
    SensorReading {
        group: ACPI_GROUP.to_string(),
        name: zone_name(instance),
        kind: SensorKind::Temperature,
        value: Some(celsius),
        source: ACPI_SOURCE.to_string(),
        note: String::new(),
    }
}

/// Merges the two thermal-zone counters into readings: the high-precision
/// value (tenths of a kelvin) wins, the whole-kelvin value fills in where
/// the precise one is missing or bogus. Zones with no usable value are dropped.
pub fn zones_from_counters(
    precise: &[(String, f64)],
    whole: &[(String, f64)],
) -> Vec<SensorReading> {
    let mut out: Vec<SensorReading> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (instance, tenths) in precise {
        if seen.contains(&instance.as_str()) {
            continue;
        }
        let value = tenth_kelvin_to_celsius(*tenths).or_else(|| {
            whole
                .iter()
                .find(|(other, _)| other == instance)
                .and_then(|(_, kelvin)| kelvin_to_celsius(*kelvin))
        });
        if let Some(celsius) = value {
            seen.push(instance);
            out.push(acpi_reading(instance, celsius));
        }
    }
    for (instance, kelvin) in whole {
        if seen.contains(&instance.as_str()) {
            continue;
        }
        if let Some(celsius) = kelvin_to_celsius(*kelvin) {
            seen.push(instance);
            out.push(acpi_reading(instance, celsius));
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The WMI classes that are probed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WmiClass {
    AcpiZones,
    Fans,
    TemperatureProbes,
    VoltageProbes,
}

impl WmiClass {
    const ALL: [WmiClass; 4] = [
        WmiClass::AcpiZones,
        WmiClass::Fans,
        WmiClass::TemperatureProbes,
        WmiClass::VoltageProbes,
    ];

    fn namespace(self) -> &'static str {
        match self {
            WmiClass::AcpiZones => r"root\wmi",
            _ => r"root\cimv2",
        }
    }

    fn query(self) -> &'static str {
        match self {
            WmiClass::AcpiZones => {
                "SELECT InstanceName, CurrentTemperature FROM MSAcpi_ThermalZoneTemperature"
            }
            WmiClass::Fans => "SELECT Name, DeviceID, DesiredSpeed FROM Win32_Fan",
            WmiClass::TemperatureProbes => {
                "SELECT Name, Description, CurrentReading FROM Win32_TemperatureProbe"
            }
            WmiClass::VoltageProbes => {
                "SELECT Name, Description, CurrentReading FROM Win32_VoltageProbe"
            }
        }
    }
}

fn row_label(row: &Row, fallback: &str, index: usize) -> String {
    ["Name", "Description", "DeviceID"]
        .iter()
        .filter_map(|key| row.get(*key).and_then(Value::as_text))
        .find(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{fallback} {}", index + 1))
}

/// Turns the rows of one WMI class into readings. Rows without a usable
/// number give nothing: most firmware lists its probes without values.
pub fn readings_from_rows(class: WmiClass, rows: &[Row]) -> Vec<SensorReading> {
    let mut out = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let number = |key: &str| row.get(key).and_then(Value::as_f64);
        match class {
            WmiClass::AcpiZones => {
                let instance = row
                    .get("InstanceName")
                    .and_then(Value::as_text)
                    .unwrap_or("");
                if let Some(celsius) =
                    number("CurrentTemperature").and_then(tenth_kelvin_to_celsius)
                {
                    out.push(acpi_reading(instance, celsius));
                }
            }
            WmiClass::Fans => {
                if let Some(rpm) = number("DesiredSpeed").filter(|r| (1.0..=50_000.0).contains(r)) {
                    out.push(SensorReading {
                        group: "Motherboard".to_string(),
                        name: row_label(row, "Fan", index),
                        kind: SensorKind::Fan,
                        value: Some(rpm),
                        source: FIRMWARE_SOURCE.to_string(),
                        note: "speed requested by the firmware, not a tachometer reading"
                            .to_string(),
                    });
                }
            }
            WmiClass::TemperatureProbes => {
                if let Some(celsius) = number("CurrentReading").and_then(tenth_kelvin_to_celsius) {
                    out.push(SensorReading {
                        group: "Motherboard".to_string(),
                        name: row_label(row, "Temperature probe", index),
                        kind: SensorKind::Temperature,
                        value: Some(celsius),
                        source: FIRMWARE_SOURCE.to_string(),
                        note: String::new(),
                    });
                }
            }
            WmiClass::VoltageProbes => {
                // Millivolts; anything outside what a PC rail carries is noise.
                if let Some(millivolts) =
                    number("CurrentReading").filter(|mv| mv.abs() >= 1.0 && mv.abs() <= 30_000.0)
                {
                    out.push(SensorReading {
                        group: "Motherboard".to_string(),
                        name: row_label(row, "Voltage probe", index),
                        kind: SensorKind::Voltage,
                        value: Some(millivolts / 1000.0),
                        source: FIRMWARE_SOURCE.to_string(),
                        note: String::new(),
                    });
                }
            }
        }
    }
    out
}

fn missing(group: &str, name: &str, kind: SensorKind, note: &str) -> SensorReading {
    SensorReading {
        group: group.to_string(),
        name: name.to_string(),
        kind,
        value: None,
        source: String::new(),
        note: note.to_string(),
    }
}

/// The final list: the ACPI zones (from the counters, else from WMI), the
/// firmware probes, then one line for each well-known reading that is missing.
pub fn compose(
    counter_zones: Vec<SensorReading>,
    wmi_zones: &[SensorReading],
    wmi_other: &[SensorReading],
    zones_need_admin: bool,
) -> Vec<SensorReading> {
    let mut out = counter_zones;
    if out.is_empty() {
        out.extend_from_slice(wmi_zones);
    }
    let have_zones = !out.is_empty();
    out.extend_from_slice(wmi_other);
    let has = |list: &[SensorReading], kind: SensorKind| {
        list.iter().any(|r| r.kind == kind && r.value.is_some())
    };
    let have_fans = has(&out, SensorKind::Fan);
    let have_voltages = has(&out, SensorKind::Voltage);

    if !have_zones {
        let note = if zones_need_admin {
            NEEDS_ADMIN
        } else {
            "not reported by the firmware of this PC"
        };
        out.push(missing(
            ACPI_GROUP,
            "Thermal zones",
            SensorKind::Temperature,
            note,
        ));
    }
    out.push(missing(
        "Processor",
        "Core temperatures",
        SensorKind::Temperature,
        NO_DRIVER,
    ));
    if !have_fans {
        out.push(missing(
            "Motherboard",
            "Fan speeds",
            SensorKind::Fan,
            NO_DRIVER,
        ));
    }
    if !have_voltages {
        out.push(missing(
            "Motherboard",
            "Voltages",
            SensorKind::Voltage,
            NO_DRIVER,
        ));
    }
    out
}

/// Picks the reading that best stands for "processor temperature" out of the
/// list from [`SensorSampler::sample`], with a note that says what it really
/// is (for example "ACPI thermal zone, not a per-core reading"). `None` when
/// nothing suitable is exposed.
pub fn cpu_temperature(readings: &[SensorReading]) -> Option<(f32, String)> {
    let zones = || {
        readings.iter().filter(|r| {
            r.kind == SensorKind::Temperature
                && r.group == ACPI_GROUP
                && r.value.is_some_and(plausible_celsius)
        })
    };
    let hottest = |a: &&SensorReading, b: &&SensorReading| {
        a.value
            .unwrap_or(f64::MIN)
            .total_cmp(&b.value.unwrap_or(f64::MIN))
    };
    let best = zones()
        .filter(|r| is_cpu_zone(&r.name))
        .max_by(hottest)
        .or_else(|| zones().max_by(hottest))?;
    let value = best.value? as f32;
    let note = if is_cpu_zone(&best.name) {
        format!("ACPI thermal zone ({}), not a per-core reading", best.name)
    } else {
        format!(
            "ACPI thermal zone ({}, the warmest one), not a per-core reading",
            best.name
        )
    };
    Some((value, note))
}

// ----------------------------------------------------------------------------------
// Reading this machine
// ----------------------------------------------------------------------------------

struct Zones {
    query: pdh::Query,
    precise: Option<pdh::Counter>,
    whole: Option<pdh::Counter>,
}

fn open_zone_counters() -> Option<Zones> {
    let mut query = pdh::Query::open().ok()?;
    let precise = query
        .add(r"\Thermal Zone Information(*)\High Precision Temperature")
        .ok();
    let whole = query.add(r"\Thermal Zone Information(*)\Temperature").ok();
    if precise.is_none() && whole.is_none() {
        return None;
    }
    Some(Zones {
        query,
        precise,
        whole,
    })
}

/// What the WMI helper thread has found so far.
#[derive(Default)]
struct WmiFindings {
    zones: Vec<SensorReading>,
    other: Vec<SensorReading>,
    zones_need_admin: bool,
    /// True once the first round is complete.
    probed: bool,
}

/// Asks every class in `classes` once and returns (zones, other readings,
/// zones need admin, classes that delivered numbers).
fn probe_wmi(
    classes: &[WmiClass],
) -> (Vec<SensorReading>, Vec<SensorReading>, bool, Vec<WmiClass>) {
    let (mut zones, mut other, mut need_admin, mut live) =
        (Vec::new(), Vec::new(), false, Vec::new());
    for &class in classes {
        match wmi::query(class.namespace(), class.query()) {
            Ok(rows) => {
                let readings = readings_from_rows(class, &rows);
                if !readings.is_empty() {
                    live.push(class);
                }
                if class == WmiClass::AcpiZones {
                    zones = readings;
                } else {
                    other.extend(readings);
                }
            }
            Err(error) => {
                if class == WmiClass::AcpiZones && wmi::is_access_denied(&error) {
                    need_admin = true;
                }
            }
        }
    }
    (zones, other, need_admin, live)
}

fn wmi_thread(shared: Arc<Mutex<WmiFindings>>, stop: mpsc::Receiver<()>) {
    let mut classes: Vec<WmiClass> = WmiClass::ALL.to_vec();
    loop {
        let (zones, other, need_admin, live) = probe_wmi(&classes);
        {
            let mut findings = shared.lock().unwrap_or_else(|e| e.into_inner());
            if !findings.probed {
                findings.zones_need_admin = need_admin;
            }
            findings.zones = zones;
            findings.other = other;
            findings.probed = true;
        }
        // Only classes that deliver numbers are worth asking again.
        classes = live;
        if classes.is_empty() {
            return;
        }
        match stop.recv_timeout(WMI_PERIOD) {
            Err(RecvTimeoutError::Timeout) => {}
            // The sampler is gone (or asked to stop).
            _ => return,
        }
    }
}

#[derive(Default)]
pub struct SensorSampler {
    started: bool,
    zones: Option<Zones>,
    precise: Vec<(String, f64)>,
    whole: Vec<(String, f64)>,
    wmi: Arc<Mutex<WmiFindings>>,
    /// Dropping this ends the WMI helper thread at its next wake-up.
    stop: Option<mpsc::Sender<()>>,
}

impl SensorSampler {
    pub fn new() -> SensorSampler {
        SensorSampler::default()
    }

    fn start(&mut self) {
        self.started = true;
        self.zones = open_zone_counters();
        let (stop, wake) = mpsc::channel();
        let shared = Arc::clone(&self.wmi);
        let spawned = std::thread::Builder::new()
            .name("pulseforge-wmi".into())
            .spawn(move || wmi_thread(shared, wake));
        if spawned.is_ok() {
            self.stop = Some(stop);
        }
    }

    fn read_zones(&mut self) -> Vec<SensorReading> {
        self.precise.clear();
        self.whole.clear();
        let Some(zones) = &mut self.zones else {
            return Vec::new();
        };
        if !zones.query.collect() {
            return Vec::new();
        }
        if let Some(counter) = zones.precise {
            let list = &mut self.precise;
            let _ = zones
                .query
                .read_array(counter, |name, value| list.push((name.to_string(), value)));
        }
        if let Some(counter) = zones.whole {
            let list = &mut self.whole;
            let _ = zones
                .query
                .read_array(counter, |name, value| list.push((name.to_string(), value)));
        }
        zones_from_counters(&self.precise, &self.whole)
    }

    /// Everything found, plus one line per well-known reading that is missing
    /// (per-core temperatures, fan speeds, voltages) with the reason in `note`.
    /// Drive, battery and GPU readings are added by the sampler, not here.
    pub fn sample(&mut self) -> Vec<SensorReading> {
        if !self.started {
            self.start();
        }
        let zones = self.read_zones();
        let findings = self.wmi.lock().unwrap_or_else(|e| e.into_inner());
        compose(
            zones,
            &findings.zones,
            &findings.other,
            findings.zones_need_admin,
        )
    }

    /// True once the one-time WMI probe has finished.
    pub fn wmi_probed(&self) -> bool {
        self.wmi.lock().unwrap_or_else(|e| e.into_inner()).probed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|v| (v - b).abs() < 1e-6)
    }

    #[test]
    fn unit_conversion_and_bogus_values() {
        assert!(close(kelvin_to_celsius(300.0), 26.85));
        assert!(close(kelvin_to_celsius(273.15), 0.0));
        assert!(close(tenth_kelvin_to_celsius(3012.0), 28.05));
        assert!(close(tenth_kelvin_to_celsius(3732.0), 100.05));

        // 0 K is what a zone without a sensor reports.
        assert_eq!(kelvin_to_celsius(0.0), None);
        assert_eq!(tenth_kelvin_to_celsius(0.0), None);
        // Below -20 C and above 150 C are not readings.
        assert_eq!(kelvin_to_celsius(250.0), None);
        assert_eq!(kelvin_to_celsius(430.0), None);
        assert_eq!(tenth_kelvin_to_celsius(2000.0), None);
        assert_eq!(tenth_kelvin_to_celsius(65_535.0), None);
        assert_eq!(kelvin_to_celsius(-5.0), None);
        assert_eq!(kelvin_to_celsius(f64::NAN), None);
        assert_eq!(tenth_kelvin_to_celsius(f64::INFINITY), None);
        // The limits themselves count.
        assert!(kelvin_to_celsius(253.15).is_some());
        assert!(kelvin_to_celsius(423.15).is_some());
        assert!(plausible_celsius(-20.0) && plausible_celsius(150.0));
        assert!(!plausible_celsius(150.1) && !plausible_celsius(-20.1));
    }

    #[test]
    fn zone_names() {
        assert_eq!(zone_name(r"\_TZ.THM0"), "Thermal zone THM0");
        assert_eq!(zone_name(r"\_TZ.TZ00"), "Thermal zone TZ00");
        assert_eq!(zone_name(r"ACPI\ThermalZone\THM0_0"), "Thermal zone THM0");
        assert_eq!(zone_name(r"ACPI\ThermalZone\CPUZ_0"), "Thermal zone CPUZ");
        assert_eq!(zone_name(r"\_SB.PCI0.LPCB.EC0.TZ_1"), "Thermal zone TZ");
        assert_eq!(zone_name(""), "Thermal zone");
        assert_eq!(zone_name(r"\\.."), "Thermal zone");
        assert_eq!(zone_name("_0"), "Thermal zone 0");
        assert!(is_cpu_zone("Thermal zone CPUZ"));
        assert!(is_cpu_zone("Thermal zone tcpu"));
        assert!(is_cpu_zone("Thermal zone B0D4"));
        assert!(!is_cpu_zone("Thermal zone THM0"));
        assert!(!is_cpu_zone(""));
    }

    fn pairs(list: &[(&str, f64)]) -> Vec<(String, f64)> {
        list.iter().map(|(n, v)| ((*n).to_string(), *v)).collect()
    }

    #[test]
    fn counters_merge_into_zones() {
        let precise = pairs(&[
            (r"\_TZ.THM0", 3182.0),
            (r"\_TZ.CPUZ", 0.0),
            (r"\_TZ.DEAD", 0.0),
            (r"\_TZ.HOT", 9999.0),
        ]);
        let whole = pairs(&[
            (r"\_TZ.THM0", 318.0),
            (r"\_TZ.CPUZ", 330.0),
            (r"\_TZ.DEAD", 0.0),
            (r"\_TZ.HOT", 1000.0),
            (r"\_TZ.ONLY", 300.0),
        ]);
        let zones = zones_from_counters(&precise, &whole);
        let names: Vec<&str> = zones.iter().map(|z| z.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Thermal zone CPUZ",
                "Thermal zone ONLY",
                "Thermal zone THM0"
            ]
        );
        assert!(close(zones[2].value, 45.05), "the precise value wins");
        assert!(close(zones[0].value, 56.85), "whole kelvin fills in");
        assert!(zones
            .iter()
            .all(|z| z.group == "ACPI" && z.source == "ACPI thermal zone"));
        assert!(zones
            .iter()
            .all(|z| z.kind == SensorKind::Temperature && z.note.is_empty()));

        assert!(zones_from_counters(&[], &[]).is_empty());
        let bogus = pairs(&[("a", f64::NAN), ("b", -1.0), ("c", 1e12)]);
        assert!(zones_from_counters(&bogus, &bogus).is_empty());
        // The same instance twice is listed once.
        let twice = pairs(&[("z", 3000.0), ("z", 3100.0)]);
        assert_eq!(zones_from_counters(&twice, &twice).len(), 1);
    }

    fn row(items: &[(&str, Value)]) -> Row {
        items
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn wmi_rows_become_readings() {
        let zones = readings_from_rows(
            WmiClass::AcpiZones,
            &[
                row(&[
                    (
                        "InstanceName",
                        Value::Text(r"ACPI\ThermalZone\THM0_0".into()),
                    ),
                    ("CurrentTemperature", Value::Int(3032)),
                ]),
                row(&[
                    (
                        "InstanceName",
                        Value::Text(r"ACPI\ThermalZone\TZ01_0".into()),
                    ),
                    ("CurrentTemperature", Value::Int(0)),
                ]),
                row(&[("CurrentTemperature", Value::Text("3100".into()))]),
                row(&[("InstanceName", Value::Null)]),
                row(&[]),
            ],
        );
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].name, "Thermal zone THM0");
        assert!(close(zones[0].value, 30.05));
        assert_eq!(zones[1].name, "Thermal zone");

        // Probes that are listed without a value (the usual case) give nothing.
        let empty_probe = row(&[
            ("Name", Value::Text("Numeric Sensor 1".into())),
            ("CurrentReading", Value::Null),
        ]);
        assert!(readings_from_rows(
            WmiClass::TemperatureProbes,
            std::slice::from_ref(&empty_probe)
        )
        .is_empty());
        assert!(readings_from_rows(WmiClass::VoltageProbes, &[empty_probe]).is_empty());
        assert!(readings_from_rows(
            WmiClass::Fans,
            &[row(&[("Name", Value::Text("Cooling Device".into()))])]
        )
        .is_empty());

        let volts = readings_from_rows(
            WmiClass::VoltageProbes,
            &[
                row(&[
                    ("Name", Value::Text("12V rail".into())),
                    ("CurrentReading", Value::Int(12_096)),
                ]),
                row(&[("CurrentReading", Value::Int(3312))]),
                row(&[("CurrentReading", Value::Int(0))]),
                row(&[("CurrentReading", Value::Int(i64::MAX))]),
            ],
        );
        assert_eq!(volts.len(), 2);
        assert!(close(volts[0].value, 12.096));
        assert_eq!(volts[0].kind, SensorKind::Voltage);
        assert_eq!(volts[1].name, "Voltage probe 2");

        let fans = readings_from_rows(
            WmiClass::Fans,
            &[
                row(&[
                    ("Name", Value::Text("Fan A".into())),
                    ("DesiredSpeed", Value::Text("1200".into())),
                ]),
                row(&[("DesiredSpeed", Value::Text("0".into()))]),
                row(&[("DesiredSpeed", Value::Text("garbage".into()))]),
                row(&[("DesiredSpeed", Value::Int(9_999_999))]),
            ],
        );
        assert_eq!(fans.len(), 1);
        assert_eq!(fans[0].value, Some(1200.0));
        assert!(fans[0].note.contains("not a tachometer"));

        let temps = readings_from_rows(
            WmiClass::TemperatureProbes,
            &[row(&[
                ("Description", Value::Text("Board probe".into())),
                ("CurrentReading", Value::Int(3081)),
            ])],
        );
        assert_eq!(temps.len(), 1);
        assert_eq!(temps[0].name, "Board probe");
        assert_eq!(temps[0].group, "Motherboard");
    }

    #[test]
    fn missing_readings_are_listed_with_the_reason() {
        // Nothing found at all.
        let bare = compose(Vec::new(), &[], &[], false);
        let line = |list: &[SensorReading], group: &str, name: &str| {
            list.iter()
                .find(|r| r.group == group && r.name == name)
                .cloned()
        };
        let cores = line(&bare, "Processor", "Core temperatures").expect("listed");
        assert_eq!((cores.value, cores.note.as_str()), (None, NO_DRIVER));
        let fans = line(&bare, "Motherboard", "Fan speeds").expect("listed");
        assert_eq!(
            (fans.kind, fans.note.as_str()),
            (SensorKind::Fan, NO_DRIVER)
        );
        let volts = line(&bare, "Motherboard", "Voltages").expect("listed");
        assert_eq!(
            (volts.kind, volts.note.as_str()),
            (SensorKind::Voltage, NO_DRIVER)
        );
        let zones = line(&bare, "ACPI", "Thermal zones").expect("listed");
        assert!(zones.value.is_none() && !zones.note.is_empty());
        assert_eq!(bare.len(), 4);
        assert!(bare.iter().all(|r| r.value.is_none()));
        assert_eq!(cpu_temperature(&bare), None);

        let denied = compose(Vec::new(), &[], &[], true);
        assert_eq!(
            line(&denied, "ACPI", "Thermal zones").expect("listed").note,
            NEEDS_ADMIN
        );

        // Real readings replace their placeholder and nothing else.
        let zone = acpi_reading(r"\_TZ.THM0", 41.0);
        let fan = SensorReading {
            group: "Motherboard".into(),
            name: "Fan A".into(),
            kind: SensorKind::Fan,
            value: Some(900.0),
            ..Default::default()
        };
        let full = compose(vec![zone.clone()], &[], &[fan], false);
        assert!(line(&full, "ACPI", "Thermal zones").is_none());
        assert!(line(&full, "Motherboard", "Fan speeds").is_none());
        assert!(line(&full, "Motherboard", "Voltages").is_some());
        assert!(line(&full, "Processor", "Core temperatures").is_some());
        assert_eq!(full[0], zone);

        // Counter zones win over WMI zones; WMI zones are the fallback.
        let wmi_zone = acpi_reading(r"ACPI\ThermalZone\WMIZ_0", 50.0);
        let both = compose(
            vec![zone.clone()],
            std::slice::from_ref(&wmi_zone),
            &[],
            false,
        );
        assert_eq!(both.iter().filter(|r| r.group == "ACPI").count(), 1);
        let fallback = compose(Vec::new(), std::slice::from_ref(&wmi_zone), &[], true);
        assert_eq!(fallback[0], wmi_zone);
        assert!(line(&fallback, "ACPI", "Thermal zones").is_none());
    }

    #[test]
    fn processor_stand_in() {
        let list = compose(
            vec![
                acpi_reading(r"\_TZ.THM0", 61.0),
                acpi_reading(r"\_TZ.CPUZ", 48.5),
                acpi_reading(r"\_TZ.TZ01", 30.0),
            ],
            &[],
            &[],
            false,
        );
        let (value, note) = cpu_temperature(&list).expect("a zone is available");
        assert_eq!(value, 48.5, "a zone named after the processor is preferred");
        assert!(note.starts_with("ACPI thermal zone"));
        assert!(note.ends_with("not a per-core reading"));

        let unnamed = compose(
            vec![
                acpi_reading(r"\_TZ.THM0", 61.0),
                acpi_reading(r"\_TZ.TZ01", 30.0),
            ],
            &[],
            &[],
            false,
        );
        let (value, note) = cpu_temperature(&unnamed).expect("a zone is available");
        assert_eq!(value, 61.0, "otherwise the warmest zone");
        assert!(note.contains("warmest") && note.ends_with("not a per-core reading"));

        // Board probes, placeholders and bogus values never stand in.
        let mut others = vec![
            SensorReading {
                group: "Motherboard".into(),
                name: "Board probe".into(),
                kind: SensorKind::Temperature,
                value: Some(35.0),
                ..Default::default()
            },
            SensorReading {
                group: "ACPI".into(),
                name: "Thermal zone X".into(),
                kind: SensorKind::Temperature,
                value: Some(900.0),
                ..Default::default()
            },
            SensorReading {
                group: "ACPI".into(),
                name: "Thermal zone Y".into(),
                kind: SensorKind::Temperature,
                value: Some(f64::NAN),
                ..Default::default()
            },
        ];
        assert_eq!(cpu_temperature(&others), None);
        assert_eq!(cpu_temperature(&[]), None);
        others.push(acpi_reading(r"\_TZ.OK", 44.0));
        assert_eq!(cpu_temperature(&others).map(|(v, _)| v), Some(44.0));
    }

    #[test]
    fn live_smoke() {
        let mut sampler = SensorSampler::new();
        let first = sampler.sample();
        assert!(first
            .iter()
            .any(|r| r.group == "Processor" && r.name == "Core temperatures"));
        // Give the one-time WMI probe time to finish.
        for _ in 0..150 {
            if sampler.wmi_probed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let started = std::time::Instant::now();
        let mut list = Vec::new();
        for _ in 0..20 {
            list = sampler.sample();
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;

        for reading in &list {
            assert!(!reading.group.is_empty() && !reading.name.is_empty());
            match reading.value {
                Some(value) => {
                    assert!(value.is_finite());
                    if reading.kind == SensorKind::Temperature {
                        assert!(plausible_celsius(value));
                    }
                }
                None => assert!(!reading.note.is_empty(), "a missing reading says why"),
            }
        }
        let zones = list
            .iter()
            .filter(|r| r.group == "ACPI" && r.value.is_some())
            .count();
        let stand_in = cpu_temperature(&list);
        assert_eq!(stand_in.is_some(), zones > 0);
        let findings = sampler.wmi.lock().unwrap_or_else(|e| e.into_inner());
        println!(
            "sensors: counter set {}, zones with a value {zones}, wmi probed {}, wmi zones {}, wmi zones need admin {}, firmware probes with a value {}, processor stand-in {}, placeholders {}, avg {ms:.3} ms per call",
            if sampler.zones.is_some() { "yes" } else { "no" },
            if findings.probed { "yes" } else { "no" },
            findings.zones.len(),
            if findings.zones_need_admin { "yes" } else { "no" },
            findings.other.len(),
            if stand_in.is_some() { "yes" } else { "no" },
            list.iter().filter(|r| r.value.is_none()).count(),
        );
    }
}
