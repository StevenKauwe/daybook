//! Archive browsing, time selection and a virtualized journal list.
use super::*;
use chrono::{Local, NaiveDate, NaiveTime, Timelike};

const ENTRY_H: f32 = 76.;
const HOUR_H: f32 = 39.;

enum Line {
    Hour(String),
    Entry {
        id: i64,
        time: String,
        label: &'static str,
        excerpt: String,
        accent: Color32,
    },
}
struct Row {
    top: f32,
    line: Line,
}
#[derive(Default)]
pub struct Archive {
    rows: Vec<Row>,
    height: f32,
    pub activity: Value,
    pub selection: Option<(i64, i64)>,
    drag: Option<(i64, i64)>,
    from_edit: String,
    until_edit: String,
    time_error: String,
    search_edit: String,
    pub reading_back: bool,
    rewind: bool,
    offset: f32,
    restore_offset: Option<f32>,
    pub loaded_filter: Value,
    pub copying: bool,
    pub clipboard: Option<String>,
    pub copy_receipt: String,
}
fn clock(at: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|t| t.with_timezone(&Local).format(format).to_string())
        .unwrap_or_default()
}
fn choice(ui: &mut egui::Ui, id: &str, current: &mut String, options: &[(&str, &str)]) -> bool {
    let mut changed = false;
    let label = options
        .iter()
        .find(|(key, _)| key == current)
        .map_or("All", |(_, name)| *name);
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .show_ui(ui, |ui| {
            for &(key, label) in options {
                if ui.selectable_label(current == key, label).clicked() {
                    changed |= current != key;
                    *current = key.into();
                    ui.close();
                }
            }
        });
    changed
}
impl Archive {
    pub fn flush_clipboard(&mut self, ctx: &egui::Context) {
        if let Some(text) = self.clipboard.take() {
            ctx.copy_text(text);
        }
    }
    pub fn reset_day(&mut self) {
        self.selection = None;
        self.activity = Value::Null;
        self.rows.clear();
        self.height = 0.;
        self.loaded_filter = Value::Null;
        self.rewind = true;
        self.reading_back = false;
        self.drag = None;
        self.from_edit.clear();
        self.until_edit.clear();
        self.time_error.clear();
        self.copy_receipt.clear();
    }
    pub fn layout(&mut self, items: &[Value]) {
        let anchor = if self.reading_back && !self.rewind {
            self.rows.iter().find_map(|r| match &r.line {
                Line::Entry { id, .. } if r.top + ENTRY_H > self.offset => {
                    Some((*id, r.top - self.offset))
                }
                _ => None,
            })
        } else {
            None
        };
        self.rows.clear();
        self.height = 0.;
        let mut previous = String::new();
        for item in items {
            let Some(id) = item["id"].as_i64() else {
                continue;
            };
            let at = item["at"].as_i64().unwrap_or(0);
            let hour = clock(at, "%Y%m%d%H%z");
            if hour != previous {
                self.rows.push(Row {
                    top: self.height,
                    line: Line::Hour(clock(at, "%-I %p")),
                });
                self.height += HOUR_H;
                previous = hour;
            }
            let kind = item["kind"].as_str().unwrap_or("");
            let source = item["source"].as_str().unwrap_or("");
            let label = reading::entry_label(kind, source);
            let mut excerpt = item["text"]
                .as_str()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if excerpt.is_empty() {
                excerpt = match kind {
                    "screen" => "Open the screen entry",
                    "preview" => "Listening…",
                    _ => "Open entry",
                }
                .into();
            }
            let accent = match kind {
                "note" => design::MINT,
                "screen" => Color32::from_rgb(151, 87, 37),
                _ => design::VIOLET,
            };
            self.rows.push(Row {
                top: self.height,
                line: Line::Entry {
                    id,
                    time: clock(at, "%-I:%M"),
                    label,
                    excerpt,
                    accent,
                },
            });
            self.height += ENTRY_H;
        }
        self.restore_offset = anchor.and_then(|(id, delta)| {
            self.rows.iter().find_map(|r| {
                matches!(r.line,Line::Entry{id:key,..} if key==id)
                    .then_some((r.top - delta).max(0.))
            })
        });
    }
    fn bounds(&self) -> Option<(i64, i64)> {
        Some((
            self.activity["start"].as_i64()?,
            self.activity["end"].as_i64()?,
        ))
    }
    fn apply(&mut self, range: Option<(i64, i64)>) {
        self.selection = range;
        self.drag = None;
        self.rewind = true;
        self.reading_back = false;
        self.copy_receipt.clear();
        self.time_error.clear();
        if let Some((a, b)) = range {
            self.from_edit = clock(a, "%H:%M:%S");
            self.until_edit = if self.bounds().is_some_and(|(_, end)| end == b) {
                "24:00".into()
            } else {
                clock(b, "%H:%M:%S")
            };
        }
    }
    fn typed_time(&self, day: &str, input: &str, end: bool) -> Result<i64, &'static str> {
        if end && matches!(input.trim(), "24:00" | "24:00:00") {
            return self.bounds().map(|b| b.1).ok_or("Load the day first");
        }
        let t = NaiveTime::parse_from_str(input.trim(), "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(input.trim(), "%H:%M"))
            .map_err(|_| "Use 24-hour times such as 09:15 or 09:15:30")?;
        let local = NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .map_err(|_| "Invalid date")?
            .and_time(t)
            .and_local_timezone(Local);
        // For a repeated hour give the selection its widest consistent interpretation.
        let at = if end {
            local.latest()
        } else {
            local.earliest()
        };
        at.map(|d| d.timestamp())
            .ok_or("That local time does not exist on this date")
    }
    fn strip(&mut self, ui: &mut egui::Ui) -> bool {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 50.),
            egui::Sense::click_and_drag(),
        );
        let Some((start, end)) = self.bounds() else {
            return false;
        };
        let (left, right) = (rect.left() + 2., rect.right() - 2.);
        let x = |at: i64| left + (right - left) * ((at - start) as f32 / (end - start) as f32);
        let time = |p: f32| {
            start
                + ((((p - left) / (right - left).max(1.)).clamp(0., 1.)
                    * ((end - start) / 60) as f32)
                    .round() as i64)
                    * 60
        };
        if response.drag_started() {
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                self.drag = Some((time(p.x), time(p.x)));
            }
        }
        if let Some((a, b)) = &mut self.drag {
            if let Some(pos) = response.interact_pointer_pos() {
                *b = time(pos.x);
            }
            if *a == *b && !response.drag_stopped() {
                ui.ctx().request_repaint();
            }
        }
        let ordered = |a: i64, b: i64| {
            let lo = a.min(b).min(end - 60);
            (lo, a.max(b).max(lo + 60).min(end))
        };
        let mut changed = false;
        if response.drag_stopped() {
            if let Some((a, b)) = self.drag.take() {
                self.apply(Some(ordered(a, b)));
                changed = true;
            }
        } else if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let lo = start + ((time(p.x) - start) / 900) * 900;
                self.apply(Some(ordered(lo.min(end - 900), lo.min(end - 900) + 900)));
                changed = true;
            }
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                true,
                "Day activity. Drag to select a time range, or use Set times.",
            )
        });
        let p = ui.painter();
        let baseline = rect.top() + 26.;
        let band = egui::Rect::from_min_max(
            egui::pos2(left, rect.top() + 1.),
            egui::pos2(right, baseline),
        );
        p.rect_filled(band, 4., Color32::from_rgb(232, 236, 224));
        if let Some(bins) = self.activity["bins"].as_array() {
            let peak = bins
                .iter()
                .filter_map(Value::as_u64)
                .max()
                .unwrap_or(1)
                .max(1) as f32;
            for (i, total) in bins.iter().enumerate() {
                let n = total.as_u64().unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let height = 4. + 18. * (n as f32 / peak).sqrt();
                let l = x(start + i as i64 * 900);
                let r = x((start + (i + 1) as i64 * 900).min(end));
                p.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(l + 0.5, baseline - height),
                        egui::pos2(r - 0.5, baseline),
                    ),
                    1.,
                    MINT,
                );
            }
        }
        if let Some((a, b)) = self.drag.map(|(a, b)| ordered(a, b)).or(self.selection) {
            let selected =
                egui::Rect::from_min_max(egui::pos2(x(a), band.top()), egui::pos2(x(b), baseline));
            p.rect_filled(
                selected,
                1.,
                Color32::from_rgba_unmultiplied(179, 125, 46, 66),
            );
            for i in [a, b] {
                p.vline(
                    x(i),
                    band.y_range(),
                    egui::Stroke::new(2., Color32::from_rgb(145, 77, 23)),
                );
            }
        }
        for t in (start..end).step_by(3600) {
            if let Some(dt) = chrono::DateTime::from_timestamp(t, 0) {
                let hour = dt.with_timezone(&Local).hour();
                if hour % 6 == 0 {
                    p.text(
                        egui::pos2(x(t), baseline + 4.),
                        if t == start {
                            egui::Align2::LEFT_TOP
                        } else {
                            egui::Align2::CENTER_TOP
                        },
                        clock(t, "%-I %p"),
                        egui::FontId::proportional(11.),
                        MUTED,
                    );
                }
            }
        }
        p.text(
            egui::pos2(right, baseline + 4.),
            egui::Align2::RIGHT_TOP,
            "12 AM",
            egui::FontId::proportional(11.),
            MUTED,
        );
        response.on_hover_text("Drag to select a time range. Click a bar for those 15 minutes.");
        changed
    }
}
impl Controls {
    fn archive_changed(&mut self) {
        self.pages = 1;
        self.archive.rewind = true;
        self.archive.reading_back = false;
        self.archive.copy_receipt.clear();
        self.refresh(false);
    }
    fn range_controls(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if let Some((a, b)) = self.archive.selection {
                let format = if a % 60 == 0 && b % 60 == 0 {
                    "%-I:%M %p"
                } else {
                    "%-I:%M:%S %p"
                };
                ui.strong(format!("{} – {}", clock(a, format), clock(b, format)));
                if ui
                    .add_enabled(
                        !self.archive.copying,
                        egui::Button::new(RichText::new("Copy selection").color(CARD)).fill(MINT),
                    )
                    .clicked()
                {
                    if self.send("/copy_time_range", Some(self.filter())) {
                        self.archive.copying = true;
                    }
                }
                if ui.small_button("Clear").clicked() {
                    self.archive.apply(None);
                    changed = true;
                }
            } else {
                ui.strong("Day timeline");
            }
            ui.menu_button("Set times…", |ui| {
                if self.archive.from_edit.is_empty() {
                    self.archive.from_edit = "09:00".into();
                    self.archive.until_edit = "10:00".into();
                }
                for (label, value) in [
                    ("From", &mut self.archive.from_edit),
                    ("Until", &mut self.archive.until_edit),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        ui.add(egui::TextEdit::singleline(value).desired_width(82.));
                    });
                }
                if ui.button("Select range").clicked() {
                    let a = self
                        .archive
                        .typed_time(&self.day, &self.archive.from_edit, false);
                    let b = self
                        .archive
                        .typed_time(&self.day, &self.archive.until_edit, true);
                    match (a, b, self.archive.bounds()) {
                        (Ok(a), Ok(b), Some((lo, hi))) if lo <= a && a < b && b <= hi => {
                            self.archive.apply(Some((a, b)));
                            changed = true;
                            ui.close();
                        }
                        (Err(e), _, _) | (_, Err(e), _) => self.archive.time_error = e.into(),
                        _ => {
                            self.archive.time_error =
                                "Choose an end time later on the same day".into()
                        }
                    }
                }
                if !self.archive.time_error.is_empty() {
                    ui.colored_label(CORAL, &self.archive.time_error);
                }
            });
            if !self.archive.copy_receipt.is_empty() {
                ui.weak(&self.archive.copy_receipt);
            } else if self.archive.selection.is_none() {
                ui.weak("Drag across the day to select time.");
            }
        });
        changed |= self.archive.strip(ui);
        changed
    }
    pub(super) fn timeline(&mut self, ui: &mut egui::Ui) {
        self.date_bar(ui);
        ui.add_space(3.);
        let changed = self.range_controls(ui);
        let mut filters = false;
        ui.horizontal_wrapped(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.archive.search_edit)
                    .desired_width(175.)
                    .hint_text("Search this day"),
            );
            if ui.small_button("Search").clicked()
                || response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
            {
                self.query = self.archive.search_edit.clone();
                filters = true;
            }
            filters |= choice(
                ui,
                "archive_kind",
                &mut self.kind,
                &[
                    ("all", "All entries"),
                    ("transcript", "Conversations"),
                    ("preview", "Live passages"),
                    ("note", "Notes"),
                    ("screen", "Screens"),
                    ("transcript_suppressed", "Suppressed audio"),
                ],
            );
            filters |= choice(
                ui,
                "archive_source",
                &mut self.source,
                &[
                    ("all", "All sources"),
                    ("me", "Me"),
                    ("remote", "System audio"),
                    ("screen", "Screen text"),
                    ("vision", "Screen descriptions"),
                    ("clipboard", "Clipboard"),
                    ("screenshot", "Saved screenshots"),
                ],
            );
            if self.pages > 1 {
                if ui.small_button("Back to latest").clicked() {
                    self.archive_changed();
                }
            } else if ui.small_button("Refresh").clicked() {
                self.refresh(false);
            }
        });
        if filters {
            self.archive.activity = Value::Null;
        }
        if filters || changed {
            self.archive_changed();
        }
        if self.archive.loaded_filter != self.filter() {
            ui.weak("Loading entries…");
            return;
        }
        if self.items.is_empty() {
            ui.add_space(24.);
            ui.heading(
                if self.archive.selection.is_some()
                    || self.kind != "all"
                    || self.source != "all"
                    || !self.query.is_empty()
                {
                    "No matching entries"
                } else {
                    "Nothing saved on this day"
                },
            );
            if !self.query.is_empty()
                || self.kind != "all"
                || self.source != "all"
                || self.archive.selection.is_some()
            {
                ui.label("Try a different time range or search.");
            } else {
                ui.label("Save a note or a screenshot from the Daybook menu.");
                if self.status["synthetic"] != true {
                    let permissions: Value =
                        serde_json::from_str(self.status["permissions"].as_str().unwrap_or("{}"))
                            .unwrap_or_default();
                    let model_ready = std::path::Path::new(&self.config.model_path).is_file();
                    if (!model_ready
                        || permissions["microphone_permission"] != 3
                        || permissions["screen_permission"] != true)
                        && ui.small_button("Set up recording").clicked()
                    {
                        self.settings_tab = if model_ready { 3 } else { 0 };
                        self.navigate(4);
                    }
                }
            }
            return;
        }
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("archive_entries")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.archive.rewind) {
            self.archive.restore_offset = None;
            scroll = scroll.vertical_scroll_offset(0.);
        } else if let Some(y) = self.archive.restore_offset.take() {
            scroll = scroll.vertical_scroll_offset(y);
        }
        let mut open = None;
        let mut more = false;
        let output = scroll.show_viewport(ui, |ui, view| {
            let rows = &self.archive.rows;
            let height = self.archive.height;
            let footer = !self.next.is_null();
            ui.set_height(height + if footer { 48. } else { 0. });
            let top = ui.max_rect().top();
            let width = ui.max_rect().width().max(80.);
            let first = rows
                .partition_point(|row| row.top <= view.top())
                .saturating_sub(1);
            for row in &rows[first..] {
                if row.top > view.bottom() {
                    break;
                }
                let origin = egui::pos2(ui.max_rect().left(), top + row.top);
                match &row.line {
                    Line::Hour(name) => {
                        ui.painter().text(
                            origin + egui::vec2(3., HOUR_H * 0.55),
                            egui::Align2::LEFT_CENTER,
                            name,
                            design::serif(20.),
                            design::TEXT,
                        );
                        ui.painter().hline(
                            origin.x + 75. ..=origin.x + width - 4.,
                            origin.y + HOUR_H * 0.55,
                            egui::Stroke::new(1., design::LINE),
                        );
                    }
                    Line::Entry {
                        id,
                        time,
                        label,
                        excerpt,
                        accent,
                    } => {
                        let rect = egui::Rect::from_min_size(origin, egui::vec2(width, ENTRY_H));
                        let response = ui.interact(
                            rect,
                            ui.make_persistent_id(("archive_entry", id)),
                            egui::Sense::click(),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                format!("{time}, {label}, {excerpt}"),
                            )
                        });
                        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                        let p = ui.painter();
                        if response.hovered() || response.has_focus() {
                            p.rect_filled(rect.shrink(1.), 5., Color32::from_rgb(232, 237, 225));
                        }
                        p.text(
                            origin + egui::vec2(3., 13.),
                            egui::Align2::LEFT_TOP,
                            time,
                            egui::FontId::monospace(13.),
                            MUTED,
                        );
                        p.vline(
                            origin.x + 58.,
                            rect.y_range(),
                            egui::Stroke::new(1., design::LINE),
                        );
                        p.circle_filled(origin + egui::vec2(58., 20.), 3.5, *accent);
                        p.text(
                            origin + egui::vec2(75., 8.),
                            egui::Align2::LEFT_TOP,
                            *label,
                            egui::FontId::proportional(13.),
                            *accent,
                        );
                        let mut job = egui::text::LayoutJob::simple(
                            excerpt.clone(),
                            design::passage_font(18.),
                            design::TEXT,
                            (width - 101.).max(35.),
                        );
                        job.wrap.max_rows = 2;
                        job.wrap.break_anywhere = false;
                        let galley = p.layout_job(job);
                        p.galley(origin + egui::vec2(75., 26.), galley, design::TEXT);
                        p.text(
                            rect.right_center() - egui::vec2(9., 0.),
                            egui::Align2::RIGHT_CENTER,
                            "›",
                            egui::FontId::proportional(19.),
                            MUTED,
                        );
                        if response.clicked() {
                            open = Some(*id);
                        }
                    }
                }
            }
            if footer && view.bottom() >= height - 4. {
                let rect = egui::Rect::from_min_size(
                    egui::pos2(ui.max_rect().left() + 2., top + height + 4.),
                    egui::vec2(180., 32.),
                );
                if self.items.len() < 1000 {
                    more = ui
                        .put(rect, egui::Button::new("Load earlier entries"))
                        .clicked();
                } else {
                    ui.painter().text(
                        rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        "Narrow the time range to see more.",
                        egui::FontId::proportional(14.),
                        MUTED,
                    );
                }
            }
        });
        self.archive.reading_back = output.state.offset.y > 2.;
        self.archive.offset = output.state.offset.y;
        if more {
            self.refresh(true);
        }
        if let Some(id) = open {
            self.inspect("observation", id, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn controls() -> (Controls, SyncSender<Reply>, Receiver<Request>) {
        let (tx, req) = mpsc::sync_channel(12);
        let (reply, rx) = mpsc::sync_channel(12);
        let mut c = Controls::initial("synthetic://archive".into(), tx, rx);
        preview::archive_fixture(&mut c, false);
        (c, reply, req)
    }

    #[test]
    fn identical_refresh_keeps_row_geometry_and_new_items_preserve_reader_position() {
        let (mut c, reply, _req) = controls();
        let ctx = egui::Context::default();
        design::setup(&ctx);
        let measure = |c: &mut Controls| {
            let mut found = None;
            let out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900., 700.),
                    )),
                    ..Default::default()
                },
                |ui| c.timeline(ui),
            );
            for shape in &out.shapes {
                if let egui::Shape::Text(t) = &shape.shape {
                    if t.galley.job.text.starts_with("Start the listening walk") {
                        found = Some(t.pos.y);
                    }
                }
            }
            out.drop_without_applying_deltas();
            found.expect("First entry should be visible")
        };
        measure(&mut c);
        let y = measure(&mut c);
        let rows = c.archive.rows.as_ptr();
        c.loading_timeline = true;
        assert_eq!(measure(&mut c), y);
        let mut body = c.filter();
        body["background"] = json!(true);
        let data = json!({"items":c.items,"activity":c.archive.activity,"jobs":[]});
        reply
            .send(Reply {
                path: "/timeline".into(),
                body: Some(body.clone()),
                result: Ok(data.clone()),
            })
            .unwrap();
        c.poll();
        assert_eq!(measure(&mut c), y);
        assert_eq!(rows, c.archive.rows.as_ptr());
        let mut expanded = data;
        let newer =
            json!({"id":999,"at":c.items[0]["at"],"kind":"note","source":"me","text":"a new note"});
        expanded["items"].as_array_mut().unwrap().insert(0, newer);
        c.archive.reading_back = true;
        c.archive.offset = HOUR_H + 2. * ENTRY_H + 12.;
        reply
            .send(Reply {
                path: "/timeline".into(),
                body: Some(body.clone()),
                result: Ok(expanded.clone()),
            })
            .unwrap();
        c.poll();
        assert_ne!(c.items[0]["id"], 999);
        body["background"] = json!(false);
        reply
            .send(Reply {
                path: "/timeline".into(),
                body: Some(body),
                result: Ok(expanded),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.archive.restore_offset, Some(c.archive.offset + ENTRY_H));
    }

    #[test]
    fn copy_response_writes_plain_text_to_egui_clipboard_once() {
        let (mut c, reply, _) = controls();
        let text = "Daybook · 09:00 – 10:00\n\n09:15  Your note\nCafé";
        reply
            .send(Reply {
                path: "/copy_time_range".into(),
                body: Some(c.filter()),
                result: Ok(json!({"count":1,"text":text})),
            })
            .unwrap();
        c.poll();
        let ctx = egui::Context::default();
        for expected in [1, 0] {
            let out = ctx.run_ui(Default::default(), |ui| c.archive.flush_clipboard(ui.ctx()));
            assert_eq!(
                out.platform_output
                    .commands
                    .iter()
                    .filter(|cmd| matches!(cmd,egui::OutputCommand::CopyText(s) if s==text))
                    .count(),
                expected
            );
            out.drop_without_applying_deltas();
        }
        assert_eq!(c.archive.copy_receipt, "Copied 1 entry");
    }
    #[test]
    fn timeline_drag_and_typed_time_choose_the_same_interval() {
        let (mut c, _, _) = controls();
        let ctx = egui::Context::default();
        let mut frame = |events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900., 100.),
                )),
                events,
                ..Default::default()
            };
            let mut rect = egui::Rect::NOTHING;
            let out = ctx.run_ui(raw, |ui| {
                rect = egui::Rect::from_min_size(
                    ui.next_widget_position(),
                    egui::vec2(ui.available_width(), 50.),
                );
                c.archive.strip(ui);
            });
            out.drop_without_applying_deltas();
            rect
        };
        let rect = frame(vec![]);
        let a = egui::pos2(
            rect.left() + 2. + (rect.width() - 4.) * 0.25,
            rect.center().y,
        );
        let b = egui::pos2(
            rect.left() + 2. + (rect.width() - 4.) * 0.5,
            rect.center().y,
        );
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(a), button(a, true)]);
        frame(vec![egui::Event::PointerMoved(b)]);
        frame(vec![button(b, false)]);
        let (start, end) = c.archive.bounds().unwrap();
        let range = (
            start + ((end - start) / 4 / 60) * 60,
            start + ((end - start) / 2 / 60) * 60,
        );
        assert_eq!(c.archive.selection, Some(range));
        assert_eq!(
            c.archive.typed_time(&c.day, &c.archive.from_edit, false),
            Ok(range.0)
        );
        assert_eq!(c.filter()["until"], range.1);
        assert!(c.archive.typed_time(&c.day, "30:99", false).is_err());
    }
}
