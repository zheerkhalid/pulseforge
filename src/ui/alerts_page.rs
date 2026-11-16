//! The Alerts page: the rules with their limits, what is active now and
//! what happened earlier.

use super::app::App;
use super::pages;
use super::theme::{pal, regular, semibold, Icon};
use super::widgets;
use crate::alerts::{AlertEvent, Metric};
use crate::util;
use eframe::egui::{self, pos2, vec2, Align, Align2, Layout, Rect, RichText, Sense, Stroke, Ui};

/// A stored limit (Celsius for temperatures) in the unit it is shown in.
pub fn limit_to_display(metric: Metric, stored: f32, fahrenheit: bool) -> f32 {
    if metric.is_temperature() && fahrenheit {
        util::c_to_f(stored)
    } else {
        stored
    }
}

/// A limit as typed or dragged by the user, back in the stored unit and
/// inside the range the metric allows.
pub fn limit_from_display(metric: Metric, shown: f32, fahrenheit: bool) -> f32 {
    let (lo, hi) = metric.range();
    let stored = if metric.is_temperature() && fahrenheit {
        (shown - 32.0) * 5.0 / 9.0
    } else {
        shown
    };
    if stored.is_finite() {
        // Whole degrees Fahrenheit do not land on whole degrees Celsius:
        // keep one decimal so that the shown value does not jump back.
        ((stored * 10.0).round() / 10.0).clamp(lo, hi)
    } else {
        lo
    }
}

/// The range of the limit control in display units.
pub fn limit_range(metric: Metric, fahrenheit: bool) -> (f32, f32) {
    let (lo, hi) = metric.range();
    (
        limit_to_display(metric, lo, fahrenheit).round(),
        limit_to_display(metric, hi, fahrenheit).round(),
    )
}

/// "%", "°C" or "°F".
pub fn limit_unit(metric: Metric, fahrenheit: bool) -> &'static str {
    if !metric.is_temperature() {
        "%"
    } else if fahrenheit {
        "°F"
    } else {
        "°C"
    }
}

/// A value of this metric in words: "3%", "93 °C", "199 °F".
pub fn metric_value(metric: Metric, value: f32, fahrenheit: bool) -> String {
    if metric.is_temperature() {
        util::fmt_temp(value, fahrenheit)
    } else {
        format!("{value:.0}%")
    }
}

/// "Free disk space (D:) at 3%".
pub fn describe_active(metric: Metric, subject: &str, value: f32, fahrenheit: bool) -> String {
    let what = if subject.is_empty() {
        metric.label().to_string()
    } else {
        format!("{} ({subject})", metric.label())
    };
    format!("{what} at {}", metric_value(metric, value, fahrenheit))
}

/// The sentence under an event. The engine writes temperatures in Celsius;
/// in Fahrenheit the sentence is rebuilt from the numbers.
pub fn event_message(event: &AlertEvent, fahrenheit: bool) -> String {
    if !(event.metric.is_temperature() && fahrenheit) {
        return event.message.clone();
    }
    if event.raised {
        format!(
            "{}, above your limit of {}.",
            util::fmt_temp(event.value, true),
            util::fmt_temp(event.threshold, true)
        )
    } else {
        format!("Now {}.", util::fmt_temp(event.value, true))
    }
}

/// What a rule watches, in a few words (the row also says "above" or
/// "below", the limit and for how long).
fn rule_text(metric: Metric) -> &'static str {
    match metric {
        Metric::CpuUsage => "How busy the processor is",
        Metric::CpuTemp => "How hot the processor runs",
        Metric::MemoryUsage => "How much memory is in use",
        Metric::GpuUsage => "How busy the busiest adapter is",
        Metric::GpuTemp => "How hot the hottest adapter runs",
        Metric::DiskTemp => "How hot each drive runs",
        Metric::DiskHealth => "Each drive's remaining life",
        Metric::DiskFree => "Free space on each volume",
        Metric::BatteryLevel => "Charge while unplugged",
        Metric::BatteryWear => "Capacity lost to ageing",
    }
}

/// One settings-style row: title and explanation on the left, the control
/// on the right.
pub(super) fn setting_row(ui: &mut Ui, title: &str, text: &str, control: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 350.0).max(200.0));
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).font(regular(14.5)));
            if !text.is_empty() {
                ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.muted)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

impl App {
    pub(super) fn alerts_ui(&mut self, ui: &mut Ui) {
        let active = self.data.active_alerts.clone();
        let subtitle = if !self.settings.alerts_enabled {
            "Alerts are switched off".to_string()
        } else {
            match active.len() {
                0 => "Nothing is over its limit".to_string(),
                1 => "1 alert is active".to_string(),
                n => format!("{n} alerts are active"),
            }
        };
        App::page_header(ui, "Alerts", &subtitle, |_| {});
        pages::page_scroll(ui, |ui| {
            widgets::card(ui, |ui| {
                let mut on = self.settings.alerts_enabled;
                setting_row(
                    ui,
                    "Watch the limits below",
                    "When a reading crosses its limit, a notification appears from the PulseForge icon in the notification area. Nothing else is done: PulseForge never changes a setting.",
                    |ui| {
                        if widgets::toggle(ui, on, true).clicked() {
                            on = !on;
                        }
                    },
                );
                self.settings.alerts_enabled = on;
                if on && !self.settings.tray_icon {
                    pages::unavailable(
                        ui,
                        "Notifications",
                        "the tray icon is switched off in Settings, so alerts only show on this page",
                    );
                }
            });
            if ui.available_width() >= 900.0 {
                pages::card_row(ui, &[1.5, 1.0], &mut |ui, i| {
                    if i == 0 {
                        self.limits_ui(ui);
                    } else {
                        self.active_ui(ui, &active);
                        ui.add_space(6.0);
                        self.history_ui(ui);
                    }
                });
            } else {
                widgets::card(ui, |ui| self.active_ui(ui, &active));
                widgets::card(ui, |ui| self.limits_ui(ui));
                widgets::card(ui, |ui| self.history_ui(ui));
            }
        });
    }

    fn active_ui(&self, ui: &mut Ui, active: &[(Metric, String, f32)]) {
        let p = pal(ui);
        let f = self.settings.fahrenheit;
        widgets::section(ui, "Active now");
        if !self.settings.alerts_enabled {
            pages::note(ui, "Alerts are switched off.");
        } else if active.is_empty() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(Icon::DONE.glyph()).color(p.good));
                ui.label(
                    RichText::new("Every watched reading is inside its limit.").color(p.muted),
                );
            });
        }
        for (metric, subject, value) in active {
            let limit = self
                .settings
                .alerts
                .iter()
                .find(|r| r.metric == *metric)
                .map(|r| r.threshold);
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
            ui.painter().text(
                pos2(rect.left() + 9.0, rect.center().y),
                Align2::CENTER_CENTER,
                Icon::WARNING.glyph(),
                regular(15.0),
                p.warn,
            );
            let body = Rect::from_min_max(pos2(rect.left() + 28.0, rect.top()), rect.max);
            widgets::text_in(
                ui,
                Rect::from_min_size(body.min + vec2(0.0, 3.0), vec2(body.width(), 19.0)),
                &describe_active(*metric, subject, *value, f),
                semibold(14.0),
                p.text,
            );
            if let Some(limit) = limit {
                widgets::text_in(
                    ui,
                    Rect::from_min_size(body.min + vec2(0.0, 22.0), vec2(body.width(), 17.0)),
                    &format!(
                        "Your limit: {} {}",
                        if metric.below() { "below" } else { "above" },
                        metric_value(*metric, limit, f)
                    ),
                    regular(12.5),
                    p.muted,
                );
            }
        }
    }

    fn limits_ui(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let f = self.settings.fahrenheit;
        widgets::section(ui, "Limits");
        let enabled_all = self.settings.alerts_enabled;
        let count = self.settings.alerts.len();
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut rules = self.settings.alerts.clone();
        for (i, rule) in rules.iter_mut().enumerate() {
            let metric = rule.metric;
            let unit = limit_unit(metric, f);
            let (lo, hi) = limit_range(metric, f);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 50.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if widgets::toggle(ui, rule.enabled, enabled_all).clicked() {
                        rule.enabled = !rule.enabled;
                    }
                    ui.add_space(4.0);
                    let controls = 300.0;
                    let (name, _) = ui.allocate_exact_size(
                        vec2((ui.available_width() - controls).max(110.0), 50.0),
                        Sense::hover(),
                    );
                    let live = rule.enabled && enabled_all;
                    widgets::text_in(
                        ui,
                        Rect::from_min_size(name.min + vec2(0.0, 7.0), vec2(name.width(), 19.0)),
                        metric.label(),
                        regular(14.5),
                        if live { p.text } else { p.muted },
                    );
                    if widgets::text_in(
                        ui,
                        Rect::from_min_size(name.min + vec2(0.0, 26.0), vec2(name.width(), 17.0)),
                        rule_text(metric),
                        regular(12.5),
                        p.muted,
                    ) {
                        ui.interact(name, ui.id().with(("rule", i)), Sense::hover())
                            .on_hover_text(rule_text(metric));
                    }
                    ui.add_enabled_ui(live, |ui| {
                        let side = if metric.below() { "below" } else { "above" };
                        let (side_rect, _) = ui.allocate_exact_size(vec2(38.0, 20.0), Sense::hover());
                        widgets::text_in_right(ui, side_rect, side, regular(12.5), p.muted);
                        let mut shown = limit_to_display(metric, rule.threshold, f).round();
                        ui.spacing_mut().slider_width = 96.0;
                        let slider = egui::Slider::new(&mut shown, lo..=hi)
                            .integer()
                            .suffix(if unit == "%" {
                                "%".to_string()
                            } else {
                                format!(" {unit}")
                            });
                        if ui.add(slider).changed() {
                            rule.threshold = limit_from_display(metric, shown, f);
                        }
                        ui.label(RichText::new("for").size(12.5).color(p.muted));
                        let mut sustain = rule.sustain_s;
                        if ui
                            .add(
                                egui::DragValue::new(&mut sustain)
                                    .range(0..=3600)
                                    .speed(1.0)
                                    .suffix(" s"),
                            )
                            .on_hover_text("How long the reading must stay past the limit before the alert fires. 0 means at once.")
                            .changed()
                        {
                            rule.sustain_s = sustain.min(3600);
                        }
                    });
                },
            );
            if i + 1 < count {
                let (line, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().hline(
                    line.x_range(),
                    line.center().y,
                    Stroke::new(1.0_f32, p.border.gamma_multiply(0.55)),
                );
            }
        }
        if rules != self.settings.alerts {
            self.settings.alerts = rules;
        }
    }

    fn history_ui(&self, ui: &mut Ui) {
        let p = pal(ui);
        let f = self.settings.fahrenheit;
        widgets::section(ui, "History");
        if self.data.events.is_empty() {
            pages::note(ui, "No alert has fired since PulseForge started.");
            return;
        }
        pages::note(ui, "Newest first.");
        let today = util::local_date(util::now_unix());
        for event in self.data.events.iter().rev().take(100) {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
            let (icon, color) = if event.raised {
                (Icon::WARNING, p.warn)
            } else {
                (Icon::DONE, p.good)
            };
            ui.painter().text(
                pos2(rect.left() + 9.0, rect.center().y),
                Align2::CENTER_CENTER,
                icon.glyph(),
                regular(15.0),
                color,
            );
            let when = if util::local_date(event.time) == today {
                util::local_time(event.time)
            } else {
                util::local_datetime(event.time)
            };
            let body = Rect::from_min_max(pos2(rect.left() + 28.0, rect.top()), rect.max);
            if widgets::text_in(
                ui,
                Rect::from_min_size(body.min + vec2(0.0, 4.0), vec2(body.width(), 19.0)),
                &event.title,
                semibold(14.0),
                p.text,
            ) {
                ui.interact(
                    body,
                    ui.id().with(("event", event.time, event.raised)),
                    Sense::hover(),
                )
                .on_hover_text(&event.title);
            }
            widgets::text_in(
                ui,
                Rect::from_min_size(body.min + vec2(0.0, 23.0), vec2(body.width(), 17.0)),
                &format!("{when} · {}", event_message(event, f)),
                regular(12.5),
                p.muted,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_convert_both_ways() {
        // Percentages are never converted.
        assert_eq!(limit_to_display(Metric::MemoryUsage, 92.0, true), 92.0);
        assert_eq!(limit_from_display(Metric::MemoryUsage, 92.0, true), 92.0);
        assert_eq!(limit_unit(Metric::MemoryUsage, true), "%");
        // Temperatures are stored in Celsius whatever is shown.
        assert_eq!(limit_to_display(Metric::CpuTemp, 90.0, false), 90.0);
        assert_eq!(limit_to_display(Metric::CpuTemp, 90.0, true), 194.0);
        assert_eq!(limit_from_display(Metric::CpuTemp, 194.0, true), 90.0);
        assert_eq!(limit_from_display(Metric::CpuTemp, 85.0, false), 85.0);
        assert_eq!(limit_unit(Metric::CpuTemp, true), "°F");
        assert_eq!(limit_unit(Metric::DiskTemp, false), "°C");
        // Every whole Fahrenheit step survives the round trip.
        let (lo, hi) = limit_range(Metric::GpuTemp, true);
        assert_eq!((lo, hi), (104.0, 230.0));
        let mut shown = lo;
        while shown <= hi {
            let stored = limit_from_display(Metric::GpuTemp, shown, true);
            assert_eq!(
                limit_to_display(Metric::GpuTemp, stored, true).round(),
                shown
            );
            shown += 1.0;
        }
        assert_eq!(limit_range(Metric::DiskFree, true), (1.0, 100.0));
    }

    #[test]
    fn limits_stay_in_range() {
        assert_eq!(limit_from_display(Metric::CpuTemp, 500.0, false), 110.0);
        assert_eq!(limit_from_display(Metric::CpuTemp, -40.0, true), 40.0);
        assert_eq!(limit_from_display(Metric::DiskFree, 0.0, false), 1.0);
        assert_eq!(limit_from_display(Metric::DiskFree, f32::NAN, false), 1.0);
        assert_eq!(
            limit_from_display(Metric::DiskTemp, f32::INFINITY, true),
            30.0
        );
    }

    #[test]
    fn alerts_read_naturally() {
        assert_eq!(
            describe_active(Metric::DiskFree, "D:", 3.2, false),
            "Free disk space (D:) at 3%"
        );
        assert_eq!(
            describe_active(Metric::CpuTemp, "", 93.4, false),
            "Processor temperature at 93 °C"
        );
        assert_eq!(
            describe_active(Metric::CpuTemp, "", 100.0, true),
            "Processor temperature at 212 °F"
        );
        let mut event = AlertEvent {
            time: 0,
            metric: Metric::CpuTemp,
            subject: String::new(),
            value: 100.0,
            threshold: 90.0,
            raised: true,
            title: "Processor temperature is high".into(),
            message: "100 °C, above your limit of 90 °C.".into(),
        };
        assert_eq!(
            event_message(&event, false),
            "100 °C, above your limit of 90 °C."
        );
        assert_eq!(
            event_message(&event, true),
            "212 °F, above your limit of 194 °F."
        );
        event.raised = false;
        event.value = 80.0;
        assert_eq!(event_message(&event, true), "Now 176 °F.");
        event.metric = Metric::MemoryUsage;
        assert_eq!(event_message(&event, true), event.message);
    }
}
