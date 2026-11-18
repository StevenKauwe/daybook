//! Local checkout updates. Stage and verify before quitting; never start capture.
use crate::store::{Result, now};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const LOG_CAP: usize = 2 * 1024 * 1024;
fn project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").ok_or("Home directory unavailable")?,
    ))
}
fn target() -> Result<PathBuf> {
    Ok(home()?.join("Applications/Daybook.app"))
}
fn directory(root: &Path) -> PathBuf {
    root.join("update")
}
fn acquire(root: &Path) -> Result<File> {
    fs::create_dir_all(directory(root))?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory(root).join("lock"))?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("An update is already running".into());
    }
    Ok(f)
}
pub fn busy(root: &Path) -> bool {
    if !directory(root).join("lock").exists() {
        return false;
    }
    acquire(root).is_err()
}
fn save(root: &Path, phase: &str, message: &str) -> Result<()> {
    let dir = directory(root);
    fs::create_dir_all(&dir)?;
    let mut f = File::create(dir.join("status.tmp"))?;
    f.write_all(
        json!({"phase":phase,"message":message,"at":now(),"worker_pid":std::process::id()})
            .to_string()
            .as_bytes(),
    )?;
    f.sync_all()?;
    fs::rename(dir.join("status.tmp"), dir.join("status.json"))?;
    Ok(())
}
fn build_info(bundle: &Path) -> Value {
    fs::read(bundle.join("Contents/Resources/Build.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}
pub fn status(root: &Path, synthetic: bool) -> Value {
    let mut v: Value = fs::read(directory(root).join("status.json"))
        .ok()
        .filter(|b| b.len() < 32000)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(Value::is_object)
        .unwrap_or(
            json!({"phase":"idle","message":"Ready to build changes from this local checkout."}),
        );
    let running = busy(root);
    if !running && !["idle", "complete", "failed"].contains(&v["phase"].as_str().unwrap_or("")) {
        v["phase"] = json!("interrupted");
        v["message"] =
            json!("The last updater stopped unexpectedly. Recording remains off; you can retry.");
    }
    let available = !synthetic
        && project().join("build.sh").is_file()
        && project().join("data/signing-identity.txt").is_file()
        && target().is_ok_and(|p| p.is_dir());
    v["busy"] = json!(running);
    v["available"] = json!(available);
    v["source"] = json!(project());
    v["unavailable_reason"] = json!(if synthetic {
        "Updates are disabled in synthetic mode."
    } else {
        "Requires this local checkout, its stable signing identity and ~/Applications/Daybook.app."
    });
    v["installed_build"] = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent()?.parent()?.parent().map(build_info))
        .unwrap_or(Value::Null);
    if let Ok(mut f) = File::open(directory(root).join("build.log")) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(8192)));
        let mut bytes = Vec::new();
        let _ = f.take(8192).read_to_end(&mut bytes);
        v["log_tail"] = json!(String::from_utf8_lossy(&bytes));
    }
    v
}
pub fn begin(root: &Path, synthetic: bool) -> Result<()> {
    if synthetic {
        return Err("In-app updates are disabled in synthetic mode".into());
    }
    if status(root, false)["available"] != true {
        return Err("Local source, installed app or stable signing identity is unavailable".into());
    }
    let lock = acquire(root)?;
    save(
        root,
        "draining",
        "Stopping capture and finishing queued transcription…",
    )?;
    let log = File::create(directory(root).join("build.log"))?;
    let fd = lock.as_raw_fd();
    // Pass the advisory lock to the detached worker; no unlocked handoff window.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let result = Command::new(std::env::current_exe()?)
        .arg("--update-worker")
        .arg(root)
        .arg(fd.to_string())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn();
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, flags);
    }
    match result {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(e) => {
            save(
                root,
                "failed",
                "Could not start the updater; the current app is unchanged.",
            )?;
            Err(e.into())
        }
    }
}
fn control_url(root: &Path) -> Result<String> {
    let raw = fs::read_to_string(root.join("control-url.txt"))?;
    let raw = raw.trim();
    let u = url::Url::parse(raw)?;
    if u.scheme() != "http"
        || u.host_str() != Some("127.0.0.1")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err("Invalid local control URL".into());
    }
    Ok(raw.to_owned())
}
fn api(url: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let a = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .redirects(0)
        .build();
    let r = (if let Some(v) = body {
        a.post(&format!("{url}{path}")).send_json(v)
    } else {
        a.get(&format!("{url}{path}")).call()
    })
    .map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("Local Daybook request {path} returned {code}"),
        ureq::Error::Transport(_) => format!("Local Daybook request {path} could not connect"),
    })?;
    Ok(r.into_json()?)
}
fn drained(v: &Value) -> bool {
    v["active"] == "false"
        && v["armed"] == "false"
        && v["pending"] == 0
        && v["capture_transition"] == "false"
}
fn wait_drained(url: &str) -> Result<()> {
    let started = Instant::now();
    let mut quiet = None;
    while started.elapsed() < Duration::from_secs(90) {
        let v = api(url, "/live", None)?;
        if drained(&v) {
            let since = quiet.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_secs(1) {
                return Ok(());
            }
        } else {
            quiet = None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("Transcription did not finish within 90 seconds. Update cancelled; the current app and queued audio were left in place. Check Health before retrying.".into())
}
fn run_logged(command: &mut Command, log: &Path, timeout: Duration) -> Result<()> {
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = Arc::new(Mutex::new(
        OpenOptions::new().create(true).append(true).open(log)?,
    ));
    let mut threads = Vec::new();
    let readers: Vec<Box<dyn Read + Send>> = vec![
        Box::new(child.stdout.take().unwrap()),
        Box::new(child.stderr.take().unwrap()),
    ];
    for mut reader in readers {
        let output = output.clone();
        threads.push(std::thread::spawn(move || {
            let mut buf = [0; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut f = output.lock().unwrap();
                let len = f.metadata().map(|m| m.len() as usize).unwrap_or(LOG_CAP);
                let keep = n.min(LOG_CAP.saturating_sub(len));
                if keep > 0 {
                    let _ = f.write_all(&buf[..keep]);
                }
            }
        }));
    }
    let began = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if began.elapsed() > timeout {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            for t in threads {
                let _ = t.join();
            }
            return Err("Update command timed out; see the local build log".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    for t in threads {
        let _ = t.join();
    }
    if !status.success() {
        return Err(format!("Update command failed ({status}); see the local build log").into());
    }
    Ok(())
}
fn requirement(path: &Path) -> Result<String> {
    let o = Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(path)
        .output()?;
    if !o.status.success() {
        return Err("Cannot read app signing identity".into());
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    Ok(text
        .lines()
        .find(|l| l.starts_with("designated => "))
        .ok_or("Signing requirement missing")?
        .to_owned())
}
fn verify_successor(new: &Path, old: &Path, log: &Path) -> Result<()> {
    run_logged(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(new),
        log,
        Duration::from_secs(30),
    )?;
    for relative in [
        "",
        "Contents/MacOS/daybook-native",
        "Contents/Helpers/Daybook Controls.app",
    ] {
        let before = requirement(&old.join(relative))?;
        let after = requirement(&new.join(relative))?;
        if !before.contains("anchor apple generic") || before != after {
            return Err(
                "Signing identity changed. Update cancelled to preserve macOS privacy grants."
                    .into(),
            );
        }
    }
    if build_info(new)["id"].as_str().is_none() {
        return Err("Staged build metadata missing".into());
    }
    Ok(())
}
fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    fs::create_dir(dest)?;
    for e in fs::read_dir(source)? {
        let e = e?;
        let ty = e.file_type()?;
        let to = dest.join(e.file_name());
        if ty.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(e.path())?, to)?;
        } else if ty.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(e.path(), to)?;
        } else {
            return Err("Unsupported file in staged app".into());
        }
    }
    Ok(())
}
fn exchange(a: &Path, b: &Path) -> Result<()> {
    if fs::symlink_metadata(a)?.file_type().is_symlink()
        || fs::symlink_metadata(b)?.file_type().is_symlink()
    {
        return Err("Refusing to replace a symlinked app".into());
    }
    let a = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let b = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn wait_old_exit(root: &Path) -> Result<()> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("instance.lock"))?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("The current recorder did not exit; no app files were replaced".into())
}
fn launch(bundle: &Path, root: &Path) -> Result<()> {
    let result = Command::new("/usr/bin/open")
        .arg("-n")
        .arg(bundle)
        .arg("--args")
        .arg("--data")
        .arg(root)
        .status()?;
    if !result.success() {
        return Err("macOS could not reopen Daybook".into());
    }
    Ok(())
}
fn wait_successor(root: &Path, old_url: &str, id: &str) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if let Ok(u) = control_url(root) {
            if u != old_url {
                if let (Ok(v), Ok(live)) = (api(&u, "/update", None), api(&u, "/live", None)) {
                    if v["installed_build"]["id"] == id && drained(&live) {
                        return Ok(());
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("The new app did not confirm a healthy stopped startup".into())
}
fn work(root: &Path) -> Result<()> {
    let source = project();
    let target = target()?;
    let log = directory(root).join("build.log");
    let url = control_url(root)?;
    api(&url, "/command", Some(json!({"command":"stop"})))?;
    wait_drained(&url)?;
    save(
        root,
        "building",
        "Building local changes with one build job. The current app stays open until verification passes.",
    )?;
    let stage_parent = source
        .join("target")
        .join(format!("daybook-update-{}", std::process::id()));
    fs::create_dir(&stage_parent)?;
    let result = (|| -> Result<()> {
        let path = format!(
            "{}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            home()?.display()
        );
        run_logged(
            Command::new("/bin/zsh")
                .arg(source.join("build.sh"))
                .current_dir(&source)
                .env("PATH", path)
                .env("CARGO_NET_OFFLINE", "true")
                .env("DAYBOOK_DIST_DIR", &stage_parent)
                .env("DAYBOOK_COPY_DEV_HELPERS", "0"),
            &log,
            Duration::from_secs(900),
        )?;
        save(
            root,
            "verifying",
            "Checking the staged bundle and matching its signing identity…",
        )?;
        let built = stage_parent.join("Daybook.app");
        verify_successor(&built, &target, &log)?;
        let staged = target
            .parent()
            .unwrap()
            .join(format!(".Daybook.update-{}.app", std::process::id()));
        if staged.exists() {
            return Err(
                "Update staging destination already exists; refusing to overwrite it".into(),
            );
        }
        if let Err(e) = copy_tree(&built, &staged) {
            let _ = fs::remove_dir_all(&staged);
            return Err(e);
        }
        let mut previous_in_stage = false;
        let installed = (|| -> Result<()> {
            verify_successor(&staged, &target, &log)?;
            wait_drained(&url)?;
            let id = build_info(&staged)["id"]
                .as_str()
                .ok_or("Build ID missing")?
                .to_owned();
            save(
                root,
                "restarting",
                "Verified. Restarting Daybook; recording will remain off…",
            )?;
            api(&url, "/command", Some(json!({"command":"quit"})))?;
            wait_old_exit(root)?;
            if let Err(e) = exchange(&staged, &target) {
                let _ = launch(&target, root);
                return Err(e);
            }
            previous_in_stage = true;
            let started = launch(&target, root).and_then(|_| wait_successor(root, &url, &id));
            if let Err(error) = started {
                // Never swap files under a running successor. Ask it to quit; if unresponsive,
                // preserve both bundles and report the rollback path instead of killing it.
                if let Ok(u) = control_url(root) {
                    let _ = api(&u, "/command", Some(json!({"command":"quit"})));
                }
                if let Err(e) = wait_old_exit(root) {
                    return Err(format!(
                        "{error}; rollback deferred: {e}. Previous bundle: {}",
                        staged.display()
                    )
                    .into());
                }
                exchange(&staged, &target)?;
                previous_in_stage = false;
                let _ = launch(&target, root);
                return Err(
                    format!("{error}. Restored the previous app; recording remains off.").into(),
                );
            }
            save(
                root,
                "complete",
                "Update installed. Daybook restarted with recording off.",
            )?;
            // Refresh only this application's metadata; the Dock launcher path is unchanged.
            let _=Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister").arg("-f").arg(&target).status();
            Ok(())
        })();
        // Preserve a rollback bundle if a successor is still running/unresponsive.
        if installed.is_ok() || !previous_in_stage {
            let _ = fs::remove_dir_all(&staged);
        }
        installed.map_err(|e| {
            if previous_in_stage {
                format!("{e}. Previous app preserved at {}", staged.display()).into()
            } else {
                e
            }
        })
    })();
    let _ = fs::remove_dir_all(stage_parent);
    result
}
pub fn run(root: &Path, fd: i32) -> Result<()> {
    if fd < 3 {
        return Err("Invalid updater lock descriptor".into());
    }
    let _lock = unsafe { File::from_raw_fd(fd) };
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    match work(root) {
        Ok(()) => Ok(()),
        Err(e) => {
            save(root, "failed", &format!("Update failed: {e}"))?;
            Err(e)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("daybook-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn exclusive_updates_and_interruption_are_visible() {
        let r = temp("lock");
        let l = acquire(&r).unwrap();
        assert!(busy(&r));
        assert!(acquire(&r).is_err());
        save(&r, "building", "test").unwrap();
        drop(l);
        assert!(!busy(&r));
        assert_eq!(status(&r, true)["phase"], "interrupted");
        assert!(begin(&r, true).is_err());
        fs::remove_dir_all(r).unwrap();
    }
    #[test]
    fn no_restart_before_native_flush_and_transcription_finish() {
        let mut v =
            json!({"active":"false","armed":"false","pending":0,"capture_transition":"false"});
        assert!(drained(&v));
        for (k, value) in [
            ("active", json!("true")),
            ("armed", json!("true")),
            ("pending", json!(1)),
            ("capture_transition", json!("true")),
        ] {
            let old = v[k].clone();
            v[k] = value;
            assert!(!drained(&v));
            v[k] = old;
        }
        v.as_object_mut().unwrap().remove("capture_transition");
        assert!(!drained(&v));
    }
    #[test]
    fn atomic_bundle_swap_rolls_back_and_rejects_symlinks() {
        let r = temp("swap");
        let a = r.join("current.app");
        let b = r.join("stage.app");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(a.join("version"), "old").unwrap();
        fs::write(b.join("version"), "new").unwrap();
        exchange(&a, &b).unwrap();
        assert_eq!(fs::read_to_string(a.join("version")).unwrap(), "new");
        exchange(&a, &b).unwrap();
        assert_eq!(fs::read_to_string(a.join("version")).unwrap(), "old");
        let link = r.join("link.app");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        assert!(exchange(&link, &b).is_err());
        fs::remove_dir_all(r).unwrap();
    }
    #[test]
    fn failed_command_is_reported_and_logs_are_bounded() {
        let r = temp("command");
        let log = r.join("build.log");
        assert!(
            run_logged(
                Command::new("/bin/sh").args(["-c", "echo synthetic-build-failure >&2; exit 7"]),
                &log,
                Duration::from_secs(3)
            )
            .is_err()
        );
        assert!(
            fs::read_to_string(&log)
                .unwrap()
                .contains("synthetic-build-failure")
        );
        run_logged(
            Command::new("/usr/bin/head").args(["-c", "3000000", "/dev/zero"]),
            &log,
            Duration::from_secs(3),
        )
        .unwrap();
        assert_eq!(fs::metadata(&log).unwrap().len(), LOG_CAP as u64);
        fs::remove_dir_all(r).unwrap();
    }
}
