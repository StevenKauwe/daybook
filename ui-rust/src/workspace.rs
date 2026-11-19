//! Recorder controls stay within reach while a full-height rail keeps the page stable.
use super::*;
use design::{INK, Icon, LINE};

const PAGES: [(Icon, &str); 6] = [
    (Icon::Live, "Transcript"),
    (Icon::Archive, "Archive"),
    (Icon::Brief, "Daily brief"),
    (Icon::Health, "Diagnostics"),
    (Icon::Settings, "Settings"),
    (Icon::System, "Screen text"),
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
        if page == 5 {
            self.screen_history.checked = None;
        }
        if matches!(page, 3 | 4) {
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
        for (i, key) in [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
        ]
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
            self.parallel
                .day
                .as_deref()
                .or(self.stream_page["day"].as_str())
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
                    RichText::new(if self.tab == 0 && !self.paragraph_view {
                        "Parallel transcript"
                    } else {
                        PAGES[self.tab].1
                    })
                    .font(design::serif(if self.tab == 0 { 35. } else { 43. }))
                    .color(colors.ink),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new("•••").strong(), |ui| {
                    let update_label = if self.updating() {
                        "View update progress…"
                    } else if stop {
                        "Stop recording, update & restart"
                    } else {
                        "Update & Restart"
                    };
                    if ui.button(update_label).clicked() {
                        self.start_update();
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
                    .add_enabled(fresh && !self.pending_action && !self.updating(), button)
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
        for index in [0, 1, 5, 2] {
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
                if !self.draft_images.is_empty() {
                    egui::ScrollArea::vertical()
                        .id_salt("draft_images")
                        .max_height(165.)
                        .show(ui, |ui| {
                            for id in self.draft_images.clone() {
                                ui.horizontal(|ui| {
                                    if self.image_link(ui, id) {
                                        self.inspect("observation", id, None);
                                        self.note_open = false;
                                    }
                                    if ui.small_button("Remove from note").clicked() {
                                        self.send("/detach_draft_image", Some(json!({"id":id})));
                                    }
                                });
                            }
                        });
                }
                ui.horizontal(|ui| {
                    self.paste_button(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                self.can_save_note(),
                                egui::Button::new(RichText::new("Save note").color(CARD))
                                    .fill(MINT),
                            )
                            .on_hover_text("Save note  ⌘↩")
                            .clicked()
                        {
                            self.save_note();
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
        let compact = self.tab == 0 || ui.available_width() < 990.;
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
                    if self.paragraph_view {
                        if ui.link("Time-aligned columns").clicked() {
                            self.paragraph_view = false;
                        }
                        if !self.focus {
                            self.input_strip(ui, self.fresh());
                        }
                        self.stream(ui);
                    } else {
                        self.parallel_transcript(ui);
                    }
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
                    } else if self.tab == 5 {
                        self.screen_text_page(ui);
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
                "Source details"
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
        if self.update_open {
            let mut open = true;
            egui::Window::new("Daybook update")
                .open(&mut open)
                .default_width(445.)
                .vscroll(true)
                .show(ui.ctx(), |ui| {
                    self.update_card(ui, self.settings_dirty());
                    if self.settings_dirty()
                        && self.tab != 4
                        && ui.button("Review settings edits").clicked()
                    {
                        self.update_open = false;
                        self.navigate(4);
                    }
                    if !self.error.is_empty() {
                        ui.colored_label(CORAL, &self.error);
                    }
                });
            self.update_open &= open;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passage_inspector_renders_one_duration_without_unavailable_chunk_buttons() {
        let (tx, _) = mpsc::sync_channel(12);
        let (_, rx) = mpsc::sync_channel(12);
        let mut c = preview::fixture(tx, rx);
        c.detail_request = json!({"kind":"observation","id":15});
        let chunks: Vec<_> = (41..59).map(|id| {
            json!({"id":id,"track":"remote","retained":false,"capture_metadata":{"frames":8000,"sample_rate":16000}})
        }).collect();
        c.detail = json!({"id":15,"kind":"transcript","source":"remote","text":"The print proof arrived.","at":100,"end":111,"sources":chunks});
        let ctx = egui::Context::default();
        design::setup(&ctx);
        let draw = |c: &mut Controls| {
            frame(&ctx, c, [1100., 850.], vec![]);
            frame(&ctx, c, [1100., 850.], vec![]).join("\n")
        };
        let words = draw(&mut c);
        assert!(
            words.contains("9 seconds") && words.contains("The print proof arrived."),
            "{words}"
        );
        assert!(
            ![
                "#41",
                "remote · unavailable",
                "Structured provenance",
                "Show saved audio in Finder"
            ]
            .iter()
            .any(|s| words.contains(s))
        );
        c.detail["sources"][0]["retained"] = json!(true);
        assert!(draw(&mut c).contains("Show saved audio in Finder"));
    }
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
    fn paint_controls(
        ctx: &egui::Context,
        c: &mut Controls,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Pos2)> {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1120., 790.),
                )),
                events,
                ..Default::default()
            },
            |ui| c.workspace(ui),
        );
        let labels = out
            .shapes
            .iter()
            .filter_map(|shape| {
                let egui::Shape::Text(t) = &shape.shape else {
                    return None;
                };
                let visible = t
                    .galley
                    .rect
                    .translate(t.pos.to_vec2())
                    .intersect(shape.clip_rect);
                visible
                    .is_positive()
                    .then(|| (t.galley.job.text.clone(), visible.center()))
            })
            .collect();
        out.drop_without_applying_deltas();
        labels
    }
    fn click_label(ctx: &egui::Context, c: &mut Controls, label: &str) {
        paint_controls(ctx, c, vec![]);
        let labels = paint_controls(ctx, c, vec![]);
        let pos = labels
            .iter()
            .find(|(s, _)| s.lines().next() == Some(label))
            .unwrap_or_else(|| panic!("missing {label} in {labels:?}"))
            .1;
        for pressed in [true, false] {
            paint_controls(
                ctx,
                c,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    #[test]
    fn corner_menu_submits_one_update_from_archive_and_the_successor_restores_the_view() {
        let ctx = egui::Context::default();
        design::setup(&ctx);
        let (tx, requests) = mpsc::sync_channel(30);
        let (reply, rx) = mpsc::sync_channel(30);
        let mut c = preview::fixture(tx, rx);
        preview::archive_fixture(&mut c, true);
        c.status["active"] = json!("false");
        c.status["armed"] = json!("false");
        c.status["update"] = json!({"available":true,"busy":false});
        c.note = "Remember the route".into();
        click_label(&ctx, &mut c, "•••");
        click_label(&ctx, &mut c, "Update & Restart");
        assert!(c.update_open && c.update_pending);
        assert_eq!(c.tab, 1);
        c.start_update();
        let requests: Vec<_> = requests
            .try_iter()
            .filter(|r| r.path == "/update")
            .collect();
        assert_eq!(requests.len(), 1);
        let body = requests[0].body.as_ref().unwrap();
        assert_eq!(body["draft"], "Remember the route");
        assert_eq!(body["workspace"]["filter"], c.filter());
        let (tx, next_requests) = mpsc::sync_channel(12);
        let (_, rx) = mpsc::sync_channel(12);
        let mut next = Controls::initial("fixture".into(), tx, rx);
        next.restore_workspace(&body["workspace"]);
        assert_eq!(next.tab, 1);
        assert_eq!(next.filter(), c.filter());
        assert!(next_requests.try_iter().any(|r| r.path == "/timeline"));
        reply
            .send(Reply {
                path: "/update".into(),
                body: Some(body.clone()),
                result: Err("Finish the screenshot first".into()),
            })
            .unwrap();
        c.poll();
        assert!(!c.updating() && c.update_open);
        assert!(c.error.contains("screenshot"));
    }
    #[test]
    fn unavailable_updates_and_settings_drafts_keep_the_request_local() {
        let (tx, requests) = mpsc::sync_channel(12);
        let (_, rx) = mpsc::sync_channel(12);
        let mut c = preview::fixture(tx, rx);
        c.tab = 2;
        c.status["update"] = json!({"available":false,"busy":false});
        c.start_update();
        assert!(c.update_open && !c.update_pending);
        c.status["update"]["available"] = json!(true);
        c.config.screen_seconds += 1;
        c.start_update();
        assert!(c.settings_dirty() && !c.update_pending);
        assert!(requests.try_iter().all(|r| r.path != "/update"));
        assert_eq!(c.tab, 2);
    }
    #[test]
    fn a_matching_session_opens_its_entire_time_selection() {
        let ctx = egui::Context::default();
        design::setup(&ctx);
        let (tx, requests) = mpsc::sync_channel(30);
        let (reply, rx) = mpsc::sync_channel(30);
        let mut c = preview::fixture(tx, rx);
        preview::archive_fixture(&mut c, false);
        c.query = "map".into();
        click_label(&ctx, &mut c, "Sessions");
        let request = requests.try_iter().find(|r| r.path == "/timeline").unwrap();
        assert_eq!(request.body.as_ref().unwrap()["sessions"], true);
        let at = c.items[0]["at"].as_i64().unwrap();
        let result = json!({"items":[],"sessions":[{"start":at-60,"until":at+1,"title":"Harbor walk","entries":3,
            "notes":1,"passages":1,"screens":1}],"activity":c.archive.activity});
        reply
            .send(Reply {
                path: request.path,
                body: request.body,
                result: Ok(result),
            })
            .unwrap();
        c.poll();
        click_label(&ctx, &mut c, "Harbor walk");
        assert!(!c.archive.by_session);
        assert_eq!(c.archive.selection, Some((at - 60, at + 1)));
        assert_eq!(c.query, "");
        let request = requests.try_iter().find(|r| r.path == "/timeline").unwrap();
        assert_eq!(request.body.unwrap()["from"], at - 60);
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
