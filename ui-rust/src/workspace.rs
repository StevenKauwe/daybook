//! Recorder controls stay within reach while a full-height rail keeps the page stable.
use super::*;
use design::{INK, Icon, LINE};

const PAGES: [(Icon, &str); 5] = [
    (Icon::Live, "Transcript"),
    (Icon::Archive, "Archive"),
    (Icon::Brief, "Daily brief"),
    (Icon::Health, "Diagnostics"),
    (Icon::Settings, "Settings"),
];

impl Controls {
    pub(super) fn fresh(&self) -> bool {
        self.last_seen
            .is_some_and(|at| at.elapsed() < Duration::from_secs(7))
            && self.status["health"]["at"]
                .as_i64()
                .is_some_and(|at| (chrono::Utc::now().timestamp() - at).abs() < 7)
    }
    pub(super) fn navigate(&mut self, page: usize) {
        if self.tab == page {
            return;
        }
        self.tab = page;
        self.detailed_poll
            .store(page >= 1, std::sync::atomic::Ordering::Relaxed);
        if page >= 3 {
            self.send("/overview", None);
        }
        self.focus = false;
        if matches!(page, 1 | 2) {
            self.refresh(false);
        }
    }
    pub(super) fn open_note(&mut self) {
        self.note_open = true;
        self.note_focus = true;
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        for (i, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5]
            .into_iter()
            .enumerate()
        {
            if ctx.input_mut(|s| s.consume_key(Modifiers::COMMAND, key)) {
                self.navigate(i);
            }
        }
        if ctx.input_mut(|s| s.consume_key(Modifiers::COMMAND, Key::N)) {
            self.open_note();
        }
        if ctx.input_mut(|s| s.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::F)) {
            self.navigate(0);
            self.focus = !self.focus;
        }
        if (self.note_open || !self.detail_request.is_null() || self.focus)
            && !egui::Popup::is_any_open(ctx)
            && ctx.input_mut(|s| s.consume_key(Modifiers::NONE, Key::Escape))
        {
            if self.note_open {
                self.note_open = false;
            } else if !self.detail_request.is_null() {
                self.close_entry();
            } else {
                self.focus = false;
            }
        }
    }
    pub(super) fn header(&mut self, ui: &mut egui::Ui) {
        let colors = design::paper(self.reading.night && self.tab == 0);
        let fresh = self.fresh();
        let (state, kind) = if !self.loaded && self.last_seen.is_none() && self.error.is_empty() {
            ("Connecting…", MUTED)
        } else {
            status_title(&self.status, fresh)
        };
        let color = if kind == CORAL {
            colors.fault
        } else if kind == MINT {
            colors.mine
        } else {
            colors.muted
        };
        let active = self.status["active"] == "true";
        let stop =
            active || self.status["armed"] == "true" || self.status["capture_starting"] == "true";
        let date = if self.tab == 0 {
            self.stream_page["day"].as_str()
        } else if matches!(self.tab, 1 | 2) {
            Some(self.day.as_str())
        } else {
            None
        };
        let date = date
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .unwrap_or_else(|| chrono::Local::now().date_naive());
        let mut date = date.format("%A, %B %-d").to_string();
        if self.status["synthetic"] == true {
            date.push_str("   ·   Sample session");
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.;
                ui.label(RichText::new(date).size(13.).color(colors.muted));
                ui.label(
                    RichText::new(PAGES[self.tab].1)
                        .font(design::serif(43.))
                        .color(colors.ink),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new("•••").strong(), |ui| {
                    if ui.button("Update & Restart…").clicked() {
                        self.navigate(4);
                        self.settings_tab = 3;
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            fresh && !self.pending_action,
                            egui::Button::new("Enable workday schedule"),
                        )
                        .clicked()
                    {
                        self.command("arm");
                        ui.close();
                    }
                    if ui
                        .button("Close controls window")
                        .on_hover_text("Recording can continue with the window closed")
                        .clicked()
                    {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                    if ui.button("Quit Daybook").clicked() {
                        self.quit();
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Application menu");
                let label = if stop {
                    "Stop recording"
                } else {
                    "Start recording"
                };
                let fill = if stop {
                    Color32::from_rgb(129, 53, 43)
                } else {
                    MINT
                };
                let button = egui::Button::new(RichText::new(label).strong().color(CARD))
                    .fill(fill)
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(10)
                    .min_size(egui::vec2(126., 39.));
                if ui
                    .add_enabled(
                        fresh && !self.pending_action && self.status["update"]["busy"] != true,
                        button,
                    )
                    .on_hover_text(if stop {
                        "Stop and stay off until you deliberately enable recording again."
                    } else {
                        "Start recording with the inputs configured in Settings."
                    })
                    .clicked()
                {
                    self.command(if stop { "stop" } else { "start" });
                }
                if self.pending_action {
                    ui.spinner();
                }
                ui.label(RichText::new(state).size(13.).strong().color(color));
                let (slot, _) =
                    ui.allocate_exact_size(egui::Vec2::splat(12.), egui::Sense::hover());
                if active && fresh {
                    ui.painter().circle_filled(
                        slot.center(),
                        5.5,
                        Color32::from_rgb(238, 209, 198),
                    );
                    ui.painter().circle_filled(slot.center(), 3.2, CORAL);
                } else {
                    ui.painter()
                        .circle_stroke(slot.center(), 3.6, egui::Stroke::new(1.4, color));
                }
            });
        });
        if !self.error.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(colors.fault, &self.error);
                if ui.small_button("Dismiss").clicked() {
                    self.error.clear();
                }
            });
        }
        let warning = self.status["capture_warning"].as_str().unwrap_or("");
        let state = self.status["status"].as_str().unwrap_or("");
        if !warning.is_empty()
            || (!active && (state.starts_with("error:") || state.contains("capacity")))
        {
            ui.horizontal_wrapped(|ui| {
                if !warning.is_empty() {
                    ui.colored_label(colors.fault, warning);
                }
                if !active && (state.starts_with("error:") || state.contains("capacity")) {
                    ui.colored_label(colors.fault, state);
                }
                if ui.link("Open diagnostics").clicked() {
                    self.tab = 3;
                    self.focus = false;
                }
            });
        }
    }
    fn rail_entry(&mut self, ui: &mut egui::Ui, index: usize, compact: bool) {
        let (icon, name) = PAGES[index];
        if design::nav(
            ui,
            icon,
            name,
            self.tab == index,
            compact,
            &format!("⌘{}", index + 1),
        )
        .clicked()
        {
            self.navigate(index);
        }
    }
    fn rail(&mut self, ui: &mut egui::Ui, compact: bool) {
        ui.add_space(14.);
        design::brand(ui, compact);
        ui.add_space(32.);
        for index in 0..3 {
            self.rail_entry(ui, index, compact);
        }
        let artwork = !compact && ui.available_height() > 370.;
        let bottom = if compact { 114. } else { 163. };
        ui.add_space((ui.available_height() - bottom - if artwork { 147. } else { 0. }).max(2.));
        if artwork {
            design::imprint(ui);
            ui.add_space(6.);
        }
        self.rail_entry(ui, 3, compact);
        self.rail_entry(ui, 4, compact);
        if !compact {
            ui.add_space(4.);
            let (r, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.), egui::Sense::hover());
            ui.painter().hline(
                r.x_range(),
                r.center().y,
                egui::Stroke::new(1., design::CONSOLE),
            );
            ui.horizontal(|ui| {
                design::icon_slot(ui, Icon::Lock, design::DIM, 13.);
                let text = if !self.loaded {
                    "Connecting…"
                } else if self.saved_config["summary"]["remote_opt_in"] == true
                    || self.saved_config["vision"]["remote_opt_in"] == true
                {
                    "Hosted model enabled"
                } else {
                    "Local processing"
                };
                ui.label(RichText::new(text).size(12.).color(design::DIM));
            });
        }
    }
    fn note_dialog(&mut self, ctx: &egui::Context) {
        if !self.note_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Quick note")
            .id(egui::Id::new("quick_note"))
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_width(440.)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.add_space(3.);
                let response = ui.add(
                    egui::TextEdit::multiline(&mut self.note)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("A decision, a question, a thought…"),
                );
                if std::mem::take(&mut self.note_focus) {
                    response.request_focus();
                }
                if response.changed() {
                    self.note_dirty = Some(Instant::now());
                }
                ui.horizontal(|ui| {
                    muted(ui, "Draft saved locally as you type");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let valid = !self.note.trim().is_empty() && !self.saving_note;
                        if ui
                            .add_enabled(
                                valid,
                                egui::Button::new(RichText::new("Save note").color(CARD))
                                    .fill(MINT),
                            )
                            .on_hover_text("Save note  ⌘↩")
                            .clicked()
                            && self.send("/note", Some(json!({"text":self.note})))
                        {
                            self.saving_note = true;
                        }
                    });
                });
            });
        if !open {
            self.note_open = false;
        }
    }
    pub(super) fn workspace(&mut self, ui: &mut egui::Ui) {
        self.shortcuts(ui.ctx());
        let night = self.tab == 0 && self.reading.night;
        let colors = design::paper(night);
        let theme = if night {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        if ui.ctx().theme() != theme {
            ui.ctx().set_theme(theme);
            ui.ctx().request_repaint();
        }
        let compact = ui.available_width() < 990.;
        if !self.focus {
            egui::Panel::left("binder_spine")
                .exact_size(if compact { 70. } else { 190. })
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(INK)
                        .inner_margin(if compact { 12 } else { 17 }),
                )
                .show(ui, |ui| self.rail(ui, compact));
        }
        let pad = if compact { 19 } else { 33 };
        egui::Panel::top("workspace_header")
            .frame(
                egui::Frame::new()
                    .fill(colors.ground)
                    .inner_margin(egui::Margin {
                        left: pad,
                        right: pad,
                        top: 18,
                        bottom: 8,
                    }),
            )
            .show(ui, |ui| self.header(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(colors.ground)
                    .inner_margin(egui::Margin {
                        left: pad,
                        right: pad,
                        top: 7,
                        bottom: 10,
                    }),
            )
            .show(ui, |ui| {
                if !self.loaded {
                    ui.spinner();
                    muted(ui, "Connecting to Daybook…");
                    return;
                }
                if self.tab == 0 {
                    if !self.focus {
                        self.input_strip(ui, self.fresh());
                        ui.add_space(6.);
                    }
                    self.stream(ui);
                } else {
                    let (divider, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 1.),
                        egui::Sense::hover(),
                    );
                    ui.painter().hline(
                        divider.x_range(),
                        divider.center().y,
                        egui::Stroke::new(1., LINE),
                    );
                    if self.tab == 1 {
                        self.timeline(ui);
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt((
                                "workspace_page",
                                self.tab,
                                self.settings_tab,
                                self.health_tab,
                            ))
                            .show(ui, |ui| match self.tab {
                                2 => self.brief(ui),
                                3 => self.health(ui),
                                _ => self.settings(ui),
                            });
                    }
                }
            });
        if !self.detail_request.is_null() {
            let mut open = true;
            let title = if self.detail_request["kind"] == "observation" {
                "Journal entry"
            } else {
                "Source inspector"
            };
            egui::Window::new(title)
                .id(egui::Id::new("entry_inspector"))
                .open(&mut open)
                .default_width(660.)
                .default_height(520.)
                .vscroll(true)
                .show(ui.ctx(), |ui| self.inspector(ui));
            if !open {
                self.close_entry();
            }
        }
        if self.show_export {
            egui::Window::new("Daily text")
                .open(&mut self.show_export)
                .default_size([680., 510.])
                .show(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(440.)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.export)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(20)
                                    .interactive(false),
                            );
                        });
                    if ui.button("Copy daily text").clicked() {
                        ui.ctx().copy_text(self.export.clone());
                    }
                });
        }
        self.note_dialog(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(
        ctx: &egui::Context,
        c: &mut Controls,
        size: [f32; 2],
        mut events: Vec<egui::Event>,
    ) -> Vec<String> {
        let modifiers = events
            .iter()
            .find_map(|e| {
                if let egui::Event::Key { modifiers, .. } = e {
                    Some(*modifiers)
                } else {
                    None
                }
            })
            .unwrap_or_default();
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let drawn = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size.into())),
                events,
                ..Default::default()
            },
            |ui| c.workspace(ui),
        );
        let texts = drawn
            .shapes
            .iter()
            .filter_map(|shape| {
                let egui::Shape::Text(text) = &shape.shape else {
                    return None;
                };
                let rect = text.galley.rect.translate(text.pos.to_vec2());
                shape
                    .clip_rect
                    .intersects(rect)
                    .then(|| text.galley.job.text.clone())
            })
            .collect();
        drawn.drop_without_applying_deltas();
        texts
    }
    fn shortcut(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        [true, false]
            .map(|pressed| egui::Event::Key {
                key,
                pressed,
                physical_key: None,
                repeat: false,
                modifiers,
            })
            .to_vec()
    }
    #[test]
    fn compact_and_wide_transcripts_paint_both_inputs_the_latest_words_and_the_stop_control() {
        for size in [[780., 590.], [1260., 880.]] {
            let ctx = egui::Context::default();
            design::setup(&ctx);
            let (tx, _) = mpsc::sync_channel(30);
            let (_, rx) = mpsc::sync_channel(30);
            let mut c = preview::fixture(tx, rx);
            for night in [false, true] {
                c.reading.night = night;
                for _ in 0..3 {
                    frame(&ctx, &mut c, size, vec![]);
                }
                let text = frame(&ctx, &mut c, size, vec![]).join(" ");
                for expected in [
                    "Microphone",
                    "System audio",
                    "Stop recording",
                    "stone steps",
                    "Browse archive",
                ] {
                    assert!(
                        text.contains(expected),
                        "missing {expected} at {size:?} (night={night})"
                    );
                }
                assert_eq!(ctx.theme() == egui::Theme::Dark, night);
            }
        }
    }
    #[test]
    fn keyboard_note_focus_and_navigation_keep_draft_and_recorder_separate() {
        let ctx = egui::Context::default();
        design::setup(&ctx);
        let (tx, requests) = mpsc::sync_channel(30);
        let (_, rx) = mpsc::sync_channel(30);
        let mut c = preview::fixture(tx, rx);
        let size = [990., 750.];
        frame(&ctx, &mut c, size, vec![]);
        frame(
            &ctx,
            &mut c,
            size,
            shortcut(egui::Key::N, egui::Modifiers::COMMAND),
        );
        assert!(c.note_open);
        frame(
            &ctx,
            &mut c,
            size,
            vec![egui::Event::Text("Keep the quiet opening.".into())],
        );
        assert_eq!(c.note, "Keep the quiet opening.");
        frame(
            &ctx,
            &mut c,
            size,
            shortcut(egui::Key::Escape, egui::Modifiers::NONE),
        );
        assert!(!c.note_open);
        frame(
            &ctx,
            &mut c,
            size,
            shortcut(
                egui::Key::F,
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            ),
        );
        assert!(c.focus);
        frame(
            &ctx,
            &mut c,
            size,
            shortcut(egui::Key::Num2, egui::Modifiers::COMMAND),
        );
        assert_eq!(c.tab, 1);
        assert!(!c.focus);
        assert_eq!(c.note, "Keep the quiet opening.");
        assert!(!requests.try_iter().any(|r| r.path == "/command"));
    }
}
