//! Native local controls. No telemetry endpoint, browser or capture API in this process.
#![deny(unsafe_code)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Shared config includes backend-only loading and import helpers; controls use its types and validators."
    )
)]
#[path = "../../src/config.rs"]
mod config;
use eframe::egui::{self, Color32, RichText};
use serde_json::{Value, json};
use std::{
    io::Read,
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};
mod archive;
mod credentials;
mod design;
mod keywords;
mod live;
mod meters;
mod parallel;
mod parallel_canvas;
#[allow(unsafe_code)]
mod paste;
mod perf;
mod picture;
mod preview;
#[path = "../../src/reading.rs"]
mod reading;
mod scope;
mod screen_captures;
mod screen_history;
#[path = "../../src/screen_layout.rs"]
mod screen_layout;
mod time_ruler;
#[path = "../../src/transcript_range.rs"]
mod transcript_range;
mod workspace;
use design::{CARD, CORAL, MINT, MUTED};
struct Request {
    path: String,
    body: Option<Value>,
}
struct Reply {
    path: String,
    body: Option<Value>,
    result: Result<Value, String>,
}
fn call(base: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .redirects(0)
        .build();
    call_with(&agent, base, path, body)
}
fn call_with(
    agent: &ureq::Agent,
    base: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let response = if let Some(v) = body {
        let mut request = agent.post(&format!("{base}{path}"));
        if path == "/config_with_key" {
            request = request.timeout(Duration::from_secs(120));
        }
        request.send_json(v)
    } else {
        agent.get(&format!("{base}{path}")).call()
    };
    let response = match response {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => {
            let mut b = String::new();
            let _ = r.into_reader().take(16000).read_to_string(&mut b);
            return Err(serde_json::from_str::<Value>(&b)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or("Request rejected".into()));
        }
        Err(_) => {
            return Err(
                "Cannot reach the local recorder. Your draft remains in this window.".into(),
            );
        }
    };
    let limit = if matches!(path, "/copy_time_range" | "/copy_transcript") {
        13 * 1024 * 1024
    } else {
        4 * 1024 * 1024
    };
    let mut b = Vec::new();
    response
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut b)
        .map_err(|e| e.to_string())?;
    if b.len() as u64 > limit {
        return Err("Local response exceeded the inspection limit".into());
    }
    serde_json::from_slice(&b).map_err(|e| e.to_string())
}
fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(10)
        .stroke(egui::Stroke::new(1.0, design::LINE))
        .inner_margin(18)
        .show(ui, contents);
}
fn muted(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(if ui.visuals().dark_mode {
        design::DIM
    } else {
        MUTED
    }));
}
fn audio_duration(item: &Value, parts: &[Value]) -> String {
    let measured: Option<f64> = (!parts.is_empty())
        .then(|| {
            parts
                .iter()
                .map(|part| {
                    let m = &part["capture_metadata"];
                    Some(
                        m["frames"].as_f64()?
                            / m["sample_rate"].as_f64().filter(|rate| *rate > 0.)?,
                    )
                })
                .sum::<Option<f64>>()
        })
        .flatten();
    let seconds = measured.or_else(|| Some((item["end"].as_f64()? - item["at"].as_f64()?).max(0.)));
    match seconds {
        Some(s) if s > 0. && s < 1. => "under a second".into(),
        Some(s) if s >= 1. => {
            let n = s.round() as u64;
            if n >= 60 {
                format!("{}m {}s", n / 60, n % 60)
            } else {
                format!("{n} {}", if n == 1 { "second" } else { "seconds" })
            }
        }
        _ => String::new(),
    }
}
fn timestamp(v: &Value) -> String {
    v.as_i64()
        .or_else(|| v.as_f64().map(|n| n as i64))
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or("Unavailable".into())
}
fn full_timestamp(v: &Value) -> String {
    v.as_f64()
        .and_then(|n| chrono::DateTime::from_timestamp(n as i64, 0))
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%b %d %H:%M:%S")
                .to_string()
        })
        .unwrap_or("Unavailable".into())
}
fn number(v: &Value, suffix: &str) -> String {
    v.as_f64()
        .map(|n| format!("{n:.1}{suffix}"))
        .unwrap_or("Unavailable".into())
}
fn progress(ui: &mut egui::Ui, label: &str, used: u64, limit: u64) {
    ui.label(format!("{label}  {used} / {limit}"));
    ui.add(
        egui::ProgressBar::new((used as f32 / limit.max(1) as f32).clamp(0.0, 1.0))
            .fill(if used >= limit { CORAL } else { MINT }),
    );
}
fn status_title(status: &Value, fresh: bool) -> (&'static str, Color32) {
    if !fresh {
        return ("Recorder unavailable", CORAL);
    }
    if status["capture_starting"] == "true" {
        ("Starting audio", MINT)
    } else if status["capture_stopping"] == "true" {
        ("Stopping audio", MUTED)
    } else if status["active"] == "true" {
        ("Recording", CORAL)
    } else if status["status"]
        .as_str()
        .is_some_and(|s| s.starts_with("error:") || s.contains("capacity"))
    {
        ("Recording interrupted", CORAL)
    } else if status["armed"] == "true" {
        ("Workday recording enabled", MINT)
    } else if status["status"] == "paused" {
        ("Paused", MUTED)
    } else {
        ("Stopped", MUTED)
    }
}
struct Controls {
    base: String,
    tx: SyncSender<Request>,
    rx: Receiver<Reply>,
    status: Value,
    last_seen: Option<Instant>,
    config: config::Config,
    saved_config: Value,
    loaded: bool,
    error: String,
    message: String,
    pending_action: bool,
    update_pending: bool,
    update_open: bool,
    note: String,
    note_dirty: Option<Instant>,
    saving_note: bool,
    draft_images: Vec<i64>,
    paste_monitor: Option<paste::Monitor>,
    pending_paste: Option<(u64, bool)>,
    tab: usize,
    settings_tab: usize,
    health_tab: usize,
    day: String,
    day_edit: String,
    query: String,
    kind: String,
    source: String,
    items: Vec<Value>,
    jobs: Vec<Value>,
    next: Value,
    pages: usize,
    last_timeline: Instant,
    loading_timeline: bool,
    archive: archive::Archive,
    screen_history: screen_history::History,
    picture: picture::Viewer,
    thumbnails: picture::Thumbnails,
    detail: Value,
    detail_request: Value,
    history: Vec<(Value, Value)>,
    export: String,
    show_export: bool,
    import: String,
    credentials: credentials::Credentials,
    settings_saving: bool,
    quitting: bool,
    stream_page: Value,
    reader: live::Reader,
    parallel: parallel::Timeline,
    paragraph_view: bool,
    reading: reading::ReadingStyle,
    detailed_poll: std::sync::Arc<std::sync::atomic::AtomicBool>,
    minimized: std::sync::Arc<std::sync::atomic::AtomicBool>,
    note_open: bool,
    note_focus: bool,
    notice: Option<(String, Instant)>,
    traces: [scope::Trace; 2],
    stream_request: Value,
    follow_live: bool,
    jump_to_live: bool,
    focus: bool,
    preview_output: Option<std::path::PathBuf>,
    preview_frames: u32,
    preview_motion: bool,
    preview_timing: Vec<(f64, Option<f32>)>,
    meters: meters::Feed,
    meter_motion: [meters::Motion; 2],
    perf_probe: Option<perf::Probe>,
}
impl Controls {
    fn new(cc: &eframe::CreationContext<'_>, base: String) -> Self {
        design::setup(&cc.egui_ctx);
        let (tx, requests) = mpsc::sync_channel::<Request>(12);
        let (replies, rx) = mpsc::sync_channel(6);
        let detailed_poll = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let detailed_monitor = std::sync::Arc::clone(&detailed_poll);
        let minimized = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer_minimized = std::sync::Arc::clone(&minimized);
        let endpoint = base.clone();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .redirects(0)
                .build();
            let mut last_overview = Instant::now() - Duration::from_secs(2);
            let mut last_live = Instant::now();
            let mut fast = false;
            loop {
                let hidden = observer_minimized.load(std::sync::atomic::Ordering::Relaxed);
                let overview_interval = Duration::from_secs(
                    if !hidden && detailed_monitor.load(std::sync::atomic::Ordering::Relaxed) {
                        2
                    } else {
                        15
                    },
                );
                let live_interval = Duration::from_millis(if hidden {
                    2000
                } else if fast {
                    250
                } else {
                    1000
                });
                // A click always precedes scheduled polling when both are ready.
                let mut r = if let Ok(r) = requests.try_recv() {
                    r
                } else if last_overview.elapsed() >= overview_interval {
                    Request {
                        path: "/overview".into(),
                        body: None,
                    }
                } else if last_live.elapsed() >= live_interval {
                    Request {
                        path: "/live".into(),
                        body: None,
                    }
                } else {
                    let wait = overview_interval
                        .saturating_sub(last_overview.elapsed())
                        .min(live_interval.saturating_sub(last_live.elapsed()));
                    match requests.recv_timeout(wait) {
                        Ok(r) => r,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => return,
                    }
                };
                let result = if r.path == "/quit" {
                    let saved = if r.body.is_some() {
                        call_with(&agent, &endpoint, "/draft", r.body.as_ref())
                    } else {
                        Ok(Value::Null)
                    };
                    saved.and_then(|_| {
                        call_with(
                            &agent,
                            &endpoint,
                            "/command",
                            Some(&json!({"command":"quit"})),
                        )
                    })
                } else {
                    call_with(&agent, &endpoint, &r.path, r.body.as_ref())
                };
                if r.path == "/overview" {
                    last_overview = Instant::now();
                }
                if r.path == "/live" {
                    last_live = Instant::now();
                }
                if r.path == "/live" || r.path == "/overview" {
                    if let Ok(v) = &result {
                        fast = v["active"] == "true"
                            || v["capture_transition"] == "true"
                            || v["pending"].as_u64().unwrap_or(0) > 0;
                    }
                }
                // The reply only carries settings references, even on a transport error.
                if r.path == "/config_with_key" {
                    let mut envelope = r.body.take().unwrap_or_default();
                    if let Value::String(mut key) = envelope["api_key"].take() {
                        zeroize::Zeroize::zeroize(&mut key);
                    }
                    r.body = Some(envelope["config"].take());
                }
                if replies
                    .send(Reply {
                        path: r.path,
                        body: r.body,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
                ctx.request_repaint();
            }
        });
        let mut c = Self::initial(base, tx, rx);
        c.paste_monitor = paste::Monitor::install(cc.egui_ctx.clone());
        c.detailed_poll = detailed_poll;
        c.minimized = minimized;
        c.meters = meters::Feed::start(c.base.clone(), cc.egui_ctx.clone());
        c.send("/overview", None);
        c.send("/resume_workspace", Some(json!({})));
        c.follow_stream();
        c
    }
    fn initial(base: String, tx: SyncSender<Request>, rx: Receiver<Reply>) -> Self {
        let today = chrono::Local::now().date_naive().to_string();
        Self {
            base,
            tx,
            rx,
            status: Value::Null,
            last_seen: None,
            config: Default::default(),
            saved_config: Value::Null,
            loaded: false,
            error: String::new(),
            message: String::new(),
            pending_action: false,
            update_pending: false,
            update_open: false,
            note: String::new(),
            note_dirty: None,
            saving_note: false,
            draft_images: Vec::new(),
            paste_monitor: None,
            pending_paste: None,
            tab: 0,
            settings_tab: 0,
            health_tab: 0,
            day: today.clone(),
            day_edit: today,
            query: String::new(),
            kind: "all".into(),
            source: "all".into(),
            items: vec![],
            jobs: vec![],
            next: Value::Null,
            pages: 1,
            last_timeline: Instant::now(),
            loading_timeline: false,
            archive: Default::default(),
            screen_history: Default::default(),
            picture: Default::default(),
            thumbnails: Default::default(),
            detail: Value::Null,
            detail_request: Value::Null,
            history: vec![],
            export: String::new(),
            show_export: false,
            import: String::new(),
            credentials: Default::default(),
            settings_saving: false,
            quitting: false,
            stream_page: Value::Null,
            reader: Default::default(),
            parallel: Default::default(),
            paragraph_view: false,
            reading: Default::default(),
            detailed_poll: Default::default(),
            minimized: Default::default(),
            note_open: false,
            note_focus: false,
            notice: None,
            traces: Default::default(),
            stream_request: Value::Null,
            follow_live: true,
            jump_to_live: true,
            focus: false,
            preview_output: None,
            preview_frames: 0,
            preview_motion: false,
            preview_timing: Vec::new(),
            meters: Default::default(),
            meter_motion: Default::default(),
            perf_probe: None,
        }
    }
    fn send(&mut self, path: &str, body: Option<Value>) -> bool {
        if self
            .tx
            .try_send(Request {
                path: path.into(),
                body,
            })
            .is_err()
        {
            self.error = "Controls are busy. Your edits are kept; try again shortly.".into();
            false
        } else {
            true
        }
    }
    fn command(&mut self, c: &str) {
        if self.send("/command", Some(json!({"command":c}))) {
            if c == "start" {
                self.parallel.fixed = None;
                self.parallel.ruler = None;
                self.parallel.clear_picks();
                self.parallel.detail = None;
                self.tab = 0;
                self.follow_stream();
            }
            self.pending_action = true;
            self.meters.wake();
            self.message = match c {
                "start" => "Starting recorder…",
                "stop" => "Stopping recorder…",
                "pause" => "Pausing recorder…",
                "arm" => "Enabling workday schedule…",
                _ => "Request queued…",
            }
            .into();
        }
    }
    fn filter(&self) -> Value {
        let mut v =
            json!({"day":self.day,"kind":self.kind,"source":self.source,"query":self.query});
        if self.tab == 1 {
            if self.archive.by_session {
                v["sessions"] = json!(true);
            }
            if let Some((from, until)) = self.archive.selection {
                v["from"] = json!(from);
                v["until"] = json!(until);
            }
        }
        v
    }
    fn refresh(&mut self, more: bool) {
        self.request_timeline(more, false);
    }
    fn request_timeline(&mut self, more: bool, background: bool) {
        if self.loading_timeline {
            return;
        }
        let mut v = self.filter();
        v["activity"] = json!(self.tab == 1 && !more);
        v["background"] = json!(background);
        if more {
            v["before_at"] = self.next["before_at"].clone();
            v["before_id"] = self.next["before_id"].clone();
        }
        if self.send("/timeline", Some(v)) {
            self.loading_timeline = true;
            self.last_timeline = Instant::now();
        }
    }
    fn inspect(&mut self, kind: &str, id: i64, part: Option<i64>) {
        if !self.detail.is_null() {
            self.history
                .push((self.detail_request.clone(), self.detail.clone()));
            if self.history.len() > 12 {
                self.history.remove(0);
            }
        }
        let v = json!({"kind":kind,"id":id,"part":part});
        if self.send("/inspect", Some(v.clone())) {
            self.detail_request = v;
            self.detail = Value::Null;
            self.picture.clear();
        }
    }
    fn close_entry(&mut self) {
        self.detail_request = Value::Null;
        self.detail = Value::Null;
        self.history.clear();
        self.picture.clear();
    }
    fn begin_paste(&mut self, version: i64) {
        if self.pending_paste.is_none() && !self.saving_note {
            let attach = self.note_open;
            if self.send(
                "/paste_image",
                Some(json!({"version":version,"attach":attach})),
            ) {
                self.pending_paste = Some((0, attach));
                self.error.clear();
            }
        }
    }
    fn paste_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                self.pending_paste.is_none()
                    && !self.saving_note
                    && self.status["synthetic"] != true
                    && self.preview_output.is_none(),
                egui::Button::new(if self.pending_paste.is_some() {
                    "Pasting…"
                } else {
                    "Paste image"
                }),
            )
            .on_hover_text("Paste a copied image  ⌘V")
            .clicked()
        {
            if let Some(version) = paste::image_version() {
                self.begin_paste(version);
            } else {
                self.error = "Copy an image, then choose Paste image.".into();
            }
        }
    }
    fn pasted(&mut self, status: &Value) {
        let Some((ticket, attach)) = self.pending_paste else {
            return;
        };
        if ticket == 0 || status["ticket"].as_u64() != Some(ticket) {
            return;
        }
        if let Some(error) = status["error"].as_str() {
            self.pending_paste = None;
            self.error = error.into();
        } else if let Some(id) = status["image_id"].as_i64().filter(|v| *v > 0) {
            self.pending_paste = None;
            self.error.clear();
            if attach {
                if !self.draft_images.contains(&id) {
                    self.draft_images.push(id);
                }
            } else {
                self.inspect("observation", id, None);
            }
            self.notice = Some((
                (if attach {
                    "Image added to note"
                } else {
                    "Image saved"
                })
                .into(),
                Instant::now(),
            ));
            if self.tab == 1 {
                self.refresh(false);
            }
        }
    }
    fn can_save_note(&self) -> bool {
        (!self.note.trim().is_empty() || !self.draft_images.is_empty())
            && !self.saving_note
            && self.pending_paste.is_none()
    }
    fn save_note(&mut self) {
        if self.can_save_note()
            && self.send(
                "/note",
                Some(json!({"text":self.note,"include_images":true})),
            )
        {
            self.saving_note = true;
        }
    }
    fn image_link(&mut self, ui: &mut egui::Ui, id: i64) -> bool {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().min(180.), 76.),
            egui::Sense::click(),
        );
        self.thumbnails.paint(ui, rect, &self.base, id);
        response.on_hover_text("Open image").clicked()
    }
    fn inspect_page(&mut self, key: &str, after: i64) {
        let mut request = self.detail_request.clone();
        request[format!("{key}_after")] = json!(after);
        if self.send("/inspect", Some(request.clone())) {
            self.detail_request = request;
        }
    }
    fn inspection_navigation(&mut self, ui: &mut egui::Ui, v: &Value, key: &str) {
        let page = &v[format!("{key}_page")];
        if page["next"].is_null() && page["previous"].is_null() {
            return;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    page["after"].as_i64().unwrap_or(-1) >= 0,
                    egui::Button::new("First"),
                )
                .clicked()
            {
                self.inspect_page(key, -1);
            }
            if ui
                .add_enabled(page["previous"].is_i64(), egui::Button::new("Previous"))
                .clicked()
            {
                self.inspect_page(key, page["previous"].as_i64().unwrap());
            }
            if ui
                .add_enabled(page["next"].is_i64(), egui::Button::new("Next"))
                .clicked()
            {
                self.inspect_page(key, page["next"].as_i64().unwrap());
            }
            let rows = v[key].as_array().map_or(0, |a| a.len());
            muted(
                ui,
                format!(
                    "{rows} entries · {}",
                    if page["next"].is_i64() {
                        "more available"
                    } else {
                        "end of list"
                    }
                ),
            );
        });
    }
    fn poll(&mut self) {
        while let Ok(r) = self.rx.try_recv() {
            if r.path == "/timeline" {
                self.loading_timeline = false;
            }
            if r.path == "/copy_time_range" {
                self.archive.copying = false;
            }
            if matches!(r.path.as_str(), "/config" | "/config_with_key") {
                self.settings_saving = false;
            }
            if r.path == "/command" {
                self.pending_action = false;
            }
            if r.path == "/update" {
                self.update_pending = false;
            }
            if r.path == "/note" {
                self.saving_note = false;
            }
            if matches!(r.path.as_str(), "/screen_captures" | "/screen_capture") {
                self.screen_history.captures.reply(r);
                continue;
            }
            if r.path == "/screen_text" {
                self.screen_history.reply(r);
                continue;
            }
            if matches!(
                r.path.as_str(),
                "/transcript_window" | "/transcript_passages" | "/copy_transcript"
            ) {
                self.parallel_reply(r);
                continue;
            }
            match r.result {
                Err(e) => {
                    if r.path == "/paste_image" {
                        self.pending_paste = None;
                    }
                    self.error = e;
                    self.message.clear();
                }
                Ok(mut v) => match r.path.as_str() {
                    "/quit" => {
                        self.quitting = true;
                    }
                    "/transcript" => {
                        let latest = r.body.as_ref().is_some_and(|b| {
                            b["before_id"].is_null()
                                && b["after_id"].is_null()
                                && b["day"] == chrono::Local::now().date_naive().to_string()
                        });
                        if self.reading.passive_screen != (v["passive_screen"] == true) {
                            continue;
                        }
                        if (self.stream_request.is_null() && latest)
                            || (!self.stream_request.is_null()
                                && r.body.as_ref() == Some(&self.stream_request))
                        {
                            self.set_stream_page(v);
                        }
                    }
                    "/live" => {
                        self.pasted(&v["paste_image"]);
                        if self.stream_request.is_null()
                            && v["transcript"].is_object()
                            && self.reading.passive_screen
                                == (v["transcript"]["passive_screen"] == true)
                        {
                            self.set_stream_page(v["transcript"].take());
                        }
                        self.last_seen = Some(Instant::now());
                        for key in [
                            "active",
                            "armed",
                            "status",
                            "capture_warning",
                            "pending",
                            "memory_audio",
                            "capture_transition",
                            "capture_starting",
                            "capture_stopping",
                        ] {
                            self.status[key] = v[key].clone();
                        }
                        if !self.status["health"].is_object() {
                            self.status["health"] = json!({});
                        }
                        for key in [
                            "at",
                            "live_transcription",
                            "speech_blocker",
                            "expired_sources",
                            "lost_sources",
                            "memory_rejected_frames",
                            "loaded_speech_model",
                            "input_levels",
                        ] {
                            self.status["health"][key] = v[key].clone();
                        }
                    }
                    "/overview" => {
                        self.last_seen = Some(Instant::now());
                        if let Some(ids) = v["draft_images"].as_array() {
                            self.draft_images = ids.iter().filter_map(Value::as_i64).collect();
                        }
                        if !self.loaded {
                            if let Ok(c) =
                                serde_json::from_value::<config::Config>(v["config"].clone())
                            {
                                self.reading =
                                    reading::ReadingStyle::saved(&v["reading"].to_string());
                                self.credentials.reset(&c.summary);
                                self.saved_config = serde_json::to_value(&c).unwrap();
                                self.config = c;
                                self.note = v["draft"].as_str().unwrap_or("").into();
                                self.loaded = true;
                            }
                        }
                        self.status = v;
                        if self.detail["details"]["ocr_status"] == "pending"
                            && self.detail_request["kind"] == "observation"
                        {
                            self.send("/inspect", Some(self.detail_request.clone()));
                        }
                        if matches!(self.tab, 1 | 2)
                            && self.pages == 1
                            && (self.tab != 1 || !self.archive.reading_back)
                            && self.last_timeline.elapsed() > Duration::from_secs(2)
                        {
                            self.request_timeline(false, true);
                        }
                    }
                    "/timeline" => {
                        let b = r.body.unwrap_or_default();
                        let filter = self.filter();
                        if [
                            "day", "kind", "source", "query", "from", "until", "sessions",
                        ]
                        .iter()
                        .all(|k| b[*k] == filter[*k])
                        {
                            if b["background"] == true && self.tab == 1 && self.archive.reading_back
                            {
                                continue;
                            }
                            let rows = v["items"]
                                .as_array_mut()
                                .map(std::mem::take)
                                .unwrap_or_default();
                            let append = b["before_id"].is_number();
                            let changed = if append {
                                !rows.is_empty()
                            } else {
                                self.items != rows
                            };
                            if append {
                                self.items.extend(rows);
                                self.pages += 1;
                            } else {
                                if changed {
                                    self.items = rows;
                                }
                                self.pages = 1;
                            }
                            if changed {
                                self.archive.layout(&self.items);
                            }
                            self.archive.loaded_filter = filter;
                            if v["activity"].is_object() {
                                self.archive.activity = v["activity"].take();
                            }
                            if v["sessions"].is_array() {
                                self.archive.sessions = v["sessions"].take();
                            }
                            self.jobs = v["jobs"]
                                .as_array_mut()
                                .map(std::mem::take)
                                .unwrap_or_default();
                            self.next = v["next"].take();
                        } else {
                            self.refresh(false);
                        }
                    }
                    "/retry_saved_ocr" => {
                        if r.body
                            .as_ref()
                            .is_some_and(|b| b["id"] == self.detail["id"])
                        {
                            self.detail["details"]["ocr_status"] = json!("pending");
                        }
                    }
                    "/copy_time_range" => {
                        if let Some(text) = v["text"].as_str() {
                            self.archive.clipboard = Some(text.to_owned());
                            let count = v["count"].as_u64().unwrap_or(0);
                            self.archive.copy_receipt = format!(
                                "Copied {count} {}",
                                if count == 1 { "entry" } else { "entries" }
                            );
                            self.error.clear();
                        }
                    }
                    "/inspect" => {
                        if r.body.as_ref() == Some(&self.detail_request) {
                            self.detail = v;
                        }
                    }
                    "/paste_image" => {
                        if let Some(pending) = &mut self.pending_paste {
                            if let Some(ticket) = v["ticket"].as_u64() {
                                pending.0 = ticket;
                                self.send("/live", None);
                            } else {
                                self.pending_paste = None;
                                self.error = "Daybook could not start the paste.".into();
                            }
                        }
                    }
                    "/detach_draft_image" => {
                        if let Some(ids) = v["draft_images"].as_array() {
                            self.draft_images = ids.iter().filter_map(Value::as_i64).collect();
                        }
                    }
                    "/note" => {
                        if r.body.as_ref().is_some_and(|b| b["include_images"] == true) {
                            self.draft_images.clear();
                        }
                        if r.body.as_ref().and_then(|b| b["text"].as_str())
                            == Some(self.note.as_str())
                        {
                            self.note.clear();
                            self.note_open = false;
                            self.note_dirty = None;
                            self.send("/draft", Some(json!({"text":""})));
                        }
                        self.message = "Note saved on this Mac.".into();
                        self.notice = Some(("Note saved".into(), Instant::now()));
                        self.error.clear();
                        if matches!(self.tab, 1 | 2) {
                            self.refresh(false);
                        }
                    }
                    "/draft" | "/reading" => {}
                    "/resume_workspace" => self.restore_workspace(&v),
                    "/update" => {
                        self.status["update"] = v;
                        self.error.clear();
                        self.send("/overview", None);
                    }
                    "/config" | "/config_with_key" => {
                        if r.path == "/config_with_key" {
                            zeroize::Zeroize::zeroize(&mut *self.credentials.secret);
                            self.notice =
                                Some(("API key saved in Mac Keychain".into(), Instant::now()));
                        }
                        self.saved_config = r.body.unwrap_or(Value::Null);
                        self.message = if r.path == "/config_with_key" {
                            "API key saved in Mac Keychain. Settings saved."
                        } else {
                            "Settings saved. Recording remains stopped."
                        }
                        .into();
                        self.error.clear();
                        self.send("/overview", None);
                    }
                    "/command" => {
                        self.error.clear();
                        self.message.clear();
                        self.send("/overview", None);
                        if matches!(self.tab, 1 | 2) {
                            self.refresh(false);
                        }
                    }
                    "/export_day" => {
                        self.export = v["text"].as_str().unwrap_or("").into();
                        self.show_export = true;
                    }
                    "/import" => {
                        if let Ok(p) = serde_json::from_value(v) {
                            self.credentials.reset(&p);
                            self.config.summary = p;
                            self.message =
                                "Provider metadata imported into your draft. Review and save."
                                    .into();
                        }
                    }
                    _ => {
                        self.message = "Done.".into();
                    }
                },
            }
        }
    }
    fn select_day(&mut self, day: String) {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d") {
            self.day = d.to_string();
            self.day_edit = self.day.clone();
            self.archive.reset_day();
            self.items.clear();
            self.jobs.clear();
            self.next = Value::Null;
            self.pages = 1;
            self.refresh(false);
        }
    }
    fn date_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let mut move_day = 0;
            if ui.button("‹").on_hover_text("Previous day").clicked() {
                move_day = -1;
            }
            if ui.button("›").on_hover_text("Next day").clicked() {
                move_day = 1;
            }
            if move_day != 0 {
                if let Ok(d) = chrono::NaiveDate::parse_from_str(&self.day, "%Y-%m-%d") {
                    self.select_day((d + chrono::Duration::days(move_day)).to_string());
                }
            }
            if self.tab != 1 {
                ui.heading(&self.day);
            }
            if ui.button("Today").clicked() {
                self.select_day(chrono::Local::now().date_naive().to_string());
            }
            ui.menu_button("Choose date", |ui| {
                ui.text_edit_singleline(&mut self.day_edit);
                if ui.button("Go to date").clicked() {
                    if chrono::NaiveDate::parse_from_str(&self.day_edit, "%Y-%m-%d").is_ok() {
                        self.select_day(self.day_edit.clone());
                        ui.close();
                    } else {
                        self.error = "Use a date like 2026-11-16.".into();
                    }
                }
            });
            if ui.button("Daily text").clicked() {
                self.send("/export_day", Some(json!({"day":self.day})));
            }
            if self.tab == 1 && ui.button("New note").clicked() {
                self.open_note();
            }
            if self.tab == 1 {
                self.paste_button(ui);
            }
        });
    }
    fn brief(&mut self, ui: &mut egui::Ui) {
        self.date_bar(ui);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Summarize new material").clicked() {
                self.command("compress");
            }
            if ui.button("Consolidate summaries").clicked() {
                self.command("meta");
            }
        });
        if self.jobs.is_empty() {
            card(ui, |ui| {
                ui.heading("Your daily brief will appear here");
                muted(
                    ui,
                    "Once there is finalized conversation, screen context or a note, summarize new material. Live previews are excluded until final.",
                );
            });
        }
        for j in self.jobs.clone() {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(if j["kind"] == "meta" {
                        "Consolidated summary"
                    } else {
                        "Incremental summary"
                    });
                });
                ui.label(j["text"].as_str().unwrap_or(""));
                if ui.button("View sources").clicked() {
                    self.inspect("job", j["id"].as_i64().unwrap(), None);
                }
            });
        }
    }
    fn health(&mut self, ui: &mut egui::Ui) {
        muted(
            ui,
            "Local measurements and logs. No external telemetry reporting.",
        );
        ui.horizontal(|ui| {
            for (i, label) in ["Capture & speech", "Models & budgets", "Diagnostics"]
                .iter()
                .enumerate()
            {
                ui.selectable_value(&mut self.health_tab, i, *label);
            }
        });
        let h = self.status["health"].clone();
        match self.health_tab {
            0 => {
                if self.status["synthetic"] == true {
                    ui.colored_label(
                        CORAL,
                        "Synthetic mode cannot establish live capture health.",
                    );
                }
                for t in h["tracks"].as_array().cloned().unwrap_or_default() {
                    card(ui, |ui| {
                        ui.heading(if t["track"] == "me" {
                            "Microphone · me"
                        } else {
                            "Meeting/system · remote"
                        });
                        muted(
                            ui,
                            format!(
                                "Last source received: {}",
                                t["last_source_age_seconds"]
                                    .as_i64()
                                    .map(|n| format!("{n} s ago"))
                                    .unwrap_or("No source received".into())
                            ),
                        );
                        progress(
                            ui,
                            "Pending audio blocks",
                            t["pending_files"].as_u64().unwrap_or(0),
                            self.config.max_pending as u64,
                        );
                        ui.label(format!(
                            "{} PCM frames · {} seconds of audio · {} bytes",
                            t["pcm_frames"],
                            number(&t["pcm_seconds"], ""),
                            t["file_bytes"]
                        ));
                        ui.label(format!(
                            "Oldest backlog age: {}",
                            t["oldest_age_seconds"]
                                .as_i64()
                                .map(|n| format!("{n} s"))
                                .unwrap_or("No pending source".into())
                        ));
                        if t["unknown_files"].as_u64().unwrap_or(0) > 0 {
                            ui.colored_label(CORAL,"Some pending media is missing or not PCM; measured audio duration is incomplete.");
                        }
                    });
                }
                card(ui, |ui| {
                    ui.strong("Native capture queue");
                    let n = &h["native"];
                    if n.is_null() {
                        muted(ui, "Unavailable — no capture-helper telemetry received.");
                    } else {
                        ui.label(format!(
                            "{} / {} packets · {} PCM bytes · {} s buffered",
                            n["queue_packets"],
                            n["queue_capacity_packets"],
                            n["queue_pcm_bytes"],
                            number(&n["queue_audio_seconds"], "")
                        ));
                        ui.label(format!("Limits: {} MiB PCM · {} s per track. Includes the block being processed.",number(&n["queue_capacity_pcm_bytes"].as_u64().map(|b|json!(b as f64/1048576.0)).unwrap_or(Value::Null),""),number(&n["queue_track_capacity_seconds"],"")));
                        ui.label(format!("Known queue-rejected frames since helper launch: {} · fault events: {}",n["known_rejected_frames"],n["fault_events"]));
                        muted(
                            ui,
                            "OS/device dropped audio is unavailable. Known rejects are not a total dropped-audio count.",
                        );
                        if let Some(at) = h["native_seen_at"].as_i64() {
                            if chrono::Utc::now().timestamp() - at > 5 {
                                ui.colored_label(CORAL, "Capture heartbeat is stale.");
                            }
                        }
                    }
                });
                card(ui, |ui| {
                    ui.strong("Latest transcription responsiveness");
                    let loaded = &h["loaded_speech_model"];
                    if loaded["loaded"] == true {
                        ui.label(format!(
                            "Loaded speech model: {}",
                            loaded["model_path"].as_str().unwrap_or("unavailable")
                        ));
                        ui.label(format!(
                            "Actual backend: {} · {} · requested {}",
                            loaded["actual_backend"].as_str().unwrap_or("unavailable"),
                            loaded["device"].as_str().unwrap_or("unavailable"),
                            loaded["requested_backend"]
                                .as_str()
                                .unwrap_or("unavailable")
                        ));
                        if let Some(reason) = loaded["fallback_reason"].as_str() {
                            ui.colored_label(CORAL, format!("CPU fallback: {reason}"));
                        }
                    } else {
                        muted(
                            ui,
                            "Speech model not loaded yet; actual backend unavailable until audio is processed.",
                        );
                    }
                    for (key, label) in [
                        ("preview_latency", "Provisional"),
                        ("final_latency", "Final"),
                    ] {
                        let m = &h[key];
                        ui.label(format!(
                            "{label}: {} source-end → result · {} processing",
                            number(&m["source_end_to_result_ms"], " ms"),
                            number(&m["source_processing_ms"], " ms")
                        ));
                        if m["timing_estimated"] == true {
                            muted(
                                ui,
                                "Timing uses rounded or inferred source timestamps (estimate).",
                            );
                        }
                    }
                    muted(
                        ui,
                        format!(
                            "Latest preview sample: {} · latest final sample: {}",
                            full_timestamp(&h["preview_latency"]["at"]),
                            full_timestamp(&h["final_latency"]["at"])
                        ),
                    );
                    muted(
                        ui,
                        "Latest timings cover individual passages. Voice activity detection determines phrase boundaries. Speaker labels identify the input track.",
                    );
                });
                card(ui, |ui| {
                    ui.strong("Resource use");
                    ui.label(format!(
                        "Backend CPU: {} of one core",
                        number(&h["backend"]["cpu_core_percent"], "%")
                    ));
                    let footprint = h["backend"]["physical_footprint_bytes"]
                        .as_f64()
                        .map(|n| format!("{:.0} MiB", n / 1048576.0))
                        .unwrap_or_else(|| "Unavailable".into());
                    ui.label(format!("Recorder memory now   {footprint}"))
                        .on_hover_text("Physical footprint charged by macOS to the backend and speech inference.");
                    ui.label(format!(
                        "Backend peak RSS: {} MiB",
                        h["backend"]["peak_rss_bytes"]
                            .as_f64()
                            .map(|n| format!("{:.1}", n / 1048576.0))
                            .unwrap_or("Unavailable".into())
                    ));
                    ui.label(format!(
                        "Capture helper peak RSS: {} MiB",
                        h["native"]["process"]["peak_rss_bytes"]
                            .as_f64()
                            .map(|n| format!("{:.1}", n / 1048576.0))
                            .unwrap_or("Unavailable".into())
                    ));
                    muted(
                        ui,
                        "Current memory covers the recorder and speech model. Peak RSS includes earlier allocations. The capture helper and controls run in separate processes.",
                    );
                });
            }
            1 => {
                card(ui, |ui| {
                    ui.strong(format!(
                        "Today’s guardrails · {}",
                        h["day"].as_str().unwrap_or("")
                    ));
                    progress(
                        ui,
                        "Reserved requests",
                        h["budget"]["requests"].as_u64().unwrap_or(0),
                        self.config.daily_requests,
                    );
                    progress(
                        ui,
                        "Images",
                        h["budget"]["images"].as_u64().unwrap_or(0),
                        self.config.daily_images,
                    );
                    progress(
                        ui,
                        "Conservative token units",
                        h["budget"]["reserved_units"].as_u64().unwrap_or(0),
                        self.config.daily_token_reservation,
                    );
                    muted(
                        ui,
                        "Reservations include failed attempts. Token units are conservative estimates, not tokenizer measurements or billing.",
                    );
                    ui.label(if h["usage"]["reported_calls"].as_u64().unwrap_or(0) == 0 {
                        "Reported token usage unavailable (no usage-bearing responses yet).".into()
                    } else {
                        format!(
                            "Reported tokens: {} input + {} output across {} of {} completed calls",
                            h["usage"]["reported_prompt_tokens"],
                            h["usage"]["reported_completion_tokens"],
                            h["usage"]["reported_calls"],
                            h["usage"]["completed_calls"]
                        )
                    });
                    let priced = h["usage"]["priced_calls"].as_u64().unwrap_or(0);
                    ui.label(if priced==0{"Dollar estimate unavailable — configure prices and use a provider that reports token usage.".into()}else{format!("Estimated ${:.6} across {priced} priced calls ({} completed total)",h["usage"]["estimated_usd"].as_f64().unwrap_or(0.0),h["usage"]["completed_calls"])});
                    muted(
                        ui,
                        "Actual billed cost is unavailable. Missing usage or prices are not treated as free calls. Speech transcription runs locally and has no hosted transcription API charge here.",
                    );
                });
                for a in h["activity"].as_array().cloned().unwrap_or_default() {
                    card(ui, |ui| {
                        ui.strong(format!(
                            "{} · {}",
                            a["role"].as_str().unwrap_or(""),
                            a["model"].as_str().unwrap_or("")
                        ));
                        ui.label(format!(
                            "{} · {} · {}",
                            timestamp(&a["at"]),
                            if a["local"] == true {
                                "Local endpoint"
                            } else {
                                "Hosted endpoint"
                            },
                            a["status"].as_str().unwrap_or("")
                        ));
                        ui.label(format!(
                            "{} · {} bytes in / {} bytes out",
                            number(&a["elapsed_ms"], " ms"),
                            a["input_bytes"],
                            a["output_bytes"]
                        ));
                        ui.small(format!("Origin: {}", a["origin"].as_str().unwrap_or("")));
                        if let Some(cost) = a["estimated_usd"].as_f64() {
                            ui.small(format!("Estimated ${cost:.6} from your configured rates"));
                        } else {
                            muted(ui, "Cost unavailable");
                        }
                    });
                }
            }
            _ => {
                card(ui, |ui| {
                    ui.strong("Recovery & capacity");
                    ui.label(format!(
                        "{} unfinished summary jobs · {} failed · {} expired audio sources",
                        h["unfinished_jobs"], h["failed_jobs"], h["expired_sources"]
                    ));
                    if ui.button("Retry failed work").clicked() {
                        self.command("retry");
                    }
                    muted(
                        ui,
                        "Retries keep completed summary parts and still respect budgets. Expired audio cannot be restored.",
                    );
                });
                for j in h["job_queue"].as_array().cloned().unwrap_or_default() {
                    card(ui, |ui| {
                        ui.label(format!(
                            "Job #{} · {} · {}",
                            j["id"],
                            j["kind"].as_str().unwrap_or(""),
                            j["status"].as_str().unwrap_or("")
                        ));
                        ui.label(j["error"].as_str().unwrap_or(""));
                        if ui.button("Inspect job").clicked() {
                            self.inspect("job", j["id"].as_i64().unwrap(), None);
                        }
                    });
                }
                for source in h["pending_sources"].as_array().cloned().unwrap_or_default() {
                    card(ui, |ui| {
                        ui.label(format!(
                            "Audio #{} · {} · {}",
                            source["id"],
                            source["track"].as_str().unwrap_or(""),
                            source["status"].as_str().unwrap_or("")
                        ));
                        ui.label(source["error"].as_str().unwrap_or(""));
                        if ui.button("Inspect pending source").clicked() {
                            self.inspect("source", source["id"].as_i64().unwrap(), None);
                        }
                    });
                }
                for e in h["events"].as_array().cloned().unwrap_or_default() {
                    card(ui, |ui| {
                        muted(ui, timestamp(&e["at"]));
                        ui.label(e["text"].as_str().unwrap_or(""));
                    });
                }
                muted(
                    ui,
                    format!(
                        "Local data: {}",
                        h["data_directory"].as_str().unwrap_or("Unavailable")
                    ),
                );
                muted(
                    ui,
                    "Activity keeps the latest 1,000 provider attempts and durable daily totals. No prompts, images, response text or credentials are stored in the provider activity ledger.",
                );
            }
        }
    }
    fn permissions(&mut self, ui: &mut egui::Ui) {
        let p: Value = serde_json::from_str(self.status["permissions"].as_str().unwrap_or("{}"))
            .unwrap_or(Value::Null);
        card(ui, |ui| {
            ui.heading("Connect your Mac’s permissions");
            muted(
                ui,
                "Granting access never starts recording. You decide when to start.",
            );
            if self.status["synthetic"] == true {
                ui.colored_label(CORAL, "Synthetic mode: permission prompts are disabled.");
            }
            for (label, allowed, command) in [
                (
                    "Microphone",
                    p["microphone_permission"] == 3,
                    "permission_mic",
                ),
                (
                    "Screen Recording & system audio",
                    p["screen_permission"] == true,
                    "permission_screen",
                ),
            ] {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(if allowed { "●" } else { "○" }).color(if allowed {
                            MINT
                        } else {
                            CORAL
                        }),
                    );
                    ui.strong(label);
                    ui.label(if p.is_null() {
                        "Status unavailable"
                    } else if allowed {
                        "Allowed"
                    } else {
                        "Not available to this running app"
                    });
                    if ui
                        .button(if allowed {
                            "Check permission"
                        } else {
                            "Allow access…"
                        })
                        .clicked()
                    {
                        self.command(if allowed {
                            "permission_refresh"
                        } else {
                            command
                        });
                    }
                });
            }
            if ui.button("Refresh status — no permission prompt").clicked() {
                self.command("permission_refresh");
            }
            muted(
                ui,
                "If macOS already shows Allowed, quit and reopen Daybook first. A local rebuild may change its signing identity; an old enabled entry may not match this installed copy. This status reflects what the running capture helper can access.",
            );
            muted(
                ui,
                "If access was denied, the button opens the matching macOS Privacy & Security pane. Enable Daybook there; macOS may require quitting and reopening. Camera access is never requested.",
            );
        });
    }
    fn submit_settings(&mut self) {
        if self.settings_saving {
            return;
        }
        if let Err(e) = self
            .credentials
            .validate(&self.config.summary)
            .and_then(|_| self.config.validate())
        {
            self.error = e;
            return;
        }
        let settings = json!(&self.config);
        let (path, body) = if self.credentials.secret.is_empty() {
            ("/config", settings)
        } else {
            (
                "/config_with_key",
                json!({"config":settings, "api_key":self.credentials.secret.as_str()}),
            )
        };
        self.settings_saving = self.send(path, Some(body));
    }
    fn settings(&mut self, ui: &mut egui::Ui) {
        if self.settings_saving {
            ui.disable();
        }
        let dirty = self.settings_dirty();
        if self.updating() {
            self.update_card(ui, dirty);
            return;
        }
        let stopped = self.status["active"] != "true" && self.status["armed"] != "true";
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    stopped && dirty,
                    egui::Button::new(RichText::new("Save changes").strong()),
                )
                .clicked()
            {
                self.submit_settings();
            }
            if ui
                .add_enabled(dirty, egui::Button::new("Discard changes"))
                .clicked()
            {
                if let Ok(c) = serde_json::from_value::<config::Config>(self.saved_config.clone()) {
                    self.credentials.reset(&c.summary);
                    self.config = c;
                }
            }
            muted(
                ui,
                if !stopped {
                    "Stop recording before applying settings."
                } else if dirty {
                    "Unsaved changes — the running configuration is unchanged."
                } else {
                    "Saved configuration"
                },
            );
        });
        ui.horizontal_wrapped(|ui| {
            for (i, label) in [
                "Recording",
                "Understanding",
                "Storage & resources",
                "Permissions & app",
            ]
            .iter()
            .enumerate()
            {
                ui.selectable_value(&mut self.settings_tab, i, *label);
            }
        });
        match self.settings_tab {
            0 => {
                card(ui, |ui| {
                    ui.heading("Speech on this Mac");
                    field(
                        ui,
                        "Speechworks model file",
                        &mut self.config.model_path,
                        "Local GGUF file",
                    );
                    egui::ComboBox::from_label("Inference backend")
                        .selected_text(&self.config.inference_backend)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.config.inference_backend,
                                "auto".into(),
                                "Auto — GPU with CPU fallback",
                            );
                            ui.selectable_value(
                                &mut self.config.inference_backend,
                                "cpu".into(),
                                "CPU only",
                            );
                        });
                    let medium = std::path::Path::new(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../data/models/moonshine-streaming-medium-Q8_0.gguf"
                    ));
                    if medium.is_file()
                        && ui
                            .button("Use cached Streaming Medium (recommended)")
                            .clicked()
                    {
                        self.config.model_path = medium.to_string_lossy().into_owned();
                    }
                    let cached = std::path::Path::new(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../data/models/moonshine-streaming-small-Q8_0.gguf"
                    ));
                    if cached.is_file() && ui.button("Use cached Streaming Small").clicked() {
                        self.config.model_path = cached.to_string_lossy().into_owned();
                    }
                    muted(
                        ui,
                        "Small/Medium: revisable previews. Tiny/Base: final text after each bounded phrase. Auto uses GPU when available, with CPU fallback. Local voice activity detection filters silent input. Timing is per passage. Speaker labels identify the input track.",
                    );
                });
                card(ui, |ui| {
                    ui.heading("Remote audio source");
                    field(
                        ui,
                        "Meeting app bundle ID",
                        &mut self.config.meeting_bundle_id,
                        "Leave empty for system audio",
                    );
                    muted(
                        ui,
                        "An app ID filters meeting/system audio only. Screen observations still sample the first display. Me and remote are source tracks, not identified speakers.",
                    );
                });
                card(ui, |ui| {
                    ui.heading("Workday schedule");
                    ui.horizontal_wrapped(|ui| {
                        ui.label("From");
                        ui.add(
                            egui::DragValue::new(&mut self.config.work_start_hour)
                                .range(0..=23)
                                .suffix(":00"),
                        );
                        ui.label("to");
                        ui.add(
                            egui::DragValue::new(&mut self.config.work_end_hour)
                                .range(1..=24)
                                .suffix(":00"),
                        );
                        ui.checkbox(&mut self.config.weekdays_only, "Weekdays only");
                    });
                    muted(
                        ui,
                        "Hours use this Mac’s local time. Enable workday recording deliberately; Start recording works outside these hours. Lock/sleep suspend recording. Stop and Pause stay off after wake.",
                    );
                });
            }
            1 => {
                card(ui, |ui| {
                    ui.heading("Passive screen text");
                    egui::ComboBox::from_label("Screen checks")
                        .selected_text(if self.config.screen_mode == "off" {
                            "Off"
                        } else {
                            "Read on this Mac"
                        })
                        .show_ui(ui, |ui| {
                            for (k, label) in [("off", "Off"), ("ocr", "Read on this Mac")] {
                                ui.selectable_value(&mut self.config.screen_mode, k.into(), label);
                            }
                        });
                    ui.horizontal(|ui| {
                        ui.label("Check every");
                        ui.add(
                            egui::DragValue::new(&mut self.config.screen_seconds)
                                .range(5..=86400)
                                .suffix(" seconds"),
                        );
                    });
                    muted(
                        ui,
                        "Daybook keeps the latest 50 passive screenshots on this Mac. Each check keeps its text and screen positions. Manually saved screenshots stay in your journal.",
                    );
                    if ui.link("View captured screen text").clicked() {
                        self.navigate(5);
                    }
                });
                card(ui, |ui| {
                    ui.heading("Incremental summaries");
                    minutes(
                        ui,
                        "Summarize new material every",
                        &mut self.config.compression_seconds,
                        1.0,
                    );
                    muted(
                        ui,
                        "An empty summary model uses local extractive excerpts. Model summaries and consolidation retain references; they never replace original evidence.",
                    );
                });
                if provider(
                    ui,
                    "Summary provider",
                    &mut self.config.summary,
                    &mut self.credentials,
                    stopped,
                ) {
                    self.submit_settings();
                }
                card(ui, |ui| {
                    ui.collapsing("Import a summary provider from OpenCode",|ui|{muted(ui,"JSON/JSONC metadata only. No credentials, OAuth, file/environment expansion or effective-project merging is imported.");ui.add(egui::TextEdit::multiline(&mut self.import).desired_rows(5).desired_width(f32::INFINITY));if ui.button("Import into draft").clicked(){self.send("/import",Some(json!({"jsonc":self.import})));}});
                });
            }
            2 => {
                card(ui, |ui| {
                    ui.heading("Keep what matters");
                    if ui
                        .selectable_label(
                            self.config.audio_storage == "memory",
                            "In-memory transcription — do not save audio (recommended)",
                        )
                        .clicked()
                    {
                        self.config.audio_storage = "memory".into();
                        self.config.retain_audio_hours = 0;
                    }
                    if ui
                        .selectable_label(
                            self.config.audio_storage == "recovery",
                            "Disk recovery buffer — preserve unfinished audio through a crash",
                        )
                        .clicked()
                    {
                        self.config.audio_storage = "recovery".into();
                    }
                    muted(
                        ui,
                        "Memory mode discards audio after final text. A crash loses unfinished audio and reports it on restart. Disk recovery is optional and creates temporary chunk files.",
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Short rolling media").clicked() {
                            self.config.retain_audio_hours = 0;
                            self.config.retain_images = false;
                            self.config.rolling_audio_seconds = 600;
                        }
                        if ui.button("Keep completed audio 24 hours").clicked() {
                            self.config.audio_storage = "recovery".into();
                            self.config.retain_audio_hours = 24;
                        }
                    });
                    minutes(
                        ui,
                        "Rolling audio window",
                        &mut self.config.rolling_audio_seconds,
                        0.5,
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Completed AAC audio (disk recovery only)");
                        ui.add_enabled(
                            self.config.audio_storage == "recovery",
                            egui::DragValue::new(&mut self.config.retain_audio_hours)
                                .range(0..=720)
                                .suffix(" hours"),
                        );
                    });
                    muted(
                        ui,
                        "Memory mode writes no audio files. In recovery mode, 0 hours uses only the rolling window. Expired unfinished audio is reported; text and summaries remain.",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Earlier retained periodic images");
                        ui.add(
                            egui::DragValue::new(&mut self.config.image_hours)
                                .range(0..=720)
                                .suffix(" hours"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Data disk cap");
                        ui.add(
                            egui::DragValue::new(&mut self.config.disk_limit_mb)
                                .range(64..=102400)
                                .suffix(" MiB"),
                        );
                    });
                    muted(
                        ui,
                        "The cap includes model files and pauses recording instead of deleting text. Disk checks can briefly overshoot with in-flight writes. AAC size depends on activity; retained images vary.",
                    );
                });
                card(ui, |ui| {
                    ui.heading("Resource guardrails");
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Pending audio blocks");
                        ui.add(egui::DragValue::new(&mut self.config.max_pending).range(2..=1000));
                        ui.label("Inference work budget");
                        ui.add(
                            egui::DragValue::new(&mut self.config.max_inference_work_mb)
                                .range(64..=1024)
                                .suffix(" MiB"),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Cooldown per 10 seconds of audio");
                        ui.add(
                            egui::DragValue::new(&mut self.config.inference_cooldown_ms)
                                .range(100..=10000)
                                .suffix(" ms"),
                        );
                    });
                    muted(
                        ui,
                        "One audio inference worker and one background provider/retention worker. These are bounded queues and soft targets, not hard whole-app RAM/CPU limits. Model or provider failures stay visible for retry.",
                    );
                });
                card(ui, |ui| {
                    ui.heading("Daily model request budgets");
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Requests");
                        ui.add(
                            egui::DragValue::new(&mut self.config.daily_requests).range(1..=10000),
                        );
                        ui.label("Images");
                        ui.add(
                            egui::DragValue::new(&mut self.config.daily_images).range(0..=10000),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Reserved token units");
                        ui.add(egui::DragValue::new(
                            &mut self.config.daily_token_reservation,
                        ));
                        ui.label("Max output tokens");
                        ui.add(
                            egui::DragValue::new(&mut self.config.max_output_tokens)
                                .range(1..=8192),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Summary length warning");
                        ui.add(
                            egui::DragValue::new(&mut self.config.summary_warn_chars)
                                .suffix(" characters"),
                        );
                    });
                    muted(
                        ui,
                        "Failed attempts consume reservations. Dollar figures require your model prices and provider-reported usage, and remain estimates. These request/image/unit budgets are the enforced safeguards.",
                    );
                });
            }
            _ => {
                self.permissions(ui);
                card(ui, |ui| {
                    ui.heading("Daybook · native local memory");
                    muted(
                        ui,
                        "Closing or minimizing controls keeps the menu-bar recorder running. Quit stops all Daybook processes. A new launch always starts stopped; no login item is installed.",
                    );
                    muted(
                        ui,
                        "Speechworks GitHub pin: 8d2690a. The active source checkout is untouched. Native Apple framework bindings and egui controls; no browser or webview.",
                    );
                });
                self.update_card(ui, dirty);
            }
        }
    }
    fn settings_dirty(&self) -> bool {
        self.loaded
            && (self.settings_saving
                || !self.credentials.secret.is_empty()
                || serde_json::to_value(&self.config).ok().as_ref() != Some(&self.saved_config))
    }
    fn updating(&self) -> bool {
        self.update_pending || self.status["update"]["busy"] == true
    }
    fn start_update(&mut self) {
        self.update_open = true;
        if !self.loaded
            || self.updating()
            || self.settings_dirty()
            || self.status["update"]["available"] != true
        {
            return;
        }
        let view = json!({"page":self.tab,"settings":self.settings_tab,"health":self.health_tab,
            "filter":self.filter(),"focus":self.focus,"transcript_range":self.parallel.fixed,"transcript_seconds":self.parallel.seconds,"transcript_ruler":self.parallel.ruler,"paragraph_view":self.paragraph_view});
        if self.send("/update", Some(json!({"draft":self.note,"workspace":view}))) {
            self.error.clear();
            self.update_pending = true;
        }
    }
    fn restore_workspace(&mut self, view: &Value) {
        let Some(page) = view["page"].as_u64().filter(|p| *p < 6) else {
            return;
        };
        self.settings_tab = view["settings"].as_u64().unwrap_or(0).min(3) as usize;
        self.health_tab = view["health"].as_u64().unwrap_or(0).min(2) as usize;
        let f = &view["filter"];
        if matches!(page, 1 | 2) {
            if let Some(day) = f["day"]
                .as_str()
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            {
                self.day = day.to_string();
                self.day_edit = self.day.clone();
                self.kind = f["kind"].as_str().unwrap_or("all").into();
                self.source = f["source"].as_str().unwrap_or("all").into();
                self.query = f["query"].as_str().unwrap_or("").into();
                self.archive.restore_filter(f, &self.query);
            }
        }
        if page == 0 {
            self.parallel.ruler =
                serde_json::from_value::<transcript_range::Span>(view["transcript_ruler"].clone())
                    .ok()
                    .filter(|r| r.valid());
            self.paragraph_view = view["paragraph_view"] == true;
            if let Some(seconds) = view["transcript_seconds"].as_i64() {
                self.parallel.seconds =
                    seconds.clamp(1, transcript_range::MAX_TIME - transcript_range::MIN_TIME);
            }
            if let Ok(r) =
                serde_json::from_value::<transcript_range::Span>(view["transcript_range"].clone())
            {
                self.parallel.set_range(r);
            }
        }
        self.navigate(page as usize);
        self.focus = page == 0 && view["focus"] == true;
        self.notice = Some(("Daybook updated".into(), Instant::now()));
    }
    fn update_card(&mut self, ui: &mut egui::Ui, dirty: bool) {
        let update = self.status["update"].clone();
        let busy = self.updating();
        card(ui, |ui| {
            ui.heading("App updates");
            muted(ui, "Builds the code in your local Daybook checkout.");
            if let Some(at) = update["installed_build"]["built_at"].as_str() {
                muted(ui, format!("Installed build: {at}"));
            }
            ui.horizontal_wrapped(|ui| {
                if busy {
                    ui.spinner();
                }
                let message = if self.update_pending {
                    "Starting update…"
                } else {
                    update["message"]
                        .as_str()
                        .unwrap_or("Checking update availability…")
                };
                if ["failed", "interrupted"].contains(&update["phase"].as_str().unwrap_or("")) {
                    ui.colored_label(CORAL, message);
                } else {
                    ui.label(message);
                }
            });
            if ui
                .add_enabled(
                    self.loaded && !busy && !dirty && update["available"] == true,
                    egui::Button::new(if busy {
                        "Updating…"
                    } else {
                        "Update & Restart"
                    }),
                )
                .clicked()
            {
                self.start_update();
            }
            muted(
                ui,
                "Daybook finishes any queued audio, verifies the build, and reopens this page with recording stopped.",
            );
            if dirty {
                ui.colored_label(
                    CORAL,
                    "Save or discard your settings edits before updating.",
                );
            }
            if update["available"] != true {
                muted(
                    ui,
                    update["unavailable_reason"]
                        .as_str()
                        .unwrap_or("Update availability is not known yet."),
                );
            }
            ui.collapsing("Local build log", |ui| {
                let mut log = update["log_tail"].as_str().unwrap_or("No build output yet.").to_owned();
                ui.add(egui::TextEdit::multiline(&mut log).font(egui::TextStyle::Monospace).desired_rows(10).desired_width(f32::INFINITY).interactive(false));
                muted(ui, "Latest 8 KiB shown. Full local log is capped at 2 MiB in data/update/build.log.");
            });
        });
    }
    fn inspector(&mut self, ui: &mut egui::Ui) {
        if !self.history.is_empty() && ui.button("‹ Back").clicked() {
            if let Some((req, v)) = self.history.pop() {
                self.detail_request = req;
                self.detail = v;
                self.picture.clear();
            }
        }
        let v = self.detail.clone();
        if v.is_null() {
            ui.spinner();
            return;
        }
        let observation = self.detail_request["kind"] == "observation";
        let label = if observation {
            reading::entry_label(
                v["kind"].as_str().unwrap_or(""),
                v["source"].as_str().unwrap_or(""),
            )
        } else {
            match v["kind"].as_str() {
                Some("source") if v["track"] == "me" => "Microphone audio",
                Some("source") => "System audio",
                Some("part") => "Summary passage",
                Some("meta") => "Daily brief",
                _ => "Summary",
            }
        };
        ui.heading(label);
        if v["at"].is_number() {
            let mut when = full_timestamp(&v["at"]);
            if v["kind"] == "source"
                || v["sources"]
                    .as_array()
                    .is_some_and(|parts| !parts.is_empty())
            {
                let parts = v["sources"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_else(|| std::slice::from_ref(&v));
                let duration = audio_duration(&v, parts);
                if !duration.is_empty() {
                    when.push_str(" · ");
                    when.push_str(&duration);
                }
            }
            muted(ui, when);
        }
        let saved_image = v["details"]["saved_image"] == true;
        if let Some(id) = v["image_id"].as_i64() {
            self.picture.show(ui, &self.base, id);
        } else if v["kind"] == "screen"
            && !matches!(
                v["source"].as_str(),
                Some("passive_screen" | "screen" | "vision")
            )
        {
            muted(ui, "The original image is unavailable.");
        }
        if observation
            && v["kind"] == "screen"
            && matches!(
                v["source"].as_str(),
                Some("passive_screen" | "screen" | "vision")
            )
            && let Some(id) = v["id"].as_i64()
            && ui.button("Open capture layout").clicked()
        {
            self.open_capture(id);
            return;
        }
        if v["source"] == "passive_screen" && v["end"].as_i64() > v["at"].as_i64() {
            muted(ui, format!("Last seen {}", full_timestamp(&v["end"])));
        }
        if let Some(ids) = v["details"]["image_ids"].as_array() {
            for id in ids.iter().filter_map(Value::as_i64) {
                if self.image_link(ui, id) {
                    self.inspect("observation", id, None);
                }
            }
        }
        if let Some(text) = v["text"].as_str().filter(|text| !text.trim().is_empty()) {
            let show_text = |ui: &mut egui::Ui| {
                if observation && ui.button("Copy text").clicked() {
                    ui.ctx().copy_text(text.to_owned());
                }
                ui.label(text);
            };
            if v["kind"] == "screen" && (v["image_id"].is_i64() || saved_image) {
                ui.push_id(("screen-text", &v["id"].as_i64()), |ui| {
                    ui.collapsing(
                        if v["source"] == "vision" {
                            "Screen description"
                        } else {
                            "Text from image"
                        },
                        show_text,
                    );
                });
            } else {
                show_text(ui);
            }
        }
        if let Some(error) = v["error"].as_str().filter(|v| !v.is_empty()) {
            ui.colored_label(CORAL, error);
        }
        if v["kind"] == "source" {
            if v["retained"] != true {
                muted(
                    ui,
                    if v["storage"] == "memory (not saved)" {
                        "Audio was kept only while transcribing."
                    } else {
                        "The recording is no longer saved."
                    },
                );
            }
            if v["retained"] == true && ui.button("Reveal audio in Finder").clicked() {
                self.send("/reveal_source", Some(json!({"id":v["id"]})));
            }
        }
        if saved_image {
            match v["details"]["ocr_status"].as_str() {
                Some("pending") => {
                    ui.label("Reading image text…");
                }
                Some("empty") => {
                    ui.label("No text found in this image.");
                }
                Some("failed") => {
                    ui.label("Daybook kept the image. Text recognition failed.");
                    if ui.button("Try reading text again").clicked() {
                        self.send("/retry_saved_ocr", Some(json!({"id":v["id"]})));
                    }
                }
                _ => {}
            }
        }
        let image_action = if saved_image {
            "Show saved PNG in Finder"
        } else {
            "Reveal retained screen image"
        };
        if v["image_retained"] == true && ui.button(image_action).clicked() {
            self.send("/reveal_image", Some(json!({"id":v["image_id"]})));
        }
        if v["sources"]
            .as_array()
            .is_some_and(|a| a.iter().any(|part| part["retained"] == true))
            && ui.button("Show saved audio in Finder").clicked()
        {
            self.send("/reveal_passage_audio", Some(json!({"id":v["id"]})));
        }
        if let Some(a) = v["days"].as_array() {
            muted(
                ui,
                format!(
                    "Source dates: {}",
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        if let Some(a) = v["parts"].as_array().filter(|a| !a.is_empty()) {
            ui.collapsing("How this summary was written", |ui| {
                self.inspection_navigation(ui, &v, "parts");
                for p in a {
                    if ui
                        .button(format!(
                            "Part {} · {}",
                            p["part"],
                            if p["complete"] == true {
                                "done"
                            } else {
                                "pending"
                            }
                        ))
                        .clicked()
                    {
                        self.inspect("part", v["id"].as_i64().unwrap(), p["part"].as_i64());
                    }
                }
            });
        }
        if let Some(a) = v["evidence"].as_array().filter(|a| !a.is_empty()) {
            ui.separator();
            ui.strong("Sources");
            self.inspection_navigation(ui, &v, "evidence");
            for e in a {
                let label = if e["kind"] == "summary" {
                    "Summary"
                } else {
                    reading::entry_label(
                        e["kind"].as_str().unwrap_or(""),
                        e["source"].as_str().unwrap_or(""),
                    )
                };
                let mut open = ui
                    .button(format!("{label} · {}", timestamp(&e["at"])))
                    .clicked();
                if e["kind"] == "screen" {
                    if let Some(id) = e["image_id"].as_i64() {
                        open |= self.image_link(ui, id);
                    } else {
                        muted(ui, "Original image unavailable");
                    }
                } else {
                    muted(ui, e["text"].as_str().unwrap_or(""));
                }
                if open {
                    self.inspect(
                        if e["kind"] == "summary" {
                            "job"
                        } else {
                            "observation"
                        },
                        e["id"].as_i64().unwrap(),
                        None,
                    );
                }
            }
        }
        if let Some(input) = v["input"].as_str() {
            ui.collapsing("Text used for this summary", |ui| {
                ui.label(input);
            });
        }
        if let Some(id) = v["details"]["observation_id"].as_i64() {
            if ui.button("Open original capture").clicked() {
                self.inspect("observation", id, None);
            }
        }
    }
    fn quit(&mut self) {
        self.send("/quit", self.loaded.then(|| json!({"text":self.note})));
    }
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .hint_text(hint),
    );
}
fn minutes(ui: &mut egui::Ui, label: &str, seconds: &mut u64, min: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut value = *seconds as f64 / 60.0;
        if ui
            .add(
                egui::DragValue::new(&mut value)
                    .range(min..=1440.0)
                    .speed(0.5)
                    .suffix(" min"),
            )
            .changed()
        {
            *seconds = (value * 60.0).round() as u64;
        }
    });
}
fn provider(
    ui: &mut egui::Ui,
    title: &str,
    p: &mut config::Provider,
    credentials: &mut credentials::Credentials,
    stopped: bool,
) -> bool {
    let mut save = false;
    card(ui, |ui| {
        ui.heading(title);
        field(ui, "Endpoint", &mut p.endpoint, "http://127.0.0.1:8080/v1");
        field(
            ui,
            "Model name",
            &mut p.model,
            "Leave empty for local extractive summaries",
        );
        credentials.show(ui, p);
        if !credentials.secret.is_empty() {
            save = ui
                .add_enabled(stopped, egui::Button::new("Save key and settings"))
                .clicked();
        }
        ui.checkbox(
            &mut p.remote_opt_in,
            "Explicitly allow this tool to use a hosted endpoint",
        );
        if !p.local() {
            ui.colored_label(
                CORAL,
                "Hosted summaries send selected evidence text to this endpoint.",
            );
        } else {
            muted(
                ui,
                "Loopback endpoint on this Mac. No remote opt-in is needed.",
            );
        }
        ui.collapsing("Optional cost estimate",|ui|{let mut enabled=p.input_usd_per_million.is_some()&&p.output_usd_per_million.is_some();if ui.checkbox(&mut enabled,"Use my supplied USD/token prices").changed(){p.input_usd_per_million=enabled.then_some(0.0);p.output_usd_per_million=enabled.then_some(0.0);}if enabled{ui.horizontal_wrapped(|ui|{ui.label("Input $/million");ui.add(egui::DragValue::new(p.input_usd_per_million.as_mut().unwrap()).range(0.0..=10000.0).speed(0.1));ui.label("Output $/million");ui.add(egui::DragValue::new(p.output_usd_per_million.as_mut().unwrap()).range(0.0..=10000.0).speed(0.1));});}muted(ui,"Enter your actual model rates; 0 means free. Prices are not fetched. Estimates require reported usage and do not represent your invoice, cache discounts or all provider fees.");});
    });
    save
}
impl eframe::App for Controls {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if let Some(monitor) = &self.paste_monitor {
            monitor.enabled.set(
                self.loaded
                    && self.status["synthetic"] != true
                    && (self.note_open || self.tab <= 2),
            );
            if let Some(version) = monitor.pending.take() {
                self.begin_paste(version);
            }
        }
        let fixture_api = self.preview_output.is_some() && !self.base.starts_with("synthetic://");
        if fixture_api {
            self.poll();
            self.picture.poll(ctx);
        }
        if let Some(path) = &self.preview_output {
            self.preview_frames += 1;
            if self.preview_motion {
                let t = ctx.input(|i| i.time);
                let at = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                // Explicit synthetic design fixture, never microphone or system audio.
                for (i, track) in ["me", "remote"].into_iter().enumerate() {
                    let db = preview::sample_db(t, i);
                    self.status["health"]["input_levels"][track] =
                        json!({"end_at":at,"rms_dbfs":db,"peak_dbfs":db+6.0});
                }
            }
            if self.preview_frames % 10 == 4
                && (!fixture_api
                    || if self.tab == 5 {
                        self.screen_history.ready()
                    } else {
                        self.parallel.data.is_some()
                    })
                && (!self.preview_motion || self.preview_timing.len() >= 180)
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    match image::save_buffer(
                        path,
                        &rgba,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    ) {
                        Ok(()) => println!("Saved synthetic Daybook UI render: {}", path.display()),
                        Err(e) => eprintln!("Preview render failed: {e}"),
                    }
                    if self.preview_motion {
                        let mut gaps = self.preview_timing[10..]
                            .windows(2)
                            .map(|w| (w[1].0 - w[0].0) * 1000.0)
                            .collect::<Vec<_>>();
                        let mut cpu = self.preview_timing[10..]
                            .iter()
                            .filter_map(|(_, cpu)| cpu.map(|cpu| cpu as f64 * 1000.0))
                            .collect::<Vec<_>>();
                        gaps.sort_by(f64::total_cmp);
                        cpu.sort_by(f64::total_cmp);
                        let timing = json!({"frames":self.preview_timing.len(),"frame_interval_median_ms":gaps[gaps.len()/2],"frame_interval_p95_ms":gaps[gaps.len()*95/100],"ui_cpu_median_ms":cpu.get(cpu.len()/2),"ui_cpu_p95_ms":cpu.get(cpu.len()*95/100),"scope":"synthetic animated HUD in native window; eframe CPU time and frame intervals, excludes live capture and photon/display latency"});
                        let _ =
                            std::fs::write(path.with_extension("timing.json"), timing.to_string());
                        println!("SYNTHETIC_UI_TIMING {timing}");
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            if self.preview_motion {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        }
        self.poll();
        if let Some(text) = self.parallel.clipboard.take() {
            ctx.copy_text(text);
        }
        self.picture.poll(ctx);
        self.archive.flush_clipboard(ctx);
        let hidden = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        self.minimized
            .store(hidden, std::sync::atomic::Ordering::Relaxed);
        self.detailed_poll.store(
            self.tab != 0 || self.update_open,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.meters
            .visibility(!hidden && self.tab == 0 && !self.focus);
        if self
            .perf_probe
            .as_ref()
            .is_some_and(perf::Probe::finish_if_due)
        {
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if let Some((v, _)) = self
            .meters
            .snapshot()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(1))
        {
            for key in ["active", "armed", "capture_starting", "capture_stopping"] {
                self.status[key] = v[key].clone();
            }
        }
        if self
            .note_dirty
            .is_some_and(|at| at.elapsed() > Duration::from_millis(750))
        {
            if self.send("/draft", Some(json!({"text":self.note}))) {
                self.note_dirty = None;
            }
        }
        if self.note_open && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
            self.save_note();
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        if self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.workspace(ui);
        if self.preview_motion && self.preview_timing.len() < 180 {
            let time = ui.input(|i| i.time);
            if self
                .preview_timing
                .last()
                .is_none_or(|(last, _)| *last != time)
            {
                self.preview_timing.push((time, frame.info().cpu_usage));
            }
        }
    }
    fn on_exit(&mut self) {
        if self.loaded && !self.quitting && self.preview_output.is_none() {
            let _ = call(&self.base, "/draft", Some(&json!({"text":self.note})));
        }
    }
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--design-preview") {
        if let Some(path) = args.get(2) {
            preview::run(path.into(), &args[3..]);
        }
        return;
    }
    let Some(base) = std::env::args().nth(1) else {
        return;
    };
    if !base.starts_with("http://127.0.0.1:") {
        return;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Daybook — your local workday")
            .with_inner_size([1260.0, 880.0])
            .with_min_inner_size([780.0, 590.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    let perf_path = args
        .iter()
        .position(|a| a == "--profile-ui")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    if let Err(e) = eframe::run_native(
        "Daybook",
        options,
        Box::new(move |cc| {
            let mut c = Controls::new(cc, base);
            c.perf_probe = perf_path.map(perf::Probe::new);
            Ok(Box::new(c))
        }),
    ) {
        eprintln!("Daybook controls: {e}");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_does_not_invent_recording() {
        assert_eq!(
            status_title(
                &json!({"active":"false","status":"error: audio consumer unavailable"}),
                true
            )
            .0,
            "Recording interrupted"
        );
        assert_eq!(
            status_title(&json!({"active":"false","status":"capacity paused"}), true).0,
            "Recording interrupted"
        );
        assert_eq!(
            status_title(
                &json!({"active":"true","capture_warning":"Some audio was lost"}),
                true
            )
            .0,
            "Recording"
        );
        assert_eq!(
            status_title(
                &json!({"active":"false","armed":"true","capture_starting":"true"}),
                true
            )
            .0,
            "Starting audio"
        );
        assert_eq!(
            status_title(&json!({"active":"false","capture_stopping":"true"}), true).0,
            "Stopping audio"
        );
        assert_eq!(
            status_title(&json!({"active":"true"}), false).0,
            "Recorder unavailable"
        );
        assert_eq!(
            status_title(&json!({"active":"false","armed":"true"}), true).0,
            "Workday recording enabled"
        );
        assert_eq!(
            status_title(&json!({"active":"false","armed":"false"}), true).0,
            "Stopped"
        );
    }
}
#[cfg(test)]
mod interaction_tests {
    #[test]
    fn pasting_into_a_draft_waits_for_its_own_original_and_preserves_the_caption() {
        let (mut c, reply, requests) = controls();
        c.open_note();
        c.note = "Cover proof".into();
        c.begin_paste(12);
        let sent = requests.try_recv().unwrap();
        assert_eq!(sent.path, "/paste_image");
        assert_eq!(sent.body.as_ref().unwrap()["attach"], true);
        assert!(!c.can_save_note());
        reply
            .send(Reply {
                path: sent.path,
                body: sent.body,
                result: Ok(json!({"ticket":7})),
            })
            .unwrap();
        c.poll();
        c.pasted(&json!({"ticket":6,"image_id":9}));
        assert!(c.draft_images.is_empty());
        c.pasted(&json!({"ticket":7,"image_id":10}));
        assert_eq!(c.draft_images, [10]);
        assert_eq!(c.note, "Cover proof");
        assert!(c.can_save_note());
        c.save_note();
        assert!(
            requests
                .try_iter()
                .any(|r| r.path == "/note" && r.body.unwrap()["include_images"] == true)
        );
    }
    #[test]
    fn quit_keeps_ui_responsive_and_waits_for_background_draft_save() {
        let (mut c, reply, requests) = controls();
        c.loaded = true;
        c.note = "Keep this draft".into();
        c.quit();
        let r = requests.try_recv().unwrap();
        assert_eq!(r.path, "/quit");
        assert_eq!(r.body.unwrap()["text"], "Keep this draft");
        assert!(!c.quitting);
        reply
            .send(Reply {
                path: "/quit".into(),
                body: None,
                result: Err("Save failed".into()),
            })
            .unwrap();
        c.poll();
        assert!(!c.quitting);
        assert_eq!(c.note, "Keep this draft");
        reply
            .send(Reply {
                path: "/quit".into(),
                body: None,
                result: Ok(json!({})),
            })
            .unwrap();
        c.poll();
        assert!(c.quitting);
    }
    #[test]
    #[ignore = "manual release-mode CPU layout benchmark; no capture or network"]
    fn reader_frame_benchmark() {
        let (tx, _) = mpsc::sync_channel(32);
        let (_, rx) = mpsc::sync_channel(32);
        let mut c = preview::fixture(tx, rx);
        let ctx = egui::Context::default();
        design::setup(&ctx);
        for count in [5, 80] {
            let text = "A synthetic passage with decisions and follow-up actions. ".repeat(70);
            let mut page = c.stream_page.clone();
            page["items"] = json!((0..count).map(|i| json!({"id":i,"at":100+i*30,"end":120+i*30,"source":if i%2==0{"me"}else{"remote"},"kind":"transcript","text":text})).collect::<Vec<_>>());
            c.set_stream_page(page);
            c.jump_to_live = true;
            let mut samples = Vec::new();
            for frame in 0..100 {
                let began = Instant::now();
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1080.0, 700.0),
                        )),
                        time: Some(frame as f64 / 60.0),
                        ..Default::default()
                    },
                    |ui| c.stream(ui),
                );
                if frame >= 10 {
                    samples.push(began.elapsed().as_secs_f64() * 1000.0);
                }
                output.drop_without_applying_deltas();
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "READER_CPU rows={count} median_ms={:.3} p95_ms={:.3}",
                samples[45], samples[85]
            );
        }
    }
    use super::*;
    #[test]
    fn startup_and_jump_to_live_paint_the_latest_transcript_inside_the_reader() {
        let (tx, _) = mpsc::sync_channel(32);
        let (_, rx) = mpsc::sync_channel(32);
        let mut c = preview::fixture(tx, rx);
        // Exercise the actual startup state, including a page that overflows the reader.
        c.jump_to_live = true;
        let ctx = egui::Context::default();
        design::setup(&ctx);
        for jump in [true, false, true] {
            c.jump_to_live = jump;
            let mut visible = false;
            for _ in 0..5 {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(680.0, 620.0),
                        )),
                        ..Default::default()
                    },
                    |ui| c.stream(ui),
                );
                visible = output.shapes.iter().any(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape {
                        text.galley.job.text.contains("stone steps")
                            && shape
                                .clip_rect
                                .intersects(text.galley.rect.translate(text.pos.to_vec2()))
                    } else {
                        false
                    }
                });
                output.drop_without_applying_deltas();
            }
            assert!(
                visible,
                "latest words must be painted inside the visible reader after startup/jump"
            );
        }
    }
    fn controls() -> (Controls, SyncSender<Reply>, Receiver<Request>) {
        let (tx, req) = mpsc::sync_channel(32);
        let (reply, rx) = mpsc::sync_channel(32);
        (
            Controls::initial("http://127.0.0.1:9/test".into(), tx, rx),
            reply,
            req,
        )
    }
    #[test]
    fn interruption_and_gap_reason_are_visible_in_the_header() {
        let (mut c, _, _) = controls();
        let ctx = egui::Context::default();
        design::setup(&ctx);
        for active in [false, true] {
            c.last_seen = Some(Instant::now());
            c.status = json!({"active":active.to_string(),"armed":active.to_string(),"status":if active {"recording"} else {"error: consumer disconnected"},"capture_warning":if active {"Audio gap detected: 0.500 s lost. Recording continues."} else {""},"health":{"at":chrono::Utc::now().timestamp()}});
            let mut text = String::new();
            for _ in 0..3 {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(980.0, 500.0),
                        )),
                        ..Default::default()
                    },
                    |ui| c.header(ui),
                );
                for shape in &output.shapes {
                    if let egui::Shape::Text(t) = &shape.shape {
                        if shape
                            .clip_rect
                            .intersects(t.galley.rect.translate(t.pos.to_vec2()))
                        {
                            text.push_str(&t.galley.job.text);
                        }
                    }
                }
                output.drop_without_applying_deltas();
            }
            if active {
                assert!(text.contains("0.500 s lost"));
                assert!(text.contains("Stop recording"));
            } else {
                assert!(text.contains("Recording interrupted"));
                assert!(text.contains("consumer disconnected"));
            }
        }
    }
    #[test]
    fn inspection_pages_keep_independent_cursors_and_ignore_stale_replies() {
        let (mut c, reply, requests) = controls();
        c.detail_request = json!({"kind":"job","id":206,"parts_after":99,"evidence_after":-1});
        c.detail = json!({"id":206,"parts":[{"part":100}]});
        let stale = c.detail_request.clone();
        c.inspect_page("evidence", 100);
        let request = requests.try_recv().unwrap();
        assert_eq!(request.body.as_ref().unwrap()["parts_after"], 99);
        assert_eq!(request.body.as_ref().unwrap()["evidence_after"], 100);
        reply
            .send(Reply {
                path: "/inspect".into(),
                body: Some(stale),
                result: Ok(json!({"id":206,"stale":true})),
            })
            .unwrap();
        c.poll();
        assert!(c.detail["stale"].is_null());
        reply
            .send(Reply {
                path: "/inspect".into(),
                body: request.body,
                result: Ok(json!({"id":206,"parts":[{"part":100}],"evidence":[{"id":101}]})),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.detail["evidence"][0]["id"], 101);
        c.inspect("observation", 101, None);
        assert_eq!(c.history.last().unwrap().0["evidence_after"], 100);
        assert_eq!(c.history.last().unwrap().1["parts"][0]["part"], 100);
    }
    #[test]
    fn failed_note_and_stale_response_preserve_user_edits() {
        let (mut c, reply, requests) = controls();
        c.loaded = true;
        c.note = "keep this thought".into();
        reply
            .send(Reply {
                path: "/note".into(),
                body: Some(json!({"text":c.note})),
                result: Err("offline".into()),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.note, "keep this thought");
        c.note = "new edits".into();
        reply
            .send(Reply {
                path: "/note".into(),
                body: Some(json!({"text":"old version"})),
                result: Ok(json!({"ok":true})),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.note, "new edits");
        reply
            .send(Reply {
                path: "/note".into(),
                body: Some(json!({"text":"new edits"})),
                result: Ok(json!({"ok":true})),
            })
            .unwrap();
        c.poll();
        assert!(c.note.is_empty());
        assert!(
            requests
                .try_iter()
                .any(|r| r.path == "/draft" && r.body.unwrap()["text"] == "")
        );
    }
    #[test]
    fn polling_does_not_replace_unsaved_settings_or_selected_day() {
        let (mut c, reply, _) = controls();
        c.loaded = true;
        c.config.model_path = "my draft model".into();
        reply
            .send(Reply {
                path: "/overview".into(),
                body: None,
                result: Ok(json!({"config":config::Config::default()})),
            })
            .unwrap();
        reply
            .send(Reply {
                path: "/timeline".into(),
                body: Some(json!({"day":"1999-01-01","kind":"all","source":"all","query":""})),
                result: Ok(json!({"items":[{"text":"stale day"}]})),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.config.model_path, "my draft model");
        assert!(c.items.is_empty());
    }
    #[test]
    fn live_updates_preserve_history_settings_and_health_metrics() {
        let (mut c, reply, _) = controls();
        c.loaded = true;
        c.config.model_path = "unsaved choice".into();
        c.items = vec![json!({"text":"Selected history"})];
        c.status = json!({"config":{"audio_storage":"memory"},"health":{"backend":{"peak_rss_bytes":123}}});
        reply.send(Reply {
            path: "/live".into(), body: None,
            result: Ok(json!({"active":"true","armed":"true","status":"recording","pending":2,"live_transcription":[{"text":"Words arriving"}],"memory_audio":{"pcm_bytes":64000}})),
        }).unwrap();
        c.poll();
        assert_eq!(
            c.status["health"]["live_transcription"][0]["text"],
            "Words arriving"
        );
        assert_eq!(c.status["health"]["backend"]["peak_rss_bytes"], 123);
        assert_eq!(c.status["memory_audio"]["pcm_bytes"], 64000);
        assert_eq!(c.status["config"]["audio_storage"], "memory");
        assert_eq!(c.config.model_path, "unsaved choice");
        assert_eq!(c.items[0]["text"], "Selected history");
    }
    #[test]
    fn transcript_accumulates_and_reading_history_survives_live_updates() {
        let (mut c, reply, requests) = controls();
        c.status = json!({});
        let day = chrono::Local::now().date_naive().to_string();
        let page = json!({"day":day,"items":[{"id":1,"text":"Earlier words"},{"id":2,"text":"New words","kind":"preview"}],"total":2});
        reply
            .send(Reply {
                path: "/live".into(),
                body: None,
                result: Ok(json!({"transcript":page})),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.stream_page["items"].as_array().unwrap().len(), 2);
        c.stream_request(json!({"before_at":10,"before_id":2}));
        let old = requests.try_recv().unwrap();
        reply
            .send(Reply {
                path: "/live".into(),
                body: None,
                result: Ok(json!({"transcript":{"items":[{"id":3,"text":"Latest"}]}})),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.stream_page["items"][0]["text"], "Earlier words");
        c.follow_stream();
        let latest = requests.try_recv().unwrap();
        reply
            .send(Reply {
                path: "/transcript".into(),
                body: old.body,
                result: Ok(json!({"items":[{"id":0,"text":"stale history response"}]})),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.stream_page["items"][0]["text"], "Earlier words");
        reply
            .send(Reply {
                path: "/transcript".into(),
                body: latest.body,
                result: Ok(
                    json!({"items":[{"id":2,"text":"Final revised words","kind":"transcript"}]}),
                ),
            })
            .unwrap();
        c.poll();
        assert_eq!(c.stream_page["items"][0]["kind"], "transcript");
        assert!(c.follow_live && c.jump_to_live);
    }
    #[test]
    fn scrolling_back_does_not_freeze_new_text_and_start_returns_to_live() {
        let (mut c, reply, requests) = controls();
        c.status = json!({});
        c.follow_live = false;
        reply.send(Reply {path:"/live".into(),body:None,result:Ok(json!({"capture_starting":"true","capture_transition":"true","input_levels":{"me":{"peak_dbfs":-12}},"transcript":{"items":[{"id":9,"text":"Still arriving"}]}}))}).unwrap();
        c.poll();
        assert_eq!(c.stream_page["items"][0]["text"], "Still arriving");
        assert!(!c.follow_live);
        assert_eq!(c.status["capture_starting"], "true");
        assert_eq!(c.status["health"]["input_levels"]["me"]["peak_dbfs"], -12);
        c.stream_request(json!({"before_id":9,"before_at":100}));
        c.tab = 3;
        c.command("start");
        assert!(c.follow_live && c.jump_to_live && c.stream_request.is_null());
        assert_eq!(c.tab, 0);
        assert!(
            requests
                .try_iter()
                .any(|r| r.path == "/command" && r.body.unwrap()["command"] == "start")
        );
    }
    #[test]
    fn all_native_views_layout_headlessly_without_live_io() {
        let (mut c, _, _) = controls();
        c.loaded = true;
        c.saved_config = serde_json::to_value(&c.config).unwrap();
        c.status = json!({"active":"false","armed":"false","status":"stopped","synthetic":true,"health":{"tracks":[],"native":null,"backend":{},"usage":{},"budget":{}}});
        c.last_seen = Some(Instant::now());
        let ctx = egui::Context::default();
        design::setup(&ctx);
        let rendered=ctx.run_ui(Default::default(),|ui| {
            c.header(ui);
            c.timeline(ui);
            c.brief(ui);
            for i in 0..3 {
                c.health_tab = i;
                c.health(ui);
            }
            for i in 0..4 {
                c.settings_tab = i;
                c.settings(ui);
            }
            c.status["update"] = json!({"busy":true,"available":true,"phase":"building","message":"Building local changes","log_tail":"Synthetic build output"});
            c.settings(ui);
            c.status["update"] = json!({"busy":false,"available":true,"phase":"failed","message":"Synthetic build failed; current app unchanged"});
            c.update_card(ui, true);
            c.detail = json!({"id":1,"kind":"transcript","text":"A synthetic decision","details":{},"sources":[]});
            c.inspector(ui);
        });
        rendered.drop_without_applying_deltas();
    }
}
