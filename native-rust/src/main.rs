//! Rust-only macOS framework bridge. See ffi.rs for the Apple ABI boundary.
#![allow(unsafe_op_in_unsafe_fn)]
mod audio;
mod capture_queue;
mod clipboard;
mod ffi;
mod meters;
mod mic_meter;
mod ocr_bench;
mod screen;
#[path = "../../src/screen_sample.rs"]
mod screen_sample;
use screen_sample::ScreenSample;
mod state;
use block2::RcBlock;
use chrono::{Datelike, Timelike};
use ffi::*;
use objc2::{
    msg_send,
    rc::autoreleasepool,
    runtime::{AnyObject, AnyProtocol, Bool, ClassBuilder, Sel},
    sel,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    io::{BufRead, Write},
    path::PathBuf,
    ptr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn CGRequestScreenCaptureAccess() -> bool;
    fn pthread_set_qos_class_self_np(class: u32, priority: i32) -> i32;
}
static OUT: Mutex<()> = Mutex::new(());
static EVENTS: OnceLock<SyncSender<Event>> = OnceLock::new();
static AUDIO: OnceLock<capture_queue::Sender> = OnceLock::new();
pub static PERSIST_AUDIO: AtomicBool = AtomicBool::new(false);
static PERMISSION_CHECK_AT: AtomicU64 = AtomicU64::new(0);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static STREAM: AtomicUsize = AtomicUsize::new(0);
static REPORTED_LOST_FRAMES: AtomicU64 = AtomicU64::new(0);
static FAULT_EVENTS: AtomicU64 = AtomicU64::new(0);
static FAULT: AtomicBool = AtomicBool::new(false);
pub fn stamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub fn emit(v: Value) {
    if let Ok(_guard) = OUT.lock() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{v}");
    }
}
fn event(e: Event) {
    if let Some(tx) = EVENTS.get() {
        if tx.try_send(e).is_err() {
            FAULT.store(true, Ordering::Release)
        }
    }
}
fn fault(message: impl Into<String>) {
    FAULT_EVENTS.fetch_add(1, Ordering::Relaxed);
    FAULT.store(true, Ordering::Release);
    event(Event::Fault(message.into()));
}
#[derive(Clone, Deserialize)]
struct Settings {
    #[serde(default)]
    audio_storage: String,
    screen_seconds: u64,
    work_start_hour: u32,
    work_end_hour: u32,
    weekdays_only: bool,
    meeting_bundle_id: String,
    screen_mode: String,
}
enum Event {
    Command(String),
    Notification(String),
    Content(u64, Option<u64>, Result<Shareable, String>),
    Started(u64, Option<String>),
    Stopped(u64, Option<String>),
    Flushed(u64),
    Screenshot(u64, ScreenSample, Result<Image, String>),
    ScreenDone(u64, Result<Value, String>),
    PermissionDone,
    Fault(String),
}
fn enqueue_audio(p: audio::Packet) {
    if EPOCH.load(Ordering::Acquire) != p.epoch {
        return;
    }
    if let Err(e) = AUDIO.get().unwrap().admit(p) {
        // Invalid formats or a disconnected consumer are faults. Capacity loss is
        // counted separately, splits the next phrase and resumes as space returns.
        fault(e);
    }
}
unsafe extern "C-unwind" fn menu_action(_: &AnyObject, _: Sel, sender: *mut AnyObject) {
    let command: *mut AnyObject = msg_send![sender, representedObject];
    event(Event::Command(text(command)));
}
unsafe extern "C-unwind" fn notification(_: &AnyObject, _: Sel, n: *mut AnyObject) {
    let name: *mut AnyObject = msg_send![n, name];
    event(Event::Notification(text(name)));
}
unsafe extern "C-unwind" fn stream_failed(
    _: &AnyObject,
    _: Sel,
    stream: *mut AnyObject,
    err: *mut AnyObject,
) {
    if STREAM.load(Ordering::Acquire) == stream as usize {
        fault(error(err).unwrap_or("System audio stopped unexpectedly".into()))
    }
}
unsafe extern "C-unwind" fn stream_audio(
    _: &AnyObject,
    _: Sel,
    stream: *mut AnyObject,
    sample: *const c_void,
    kind: isize,
) {
    if kind != 1 || sample.is_null() || STREAM.load(Ordering::Acquire) != stream as usize {
        return;
    }
    let epoch = EPOCH.load(Ordering::Acquire);
    if epoch == 0 {
        return;
    }
    let result = (|| -> Result<audio::Packet, String> {
        let fmt = CMSampleBufferGetFormatDescription(sample);
        if fmt.is_null() {
            return Err("Missing system audio format".into());
        }
        let desc = CMAudioFormatDescriptionGetStreamBasicDescription(fmt);
        if desc.is_null() {
            return Err("Missing PCM description".into());
        }
        let frames = CMSampleBufferGetNumSamples(sample);
        if frames <= 0 || frames as usize > audio::MAX_FRAMES {
            return Err("Invalid system audio frame count".into());
        }
        let mut size = 0usize;
        let mut block = ptr::null();
        let code = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample,
            &mut size,
            ptr::null_mut(),
            0,
            ptr::null(),
            ptr::null(),
            1,
            &mut block,
        );
        if !block.is_null() {
            CFRelease(block)
        }
        if size == 0 || size > 65536 {
            return Err(format!("Invalid system audio list size ({code})"));
        }
        let mut memory = vec![0u128; size.div_ceil(16)];
        block = ptr::null();
        let list = memory.as_mut_ptr().cast::<AudioList>();
        let code = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample,
            ptr::null_mut(),
            list,
            size,
            ptr::null(),
            ptr::null(),
            1,
            &mut block,
        );
        let _block = Cf(block.cast_mut());
        if code != 0 {
            return Err(format!("System audio buffer access failed ({code})"));
        }
        let pcm = audio::decode(list, &*desc, frames as usize)?;
        Ok(audio::Packet {
            epoch,
            me: false,
            rate: (*desc).rate as usize,
            at: stamp() - frames as f64 / (*desc).rate,
            pcm,
        })
    })();
    match result {
        Ok(p) => enqueue_audio(p),
        Err(e) => fault(e),
    }
}
unsafe fn delegate() -> Obj {
    let mut builder = ClassBuilder::new(c"DaybookRustCaptureDelegate", class(c"NSObject")).unwrap();
    builder.add_method(
        sel!(menuAction:),
        menu_action as unsafe extern "C-unwind" fn(_, _, _),
    );
    builder.add_method(
        sel!(notification:),
        notification as unsafe extern "C-unwind" fn(_, _, _),
    );
    builder.add_method(
        sel!(stream:didStopWithError:),
        stream_failed as unsafe extern "C-unwind" fn(_, _, _, _),
    );
    builder.add_method(
        sel!(stream:didOutputSampleBuffer:ofType:),
        stream_audio as unsafe extern "C-unwind" fn(_, _, _, _, _),
    );
    for name in [c"SCStreamDelegate", c"SCStreamOutput"] {
        if let Some(protocol) = AnyProtocol::get(name) {
            builder.add_protocol(protocol);
        }
    }
    let cls = builder.register();
    msg_send![cls, new]
}
unsafe fn permission_status() {
    let auth: isize =
        msg_send![class(c"AVCaptureDevice"),authorizationStatusForMediaType:&*string("soun")];
    emit(
        json!({"type":"ready","screen_permission":CGPreflightScreenCaptureAccess(),"microphone_permission":auth,"native_language":"Rust","checked_at":stamp(),"helper_path":std::env::current_exe().ok()}),
    );
}
struct App {
    synthetic: bool,
    root: PathBuf,
    settings: Settings,
    url: String,
    gate: state::Gate,
    active: bool,
    starting: bool,
    stopping: bool,
    quit_requested: bool,
    update_blocked: bool,
    quit: bool,
    epoch: u64,
    delegate: Obj,
    app: Obj,
    status_item: Obj,
    save_receipt: Obj,
    save_flash: Option<(String, Instant)>,
    stream: Option<Obj>,
    engine: Option<Obj>,
    input: Option<Obj>,
    meter_sink: Option<Obj>,
    queue: *mut c_void,
    ui_child: Option<std::process::Child>,
    screens_deferred: bool,
    screen_busy: bool,
    last_screen: Instant,
    screen_sequence: u64,
    screen_worker: SyncSender<ScreenJob>,
    stop_since: Option<Instant>,
}
struct ScreenJob {
    epoch: u64,
    image: Image,
    sample: ScreenSample,
    interval: u64,
}
fn screen_worker(rx: mpsc::Receiver<ScreenJob>) {
    unsafe {
        pthread_set_qos_class_self_np(0x11, 0);
    }
    let mut previous: Option<(ScreenSample, [u8; 32])> = None;
    for job in rx {
        let result = autoreleasepool(|_| unsafe {
            screen_reading(&job.image, job.sample, job.interval, &mut previous)
        });
        event(Event::ScreenDone(job.epoch, result));
    }
}

unsafe fn screen_reading(
    image: &Image,
    sample: ScreenSample,
    interval: u64,
    previous: &mut Option<(ScreenSample, [u8; 32])>,
) -> Result<Value, String> {
    let signature = screen::fingerprint(image);
    let same = previous
        .take()
        .is_some_and(|(last, hash)| sample.follows(&last, interval) && signature == Some(hash));
    let message = if same {
        json!({"type":"screen_unchanged","sample":sample})
    } else {
        use base64::Engine;
        let recognized = screen::recognize(image);
        let failed = recognized.is_err();
        let reading = recognized.unwrap_or_else(|_| screen::Reading {
            text: String::new(),
            lines: vec![],
            words: vec![],
        });
        let mut message = json!({"type":"screen","sample":sample,"text":reading.text,"words":reading.words,"line_boxes":reading.lines,
            "pixels":[CGImageGetWidth(image.0.0),CGImageGetHeight(image.0.0)]});
        // Bound OCR separately from the optional JPEG.
        while message.to_string().len() > 255 * 1024 {
            let boxes = message["words"].as_array_mut().unwrap();
            boxes.truncate(boxes.len() / 2);
        }
        if failed {
            message["ocr_failed"] = json!(true);
        }
        if let Ok(jpeg) = screen::jpeg(image) {
            message["jpeg"] = Value::String(base64::engine::general_purpose::STANDARD.encode(jpeg));
        }
        message
    };
    if same || (message["ocr_failed"] != true && message["jpeg"].is_string()) {
        *previous = signature.map(|hash| (sample, hash));
    }
    Ok(message)
}
impl App {
    unsafe fn new(root: PathBuf, settings: Settings, url: String) -> Result<Self, String> {
        let app: Obj = msg_send![class(c"NSApplication"), sharedApplication];
        let _: bool = msg_send![&*app,setActivationPolicy:1isize];
        let delegate = delegate();
        let bar: Obj = msg_send![class(c"NSStatusBar"), systemStatusBar];
        let status_item: Obj = msg_send![&*bar,statusItemWithLength:-1.0f64];
        let menu = new(c"NSMenu");
        let save_receipt = new(c"NSMenuItem");
        let _: () = msg_send![&*save_receipt, setHidden:true];
        let _: () = msg_send![&*save_receipt, setEnabled:false];
        for (title, command) in [
            ("Open Daybook controls", "open"),
            ("Save Clipboard to Daybook", "save_clipboard"),
            ("Save Screenshot…", "save_screenshot"),
            ("Start recording now", "start"),
            ("Enable workday auto", "arm"),
            ("Stop", "stop"),
            ("Microphone permission…", "permission_mic"),
            ("Screen Recording permission…", "permission_screen"),
            ("Quit Daybook", "quit"),
        ] {
            let item: objc2::rc::Allocated<AnyObject> = msg_send![class(c"NSMenuItem"), alloc];
            let item: Obj = msg_send![item,initWithTitle:&*string(title), action:sel!(menuAction:), keyEquivalent:&*string("")];
            let _: () = msg_send![&*item,setTarget:&*delegate];
            let _: () = msg_send![&*item,setRepresentedObject:&*string(command)];
            let _: () = msg_send![&*menu,addItem:&*item];
            if command == "save_screenshot" {
                let _: () = msg_send![&*menu, addItem:&*save_receipt];
                let divider: Obj = msg_send![class(c"NSMenuItem"), separatorItem];
                let _: () = msg_send![&*menu, addItem:&*divider];
            }
        }
        let _: () = msg_send![&*status_item,setMenu:&*menu];
        let workspace: Obj = msg_send![class(c"NSWorkspace"), sharedWorkspace];
        let workspace_nc: Obj = msg_send![&*workspace, notificationCenter];
        let distributed: Obj = msg_send![class(c"NSDistributedNotificationCenter"), defaultCenter];
        let default_nc: Obj = msg_send![class(c"NSNotificationCenter"), defaultCenter];
        for name in [
            "NSWorkspaceWillSleepNotification",
            "NSWorkspaceDidWakeNotification",
            "NSWorkspaceSessionDidResignActiveNotification",
            "NSWorkspaceSessionDidBecomeActiveNotification",
        ] {
            let _: () = msg_send![&*workspace_nc,addObserver:&*delegate, selector:sel!(notification:), name:&*string(name), object:nil()];
        }
        for name in ["com.apple.screenIsLocked", "com.apple.screenIsUnlocked"] {
            let _: () = msg_send![&*distributed,addObserver:&*delegate, selector:sel!(notification:), name:&*string(name), object:nil()];
        }
        let _: () = msg_send![&*default_nc,addObserver:&*delegate, selector:sel!(notification:), name:&*string("AVAudioEngineConfigurationChangeNotification"), object:nil()];
        let queue = dispatch_queue_create(c"local.daybook.audio".as_ptr(), ptr::null());
        if queue.is_null() {
            return Err("Could not create audio callback queue".into());
        }
        let (tx, rx) = mpsc::sync_channel::<ScreenJob>(1);
        std::thread::Builder::new()
            .name("daybook-ocr".into())
            .spawn(move || screen_worker(rx))
            .map_err(|e| e.to_string())?;
        let _: () = msg_send![&*app, finishLaunching];
        let this = Self {
            synthetic: std::env::var("DAYBOOK_SYNTHETIC").ok().as_deref() == Some("1"),
            root,
            settings,
            url,
            gate: state::Gate::default(),
            active: false,
            starting: false,
            stopping: false,
            quit_requested: false,
            update_blocked: false,
            quit: false,
            epoch: 0,
            delegate,
            app,
            status_item,
            save_receipt,
            save_flash: None,
            stream: None,
            engine: None,
            input: None,
            meter_sink: None,
            queue,
            ui_child: None,
            screens_deferred: false,
            screen_busy: false,
            last_screen: Instant::now() - Duration::from_secs(86400),
            screen_sequence: 0,
            screen_worker: tx,
            stop_since: None,
        };
        this.status("stopped");
        permission_status();
        Ok(this)
    }
    unsafe fn refresh_title(&self) {
        let name = if self.active || self.starting {
            "● Daybook"
        } else {
            "○ Daybook"
        };
        let title = self
            .save_flash
            .as_ref()
            .filter(|(_, until)| Instant::now() < *until)
            .map_or_else(|| name.to_owned(), |(badge, _)| format!("{name} {badge}"));
        let button: *mut AnyObject = msg_send![&*self.status_item, button];
        let _: () = msg_send![button,setTitle:&*string(&title)];
    }
    unsafe fn save_feedback(&mut self, message: &str, badge: &str) {
        let _: () = msg_send![&*self.save_receipt, setTitle:&*string(message)];
        let _: () = msg_send![&*self.save_receipt, setHidden:false];
        self.save_flash = Some((badge.into(), Instant::now() + Duration::from_secs(4)));
        self.refresh_title();
    }
    unsafe fn status(&self, state: &str) {
        let button: *mut AnyObject = msg_send![&*self.status_item, button];
        self.refresh_title();
        let _: () = msg_send![button,setToolTip:&*string(&format!("Daybook: {state}"))];
        emit(
            json!({"type":"state","state":state,"active":self.active,"armed":self.gate.armed,"starting":self.starting,"stopping":self.stopping,"epoch":self.epoch}),
        );
    }
    fn hours(&self) -> bool {
        let t = chrono::Local::now();
        t.hour() >= self.settings.work_start_hour
            && t.hour() < self.settings.work_end_hour
            && (!self.settings.weekdays_only || t.weekday().number_from_monday() <= 5)
    }
    unsafe fn open(&mut self) {
        if let Some(child) = &mut self.ui_child {
            if child.try_wait().ok().flatten().is_none() {
                let app: *mut AnyObject = msg_send![class(c"NSRunningApplication"),runningApplicationWithProcessIdentifier:child.id() as i32];
                if !app.is_null() {
                    let _: bool = msg_send![app,activateWithOptions:2usize];
                }
                return;
            }
        }
        let binary = std::env::current_exe().ok().and_then(|p| {
            p.parent().map(|p| {
                let bundled = p.join("../Helpers/Daybook Controls.app/Contents/MacOS/daybook-ui");
                if bundled.is_file() {
                    bundled
                } else {
                    p.join("daybook-ui")
                }
            })
        });
        match binary.and_then(|p| {
            std::process::Command::new(p)
                .arg(&self.url)
                .stdout(std::process::Stdio::null())
                .spawn()
                .ok()
        }) {
            Some(child) => self.ui_child = Some(child),
            None => emit(
                json!({"type":"error","message":"Native controls could not launch; rebuild the Daybook application bundle"}),
            ),
        }
    }
    unsafe fn privacy_settings(&self, kind: &str) {
        let url: Obj = msg_send![class(c"NSURL"),URLWithString:&*string(&format!("x-apple.systempreferences:com.apple.preference.security?Privacy_{kind}"))];
        let workspace: Obj = msg_send![class(c"NSWorkspace"), sharedWorkspace];
        let _: bool = msg_send![&*workspace,openURL:&*url];
    }
    unsafe fn request_microphone(&self) {
        if self.synthetic {
            emit(
                json!({"type":"error","message":"Synthetic mode: microphone permission prompt suppressed"}),
            );
            return;
        }
        let auth: isize =
            msg_send![class(c"AVCaptureDevice"),authorizationStatusForMediaType:&*string("soun")];
        if auth == 0 {
            let done = RcBlock::new(|_: Bool| event(Event::PermissionDone));
            let _: () = msg_send![class(c"AVCaptureDevice"),requestAccessForMediaType:&*string("soun"), completionHandler:&*done];
        } else if auth != 3 {
            self.privacy_settings("Microphone");
        }
        permission_status();
    }
    unsafe fn command(&mut self, command: &str) {
        if let Some(message) = command.strip_prefix("save_feedback ") {
            let badge = if message.ends_with(" saved") || message.starts_with("Image saved") {
                "✓"
            } else {
                "·"
            };
            self.save_feedback(message, badge);
            return;
        }
        PERSIST_AUDIO.store(self.settings.audio_storage == "recovery", Ordering::Release);
        if self.update_blocked && ["start", "arm"].contains(&command) {
            self.status("update in progress; recording remains stopped");
            return;
        }
        match command {
            "update_begin" => {
                self.update_blocked = true;
                self.gate.apply(state::Event::Stop);
                self.stop("updating; recording stopped");
            }
            "update_end" => {
                self.update_blocked = false;
                self.status("stopped");
            }
            "open" => self.open(),
            "save_clipboard" | "save_screenshot" => {
                if self.synthetic
                    || self.update_blocked
                    || self.gate.blocked()
                    || self.quit_requested
                {
                    self.save_feedback("Saving is unavailable during this session", "·");
                    return;
                }
                let screen = command == "save_screenshot";
                if screen && !CGPreflightScreenCaptureAccess() {
                    self.save_feedback(
                        "Allow Screen Recording in Daybook settings, then try again",
                        "·",
                    );
                    self.command("permission_screen");
                    return;
                }
                self.save_feedback(
                    if screen {
                        "Select an area or window"
                    } else {
                        "Saving clipboard…"
                    },
                    "…",
                );
                emit(if screen {
                    json!({"type":"save_request","source":"screenshot"})
                } else {
                    json!({"type":"save_request","source":"clipboard","version":clipboard::version()})
                });
            }
            "reload" => {
                if self.gate.armed || self.active {
                    emit(json!({"type":"error","message":"Stop before reloading native settings"}));
                } else {
                    match std::fs::read(self.root.join("config.json"))
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
                    {
                        Some(c) => {
                            self.settings = c;
                            self.status("settings saved; stopped");
                        }
                        None => self.fail("Native configuration reload failed"),
                    }
                }
            }
            "start" => {
                self.gate.apply(state::Event::Start);
                self.tick()
            }
            "arm" => {
                self.gate.apply(state::Event::Arm);
                self.tick()
            }
            "pause" => {
                self.gate.apply(state::Event::Pause);
                self.stop("paused")
            }
            "stop" => {
                self.gate.apply(state::Event::Stop);
                self.stop("stopped")
            }
            "quit" => {
                self.gate.apply(state::Event::Stop);
                self.quit_requested = true;
                self.stop("stopped")
            }
            "defer_screens" => {
                if !self.screens_deferred {
                    self.screen_sequence += 1;
                }
                self.screens_deferred = true;
            }
            "refresh_screen" => self.screen_sequence += 1,
            "allow_screens" => self.screens_deferred = false,
            "permission_refresh" => permission_status(),
            "permission_mic" => self.request_microphone(),
            "permission_screen" => {
                if self.synthetic {
                    emit(
                        json!({"type":"error","message":"Synthetic mode: screen permission prompt suppressed"}),
                    );
                    return;
                }
                if !CGPreflightScreenCaptureAccess() && !CGRequestScreenCaptureAccess() {
                    self.privacy_settings("ScreenCapture");
                }
                permission_status();
            }
            _ => {}
        }
    }
    unsafe fn fail(&mut self, message: &str) {
        emit(json!({"type":"error","message":message}));
        self.gate.apply(state::Event::Stop);
        self.stop(&format!("error: {message}"));
    }
    unsafe fn start(&mut self) {
        if !CGPreflightScreenCaptureAccess() {
            self.fail("Screen Recording permission is missing. Use Allow screen recording in Daybook settings, then Start separately.");
            return;
        }
        let auth: isize =
            msg_send![class(c"AVCaptureDevice"),authorizationStatusForMediaType:&*string("soun")];
        if auth != 3 {
            self.fail("Microphone permission is missing. Use Allow microphone in Daybook settings, then Start separately.");
            return;
        }
        self.epoch += 1;
        self.starting = true;
        self.status("starting capture");
        self.request_content(false);
    }
    unsafe fn request_content(&self, screen: bool) {
        let token = self.epoch;
        let sequence = screen.then_some(self.screen_sequence);
        let callback = RcBlock::new(move |content: *mut AnyObject, err: *mut AnyObject| {
            let result = if let Some(e) = error(err) {
                Err(e)
            } else {
                retain(content)
                    .map(Shareable)
                    .ok_or("No shareable display content".into())
            };
            event(Event::Content(token, sequence, result));
        });
        let _: () = msg_send![class(c"SCShareableContent"),getShareableContentExcludingDesktopWindows:false, onScreenWindowsOnly:true, completionHandler:&*callback];
    }
    unsafe fn content(
        &mut self,
        token: u64,
        screenshot: bool,
        content: Shareable,
    ) -> Result<(), String> {
        if token != self.epoch || !self.gate.eligible(self.hours()) {
            return Ok(());
        }
        let displays: *mut AnyObject = msg_send![&*content.0, displays];
        let count: usize = msg_send![displays, count];
        if count == 0 {
            return Err("No available display".into());
        }
        let display: *mut AnyObject = msg_send![displays,objectAtIndex:0usize];
        let empty = new(c"NSArray");
        let allocated: objc2::rc::Allocated<AnyObject> =
            msg_send![class(c"SCContentFilter"), alloc];
        let filter: Obj = if screenshot || self.settings.meeting_bundle_id.is_empty() {
            msg_send![allocated,initWithDisplay:display, excludingWindows:&*empty]
        } else {
            let apps: *mut AnyObject = msg_send![&*content.0, applications];
            let count: usize = msg_send![apps, count];
            let matches = new(c"NSMutableArray");
            for i in 0..count {
                let app: *mut AnyObject = msg_send![apps,objectAtIndex:i];
                let id: *mut AnyObject = msg_send![app, bundleIdentifier];
                if text(id) == self.settings.meeting_bundle_id {
                    let _: () = msg_send![&*matches,addObject:app];
                }
            }
            let count: usize = msg_send![&*matches, count];
            if count == 0 {
                return Err("Selected meeting app is not running".into());
            }
            msg_send![allocated,initWithDisplay:display, includingApplications:&*matches, exceptingWindows:&*empty]
        };
        let config = new(c"SCStreamConfiguration");
        if screenshot {
            let width: usize = msg_send![display, width];
            let height: usize = msg_send![display, height];
            let _: () = msg_send![&*config,setWidth:1280usize];
            let _: () = msg_send![&*config,setHeight:(height*1280/width.max(1)).clamp(1,4096)];
            let _: () = msg_send![&*config,setShowsCursor:false];
            let display_id: u32 = msg_send![display, displayID];
            let (pid, window) = screen::foreground_window();
            let sequence = self.screen_sequence;
            let target = if pid == 0 {
                format!("{display_id}/unknown/{sequence}")
            } else {
                format!("{display_id}/{width}x{height}/{pid}/{window}")
            };
            let callback = RcBlock::new(move |image: *mut c_void, err: *mut AnyObject| {
                let result = if let Some(e) = error(err) {
                    Err(e)
                } else {
                    Image::retained(image).ok_or("No screenshot image returned".into())
                };
                let sample = ScreenSample {
                    session: format!("{}-{token}", std::process::id()),
                    sequence,
                    target: target.clone(),
                    at: stamp() as i64,
                };
                event(Event::Screenshot(token, sample, result));
            });
            let _: () = msg_send![class(c"SCScreenshotManager"),captureImageWithFilter:&*filter, configuration:&*config, completionHandler:&*callback];
            return Ok(());
        }
        let _: () = msg_send![&*config,setCapturesAudio:true];
        let _: () = msg_send![&*config,setExcludesCurrentProcessAudio:true];
        let _: () = msg_send![&*config,setSampleRate:16000isize];
        let _: () = msg_send![&*config,setChannelCount:1isize];
        let _: () = msg_send![&*config,setWidth:2usize];
        let _: () = msg_send![&*config,setHeight:2usize];
        let _: () = msg_send![&*config,setQueueDepth:3isize];
        let _: () =
            msg_send![&*config,setMinimumFrameInterval:Time{value:60,scale:1,flags:1,epoch:0}];
        let stream: objc2::rc::Allocated<AnyObject> = msg_send![class(c"SCStream"), alloc];
        let stream: Obj = msg_send![stream,initWithFilter:&*filter, configuration:&*config, delegate:&*self.delegate];
        let mut err = nil();
        let ok: bool = msg_send![&*stream,addStreamOutput:&*self.delegate, type:1isize, sampleHandlerQueue:self.queue, error:&mut err];
        if !ok {
            return Err(error(err).unwrap_or("Could not add system audio output".into()));
        }
        STREAM.store((&*stream as *const AnyObject) as usize, Ordering::Release);
        EPOCH.store(token, Ordering::Release);
        meters::wake();
        let callback =
            RcBlock::new(move |err: *mut AnyObject| event(Event::Started(token, error(err))));
        let _: () = msg_send![&*stream,startCaptureWithCompletionHandler:&*callback];
        self.stream = Some(stream);
        Ok(())
    }
    unsafe fn start_microphone(&mut self) -> Result<(), String> {
        let engine = new(c"AVAudioEngine");
        let input: Obj = msg_send![&*engine, inputNode];
        let format: Obj = msg_send![&*input,outputFormatForBus:0usize];
        let rate: f64 = msg_send![&*format, sampleRate];
        if rate <= 0.0 {
            return Err("No microphone device".into());
        }
        let desc: *const Asbd = msg_send![&*format, streamDescription];
        if desc.is_null() {
            return Err("Microphone PCM description missing".into());
        }
        let sink = mic_meter::node(*desc, self.epoch);
        let _: () = msg_send![&*engine,attachNode:&*sink];
        let _: () = msg_send![&*engine,connect:&*input,to:&*sink,format:&*format];
        let token = self.epoch;
        let tap = RcBlock::new(move |buffer: *mut AnyObject, _time: *mut AnyObject| {
            if EPOCH.load(Ordering::Acquire) != token {
                return;
            }
            let frames: u32 = msg_send![buffer, frameLength];
            let format: *mut AnyObject = msg_send![buffer, format];
            let desc: *const Asbd = msg_send![format, streamDescription];
            let list: *const AudioList = msg_send![buffer, audioBufferList];
            if desc.is_null() {
                fault("Microphone PCM description missing");
                return;
            }
            match audio::decode(list, &*desc, frames as usize) {
                Ok(pcm) => enqueue_audio(audio::Packet {
                    epoch: token,
                    me: true,
                    rate: (*desc).rate as usize,
                    at: stamp() - frames as f64 / (*desc).rate,
                    pcm,
                }),
                Err(e) => fault(e),
            }
        });
        let _: () = msg_send![&*input,installTapOnBus:0usize, bufferSize:2048u32, format:&*format, block:&*tap];
        let mut err = nil();
        let ok: bool = msg_send![&*engine,startAndReturnError:&mut err];
        if !ok {
            let _: () = msg_send![&*input,removeTapOnBus:0usize];
            return Err(error(err).unwrap_or("Microphone engine start failed".into()));
        }
        self.input = Some(input);
        self.meter_sink = Some(sink);
        self.engine = Some(engine);
        Ok(())
    }
    unsafe fn stop(&mut self, reason: &str) {
        EPOCH.store(0, Ordering::Release);
        STREAM.store(0, Ordering::Release);
        self.active = false;
        self.starting = false;
        if self.stopping {
            self.status(reason);
            return;
        }
        self.stopping = true;
        self.stop_since = Some(Instant::now());
        if let Some(input) = self.input.take() {
            let _: () = msg_send![&*input,removeTapOnBus:0usize];
        }
        if let Some(engine) = self.engine.take() {
            let _: () = msg_send![&*engine, stop];
            if let Some(sink) = self.meter_sink.take() {
                let _: () = msg_send![&*engine,detachNode:&*sink];
            }
        }
        mic_meter::ACTIVE.store(false, Ordering::Release);
        let token = self.epoch;
        self.epoch += 1;
        self.screen_busy = false;
        if let Some(stream) = self.stream.take() {
            let callback =
                RcBlock::new(move |err: *mut AnyObject| event(Event::Stopped(token, error(err))));
            let _: () = msg_send![&*stream,stopCaptureWithCompletionHandler:&*callback];
        } else {
            event(Event::Stopped(token, None))
        }
        self.status(reason);
    }
    unsafe fn tick(&mut self) {
        if self
            .save_flash
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() >= *until)
        {
            self.save_flash = None;
            self.refresh_title();
        }
        PERSIST_AUDIO.store(self.settings.audio_storage == "recovery", Ordering::Release);
        let second = stamp() as u64;
        if second.saturating_sub(PERMISSION_CHECK_AT.load(Ordering::Relaxed)) >= 5 {
            PERMISSION_CHECK_AT.store(second, Ordering::Relaxed);
            permission_status();
        }
        let mut usage: libc::rusage = std::mem::zeroed();
        let resource = if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
            json!({"peak_rss_bytes":usage.ru_maxrss,"cpu_seconds":usage.ru_utime.tv_sec as f64+usage.ru_utime.tv_usec as f64/1e6+usage.ru_stime.tv_sec as f64+usage.ru_stime.tv_usec as f64/1e6})
        } else {
            Value::Null
        };
        let mut metrics = AUDIO.get().unwrap().metrics();
        let lost = metrics["known_rejected_frames"].as_u64().unwrap_or(0);
        if lost > REPORTED_LOST_FRAMES.swap(lost, Ordering::Relaxed) {
            emit(
                json!({"type":"audio_gap","at":stamp(),"known_rejected_frames":lost,"losses":metrics["losses"],"message":"Audio buffer overflow: some audio was lost. Capture continues and resumes processing as the queue drains."}),
            );
        }
        for (key,value) in json!({"type":"metrics","sampled_at":stamp(),"fault_events":FAULT_EVENTS.load(Ordering::Relaxed),"device_dropped_frames":Value::Null,"process":resource}).as_object().unwrap() {
            metrics[key] = value.clone();
        }
        emit(metrics);
        emit(
            json!({"type":"meter_health","microphone_io_active":mic_meter::ACTIVE.load(Ordering::Acquire),"callbacks":mic_meter::CALLBACKS.load(Ordering::Relaxed),"callback_frames":mic_meter::LAST_FRAMES.load(Ordering::Relaxed),"sample_rate":mic_meter::LAST_RATE.load(Ordering::Relaxed),"skipped_meter_callbacks":mic_meter::SKIPPED.load(Ordering::Relaxed)}),
        );
        if self.synthetic {
            self.active = self.gate.eligible(self.hours());
            self.status(if self.active {
                "synthetic recording"
            } else {
                "stopped"
            });
            return;
        }
        if self.stopping {
            if self
                .stop_since
                .is_some_and(|t| t.elapsed() > Duration::from_secs(8))
            {
                self.gate.apply(state::Event::Stop);
                emit(
                    json!({"type":"error","message":"Native stop timed out; capture process is exiting fail-closed"}),
                );
                self.quit = true;
            }
            return;
        }
        if !self.gate.eligible(self.hours()) {
            if self.active {
                self.stop(if self.gate.blocked() {
                    "suspended"
                } else {
                    "outside work hours"
                })
            } else if self.gate.armed && !self.gate.blocked() {
                self.status("armed; outside work hours")
            }
            return;
        }
        if !self.active && !self.starting {
            self.start()
        }
        if self.active
            && !self.screens_deferred
            && !self.screen_busy
            && self.settings.screen_mode != "off"
            && self.last_screen.elapsed() >= Duration::from_secs(self.settings.screen_seconds)
        {
            self.screen_busy = true;
            self.screen_sequence += 1;
            self.last_screen = Instant::now();
            self.request_content(true)
        }
    }
    unsafe fn handle(&mut self, e: Event) {
        match e {
            Event::Command(c) => self.command(&c),
            Event::PermissionDone => {
                permission_status();
                self.status("permissions updated; recording remains stopped")
            }
            Event::Fault(e) => self.fail(&e),
            Event::Notification(n) => {
                let e = match n.as_str() {
                    "NSWorkspaceWillSleepNotification" => Some(state::Event::Sleep),
                    "NSWorkspaceDidWakeNotification" => Some(state::Event::Wake),
                    "NSWorkspaceSessionDidResignActiveNotification" => Some(state::Event::Inactive),
                    "NSWorkspaceSessionDidBecomeActiveNotification" => Some(state::Event::Active),
                    "com.apple.screenIsLocked" => Some(state::Event::Lock),
                    "com.apple.screenIsUnlocked" => Some(state::Event::Unlock),
                    "AVAudioEngineConfigurationChangeNotification" => {
                        if self.active {
                            self.stop("audio device changed; restarting")
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(e) = e {
                    self.gate.apply(e);
                    if self.gate.blocked() {
                        emit(json!({"type":"save_cancel"}));
                    }
                    if self.gate.blocked() && (self.active || self.starting || self.gate.armed) {
                        self.stop("suspended")
                    } else {
                        self.tick()
                    }
                }
            }
            Event::Content(token, sequence, result) => {
                let screen = sequence.is_some();
                if token == self.epoch {
                    if sequence.is_some_and(|n| self.screens_deferred || n != self.screen_sequence)
                    {
                        self.screen_busy = false;
                        return;
                    }
                    match result {
                        Ok(c) => {
                            if let Err(e) = self.content(token, screen, c) {
                                if screen {
                                    self.screen_busy = false;
                                    emit(json!({"type":"screen_error","message":e}))
                                } else {
                                    self.fail(&e)
                                }
                            }
                        }
                        Err(e) => {
                            if screen {
                                self.screen_busy = false;
                                emit(json!({"type":"screen_error","message":e}))
                            } else {
                                self.fail(&e)
                            }
                        }
                    }
                }
            }
            Event::Started(token, error) => {
                if token == self.epoch {
                    self.starting = false;
                    if let Some(e) = error {
                        self.fail(&e)
                    } else if self.gate.eligible(self.hours()) {
                        match self.start_microphone() {
                            Ok(()) => {
                                self.active = true;
                                self.last_screen = Instant::now() - Duration::from_secs(86400);
                                self.status("recording")
                            }
                            Err(e) => self.fail(&e),
                        }
                    } else {
                        self.stop("suspended")
                    }
                }
            }
            Event::Stopped(token, error) => {
                if let Some(e) = error {
                    emit(
                        json!({"type":"error","message":format!("System audio stop reported: {e}")}),
                    );
                }
                if AUDIO.get().unwrap().flush(token).is_err() {
                    self.quit = true;
                }
            }
            Event::Flushed(_token) => {
                self.stopping = false;
                self.stop_since = None;
                emit(json!({"type":"capture_drained"}));
                if self.quit_requested {
                    emit(json!({"type":"quit"}));
                    self.quit = true;
                }
            }
            Event::Screenshot(token, sample, result) => {
                if token != self.epoch || !self.active {
                    return;
                }
                if self.screens_deferred || sample.sequence != self.screen_sequence {
                    self.screen_busy = false;
                    return;
                }
                match result {
                    Err(e) => {
                        self.screen_busy = false;
                        emit(json!({"type":"screen_error","message":e}));
                    }
                    Ok(image) => {
                        if self
                            .screen_worker
                            .try_send(ScreenJob {
                                epoch: token,
                                image,
                                sample,
                                interval: self.settings.screen_seconds,
                            })
                            .is_err()
                        {
                            self.screen_busy = false;
                            emit(
                                json!({"type":"screen_error","message":"OCR worker capacity reached"}),
                            );
                        }
                    }
                }
            }
            Event::ScreenDone(token, result) => {
                if token != self.epoch || !self.active {
                    return;
                }
                self.screen_busy = false;
                match result {
                    Ok(v) => emit(v),
                    Err(e) => emit(json!({"type":"screen_error","message":e})),
                }
            }
        }
    }
}
fn audio_worker(root: PathBuf, rx: capture_queue::Receiver) {
    unsafe {
        pthread_set_qos_class_self_np(0x19, 0);
    }
    let mut me = audio::Track::new(root.clone(), "me");
    let mut remote = audio::Track::new(root, "remote");
    let mut closed = 0;
    for message in rx {
        match message {
            capture_queue::Message::Samples(p) => {
                if p.epoch <= closed {
                    continue;
                }
                if let Err(e) = if p.me {
                    me.push_queued(&p)
                } else {
                    remote.push_queued(&p)
                } {
                    fault(e)
                }
            }
            capture_queue::Message::Flush(epoch) => {
                closed = closed.max(epoch);
                if let Err(e) = me.finish().and_then(|_| remote.finish()) {
                    fault(e)
                }
                event(Event::Flushed(epoch));
            }
        }
    }
}
fn main() {
    if let Err(e) = run() {
        emit(json!({"type":"error","message":e}));
        std::process::exit(1)
    }
}
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--benchmark-ocr") => {
            emit(ocr_bench::run(&args[2..])?);
            return Ok(());
        }
        Some("--self-test-screen-frame") => {
            let path = std::path::Path::new(args.get(2).ok_or("Synthetic image path required")?);
            let sample = ScreenSample {
                session: "fixture".into(),
                target: "fixture".into(),
                sequence: 1,
                at: 0,
            };
            let frame =
                unsafe { screen_reading(&clipboard::load_image(path)?, sample, 5, &mut None)? };
            emit(frame);
            return Ok(());
        }
        Some("--make-ocr-fixture") => return ocr_bench::fixture(&args[2..]),
        Some(mode @ ("--read-clipboard" | "--ocr-file" | "--self-test-clipboard")) => {
            let path = std::path::Path::new(args.get(2).ok_or("Image path required")?);
            let result = unsafe {
                match mode {
                    "--read-clipboard" => {
                        let version = args
                            .get(3)
                            .and_then(|arg| arg.parse().ok())
                            .ok_or("Clipboard version required")?;
                        clipboard::read(path, version)
                    }
                    "--ocr-file" => clipboard::ocr_file(path),
                    _ => clipboard::self_test(path),
                }
            };
            emit(result?);
            return Ok(());
        }
        _ => {}
    }
    if args.iter().any(|a| a == "--self-test-backpressure") {
        return test_backpressure();
    }
    if args.iter().any(|a| a == "--self-test-day-queue") {
        emit(capture_queue::verify_day());
        return Ok(());
    }
    if let Some(index) = args.iter().position(|a| a == "--self-test-meters") {
        let seconds = args
            .get(index + 1)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(3)
            .clamp(1, 10);
        return meters::self_test(seconds);
    }
    if args.iter().any(|a| a == "--self-test-bounds") {
        emit(audio::verify_bounds());
        return Ok(());
    }
    if args.iter().any(|a| a == "--self-test-state") {
        state::verify();
        emit(
            json!({"type":"test","result":"PASS Rust manual start, schedule, independent lock/sleep/session gates, persistent Stop/Pause"}),
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--self-test-audio") {
        audio::self_test(PathBuf::from(args.last().unwrap()))?;
        emit(
            json!({"type":"test","result":"PASS Rust/Speechworks separate resampled tracks; no capture"}),
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--self-test-denied") {
        emit(
            json!({"type":"error","message":"Screen Recording permission missing (synthetic denial)"}),
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--self-test-image") {
        return unsafe {
            let root = PathBuf::from("/tmp/daybook-rust-image-fixture");
            let image = screen::synthetic_image()?;
            let path = screen::save_jpeg(&image, &root)?;
            emit(json!({"type":"test","path":root.join(path)}));
            Ok(())
        };
    }
    if args.iter().any(|a| a == "--self-test-ocr") {
        return unsafe {
            let image = screen::synthetic_image()?;
            let mut previous = None;
            let sample = |sequence, target: &str| ScreenSample {
                session: "fixture".into(),
                sequence,
                target: target.into(),
                at: 100 + sequence as i64 * 5,
            };
            let first = screen_reading(&image, sample(1, "a"), 5, &mut previous)?;
            let text = first["text"].as_str().ok_or("Missing text")?;
            let words = first["words"].as_array().ok_or("Missing word boxes")?;
            if !text.contains("Daybook") || words.len() < 3 {
                return Err("Synthetic OCR fixture mismatch".into());
            }
            for word in words {
                if word["width"].as_f64().unwrap_or(0.0) <= 0.0 || word["line"] != 0 {
                    return Err("OCR word box invalid".into());
                }
            }
            let same = screen_reading(&image, sample(2, "a"), 5, &mut previous)?;
            let moved = screen_reading(&image, sample(3, "b"), 5, &mut previous)?;
            let blank = screen::drawn_image(800, 180, 0)?;
            let cleared = screen_reading(&blank, sample(4, "b"), 5, &mut previous)?;
            if same["type"] != "screen_unchanged"
                || same["sample"]["at"] != 110
                || !same["text"].is_null()
                || moved["text"] != first["text"]
                || cleared["text"] != ""
                || first["pixels"] != json!([800, 180])
                || first["line_boxes"][0]["height"]
                    .as_f64()
                    .unwrap_or_default()
                    <= 0.0
                || words[0]["index"] != 0
                || !first["jpeg"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("/9j/"))
                || same.get("jpeg").is_some()
                || !cleared["jpeg"].is_string()
                || first.as_object().unwrap().len() != 7
            {
                return Err(
                    "Passive screen change detection or screenshot payload mismatch".into(),
                );
            }
            emit(
                json!({"type":"test","result":"PASS native Vision positions, JPEGs, repeated frames, window changes and blank screens","text":text,"words":words.len()}),
            );
            Ok(())
        };
    }
    if args.len() != 4 {
        return Err("usage: daybook-native DATA CONFIG UI_URL".into());
    }
    let root = PathBuf::from(&args[1]);
    let settings: Settings =
        serde_json::from_slice(&std::fs::read(&args[2]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if !args[3].starts_with("http://127.0.0.1:") {
        return Err("Control window requires a local loopback URL".into());
    }
    let (tx, rx) = mpsc::sync_channel(64);
    EVENTS
        .set(tx.clone())
        .map_err(|_| "events already initialized")?;
    let (atx, arx) = capture_queue::bounded_queue();
    AUDIO.set(atx).map_err(|_| "audio already initialized")?;
    let audio_root = root.clone();
    meters::start()?;
    std::thread::Builder::new()
        .name("daybook-audio".into())
        .spawn(move || audio_worker(audio_root, arx))
        .map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) if line.len() < 1024 => {
                    if tx.send(Event::Command(line)).is_err() {
                        return;
                    }
                }
                _ => break,
            }
        }
        let _ = tx.send(Event::Command("quit".into()));
    });
    unsafe {
        let mut app = App::new(root, settings, args[3].clone())?;
        if std::env::var("DAYBOOK_SHOW_UI").ok().as_deref() == Some("1") {
            app.open();
        }
        let mut last_tick = Instant::now();
        while !app.quit {
            autoreleasepool(|_| {
                for _ in 0..64 {
                    match rx.try_recv() {
                        Ok(e) => app.handle(e),
                        Err(_) => break,
                    }
                }
                if FAULT.swap(false, Ordering::AcqRel) && app.gate.armed {
                    app.fail("Native queue/format fault; recording paused. See error event.");
                }
                if last_tick.elapsed() > Duration::from_secs(1) {
                    app.tick();
                    last_tick = Instant::now();
                }
                let until: Obj = msg_send![class(c"NSDate"),dateWithTimeIntervalSinceNow:0.02f64];
                let input: *mut AnyObject = msg_send![&*app.app,nextEventMatchingMask:usize::MAX, untilDate:&*until, inMode:&*string("kCFRunLoopDefaultMode"), dequeue:true];
                if !input.is_null() {
                    let _: () = msg_send![&*app.app,sendEvent:input];
                }
                let _: () = msg_send![&*app.app, updateWindows];
            });
        }
        EPOCH.store(0, Ordering::Release);
        if let Some(child) = &mut app.ui_child {
            let _ = child.kill();
            let _ = child.wait();
        }
        dispatch_release(app.queue);
    }
    Ok(())
}

// Generated PCM through the real queue, resampler, meter publisher and stdout
// transport. A test reader deliberately stalls the pipe; no capture APIs run.
fn test_backpressure() -> Result<(), String> {
    let (tx, rx) = capture_queue::bounded_queue();
    EPOCH.store(1, Ordering::Release);
    meters::start()?;
    let worker = std::thread::spawn(move || {
        audio_worker(
            std::env::temp_dir().join("daybook-unused-synthetic-root"),
            rx,
        )
    });
    let began = Instant::now();
    let at = stamp();
    let rates = [44100usize, 16000];
    let mut frames = [0usize; 2];
    let mut sequence = [0usize; 2];
    let mut peak_bytes = 0;
    let mut peak_packets = 0;
    while frames[0] < rates[0] * 12 || frames[1] < rates[1] * 12 {
        for track in 0..2 {
            let n = (rates[track] / 100 * (sequence[track] % 4 + 1))
                .min(rates[track] * 12 - frames[track]);
            if n > 0
                && began.elapsed().as_secs_f64() >= (frames[track] + n) as f64 / rates[track] as f64
            {
                tx.admit(audio::Packet {
                    epoch: 1,
                    me: track == 0,
                    rate: rates[track],
                    pcm: vec![0.01; n],
                    at: at + frames[track] as f64 / rates[track] as f64,
                })?;
                frames[track] += n;
                sequence[track] += 1;
            }
        }
        let m = tx.metrics();
        peak_bytes = peak_bytes.max(m["queue_pcm_bytes"].as_u64().unwrap());
        peak_packets = peak_packets.max(m["queue_packets"].as_u64().unwrap());
        std::thread::sleep(Duration::from_millis(1));
    }
    tx.flush(1)?;
    let metrics = tx.clone();
    drop(tx);
    // The worker ends after Flush only once all senders have closed.
    let final_metrics = loop {
        let m = metrics.metrics();
        if m["queue_packets"] == 0 {
            break m;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(metrics);
    worker.join().map_err(|_| "Synthetic audio worker failed")?;
    EPOCH.store(0, Ordering::Release);
    emit(
        json!({"type":"test","result":"completed synthetic backpressure transport","at":at,"elapsed_seconds":began.elapsed().as_secs_f64(),"generated_frames":frames,"sample_rates":rates,"peak_pcm_bytes":peak_bytes,"peak_packets":peak_packets,"queue":final_metrics,"fault_events":FAULT_EVENTS.load(Ordering::Relaxed)}),
    );
    Ok(())
}
