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
    columns_fixture(&mut c, false, false);
    // Keep the normal startup/jump behavior; previews must exercise the real reader.
    c
}
fn columns_fixture(c: &mut Controls, passive: bool, empty: bool) {
    use crate::transcript_range::*;
    let from = chrono::Local::now()
        .date_naive()
        .and_hms_opt(10, 14, 0)
        .unwrap()
        .and_local_timezone(chrono::Local)
        .earliest()
        .unwrap()
        .timestamp();
    let range = Span {
        from,
        until: from + 60,
    };
    let overview = Span {
        from,
        until: from + 600,
    };
    let passages = if empty {
        vec![]
    } else {
        [(3,12,"transcript","remote","Did you record the ferry arriving?"),
         (6,6,"screen","screenshot","Waterfront walk · Route map"),
         (6,16,"screen","passive_screen","Waterfront walk"),
         (9,18,"transcript","me","I was standing on the stone steps. You can hear the rigging before the boat comes into view."),
         (20,26,"transcript","remote","Keep that quiet opening."),
         (23,23,"note","clipboard","Saturday · second pass at the stone steps"),
         (36,46,"transcript","me","I’ll bring the recorder back on Saturday and walk the route again."),
         (40,50,"screen","passive_screen","Field recording · Saturday"),
         (47,54,"transcript","remote","Let’s leave a little room for the sound of the water.")]
        .into_iter().enumerate().filter_map(|(i,(a,b,k,s,t))|{
          let lane=Lane::of(k,s)?;
          if lane==Lane::System && passive || lane==Lane::Passive && !passive{return None}
          Some(Passage{id:50+i as i64,at:from+a,end:from+b,kind:k.into(),source:s.into(),text:t.into(),image_id:(s=="screenshot").then_some(51),truncated:false})
        }).collect::<Vec<_>>()
    };
    let mut nearby = passages.clone();
    if !empty {
        for (i, (t, k, src, text)) in [
            (
                78,
                "transcript",
                "me",
                "The ferry timetable lists the Saturday crossing.",
            ),
            (
                90,
                "transcript",
                "remote",
                "Catch the bell as it leaves the waterfront.",
            ),
            (
                144,
                "screen",
                "passive_screen",
                "Saturday departures · Waterfront",
            ),
            (156, "note", "clipboard", "Ferry tickets · $125"),
            (
                190,
                "transcript",
                "me",
                "The Market Street entrance is the second recording stop.",
            ),
            (
                217,
                "transcript",
                "remote",
                "Keep the market sounds separate from the ferry.",
            ),
            (
                300,
                "screen",
                "screenshot",
                "Market Street · recording locations",
            ),
            (
                326,
                "transcript",
                "me",
                "Saturday works for the last crossing.",
            ),
            (
                385,
                "transcript",
                "remote",
                "Ask for the ferry captain by name.",
            ),
            (450, "screen", "passive_screen", "Field Notes · Saturday"),
            (
                515,
                "transcript",
                "me",
                "We can finish the route at the stone steps.",
            ),
            (
                550,
                "transcript",
                "remote",
                "Keep the waterfront version for the listening walk.",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let lane = Lane::of(k, src).unwrap();
            if lane == Lane::Passive && !passive || lane == Lane::System && passive {
                continue;
            }
            let end = t + if lane == Lane::Saves { 0 } else { 8 };
            nearby.push(Passage {
                id: 200 + i as i64,
                at: from + t,
                end: from + end,
                kind: k.into(),
                source: src.into(),
                text: text.into(),
                image_id: (src == "screenshot").then_some(200 + i as i64),
                truncated: false,
            });
        }
    }
    let bands = nearby
        .iter()
        .map(|p| Cluster {
            lane: p.lane(),
            range: Span {
                from: p.at,
                until: p.end.max(p.at + 1),
            },
            count: 1,
            excerpt: p.text.clone(),
        })
        .collect();
    c.parallel.apply(Window {
        range,
        extent: overview,
        overview,
        bands,
        nearby,
        nearby_grouped: false,
        passages,
        clusters: vec![],
        quiet_at: None,
    });
    c.reading.passive_screen = passive;
}
pub(super) fn archive_fixture(c: &mut Controls, selection: bool) {
    let day = chrono::Local::now().date_naive();
    let local = |h, m| {
        day.and_hms_opt(h, m, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap()
            .timestamp()
    };
    let start = local(0, 0);
    let end = day
        .succ_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono::Local)
        .earliest()
        .unwrap()
        .timestamp();
    c.tab = 1;
    c.day = day.to_string();
    c.day_edit = c.day.clone();
    c.items=[
        (15,42,"note","me","Start the listening walk at the stone steps. The first thing you hear there is the rigging, before the ferry comes into view."),
        (15,34,"screen","screenshot","Waterfront walk · route map and recording stops"),
        (15,22,"transcript","remote","Give people a moment on the water before they turn toward the market."),
        (15,9,"note","clipboard","Saturday ferry times: 8:15, 9:00 and 9:45."),
        (14,52,"transcript","me","I’ll take the recorder back on Saturday and get a second pass."),
        (14,28,"screen","clipboard","Field notes from the first walk"),
        (10,12,"note","me","Bring spare batteries and the wind cover."),
        (9,35,"transcript","me","Let’s follow the shoreline from the station.")
    ].iter().enumerate().map(|(i,(h,m,k,s,t))|json!({"id":20-i,"at":local(*h,*m),"kind":k,"source":s,"text":t})).collect();
    let mut bins = vec![0u32; ((end - start) / 900) as usize];
    for i in &c.items {
        bins[((i["at"].as_i64().unwrap() - start) / 900) as usize] += 1;
    }
    c.archive.activity = json!({"start":start,"end":end,"bins":bins});
    if selection {
        let (a, b) = (local(14, 20), local(15, 50));
        c.archive.selection = Some((a, b));
        c.items
            .retain(|i| i["at"].as_i64().is_some_and(|t| t >= a && t < b));
    }
    c.archive.layout(&c.items);
    c.archive.loaded_filter = c.filter();
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
    let passive = enabled("--passive");
    let ten_minutes = enabled("--ten-minutes");
    let screen_history = enabled("--screen-history");
    let fixture_dir = args
        .iter()
        .position(|s| s == "--fixture-dir")
        .and_then(|i| args.get(i + 1));
    let endpoint = fixture_dir.map(|dir| {
        let url = std::fs::read_to_string(std::path::Path::new(dir).join("control-url.txt"))
            .expect("synthetic journal address");
        let url = url.trim();
        assert!(
            url.starts_with("http://127.0.0.1:")
                && call(url, "/overview", None).is_ok_and(|v| v["synthetic"] == true),
            "a synthetic local journal is required"
        );
        url.to_owned()
    });
    let archive = enabled("--archive");
    let selection = enabled("--selection");
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
            if let Some(base) = endpoint {
                let mut c = Controls::new(cc, base);
                if screen_history {
                    c.navigate(5);
                }
                if ten_minutes {
                    c.parallel.seconds = 600;
                }
                c.focus = focus;
                c.preview_output = Some(path);
                return Ok(Box::new(c));
            }
            design::setup(&cc.egui_ctx);
            let (tx, _requests) = mpsc::sync_channel(12);
            let (_replies, rx) = mpsc::sync_channel(12);
            let mut c = fixture(tx, rx);
            c.preview_output = Some(path);
            columns_fixture(&mut c, passive, empty);
            if ten_minutes {
                let r = c.parallel.data.as_ref().unwrap().overview;
                c.parallel.set_range(r);
            }
            if archive {
                archive_fixture(&mut c, selection);
            }
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
