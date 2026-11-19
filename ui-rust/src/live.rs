//! Transcript-first reading surface. Only one bounded page is held in the controls.
use super::*;
use design::Icon;
use std::sync::Arc;
struct Paragraph {
    source: String,
    parts: Vec<Value>,
    chars: usize,
}
fn passage_text(row: &Value) -> &str {
    let text = row["text"].as_str().unwrap_or("").trim();
    if text.is_empty() && row["image_id"].is_i64() {
        if row["source"] == "clipboard" {
            "Pasted image"
        } else {
            "Saved screenshot"
        }
    } else {
        text
    }
}
impl Paragraph {
    fn separator(&self) -> &'static str {
        if self.source == "passive_screen" {
            "\n"
        } else {
            " "
        }
    }
    fn text(&self) -> String {
        self.parts
            .iter()
            .map(passage_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(self.separator())
    }
    fn provisional(&self) -> bool {
        self.parts.iter().any(|r| r["kind"] == "preview")
    }
}
fn paragraphs(rows: &[Value]) -> Vec<Paragraph> {
    let mut result: Vec<Paragraph> = Vec::new();
    for row in rows {
        let text = passage_text(row);
        if text.is_empty() {
            continue;
        }
        let source = row["source"].as_str().unwrap_or("remote");
        let count = text.chars().count();
        let join = result.last().is_some_and(|p| {
            let last = p.parts.last().unwrap();
            let gap = row["at"].as_i64().unwrap_or(0) - last["end"].as_i64().unwrap_or(0);
            let boundary = last["text"]
                .as_str()
                .unwrap_or("")
                .trim_end()
                .ends_with(['.', '!', '?']);
            let related = match source {
                "passive_screen" => last["at"] == row["at"] && last["end"] == row["end"],
                "me" | "remote" => (-2..=12).contains(&gap),
                _ => false,
            };
            p.source == source
                && related
                && p.chars + count <= 4000
                && !(p.chars >= 700 && boundary)
                && last["truncated"] != true
                && row["truncated"] != true
        });
        if join {
            let p = result.last_mut().unwrap();
            p.chars += count;
            p.parts.push(row.clone());
        } else {
            result.push(Paragraph {
                source: source.into(),
                parts: vec![row.clone()],
                chars: count,
            });
        }
    }
    result
}
fn input_reading(
    status: &Value,
    track: &str,
    now: f64,
    fresh: bool,
) -> (&'static str, Option<(f32, f32)>) {
    if !fresh {
        return ("Unavailable", None);
    }
    let starting = status["capture_starting"] == "true";
    if status["active"] != "true" && !starting {
        return ("Off", None);
    }
    let v = &status["health"]["input_levels"][track];
    let age = v["end_at"].as_f64().map(|at| now - at);
    let freshness = if status["publish_interval_ms"].is_number() || status["window_ms"] == 20 {
        0.3
    } else {
        2.0
    };
    if !age.is_some_and(|age| (-0.1..=freshness).contains(&age)) {
        return (
            if starting {
                "Starting…"
            } else if age.is_some() {
                "No recent audio"
            } else {
                "Waiting for audio"
            },
            None,
        );
    }
    match (v["rms_dbfs"].as_f64(), v["peak_dbfs"].as_f64()) {
        (Some(rms), Some(peak)) if (-120.0..=0.0).contains(&rms) && (rms..=0.0).contains(&peak) => {
            (
                if peak <= -60.0 { "Silent" } else { "Signal" },
                Some((rms as f32, peak as f32)),
            )
        }
        _ => ("Level unavailable", None),
    }
}
#[derive(Default)]
pub(super) struct Reader {
    paragraphs: Arc<[Paragraph]>,
    layout: Option<(LayoutKey, Arc<[PassageLayout]>)>,
}
struct PassageLayout {
    job: egui::text::LayoutJob,
    height: f32,
}
impl PassageLayout {
    // Galleys contain UV coordinates into egui's current font atlas. A theme
    // switch or atlas rollover can invalidate those coordinates between frames.
    fn current_galley(&self, ui: &egui::Ui) -> Arc<egui::Galley> {
        ui.fonts_mut(|fonts| fonts.layout_job(self.job.clone()))
    }
}
#[derive(Clone, Copy, PartialEq)]
struct LayoutKey {
    width: f32,
    size: f32,
    journal: bool,
    night: bool,
    scale: f32,
}

impl Reader {
    fn prepare(&mut self, ui: &egui::Ui, key: LayoutKey) -> Arc<[PassageLayout]> {
        if let Some((cached_key, rows)) = &self.layout {
            if *cached_key == key {
                return Arc::clone(rows);
            }
        }
        let font = if key.journal {
            design::passage_font(key.size)
        } else {
            egui::FontId::proportional(key.size)
        };
        let layouts: Arc<[_]> = ui
            .fonts_mut(|fonts| {
                self.paragraphs
                    .iter()
                    .map(|p| {
                        let mut job = egui::text::LayoutJob::default();
                        job.wrap.max_width = key.width;
                        let colors = design::paper(key.night);
                        let accent = if p.source == "me" {
                            colors.mine
                        } else {
                            colors.remote
                        };
                        for (i, row) in p.parts.iter().enumerate() {
                            let format = egui::TextFormat {
                                font_id: font.clone(),
                                line_height: Some(key.size * 1.40),
                                color: if row["kind"] == "preview" {
                                    accent
                                } else {
                                    colors.ink
                                },
                                ..Default::default()
                            };
                            if i > 0 {
                                job.append(p.separator(), 0., format.clone());
                            }
                            job.append(passage_text(row), 0., format);
                        }
                        let height = fonts.layout_job(job.clone()).size().y;
                        PassageLayout { job, height }
                    })
                    .collect::<Vec<_>>()
            })
            .into();
        self.layout = Some((key, Arc::clone(&layouts)));
        layouts
    }
}

fn source_label(track: &str) -> &'static str {
    match track {
        "me" => "You",
        "passive_screen" | "screen" | "vision" => "Passive screen text",
        "screenshot" => "Screenshot",
        "clipboard" => "Clipboard",
        _ => "System audio",
    }
}

fn source_button(ui: &mut egui::Ui, glyph: Icon, label: &str) -> egui::Response {
    let response =
        ui.add(egui::Button::new(RichText::new(label).size(13.)).min_size(egui::vec2(0., 29.)));
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(match glyph {
            Icon::Note => "Write a note  ⌘N",
            _ => "Toggle reading focus  ⌘⇧F",
        })
}
impl Controls {
    pub(super) fn set_stream_page(&mut self, page: Value) {
        if self.stream_page["items"] != page["items"] {
            self.reader.paragraphs = paragraphs(
                page["items"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
            .into();
            self.reader.layout = None;
        }
        self.stream_page = page;
    }

    pub(super) fn input_strip(&mut self, ui: &mut egui::Ui, fresh: bool) {
        let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.;
        let snapshot = self.meters.snapshot();
        let (status, fresh) = snapshot
            .as_ref()
            .map(|(value, last)| (value.as_ref(), last.elapsed() < Duration::from_secs(1)))
            .unwrap_or((&self.status, fresh));
        if let Some(probe) = &mut self.perf_probe {
            probe.drawn(ui.ctx().cumulative_frame_nr(), status);
        }
        let t = ui.input(|i| i.time);
        let visible = !ui.input(|i| i.viewport().minimized.unwrap_or(false));
        ui.columns(2, |cols| {
            for (i,track,name,glyph,color) in [(0,"me","Microphone",Icon::Mic,design::SIGNAL),(1,"remote","System audio",Icon::System,design::LILAC)] {
                let ui = &mut cols[i];
                let (state, level) = input_reading(status,track,now,fresh);
                let reading = &status["health"]["input_levels"][track];
                match (level,reading["end_at"].as_f64()) {
                    (Some((rms,peak)),Some(at)) => self.traces[i].observe(status["epoch"].as_u64(),at,rms,peak),
                    _ => self.traces[i].clear(),
                }
                let (rms,peak,moving)=self.meter_motion[i].step(level,t);
                if moving && visible {ui.ctx().request_repaint_after(Duration::from_millis(8));}
                egui::Frame::new().fill(design::CONSOLE).corner_radius(12).inner_margin(egui::Margin::symmetric(15,10)).show(ui,|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(7.,5.);
                    ui.horizontal(|ui| {
                        design::icon_slot(ui,glyph,color,16.);
                        ui.label(RichText::new(name).size(14.).strong().color(design::BG));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                            let db = level.map(|(_,p)| if p <= -60. {"<−60 dB".into()} else {format!("{p:.0} dB")}).unwrap_or_else(||"— dB".into());
                            design::eyebrow(ui,&db,color);
                            if ui.available_width() > 54. {ui.label(RichText::new(state).size(11.5).color(design::DIM));}
                        });
                    });
                    let (rect,response)=ui.allocate_exact_size(egui::vec2(ui.available_width(),30.),egui::Sense::hover());
                    self.traces[i].paint(ui,rect,now,color,(rms,peak));
                    if let Some((rms, peak)) = level {
                        let origin = match reading["method"].as_str() {
                            Some("microphone_io") => "Microphone audio I/O",
                            Some("audio_consumer") => "Audio consumer",
                            _ => "Input",
                        };
                        let window = reading["window_ms"].as_f64()
                            .map(|ms| format!(" · {ms:.1} ms window")).unwrap_or_default();
                        response.on_hover_text(format!("{origin}{window}\nRMS {rms:.1} dBFS · peak {peak:.1} dBFS\nRecent input history and current level."));
                    } else {
                        response.on_hover_text(format!("{name} · {state}"));
                    }
                });
            }
        });
    }
    pub(super) fn stream_request(&mut self, cursor: Value) {
        self.follow_live = false;
        let mut body = json!({"day":self.stream_page["day"].as_str().map(str::to_owned).unwrap_or_else(||chrono::Local::now().date_naive().to_string()),"with_saves":true});
        if self.reading.passive_screen {
            body["passive_screen"] = json!(true);
        }
        if let Some(c) = cursor.as_object() {
            for k in ["before_at", "before_id", "after_at", "after_id"] {
                if let Some(v) = c.get(k) {
                    body[k] = v.clone();
                }
            }
        }
        self.stream_request = body.clone();
        self.send("/transcript", Some(body));
        self.jump_to_live = false;
    }
    pub(super) fn follow_stream(&mut self) {
        self.follow_live = true;
        self.jump_to_live = true;
        self.stream_request = Value::Null;
        self.send(
            "/transcript",
            Some(json!({"day":chrono::Local::now().date_naive().to_string(),"passive_screen":self.reading.passive_screen,"with_saves":true})),
        );
    }
    pub(super) fn stream(&mut self, ui: &mut egui::Ui) {
        let colors = design::paper(self.reading.night);
        let old_style = self.reading;
        let active = self.status["active"] == "true";
        let fresh = self.fresh();
        let follow_label = if !self.stream_request.is_null() {
            "Earlier passages"
        } else if !self.follow_live {
            "Reading back"
        } else if active && fresh {
            "Following live"
        } else {
            "Latest passages"
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(follow_label)
                    .strong()
                    .size(15.)
                    .color(colors.ink),
            );
            if !self.follow_live && ui.link("Jump to live").clicked() {
                self.follow_stream();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if source_button(
                    ui,
                    Icon::Focus,
                    if self.focus { "Exit focus" } else { "Focus" },
                )
                .clicked()
                {
                    self.focus = !self.focus;
                }
                ui.menu_button("Reading", |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.reading.size > 16, egui::Button::new("A−"))
                            .clicked()
                        {
                            self.reading.size = (self.reading.size - 2).max(16);
                        }
                        ui.label(format!("{} pt", self.reading.size));
                        if ui
                            .add_enabled(self.reading.size < 33, egui::Button::new("A+"))
                            .clicked()
                        {
                            self.reading.size = (self.reading.size + 2).min(33);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.reading.night, false, "Paper");
                        ui.selectable_value(&mut self.reading.night, true, "Night");
                    });
                    ui.selectable_value(&mut self.reading.journal, true, "Journal serif");
                    ui.selectable_value(&mut self.reading.journal, false, "System sans");
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.reader.paragraphs.is_empty(),
                            egui::Button::new("Copy this page"),
                        )
                        .clicked()
                    {
                        let text = self
                            .reader
                            .paragraphs
                            .iter()
                            .map(|p| format!("{}\n{}", source_label(&p.source), p.text()))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        ui.ctx().copy_text(text);
                        ui.close();
                    }
                });
                if source_button(ui, Icon::Note, "Quick note").clicked() {
                    self.open_note();
                }
                if let Some((msg, at)) = &self.notice {
                    if at.elapsed() < Duration::from_secs(3) {
                        ui.colored_label(MINT, msg);
                    }
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Read alongside your microphone");
            ui.selectable_value(&mut self.reading.passive_screen, false, "System audio");
            ui.selectable_value(
                &mut self.reading.passive_screen,
                true,
                "Passive screen text",
            );
        });
        if self.reading != old_style && self.preview_output.is_none() {
            self.send("/reading", Some(json!(self.reading)));
            if self.reading.passive_screen != old_style.passive_screen {
                if self.stream_request.is_null() {
                    self.follow_stream();
                } else {
                    self.stream_request(self.stream_request.clone());
                }
                self.set_stream_page(json!({"day":self.stream_page["day"]}));
            }
        }
        let loaded = &self.status["health"]["loaded_speech_model"];
        if let Some(reason) = self.status["health"]["speech_blocker"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            ui.colored_label(colors.fault, reason);
        }
        if let Some(reason) = loaded["error"].as_str().filter(|s| !s.is_empty()) {
            ui.colored_label(colors.fault, reason);
        }
        if active && loaded["loading"] == true {
            muted(
                ui,
                "Preparing the speech model. Incoming audio is buffered.",
            );
        }
        let groups = Arc::clone(&self.reader.paragraphs);
        let jump = std::mem::take(&mut self.jump_to_live);
        let height = (ui.available_height() - 46.).max(115.);
        egui::Frame::new().fill(colors.sheet).stroke(egui::Stroke::new(1.,colors.line)).corner_radius(14).inner_margin(egui::Margin::symmetric(15,7)).show(ui,|ui|{
            let scroll=egui::ScrollArea::vertical().id_salt("journal_transcript").auto_shrink([false,false]).max_height(height-16.).min_scrolled_height(height-16.).stick_to_bottom(self.follow_live).animated(false);
            let result=scroll.show(ui,|ui|{
                let outer=ui.available_width();
                let pad=((outer-812.)/2.).clamp(12.,300.);
                let width=(outer-pad*2.-4.).max(65.);
                ui.horizontal_top(|ui|{
                    ui.spacing_mut().item_spacing.x=0.;
                    ui.add_space(pad);
                    ui.vertical(|ui|{
                        ui.set_width(width);
                        if groups.is_empty(){
                            ui.add_space(36.);
                            design::icon_slot(ui,Icon::Book,colors.mine,32.);
                            ui.add_space(4.);
                            let (heading,body)=if !fresh {("Waiting for the recorder","Your saved conversation will appear when Daybook reconnects.")}
                                else if self.reading.passive_screen {("Microphone and screen text","Spoken words and newly observed screen text appear here during recording.")}
                                else if active {("Listening for speech","Words appear here as the microphone or system audio picks up speech.")}
                                else {("Your transcript starts here","Start recording when you’re ready. Your microphone and system audio will appear as separate voices.")};
                            ui.label(RichText::new(heading).font(design::serif(30.)).color(colors.ink));
                            muted(ui,body);
                        } else {
                            let key=LayoutKey{width,size:self.reading.size as f32,journal:self.reading.journal,night:self.reading.night,scale:ui.ctx().pixels_per_point()};
                            let layouts=self.reader.prepare(ui,key);
                            ui.add_space(5.);
                            for (index,(p,layout)) in groups.iter().zip(layouts.iter()).enumerate() {
                                let cuts=p.parts.iter().filter(|v|v["truncated"]==true).count();
                                let height=39.+layout.height+15.+cuts as f32*36.;
                                let (r,_)=ui.allocate_exact_size(egui::vec2(width,height),egui::Sense::hover());
                                if !ui.is_rect_visible(r) {continue;}
                                let mut row=ui.new_child(egui::UiBuilder::new().id_salt(("passage",&p.parts[0]["id"],index)).max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
                                row.spacing_mut().item_spacing.y=5.;
                                let mine=p.source=="me"; let accent=if mine {colors.mine} else {colors.remote};
                                row.horizontal(|ui|{
                                    ui.spacing_mut().item_spacing.x=7.;
                                    let glyph=match p.source.as_str() { "me"=>Icon::Mic,"remote"=>Icon::System,"clipboard"=>Icon::Note,_=>Icon::Book };
                                    let (slot,_)=ui.allocate_exact_size(egui::Vec2::splat(23.),egui::Sense::hover());
                                    ui.painter().circle_filled(slot.center(),11.5,if self.reading.night{design::CONSOLE}else if mine{Color32::from_rgb(221,238,224)}else{Color32::from_rgb(236,229,246)});
                                    design::icon(ui.painter(),slot.center(),14.,glyph,accent);
                                    ui.label(RichText::new(source_label(&p.source)).size(13.5).strong().color(accent));
                                    if p.provisional(){design::eyebrow(ui,"Writing…",accent);}
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                                        if p.parts.len()==1 {
                                            let action=if p.parts[0]["image_id"].is_i64() {"Open original"} else {"Passage details"};
                                            if ui.small_button(action).clicked() { self.inspect("observation",p.parts[0]["id"].as_i64().unwrap_or(0),None); }
                                        } else { ui.menu_button(RichText::new("Passages").size(11.).color(colors.muted),|ui|{
                                            for part in &p.parts{
                                                let excerpt = part["text"].as_str().unwrap_or("").split_whitespace().take(5).collect::<Vec<_>>().join(" ");
                                                if ui.button(format!("{} · {excerpt}",timestamp(&part["at"]))).clicked(){
                                                    self.inspect("observation",part["id"].as_i64().unwrap_or(0),None);ui.close();
                                                }
                                            }
                                        }); }
                                    });
                                });
                                row.add(egui::Label::new(layout.current_galley(&row)).selectable(true));
                                for part in p.parts.iter().filter(|v|v["truncated"]==true) {
                                    if row.small_button("Read full passage").clicked(){self.inspect("observation",part["id"].as_i64().unwrap_or(0),None);}
                                }
                                if index+1<groups.len() {row.painter().hline(r.x_range(),r.bottom()-1.,egui::Stroke::new(0.65,colors.line));}
                            }
                        }
                    });
                });
                if jump {ui.scroll_to_cursor(Some(egui::Align::BOTTOM));}
            });
            let in_reader=ui.input(|i|i.pointer.hover_pos()).is_some_and(|p|result.inner_rect.expand(8.).contains(p));
            if in_reader && (ui.input(|i|i.smooth_scroll_delta.y>0.) || (ui.input(|i|i.pointer.any_down()) && result.state.offset.y+result.inner_rect.height()+8.<result.content_size.y)) {self.follow_live=false;}
        });
        ui.horizontal(|ui| {
            let count = groups.len();
            muted(
                ui,
                format!(
                    "{count} {}",
                    if count == 1 {
                        "paragraph"
                    } else {
                        "paragraphs"
                    }
                ),
            );
            if !self.stream_page["older"].is_null() && ui.link("← Earlier").clicked() {
                self.stream_request(self.stream_page["older"].clone());
            }
            if !self.stream_page["newer"].is_null() && ui.link("Later →").clicked() {
                self.stream_request(self.stream_page["newer"].clone());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link("Browse archive →").clicked() {
                    self.navigate(1);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_pages_reuse_layout_and_a_revision_or_reading_change_reflows() {
        let ctx = egui::Context::default();
        design::setup(&ctx);
        let (tx, _) = mpsc::sync_channel(30);
        let (_, rx) = mpsc::sync_channel(30);
        let mut c = preview::fixture(tx, rx);
        let key = LayoutKey {
            width: 410.,
            size: 23.,
            journal: true,
            night: false,
            scale: 1.,
        };
        let drawn = ctx.run_ui(Default::default(), |ui| {
            let first = c.reader.prepare(ui, key);
            let mut page = c.stream_page.clone();
            page["total"] = json!(1000);
            c.set_stream_page(page.clone());
            assert!(Arc::ptr_eq(&first, &c.reader.prepare(ui, key)));
            page["items"][3]["text"] = json!("Revised phrase.");
            c.set_stream_page(page);
            let revised = c.reader.prepare(ui, key);
            assert!(!Arc::ptr_eq(&first, &revised));
            assert_eq!(revised.last().unwrap().job.text, "Revised phrase.");
            for changed in [
                LayoutKey { width: 300., ..key },
                LayoutKey { night: true, ..key },
                LayoutKey {
                    journal: false,
                    ..key
                },
            ] {
                let new = c.reader.prepare(ui, changed);
                assert!(!Arc::ptr_eq(&new, &revised));
            }
        });
        drawn.drop_without_applying_deltas();
    }
    #[test]
    fn a_rebuilt_font_atlas_gets_fresh_glyphs_even_when_paragraph_metrics_match() {
        let (tx, _) = mpsc::sync_channel(30);
        let (_, rx) = mpsc::sync_channel(30);
        let mut c = preview::fixture(tx, rx);
        let key = LayoutKey {
            width: 420.,
            size: 23.,
            journal: true,
            night: true,
            scale: 1.,
        };
        let mut previous = None;
        for glyphs in ["abcdefgh", "ZYX987!?"] {
            let ctx = egui::Context::default();
            design::setup(&ctx);
            let output = ctx.run_ui(Default::default(), |ui| {
                ui.label(RichText::new(glyphs).font(design::passage_font(23.)));
                let metrics = c.reader.prepare(ui, key);
                let drawn = metrics[0].current_galley(ui);
                let current = ui.fonts_mut(|fonts| fonts.layout_job(metrics[0].job.clone()));
                assert!(Arc::ptr_eq(&drawn, &current));
                if let Some(old) = &previous {
                    assert!(!Arc::ptr_eq(&drawn, old));
                }
                previous = Some(drawn);
            });
            output.drop_without_applying_deltas();
        }
    }
    #[test]
    fn paragraphs_join_continuations_keep_provenance_and_split_speakers_or_gaps() {
        let rows = vec![
            json!({"id":1,"source":"me","at":100,"end":108,"text":"A sentence that continues","kind":"transcript"}),
            json!({"id":2,"source":"me","at":108,"end":115,"text":" into the next chunk.","kind":"preview"}),
            json!({"id":3,"source":"remote","at":116,"end":120,"text":"Another speaker.","kind":"transcript"}),
            json!({"id":4,"source":"remote","at":150,"end":154,"text":"A later conversation.","kind":"transcript"}),
        ];
        let grouped = paragraphs(&rows);
        assert_eq!(grouped.len(), 3);
        assert_eq!(
            grouped[0].text(),
            "A sentence that continues into the next chunk."
        );
        assert!(grouped[0].provisional());
        assert_eq!(
            grouped[0]
                .parts
                .iter()
                .map(|r| r["id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(grouped[0].parts[1]["at"], 108);
        assert!(!grouped[0].text().contains("108"));
    }
    #[test]
    fn screen_lines_keep_their_span_and_manual_images_keep_their_own_original() {
        let entries = [
            json!({"kind":"screen","source":"passive_screen","text":"Title","at":5,"end":15}),
            json!({"kind":"screen","source":"passive_screen","text":"Proof 4","at":5,"end":15}),
            json!({"kind":"screen","source":"passive_screen","text":"Title","at":20,"end":25}),
            json!({"kind":"screen","source":"screenshot","text":"","at":21,"end":21,"image_id":4}),
            json!({"kind":"screen","source":"screenshot","text":"Print proof","at":21,"end":21,"image_id":5}),
        ];
        let p = paragraphs(&entries);
        assert_eq!(
            p.iter().map(Paragraph::text).collect::<Vec<_>>(),
            ["Title\nProof 4", "Title", "Saved screenshot", "Print proof"]
        );
    }
    #[test]
    fn input_meters_distinguish_silence_waiting_stale_stopped_and_each_track() {
        let mut s = json!({"active":"true","health":{"input_levels":{"me":{"end_at":100.0,"rms_dbfs":-24.0,"peak_dbfs":-12.0},"remote":{"end_at":100.0,"rms_dbfs":-120.0,"peak_dbfs":-120.0}}}});
        assert_eq!(
            input_reading(&s, "me", 101.0, true),
            ("Signal", Some((-24.0, -12.0)))
        );
        assert_eq!(input_reading(&s, "remote", 101.0, true).0, "Silent");
        assert_eq!(
            input_reading(&s, "me", 103.0, true),
            ("No recent audio", None)
        );
        assert_eq!(input_reading(&s, "me", 101.0, false), ("Unavailable", None));
        s["active"] = json!("false");
        assert_eq!(input_reading(&s, "me", 101.0, true), ("Off", None));
        s["capture_starting"] = json!("true");
        s["health"]["input_levels"] = json!({});
        assert_eq!(input_reading(&s, "me", 101.0, true), ("Starting…", None));
        s["capture_starting"] = json!("false");
        s["active"] = json!("true");
        assert_eq!(
            input_reading(&s, "me", 101.0, true),
            ("Waiting for audio", None)
        );
    }
    #[test]
    fn fast_meter_expires_promptly_instead_of_animating_dead_input() {
        let s = json!({"active":"true","window_ms":20,"health":{"input_levels":{"me":{"end_at":100.0,"rms_dbfs":-24.0,"peak_dbfs":-12.0}}}});
        assert_eq!(input_reading(&s, "me", 100.1, true).0, "Signal");
        assert_eq!(
            input_reading(&s, "me", 100.31, true),
            ("No recent audio", None)
        );
        assert_eq!(
            input_reading(&s, "me", 99.0, true),
            ("No recent audio", None)
        );
    }
}
