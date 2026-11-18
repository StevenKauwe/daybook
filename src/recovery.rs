//! Restore committed audio using a sidecar committed before the PCM rename.
use crate::store::{Result, Store};
use serde_json::{Value, json};
use std::{path::Path, time::UNIX_EPOCH};
pub fn audio_timing(path: &Path, track: &str) -> Result<Value> {
    let meta = std::fs::metadata(path)?;
    let frames = meta.len() / 4;
    if meta.len() % 4 != 0 || frames == 0 || frames > 160000 {
        return Err("orphan PCM has invalid frame count".into());
    }
    let sidecar = path.with_extension("json");
    if sidecar.is_file() {
        if std::fs::metadata(&sidecar)?.len() > 4096 {
            return Err("audio timing sidecar exceeds cap".into());
        }
        let v: Value = serde_json::from_slice(&std::fs::read(sidecar)?)?;
        let at = v["at"].as_f64().ok_or("missing capture start")?;
        let end = v["end"].as_f64().ok_or("missing capture end")?;
        if !at.is_finite()
            || !end.is_finite()
            || at < 0.0
            || v["frames"] != frames
            || v["sample_rate"] != 16000
            || v["track"] != track
            || (end - at - frames as f64 / 16000.0).abs() > 0.001
            || v["session"]
                .as_str()
                .is_none_or(|x| x.is_empty() || x.len() > 128)
        {
            return Err("audio timing sidecar does not match PCM".into());
        }
        return Ok(v);
    }
    let end = meta.modified()?.duration_since(UNIX_EPOCH)?.as_secs_f64();
    Ok(
        json!({"at":end-frames as f64/16000.0,"end":end,"track":track,"frames":frames,"sample_rate":16000,"session":format!("legacy-{}",path.file_name().unwrap().to_string_lossy()),"timing_estimated":true,"timing_basis":"file modification time minus measured PCM duration; original capture time/session unavailable"}),
    )
}
pub fn recover_audio(s: &Store) -> Result<()> {
    for entry in std::fs::read_dir(s.root.join("audio"))?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".f32") || !(name.starts_with("me-") || name.starts_with("remote-")) {
            continue;
        }
        let path = format!("audio/{name}");
        let known: i64 =
            s.db.query_row("SELECT count(*) FROM sources WHERE path=?1", [&path], |r| {
                r.get(0)
            })?;
        if known > 0 {
            // A crash can occur after source admission but before metadata persistence.
            let stored:i64=s.db.query_row("SELECT count(*) FROM source_metadata m JOIN sources s ON s.id=m.source_id WHERE s.path=?1",[&path],|r|r.get(0))?;
            if stored == 0 && entry.path().with_extension("json").is_file() {
                let track = if name.starts_with("me-") {
                    "me"
                } else {
                    "remote"
                };
                match audio_timing(&entry.path(), track) {
                    Ok(v) => s.source_metadata(&path, &v)?,
                    Err(e) => {
                        s.event(&format!("Source metadata requires review for {name}: {e}"))?
                    }
                }
            }
            continue;
        }
        let compressed = path.replace(".f32", ".m4a");
        if s.db.query_row(
            "SELECT count(*) FROM sources WHERE path=?1",
            [compressed],
            |r| r.get::<_, i64>(0),
        )? > 0
        {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        let track = if name.starts_with("me-") {
            "me"
        } else {
            "remote"
        };
        match audio_timing(&entry.path(),track){
            Ok(v)=>{s.add_source_session(v["at"].as_f64().unwrap().floor() as i64,v["end"].as_f64().unwrap().ceil() as i64,track,&path,v["session"].as_str().unwrap())?;s.source_metadata(&path,&v)?;if v["timing_estimated"]==true{s.event(&format!("Recovered {name}: capture time estimated from file modification time and measured PCM duration; session unknown"))?;}},
            Err(e)=>s.event(&format!("Orphan audio {name} requires review: {e}; retained without inventing capture timing"))?,
        }
    }
    Ok(())
}
/// Remove expired, untracked capture remnants only while capture is inactive.
/// A live writer PID, any database source, or a fresh member protects the whole group.
pub fn cleanup_orphans(s: &Store, c: &crate::config::Config) -> Result<()> {
    if s.get("active") == "true" {
        return Ok(());
    }
    let lifetime = c
        .rolling_audio_seconds
        .max(c.retain_audio_hours * 3600)
        .max(60);
    let mut removed = 0;
    for entry in std::fs::read_dir(s.root.join("audio"))?.flatten() {
        if removed >= 256 {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("me-") || name.starts_with("remote-")) {
            continue;
        }
        let stem = [".meta.tmp", ".f32", ".tmp", ".json"]
            .iter()
            .find_map(|ext| name.strip_suffix(ext));
        let Some(stem) = stem else {
            continue;
        };
        // Current capture names embed the writer PID. EPERM also means it may be alive.
        if let Some(pid) = stem
            .split('-')
            .nth(1)
            .and_then(|x| x.parse::<i32>().ok())
            .filter(|p| *p > 0)
        {
            if unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                continue;
            }
        }
        let tracked: i64 = s.db.query_row(
            "SELECT count(*) FROM sources WHERE path=?1 OR path=?2",
            rusqlite::params![format!("audio/{stem}.f32"), format!("audio/{stem}.m4a")],
            |r| r.get(0),
        )?;
        if tracked != 0 {
            continue;
        }
        let sidecar = s.root.join("audio").join(format!("{stem}.json"));
        if std::fs::symlink_metadata(&sidecar).is_ok_and(|m| m.is_file() && m.len() <= 4096) {
            if let Ok(v) = std::fs::read(&sidecar)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .ok_or(())
            {
                if v["end"]
                    .as_f64()
                    .is_some_and(|end| end >= crate::store::now() as f64 - lifetime as f64)
                {
                    continue;
                }
            }
        }
        let paths: Vec<_> = ["f32", "tmp", "json", "meta.tmp"]
            .iter()
            .map(|ext| s.root.join("audio").join(format!("{stem}.{ext}")))
            .collect();
        let mut expired = true;
        let mut existing = Vec::new();
        for path in paths {
            match std::fs::symlink_metadata(&path) {
                Ok(m) => {
                    if !m.is_file()
                        || m.modified()?.elapsed().unwrap_or_default().as_secs() <= lifetime
                    {
                        expired = false;
                        break;
                    }
                    existing.push(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if !expired || existing.is_empty() {
            continue;
        }
        for path in existing {
            std::fs::remove_file(path)?;
        }
        removed += 1;
        s.event(&format!("ORPHAN AUDIO EXPIRED: {stem}; untracked capture remnants exceeded the longer rolling/retained audio window; deleted"))?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn orphan_retention_protects_active_recent_tracked_and_live_writer_files() {
        let root =
            std::env::temp_dir().join(format!("daybook-orphan-expiry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let c = crate::config::Config::default();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86400);
        let write = |name: &str, bytes: &[u8], aged: bool| {
            let p = root.join("audio").join(name);
            std::fs::write(&p, bytes).unwrap();
            if aged {
                std::fs::File::options()
                    .write(true)
                    .open(p)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(old))
                    .unwrap();
            }
        };
        write("me-invalid.f32", &[0; 64], true);
        write("me-invalid.json", b"broken", true);
        write("remote-interrupted.tmp", &[0; 64], true);
        write("remote-interrupted.meta.tmp", b"partial", true);
        write("me-recent.tmp", &[0; 64], false);
        write("me-freshsidecar.f32", &[0; 64], true);
        write("me-freshsidecar.json", b"broken", false);
        write("me-future.f32", &[0; 64], true);
        write(
            "me-future.json",
            json!({"end":crate::store::now()+3600})
                .to_string()
                .as_bytes(),
            true,
        );
        let live = format!("me-{}-1-0.tmp", std::process::id());
        write(&live, &[0; 64], true);
        write("remote-tracked.f32", &[0; 64], true);
        s.add_source(1, 2, "remote", "audio/remote-tracked.f32")
            .unwrap();
        s.state("active", "true").unwrap();
        cleanup_orphans(&s, &c).unwrap();
        assert!(root.join("audio/me-invalid.f32").exists());
        s.state("active", "false").unwrap();
        let mut longer = c.clone();
        longer.retain_audio_hours = 24 * 30;
        cleanup_orphans(&s, &longer).unwrap();
        assert!(
            root.join("audio/me-invalid.f32").exists(),
            "longer retention still protects old orphan"
        );
        cleanup_orphans(&s, &c).unwrap();
        for name in [
            "me-invalid.f32",
            "me-invalid.json",
            "remote-interrupted.tmp",
            "remote-interrupted.meta.tmp",
        ] {
            assert!(!root.join("audio").join(name).exists(), "{name}");
        }
        for name in [
            "me-recent.tmp",
            "me-freshsidecar.f32",
            "me-freshsidecar.json",
            "me-future.f32",
            "remote-tracked.f32",
            &live,
        ] {
            assert!(root.join("audio").join(name).exists(), "{name}");
        }
        assert!(s.get("notice").contains("ORPHAN AUDIO EXPIRED"));
        cleanup_orphans(&s, &c).unwrap();
    }
    #[test]
    fn exact_midnight_metadata_and_legacy_duration_recover_idempotently() {
        let root = std::env::temp_dir().join(format!("daybook-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let day = chrono::Local::now().date_naive();
        let midnight = day
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap()
            .timestamp() as f64;
        let p = root.join("audio/me-midnight.f32");
        std::fs::write(&p, vec![0u8; 64000]).unwrap();
        let v = json!({"at":midnight-0.25,"end":midnight+0.75,"track":"me","frames":16000,"sample_rate":16000,"session":"original-epoch"});
        std::fs::write(p.with_extension("json"), v.to_string()).unwrap();
        assert_eq!(audio_timing(&p, "me").unwrap(), v);
        recover_audio(&s).unwrap();
        s.db.execute("DELETE FROM source_metadata", []).unwrap();
        recover_audio(&s).unwrap();
        assert_eq!(
            s.db.query_row("SELECT count(*) FROM source_metadata", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            s.db.query_row("SELECT count(*) FROM sources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            s.db.query_row("SELECT at FROM sources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            midnight as i64 - 1
        );
        assert_eq!(
            s.db.query_row("SELECT session FROM source_sessions", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "original-epoch"
        );
        let (at, end): (i64, i64) =
            s.db.query_row("SELECT at,end_at FROM sources", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            end,
            midnight as i64 + 1,
            "coarse interval must include the next-day fraction"
        );
        s.observation(
            at,
            end,
            "transcript",
            "me",
            "spans midnight",
            &json!({}),
            Some(1),
        )
        .unwrap();
        let job = s.prepare_job("incremental").unwrap().unwrap();
        s.db.execute(
            "UPDATE jobs SET status='done',text='cross-day source' WHERE id=?1",
            [job],
        )
        .unwrap();
        assert_eq!(
            s.db.query_row(
                "SELECT count(*) FROM job_days WHERE job_id=?1",
                [job],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        for (name, frames) in [("remote-one", 16000), ("remote-legacy", 160000)] {
            let p = root.join(format!("audio/{name}.f32"));
            std::fs::write(&p, vec![0u8; frames * 4]).unwrap();
            let v = audio_timing(&p, "remote").unwrap();
            assert!(
                (v["end"].as_f64().unwrap() - v["at"].as_f64().unwrap() - frames as f64 / 16000.0)
                    .abs()
                    < 0.001
            );
            assert_eq!(v["timing_estimated"], true);
        }
        std::fs::write(p.with_extension("json"), b"bad json").unwrap();
        assert!(audio_timing(&p, "me").is_err());
    }
}
