//! A time-indexed list of checks with its local screenshot and OCR overlay.
use super::*;
use crate::{
    design::{INK, LINE},
    parallel::clock,
    screen_layout::{Bounds, Layout, Line},
};
use egui::{FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::Deserialize;

#[derive(Deserialize)]
struct Summary {
    id: i64,
    at: i64,
    excerpt: String,
    #[serde(default)]
    image_available: bool,
}
#[derive(Default, Deserialize)]
struct Page {
    items: Vec<Summary>,
    next: Option<[i64; 2]>,
    oldest_image: Option<i64>,
    last_check: Option<i64>,
    delay: Option<String>,
    capture_error: Option<String>,
}
#[derive(Deserialize)]
struct Capture {
    id: i64,
    at: i64,
    legacy: bool,
    #[serde(default)]
    image_available: bool,
    #[serde(default)]
    truncated: bool,
    layout: Layout,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Mode {
    #[default]
    Screenshot,
    Layout,
    Reading,
}

#[derive(Default)]
struct Display {
    mode: Mode,
    magnified: bool,
    hide_overlay: bool,
    image: picture::Viewer,
}

#[derive(Default)]
pub(super) struct Browser {
    query: String,
    cursors: Vec<[i64; 2]>,
    page: Page,
    page_key: Option<Value>,
    pending_page: Option<Value>,
    edited: Option<Instant>,
    pub checked: Option<Instant>,
    page_error: Option<String>,
    selected: Option<Value>,
    pinned: bool,
    recording: bool,
    pending_detail: Option<Value>,
    capture: Option<Capture>,
    error: Option<String>,
    display: Display,
}
impl Browser {
    pub fn ready(&self) -> bool {
        self.page_key.as_ref() == Some(&self.key())
            && self.page_error.is_none()
            && (self.capture.as_ref().is_some_and(|c| {
                !c.image_available
                    || self.display.mode != Mode::Screenshot
                    || self.display.image.ready()
            }) || (self.page.items.is_empty() && self.selected.is_none()))
    }
    fn key(&self) -> Value {
        json!({"query":self.query.trim(),"before":self.cursors.last()})
    }
    fn select(&mut self, selection: Value, pinned: bool) {
        self.pinned = pinned;
        self.selected = Some(selection);
        self.capture = None;
        self.display.image.clear();
        self.error = None;
    }
    fn reset_selection(&mut self) {
        self.pinned = false;
        self.page_key = None;
        self.selected = None;
        self.capture = None;
        self.display.image.clear();
        self.error = None;
    }
    pub fn reply(&mut self, r: Reply) {
        if r.path == "/screen_captures" {
            self.pending_page = None;
            if r.body.as_ref() != Some(&self.key()) {
                return;
            }
            self.page_key = r.body;
            match r
                .result
                .and_then(|v| serde_json::from_value::<Page>(v).map_err(|e| e.to_string()))
            {
                Ok(page) => {
                    self.page_error = None;
                    if !self.pinned
                        && let Some(first) = page.items.first()
                        && !self.selected.as_ref().is_some_and(|v| v["id"] == first.id)
                    {
                        self.select(json!({"id":first.id}), false);
                    }
                    if let Some(c) = self.capture.as_mut()
                        && c.image_available
                        && page.oldest_image.is_none_or(|id| c.id < id)
                    {
                        c.image_available = false;
                        self.display.image.clear();
                    }
                    self.page = page;
                }
                Err(error) => self.page_error = Some(error),
            }
        } else {
            self.pending_detail = None;
            if self.selected.is_none() || r.body != self.selected {
                return;
            }
            match r
                .result
                .and_then(|v| serde_json::from_value::<Capture>(v).map_err(|e| e.to_string()))
            {
                Ok(capture) => {
                    if r.body
                        .as_ref()
                        .is_some_and(|key| key["observation"].is_i64())
                        && !self.page.items.iter().any(|p| p.id == capture.id)
                    {
                        self.cursors = vec![[capture.at, capture.id.saturating_add(1)]];
                    }
                    self.selected = Some(json!({"id":capture.id}));
                    self.capture = Some(capture);
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
    }
}

impl Controls {
    pub(super) fn open_capture(&mut self, observation: i64) {
        self.close_entry();
        self.parallel.detail = None;
        self.navigate(5);
        self.screen_history.visits = false;
        let b = &mut self.screen_history.captures;
        b.query.clear();
        b.cursors.clear();
        b.display.mode = Mode::Screenshot;
        b.select(json!({"observation":observation}), true);
    }
    pub(super) fn capture_browser(&mut self, ui: &mut egui::Ui) {
        let b = &mut self.screen_history.captures;
        let settings = ui
            .horizontal_wrapped(|ui| {
                let search = ui.add(
                    egui::TextEdit::singleline(&mut b.query)
                        .hint_text("Search captures")
                        .desired_width(265.)
                        .char_limit(200),
                );
                if search.changed() {
                    b.cursors.clear();
                    b.reset_selection();
                    b.edited = Some(Instant::now());
                }
                if ui.small_button("Refresh").clicked() {
                    b.checked = None;
                    b.error = None;
                }
                if (b.pinned || !b.cursors.is_empty()) && ui.small_button("Follow latest").clicked()
                {
                    b.cursors.clear();
                    b.reset_selection();
                }
                ui.link("Capture settings").clicked()
            })
            .inner;
        if settings {
            self.settings_tab = 1;
            self.navigate(4);
            return;
        }
        let key = b.key();
        let live = self.status["active"] == "true";
        if b.recording != live {
            b.checked = None;
            b.recording = live;
        }
        let typing = b
            .edited
            .is_some_and(|t| t.elapsed() < Duration::from_millis(170));
        if typing {
            ui.ctx().request_repaint_after(Duration::from_millis(175));
        }
        let request_page = b.pending_page.is_none()
            && !typing
            && (b.page_key.as_ref() != Some(&key)
                || b.checked
                    .is_none_or(|t| live && t.elapsed() >= Duration::from_secs(5)));
        let detail = b
            .selected
            .clone()
            .filter(|_| b.pending_detail.is_none() && b.capture.is_none() && b.error.is_none());
        if request_page && self.send("/screen_captures", Some(key.clone())) {
            let b = &mut self.screen_history.captures;
            b.pending_page = Some(key);
            b.checked = Some(Instant::now());
        }
        if let Some(key) = detail
            && self.send("/screen_capture", Some(key.clone()))
        {
            self.screen_history.captures.pending_detail = Some(key);
        }
        let b = &mut self.screen_history.captures;
        ui.add_space(8.);
        let current = b.page_key.as_ref() == Some(&b.key());
        if let Some(e) = b.page_error.as_ref().filter(|_| current) {
            ui.colored_label(CORAL, e);
            return;
        }
        let enabled = self.saved_config["screen_mode"] != "off";
        let seconds = self.saved_config["screen_seconds"].as_u64().unwrap_or(300);
        let schedule = if !enabled {
            "Automatic screen checks are off.".to_owned()
        } else if live {
            format!("Screen checks every {seconds} seconds")
        } else {
            format!("Recording stopped · Screen checks every {seconds} seconds when recording")
        };
        ui.horizontal_wrapped(|ui| {
            muted(ui, schedule);
            if let Some(at) = b.page.last_check {
                muted(
                    ui,
                    format!("· Last check {}", clock(at, "%b %-d, %H:%M:%S")),
                );
            }
        });
        if enabled {
            if live && let Some(reason) = b.page.delay.as_deref() {
                ui.colored_label(CORAL, format!("Screen checks delayed. {reason}"));
            } else if let Some(error) = b.page.capture_error.as_deref() {
                ui.colored_label(CORAL, format!("Last screen check failed. {error}"));
            }
        }
        if current && b.page.items.is_empty() && b.selected.is_none() {
            if !b.query.trim().is_empty() {
                muted(ui, "No captures match this search.");
            } else if live && enabled && b.page.capture_error.is_none() && b.page.delay.is_none() {
                muted(ui, "Waiting for the first screen check…");
            } else {
                muted(ui, "No screen captures yet.");
            }
            return;
        }
        let (width, height) = (ui.available_width(), ui.available_height());
        let narrow = width < 535.;
        let sidebar = if narrow {
            width
        } else {
            (width * 0.25).clamp(140., 225.)
        };
        let list_height = if narrow {
            (height * 0.32).clamp(85., 170.)
        } else {
            height
        };
        let split = if narrow {
            egui::Layout::top_down(egui::Align::Min)
        } else {
            egui::Layout::left_to_right(egui::Align::Min)
        };
        ui.with_layout(split, |ui| {
            ui.allocate_ui_with_layout(
                vec2(sidebar, list_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_max_size(vec2(sidebar, list_height));
                    if !current {
                        ui.spinner();
                        return;
                    }
                    if !b.cursors.is_empty() || b.page.next.is_some() {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!b.cursors.is_empty(), egui::Button::new("‹ Newer"))
                                .clicked()
                            {
                                b.cursors.pop();
                                b.reset_selection();
                            }
                            if let Some(next) = b.page.next
                                && ui.button("Older ›").clicked()
                            {
                                b.cursors.push(next);
                                b.reset_selection();
                            }
                        });
                    }
                    let mut choose = None;
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(ui.available_height())
                        .id_salt(("passive-list", &b.query, b.cursors.last()))
                        .show(ui, |ui| {
                            let mut date = String::new();
                            let available = ui.available_width();
                            for item in &b.page.items {
                                let day = clock(item.at, "%a, %b %-d");
                                if day != date {
                                    ui.add_space(4.);
                                    ui.label(RichText::new(&day).color(MUTED).size(12.).strong());
                                    date = day;
                                }
                                let selected =
                                    b.selected.as_ref().is_some_and(|v| v["id"] == item.id);
                                let (row, r) =
                                    ui.allocate_exact_size(vec2(available, 43.), Sense::click());
                                let ink = if selected { MINT } else { INK };
                                if selected || r.hovered() {
                                    ui.painter().rect_filled(
                                        row,
                                        5.,
                                        if selected {
                                            Color32::from_rgb(222, 235, 226)
                                        } else {
                                            CARD
                                        },
                                    );
                                }
                                let painter = ui
                                    .painter()
                                    .with_clip_rect(row.shrink(2.).intersect(ui.clip_rect()));
                                let title = clock(item.at, "%H:%M:%S");
                                painter.text(
                                    row.left_top() + vec2(7., 4.),
                                    egui::Align2::LEFT_TOP,
                                    &title,
                                    FontId::proportional(14.),
                                    ink,
                                );
                                if item.image_available {
                                    design::icon(
                                        &painter,
                                        pos2(row.right() - 13., row.top() + 12.),
                                        13.,
                                        design::Icon::Photo,
                                        MUTED,
                                    );
                                }
                                let summary = item
                                    .excerpt
                                    .lines()
                                    .next()
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("No text detected");
                                let mut job = egui::text::LayoutJob::simple_singleline(
                                    summary.to_owned(),
                                    FontId::proportional(12.),
                                    MUTED,
                                );
                                job.wrap.max_width = (row.width() - 14.).max(1.);
                                job.wrap.max_rows = 1;
                                job.wrap.break_anywhere = true;
                                let snippet = painter.layout_job(job);
                                painter.galley(row.left_top() + vec2(7., 23.), snippet, MUTED);
                                r.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::SelectableLabel,
                                        true,
                                        selected,
                                        format!("{title}  {summary}"),
                                    )
                                });
                                if r.clicked() {
                                    choose = Some(item.id);
                                }
                            }
                        });
                    if let Some(id) = choose {
                        b.select(json!({"id":id}), true);
                        ui.ctx().request_repaint();
                    }
                },
            );
            ui.separator();
            ui.allocate_ui_with_layout(
                ui.available_size(),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    if let Some(e) = &b.error {
                        ui.colored_label(CORAL, e);
                    } else if let Some(c) = &b.capture {
                        show_capture(ui, c, &mut b.display, &self.base);
                    } else {
                        ui.spinner();
                    }
                },
            );
        });
    }
}

fn show_capture(ui: &mut egui::Ui, c: &Capture, display: &mut Display, base: &str) {
    let layout = &c.layout;
    ui.label(
        RichText::new(clock(c.at, "%B %-d · %H:%M:%S"))
            .font(design::serif(23.))
            .color(INK),
    );
    if c.legacy {
        muted(
            ui,
            "Earlier recording · Only lines first seen at this time are available.",
        );
    } else if !c.image_available {
        muted(
            ui,
            "Screenshots are kept for the latest 50 checks. This capture has text history only.",
        );
    }
    let mut mode = if display.mode == Mode::Screenshot && !c.image_available {
        Mode::Layout
    } else {
        display.mode
    };
    ui.horizontal_wrapped(|ui| {
        if c.image_available
            && ui
                .selectable_value(&mut mode, Mode::Screenshot, "Screenshot")
                .clicked()
        {
            display.mode = mode;
        }
        if ui
            .selectable_value(&mut mode, Mode::Layout, "Text layout")
            .clicked()
        {
            display.mode = mode;
        }
        if ui
            .selectable_value(&mut mode, Mode::Reading, "Reading order")
            .clicked()
        {
            display.mode = mode;
        }
        ui.separator();
        if !layout.lines.is_empty() && ui.small_button("Copy capture text").clicked() {
            ui.ctx().copy_text(layout.text());
        }
    });
    let placed = layout
        .lines
        .iter()
        .any(|l| l.bounds.is_some() || !l.words.is_empty());
    if layout.ocr_failed {
        muted(ui, "Daybook couldn’t read the text in this capture.");
    } else if layout.lines.is_empty() {
        muted(ui, "No readable text in this capture.");
    }
    let photo = mode == Mode::Screenshot;
    if mode == Mode::Reading || (!photo && !placed) {
        if !layout.lines.is_empty() && mode != Mode::Reading {
            muted(
                ui,
                "This capture has no saved positions. Its text is shown in reading order.",
            );
        }
        egui::ScrollArea::vertical()
            .id_salt(("capture-reading", c.id))
            .show(ui, |ui| {
                for line in &layout.lines {
                    ui.add(egui::Label::new(&line.text).selectable(true).wrap());
                }
                if c.truncated {
                    muted(
                        ui,
                        "Showing the first 200 lines from this earlier recording.",
                    );
                }
            });
        return;
    }
    let label = layout
        .pixels
        .map(|[w, h]| format!("{w} × {h}"))
        .unwrap_or_else(|| "Original screen size unknown".into());
    let partial = c.truncated
        || layout
            .lines
            .iter()
            .any(|l| l.bounds.is_none() && l.words.len() < l.text.split_whitespace().count());
    ui.horizontal_wrapped(|ui| {
        muted(
            ui,
            format!(
                "{label}  ·  {} {}{}",
                layout.lines.len(),
                if layout.lines.len() == 1 {
                    "line"
                } else {
                    "lines"
                },
                if partial {
                    "  ·  Some positions unavailable"
                } else {
                    ""
                }
            ),
        );
        if photo && placed {
            let mut overlay = !display.hide_overlay;
            if ui.checkbox(&mut overlay, "Text overlay").changed() {
                display.hide_overlay = !overlay;
            }
        }
        ui.toggle_value(&mut display.magnified, "2×")
            .on_hover_text("Enlarge this capture");
    });
    let texture = if photo {
        let Some(texture) = display.image.passive_texture(ui, base, c.id) else {
            return;
        };
        Some(texture)
    } else {
        None
    };
    let aspect = layout
        .pixels
        .map(|[w, h]| w as f32 / h as f32)
        .unwrap_or(1.);
    let avail = ui.available_size();
    let fit = (avail.x - 14.).min((avail.y - 14.) * aspect).max(1.);
    let w = fit * if display.magnified { 2. } else { 1. };
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .id_salt(("capture-canvas", c.id, display.magnified))
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(w, w / aspect), Sense::hover());
            let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
            if let Some(id) = texture {
                painter.image(
                    id,
                    rect,
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
            } else {
                painter.rect_filled(rect, 4., CARD);
            }
            painter.rect_stroke(rect, 4., Stroke::new(1., LINE), StrokeKind::Inside);
            if !photo || !display.hide_overlay {
                for (i, line) in layout.lines.iter().enumerate() {
                    draw_line(&painter, rect, line, photo);
                    if let Some(bounds) = line.bounds {
                        let area = on_screen(rect, bounds);
                        if ui.rect_contains_pointer(area) {
                            ui.interact(area, ui.id().with(("ocr-line", c.id, i)), Sense::hover())
                                .on_hover_text(&line.text);
                        }
                    }
                }
            }
        });
}

fn on_screen(canvas: Rect, [x, y, w, h]: Bounds) -> Rect {
    Rect::from_min_size(
        pos2(
            canvas.left() + x * canvas.width(),
            canvas.top() + (1. - y - h) * canvas.height(),
        ),
        vec2(w * canvas.width(), h * canvas.height()),
    )
}

// Vision sometimes returns the entire line for every word. Lay out that line once.
// Distinct word boxes retain the actual gaps, even in right-to-left text.
fn individual_words(line: &Line) -> bool {
    if line.bounds.is_none() {
        return true;
    }
    if line.words.len() < 2 || line.words.len() != line.text.split_whitespace().count() {
        return false;
    }
    line.words.windows(2).all(|pair| {
        let (_, [x, _, w, _]) = pair[0];
        let (_, [a, _, b, _]) = pair[1];
        (x + w).min(a + b) - x.max(a) < 0.45 * w.min(b)
    })
}
fn draw_line(p: &egui::Painter, canvas: Rect, line: &Line, overlay: bool) {
    if individual_words(line) {
        let mut tokens = line.text.split_whitespace().enumerate().peekable();
        for &(i, b) in &line.words {
            while tokens.peek().is_some_and(|(n, _)| *n < i) {
                tokens.next();
            }
            if let Some((_, word)) = tokens.next() {
                draw_text(p, on_screen(canvas, b), word, overlay);
            }
        }
    } else if let Some(b) = line.bounds {
        draw_text(p, on_screen(canvas, b), &line.text, overlay);
    }
}
fn draw_text(p: &egui::Painter, rect: Rect, text: &str, overlay: bool) {
    if !p.clip_rect().intersects(rect) || rect.width() <= 0. || rect.height() <= 0. {
        return;
    }
    if overlay {
        p.rect_filled(rect.expand(0.8), 1., Color32::from_rgb(231, 245, 232));
    }
    let font = FontId::proportional(rect.height().max(1.));
    let mut g = p.layout_no_wrap(text.to_owned(), font.clone(), INK);
    let scale = (rect.width() / g.size().x.max(0.01))
        .min(rect.height() / g.size().y.max(0.01))
        .min(1.);
    if scale < 0.99 {
        g = p.layout_no_wrap(
            text.to_owned(),
            FontId::proportional((font.size * scale).max(0.5)),
            INK,
        );
    }
    let pos = rect.center() - g.size() * 0.5;
    p.with_clip_rect(rect.intersect(p.clip_rect()))
        .galley(pos, g, INK);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vision_positions_flip_vertically_and_use_the_full_screen_aspect() {
        let canvas = Rect::from_min_size(pos2(10., 20.), vec2(800., 400.));
        let top = on_screen(canvas, [0.125, 0.75, 0.25, 0.125]);
        assert_eq!(top.min, pos2(110., 70.));
        assert_eq!(top.size(), vec2(200., 50.));
        let bottom = on_screen(canvas, [0.75, 0., 0.25, 0.125]);
        assert_eq!(bottom.max, canvas.max);
        let mut line = Line {
            text: "Open Open".into(),
            bounds: Some([0., 0., 1., 1.]),
            words: vec![(0, [0.1, 0., 0.2, 0.1]), (1, [0.65, 0., 0.2, 0.1])],
        };
        assert!(individual_words(&line));
        line.words[1].1 = line.words[0].1;
        assert!(!individual_words(&line));
        line.words.remove(0);
        assert!(!individual_words(&line));
        line.bounds = None;
        assert!(individual_words(&line));
    }
    #[test]
    fn browsing_ignores_stale_replies_and_refresh_keeps_the_chosen_check() {
        let mut b = Browser::default();
        let send_page = |b: &mut Browser, latest: i64| {
            b.reply(Reply {
                path: "/screen_captures".into(),
                body: Some(b.key()),
                result: Ok(json!({"items":[{"id":latest,"at":100+latest*5,"excerpt":"A"},{"id":1,"at":100,"excerpt":"A"}]})),
            });
        };
        send_page(&mut b, 2);
        assert_eq!(b.selected, Some(json!({"id":2})));
        send_page(&mut b, 3);
        assert_eq!(b.selected, Some(json!({"id":3})));
        b.select(json!({"id":1}), true);
        b.reply(Reply {
            path: "/screen_capture".into(),
            body: Some(json!({"id":2})),
            result: Err("stale".into()),
        });
        assert!(b.error.is_none());
        send_page(&mut b, 4);
        assert_eq!(b.selected, Some(json!({"id":1})));
        b.reply(Reply {
            path: "/screen_capture".into(),
            body: b.selected.clone(),
            result: Ok(
                json!({"id":1,"at":100,"legacy":false,"layout":{"pixels":[800,400],"lines":[]}}),
            ),
        });
        assert!(b.ready());
        b.reset_selection();
        send_page(&mut b, 4);
        assert_eq!(b.selected, Some(json!({"id":4})));
        b.query = "New search".into();
        b.reset_selection();
        b.reply(Reply {
            path: "/screen_captures".into(),
            body: Some(json!({"query":"","before":null})),
            result: Err("stale".into()),
        });
        assert!(b.page_error.is_none());
        assert!(!b.ready());
    }
}
