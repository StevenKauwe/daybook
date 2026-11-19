mod archive_image;
mod config;
mod inspect;
mod live_audio;
mod passive;
mod provider;
mod reading;
mod realtime;
mod recovery;
mod saved;
mod screen_sample;
mod speech_file;
mod store;
mod streaming;
mod update;
mod voice;
mod worker;
use config::Config;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use store::{Result, Store, now};
static EXIT: AtomicBool = AtomicBool::new(false);
extern "C" fn signal(_: i32) {
    EXIT.store(true, Ordering::Relaxed);
}
struct Bridge {
    child: Child,
    input: ChildStdin,
}
impl Bridge {
    fn send(&mut self, s: &str) {
        let _ = writeln!(self.input, "{s}");
        let _ = self.input.flush();
    }
    fn stop(&mut self) {
        self.send("quit");
        for _ in 0..200 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn native_helper() -> Result<PathBuf> {
    let sibling = std::env::current_exe()?
        .parent()
        .ok_or("Missing executable folder")?
        .join("daybook-native");
    Ok(if sibling.exists() {
        sibling
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("native/daybook-native")
    })
}
fn bridge(
    root: &Path,
    url: &str,
    tx: mpsc::SyncSender<Value>,
    synthetic: bool,
    show_ui: bool,
    meters: realtime::Shared,
) -> Result<Bridge> {
    let mut child = Command::new(native_helper()?)
        .args([
            root.to_str().ok_or("invalid data path")?,
            root.join("config.json").to_str().unwrap(),
            url,
        ])
        .env("DAYBOOK_SYNTHETIC", if synthetic { "1" } else { "0" })
        .env("DAYBOOK_SHOW_UI", if show_ui { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            if line.len() > 256 * 1024 {
                let _=tx.send(json!({"type":"error","message":"Native event exceeded 256 KiB; source ingestion rejected","bridge_pid":pid}));
                continue;
            }
            if let Ok(mut v) = serde_json::from_str::<Value>(&line) {
                meters.lock().unwrap().update(&v);
                if v["type"] == "meters" || v["type"] == "meter_health" {
                    continue;
                }
                v["bridge_pid"] = json!(pid);
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
        meters
            .lock()
            .unwrap()
            .update(&json!({"type":"bridge_exit"}));
        let _ = tx.send(json!({"type":"bridge_exit","bridge_pid":pid}));
    });
    Ok(Bridge { child, input })
}
fn write_config(root: &Path, c: &Config) -> Result<()> {
    c.validate()?;
    let p = root.join("config.tmp");
    let mut f = std::fs::File::create(&p)?;
    f.write_all(&serde_json::to_vec_pretty(c)?)?;
    f.sync_all()?;
    std::fs::rename(p, root.join("config.json"))?;
    Ok(())
}
fn ingest(s: &Store, c: &Config, v: &Value, inbox: &live_audio::Shared) -> Result<()> {
    match v["type"].as_str().unwrap_or("") {
        "state" => {
            if v["starting"] == true && s.get("capture_starting") != "true" {
                s.state("capture_warning", "")?;
                for track in ["me", "remote"] {
                    s.state(&format!("input_level_{track}"), "")?;
                }
            }
            s.state(
                "capture_starting",
                if v["starting"] == true {
                    "true"
                } else {
                    "false"
                },
            )?;
            s.state(
                "capture_stopping",
                if v["stopping"] == true {
                    "true"
                } else {
                    "false"
                },
            )?;
            s.state(
                "capture_transition",
                if v["stopping"] == true || v["starting"] == true {
                    "true"
                } else {
                    "false"
                },
            )?;
            s.state("status", v["state"].as_str().unwrap_or("unknown"))?;
            s.state("armed", if v["armed"] == true { "true" } else { "false" })?;
            s.state("active", if v["active"] == true { "true" } else { "false" })?;
        }
        "metrics" => {
            s.state("native_metrics", &v.to_string())?;
            s.state("native_seen_at", &now().to_string())?;
        }
        "capture_drained" => {
            s.state("capture_transition", "false")?;
            s.state("capture_stopping", "false")?;
        }
        "audio" => {
            let path = v["path"].as_str().ok_or("audio path missing")?;
            if !worker::valid_relative(path, "audio") && !live_audio::valid_path(path) {
                return Err("unsafe source path".into());
            }
            let track = v["track"].as_str().ok_or("track missing")?;
            if !["me", "remote"].contains(&track) {
                return Err("invalid track".into());
            }
            let mut metadata = v.clone();
            if live_audio::valid_path(path) {
                use base64::Engine;
                let encoded = v["pcm_base64"]
                    .as_str()
                    .ok_or("memory audio payload missing")?;
                if encoded.len() > 43000 {
                    return Err("memory audio payload exceeds limit".into());
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                if bytes.len() / 4 != v["frames"].as_u64().unwrap_or(0) as usize {
                    return Err("PCM frame count mismatch".into());
                }
                metadata.as_object_mut().unwrap().remove("pcm_base64");
                let admission = inbox.lock().unwrap().insert(path, bytes, c.max_pending)?;
                if admission == live_audio::Admission::Full {
                    let exists: bool = s.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sources WHERE path=?1)",
                        [path],
                        |r| r.get(0),
                    )?;
                    if exists {
                        return Ok(());
                    }
                    let dropped = s.get("memory_rejected_frames").parse::<u64>().unwrap_or(0)
                        + v["frames"].as_u64().unwrap_or(0);
                    s.state("memory_rejected_frames", &dropped.to_string())?;
                    let key = format!("memory_loss_generation_{track}");
                    let generation = s.get(&key).parse::<u64>().unwrap_or(0) + 1;
                    s.state(&key, &generation.to_string())?;
                    metadata["loss_reason"] = json!(
                        "Transcription RAM queue full; this block was not retained. Capture continues."
                    );
                    s.admit_audio(&metadata)?;
                    let warning = format!(
                        "Transcription fell behind: {:.3} s of audio lost across both tracks. Recording continues as the queue drains. See Diagnostics for details.",
                        dropped as f64 / 16000.0
                    );
                    s.state("capture_warning", &warning)?;
                    if now() - s.get("memory_loss_event_at").parse::<i64>().unwrap_or(0) >= 10 {
                        let source_id: i64 =
                            s.db.query_row("SELECT id FROM sources WHERE path=?1", [path], |r| {
                                r.get(0)
                            })?;
                        s.observation(
                            now(),
                            now(),
                            "event",
                            "system",
                            &warning,
                            &metadata,
                            Some(source_id),
                        )?;
                        s.state("memory_loss_event_at", &now().to_string())?;
                    }
                    return Ok(());
                }
                let generation = s
                    .get(&format!("memory_loss_generation_{track}"))
                    .parse::<u64>()
                    .unwrap_or(0);
                if generation > 0 {
                    metadata["session"] = json!(format!(
                        "{}:ram:{generation}",
                        v["session"].as_str().unwrap_or("")
                    ));
                }
            }
            if let Err(e) = s.admit_audio(&metadata) {
                inbox.lock().unwrap().remove(path);
                return Err(e);
            }
            if let (Some(rms), Some(peak), Some(end)) = (
                v["level"]["rms_dbfs"].as_f64(),
                v["level"]["peak_dbfs"].as_f64(),
                v["end"].as_f64(),
            ) {
                if (-120.0..=0.0).contains(&rms) && (rms..=0.0).contains(&peak) && end.is_finite() {
                    s.state(&format!("input_level_{track}"), &json!({"rms_dbfs":rms,"peak_dbfs":peak,"end_at":end,"received_at":chrono::Utc::now().timestamp_millis() as f64/1000.0,"frames":v["frames"]}).to_string())?;
                }
            }
        }
        "screen" | "screen_unchanged" => {
            if c.screen_mode != "off" {
                passive::observe(s, v, c.screen_seconds)?;
            }
        }
        "audio_gap" => {
            if s.get("audio_gap") != v.to_string() {
                let seconds = |track: &str| {
                    v["losses"]
                        .as_array()
                        .and_then(|a| a.iter().find(|l| l["track"] == track))
                        .and_then(|l| l["audio_seconds"].as_f64())
                };
                let amount = |v: Option<f64>| {
                    v.map(|n| format!("{n:.3} s"))
                        .unwrap_or_else(|| "unknown duration".into())
                };
                let message = format!(
                    "Audio gap detected: microphone {}, system audio {} lost since capture helper launch. Recording continues. See Diagnostics for details.",
                    amount(seconds("me")),
                    amount(seconds("remote"))
                );
                s.observation(now(), now(), "event", "system", &message, v, None)?;
                s.state("capture_warning", &message)?;
                s.state("audio_gap", &v.to_string())?;
            }
        }
        "error" => s.event(v["message"].as_str().unwrap_or("Native capture error"))?,
        "ready" => {
            s.state("permissions", &v.to_string())?;
        }
        _ => {}
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Daybook: {e}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    let early: Vec<String> = std::env::args().collect();
    if early.get(1).is_some_and(|a| a == "--update-worker") {
        return update::run(
            Path::new(early.get(2).ok_or("Missing update data directory")?),
            early.get(3).ok_or("Missing update lock")?.parse()?,
        );
    }
    unsafe {
        libc::umask(0o077);
        libc::signal(libc::SIGINT, signal as *const () as usize);
        libc::signal(libc::SIGTERM, signal as *const () as usize);
    }
    let args: Vec<String> = std::env::args().collect();
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data");
    let root = args
        .windows(2)
        .find(|a| a[0] == "--data")
        .map(|a| PathBuf::from(&a[1]))
        .unwrap_or(default);
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(root)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(root.join("instance.lock"))?;
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let url = std::fs::read_to_string(root.join("control-url.txt"))?;
        if !url.starts_with("http://127.0.0.1:") {
            return Err("existing control URL is invalid".into());
        }
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .build()
            .post(&format!("{}/command", url.trim()))
            .send_json(json!({"command":"open"}))
            .map_err(|_| "existing Daybook instance could not be focused")?;
        return Ok(());
    }
    let mut resume_workspace =
        update::take_workspace(&root, args.iter().any(|s| s == "--after-update"));
    let s = Store::open(&root)?;
    let inbox: live_audio::Shared = Default::default();
    let meters: realtime::Shared = Default::default();
    let mut c = Config::load(&root.join("config.json"))?;
    write_config(&root, &c)?;
    s.recover()?;
    s.suppress_proven_silence()?;
    s.export()?;
    s.state("native_metrics", "")?;
    s.state("loaded_speech_model", "")?;
    s.state("native_seen_at", "")?;
    s.state("status", "stopped")?;
    s.state("active", "false")?;
    s.state("armed", "false")?;
    s.state("capture_transition", "false")?;
    s.state("capture_starting", "false")?;
    s.state("capture_stopping", "false")?;
    s.state("capture_warning", "")?;
    for track in ["me", "remote"] {
        s.state(&format!("input_level_{track}"), "")?;
    }
    s.event("Application opened stopped; explicit Start required after process restart")?;
    recovery::recover_audio(&s)?;
    let mut secret = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut secret)?;
    let token = secret
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let prefix = format!("/{token}");
    let server = tiny_http::Server::http("127.0.0.1:0")?;
    let addr = server.server_addr().to_ip().ok_or("no local address")?;
    let url = format!("http://{addr}{prefix}");
    std::fs::write(root.join("control-url.txt"), &url)?;
    println!("Daybook controls: {url}");
    let (tx, rx) = mpsc::sync_channel(64);
    let show_ui = !args.iter().any(|s| s == "--headless");
    let synthetic_ui = args.iter().any(|s| s == "--synthetic-ui");
    let synthetic = synthetic_ui || args.iter().any(|s| s == "--synthetic");
    let mut native = if synthetic && !synthetic_ui {
        None
    } else {
        Some(bridge(
            &root,
            &url,
            tx.clone(),
            synthetic,
            show_ui,
            meters.clone(),
        )?)
    };
    let alive = Arc::new(AtomicBool::new(true));
    let worker_alive = alive.clone();
    let worker_root = root.clone();
    let worker_inbox = inbox.clone();
    let _worker =
        std::thread::spawn(move || worker::run(worker_root, worker_alive, synthetic, worker_inbox));
    let mut sampler = inspect::Sampler::default();
    let mut last_clean = Instant::now();
    let mut last_compress = Instant::now();
    let mut update_blocked = false;
    let mut saves = saved::Saves::new(&s, native_helper()?)?;
    let images = archive_image::Images::new(root.clone())?;
    while !EXIT.load(Ordering::Relaxed) {
        let disk_limit = c.disk_limit_mb * 1024 * 1024;
        if let Some(result) = saves.poll(&s, disk_limit) {
            let message = result.unwrap_or_else(|e| format!("Save error · {e}"));
            let message: String = message
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect();
            s.state("notice", &message)?;
            if let Some(b) = &mut native {
                b.send(&format!("save_feedback {message}"));
            }
        }
        let blocked = update::busy(&root);
        if blocked != update_blocked {
            if let Some(b) = &mut native {
                b.send(if blocked {
                    "update_begin"
                } else {
                    "update_end"
                });
            }
            update_blocked = blocked;
        }
        for _ in 0..64 {
            let Ok(v) = rx.try_recv() else { break };
            if v["bridge_pid"].as_u64() != native.as_ref().map(|b| b.child.id() as u64) {
                continue;
            }
            if v["type"] == "quit" || v["type"] == "bridge_exit" {
                EXIT.store(true, Ordering::Relaxed);
                break;
            }
            if v["type"] == "save_request" {
                if !synthetic && !blocked {
                    let action = match v["source"].as_str() {
                        Some("clipboard") => match v["version"].as_i64() {
                            Some(version) => saved::Action::Clipboard(version),
                            None => continue,
                        },
                        Some("screenshot") => saved::Action::Screenshot,
                        _ => continue,
                    };
                    if let Err(e) = saves.start(&s, action, disk_limit) {
                        if let Some(b) = &mut native {
                            b.send(&format!("save_feedback {e}"));
                        }
                    }
                } else if let Some(b) = &mut native {
                    b.send("save_feedback Saving is unavailable during this session");
                }
                continue;
            }
            if v["type"] == "save_cancel" {
                saves.cancel();
                continue;
            }
            if let Err(e) = ingest(&s, &c, &v, &inbox) {
                s.event(&format!("Capture ingestion error: {e}"))?;
                if let Some(b) = &mut native {
                    if matches!(v["type"].as_str(), Some("screen_unchanged" | "screen")) {
                        b.send("refresh_screen");
                    } else {
                        b.send("pause");
                    }
                }
            }
        }
        if last_clean.elapsed() > Duration::from_secs(5) {
            worker::cleanup(&s, &c)?;
            inbox.lock().unwrap().reap(&s)?;
            if let Some(b) = &mut native {
                b.send(if s.pending() > 4 || worker::memory_pressure() {
                    "defer_screens"
                } else {
                    "allow_screens"
                });
            }
            let bytes = store::disk_bytes(&root);
            if (c.audio_storage != "memory" && s.pending() >= c.max_pending)
                || bytes >= c.disk_limit_mb * 1024 * 1024
            {
                if s.get("active") == "true" || s.get("armed") == "true" {
                    if let Some(b) = &mut native {
                        b.send("pause")
                    }
                    s.state("active", "false")?;
                    s.state("armed", "false")?;
                    s.state("status", "capacity paused")?;
                    s.event("CAPACITY: recording paused; pending queue or disk limit reached. Review retention/backlog before restarting")?;
                }
            }
            last_clean = Instant::now();
        }
        if last_compress.elapsed() > Duration::from_secs(c.compression_seconds) {
            s.prepare_job("incremental")?;
            last_compress = Instant::now();
        }
        let Some(mut request) = server.recv_timeout(Duration::from_millis(100))? else {
            continue;
        };
        let path = request.url().split('?').next().unwrap_or("").to_owned();
        let origin = format!("http://{addr}");
        let host_ok = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .is_some_and(|h| h.value.as_str() == addr.to_string());
        let origin_ok = request
            .headers()
            .iter()
            .filter(|h| h.field.equiv("Origin"))
            .all(|h| h.value.as_str() == origin);
        if !host_ok || !origin_ok || !(path == prefix || path.starts_with(&(prefix.clone() + "/")))
        {
            let _ = request
                .respond(tiny_http::Response::from_string("Forbidden").with_status_code(403));
            continue;
        }
        let endpoint = &path[prefix.len()..];
        if request.method() == &tiny_http::Method::Get {
            if let Some(id) = endpoint.strip_prefix("/image/") {
                images.send(id, request);
                continue;
            }
        }
        if request.method() == &tiny_http::Method::Get && endpoint.starts_with("/source/") {
            let id = endpoint
                .trim_start_matches("/source/")
                .parse::<i64>()
                .unwrap_or(-1);
            let source =
                s.db.query_row("SELECT path,status FROM sources WHERE id=?1", [id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                });
            if let Ok((path, state)) = source {
                if worker::valid_relative(&path, "audio")
                    && !["expired", "retention_deleted"].contains(&state.as_str())
                {
                    if let Ok(file) = std::fs::File::open(root.join(&path)) {
                        let _ = request.respond(
                            tiny_http::Response::from_file(file)
                                .with_header(
                                    tiny_http::Header::from_bytes(
                                        "Content-Type",
                                        if path.ends_with(".m4a") {
                                            "audio/mp4"
                                        } else {
                                            "application/octet-stream"
                                        },
                                    )
                                    .unwrap(),
                                )
                                .with_header(
                                    tiny_http::Header::from_bytes("Cache-Control", "no-store")
                                        .unwrap(),
                                ),
                        );
                        continue;
                    }
                }
            }
            let _ = request.respond(
                tiny_http::Response::from_string(
                    "Source expired or unavailable; transcript provenance remains in SQLite",
                )
                .with_status_code(410),
            );
            continue;
        }
        let mut status = 200;
        let mut content_type = "application/json";
        let result: Result<String> = (|| {
            if request.method() == &tiny_http::Method::Get {
                return match endpoint {
    ""|"/"=>{content_type="text/html; charset=utf-8";Ok(include_str!("ui.html").replace("__PREFIX__",&prefix))},
    "/update"=>Ok(update::status(&root,synthetic).to_string()),
    "/meters"=>Ok(meters.lock().unwrap().snapshot().to_string()),
    "/live"=>{let mut v=inspect::live(&s)?;v["capture_transition"]=json!(s.get("capture_transition"));v["transcript"]=inspect::transcript(&s,&json!({"day":chrono::Local::now().date_naive().to_string(),"with_saves":true,"passive_screen":reading::ReadingStyle::saved(&s.get("reading_style")).passive_screen}))?;v["memory_audio"]=inbox.lock().unwrap().stats();v["paste_image"]=saves.paste_status();Ok(v.to_string())},
    "/status"=>Ok(json!({"status":s.get("status"),"capture_warning":s.get("capture_warning"),"active":s.get("active"),"armed":s.get("armed"),"notice":s.get("notice"),"permissions":s.get("permissions"),"pending":s.pending(),"summary_chars":s.summary_chars(),"disk_bytes":store::disk_bytes(&root),"config":c,"observations":s.recent()?,"jobs":s.summaries()?,"synthetic":synthetic}).to_string()),
    "/overview"=>Ok(json!({"reading":reading::ReadingStyle::saved(&s.get("reading_style")),"capture_starting":s.get("capture_starting"),"capture_stopping":s.get("capture_stopping"),"capture_transition":s.get("capture_transition"),"update":update::status(&root,synthetic),"status":s.get("status"),"capture_warning":s.get("capture_warning"),"active":s.get("active"),"armed":s.get("armed"),"notice":s.get("notice"),"permissions":s.get("permissions"),"pending":s.pending(),"summary_chars":s.summary_chars(),"disk_bytes":store::disk_bytes(&root),"config":c,"synthetic":synthetic,"draft":s.get("note_draft"),"draft_images":s.draft_images()?,"memory_audio":inbox.lock().unwrap().stats(),"health":inspect::health(&s,&c,&mut sampler)?}).to_string()),
    "/export"=>{s.export()?;content_type="text/plain; charset=utf-8";Ok(std::fs::read_to_string(root.join(format!("{}.txt",chrono::Local::now().date_naive())))?)},
    _=>{status=404;Ok("{}".into())}};
            }
            if request.method() != &tiny_http::Method::Post {
                status = 405;
                return Ok("{}".into());
            }
            if !request.headers().iter().any(|h| {
                h.field.equiv("Content-Type") && h.value.as_str().starts_with("application/json")
            }) {
                return Err("JSON Content-Type required".into());
            }
            let mut body = Vec::new();
            request
                .as_reader()
                .take(128 * 1024 + 1)
                .read_to_end(&mut body)?;
            if body.len() > 128 * 1024 {
                return Err("request too large".into());
            }
            let v: Value = serde_json::from_slice(&body)?;
            match endpoint {
                "/resume_workspace" => {
                    return Ok(resume_workspace.take().unwrap_or(Value::Null).to_string());
                }
                "/update" => {
                    if saves.busy() {
                        return Err("Finish or cancel the current save before updating".into());
                    }
                    if let Some(draft) = v["draft"].as_str() {
                        if draft.len() > 16000 {
                            return Err("Draft exceeds 16,000 bytes".into());
                        }
                        s.state("note_draft", draft)?;
                    }
                    update::begin(&root, synthetic, v.get("workspace"))?;
                    return Ok(update::status(&root, synthetic).to_string());
                }
                "/timeline" => return Ok(inspect::timeline(&s, &v)?.to_string()),
                "/copy_time_range" => return Ok(inspect::copy_time_range(&s, &v)?.to_string()),
                "/transcript" => return Ok(inspect::transcript(&s, &v)?.to_string()),
                "/inspect" => return Ok(inspect::inspect(&s, &v)?.to_string()),
                "/export_day" => {
                    let day = v["day"].as_str().ok_or("day required")?;
                    inspect::day_bounds(day)?;
                    s.db.execute("INSERT OR IGNORE INTO export_dirty VALUES(?1)", [day])?;
                    s.export()?;
                    return Ok(json!({"day":day,"text":std::fs::read_to_string(root.join(format!("{day}.txt")))?}).to_string());
                }
                "/paste_image" => {
                    if synthetic || update::busy(&root) {
                        return Err(
                            "Image paste is unavailable during an update or a synthetic run".into(),
                        );
                    }
                    let version = v["version"].as_i64().ok_or("Clipboard version required")?;
                    let ticket = saves.paste_image(&s, version, v["attach"] == true, disk_limit)?;
                    return Ok(json!({"ticket": ticket}).to_string());
                }
                "/detach_draft_image" => {
                    let id = v["id"].as_i64().ok_or("Image ID required")?;
                    s.db.execute("DELETE FROM note_draft_images WHERE image_id=?1", [id])?;
                    return Ok(json!({"draft_images":s.draft_images()?}).to_string());
                }
                "/retry_saved_ocr" => {
                    let id = v["id"].as_i64().ok_or("Observation ID required")?;
                    let item = inspect::inspect(&s, &json!({"kind":"observation","id":id}))?;
                    if item["details"]["saved_image"] != true
                        || item["details"]["ocr_status"] != "failed"
                    {
                        return Err("Choose a saved image with a recognition error".into());
                    }
                    if update::busy(&root) {
                        return Err("An app update is in progress".into());
                    }
                    saves.start(&s, saved::Action::ReadText(id), disk_limit)?;
                }
                "/reveal_image" => {
                    let id = v["id"].as_i64().ok_or("observation id required")?;
                    let item = inspect::inspect(&s, &json!({"kind":"observation","id":id}))?;
                    if item["image_retained"] != true {
                        return Err("Screen image was not retained or has expired".into());
                    }
                    let file = if item["details"]["saved_image"] == true {
                        saved::export_png(&s, id)?
                    } else {
                        root.join(
                            item["details"]["path"]
                                .as_str()
                                .ok_or("image path missing")?,
                        )
                    };
                    let mut open = Command::new("/usr/bin/open");
                    if item["details"]["saved_image"] != true || v["open"] != true {
                        open.arg("-R");
                    }
                    open.arg(file).spawn()?;
                }
                "/reveal_source" => {
                    let id = v["id"].as_i64().ok_or("source id required")?;
                    let item = inspect::source(&s, id)?;
                    if item["retained"] != true {
                        return Err("Source media has expired or is unavailable".into());
                    }
                    Command::new("/usr/bin/open")
                        .arg("-R")
                        .arg(root.join(item["path"].as_str().ok_or("source path missing")?))
                        .spawn()?;
                }
                "/reveal_passage_audio" => {
                    let item = inspect::inspect(&s, &json!({"kind":"observation","id":v["id"]}))?;
                    let paths: Vec<_> = item["sources"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|part| part["retained"] == true)
                        .filter_map(|part| part["path"].as_str())
                        .map(|relative| root.join(relative))
                        .collect();
                    if paths.is_empty() {
                        return Err("The audio for this passage is no longer saved".into());
                    }
                    Command::new("/usr/bin/open")
                        .arg("-R")
                        .args(paths)
                        .spawn()?;
                }
                "/command" => {
                    let cmd = v["command"].as_str().ok_or("command missing")?;
                    match cmd {
                        "start" | "arm" => {
                            if update::busy(&root) {
                                return Err("Update in progress; recording will remain off until you start it after restart".into());
                            }
                            if !synthetic
                                && (c.model_path.is_empty() || !Path::new(&c.model_path).is_file())
                            {
                                return Err("Choose an existing local Moonshine GGUF model before recording".into());
                            }
                            if s.pending() >= c.max_pending
                                || store::disk_bytes(&root) >= c.disk_limit_mb * 1024 * 1024
                            {
                                return Err("capacity exhausted".into());
                            }
                            if let Some(b) = &mut native {
                                b.send(cmd)
                            } else {
                                s.state("status", "synthetic recording")?;
                                s.state("active", "true")?;
                                s.state("armed", "true")?;
                                meters.lock().unwrap().synthetic_state(true);
                            }
                        }
                        "pause" | "stop" => {
                            if let Some(b) = &mut native {
                                b.send(cmd)
                            } else {
                                s.state(
                                    "status",
                                    if cmd == "pause" { "paused" } else { "stopped" },
                                )?;
                                s.state("active", "false")?;
                                s.state("armed", "false")?;
                                meters.lock().unwrap().synthetic_state(false);
                            }
                        }
                        "compress" => {
                            s.prepare_job("incremental")?;
                        }
                        "meta" => {
                            s.prepare_job("meta")?;
                        }
                        "retry" => {
                            s.db.execute("UPDATE jobs SET retry_at=0 WHERE status='failed'", [])?;
                            s.db.execute(
                                "UPDATE sources SET retry_at=0 WHERE status='pending'",
                                [],
                            )?;
                        }
                        "open" | "permission_mic" | "permission_screen" | "permission_refresh" => {
                            if let Some(b) = &mut native {
                                b.send(cmd)
                            } else {
                                s.state(
                                    "notice",
                                    "Synthetic test: native permission/window action suppressed",
                                )?;
                            }
                        }
                        "quit" => {
                            EXIT.store(true, Ordering::Relaxed);
                        }
                        _ => return Err("unknown command".into()),
                    }
                }
                "/config" => {
                    if update::busy(&root) {
                        return Err("Wait for the update before changing settings".into());
                    }
                    if s.get("active") == "true" || s.get("armed") == "true" {
                        return Err("Stop recording before changing settings".into());
                    }
                    let mut next: Config = serde_json::from_value(v)?;
                    next.migrate_screen_mode();
                    next.validate()?;
                    write_config(&root, &next)?;
                    c = next;
                    if let Some(b) = &mut native {
                        b.send("reload");
                        s.state("notice", "Settings saved; capture remains stopped")?;
                    }
                }
                "/import" => {
                    return Ok(serde_json::to_string(&config::import_opencode(
                        &if let Some(raw) = v["jsonc"].as_str() {
                            config::parse_jsonc(raw)?
                        } else {
                            v
                        },
                    )?)?);
                }
                "/search" => {
                    let text = v["query"].as_str().ok_or("query missing")?;
                    if text.len() < 2 || text.len() > 200 {
                        return Err("search query must be 2..200 bytes".into());
                    }
                    return Ok(s.search(text)?.to_string());
                }
                "/reading" => {
                    let style = serde_json::from_value::<reading::ReadingStyle>(v)?.validate()?;
                    s.state("reading_style", &serde_json::to_string(&style)?)?;
                }
                "/draft" => {
                    let text = v["text"].as_str().ok_or("draft text required")?;
                    if text.len() > 16000 {
                        return Err("draft exceeds 16,000 bytes".into());
                    }
                    s.state("note_draft", text)?;
                }
                "/note" => {
                    let text = v["text"].as_str().ok_or("note text missing")?;
                    s.save_note(text, v["include_images"] == true)?;
                }
                "/synthetic_meters" if synthetic => {
                    let mut event = v.clone();
                    event["type"] = json!("meters");
                    meters.lock().unwrap().update(&event);
                }
                "/synthetic_audio" if synthetic => {
                    if s.get("active") != "true" {
                        return Err("synthetic recorder paused/stopped".into());
                    }
                    let mut event = v.clone();
                    event["type"] = json!("audio");
                    ingest(&s, &c, &event, &inbox)?;
                }
                "/synthetic" if synthetic => {
                    if s.get("active") != "true" {
                        return Err("synthetic recorder paused/stopped".into());
                    }
                    let track = v["source"].as_str().unwrap_or("me");
                    if !["me", "remote"].contains(&track) {
                        return Err("invalid source".into());
                    }
                    let text = v["text"].as_str().unwrap_or(
                        "Synthetic meeting: decide to ship tomorrow; action: review checklist.",
                    );
                    if text.len() > 16000 {
                        return Err("fixture too large".into());
                    }
                    s.observation(
                        now(),
                        now(),
                        "transcript",
                        track,
                        text,
                        &json!({"synthetic":true}),
                        None,
                    )?;
                }
                _ => {
                    status = 404;
                    return Ok("{}".into());
                }
            }
            Ok(json!({"ok":true}).to_string())
        })();
        let text = match result {
            Ok(text) => text,
            Err(e) => {
                status = 400;
                json!({"error":e.to_string()}).to_string()
            }
        };
        let response=tiny_http::Response::from_string(text).with_status_code(status).with_header(tiny_http::Header::from_bytes("Content-Type",content_type).unwrap()).with_header(tiny_http::Header::from_bytes("Cache-Control","no-store").unwrap()).with_header(tiny_http::Header::from_bytes("X-Frame-Options","DENY").unwrap()).with_header(tiny_http::Header::from_bytes("Content-Security-Policy","default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'").unwrap());
        let _ = request.respond(response);
    }
    drop(saves);
    if let Some(b) = &mut native {
        b.stop()
    }
    alive.store(false, Ordering::Relaxed);
    s.state("active", "false")?;
    s.state("armed", "false")?;
    s.state("status", "stopped")?;
    s.export()?;
    let _ = std::fs::remove_file(root.join("control-url.txt"));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_loss_is_durable_visible_and_does_not_stop_recording() {
        let root = std::env::temp_dir().join(format!("daybook-audio-gap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let inbox = Default::default();
        s.state("active", "true").unwrap();
        s.state("armed", "true").unwrap();
        let gap = json!({"type":"audio_gap","known_rejected_frames":8000,"losses":[{"track":"me","frames":8000,"audio_seconds":0.5,"last_source_end":100.5},{"track":"remote","frames":0,"audio_seconds":0.0}]});
        ingest(&s, &Config::default(), &gap, &inbox).unwrap();
        ingest(&s, &Config::default(), &gap, &inbox).unwrap();
        assert_eq!(s.get("active"), "true");
        assert_eq!(s.get("armed"), "true");
        assert!(
            inspect::live(&s).unwrap()["capture_warning"]
                .as_str()
                .unwrap()
                .contains("0.500 s")
        );
        assert_eq!(
            s.db.query_row(
                "SELECT count(*) FROM observations WHERE kind='event'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let details: String =
            s.db.query_row(
                "SELECT details FROM observations WHERE kind='event'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&details).unwrap(), gap);
        ingest(
            &s,
            &Config::default(),
            &json!({"type":"state","starting":true}),
            &inbox,
        )
        .unwrap();
        assert_eq!(s.get("capture_warning"), "");
        assert_eq!(
            s.db.query_row(
                "SELECT count(*) FROM observations WHERE kind='event'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn input_levels_are_separate_and_start_clears_old_session_measurements() {
        use base64::Engine;
        let root = std::env::temp_dir().join(format!("daybook-levels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let inbox: live_audio::Shared = Default::default();
        for (i, track, rms, peak) in [(1, "me", -24.0, -12.0), (2, "remote", -120.0, -120.0)] {
            ingest(&s,&Config::default(),&json!({"type":"audio","path":format!("memory:333-{i}"),"track":track,"at":100.0,"end":100.5,"frames":8000,"level":{"rms_dbfs":rms,"peak_dbfs":peak},"pcm_base64":base64::engine::general_purpose::STANDARD.encode(vec![0;32000])}),&inbox).unwrap();
        }
        let v = inspect::live(&s).unwrap();
        assert_eq!(v["input_levels"]["me"]["rms_dbfs"], -24.0);
        assert_eq!(v["input_levels"]["remote"]["peak_dbfs"], -120.0);
        assert_eq!(v["input_levels"]["me"]["end_at"], 100.5);
        ingest(
            &s,
            &Config::default(),
            &json!({"type":"state","state":"starting capture","starting":true,"stopping":false}),
            &inbox,
        )
        .unwrap();
        let v = inspect::live(&s).unwrap();
        assert_eq!(v["capture_starting"], "true");
        assert!(v["input_levels"]["me"].is_null() && v["input_levels"]["remote"].is_null());
        ingest(
            &s,
            &Config::default(),
            &json!({"type":"state","state":"stopped","starting":false,"stopping":true}),
            &inbox,
        )
        .unwrap();
        assert_eq!(inspect::live(&s).unwrap()["capture_stopping"], "true");
        ingest(
            &s,
            &Config::default(),
            &json!({"type":"capture_drained"}),
            &inbox,
        )
        .unwrap();
        assert_eq!(inspect::live(&s).unwrap()["capture_stopping"], "false");
    }
    #[test]
    fn memory_audio_never_persists_payload_and_restart_reports_unfinished_loss() {
        use base64::Engine;
        let root =
            std::env::temp_dir().join(format!("daybook-memory-ingest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let inbox: live_audio::Shared = Default::default();
        let event = json!({"type":"audio","path":"memory:123-1","track":"me","at":1.25,"end":1.75,"session":"test","frames":8000,"pcm_base64":base64::engine::general_purpose::STANDARD.encode(vec![0;32000])});
        ingest(&s, &Config::default(), &event, &inbox).unwrap();
        assert_eq!(inbox.lock().unwrap().stats()["pcm_bytes"], 32000);
        assert_eq!(std::fs::read_dir(root.join("audio")).unwrap().count(), 0);
        let raw: String =
            s.db.query_row("SELECT details FROM source_metadata", [], |r| r.get(0))
                .unwrap();
        assert!(!raw.contains("pcm_base64"));
        assert_eq!(s.pending(), 1);
        s.recover().unwrap();
        assert_eq!(s.pending(), 0);
        assert!(s.get("notice").contains("AUDIO LOST ON RESTART"));
        assert_eq!(
            s.db.query_row("SELECT status FROM sources", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "lost"
        );
        inbox.lock().unwrap().reap(&s).unwrap();
        assert_eq!(inbox.lock().unwrap().stats()["blocks"], 0);
    }
    #[test]
    fn ram_capacity_rejection_is_counted_and_expiry_releases_pcm() {
        use base64::Engine;
        let root =
            std::env::temp_dir().join(format!("daybook-memory-capacity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let inbox: live_audio::Shared = Default::default();
        let c = Config {
            max_pending: 2,
            ..Config::default()
        };
        s.state("active", "true").unwrap();
        s.state("armed", "true").unwrap();
        for (i, track, frames) in [(1, "me", 8000), (2, "remote", 1024)] {
            let event = json!({"type":"audio","path":format!("memory:500-{i}"),"track":track,"at":1,"end":2,"frames":frames,"pcm_base64":base64::engine::general_purpose::STANDARD.encode(vec![0;frames*4])});
            ingest(&s, &c, &event, &inbox).unwrap();
        }
        let event = json!({"type":"audio","path":"memory:500-3","track":"me","at":1,"end":2,"frames":8000,"pcm_base64":base64::engine::general_purpose::STANDARD.encode(vec![0;32000])});
        ingest(&s, &c, &event, &inbox).unwrap();
        ingest(&s, &c, &event, &inbox).unwrap(); // duplicate delivery does not count twice
        assert_eq!(s.pending(), 2);
        assert_eq!(inspect::live(&s).unwrap()["memory_rejected_frames"], 8000);
        assert_eq!(inbox.lock().unwrap().stats()["pcm_bytes"], 36096);
        assert_eq!(s.get("active"), "true");
        assert_eq!(s.get("armed"), "true");
        assert!(s.get("capture_warning").contains("0.500 s"));
        assert_eq!(
            s.db.query_row(
                "SELECT status FROM sources WHERE path='memory:500-3'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "lost"
        );
        worker::cleanup(&s, &c).unwrap();
        inbox.lock().unwrap().reap(&s).unwrap();
        assert_eq!(s.pending(), 0);
        assert_eq!(inbox.lock().unwrap().stats()["pcm_bytes"], 0);
        assert!(s.get("notice").contains("EXPIRED"));
        let mut recovered = event.clone();
        recovered["path"] = json!("memory:500-4");
        recovered["at"] = json!(now() as f64);
        recovered["end"] = json!(now() as f64 + 0.5);
        recovered["session"] = json!("fixture");
        ingest(&s, &c, &recovered, &inbox).unwrap();
        assert_eq!(s.pending(), 1);
        assert_eq!(inbox.lock().unwrap().stats()["pcm_bytes"], 32000);
        let session:String=s.db.query_row("SELECT session FROM source_sessions JOIN sources ON source_id=id WHERE path='memory:500-4'",[],|r|r.get(0)).unwrap();
        assert_eq!(session, "fixture:ram:1");
        assert_eq!(inspect::live(&s).unwrap()["memory_rejected_frames"], 8000);
        assert_eq!(std::fs::read_dir(root.join("audio")).unwrap().count(), 0);
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn path_escape_rejected() {
        assert!(!worker::valid_relative("audio/../../secret", "audio"));
        assert!(!worker::valid_relative("/tmp/a", "audio"));
        assert!(worker::valid_relative("audio/me-1.f32", "audio"));
    }
    #[test]
    fn permission_denial_visible() {
        let p = std::env::temp_dir().join(format!("daybook-denial-{}", now()));
        let s = Store::open(&p).unwrap();
        ingest(
            &s,
            &Config::default(),
            &json!({"type":"error","message":"Microphone permission denied"}),
            &Default::default(),
        )
        .unwrap();
        assert!(s.get("notice").contains("denied"));
    }
    #[test]
    fn expired_backlog_reported() {
        let p = std::env::temp_dir().join(format!("daybook-expiry-{}", now()));
        let s = Store::open(&p).unwrap();
        s.add_source(0, 1, "me", "audio/me-test.f32").unwrap();
        std::fs::write(p.join("audio/me-test.f32"), [0; 4]).unwrap();
        worker::cleanup(&s, &Config::default()).unwrap();
        assert_eq!(s.pending(), 0);
        assert!(s.get("notice").contains("EXPIRED"));
        assert!(!p.join("audio/me-test.f32").exists());
    }
}
