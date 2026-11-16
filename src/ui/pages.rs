//! What the pages share: their remembered state, the layout helpers (card
//! rows, fact grids, painted tables) and the dispatch from the navigation
//! rail to the page functions.

use super::app::{App, Tab};
use super::processes::ProcessColumn;
use super::theme::{pal, regular, semibold, Icon, Palette};
use super::widgets;
use crate::alerts::AlertEvent;
use crate::model::{HealthStatus, SensorKind};
use crate::report::Format;
use eframe::egui::{
    self, epaint::RectShape, pos2, text::LayoutJob, text::TextWrapping, vec2, Align, Color32,
    CornerRadius, FontId, Layout, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui,
    UiBuilder,
};
use std::sync::Arc;

/// Space between cards.
pub(super) const GAP: f32 = 12.0;
/// Padding inside a card.
pub(super) const PAD: f32 = 16.0;

/// What the pages remember between frames.
pub struct PageState {
    /// True while rendering for `--screenshot`: no native dialogs, no animations.
    pub screenshot: bool,
    /// Alert events of the invented past (demo mode only), oldest first.
    pub demo_events: Vec<AlertEvent>,

    /// `DriveInfo::index` of the drive shown on the Storage page.
    pub storage_drive: Option<u32>,
    /// 0 = overview, 1 = health details, 2 = partitions and volumes.
    pub storage_tab: usize,
    /// Show serial numbers in full although masking is on.
    pub reveal_serials: bool,

    pub process_search: String,
    pub process_sort: ProcessColumn,
    pub process_descending: bool,
    pub process_selected: Option<u32>,
    /// The process the "End task" confirmation is open for: id and name.
    pub end_task: Option<(u32, String)>,

    pub sensor_filter: Option<SensorKind>,

    pub export_open: bool,
    pub export_format: Format,
    pub export_mask: bool,

    /// Show the mini widget's content inside the main window (`--view mini`).
    pub mini_preview: bool,
    /// Where the mini widget's window opens (the place it was last left).
    pub mini_start: Option<(f32, f32)>,
    /// The theme in effect this frame.
    pub dark: bool,
    pub mini: Arc<super::mini::MiniShared>,
}

impl Default for PageState {
    fn default() -> Self {
        PageState {
            screenshot: false,
            demo_events: Vec::new(),
            storage_drive: None,
            storage_tab: 0,
            reveal_serials: false,
            process_search: String::new(),
            process_sort: ProcessColumn::Cpu,
            process_descending: true,
            process_selected: None,
            end_task: None,
            sensor_filter: None,
            export_open: false,
            export_format: Format::Html,
            export_mask: true,
            mini_preview: false,
            mini_start: None,
            dark: true,
            mini: Arc::new(super::mini::MiniShared::default()),
        }
    }
}

impl App {
    /// The extra state behind the `--view` names that are more than a page.
    pub(super) fn open_view_state(&mut self, view: &str) {
        match view {
            "storage-health" => {
                // The drive with something to say, on its health tab.
                self.st.storage_drive = self.data_drive_with_warnings().or(self.st.storage_drive);
                self.st.storage_tab = 1;
            }
            "storage-volumes" => self.st.storage_tab = 2,
            "end-task" => {
                // The busiest ordinary program, as if the user had picked it.
                let sample = self.monitor.snapshot().2;
                let pick = super::hardware::top_processes(
                    &sample.processes,
                    super::hardware::TopBy::Cpu,
                    12,
                )
                .into_iter()
                .find(|q| q.user != "SYSTEM" && q.pid > 4)
                .map(|q| (q.pid, q.name.clone()));
                if let Some((pid, name)) = pick {
                    self.st.process_selected = Some(pid);
                    self.st.end_task = Some((pid, name));
                }
            }
            "export" | "export-report" => self.open_export(),
            "mini" | "mini-widget" => self.st.mini_preview = true,
            "mini-live" => self.settings.mini_widget = true,
            _ => {}
        }
    }

    fn data_drive_with_warnings(&self) -> Option<u32> {
        let slow = self.monitor.snapshot().1;
        slow.drives
            .iter()
            .find(|d| d.health == HealthStatus::Caution || d.health == HealthStatus::Bad)
            .map(|d| d.index)
    }

    /// Escape: closes whichever dialog is open.
    pub(super) fn close_dialogs(&mut self) {
        self.st.end_task = None;
        self.st.export_open = false;
    }

    pub(super) fn page_ui(&mut self, ui: &mut Ui) {
        if self.st.mini_preview {
            self.mini_preview_ui(ui);
            return;
        }
        match self.tab {
            Tab::Dashboard => self.dashboard_ui(ui),
            Tab::Cpu => self.cpu_ui(ui),
            Tab::Memory => self.memory_ui(ui),
            Tab::Gpu => self.gpu_ui(ui),
            Tab::Storage => self.storage_ui(ui),
            Tab::Network => self.network_ui(ui),
            Tab::Battery => self.battery_ui(ui),
            Tab::System => self.system_ui(ui),
            Tab::Displays => self.displays_ui(ui),
            Tab::Sensors => self.sensors_ui(ui),
            Tab::Processes => self.processes_ui(ui),
            Tab::Alerts => self.alerts_ui(ui),
            Tab::Settings => self.settings_ui(ui),
        }
    }

    /// A serial number as the window shows it: masked unless the user asked
    /// to see serial numbers in full.
    pub(super) fn serial(&self, serial: &str) -> String {
        if serial.trim().is_empty() {
            String::new()
        } else if self.settings.mask_serials && !self.st.reveal_serials {
            mask(serial)
        } else {
            serial.to_string()
        }
    }

    /// A network address as the window shows it (masked like serial numbers).
    pub(super) fn address(&self, address: &str) -> String {
        self.serial(address)
    }
}

/// Masks a serial number for display. The report module owns the rule; if
/// it leaves the text untouched (or empties it) a simple rule is applied
/// here so that the window never shows a full serial while masking is on.
pub(super) fn mask(serial: &str) -> String {
    let masked = crate::report::mask_serial(serial);
    if masked != serial && !masked.is_empty() {
        return masked;
    }
    let chars: Vec<char> = serial.chars().collect();
    if chars.len() <= 4 {
        return "••••".to_string();
    }
    let keep: String = chars[chars.len() - 4..].iter().collect();
    format!("{}{keep}", "•".repeat((chars.len() - 4).min(8)))
}

/// The scrolling body every page uses.
pub(super) fn page_scroll(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = GAP;
            add(ui);
            ui.add_space(6.0);
        });
}

/// The state shown before the first hardware inventory has arrived.
pub(super) fn loading(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(18.0).color(p.accent));
            ui.label(RichText::new("Reading hardware...").font(semibold(14.5)));
            ui.label(RichText::new(text).color(p.muted));
        });
    });
}

/// A line of muted text that wraps: notes and honest "not available" reasons.
pub(super) fn note(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.muted)).wrap());
}

/// "Not available: <reason>" with an info icon.
pub(super) fn unavailable(ui: &mut Ui, what: &str, reason: &str) {
    let p = pal(ui);
    let reason = if reason.trim().is_empty() {
        "this computer does not report it"
    } else {
        reason.trim()
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(Icon::INFO.glyph()).size(13.0).color(p.faint));
        ui.label(
            RichText::new(format!("{what}: {reason}."))
                .size(12.5)
                .color(p.muted),
        );
    });
}

/// A row of cards with the given width weights, all as tall as the tallest.
/// The contents must not ask for "all remaining height".
pub(super) fn card_row(ui: &mut Ui, weights: &[f32], add: &mut dyn FnMut(&mut Ui, usize)) {
    let p = pal(ui);
    let n = weights.len().max(1);
    let total = ui.available_width();
    let usable = total - GAP * (n as f32 - 1.0);
    let sum: f32 = weights.iter().sum::<f32>().max(0.001);
    let origin = ui.cursor().min;
    let mut x = origin.x;
    let mut bottom = origin.y;
    let mut frames = Vec::with_capacity(n);
    for (i, weight) in weights.iter().enumerate() {
        let width = (usable * weight / sum).floor();
        let background = ui.painter().add(Shape::Noop);
        let inner = Rect::from_min_max(
            pos2(x + PAD, origin.y + PAD),
            pos2(x + width - PAD, origin.y + PAD + 6000.0),
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(("card-row", i))
                .max_rect(inner)
                .layout(Layout::top_down(Align::Min)),
        );
        child.set_width(inner.width());
        child.spacing_mut().item_spacing.y = 8.0;
        add(&mut child, i);
        bottom = bottom.max(child.min_rect().bottom() + PAD);
        frames.push((background, x, width));
        x += width + GAP;
    }
    for (background, x, width) in frames {
        let rect = Rect::from_min_max(pos2(x, origin.y), pos2(x + width, bottom));
        ui.painter().set(
            background,
            RectShape::new(
                rect,
                CornerRadius::same(12),
                p.card,
                Stroke::new(1.0_f32, p.border),
                StrokeKind::Inside,
            ),
        );
    }
    ui.allocate_rect(
        Rect::from_min_max(origin, pos2(origin.x + total, bottom)),
        Sense::hover(),
    );
}

/// How many columns of at least `min_width` fit, between 1 and `max`.
pub(super) fn columns_for(width: f32, min_width: f32, max: usize) -> usize {
    (((width + GAP) / (min_width + GAP)).floor() as usize).clamp(1, max.max(1))
}

/// A grid of facts: small label above, value below. Empty values are shown
/// as "Not available"; values that do not fit are cut and shown on hover.
pub(super) fn facts(ui: &mut Ui, columns: usize, items: &[(&str, String)]) {
    let p = pal(ui);
    let columns = columns.max(1);
    let width = ui.available_width();
    let cell_w = (width / columns as f32).floor();
    let row_h = 42.0;
    let rows = items.len().div_ceil(columns);
    let (rect, _) = ui.allocate_exact_size(vec2(width, rows as f32 * row_h), Sense::hover());
    for (i, (label, value)) in items.iter().enumerate() {
        let min = rect.min + vec2((i % columns) as f32 * cell_w, (i / columns) as f32 * row_h);
        let cell = Rect::from_min_size(min, vec2(cell_w - 14.0, row_h));
        let top = Rect::from_min_size(cell.min, vec2(cell.width(), 16.0));
        let low = Rect::from_min_size(cell.min + vec2(0.0, 16.0), vec2(cell.width(), 20.0));
        widgets::text_in(ui, top, label, regular(12.0), p.faint);
        let missing = value.trim().is_empty();
        let cut = widgets::text_in(
            ui,
            low,
            if missing { "Not available" } else { value },
            regular(14.0),
            if missing { p.faint } else { p.text },
        );
        if cut {
            ui.interact(cell, ui.id().with(("fact", i, *label)), Sense::hover())
                .on_hover_text(value);
        }
    }
}

/// Lays out text that wraps to at most `max_rows` lines and is cut after that.
pub(super) fn wrapped(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::simple(text.to_string(), font, color, max_width.max(8.0));
    job.wrap = TextWrapping {
        max_width: max_width.max(8.0),
        max_rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    ui.fonts(|f| f.layout_job(job))
}

// ----------------------------------------------------------------------------------
// Painted tables
// ----------------------------------------------------------------------------------

/// One column: a fixed width in points, or (for widths up to 1.0) a share of
/// what the fixed columns leave over.
#[derive(Clone, Copy)]
pub(super) struct Col {
    pub title: &'static str,
    pub width: f32,
    pub right: bool,
}

pub(super) const fn col(title: &'static str, width: f32) -> Col {
    Col {
        title,
        width,
        right: false,
    }
}

pub(super) const fn col_right(title: &'static str, width: f32) -> Col {
    Col {
        title,
        width,
        right: true,
    }
}

const COL_GAP: f32 = 14.0;

/// The horizontal extent of every column inside `left..left + total`.
pub(super) fn column_spans(left: f32, total: f32, cols: &[Col]) -> Vec<(f32, f32)> {
    let gaps = COL_GAP * cols.len().saturating_sub(1) as f32;
    let fixed: f32 = cols.iter().filter(|c| c.width > 1.0).map(|c| c.width).sum();
    let shares: f32 = cols
        .iter()
        .filter(|c| c.width <= 1.0)
        .map(|c| c.width)
        .sum();
    let rest = (total - gaps - fixed).max(0.0);
    let mut x = left;
    cols.iter()
        .map(|c| {
            let w = if c.width > 1.0 {
                c.width
            } else if shares > 0.0 {
                rest * c.width / shares
            } else {
                0.0
            };
            let span = (x, w);
            x += w + COL_GAP;
            span
        })
        .collect()
}

/// What one table cell shows.
#[derive(Clone, Default)]
pub(super) struct Cell {
    pub text: String,
    /// A second, muted line (wraps to two rows when the row is tall enough).
    pub sub: String,
    pub color: Option<Color32>,
    pub strong: bool,
    /// Draw the text as a pill in this colour.
    pub badge: Option<Color32>,
    /// A usage bar (0..1) in front of the text.
    pub bar: Option<(f32, Color32)>,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            ..Default::default()
        }
    }
    pub fn strong(mut self) -> Cell {
        self.strong = true;
        self
    }
    pub fn color(mut self, color: Color32) -> Cell {
        self.color = Some(color);
        self
    }
    pub fn sub(mut self, sub: impl Into<String>) -> Cell {
        self.sub = sub.into();
        self
    }
    pub fn badge(mut self, color: Color32) -> Cell {
        self.badge = Some(color);
        self
    }
    pub fn bar(mut self, fraction: f32, color: Color32) -> Cell {
        self.bar = Some((fraction, color));
        self
    }
}

/// Paints the header row. With `sort` the titles are clickable and the
/// sorted column carries an arrow; returns the column that was clicked.
pub(super) fn table_header(
    ui: &mut Ui,
    cols: &[Col],
    sort: Option<(usize, bool)>,
) -> Option<usize> {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let mut clicked = None;
    for (i, ((x, w), c)) in column_spans(rect.left(), rect.width(), cols)
        .into_iter()
        .zip(cols)
        .enumerate()
    {
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height() - 4.0));
        let mut color = p.faint;
        let mut title = c.title.to_uppercase();
        if let Some((sorted, descending)) = sort {
            let resp = ui.interact(cell, ui.id().with(("th", i)), Sense::click());
            if resp.hovered() {
                color = p.text;
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(i);
            }
            if sorted == i {
                color = p.text;
                let arrow = if descending {
                    Icon::SORT_DOWN
                } else {
                    Icon::SORT_UP
                };
                if !arrow.glyph().is_empty() {
                    title = if c.right {
                        format!("{} {title}", arrow.glyph())
                    } else {
                        format!("{title} {}", arrow.glyph())
                    };
                }
            }
        }
        if c.right {
            widgets::text_in_right(ui, cell, &title, semibold(11.0), color);
        } else {
            widgets::text_in(ui, cell, &title, semibold(11.0), color);
        }
    }
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0_f32, p.border),
    );
    clicked
}

/// Paints one row and returns its response (hover, click).
pub(super) fn table_row(
    ui: &mut Ui,
    cols: &[Col],
    cells: &[Cell],
    height: f32,
    selected: bool,
    sense: Sense,
) -> Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let back = rect.expand2(vec2(8.0, 0.0));
    if selected {
        ui.painter()
            .rect_filled(back, CornerRadius::same(7), p.selected);
    } else if resp.hovered() && sense != Sense::hover() {
        ui.painter()
            .rect_filled(back, CornerRadius::same(7), p.hover);
    }
    for ((x, w), (c, cell)) in column_spans(rect.left(), rect.width(), cols)
        .into_iter()
        .zip(cols.iter().zip(cells))
    {
        let area = Rect::from_min_size(pos2(x, rect.top()), vec2(w, height));
        let color = cell.color.unwrap_or(p.text);
        let font = if cell.strong {
            semibold(13.5)
        } else {
            regular(13.5)
        };
        if let Some(badge) = cell.badge {
            widgets::paint_badge(ui, area, &cell.text, badge);
            continue;
        }
        if let Some((fraction, bar_color)) = cell.bar {
            let text_w = 46.0_f32.min(area.width() * 0.5);
            let bar = Rect::from_min_max(
                pos2(area.left(), area.center().y - 3.0),
                pos2(area.right() - text_w - 8.0, area.center().y + 3.0),
            );
            super::charts::paint_meter(ui, bar, fraction, bar_color);
            let text = Rect::from_min_max(pos2(area.right() - text_w, area.top()), area.max);
            widgets::text_in_right(ui, text, &cell.text, font, color);
            continue;
        }
        if cell.sub.is_empty() {
            if c.right {
                widgets::text_in_right(ui, area, &cell.text, font, color);
            } else if widgets::text_in(ui, area, &cell.text, font, color) {
                ui.interact(area, resp.id.with(("cell", x as i32)), Sense::hover())
                    .on_hover_text(&cell.text);
            }
        } else {
            let rows = if height >= 56.0 { 2 } else { 1 };
            let sub = wrapped(ui, &cell.sub, regular(12.0), p.muted, area.width(), rows);
            let block = 19.0 + sub.size().y;
            let top = area.center().y - block / 2.0;
            let head = Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), 19.0));
            widgets::text_in(ui, head, &cell.text, font, color);
            let cut = sub.elided;
            ui.painter()
                .galley(pos2(area.left(), top + 19.0), sub, p.muted);
            if cut {
                ui.interact(area, resp.id.with(("sub", x as i32)), Sense::hover())
                    .on_hover_text(&cell.sub);
            }
        }
    }
    resp
}

/// A complete static table: header, rows and hairlines between them.
pub(super) fn table(ui: &mut Ui, cols: &[Col], rows: &[Vec<Cell>], row_height: f32) {
    let p = pal(ui);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        table_header(ui, cols, None);
        for (i, cells) in rows.iter().enumerate() {
            let resp = table_row(ui, cols, cells, row_height, false, Sense::hover());
            if i + 1 < rows.len() {
                ui.painter().hline(
                    resp.rect.x_range(),
                    resp.rect.bottom() - 0.5,
                    Stroke::new(1.0_f32, p.border.gamma_multiply(0.55)),
                );
            }
        }
    });
}

// ----------------------------------------------------------------------------------
// States in words, icon and colour
// ----------------------------------------------------------------------------------

pub(super) fn health_color(p: &Palette, health: HealthStatus) -> Color32 {
    match health {
        HealthStatus::Good => p.good,
        HealthStatus::Caution => p.warn,
        HealthStatus::Bad => p.danger,
        HealthStatus::Unknown => p.muted,
    }
}

pub(super) fn health_icon(health: HealthStatus) -> Icon {
    match health {
        HealthStatus::Good => Icon::DONE,
        HealthStatus::Caution => Icon::WARNING,
        HealthStatus::Bad => Icon::ERROR,
        HealthStatus::Unknown => Icon::INFO,
    }
}

/// "<icon>  Good": the state is always written out, never colour alone.
pub(super) fn health_text(health: HealthStatus) -> String {
    health_icon(health).with(health.label())
}

/// The colour of a usage bar: the family colour, turning to caution and
/// then to bad as it fills up.
pub(super) fn usage_color(p: &Palette, base: Color32, percent: f32) -> Color32 {
    if percent >= 95.0 {
        p.danger
    } else if percent >= 85.0 {
        p.warn
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_share_the_leftover_width() {
        let cols = [col("a", 100.0), col("b", 1.0), col_right("c", 60.0)];
        let spans = column_spans(10.0, 400.0, &cols);
        assert_eq!(spans[0], (10.0, 100.0));
        assert_eq!(spans[1], (124.0, 400.0 - 160.0 - 28.0));
        assert_eq!(spans[2].1, 60.0);
        assert!((spans[2].0 + spans[2].1 - 410.0).abs() < 0.01);
        // Too narrow: the flexible column collapses instead of going negative.
        let spans = column_spans(0.0, 50.0, &cols);
        assert_eq!(spans[1].1, 0.0);
        // Two flexible columns split by weight.
        let spans = column_spans(0.0, 314.0, &[col("a", 0.5), col("b", 1.0)]);
        assert_eq!(spans[0].1, 100.0);
        assert_eq!(spans[1].1, 200.0);
    }

    #[test]
    fn grid_columns_follow_the_width() {
        assert_eq!(columns_for(1024.0, 230.0, 4), 4);
        assert_eq!(columns_for(784.0, 230.0, 4), 3);
        assert_eq!(columns_for(100.0, 230.0, 4), 1);
        assert_eq!(columns_for(5000.0, 230.0, 4), 4);
    }

    #[test]
    fn serials_are_never_shown_whole_when_masked() {
        for serial in ["BNV7X41M902713K", "4C1F90A7", "AB", "02:4C:1F:90:A7:2B"] {
            let m = mask(serial);
            assert_ne!(m, serial);
            assert!(!m.is_empty());
        }
        assert!(
            mask("BNV7X41M902713K").ends_with("713K")
                || !mask("BNV7X41M902713K").contains("BNV7X41M")
        );
    }

    #[test]
    fn states_have_words_and_icons() {
        for h in [
            HealthStatus::Good,
            HealthStatus::Caution,
            HealthStatus::Bad,
            HealthStatus::Unknown,
        ] {
            assert!(health_text(h).contains(h.label()));
        }
    }
}
