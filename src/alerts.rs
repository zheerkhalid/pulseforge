//! Threshold alerts: pure logic, no Windows calls.
//!
//! A rule fires once when its reading has been past the threshold for the
//! rule's "sustain" time, stays active while the condition holds, and clears
//! when the reading comes back past the threshold by a small margin (so a
//! value hovering around the threshold does not flap).

use crate::model::{Sample, Slow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Metric {
    CpuUsage,
    CpuTemp,
    MemoryUsage,
    GpuUsage,
    GpuTemp,
    DiskTemp,
    /// Remaining drive life in percent; fires when it falls below the threshold.
    DiskHealth,
    /// Free space on a volume in percent; fires below the threshold.
    DiskFree,
    /// Battery charge in percent while discharging; fires below the threshold.
    BatteryLevel,
    /// Battery wear in percent; fires above the threshold.
    BatteryWear,
}

impl Metric {
    pub const ALL: [Metric; 10] = [
        Metric::CpuUsage,
        Metric::CpuTemp,
        Metric::MemoryUsage,
        Metric::GpuUsage,
        Metric::GpuTemp,
        Metric::DiskTemp,
        Metric::DiskHealth,
        Metric::DiskFree,
        Metric::BatteryLevel,
        Metric::BatteryWear,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Metric::CpuUsage => "Processor usage",
            Metric::CpuTemp => "Processor temperature",
            Metric::MemoryUsage => "Memory usage",
            Metric::GpuUsage => "Graphics usage",
            Metric::GpuTemp => "Graphics temperature",
            Metric::DiskTemp => "Drive temperature",
            Metric::DiskHealth => "Drive health",
            Metric::DiskFree => "Free disk space",
            Metric::BatteryLevel => "Battery level",
            Metric::BatteryWear => "Battery wear",
        }
    }

    /// True when the alert fires for values *below* the threshold.
    pub fn below(self) -> bool {
        matches!(
            self,
            Metric::DiskHealth | Metric::DiskFree | Metric::BatteryLevel
        )
    }

    pub fn is_temperature(self) -> bool {
        matches!(self, Metric::CpuTemp | Metric::GpuTemp | Metric::DiskTemp)
    }

    /// "%" or "°C" (thresholds are always stored in Celsius).
    pub fn unit(self) -> &'static str {
        if self.is_temperature() {
            "°C"
        } else {
            "%"
        }
    }

    /// The range a threshold may be set to.
    pub fn range(self) -> (f32, f32) {
        match self {
            Metric::CpuTemp | Metric::GpuTemp => (40.0, 110.0),
            Metric::DiskTemp => (30.0, 90.0),
            _ => (1.0, 100.0),
        }
    }

    fn default_rule(self) -> AlertRule {
        let (threshold, sustain_s, enabled) = match self {
            Metric::CpuUsage => (95.0, 60, false),
            Metric::CpuTemp => (90.0, 10, true),
            Metric::MemoryUsage => (92.0, 30, true),
            Metric::GpuUsage => (98.0, 120, false),
            Metric::GpuTemp => (88.0, 10, true),
            Metric::DiskTemp => (60.0, 30, true),
            Metric::DiskHealth => (20.0, 0, true),
            Metric::DiskFree => (5.0, 0, true),
            Metric::BatteryLevel => (15.0, 0, true),
            Metric::BatteryWear => (40.0, 0, false),
        };
        AlertRule {
            metric: self,
            enabled,
            threshold,
            sustain_s,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    pub metric: Metric,
    pub enabled: bool,
    pub threshold: f32,
    /// Seconds the reading must stay past the threshold before the alert fires.
    pub sustain_s: u32,
}

/// One rule per metric, with sensible thresholds.
pub fn default_rules() -> Vec<AlertRule> {
    Metric::ALL.iter().map(|m| m.default_rule()).collect()
}

/// Makes a loaded rule list complete and sane: one rule per metric, thresholds in range.
pub fn normalize(rules: &mut Vec<AlertRule>) {
    let mut seen = Vec::new();
    rules.retain(|r| {
        if seen.contains(&r.metric) {
            false
        } else {
            seen.push(r.metric);
            true
        }
    });
    for m in Metric::ALL {
        if !seen.contains(&m) {
            rules.push(m.default_rule());
        }
    }
    for r in rules.iter_mut() {
        let (lo, hi) = r.metric.range();
        if !r.threshold.is_finite() {
            r.threshold = r.metric.default_rule().threshold;
        }
        r.threshold = r.threshold.clamp(lo, hi);
        r.sustain_s = r.sustain_s.min(3600);
    }
    rules.sort_by_key(|r| Metric::ALL.iter().position(|m| *m == r.metric));
}

/// One measured value that rules are checked against.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub metric: Metric,
    /// What it belongs to: "" for the machine, "Drive 1", "C:", an adapter name.
    pub subject: String,
    pub value: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlertEvent {
    /// Unix time.
    pub time: u64,
    pub metric: Metric,
    pub subject: String,
    pub value: f32,
    pub threshold: f32,
    /// True when the alert starts, false when it clears.
    pub raised: bool,
    /// Short title for the notification ("Processor temperature is high").
    pub title: String,
    /// One sentence with the numbers. Temperatures are written in Celsius;
    /// the user interface reformats them from `value` when it shows Fahrenheit.
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default)]
struct State {
    /// Since when the reading has been past the threshold.
    since: Option<f64>,
    active: bool,
    value: f32,
}

/// Remembers which alerts are pending and active.
#[derive(Debug, Default)]
pub struct Engine {
    states: HashMap<(Metric, String), State>,
}

fn margin(metric: Metric) -> f32 {
    if metric.is_temperature() {
        3.0
    } else {
        2.0
    }
}

fn describe(
    metric: Metric,
    subject: &str,
    value: f32,
    threshold: f32,
    raised: bool,
) -> (String, String) {
    let what = if subject.is_empty() {
        metric.label().to_string()
    } else {
        format!("{} ({subject})", metric.label())
    };
    let unit = metric.unit();
    let sep = if unit == "%" { "" } else { " " };
    if !raised {
        return (
            format!("{what} is back to normal"),
            format!("Now {value:.0}{sep}{unit}."),
        );
    }
    let title = match metric {
        Metric::DiskHealth => format!("{what} is low"),
        Metric::DiskFree => format!("{what} is running out"),
        Metric::BatteryLevel => "Battery is low".to_string(),
        _ => format!("{what} is high"),
    };
    let side = if metric.below() { "below" } else { "above" };
    (
        title,
        format!("{value:.0}{sep}{unit}, {side} your limit of {threshold:.0}{sep}{unit}."),
    )
}

impl Engine {
    pub fn new() -> Engine {
        Engine::default()
    }

    /// Checks the readings against the rules. `t` is a monotonic time in
    /// seconds, `unix` the wall clock for the events. Returns alerts that
    /// started or cleared in this step.
    pub fn evaluate(
        &mut self,
        rules: &[AlertRule],
        readings: &[Reading],
        t: f64,
        unix: u64,
    ) -> Vec<AlertEvent> {
        let mut events = Vec::new();
        for reading in readings {
            if !reading.value.is_finite() {
                continue;
            }
            let key = (reading.metric, reading.subject.clone());
            let Some(rule) = rules
                .iter()
                .find(|r| r.metric == reading.metric && r.enabled)
            else {
                // Rule switched off: forget silently.
                self.states.remove(&key);
                continue;
            };
            let state = self.states.entry(key).or_default();
            state.value = reading.value;
            let below = rule.metric.below();
            let past = if below {
                reading.value < rule.threshold
            } else {
                reading.value > rule.threshold
            };
            let m = margin(rule.metric);
            let cleared = if below {
                reading.value >= rule.threshold + m
            } else {
                reading.value <= rule.threshold - m
            };
            let mut event = None;
            if past {
                let since = *state.since.get_or_insert(t);
                if !state.active && t - since >= f64::from(rule.sustain_s) {
                    state.active = true;
                    event = Some(true);
                }
            } else {
                if !state.active || cleared {
                    state.since = None;
                }
                if state.active && cleared {
                    state.active = false;
                    event = Some(false);
                }
            }
            if let Some(raised) = event {
                let (title, message) = describe(
                    rule.metric,
                    &reading.subject,
                    reading.value,
                    rule.threshold,
                    raised,
                );
                events.push(AlertEvent {
                    time: unix,
                    metric: rule.metric,
                    subject: reading.subject.clone(),
                    value: reading.value,
                    threshold: rule.threshold,
                    raised,
                    title,
                    message,
                });
            }
        }
        // Subjects that disappeared (a drive unplugged) clear without an event.
        self.states.retain(|(metric, subject), _| {
            readings
                .iter()
                .any(|r| r.metric == *metric && r.subject == *subject)
        });
        events
    }

    /// The alerts that are active right now: metric, subject, latest value.
    pub fn active(&self) -> Vec<(Metric, String, f32)> {
        let mut out: Vec<_> = self
            .states
            .iter()
            .filter(|(_, s)| s.active)
            .map(|((m, subject), s)| (*m, subject.clone(), s.value))
            .collect();
        out.sort_by(|a, b| {
            (Metric::ALL.iter().position(|m| *m == a.0), &a.1)
                .cmp(&(Metric::ALL.iter().position(|m| *m == b.0), &b.1))
        });
        out
    }
}

/// Pulls every reading that a rule can watch out of the current data.
pub fn readings(sample: &Sample, slow: &Slow) -> Vec<Reading> {
    let mut out = Vec::new();
    let mut add = |metric, subject: &str, value: f32| {
        out.push(Reading {
            metric,
            subject: subject.to_string(),
            value,
        })
    };
    add(Metric::CpuUsage, "", sample.cpu.total_pct);
    if let Some(t) = sample.cpu.temperature_c {
        add(Metric::CpuTemp, "", t);
    }
    if sample.memory.total > 0 {
        add(Metric::MemoryUsage, "", sample.memory.used_pct());
    }
    if let Some(load) = sample.gpu_load() {
        add(Metric::GpuUsage, "", load);
    }
    if let Some(t) = sample.gpu_temp() {
        add(Metric::GpuTemp, "", t);
    }
    for d in &slow.drives {
        let subject = format!("Drive {}", d.index);
        if let Some(t) = d.temperature_c {
            add(Metric::DiskTemp, &subject, t);
        }
        if let Some(h) = d.health_pct {
            add(Metric::DiskHealth, &subject, f32::from(h));
        }
    }
    for v in &slow.volumes {
        if v.total > 0 && v.kind == "Fixed" {
            add(Metric::DiskFree, &v.mount, 100.0 - v.used_pct());
        }
    }
    if let Some(b) = &sample.battery {
        if !b.ac_online {
            add(Metric::BatteryLevel, "", b.percent);
        }
        if let Some(w) = b.wear_pct {
            add(Metric::BatteryWear, "", w);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BatteryInfo, DriveInfo, Volume};

    fn rule(metric: Metric, threshold: f32, sustain_s: u32) -> AlertRule {
        AlertRule {
            metric,
            enabled: true,
            threshold,
            sustain_s,
        }
    }

    fn cpu(value: f32) -> Vec<Reading> {
        vec![Reading {
            metric: Metric::CpuTemp,
            subject: String::new(),
            value,
        }]
    }

    #[test]
    fn fires_once_after_sustain_and_clears_with_hysteresis() {
        let rules = [rule(Metric::CpuTemp, 90.0, 10)];
        let mut e = Engine::new();
        assert!(e.evaluate(&rules, &cpu(95.0), 0.0, 100).is_empty());
        assert!(e.evaluate(&rules, &cpu(96.0), 5.0, 105).is_empty());
        let ev = e.evaluate(&rules, &cpu(97.0), 10.0, 110);
        assert_eq!(ev.len(), 1);
        assert!(ev[0].raised);
        assert_eq!(ev[0].time, 110);
        assert_eq!(ev[0].title, "Processor temperature is high");
        assert_eq!(ev[0].message, "97 °C, above your limit of 90 °C.");
        // Still hot: no repeat.
        assert!(e.evaluate(&rules, &cpu(99.0), 11.0, 111).is_empty());
        assert_eq!(e.active().len(), 1);
        // Just under the threshold is inside the margin: stays active.
        assert!(e.evaluate(&rules, &cpu(89.0), 12.0, 112).is_empty());
        assert_eq!(e.active().len(), 1);
        // Back over: no second notification.
        assert!(e.evaluate(&rules, &cpu(93.0), 13.0, 113).is_empty());
        // Properly cool: clears.
        let ev = e.evaluate(&rules, &cpu(80.0), 14.0, 114);
        assert_eq!(ev.len(), 1);
        assert!(!ev[0].raised);
        assert!(e.active().is_empty());
        // And it can fire again, after a fresh sustain period.
        assert!(e.evaluate(&rules, &cpu(95.0), 15.0, 115).is_empty());
        assert_eq!(e.evaluate(&rules, &cpu(95.0), 25.0, 125).len(), 1);
    }

    #[test]
    fn a_short_spike_does_not_fire() {
        let rules = [rule(Metric::CpuTemp, 90.0, 10)];
        let mut e = Engine::new();
        assert!(e.evaluate(&rules, &cpu(95.0), 0.0, 0).is_empty());
        assert!(e.evaluate(&rules, &cpu(70.0), 5.0, 5).is_empty());
        // The clock starts again.
        assert!(e.evaluate(&rules, &cpu(95.0), 8.0, 8).is_empty());
        assert!(e.evaluate(&rules, &cpu(95.0), 12.0, 12).is_empty());
        assert_eq!(e.evaluate(&rules, &cpu(95.0), 18.0, 18).len(), 1);
    }

    #[test]
    fn below_rules_and_subjects() {
        let rules = [
            rule(Metric::DiskFree, 5.0, 0),
            rule(Metric::BatteryLevel, 15.0, 0),
        ];
        let mut e = Engine::new();
        let r = vec![
            Reading {
                metric: Metric::DiskFree,
                subject: "C:".into(),
                value: 3.0,
            },
            Reading {
                metric: Metric::DiskFree,
                subject: "D:".into(),
                value: 60.0,
            },
            Reading {
                metric: Metric::BatteryLevel,
                subject: String::new(),
                value: 14.0,
            },
        ];
        let ev = e.evaluate(&rules, &r, 0.0, 0);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].subject, "C:");
        assert_eq!(ev[0].title, "Free disk space (C:) is running out");
        assert_eq!(ev[0].message, "3%, below your limit of 5%.");
        assert_eq!(ev[1].title, "Battery is low");
        assert_eq!(e.active().len(), 2);
        // The battery reading disappears (plugged in): its alert is dropped quietly.
        let ev = e.evaluate(&rules, &r[..2], 1.0, 1);
        assert!(ev.is_empty());
        assert_eq!(e.active().len(), 1);
    }

    #[test]
    fn disabled_rules_and_bad_values_are_ignored() {
        let mut rules = vec![rule(Metric::CpuTemp, 90.0, 0)];
        let mut e = Engine::new();
        assert!(e.evaluate(&rules, &cpu(f32::NAN), 0.0, 0).is_empty());
        assert_eq!(e.evaluate(&rules, &cpu(99.0), 1.0, 1).len(), 1);
        rules[0].enabled = false;
        assert!(e.evaluate(&rules, &cpu(99.0), 2.0, 2).is_empty());
        assert!(e.active().is_empty());
        assert!(e.evaluate(&[], &cpu(99.0), 3.0, 3).is_empty());
    }

    #[test]
    fn normalizing_loaded_rules() {
        let mut rules = vec![
            rule(Metric::GpuTemp, 500.0, 99_999),
            rule(Metric::GpuTemp, 70.0, 0),
            rule(Metric::CpuUsage, f32::NAN, 0),
        ];
        normalize(&mut rules);
        assert_eq!(rules.len(), Metric::ALL.len());
        assert_eq!(rules[0].metric, Metric::CpuUsage);
        assert_eq!(rules[0].threshold, 95.0);
        let gpu = rules.iter().find(|r| r.metric == Metric::GpuTemp).unwrap();
        assert_eq!((gpu.threshold, gpu.sustain_s), (110.0, 3600));
        let mut empty = Vec::new();
        normalize(&mut empty);
        assert_eq!(empty, default_rules());
    }

    #[test]
    fn readings_from_data() {
        let mut sample = Sample::default();
        sample.cpu.total_pct = 12.0;
        sample.memory.total = 100;
        sample.memory.used = 40;
        sample.battery = Some(BatteryInfo {
            percent: 55.0,
            ac_online: false,
            wear_pct: Some(8.0),
            ..Default::default()
        });
        let slow = Slow {
            drives: vec![DriveInfo {
                index: 1,
                temperature_c: Some(41.0),
                health_pct: Some(97),
                ..Default::default()
            }],
            volumes: vec![
                Volume {
                    mount: "C:".into(),
                    total: 1000,
                    free: 250,
                    kind: "Fixed".into(),
                    ..Default::default()
                },
                Volume {
                    mount: "E:".into(),
                    total: 1000,
                    free: 0,
                    kind: "Optical".into(),
                    ..Default::default()
                },
            ],
        };
        let r = readings(&sample, &slow);
        let get = |m: Metric| r.iter().find(|x| x.metric == m).map(|x| x.value);
        assert_eq!(get(Metric::CpuUsage), Some(12.0));
        assert_eq!(get(Metric::CpuTemp), None);
        assert_eq!(get(Metric::MemoryUsage), Some(40.0));
        assert_eq!(get(Metric::DiskTemp), Some(41.0));
        assert_eq!(get(Metric::DiskHealth), Some(97.0));
        assert_eq!(get(Metric::DiskFree), Some(25.0));
        assert_eq!(get(Metric::BatteryLevel), Some(55.0));
        assert_eq!(get(Metric::BatteryWear), Some(8.0));
        assert_eq!(r.iter().filter(|x| x.metric == Metric::DiskFree).count(), 1);
    }
}
