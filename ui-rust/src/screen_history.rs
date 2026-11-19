//! A searchable history of automatic screen readings, independent of audio or saved pictures.
use super::*;
use crate::parallel::clock;
use serde::Deserialize;

#[derive(Deserialize)]
struct Visit {
    id: i64,
    at: i64,
    end: i64,
    text: String,
    truncated: bool,
}
#[derive(Deserialize, Default)]
struct Page {
    items: Vec<Visit>,
    next: Option<[i64; 2]>,
}
#[derive(Default)]
pub(super) struct History {
    pub captures: crate::screen_captures::Browser,
    pub visits: bool,
    query: String,
    cursors: Vec<[i64; 2]>,
    page: Page,
    loaded: Option<Value>,
    pending: Option<Value>,
    edited: Option<Instant>,
    pub checked: Option<Instant>,
    error: Option<String>,
}
impl History {
    pub fn ready(&self) -> bool {
        if self.visits {
            self.loaded.is_some() && self.error.is_none()
        } else {
            self.captures.ready()
        }
    }
    fn request(&self) -> Value {
        json!({"query":self.query.trim(),"before":self.cursors.last()})
    }
    pub fn reply(&mut self, r: Reply) {
        self.pending = None;
        if r.body.as_ref() != Some(&self.request()) {
            return;
        }
        self.loaded = r.body;
        match r
            .result
            .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        {
            Ok(page) => {
                self.page = page;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}
impl Controls {
    pub(super) fn screen_text_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.screen_history.visits, false, "Captures");
            ui.selectable_value(&mut self.screen_history.visits, true, "Line history");
        });
        if !self.screen_history.visits {
            self.capture_browser(ui);
            return;
        }
        let h = &mut self.screen_history;
        ui.horizontal_wrapped(|ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut h.query)
                    .hint_text("Search captured screen text")
                    .desired_width(300.)
                    .char_limit(200),
            );
            if search.changed() {
                h.cursors.clear();
                h.edited = Some(Instant::now());
            }
            if ui.small_button("Refresh").clicked() {
                h.checked = None;
            }
        });
        let key = h.request();
        let current = h.loaded.as_ref() == Some(&key);
        let live = self.status["active"] == "true" && h.cursors.is_empty();
        let due = h
            .checked
            .is_none_or(|t| live && t.elapsed() >= Duration::from_secs(5));
        let typing = h
            .edited
            .is_some_and(|t| t.elapsed() < Duration::from_millis(170));
        if typing {
            ui.ctx().request_repaint_after(Duration::from_millis(175));
        }
        if self.screen_history.pending.is_none()
            && !typing
            && (!current || due)
            && self.send("/screen_text", Some(key.clone()))
        {
            self.screen_history.checked = Some(Instant::now());
            self.screen_history.pending = Some(key);
        }
        let h = &mut self.screen_history;
        if !current {
            ui.spinner();
            return;
        }
        if let Some(error) = &h.error {
            ui.colored_label(CORAL, error);
            return;
        }
        if h.page.items.is_empty() {
            ui.add_space(12.);
            if !h.query.trim().is_empty() {
                ui.label("No captured screen text matches this search.");
            } else {
                ui.heading("No automatic screen text yet");
                if self.saved_config["screen_mode"] == "off" {
                    muted(ui, "Automatic screen checks are off.");
                } else {
                    let interval = self.saved_config["screen_seconds"].as_u64().unwrap_or(300);
                    muted(
                        ui,
                        format!(
                            "While recording, Daybook checks the screen every {interval} seconds and keeps the words it can read."
                        ),
                    );
                }
                if ui.link("Screen capture settings").clicked() {
                    self.settings_tab = 1;
                    self.navigate(4);
                }
            }
            return;
        }
        ui.add_space(6.);
        muted(
            ui,
            "Each line shows when it first appeared and when Daybook last saw it.",
        );
        let mut open = None;
        if !h.cursors.is_empty() || h.page.next.is_some() {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!h.cursors.is_empty(), egui::Button::new("‹ Newer"))
                    .clicked()
                {
                    h.cursors.pop();
                }
                if let Some(next) = h.page.next
                    && ui.button("Older ›").clicked()
                {
                    h.cursors.push(next);
                }
            });
        }
        egui::ScrollArea::vertical()
            .id_salt(("screen-visits", &h.query, h.cursors.last()))
            .show(ui, |ui| {
                let mut date = None;
                let mut previous_span = None;
                for visit in &h.page.items {
                    let day = clock(visit.at, "%A, %B %-d, %Y");
                    if date.as_ref() != Some(&day) {
                        ui.add_space(12.);
                        ui.heading(&day);
                        date = Some(day);
                    }
                    if previous_span != Some((visit.at, visit.end)) {
                        previous_span = Some((visit.at, visit.end));
                        ui.add_space(7.);
                        let mut when = clock(visit.at, "%H:%M:%S");
                        if visit.end > visit.at {
                            let fmt = if clock(visit.end, "%F") == clock(visit.at, "%F") {
                                "%H:%M:%S"
                            } else {
                                "%b %-d, %H:%M:%S"
                            };
                            when.push_str(" – ");
                            when.push_str(&clock(visit.end, fmt));
                        }
                        ui.label(RichText::new(when).size(12.).color(MUTED));
                    }
                    ui.add(egui::Label::new(&visit.text).selectable(true).wrap());
                    if visit.truncated && ui.small_button("Read full text").clicked() {
                        open = Some(visit.id);
                    }
                }
            });
        if let Some(id) = open {
            self.inspect("observation", id, None);
        }
    }
}
