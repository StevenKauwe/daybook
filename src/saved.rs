//! User-initiated saves. Original PNGs and their archive entries commit together.
use crate::store::{Result, Store, disk_bytes, now};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_PNG: usize = 64 * 1024 * 1024;
const MAX_TEXT: usize = 512 * 1024;

pub enum Action {
    Clipboard(i64),
    Screenshot,
    ReadText(i64),
}

pub struct Saves {
    native: PathBuf,
    task: Option<JoinHandle<Result<String>>>,
    cancel_input: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    interrupted: VecDeque<i64>,
}

impl Saves {
    pub fn new(store: &Store, native: PathBuf) -> Result<Self> {
        let mut query = store.db.prepare("SELECT id FROM observations WHERE kind='screen' AND json_extract(details,'$.ocr_status')='pending' ORDER BY id")?;
        let interrupted = query
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self {
            native,
            task: None,
            cancel_input: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            interrupted,
        })
    }

    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
    pub fn cancel(&self) {
        self.cancel_input.store(true, Ordering::Release);
    }

    pub fn start(&mut self, store: &Store, action: Action, limit: u64) -> Result<()> {
        if self.task.is_some() {
            return Err("A save is already in progress".into());
        }
        self.cancel_input.store(false, Ordering::Release);
        let root = store.root.clone();
        let helper = self.native.clone();
        let cancel = self.cancel_input.clone();
        let shutdown = self.shutdown.clone();
        self.task = Some(
            thread::Builder::new()
                .name("daybook-save".into())
                .spawn(move || {
                    run(
                        &Store::open(&root)?,
                        &helper,
                        action,
                        limit,
                        &cancel,
                        &shutdown,
                    )
                })?,
        );
        Ok(())
    }

    pub fn poll(&mut self, store: &Store, limit: u64) -> Option<Result<String>> {
        if self.task.as_ref().is_some_and(|task| task.is_finished()) {
            return Some(
                self.task
                    .take()
                    .unwrap()
                    .join()
                    .unwrap_or_else(|_| Err("Save worker stopped unexpectedly".into())),
            );
        }
        if self.task.is_none() {
            if let Some(id) = self.interrupted.pop_front() {
                if let Err(e) = self.start(store, Action::ReadText(id), limit) {
                    return Some(Err(e));
                }
            }
        }
        None
    }
}

impl Drop for Saves {
    fn drop(&mut self) {
        self.cancel();
        self.shutdown.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn wait_for_child(
    command: &mut Command,
    cancel: &AtomicBool,
    timeout: Option<Duration>,
) -> Result<ExitStatus> {
    if cancel.load(Ordering::Acquire) {
        return Err("Save interrupted".into());
    }
    let mut child = command.stdin(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let timed_out = timeout.is_some_and(|limit| started.elapsed() >= limit);
        if timed_out || cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if timed_out {
                "Save helper timed out"
            } else {
                "Save interrupted"
            }
            .into());
        }
        thread::sleep(Duration::from_millis(35));
    }
}

fn native_result(command: &mut Command, image: &Path, cancel: &AtomicBool) -> Result<Value> {
    // Write to a private temporary file so a full pipe cannot block large Unicode notes.
    let response = image.with_file_name("response.json");
    let status = wait_for_child(
        command.stdout(File::create(&response)?),
        cancel,
        Some(Duration::from_secs(120)),
    )?;
    let json: Value =
        serde_json::from_reader(File::open(response)?.take((MAX_TEXT * 6 + 4096) as u64))?;
    if !status.success() {
        return Err(json["message"]
            .as_str()
            .unwrap_or("Native save failed")
            .to_owned()
            .into());
    }
    Ok(json)
}

fn check_space(s: &Store, bytes: usize, limit: u64) -> Result<()> {
    if disk_bytes(&s.root).saturating_add(bytes as u64) >= limit {
        return Err("Daybook's storage limit is full. Check Settings → Storage & resources".into());
    }
    Ok(())
}

fn run(
    s: &Store,
    helper: &Path,
    action: Action,
    limit: u64,
    cancel: &AtomicBool,
    shutdown: &AtomicBool,
) -> Result<String> {
    let temporary = tempfile::Builder::new().prefix("daybook-save-").tempdir()?;
    let image = temporary.path().join("image.png");
    if let Action::ReadText(id) = action {
        fs::write(&image, image_bytes(s, id)?)?;
        s.db.execute("UPDATE observations SET details=json_set(details,'$.ocr_status','pending') WHERE id=?1", [id])?;
        return finish_ocr(s, id, helper, &image, shutdown);
    }
    check_space(s, 0, limit)?;
    let source = match action {
        Action::Clipboard(version) => {
            let clip = native_result(
                Command::new(helper)
                    .arg("--read-clipboard")
                    .arg(&image)
                    .arg(version.to_string()),
                &image,
                cancel,
            )?;
            match clip["kind"].as_str() {
                Some("text") => {
                    let text = clip["text"].as_str().ok_or("Clipboard text unavailable")?;
                    if text.trim().is_empty() || text.len() > MAX_TEXT {
                        return Err("Clipboard text is empty or too large".into());
                    }
                    check_space(s, text.len(), limit)?;
                    let at = now();
                    s.observation(at, at, "note", "clipboard", text, &json!({}), None)?;
                    return Ok("Clipboard note saved".into());
                }
                Some("image") => "clipboard",
                _ => return Err("Unsupported clipboard content".into()),
            }
        }
        Action::Screenshot => {
            let status = wait_for_child(
                Command::new("/usr/sbin/screencapture")
                    .args(["-i", "-x", "-t", "png"])
                    .arg(&image)
                    .stdout(Stdio::null()),
                cancel,
                None,
            )?;
            if !image.exists() {
                return Ok("No screenshot saved".into());
            }
            if !status.success() {
                return Err("Screen capture did not finish".into());
            }
            "screenshot"
        }
        Action::ReadText(_) => unreachable!(),
    };
    // Read in blocks, retaining just the encoded PNG until the SQLite commit.
    let mut png = Vec::new();
    File::open(&image)?
        .take((MAX_PNG + 1) as u64)
        .read_to_end(&mut png)?;
    check_space(s, png.len(), limit)?;
    let id = store_image(s, source, &png)?;
    drop(png);
    finish_ocr(s, id, helper, &image, shutdown)
}

fn store_image(s: &Store, source: &str, png: &[u8]) -> Result<i64> {
    if png.len() > MAX_PNG || png.len() < 33 || !png.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR") {
        return Err("Invalid PNG or image exceeds 64 MiB".into());
    }
    let width = u32::from_be_bytes(png[16..20].try_into()?);
    let height = u32::from_be_bytes(png[20..24].try_into()?);
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 40_000_000 {
        return Err("Image exceeds 40 megapixels or has invalid dimensions".into());
    }
    let tx = s.db.unchecked_transaction()?;
    let at = now();
    let details = json!({"saved_image":true,"width":width,"height":height,"ocr_status":"pending"});
    let id = s.observation(at, at, "screen", source, "", &details, None)?;
    tx.execute(
        "INSERT INTO saved_images(observation_id,png) VALUES(?1,?2)",
        params![id, png],
    )?;
    tx.commit()?;
    Ok(id)
}

fn finish_ocr(
    s: &Store,
    id: i64,
    helper: &Path,
    path: &Path,
    cancel: &AtomicBool,
) -> Result<String> {
    let recognition = native_result(
        Command::new(helper).arg("--ocr-file").arg(path),
        path,
        cancel,
    );
    if cancel.load(Ordering::Acquire) {
        return Err("Saved image will finish text recognition on next launch".into());
    }
    let (status, text, message) = match recognition {
        Ok(v) => match v["text"].as_str() {
            Some("") => ("empty", String::new(), "Image saved · no text found"),
            Some(t) if t.len() <= MAX_TEXT => ("complete", t.to_owned(), "Image and text saved"),
            _ => (
                "failed",
                String::new(),
                "Image saved · text recognition failed",
            ),
        },
        Err(_) => (
            "failed",
            String::new(),
            "Image saved · text recognition failed",
        ),
    };
    s.db.execute(
        "UPDATE observations SET text=?1,details=json_set(details,'$.ocr_status',?2) WHERE id=?3",
        params![text, status, id],
    )?;
    Ok(message.into())
}

fn image_bytes(s: &Store, id: i64) -> Result<Vec<u8>> {
    Ok(s.db.query_row(
        "SELECT png FROM saved_images WHERE observation_id=?1",
        [id],
        |r| r.get(0),
    )?)
}

/// Materialize the original only on request. Keep user-edited exports intact.
pub fn export_png(s: &Store, id: i64) -> Result<PathBuf> {
    let bytes = image_bytes(s, id)?;
    let dir = s.root.join("saved-images");
    fs::create_dir_all(&dir)?;
    let target = dir.join(format!("Daybook-{id}.png"));
    if target.exists() {
        if target.is_symlink()
            || target.metadata()?.len() != bytes.len() as u64
            || fs::read(&target)? != bytes
        {
            return Err("Move the existing exported file to export this PNG again".into());
        }
    } else {
        let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&target)?;
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, inspect, worker};
    use std::os::unix::fs::PermissionsExt;
    const PNG: &[u8] = include_bytes!("../assets/Daybook.iconset/icon_16x16.png");

    fn fake_helper(dir: &Path, script: &str) -> PathBuf {
        let file = dir.join("test-native");
        fs::write(&file, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        file
    }

    #[test]
    fn clipboard_text_is_searchable_without_recording_or_replacing_a_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::open(tmp.path()).unwrap();
        let helper = fake_helper(
            tmp.path(),
            r#"printf '%s' '{"kind":"text","text":"Project note\n第二行"}'"#,
        );
        let flag = AtomicBool::new(false);
        s.state("note_draft", "unfinished").unwrap();
        assert!(run(&s, &helper, Action::Clipboard(1), 1, &flag, &flag).is_err());
        run(&s, &helper, Action::Clipboard(1), u64::MAX, &flag, &flag).unwrap();
        let day = chrono::Local::now().date_naive().to_string();
        let page = inspect::timeline(
            &s,
            &json!({"day":day,"kind":"note","source":"clipboard","query":"第二行"}),
        )
        .unwrap();
        assert_eq!(page["items"][0]["text"], "Project note\n第二行");
        assert_eq!(s.get("note_draft"), "unfinished");
        assert_ne!(s.get("active"), "true");
        assert_ne!(s.get("armed"), "true");
    }

    #[test]
    fn interrupted_image_recovers_text_and_outlives_periodic_image_retention() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::open(tmp.path()).unwrap();
        let id = store_image(&s, "screenshot", PNG).unwrap();
        s.db.execute("UPDATE observations SET at=1,end_at=1 WHERE id=?1", [id])
            .unwrap();
        s.observation(now(), now(), "note", "me", "later", &json!({}), None)
            .unwrap();
        assert!(s.prepare_job("incremental").unwrap().is_none());
        drop(s);
        let s = Store::open(tmp.path()).unwrap();
        let helper = fake_helper(tmp.path(), r#"printf '%s' '{"text":"meeting notes"}'"#);
        let mut saves = Saves::new(&s, helper).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(result) = saves.poll(&s, u64::MAX) {
                result.unwrap();
                break;
            }
            assert!(std::time::Instant::now() < deadline, "recovery timed out");
            thread::sleep(Duration::from_millis(10));
        }
        worker::cleanup(&s, &Config::default()).unwrap();
        let item = inspect::inspect(&s, &json!({"kind":"observation","id":id})).unwrap();
        assert_eq!(item["text"], "meeting notes");
        assert_eq!(item["details"]["ocr_status"], "complete");
        assert_eq!(item["image_retained"], true);
        assert!(s.prepare_job("incremental").unwrap().is_some());
        let path = export_png(&s, id).unwrap();
        assert_eq!(fs::read(&path).unwrap(), PNG);
        assert_eq!(export_png(&s, id).unwrap(), path);
        fs::write(&path, b"edited export").unwrap();
        assert!(export_png(&s, id).is_err());
        assert_eq!(image_bytes(&s, id).unwrap(), PNG);
    }

    #[test]
    fn clipboard_image_survives_ocr_failure_and_can_be_retried() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::open(tmp.path()).unwrap();
        fs::write(tmp.path().join("fixture.png"), PNG).unwrap();
        let helper = fake_helper(
            tmp.path(),
            r#"
            if [ "$1" = "--read-clipboard" ]; then
                cp "$(dirname "$0")/fixture.png" "$2"
                printf '%s' '{"kind":"image"}'
            else
                printf '%s' '{"message":"fixture failure"}'
                exit 1
            fi"#,
        );
        let flag = AtomicBool::new(false);
        assert!(
            run(&s, &helper, Action::Clipboard(1), u64::MAX, &flag, &flag)
                .unwrap()
                .contains("recognition failed")
        );
        let id = s.db.last_insert_rowid();
        let item = inspect::inspect(&s, &json!({"kind":"observation","id":id})).unwrap();
        assert_eq!(item["source"], "clipboard");
        assert_eq!(item["details"]["ocr_status"], "failed");
        assert_eq!(image_bytes(&s, id).unwrap(), PNG);
        let helper = fake_helper(tmp.path(), r#"printf '%s' '{"text":""}'"#);
        run(&s, &helper, Action::ReadText(id), u64::MAX, &flag, &flag).unwrap();
        let item = inspect::inspect(&s, &json!({"kind":"observation","id":id})).unwrap();
        assert_eq!(item["details"]["ocr_status"], "empty");
        assert_eq!(item["image_retained"], true);
    }

    #[test]
    fn image_and_archive_entry_commit_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::open(tmp.path()).unwrap();
        s.db.execute_batch("CREATE TRIGGER reject_image BEFORE INSERT ON saved_images BEGIN SELECT raise(ABORT,'fixture failure'); END;").unwrap();
        assert!(store_image(&s, "screenshot", PNG).is_err());
        let count: i64 =
            s.db.query_row(
                "SELECT count(*) FROM observations WHERE kind='screen'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
