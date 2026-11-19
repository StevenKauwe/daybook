use crate::{
    config::Config,
    provider,
    store::{Result, Store, now},
};
use rusqlite::params;
use serde_json::json;
use speechworks_inference::{Backend, LoadOptions, Model};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub fn run(
    root: std::path::PathBuf,
    alive: Arc<AtomicBool>,
    synthetic: bool,
    inbox: crate::live_audio::Shared,
) {
    let background_root = root.clone();
    let background_alive = alive.clone();
    let background_inbox = inbox.clone();
    std::thread::spawn(move || {
        worker_loop(
            background_root,
            background_alive,
            synthetic,
            true,
            background_inbox,
        )
    });
    worker_loop(root, alive, synthetic, false, inbox);
}
fn worker_loop(
    root: std::path::PathBuf,
    alive: Arc<AtomicBool>,
    synthetic: bool,
    background: bool,
    inbox: crate::live_audio::Shared,
) {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    let mut model: Option<Model> = None;
    let mut streams = crate::streaming::Streams::with_inbox(inbox.clone());
    let mut s = match Store::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("worker database: {e}");
            return;
        }
    };
    let mut disk_checked = std::time::Instant::now() - std::time::Duration::from_secs(5);
    let mut disk_used = 0;
    let mut model_path = String::new();
    let mut model_work_mb = 0;
    let mut model_backend = String::new();
    let mut cooldown = 500;
    let mut last_load_request = String::new();
    let mut load_retry_at = 0;
    let mut load_error = String::new();
    let mut model_load_ms = None;
    while alive.load(Ordering::Relaxed) {
        let work = (|| -> Result<bool> {
            let c = Config::load(&root.join("config.json"))?;
            cooldown = if background {
                c.inference_cooldown_ms
            } else if s.get("active") == "true" || s.pending() > 0 {
                20
            } else {
                200
            };
            if disk_checked.elapsed() >= std::time::Duration::from_secs(5) {
                disk_used = crate::store::disk_bytes(&root);
                disk_checked = std::time::Instant::now();
            }
            if !synthetic && defer_for_pressure(memory_pressure_level(), background) {
                s.state(
                    "background_blocker",
                    "Memory pressure: OCR/provider work deferred; transcription has priority.",
                )?;
                return Ok(false);
            }
            if !background {
                s.state("speech_blocker", "")?;
                if s.get("notice")
                    == "macOS memory pressure: background inference/OCR-provider jobs deferred"
                {
                    s.state("notice", "")?;
                }
            }
            if disk_used > c.disk_limit_mb * 1024 * 1024 {
                if !background {
                    s.state("speech_blocker","Disk limit reached: transcription is waiting for space. Review Storage settings; queued audio still has a retention deadline.")?;
                }
                return Ok(false);
            }
            if !background {
                // Prepare the configured model without opening any capture device. Keep it
                // warm across Stop/Start, and bound failed load retries to once per minute.
                let request = format!(
                    "{}\n{}\n{}",
                    c.model_path, c.inference_backend, c.max_inference_work_mb
                );
                let changed = request != last_load_request;
                let needs_load = model.is_none()
                    || model_path != c.model_path
                    || model_backend != c.inference_backend
                    || model_work_mb != c.max_inference_work_mb;
                if needs_load && (changed || now() >= load_retry_at) {
                    last_load_request = request;
                    load_retry_at = now() + 60;
                    let started = std::time::Instant::now();
                    let result = (|| -> Result<()> {
                        if let Some(old) = &model {
                            streams.flush(&s, old, true)?;
                        }
                        model = None;
                        model_load_ms = None;
                        if c.model_path.is_empty() {
                            return Err("Choose a Speechworks model in Recording settings".into());
                        }
                        s.state("loaded_speech_model", &json!({"loaded":false,"loading":true,"model_path":c.model_path,"requested_backend":c.inference_backend}).to_string())?;
                        model = Some(crate::speech_file::load(
                            Path::new(&c.model_path),
                            LoadOptions {
                                backend: if c.inference_backend == "cpu" {
                                    Backend::Cpu
                                } else {
                                    Backend::Auto
                                },
                                max_audio_samples: crate::streaming::PHRASE_FRAMES,
                                max_work_bytes: c.max_inference_work_mb * 1024 * 1024,
                            },
                        )?);
                        model_path = c.model_path.clone();
                        model_work_mb = c.max_inference_work_mb;
                        model_backend = c.inference_backend.clone();
                        model_load_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                        Ok(())
                    })();
                    load_error = result.err().map(|e| e.to_string()).unwrap_or_default();
                }
                if model.is_none() || !load_error.is_empty() {
                    if s.get("active") == "true" || s.pending() > 0 {
                        s.state(
                            "speech_blocker",
                            &format!("Transcription waiting: {load_error}"),
                        )?;
                    }
                    return Ok(false);
                }
                if let Some((id, path, at, end, track, session)) = next_audio(&s)? {
                    if s.db.execute(
                    "UPDATE sources SET status='running',attempts=attempts+1 WHERE id=?1 AND status='pending'",
                    [id],
                )?==0{return Ok(true)};
                    let result = (|| -> Result<()> {
                        let bytes = if crate::live_audio::valid_path(&path) {
                            inbox.lock().unwrap().read(&path)?
                        } else {
                            read_limited(&root.join(&path), 640000)?
                        };
                        if bytes.len() > 640000 || bytes.len() % 4 != 0 {
                            return Err("invalid PCM size".into());
                        }
                        let pcm: Vec<f32> = bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                            .collect();
                        if pcm.iter().any(|x| !x.is_finite() || x.abs() > 1.0) {
                            return Err("invalid PCM samples".into());
                        }
                        // Preserve the configured cooldown per 10 seconds of audio as file granularity changes.
                        cooldown = (c.inference_cooldown_ms * pcm.len() as u64 / 160000).max(10);
                        streams.feed(
                            &s,
                            model.as_ref().unwrap(),
                            id,
                            at,
                            end,
                            &track,
                            &session,
                            &pcm,
                        )
                    })();
                    match result {
                        Ok(()) => {}
                        Err(e) => {
                            streams.abandon(&s, &track)?;
                            s.db.execute(
                            "UPDATE sources SET status='pending',error=?2,retry_at=?3 WHERE id=?1",
                            params![id, e.to_string(), now() + 60],
                        )?;
                            s.state("notice", &format!("Transcription pending: {e}"))?;
                        }
                    }
                    return Ok(true);
                }
                if let Some(model) = &model {
                    streams.flush(&s, model, s.get("active") != "true")?;
                }
                return Ok(false);
            }
            // One background provider/retention task at a time; never occupies the audio worker.
            if s.pending() > 4 {
                return Ok(false);
            }
            if c.retain_audio_hours > 0 && compress_audio(&s)? {
                return Ok(true);
            }
            if !synthetic && s.get("active") != "true" {
                s.prepare_job("incremental")?;
            }
            // Capture and transcript work take precedence over LLM jobs.
            let job=s.db.query_row("SELECT id,kind,lo,hi FROM jobs WHERE status IN ('pending','failed') AND retry_at<=?1 ORDER BY id LIMIT 1",[now()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)));
            if let Ok((id, kind, lo, hi)) = job {
                s.db.execute("UPDATE jobs SET status='running' WHERE id=?1", [id])?;
                let result = (|| -> Result<String> {
                    s.prepare_parts(id, &kind, lo, hi)?;
                    while let Some((part, input)) = s.next_part(id)? {
                        let text = if c.summary.model.is_empty() {
                            local_digest(&input)
                        } else {
                            provider::chat(&mut s, &c, &c.summary, &input, None)?
                        };
                        s.complete_part(id, part, &text)?;
                    }
                    s.part_output(id)
                })();
                match result {
                    Ok(text) => {
                        s.db.execute(
                            "UPDATE jobs SET status='done',text=?2,error='' WHERE id=?1",
                            params![id, text],
                        )?;
                        s.export()?;
                        if s.summary_chars() > c.summary_warn_chars as i64 {
                            s.state("notice","Summary exceeds configured text-size warning; consider manual meta-compression")?
                        }
                    }
                    Err(e) => {
                        s.db.execute(
                            "UPDATE jobs SET status='failed',error=?2,retry_at=?3 WHERE id=?1",
                            params![id, e.to_string(), now() + 300],
                        )?;
                        s.state("notice", &e.to_string())?
                    }
                }
                return Ok(true);
            }
            // Vision observations retain their source observation ID; requests are budgeted before transmission.
            let vision=s.db.query_row("SELECT id,details,at,end_at FROM observations WHERE kind='screen' AND json_extract(details,'$.vision_pending')=1 LIMIT 1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)));
            if let Ok((id, details, captured_at, captured_end)) = vision {
                let mut d: serde_json::Value = serde_json::from_str(&details)?;
                let path = d["path"].as_str().unwrap_or("").to_owned();
                let result = (|| -> Result<String> {
                    let bytes = read_limited(
                        &root.join(&path),
                        if path.ends_with(".jpg") {
                            2 * 1024 * 1024
                        } else {
                            640000
                        },
                    )?;
                    provider::chat(
                        &mut s,
                        &c,
                        &c.vision,
                        "Describe this screen's work context, visible decisions and actions. Do not infer hidden content.",
                        Some(&bytes),
                    )
                })();
                d["vision_pending"] = json!(false);
                match result {Ok(text)=>{s.observation(captured_at,captured_end,"screen","vision",&text,&json!({"observation_id":id,"path":if c.retain_images{path.clone()}else{String::new()}}),None)?},Err(e)=>{s.event(&format!("Vision source #{id} failed: {e}; no automatic retry"))?;0}};
                if !c.retain_images && !path.is_empty() {
                    let _ = std::fs::remove_file(root.join(&path));
                    d["path"] = json!("");
                    d["image_status"] = json!("deleted after vision attempt");
                }
                s.db.execute(
                    "UPDATE observations SET details=?2 WHERE id=?1",
                    params![id, d.to_string()],
                )?;
                return Ok(true);
            }
            Ok(false)
        })();
        if !background {
            {
                let status = match model.as_ref() {
                    Some(m) => match m.device() {
                        Ok(d) => {
                            json!({"at":now(),"loaded":true,"loading":false,"load_ms":model_load_ms,"model_path":model_path,"streaming":m.is_streaming(),"requested_backend":model_backend,"actual_backend":format!("{:?}",d.api),"device":d.name,"driver":d.driver,"cpu_thread_cap":2,"kernels":format!("{:?}",d.kernels),"fallback_reason":m.fallback_reason(),"work_limit_mb":model_work_mb})
                        }
                        Err(e) => {
                            json!({"loaded":true,"actual_backend":null,"error":e.to_string()})
                        }
                    },
                    None => {
                        json!({"loaded":false,"loading":false,"actual_backend":null,"error":if load_error.is_empty(){None}else{Some(load_error.as_str())},"retry_at":load_retry_at})
                    }
                };
                let _ = s.state("loaded_speech_model", &status.to_string());
            }
        }
        if let Err(e) = work {
            eprintln!("background task: {e}");
        }
        std::thread::sleep(std::time::Duration::from_millis(cooldown));
    }
}
fn next_audio(s: &Store) -> Result<Option<(i64, String, i64, i64, String, String)>> {
    let mut q=s.db.prepare("SELECT id,path,at,end_at,track,coalesce((SELECT session FROM source_sessions WHERE source_id=sources.id),'') FROM sources WHERE status='pending' AND retry_at<=?1 ORDER BY id LIMIT 1")?;
    let mut rows = q.query([now()])?;
    Ok(if let Some(r) = rows.next()? {
        Some((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    } else {
        None
    })
}
pub fn cleanup(s: &Store, c: &Config) -> Result<()> {
    crate::recovery::cleanup_orphans(s, c)?;
    let cutoff = now() - c.rolling_audio_seconds as i64;
    let retained = now() - (c.retain_audio_hours * 3600) as i64;
    let mut q=s.db.prepare("SELECT id,path,status FROM sources WHERE (status='pending' AND end_at<?1) OR (status='done' AND end_at<?2) LIMIT 256")?;
    let rows = q
        .query_map(
            params![
                cutoff,
                if c.retain_audio_hours == 0 {
                    cutoff
                } else {
                    retained
                }
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, path, status) in rows {
        if s.db.execute(
            "UPDATE sources SET status=?3 WHERE id=?1 AND status=?2",
            params![id, status, format!("expiring_{status}")],
        )? == 0
        {
            continue;
        }
        let p = s.root.join(&path);
        if p.exists() {
            std::fs::remove_file(&p)?;
            let _ = std::fs::remove_file(p.with_extension("json"));
        }
        s.db.execute(
            "UPDATE sources SET status=?2 WHERE id=?1",
            params![
                id,
                if status == "pending" {
                    "expired"
                } else {
                    "retention_deleted"
                }
            ],
        )?;
        if status == "pending" {
            s.event(&format!("BACKLOG EXPIRED: source #{id} reached rolling retention before transcription; audio deleted"))?
        }
    }
    let mut q=s.db.prepare("SELECT id,details FROM observations WHERE kind='screen' AND json_extract(details,'$.path')!='' AND (at<?1 OR (json_extract(details,'$.vision_pending')=1 AND at<?2)) LIMIT 256")?;
    let rows = q
        .query_map(
            params![now() - (c.image_hours * 3600) as i64, cutoff],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, raw) in rows {
        let mut d: serde_json::Value = serde_json::from_str(&raw)?;
        if let Some(path) = d["path"].as_str().filter(|p| !p.is_empty()) {
            let _ = std::fs::remove_file(s.root.join(path));
            if d["vision_pending"] == true {
                s.event(&format!(
                    "BACKLOG EXPIRED: vision observation #{id} expired before processing"
                ))?;
                d["vision_pending"] = json!(false);
            }
            d["path"] = json!("");
            d["image_status"] = json!("retention expired");
            s.db.execute(
                "UPDATE observations SET details=?2 WHERE id=?1",
                params![id, d.to_string()],
            )?;
        }
    }
    for entry in std::fs::read_dir(s.root.join("images"))?
        .flatten()
        .take(256)
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jpg") {
            continue;
        }
        let age = entry
            .metadata()?
            .modified()?
            .elapsed()
            .unwrap_or_default()
            .as_secs();
        if age < 60 {
            continue;
        }
        let rel = format!("images/{}", entry.file_name().to_string_lossy());
        let count: i64 = s.db.query_row(
            "SELECT count(*) FROM observations WHERE json_extract(details,'$.path')=?1",
            [&rel],
            |r| r.get(0),
        )?;
        if count == 0 {
            std::fs::remove_file(path)?;
            s.event("Removed orphan temporary screenshot from interrupted capture")?;
        }
    }
    Ok(())
}
pub fn valid_relative(p: &str, dir: &str) -> bool {
    let p = Path::new(p);
    p.components().count() == 2
        && p.parent() == Some(Path::new(dir))
        && p.file_name().is_some()
        && !p.is_symlink()
}
fn compress_audio(s: &Store) -> Result<bool> {
    use std::io::Write;
    let source=s.db.query_row("SELECT id,path FROM sources WHERE status='done' AND path LIKE '%.f32' AND error='' ORDER BY id LIMIT 1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)));
    let Ok((id, path)) = source else {
        return Ok(false);
    };
    if s.db.execute(
        "UPDATE sources SET status='compressing' WHERE id=?1 AND status='done'",
        [id],
    )? == 0
    {
        return Ok(false);
    }
    let result = (|| -> Result<String> {
        let bytes = read_limited(&s.root.join(&path), 640000)?;
        if bytes.len() > 640000 {
            return Err("retention input exceeds chunk limit".into());
        }
        let wav = s.root.join("audio/retention-work.wav");
        let mut f = std::fs::File::create(&wav)?;
        f.write_all(b"RIFF")?;
        f.write_all(&(36 + bytes.len() as u32).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&3u16.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?;
        f.write_all(&16000u32.to_le_bytes())?;
        f.write_all(&64000u32.to_le_bytes())?;
        f.write_all(&4u16.to_le_bytes())?;
        f.write_all(&32u16.to_le_bytes())?;
        f.write_all(b"data")?;
        f.write_all(&(bytes.len() as u32).to_le_bytes())?;
        f.write_all(&bytes)?;
        drop(f);
        let output = path.replace(".f32", ".m4a");
        let mut child = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "m4af", "-d", "aac", "-b", "32000"])
            .arg(&wav)
            .arg(s.root.join(&output))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err("AAC retention conversion failed".into());
                }
                break;
            }
            if start.elapsed() > std::time::Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("AAC retention conversion timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::fs::remove_file(wav)?;
        Ok(output)
    })();
    match result {
        Ok(output) => {
            s.db.execute(
                "UPDATE sources SET path=?2,status='done' WHERE id=?1",
                params![id, output],
            )?;
            std::fs::remove_file(s.root.join(path))?;
        }
        Err(e) => {
            s.db.execute(
                "UPDATE sources SET error=?2,status='done' WHERE id=?1",
                params![id, e.to_string()],
            )?;
            s.event(&format!(
                "Source #{id}: {e}; original PCM kept until configured expiry"
            ))?;
        }
    }
    Ok(true)
}

pub fn defer_for_pressure(level: i32, background: bool) -> bool {
    background && level > 1
}
pub fn memory_pressure_level() -> i32 {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut value: i32 = 0;
        let mut size = std::mem::size_of::<i32>();
        let name = b"kern.memorystatus_vm_pressure_level\0";
        if libc::sysctlbyname(
            name.as_ptr().cast(),
            (&mut value as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) == 0
        {
            return value;
        }
    }
    0
}

/// Bounded extractive fallback. Quotes evidence; never invents decisions or speaker identity.
fn local_digest(input: &str) -> String {
    let mut candidates: Vec<(usize, usize, &str)> = input
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            let lower = line.to_lowercase();
            let score = [
                "decision", "decided", "action", "will ", "next ", "need ", "plan", "thought",
            ]
            .iter()
            .filter(|word| lower.contains(**word))
            .count();
            (i, score, line)
        })
        .collect();
    candidates.sort_by_key(|(i, score, _)| (std::cmp::Reverse(*score), *i));
    candidates.truncate(12);
    candidates.sort_by_key(|(i, _, _)| *i);
    let mut result = String::from(
        "Local extractive summary (heuristic selection; consult linked sources for complete context):\n",
    );
    for (_, _, line) in candidates {
        let snippet: String = line.chars().take(280).collect();
        result.push_str(&snippet);
        if snippet.len() < line.len() {
            result.push_str(" … [excerpt]")
        }
        result.push('\n');
    }
    result
}
#[cfg(test)]
mod digest_tests {
    use super::*;
    #[test]
    fn digest_is_bounded_and_retains_refs() {
        let input = (0..200)
            .map(|i| format!("[{i} @123 me] Decision: {}\n", "a".repeat(500)))
            .collect::<String>();
        let digest = local_digest(&input);
        assert!(digest.len() < 5000);
        assert!(digest.contains("[0 @123 me]"));
        assert!(digest.contains("excerpt"));
    }
}

fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("source exceeds bounded read limit".into());
    }
    Ok(bytes)
}
#[cfg(test)]
mod retention_tests {
    use super::*;
    #[test]
    fn warning_pressure_preserves_transcription_priority() {
        assert!(!defer_for_pressure(1, false));
        assert!(!defer_for_pressure(2, false));
        assert!(defer_for_pressure(2, true));
        assert!(!defer_for_pressure(4, false));
    }
    #[test]
    fn pending_vision_expires_visibly() {
        let root =
            std::env::temp_dir().join(format!("daybook-vision-expiry-{}", std::process::id()));
        let s = Store::open(&root).unwrap();
        std::fs::write(root.join("images/test.jpg"), b"fixture").unwrap();
        s.observation(
            1,
            1,
            "screen",
            "screen",
            "",
            &json!({"path":"images/test.jpg","vision_pending":true}),
            None,
        )
        .unwrap();
        cleanup(&s, &Config::default()).unwrap();
        assert!(!root.join("images/test.jpg").exists());
        assert!(s.get("notice").contains("EXPIRED"));
    }
    #[test]
    fn retention_bounds_batches_and_preserves_recent_sources() {
        let root = std::env::temp_dir().join(format!("daybook-disk-bounds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let c = Config::default();
        for i in 0..300 {
            let path = format!("audio/old-{i}.f32");
            std::fs::write(root.join(&path), [0u8; 1024]).unwrap();
            s.add_source(1, 2, if i % 2 == 0 { "me" } else { "remote" }, &path)
                .unwrap();
        }
        std::fs::write(root.join("audio/recent.f32"), [0u8; 2048]).unwrap();
        s.add_source(now(), now(), "me", "audio/recent.f32")
            .unwrap();
        cleanup(&s, &c).unwrap();
        let count: i64 =
            s.db.query_row(
                "SELECT count(*) FROM sources WHERE status='expired'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 256, "one cleanup batch is bounded");
        cleanup(&s, &c).unwrap();
        let media: u64 = std::fs::read_dir(root.join("audio"))
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(media, 2048, "only recent media remains");
        assert!(s.get("notice").contains("BACKLOG EXPIRED"));
        assert_eq!(
            s.db.query_row("SELECT count(*) FROM sources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            301,
            "source provenance remains after expiry"
        );
        let f = std::fs::File::create(root.join("audio/cap-fixture.f32")).unwrap();
        f.set_len(c.disk_limit_mb * 1024 * 1024 + 1).unwrap();
        assert!(
            crate::store::disk_bytes(&root) > c.disk_limit_mb * 1024 * 1024,
            "logical bytes include sparse files"
        );
        drop(f);
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_media_read() {
        let path = std::env::temp_dir().join(format!("daybook-read-limit-{}", std::process::id()));
        std::fs::write(&path, [0; 100]).unwrap();
        assert!(read_limited(&path, 10).is_err());
        assert_eq!(read_limited(&path, 100).unwrap().len(), 100);
    }
}
