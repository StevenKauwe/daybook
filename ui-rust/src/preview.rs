//! Developer-only render of synthetic content, without network or capture APIs.
use super::*;
pub(super) fn fixture(tx: SyncSender<Request>, rx: Receiver<Reply>) -> Controls {
    let mut c = Controls::initial("synthetic://design".into(), tx, rx);
    c.loaded = true;
    c.last_seen = Some(Instant::now());
    let now = chrono::Utc::now().timestamp();
    c.status = json!({"active":"true","armed":"true","pending":2,"status":"synthetic recording","synthetic":true,"config":{"audio_storage":"memory"},"health":{"input_levels":{"me":{"end_at":now,"rms_dbfs":-24.0,"peak_dbfs":-12.0},"remote":{"end_at":now,"rms_dbfs":-36.0,"peak_dbfs":-18.0}},"at":now,"expired_sources":0,"lost_sources":0,"speech_blocker":"","loaded_speech_model":{"loaded":true,"actual_backend":"Metal"},"preview_latency":{"source_end_to_result_ms":184},"tracks":[{"track":"me","last_source_age_seconds":0},{"track":"remote","last_source_age_seconds":1}]}});
    c.saved_config = serde_json::to_value(&c.config).unwrap();
    let lines = [
        (
            "me",
            "I went down to the waterfront before sunrise. The first sound you hear is the rigging on the boats, long before the ferry comes into view.",
        ),
        ("me", "I’d love to open the listening walk with that."),
        (
            "remote",
            "Give people a moment there. If the first stop is just the water and the wind, the market will feel completely different when they turn the corner.",
        ),
        (
            "me",
            "Let’s keep that quiet opening. I’ll take the recorder back on Saturday and get a second pass from the stone steps.",
        ),
    ];
    let items=lines.iter().enumerate().map(|(i,(source,text))|json!({"id":i+1,"at":now-40+i as i64*8,"end":now-32+i as i64*8,"kind":if i==3{"preview"}else{"transcript"},"source":source,"text":text,"truncated":false})).collect::<Vec<_>>();
    c.set_stream_page(json!({"day":chrono::Local::now().date_naive().to_string(),"items":items,"older":null,"newer":null,"total":4,"limit":80}));
    let at = chrono::Utc::now().timestamp_millis() as f64 / 1000.;
    for frame in 0..64 {
        for (i, track) in ["me", "remote"].into_iter().enumerate() {
            let t = frame as f64 * 0.05;
            let db = sample_db(t, i);
            let end = at - (63 - frame) as f64 * 0.05;
            c.traces[i].observe(None, end, db, db + 5.);
            if frame == 63 {
                c.status["health"]["input_levels"][track] =
                    json!({"end_at":end,"rms_dbfs":db,"peak_dbfs":db+5.});
            }
        }
    }
    // Keep the normal startup/jump behavior; previews must exercise the real reader.
    c
}
pub(super) fn sample_db(t: f64, track: usize) -> f32 {
    let x = t + track as f64 * 2.19;
    let envelope =
        ((x * 3.3).sin() * 0.54 + (x * 7.2).cos() * 0.32 + (x * 13.7).sin() * 0.14 + 0.57)
            .clamp(0., 1.);
    (-76. + envelope * (38. + (x * 32.).cos().abs() * 23.)) as f32
}
pub(super) fn run(path: std::path::PathBuf, args: &[String]) {
    let enabled = |option: &str| args.iter().any(|a| a == option);
    let (focus, compact, motion, empty, night) = (
        enabled("--focus"),
        enabled("--compact"),
        enabled("--motion"),
        enabled("--empty"),
        enabled("--night"),
    );
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Daybook — synthetic design preview")
            .with_inner_size(if compact {
                [820.0, 660.0]
            } else {
                [1260.0, 880.0]
            }),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Daybook design preview",
        options,
        Box::new(move |cc| {
            design::setup(&cc.egui_ctx);
            let (tx, _requests) = mpsc::sync_channel(12);
            let (_replies, rx) = mpsc::sync_channel(12);
            let mut c = fixture(tx, rx);
            c.preview_output = Some(path);
            if empty {
                c.set_stream_page(
                    json!({"day":chrono::Local::now().date_naive().to_string(),"items":[]}),
                );
                c.status["active"] = json!("false");
                c.status["armed"] = json!("false");
                c.status["health"]["input_levels"] = json!({});
            }
            c.reading.night = night;
            c.focus = focus;
            c.preview_motion = motion;
            Ok(Box::new(c))
        }),
    );
    if let Err(e) = result {
        eprintln!("Design preview: {e}");
    }
}
