//! The time-aligned reader owns its selected window independently of live transcription.
use super::*;
use crate::transcript_range::*;
use chrono::{Local, NaiveDateTime, NaiveTime, TimeZone};
use std::sync::Arc;

pub(super) fn clock(at: i64, fmt: &str) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|d| d.with_timezone(&Local).format(fmt).to_string())
        .unwrap_or_default()
}
fn time_input(s: &str, at: i64) -> Option<i64> {
    let date = chrono::DateTime::from_timestamp(at, 0)?
        .with_timezone(&Local)
        .date_naive();
    let full = NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S").ok();
    let local = full.or_else(|| {
        ["%H:%M:%S", "%H:%M"]
            .iter()
            .find_map(|fmt| NaiveTime::parse_from_str(s.trim(), fmt).ok())
            .map(|t| date.and_time(t))
    })?;
    Local
        .from_local_datetime(&local)
        .earliest()
        .map(|d| d.timestamp())
}
pub(super) struct Detail {
    pub lane: Lane,
    pub bucket: Span,
    pub range: Span,
    pub passive: bool,
    pub items: Vec<Passage>,
    pub more: bool,
    pub request: Option<Value>,
}
pub(super) struct Timeline {
    pub data: Option<Arc<Window>>,
    pub day: Option<String>,
    pub fixed: Option<Span>,
    pub seconds: i64,
    pending: Option<Value>,
    loaded: Value,
    checked: Option<Instant>,
    pub picks: Picks,
    pub marks: std::collections::BTreeMap<i64, (Lane, i64, i64)>,
    pub ruler: Option<Span>,
    time_fields: [Option<egui::Id>; 2],
    pub drag_start: Option<(i64, usize, egui::Id)>,
    pub detail: Option<Detail>,
    pub clipboard: Option<String>,
    copying: bool,
    preview_copy: bool,
    copy_text: Option<String>,
    receipt: String,
    pub held: Option<Arc<Window>>,
    pub overview_drag: Option<crate::time_ruler::Brush>,
    pub last_browse: Option<Instant>,
}
impl Default for Timeline {
    fn default() -> Self {
        Self {
            data: None,
            day: None,
            fixed: None,
            seconds: 60,
            pending: None,
            loaded: Value::Null,
            checked: None,
            picks: Default::default(),
            marks: Default::default(),
            ruler: None,
            time_fields: [None; 2],
            drag_start: None,
            detail: None,
            clipboard: None,
            copying: false,
            preview_copy: false,
            copy_text: None,
            receipt: String::new(),
            held: None,
            overview_drag: None,
            last_browse: None,
        }
    }
}
impl Timeline {
    pub fn mark(&mut self, id: i64, lane: Lane, at: i64, end: i64, choice: Pick) {
        self.marks.insert(id, (lane, at, end));
        self.picks.entries.insert(id, choice);
    }
    pub fn marked(&self, r: Span, lane: Lane) -> bool {
        self.marks.iter().any(|(id, (l, at, end))| {
            *l == lane
                && *at < r.until
                && (*at >= r.from || *end > r.from || lane == Lane::Passive && *end == r.from)
                && self.picks.entries.get(id).is_some_and(|p| *p != Pick::Skip)
        })
    }
    pub fn clear_picks(&mut self) {
        self.picks = Default::default();
        self.marks.clear();
    }
    fn query(&self, passive: bool) -> Value {
        json!({"range":self.fixed,"seconds":self.seconds,"passive":passive,"overview":self.ruler})
    }
    pub fn apply(&mut self, data: Window) {
        self.day = Some(clock(data.range.until - 1, "%F"));
        if self.overview_drag.is_none() {
            self.held = None;
        }
        self.data = Some(Arc::new(data));
    }
    pub fn set_range(&mut self, r: Span) {
        if !r.valid() {
            return;
        }
        self.fixed = Some(r);
        self.seconds = r.len();
        self.detail = None;
        self.day = Some(clock(r.until - 1, "%F"));
        self.receipt.clear();
    }
    /// Explicit navigation only pans the ruler if the new selection is outside it.
    pub fn reveal(&mut self, r: Span) {
        if let Some(axis) = self
            .ruler
            .or_else(|| self.data.as_ref().map(|w| w.overview))
        {
            let len = axis.len().max(r.len());
            let edge = axis.until.clamp(r.until, r.from + len);
            self.ruler = Some(Span::ending(edge, len));
        }
        self.set_range(r);
    }
    pub fn display(&self, passive: bool) -> Option<Arc<Window>> {
        let last = self.data.as_ref()?;
        let range = self.fixed.unwrap_or(last.range);
        if last.range == range && (self.loaded.is_null() || self.loaded["passive"] == passive) {
            return Some(last.clone());
        }
        let source = self
            .overview_drag
            .as_ref()
            .map(|d| &d.domain)
            .or(self.held.as_ref())
            .unwrap_or(last);
        let middle = if passive { Lane::Passive } else { Lane::System };
        let allowed = |lane| lane == Lane::Mic || lane == Lane::Saves || lane == middle;
        let within = range.from >= source.range.from && range.until <= source.range.until;
        let exact = within && source.clusters.is_empty();
        let contained = |p: &&Passage| {
            allowed(p.lane())
                && p.at < range.until
                && (p.at >= range.from
                    || p.end > range.from
                    || p.lane() == Lane::Passive && p.end == range.from)
        };
        let rows = if exact || source.overview == source.range {
            &source.passages
        } else {
            &source.nearby
        };
        let passages = rows.iter().filter(contained).cloned().collect();
        let groups = if within && !source.clusters.is_empty() {
            source.clusters.as_slice()
        } else if source.nearby_grouped {
            source.bands.as_slice()
        } else {
            &[]
        };
        let clusters = groups
            .iter()
            .filter(|g| allowed(g.lane) && g.range.from < range.until && g.range.until > range.from)
            .map(|g| {
                let mut g = g.clone();
                g.range.from = g.range.from.max(range.from);
                g.range.until = g.range.until.min(range.until);
                g
            })
            .collect();
        Some(Arc::new(Window {
            range,
            extent: source.extent,
            overview: source.overview,
            bands: vec![],
            passages,
            clusters,
            nearby: vec![],
            nearby_grouped: false,
            quiet_at: source
                .quiet_at
                .filter(|t| (range.from..range.until).contains(t)),
        }))
    }
    pub fn pin(&mut self, r: Span) {
        if self.fixed.is_none() {
            self.fixed = Some(r);
            self.seconds = r.len();
        }
    }
}
impl Controls {
    fn ask_window(&mut self) {
        if self.base.starts_with("synthetic://") || self.parallel.pending.is_some() {
            return;
        }
        // While the ruler is moving, reproject the activity already in memory. Request
        // the final overview after the gesture settles, keeping SQLite off the input path.
        if self.parallel.overview_drag.is_none()
            && self
                .parallel
                .last_browse
                .is_some_and(|at| at.elapsed() < Duration::from_millis(110))
        {
            return;
        }
        let body = self.parallel.query(self.reading.passive_screen);
        let historical = self
            .parallel
            .fixed
            .is_some_and(|r| r.until < chrono::Utc::now().timestamp() - 90 || r.len() > 86400);
        if self.parallel.overview_drag.is_some()
            && self
                .parallel
                .checked
                .is_some_and(|t| t.elapsed() < Duration::from_millis(90))
        {
            return;
        }
        let refresh = if historical {
            30
        } else if self.status["active"] == "true" {
            1
        } else {
            4
        };
        if self.parallel.loaded == body
            && self
                .parallel
                .checked
                .is_some_and(|t| t.elapsed() < Duration::from_secs(refresh))
        {
            return;
        }
        if self.send("/transcript_window", Some(body.clone())) {
            self.parallel.pending = Some(body);
            self.parallel.checked = Some(Instant::now());
        }
    }
    pub(super) fn parallel_reply(&mut self, r: Reply) {
        if r.path == "/transcript_window" {
            self.parallel.pending = None;
            if r.body.as_ref() != Some(&self.parallel.query(self.reading.passive_screen)) {
                return;
            }
            self.parallel.loaded = r.body.unwrap_or(Value::Null);
            match r
                .result
                .and_then(|v| serde_json::from_value::<Window>(v).map_err(|e| e.to_string()))
            {
                Ok(data) => self.parallel.apply(data),
                Err(e) => self.error = e,
            }
        } else if r.path == "/transcript_passages" {
            let Some(detail) = &mut self.parallel.detail else {
                return;
            };
            if detail.request.is_none() || r.body != detail.request {
                return;
            }
            detail.request = None;
            match r.result.and_then(|v| {
                let items = serde_json::from_value::<Vec<Passage>>(v["items"].clone())
                    .map_err(|e| e.to_string())?;
                Ok((items, v["more"] == true))
            }) {
                Ok((items, more)) => {
                    detail.items.extend(items);
                    detail.more = more
                }
                Err(e) => self.error = e,
            }
        } else {
            self.parallel.copying = false;
            let preview = std::mem::take(&mut self.parallel.preview_copy);
            match r.result {
                Ok(v) => {
                    let text = v["text"].as_str().map(str::to_owned);
                    if preview {
                        self.parallel.copy_text = text;
                        return;
                    }
                    self.parallel.clipboard = text;
                    self.parallel.receipt = format!(
                        "Copied {} {}",
                        v["count"],
                        if v["count"] == 1 {
                            "passage"
                        } else {
                            "passages"
                        }
                    );
                }
                Err(e) => self.error = e,
            }
        }
    }
    pub(super) fn copy_parallel(&mut self, r: Span, lane: Option<Lane>, selected: bool) {
        self.parallel.pin(r);
        if self.base.starts_with("synthetic://") || self.parallel.copying {
            return;
        }
        let mut body = json!({"range":r,"lane":lane,"passive":self.reading.passive_screen,"selected":selected});
        if selected {
            body["picks"] = json!(self.parallel.picks);
        }
        self.parallel.copying = self.send("/copy_transcript", Some(body));
    }
    pub(super) fn details_at(&mut self, r: Span, bucket: Span, lane: Lane) {
        self.parallel.pin(r);
        self.parallel.detail = Some(Detail {
            lane,
            bucket,
            range: r,
            passive: self.reading.passive_screen,
            items: vec![],
            more: true,
            request: None,
        });
        self.next_detail();
    }
    fn next_detail(&mut self) {
        let Some(d) = &self.parallel.detail else {
            return;
        };
        if d.request.is_some() || !d.more {
            return;
        }
        let mut b = json!({"range":d.range,"bucket":d.bucket,"lane":d.lane,"passive":d.passive});
        if let Some(p) = d.items.last() {
            b["after_at"] = json!(p.at);
            b["after_id"] = json!(p.id);
        }
        if self.base.starts_with("synthetic://") {
            let rows = self
                .parallel
                .data
                .as_ref()
                .map(|w| {
                    w.nearby
                        .iter()
                        .chain(
                            w.passages
                                .iter()
                                .filter(|p| !w.nearby.iter().any(|q| q.id == p.id)),
                        )
                        .filter(|p| {
                            p.lane() == d.lane
                                && (d.bucket.from..d.bucket.until).contains(&p.at.max(d.range.from))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let d = self.parallel.detail.as_mut().unwrap();
            d.items = rows;
            d.more = false;
            return;
        }
        if self.send("/transcript_passages", Some(b.clone())) {
            self.parallel.detail.as_mut().unwrap().request = Some(b);
        }
    }
    fn detail_window(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.parallel.detail else {
            return;
        };
        let (lane, range, bucket) = (d.lane, d.range, d.bucket);
        let items = d.items.clone();
        let more = d.more;
        let busy = d.request.is_some();
        let mut open = true;
        egui::Window::new(format!(
            "{} · {}",
            lane.label(),
            clock(bucket.from, "%H:%M:%S")
        ))
        .id(egui::Id::new("parallel-detail"))
        .open(&mut open)
        .default_width(510.)
        .max_height(480.)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Zoom here").clicked() {
                    self.parallel.set_range(bucket);
                }
                if ui.button("Close").clicked() {
                    self.parallel.detail = None;
                }
            });
            egui::ScrollArea::vertical()
                .id_salt(("parallel-full", bucket.from, lane as u8))
                .show(ui, |ui| {
                    let mut hits = vec![];
                    for p in &items {
                        let color = design::paper(self.reading.night).ink;
                        let text =
                            self.passage_layout(ui, p, 16., ui.available_width() - 34., color);
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), text.size().y.max(24.) + 8.),
                            egui::Sense::hover(),
                        );
                        self.paint_passage(ui, rect, text, p, range, &mut hits);
                        if p.truncated && ui.small_button("Read full passage").clicked() {
                            self.inspect("observation", p.id, None);
                            self.parallel.detail = None;
                        }
                    }
                    self.word_drag(ui, range, &hits);
                    if busy {
                        ui.spinner();
                    } else if more && ui.button("More passages").clicked() {
                        self.next_detail();
                    }
                });
        });
        if !open {
            self.parallel.detail = None;
        }
    }
    fn parallel_tools(&mut self, ui: &mut egui::Ui) {
        let style = self.reading;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Alongside microphone").size(13.));
            ui.selectable_value(&mut self.reading.passive_screen, false, "System audio");
            ui.selectable_value(
                &mut self.reading.passive_screen,
                true,
                "Passive screen text",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(if self.focus { "Exit focus" } else { "Focus" })
                    .clicked()
                {
                    self.focus = !self.focus;
                }
                ui.menu_button("Reading", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Text size");
                        ui.add(
                            egui::DragValue::new(&mut self.reading.size)
                                .range(16..=33)
                                .suffix(" pt"),
                        );
                    });
                    ui.checkbox(&mut self.reading.journal, "Journal serif");
                    ui.checkbox(&mut self.reading.night, "Night paper");
                    if ui.button("Paragraph view").clicked() {
                        self.paragraph_view = true;
                        self.follow_stream();
                        ui.close();
                    }
                });
                ui.menu_button("Inputs", |ui| {
                    ui.set_min_width(360.);
                    self.input_strip(ui, self.fresh());
                });
                if ui.small_button("Quick note").clicked() {
                    self.open_note();
                }
            });
        });
        if style != self.reading {
            if style.passive_screen != self.reading.passive_screen {
                self.parallel.detail = None;
                if self.status["active"] != "true"
                    && self.parallel.fixed.is_none()
                    && let Some(w) = &self.parallel.data
                    && (!w.passages.is_empty() || !w.clusters.is_empty())
                {
                    self.parallel.fixed = Some(w.range);
                }
            }
            if !self.base.starts_with("synthetic://") {
                self.send("/reading", Some(json!(self.reading)));
            }
        }
    }
    fn parallel_range(&mut self, ui: &mut egui::Ui, w: &Window) {
        let r = self.parallel.fixed.unwrap_or(w.range);
        ui.horizontal_wrapped(|ui| {
            let axis = self.parallel.ruler.unwrap_or(w.overview);
            let fmt = if clock(r.from, "%F") == clock(r.until, "%F") { "%H:%M:%S" } else { "%F %H:%M:%S" };
            for (i, start) in [true, false].into_iter().enumerate() {
                let label = if start { "From" } else { "Until" };
                ui.label(label);
                let at = if start { r.from } else { r.until };
                let mut value = at;
                let bounds = if start { MIN_TIME..=r.until - 1 } else { r.from + 1..=MAX_TIME };
                let response = ui.push_id(("parallel-clock", start), |ui| {
                    ui.add(egui::DragValue::new(&mut value)
                        .range(bounds)
                        .speed((axis.len() as f64 / 900.).clamp(0.25, 3600.))
                        .update_while_editing(false)
                        .custom_formatter(|n, _| clock(n.round() as i64, fmt))
                        .custom_parser(|s| time_input(s, at).map(|t| t as f64)))
                }).inner.on_hover_text("Drag left or right to change the time. Hold Shift for finer steps. Click to type.");
                self.parallel.time_fields[i] = Some(response.id);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, label));
                if response.drag_started() || response.clicked() || response.has_focus() {
                    self.parallel.pin(r);
                    self.parallel.ruler.get_or_insert(axis);
                }
                if response.changed() {
                    self.parallel.set_range(if start { Span { from: value, ..r } } else { Span { until: value, ..r } });
                }
            }
            ui.menu_button("Duration", |ui| {
                for (label, seconds) in [
                    ("30 sec", 30), ("1 min", 60), ("5 min", 300), ("10 min", 600),
                    ("Twice as long", r.len().saturating_mul(2)), ("Half as long", (r.len() / 2).max(1)),
                ] {
                    if ui.selectable_label(r.len() == seconds, label).clicked() {
                        self.parallel.reveal(Span::ending(r.until, seconds));
                        ui.close();
                    }
                }
            });
            for (label, end, title) in [("‹", r.from, "Previous selection of this duration"), ("›", r.until + r.len(), "Next selection of this duration")] {
                if ui.small_button(label).on_hover_text(title).clicked() {
                    self.parallel.reveal(Span::ending(end, r.len()));
                }
            }
            if ui
                .small_button(if self.parallel.fixed.is_some() {
                    "Latest"
                } else {
                    "Following latest"
                })
                .on_hover_text("Follow the latest activity")
                .clicked()
            {
                self.parallel.fixed = None;
                self.parallel.ruler = None;
                self.parallel.detail = None;
                self.parallel.clear_picks();
                self.parallel.checked = None;
            }
        });
    }
    pub(super) fn parallel_transcript(&mut self, ui: &mut egui::Ui) {
        self.parallel_tools(ui);
        let Some(source) = self
            .parallel
            .overview_drag
            .as_ref()
            .map(|d| d.domain.clone())
            .or_else(|| self.parallel.data.clone())
        else {
            self.ask_window();
            ui.spinner();
            ui.label("Opening your transcript…");
            return;
        };
        self.overview_ruler(ui, &source);
        self.parallel_range(ui, &source);
        self.ask_window();
        let w = self
            .parallel
            .display(self.reading.passive_screen)
            .expect("source is loaded");
        let current = self.base.starts_with("synthetic://")
            || self.parallel.loaded == self.parallel.query(self.reading.passive_screen);
        // A copy always queries the exact visible bounds, even while the canvas is drawing from the nearby cache.
        let paper = design::paper(self.reading.night);
        egui::Frame::new()
            .fill(paper.sheet)
            .stroke(egui::Stroke::new(1., paper.line))
            .corner_radius(10)
            .inner_margin(10)
            .show(ui, |ui| self.draw_columns(ui, &w));
        let has = !w.passages.is_empty() || !w.clusters.is_empty();
        ui.horizontal_wrapped(|ui| {
            let picked = self.has_visible_picks(&w);
            let copy = egui::Button::new(RichText::new("Copy conversation").color(CARD)).fill(MINT);
            if ui
                .add_enabled((has || !current) && !self.parallel.copying, copy)
                .clicked()
            {
                self.copy_parallel(w.range, None, false);
            }
            if picked && ui.button("Copy selected").clicked() {
                self.copy_parallel(w.range, None, true);
            }
            let preview = egui::Button::new("Preview copy").frame(false);
            if ui
                .add_enabled(has && !self.parallel.copying, preview)
                .clicked()
            {
                self.parallel.preview_copy = true;
                self.copy_parallel(w.range, None, picked);
            }
            let editing_time = ui.memory(|m| {
                self.parallel
                    .time_fields
                    .iter()
                    .flatten()
                    .any(|id| m.has_focus(*id))
            });
            if picked
                && !editing_time
                && !self.note_open
                && self.detail_request.is_null()
                && self.parallel.copy_text.is_none()
                && !self.parallel.copying
                && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
            {
                self.copy_parallel(w.range, None, true);
            }
            if picked && ui.small_button("Clear selection").clicked() {
                self.parallel.clear_picks();
            }
            if !self.parallel.receipt.is_empty() {
                muted(ui, self.parallel.receipt.clone());
            }
            if ui.link("Screen text history").clicked() {
                self.navigate(5);
            }
            if ui.link("Browse archive →").clicked() {
                self.navigate(1);
            }
        });
        let recording_issue = self.status["active"] == "true"
            || self.status["armed"] == "true"
            || self.status["capture_starting"] == "true"
            || self.status["status"]
                .as_str()
                .is_some_and(|s| s.starts_with("error:"));
        let blocker = self.status["health"]["speech_blocker"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.status["health"]["loaded_speech_model"]["error"]
                    .as_str()
                    .filter(|s| !s.is_empty())
            });
        if let Some(issue) = blocker.filter(|_| recording_issue || !has) {
            ui.colored_label(paper.fault, issue);
        } else if let Some(at) = w.quiet_at {
            muted(
                ui,
                format!(
                    "Audio received at {} · No speech transcribed there yet.",
                    clock(at, "%H:%M:%S")
                ),
            );
        } else if has {
            muted(
                ui,
                "Click a passage to select it, or drag across words. A column heading selects the whole column.",
            );
        }
        self.detail_window(ui.ctx());
        if let Some(text) = &self.parallel.copy_text {
            let mut open = true;
            egui::Window::new("Copy preview")
                .default_width(550.)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    if ui.button("Copy to clipboard").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                    egui::ScrollArea::vertical()
                        .max_height(370.)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(text).selectable(true));
                        });
                });
            if !open {
                self.parallel.copy_text = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Controls, egui::Context, Receiver<Request>) {
        let (tx, requests) = std::sync::mpsc::sync_channel(32);
        let (_, rx) = std::sync::mpsc::sync_channel(32);
        let c = crate::preview::fixture(tx, rx);
        let ctx = egui::Context::default();
        design::setup(&ctx);
        (c, ctx, requests)
    }
    fn frame(
        ctx: &egui::Context,
        c: &mut Controls,
        events: Vec<egui::Event>,
    ) -> Vec<(String, f32)> {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1040., 790.),
                )),
                events,
                ..Default::default()
            },
            |ui| c.parallel_transcript(ui),
        );
        let mut labels = vec![];
        for s in &out.shapes {
            if let egui::Shape::Text(t) = &s.shape {
                if !s
                    .clip_rect
                    .intersects(t.galley.rect.translate(t.pos.to_vec2()))
                {
                    continue;
                }
                for section in &t.galley.job.sections {
                    labels.push((
                        t.galley.job.text[section.byte_range.start.0..section.byte_range.end.0]
                            .to_owned(),
                        section.format.font_id.size,
                    ));
                }
            }
        }
        out.drop_without_applying_deltas();
        labels
    }
    fn press(p: egui::Pos2, down: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: down,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn workspace_frame(ctx: &egui::Context, c: &mut Controls, events: Vec<egui::Event>) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(950., 690.),
                )),
                events,
                ..Default::default()
            },
            |ui| c.workspace(ui),
        )
        .drop_without_applying_deltas();
    }
    #[test]
    fn workspace_ruler_keeps_the_same_instant_under_a_trackpad_zoom() {
        let (mut c, ctx, _) = setup();
        for _ in 0..3 {
            workspace_frame(&ctx, &mut c, vec![]);
        }
        let rect = ctx
            .read_response(egui::Id::new("parallel-time-ruler"))
            .unwrap()
            .rect;
        let w = c.parallel.data.as_ref().unwrap().clone();
        let anchor = w.overview.from as f64 + w.overview.len() as f64 * 0.5;
        for _ in 0..60 {
            workspace_frame(
                &ctx,
                &mut c,
                vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0., 3.),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            let axis = c.parallel.ruler.unwrap();
            let under_cursor = axis.from as f64 + axis.len() as f64 * 0.5;
            assert!(
                (under_cursor - anchor).abs() <= 1.,
                "stationary cursor drifted by {} seconds",
                under_cursor - anchor
            );
            assert_eq!(c.parallel.fixed, Some(w.range));
        }
        assert!(c.parallel.ruler.unwrap().len() < w.overview.len() * 2 / 3);
    }
    #[test]
    fn workspace_ruler_captures_a_drag_between_two_rendered_frames() {
        let (mut c, ctx, _) = setup();
        for _ in 0..3 {
            workspace_frame(&ctx, &mut c, vec![]);
        }
        let rect = ctx
            .read_response(egui::Id::new("parallel-time-ruler"))
            .unwrap()
            .rect;
        let axis = c.parallel.data.as_ref().unwrap().overview;
        let at = |f| egui::pos2(rect.left() + rect.width() * f, rect.center().y);
        workspace_frame(
            &ctx,
            &mut c,
            vec![
                egui::Event::PointerMoved(at(0.75)),
                press(at(0.75), true),
                egui::Event::PointerMoved(at(0.25)),
                press(at(0.25), false),
            ],
        );
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: axis.from + axis.len() / 4,
                until: axis.from + axis.len() * 3 / 4,
            })
        );
        assert_eq!(c.parallel.ruler, Some(axis));
    }
    #[test]
    fn dragging_repositions_and_loads_the_columns_before_the_mouse_is_released() {
        let (mut c, ctx, _) = setup();
        for _ in 0..3 {
            frame(&ctx, &mut c, vec![]);
        }
        let from = c.parallel.data.as_ref().unwrap().overview.from;
        let rect = ctx
            .read_response(egui::Id::new("parallel-time-ruler"))
            .unwrap()
            .rect;
        let point = |t: f32| egui::pos2(rect.left() + rect.width() * (t / 600.), rect.center().y);
        frame(
            &ctx,
            &mut c,
            vec![
                egui::Event::PointerMoved(point(70.)),
                press(point(70.), true),
            ],
        );
        let first = frame(&ctx, &mut c, vec![egui::Event::PointerMoved(point(170.))]);
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: from + 70,
                until: from + 170
            })
        );
        assert!(c.parallel.overview_drag.is_some());
        assert!(
            first.iter().any(|(s, _)| s.contains("ferry timetable")),
            "first drag must show neighboring words immediately"
        );
        assert!(first.iter().all(|(s, _)| !s.contains("Did you record")));
        let next = frame(&ctx, &mut c, vec![egui::Event::PointerMoved(point(209.))]);
        assert_eq!(c.parallel.fixed.unwrap().until, from + 209);
        assert!(
            next.iter().any(|(s, _)| s.contains("Market Street")),
            "second move must reveal another passage before mouseup"
        );
        frame(&ctx, &mut c, vec![press(point(209.), false)]);
        assert!(c.parallel.overview_drag.is_none());
        assert_eq!(c.parallel.fixed.unwrap().until, from + 209);
        // A second drag resizes the existing selection using its handle.
        let band = ctx
            .read_response(egui::Id::new("parallel-selection-track"))
            .unwrap()
            .rect;
        let handle = egui::pos2(point(70.).x, band.center().y);
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::PointerMoved(handle), press(handle, true)],
        );
        frame(&ctx, &mut c, vec![egui::Event::PointerMoved(point(100.))]);
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: from + 100,
                until: from + 209
            })
        );
    }
    fn drag(ctx: &egui::Context, c: &mut Controls, begin: egui::Pos2, end: egui::Pos2) {
        frame(
            ctx,
            c,
            vec![egui::Event::PointerMoved(begin), press(begin, true)],
        );
        frame(ctx, c, vec![egui::Event::PointerMoved(end)]);
        frame(ctx, c, vec![press(end, false)]);
    }
    fn wheel(ctx: &egui::Context, c: &mut Controls, pos: egui::Pos2, amount: f32) {
        frame(
            ctx,
            c,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0., amount),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    #[test]
    fn selection_band_moves_when_it_fills_the_view_or_is_only_one_second_wide() {
        let (mut c, ctx, _) = setup();
        let axis = c.parallel.data.as_ref().unwrap().overview;
        c.parallel.ruler = Some(axis);
        for original in [axis, Span::ending(axis.from + axis.len() / 2 + 1, 1)] {
            c.parallel.set_range(original);
            for _ in 0..2 {
                workspace_frame(&ctx, &mut c, vec![]);
            }
            let track = ctx
                .read_response(egui::Id::new("parallel-selection-track"))
                .unwrap()
                .rect;
            let from = track.center();
            let to = from + egui::vec2(track.width() / 4., 0.);
            workspace_frame(
                &ctx,
                &mut c,
                vec![
                    egui::Event::PointerMoved(from),
                    press(from, true),
                    egui::Event::PointerMoved(to),
                    press(to, false),
                ],
            );
            assert_eq!(
                c.parallel.fixed,
                Some(Span::ending(
                    original.until + axis.len() / 4,
                    original.len()
                ))
            );
            assert_eq!(c.parallel.ruler, Some(axis));
        }
    }
    #[test]
    fn moving_the_selection_band_preserves_duration_and_the_ruler_survives_release_and_replies() {
        let (mut c, ctx, _) = setup();
        for _ in 0..3 {
            frame(&ctx, &mut c, vec![]);
        }
        let domain = c.parallel.data.as_ref().unwrap().clone();
        let axis = domain.overview;
        let rect = ctx
            .read_response(egui::Id::new("parallel-time-ruler"))
            .unwrap()
            .rect;
        let point = |seconds: i64| {
            egui::pos2(
                rect.left() + rect.width() * seconds as f32 / axis.len() as f32,
                rect.center().y,
            )
        };
        let band = ctx
            .read_response(egui::Id::new("parallel-selection-track"))
            .unwrap()
            .rect;
        let on_band = |t| egui::pos2(point(t).x, band.center().y);
        drag(&ctx, &mut c, on_band(30), on_band(49));
        let moved = c.parallel.fixed.unwrap();
        assert_eq!((moved.from, moved.len()), (axis.from + 19, 60));
        // Grab the handle three pixels off its center. Only the dragged endpoint changes.
        let offset = egui::vec2(3., 0.);
        drag(&ctx, &mut c, on_band(19) + offset, on_band(29) + offset);
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: axis.from + 29,
                until: moved.until
            })
        );
        drag(&ctx, &mut c, point(410), point(270));
        let selection = Span {
            from: axis.from + 270,
            until: axis.from + 410,
        };
        assert_eq!(c.parallel.fixed, Some(selection));
        let request = c.parallel.query(false);
        assert_eq!(request["overview"], json!(axis));
        let mut reply = (*domain).clone();
        reply.range = selection;
        c.parallel_reply(Reply {
            path: "/transcript_window".into(),
            body: Some(request),
            result: Ok(json!(reply)),
        });
        frame(&ctx, &mut c, vec![]);
        assert_eq!(c.parallel.ruler, Some(axis));
        assert_eq!(c.parallel.fixed, Some(selection));
    }
    #[test]
    fn wheel_zooms_the_hovered_surface_around_the_cursor() {
        let (mut c, ctx, _) = setup();
        for _ in 0..3 {
            frame(&ctx, &mut c, vec![]);
        }
        let w = c.parallel.data.as_ref().unwrap().clone();
        let rect = ctx
            .read_response(egui::Id::new("parallel-time-ruler"))
            .unwrap()
            .rect;
        let p = egui::pos2(rect.left() + rect.width() * 0.25, rect.center().y);
        wheel(&ctx, &mut c, p, 3.);
        let zoomed = c.parallel.ruler.unwrap();
        assert!(zoomed.len() < w.overview.len());
        assert!(
            (zoomed.from + zoomed.len() / 4 - w.overview.from - w.overview.len() / 4).abs() <= 1
        );
        assert_eq!(c.parallel.fixed, Some(w.range));
        wheel(&ctx, &mut c, p, -3.);
        assert!((c.parallel.ruler.unwrap().len() - w.overview.len()).abs() <= 1);
        wheel(&ctx, &mut c, p + egui::vec2(0., 180.), 3.);
        assert!(c.parallel.fixed.unwrap().len() < w.range.len());
    }
    // Start with a ten-minute selection, then use the real wheel gesture to bring
    // both endpoints outside the visible ruler, as in a zoomed-in recording.
    fn zoom_inside_selection() -> (Controls, egui::Context, egui::Rect, Span, Span) {
        let (mut c, ctx, _) = setup();
        let whole = c.parallel.data.as_ref().unwrap().overview;
        c.parallel.set_range(whole);
        for _ in 0..3 {
            frame(&ctx, &mut c, vec![]);
        }
        let rect = ctx
            .read_response(egui::Id::new("parallel-time-ruler"))
            .unwrap()
            .rect;
        wheel(&ctx, &mut c, rect.center(), 6.);
        let axis = c.parallel.ruler.unwrap();
        assert!(whole.from < axis.from && axis.until < whole.until);
        (c, ctx, rect, whole, axis)
    }
    #[test]
    fn zoomed_ruler_can_draw_a_subsection_while_both_ends_are_out_of_view() {
        let (mut c, ctx, rect, _, axis) = zoom_inside_selection();
        let point = |t: i64| {
            egui::pos2(
                rect.left() + rect.width() * (t - axis.from) as f32 / axis.len() as f32,
                rect.center().y,
            )
        };
        let a = axis.from + axis.len() / 4;
        let b = axis.until - axis.len() / 4;
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::PointerMoved(point(b)), press(point(b), true)],
        );
        frame(&ctx, &mut c, vec![egui::Event::PointerMoved(point(a))]);
        assert_eq!(
            c.parallel.fixed,
            Some(Span { from: a, until: b }),
            "a plain reverse drag selects inside the previous selection before release"
        );
        frame(&ctx, &mut c, vec![press(point(a), false)]);
        assert_eq!(c.parallel.ruler, Some(axis));
        // Repeat inside a now fully visible selection and leave the axis stationary.
        drag(&ctx, &mut c, point(a + 10), point(b - 10));
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: a + 10,
                until: b - 10
            })
        );
        frame(
            &ctx,
            &mut c,
            vec![
                egui::Event::PointerMoved(point(a + 20)),
                press(point(a + 20), true),
            ],
        );
        frame(&ctx, &mut c, vec![press(point(a + 20), false)]);
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: a + 10,
                until: b - 10
            }),
            "a click inside keeps the range"
        );
        assert_eq!(c.parallel.ruler, Some(axis));
    }
    #[test]
    fn zoomed_ruler_border_handles_bring_each_endpoint_to_the_pointer() {
        let (mut c, ctx, _, whole, axis) = zoom_inside_selection();
        let rect = ctx
            .read_response(egui::Id::new("parallel-selection-track"))
            .unwrap()
            .rect;
        let left = egui::pos2(rect.left() + 4., rect.center().y);
        let right = egui::pos2(rect.right() - 4., rect.center().y);
        let t1 = axis.from + axis.len() / 4;
        let t2 = axis.until - axis.len() / 4;
        let point = |t| {
            egui::pos2(
                rect.left() + rect.width() * (t - axis.from) as f32 / axis.len() as f32,
                rect.center().y,
            )
        };
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::PointerMoved(left), press(left, true)],
        );
        frame(&ctx, &mut c, vec![press(left, false)]);
        assert_eq!(
            c.parallel.fixed,
            Some(whole),
            "a click on a clipped handle leaves the actual time intact"
        );
        drag(&ctx, &mut c, left, point(t1));
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: t1,
                until: whole.until
            })
        );
        drag(&ctx, &mut c, right, point(t2));
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: t1,
                until: t2
            })
        );
        assert_eq!(c.parallel.ruler, Some(axis));
    }
    #[test]
    fn a_selection_entirely_outside_the_ruler_can_be_revealed_or_resized() {
        let (mut c, ctx, _, _, axis) = zoom_inside_selection();
        let rect = ctx
            .read_response(egui::Id::new("parallel-selection-track"))
            .unwrap()
            .rect;
        let before = Span {
            from: axis.from - 60,
            until: axis.from - 30,
        };
        c.parallel.set_range(before);
        let left = egui::pos2(rect.left() + 4., rect.center().y);
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::PointerMoved(left), press(left, true)],
        );
        frame(&ctx, &mut c, vec![press(left, false)]);
        let revealed = c.parallel.ruler.unwrap();
        assert!(revealed.from <= before.from && before.until <= revealed.until);
        assert_eq!(c.parallel.fixed, Some(before));
        assert_eq!(revealed.len(), axis.len());

        let after = Span {
            from: axis.until + 30,
            until: axis.until + 60,
        };
        c.parallel.ruler = Some(axis);
        c.parallel.set_range(after);
        let right = egui::pos2(rect.right() - 4., rect.center().y);
        drag(&ctx, &mut c, right, rect.center());
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: axis.from + (axis.len() + 1) / 2,
                until: after.until,
            })
        );
        assert_eq!(c.parallel.ruler, Some(axis));
    }
    #[test]
    fn both_clock_fields_scrub_live_and_still_accept_a_typed_time() {
        let (mut c, ctx, _) = setup();
        for _ in 0..3 {
            frame(&ctx, &mut c, vec![]);
        }
        let initial = c.parallel.data.as_ref().unwrap().range;
        let from = ctx
            .read_response(c.parallel.time_fields[0].unwrap())
            .unwrap()
            .rect
            .center();
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::PointerMoved(from), press(from, true)],
        );
        let right = from + egui::vec2(30., 0.);
        frame(&ctx, &mut c, vec![egui::Event::PointerMoved(right)]);
        let moved = c.parallel.fixed.unwrap();
        assert!(moved.from > initial.from);
        assert_eq!(moved.until, initial.until);
        frame(&ctx, &mut c, vec![press(right, false)]);
        let until = ctx
            .read_response(c.parallel.time_fields[1].unwrap())
            .unwrap()
            .rect
            .center();
        drag(&ctx, &mut c, until, until - egui::vec2(15., 0.));
        let shortened = c.parallel.fixed.unwrap();
        assert!(shortened.until < moved.until);
        assert_eq!(shortened.from, moved.from);
        let until = ctx
            .read_response(c.parallel.time_fields[1].unwrap())
            .unwrap()
            .rect
            .center();
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::PointerMoved(until), press(until, true)],
        );
        frame(&ctx, &mut c, vec![press(until, false)]);
        frame(&ctx, &mut c, vec![]);
        let typed = clock(initial.until + 20, "%H:%M:%S");
        frame(&ctx, &mut c, vec![egui::Event::Text(typed)]);
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(&ctx, &mut c, vec![]);
        assert_eq!(
            c.parallel.fixed,
            Some(Span {
                from: moved.from,
                until: initial.until + 20
            })
        );
    }
    #[test]
    fn folded_original_words_stay_readable_and_open_at_their_time() {
        let (mut c, ctx, _) = setup();
        let overview = c.parallel.data.as_ref().unwrap().overview;
        c.parallel.set_range(overview);
        let mut labels = vec![];
        for _ in 0..3 {
            labels = frame(&ctx, &mut c, vec![]);
        }
        assert!(
            labels.iter().any(|(s, size)| s == "$125" && *size >= 14.),
            "a literal price should stay readable at 10 minutes"
        );
        assert!(
            labels
                .iter()
                .any(|(s, size)| s.contains("Ferry tickets") && *size < 10.)
        );
        let range = Span {
            from: overview.from + 156,
            until: overview.from + 157,
        };
        let row = ctx
            .read_response(egui::Id::new((
                "parallel-unfold",
                Lane::Saves as u8,
                range.from,
            )))
            .unwrap()
            .rect;
        let p = row.left_center() + egui::vec2(22., 0.);
        frame(
            &ctx,
            &mut c,
            vec![egui::Event::PointerMoved(p), press(p, true)],
        );
        frame(&ctx, &mut c, vec![press(p, false)]);
        assert_eq!(c.parallel.detail.as_ref().unwrap().bucket, range);
        assert_eq!(
            c.parallel.detail.as_ref().unwrap().items[0].body(),
            "Ferry tickets · $125"
        );
    }
    #[test]
    fn late_window_responses_never_change_the_final_range_or_copy_request() {
        let (mut c, _ctx, requests) = setup();
        c.base = "test://local-api".into();
        c.ask_window();
        let old = requests.try_recv().unwrap();
        let window = c.parallel.data.as_ref().unwrap().clone();
        let selected = Span {
            from: window.range.from + 9,
            until: window.range.from + 22,
        };
        c.parallel.set_range(selected);
        c.parallel_reply(Reply {
            path: old.path,
            body: old.body,
            result: Ok(json!(&*window)),
        });
        c.ask_window();
        let fresh = requests.try_recv().unwrap();
        assert_eq!(fresh.body.unwrap()["range"], json!(selected));
        c.copy_parallel(selected, Some(Lane::Saves), false);
        let copy = requests.try_recv().unwrap();
        assert_eq!(copy.path, "/copy_transcript");
        assert_eq!(copy.body.unwrap()["range"], json!(selected));
    }
}
