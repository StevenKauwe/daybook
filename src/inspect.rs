//! Local-only inspection. No captured content, credentials or telemetry is sent out.
use crate::{
    config::Config,
    store::{Result, Store, now},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::time::Instant;
#[derive(Default)]
pub struct Sampler {
    previous: Option<(Instant, f64)>,
}
#[cfg(target_os = "macos")]
fn current_process_memory() -> Option<(u64, u64)> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
    // Darwin's void** signature takes the address of the versioned output struct.
    // A successful call initializes all fields of rusage_info_v2.
    let result = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as libc::pid_t,
            libc::RUSAGE_INFO_V2,
            usage.as_mut_ptr().cast(),
        )
    };
    if result != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    Some((usage.ri_phys_footprint, usage.ri_resident_size))
}
#[cfg(not(target_os = "macos"))]
fn current_process_memory() -> Option<(u64, u64)> {
    None
}

impl Sampler {
    pub fn sample(&mut self) -> Value {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return json!({"available":false});
        }
        let cpu = usage.ru_utime.tv_sec as f64
            + usage.ru_utime.tv_usec as f64 / 1e6
            + usage.ru_stime.tv_sec as f64
            + usage.ru_stime.tv_usec as f64 / 1e6;
        let at = Instant::now();
        let percent = self.previous.map(|(last, old)| {
            (cpu - old).max(0.0) / at.duration_since(last).as_secs_f64().max(0.001) * 100.0
        });
        self.previous = Some((at, cpu));
        let memory = current_process_memory();
        json!({"available":true,"pid":std::process::id(),"cpu_core_percent":percent,"cpu_seconds":cpu,"peak_rss_bytes":usage.ru_maxrss,"current_rss_bytes":memory.map(|m|m.1),"physical_footprint_bytes":memory.map(|m|m.0),"scope":"backend and inference threads only; excludes capture helper and controls","measurement":"macOS proc_pid_rusage physical footprint and current RSS, getrusage peak RSS and CPU percent of one core"})
    }
}
fn scalar(s: &Store, sql: &str) -> Result<i64> {
    Ok(s.db.query_row(sql, [], |r| r.get(0))?)
}
fn input_levels(s: &Store) -> Value {
    json!({"me":serde_json::from_str::<Value>(&s.get("input_level_me")).unwrap_or(Value::Null),"remote":serde_json::from_str::<Value>(&s.get("input_level_remote")).unwrap_or(Value::Null)})
}
pub fn live(s: &Store) -> Result<Value> {
    let mut live = Vec::new();
    for track in ["me", "remote"] {
        let mut q=s.db.prepare("SELECT id,at,end_at,kind,substr(text,1,4000) FROM journal_entries WHERE source=?1 AND kind IN ('preview','transcript') ORDER BY id DESC LIMIT 1")?;
        live.extend(q.query_map([track],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"kind":r.get::<_,String>(3)?,"text":r.get::<_,String>(4)?,"track":track})))?.collect::<std::result::Result<Vec<_>,_>>()?);
    }
    Ok(
        json!({"at":now(),"input_levels":input_levels(s),"capture_starting":s.get("capture_starting"),"capture_stopping":s.get("capture_stopping"),"active":s.get("active"),"armed":s.get("armed"),"status":s.get("status"),"capture_warning":s.get("capture_warning"),"pending":s.pending(),"live_transcription":live,"speech_activity":{"me":serde_json::from_str::<Value>(&s.get("voice_activity_me")).unwrap_or(Value::Null),"remote":serde_json::from_str::<Value>(&s.get("voice_activity_remote")).unwrap_or(Value::Null)},"silence_filtered_sources":scalar(s,"SELECT count(*) FROM source_metadata WHERE json_extract(details,'$.speech_gate')='no_speech'")?,"speech_blocker":s.get("speech_blocker"),"expired_sources":scalar(s,"SELECT count(*) FROM sources WHERE status='expired'")?,"lost_sources":scalar(s,"SELECT count(*) FROM sources WHERE status='lost'")?,"memory_rejected_frames":s.get("memory_rejected_frames").parse::<u64>().ok(),"loaded_speech_model":serde_json::from_str::<Value>(&s.get("loaded_speech_model")).unwrap_or(Value::Null)}),
    )
}
pub fn health(s: &Store, c: &Config, sampler: &mut Sampler) -> Result<Value> {
    let live = live(s)?["live_transcription"].clone();
    let mut tracks = Vec::new();
    for track in ["me", "remote"] {
        let mut q=s.db.prepare("SELECT id,path,at,end_at,status FROM sources WHERE track=?1 AND status IN ('pending','running','streaming') ORDER BY id LIMIT 1001")?;
        let mut count = 0;
        let mut frames = 0u64;
        let mut bytes = 0u64;
        let mut unknown = 0;
        let mut oldest = None;
        for row in q.query_map([track], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })? {
            let (row_id, path, at) = row?;
            count += 1;
            oldest = Some(oldest.map_or(at, |v: i64| v.min(at)));
            if crate::live_audio::valid_path(&path) {
                let n:Option<i64>=s.db.query_row("SELECT json_extract(details,'$.frames') FROM source_metadata WHERE source_id=?1",[row_id],|r|r.get(0)).optional()?.flatten();
                if let Some(n) = n {
                    frames += n as u64;
                    bytes += n as u64 * 4;
                } else {
                    unknown += 1;
                }
            } else if crate::worker::valid_relative(&path, "audio") {
                if let Ok(m) = std::fs::metadata(s.root.join(&path)) {
                    bytes += m.len();
                    if path.ends_with(".f32") {
                        frames += m.len() / 4;
                    } else {
                        unknown += 1;
                    }
                } else {
                    unknown += 1;
                }
            } else {
                unknown += 1;
            }
        }
        let last: Option<i64> = s.db.query_row(
            "SELECT max(end_at) FROM sources WHERE track=?1",
            [track],
            |r| r.get(0),
        )?;
        tracks.push(json!({"track":track,"pending_files":count,"pcm_frames":frames,"pcm_seconds":frames as f64/16000.0,"file_bytes":bytes,"unknown_files":unknown,"oldest_age_seconds":oldest.map(|v|(now()-v).max(0)),"last_source_at":last,"last_source_age_seconds":last.map(|v|(now()-v).max(0)),"scan_limit":1001}));
    }
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let budget=s.db.query_row("SELECT requests,images,tokens FROM budget WHERE day=?1",[&day],|r|Ok(json!({"requests":r.get::<_,i64>(0)?,"images":r.get::<_,i64>(1)?,"reserved_units":r.get::<_,i64>(2)?}))).optional()?.unwrap_or(json!({"requests":0,"images":0,"reserved_units":0}));
    let mut q=s.db.prepare("SELECT id,started,role,model,origin,is_local,status,elapsed_ms,input_bytes,output_bytes,reserved_units,prompt_tokens,completion_tokens,estimated_usd FROM provider_activity ORDER BY id DESC LIMIT 30")?;
    let activity=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"role":r.get::<_,String>(2)?,"model":r.get::<_,String>(3)?,"origin":r.get::<_,String>(4)?,"local":r.get::<_,bool>(5)?,"status":r.get::<_,String>(6)?,"elapsed_ms":r.get::<_,Option<i64>>(7)?,"input_bytes":r.get::<_,i64>(8)?,"output_bytes":r.get::<_,Option<i64>>(9)?,"reserved_units":r.get::<_,i64>(10)?,"prompt_tokens":r.get::<_,Option<i64>>(11)?,"completion_tokens":r.get::<_,Option<i64>>(12)?,"estimated_usd":r.get::<_,Option<f64>>(13)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let usage=s.db.query_row("SELECT reported_prompt,reported_completion,estimated_usd,priced_calls,completed_calls,reported_calls FROM provider_daily WHERE day=?1",[&day],|r|Ok(json!({"reported_prompt_tokens":r.get::<_,i64>(0)?,"reported_completion_tokens":r.get::<_,i64>(1)?,"estimated_usd":r.get::<_,f64>(2)?,"priced_calls":r.get::<_,i64>(3)?,"completed_calls":r.get::<_,i64>(4)?,"reported_calls":r.get::<_,i64>(5)?}))).optional()?.unwrap_or(json!({"reported_prompt_tokens":0,"reported_completion_tokens":0,"estimated_usd":0.0,"priced_calls":0,"completed_calls":0,"reported_calls":0}));
    let mut q = s.db.prepare(
        "SELECT id,at,text FROM observations WHERE kind='event' ORDER BY id DESC LIMIT 20",
    )?;
    let events=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"text":r.get::<_,String>(2)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut q=s.db.prepare("SELECT id,kind,status,substr(error,1,400),retry_at FROM jobs WHERE status!='done' ORDER BY id DESC LIMIT 20")?;
    let queue=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"error":r.get::<_,String>(3)?,"retry_at":r.get::<_,i64>(4)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut q=s.db.prepare("SELECT id,track,status,substr(error,1,400),attempts,retry_at FROM sources WHERE status IN ('pending','running','streaming') ORDER BY CASE WHEN error!='' THEN 0 ELSE 1 END,id LIMIT 20")?;
    let pending_sources=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"track":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"error":r.get::<_,String>(3)?,"attempts":r.get::<_,i64>(4)?,"retry_at":r.get::<_,i64>(5)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(
        json!({"input_levels":input_levels(s),"live_transcription":live,"speech_activity":{"me":serde_json::from_str::<Value>(&s.get("voice_activity_me")).unwrap_or(Value::Null),"remote":serde_json::from_str::<Value>(&s.get("voice_activity_remote")).unwrap_or(Value::Null)},"silence_filtered_sources":scalar(s,"SELECT count(*) FROM source_metadata WHERE json_extract(details,'$.speech_gate')='no_speech'")?,"speech_blocker":s.get("speech_blocker"),"loaded_speech_model":serde_json::from_str::<Value>(&s.get("loaded_speech_model")).unwrap_or(Value::Null),"at":now(),"pending_sources":pending_sources,"job_queue":queue,"tracks":tracks,"native":serde_json::from_str::<Value>(&s.get("native_metrics")).unwrap_or(Value::Null),"native_seen_at":s.get("native_seen_at").parse::<i64>().ok(),"backend":sampler.sample(),"preview_latency":serde_json::from_str::<Value>(&s.get("speech_preview_metrics")).unwrap_or(Value::Null),"final_latency":serde_json::from_str::<Value>(&s.get("speech_final_metrics")).unwrap_or(Value::Null),"inference":serde_json::from_str::<Value>(&s.get("inference_metrics")).unwrap_or(Value::Null),"queue_capacity_files":c.max_pending,"failed_jobs":scalar(s,"SELECT count(*) FROM jobs WHERE status='failed'")?,"unfinished_jobs":scalar(s,"SELECT count(*) FROM jobs WHERE status IN ('pending','running','failed')")?,"expired_sources":scalar(s,"SELECT count(*) FROM sources WHERE status='expired'")?,"lost_sources":scalar(s,"SELECT count(*) FROM sources WHERE status='lost'")?,"memory_rejected_frames":s.get("memory_rejected_frames").parse::<u64>().ok(),"budget":budget,"usage":usage,"day":day,"activity":activity,"events":events,"data_directory":s.root,"telemetry":"local SQLite only; provider activity keeps latest 1000 attempts; daily totals retained","actual_billed_usd":Value::Null,"unknown":"Actual billed cost, full-app current RSS and whole-app CPU are unavailable. Provider token counts are reported, not independently measured; configured dollar rates yield estimates only."}),
    )
}
pub fn day_bounds(day: &str) -> Result<(i64, i64)> {
    let date = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")?;
    if date.format("%Y-%m-%d").to_string() != day {
        return Err("Use YYYY-MM-DD".into());
    }
    let start = date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono::Local)
        .earliest()
        .ok_or("Local midnight unavailable")?
        .timestamp();
    let end = date
        .succ_opt()
        .ok_or("date out of range")?
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono::Local)
        .earliest()
        .ok_or("Local midnight unavailable")?
        .timestamp();
    Ok((start, end))
}
/// A bounded, chronological reading window. Cursors use timestamp + ID so
/// simultaneous tracks and provisional-to-final updates never skip a passage.
pub fn transcript(s: &Store, v: &Value) -> Result<Value> {
    let day = v["day"].as_str().ok_or("day missing")?;
    let (start, end) = day_bounds(day)?;
    let newer = v["after_id"].is_i64();
    if newer && v["before_id"].is_i64() {
        return Err("Choose one transcript direction".into());
    }
    let at = v[if newer { "after_at" } else { "before_at" }]
        .as_i64()
        .unwrap_or(if newer { i64::MIN } else { i64::MAX });
    let id = v[if newer { "after_id" } else { "before_id" }]
        .as_i64()
        .unwrap_or(if newer { i64::MIN } else { i64::MAX });
    let cmp = if newer { ">" } else { "<" };
    let order = if newer { "ASC" } else { "DESC" };
    let passive = v["passive_screen"] == true;
    let scope = if passive {
        "((at>=?1 AND at<?2 AND ((kind IN ('preview','transcript') AND source='me') OR (kind='screen' AND source IN ('passive_screen','screen','vision') AND trim(text)<>''))) OR (kind='screen' AND source='passive_screen' AND at<?1 AND end_at>at AND end_at>=?1))"
    } else {
        "at>=?1 AND at<?2 AND kind IN ('preview','transcript')"
    };
    let scope = if v["with_saves"] == true {
        format!(
            "(({scope}) OR (at>=?1 AND at<?2 AND ((kind='screen' AND source IN ('screenshot','clipboard')) OR (kind='note' AND source='clipboard'))))"
        )
    } else {
        scope.to_owned()
    };
    let mut q = s.db.prepare(&format!("SELECT id,at,end_at,kind,source,substr(text,1,4000),length(text)>4000,CASE WHEN kind='screen' AND source IN ('screenshot','clipboard') THEN (SELECT observation_id FROM saved_images WHERE observation_id=journal_entries.id) END FROM journal_entries WHERE {scope} AND (at{cmp}?3 OR (at=?3 AND id{cmp}?4)) ORDER BY at {order},id {order} LIMIT 80"))?;
    let mut rows=q.query_map(params![start,end,at,id],|r|{
        let mut row=json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"kind":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"truncated":r.get::<_,bool>(6)?});
        if let Some(id)=r.get::<_,Option<i64>>(7)? { row["image_id"]=json!(id); }
        Ok(row)
    })?.collect::<std::result::Result<Vec<_>,_>>()?;
    if !newer {
        rows.reverse();
    }
    let total: i64 = s.db.query_row(
        &format!("SELECT count(*) FROM journal_entries WHERE {scope}"),
        params![start, end],
        |r| r.get(0),
    )?;
    let mut older = Value::Null;
    let mut later = Value::Null;
    if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
        for (row, cmp, cursor) in [(first, "<", &mut older), (last, ">", &mut later)] {
            let a = row["at"].as_i64().unwrap();
            let i = row["id"].as_i64().unwrap();
            let more:bool=s.db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM journal_entries WHERE {scope} AND (at{cmp}?3 OR (at=?3 AND id{cmp}?4)))"),params![start,end,a,i],|r|r.get(0))?;
            if more {
                *cursor = if cmp == "<" {
                    json!({"before_at":a,"before_id":i})
                } else {
                    json!({"after_at":a,"after_id":i})
                };
            }
        }
    }
    Ok(
        json!({"day":day,"items":rows,"older":older,"newer":later,"total":total,"limit":80,"passive_screen":passive}),
    )
}
struct ArchiveQuery<'a> {
    day: &'a str,
    bounds: (i64, i64),
    selected: Option<(i64, i64)>,
    kind: &'a str,
    source: &'a str,
    query: &'a str,
}

impl<'a> ArchiveQuery<'a> {
    fn parse(v: &'a Value) -> Result<Self> {
        let day = v["day"].as_str().ok_or("day missing")?;
        let (start, end) = day_bounds(day)?;
        let kind = v["kind"].as_str().unwrap_or("all");
        let source = v["source"].as_str().unwrap_or("all");
        let query = v["query"].as_str().unwrap_or("");
        if query.len() > 200 {
            return Err("search is limited to 200 bytes".into());
        }
        if ![
            "all",
            "transcript",
            "preview",
            "note",
            "screen",
            "event",
            "preview_abandoned",
            "transcript_suppressed",
        ]
        .contains(&kind)
            || ![
                "all",
                "me",
                "remote",
                "screen",
                "vision",
                "passive_screen",
                "system",
                "clipboard",
                "screenshot",
            ]
            .contains(&source)
        {
            return Err("invalid timeline filter".into());
        }
        let selected = match (v.get("from"), v.get("until")) {
            (None, None) => None,
            (Some(a), Some(b)) => {
                let from = a.as_i64().ok_or("Invalid selection start")?;
                let until = b.as_i64().ok_or("Invalid selection end")?;
                if !(start <= from && from < until && until <= end) {
                    return Err("Select a time range within this day".into());
                }
                Some((from, until))
            }
            _ => return Err("Both ends of the time selection are required".into()),
        };
        Ok(Self {
            day,
            bounds: (start, end),
            selected,
            kind,
            source,
            query,
        })
    }
}

// Timestamped passages can straddle the start of a selected range. Ordinary day browsing
// keeps the existing source-date convention and its indexed range scan.
const DAY_ENTRIES: &str = "journal_entries WHERE at>=?1 AND at<?2";
pub(crate) const RANGE_ENTRIES: &str = "(SELECT * FROM journal_entries WHERE at>=?1 AND at<?2
    UNION ALL SELECT * FROM journal_entries WHERE at<?1 AND end_at>=?1 AND end_at>at AND (end_at>?1 OR source='passive_screen')) WHERE 1";
const MATCH_ENTRY: &str =
    "((?3='all' AND kind IN ('transcript','preview','note','screen')) OR kind=?3)
    AND (?4='all' OR source=?4 OR (?4='screen' AND source='passive_screen')) AND (?5='' OR instr(lower(text),lower(?5))>0)";

#[derive(Default, serde::Serialize)]
struct ArchiveSession {
    start: i64,
    until: i64,
    title: String,
    entries: usize,
    notes: usize,
    screens: usize,
    passages: usize,
    #[serde(skip)]
    title_rank: u8,
    #[serde(skip)]
    matches: bool,
}

fn archive_sessions(s: &Store, f: &ArchiveQuery<'_>) -> Result<Vec<ArchiveSession>> {
    // Establish groups from the whole day. Searching for a map should still find the
    // conversation beside it, even if "map" appears only in the screenshot.
    let (day_start, day_end) = f.bounds;
    let (from, until) = f.selected.unwrap_or(f.bounds);
    let mut q = s.db.prepare(&format!(
        "SELECT at,end_at,kind,source,substr(text,1,144),
            (({MATCH_ENTRY}) AND at<?7 AND (at>=?6 OR end_at>?6 OR (end_at=?6 AND source='passive_screen')))
         FROM {RANGE_ENTRIES} AND (kind IN ('transcript','preview','note','screen') OR kind=?3)
         ORDER BY at,id"
    ))?;
    let mut rows = q.query(params![
        day_start, day_end, f.kind, f.source, f.query, from, until
    ])?;
    let mut sessions: Vec<ArchiveSession> = Vec::new();
    let mut last_activity = day_start;
    while let Some(row) = rows.next()? {
        let at = row.get::<_, i64>(0)?.max(day_start);
        let last = row.get::<_, i64>(1)?.max(at).min(day_end);
        let end = last.max(at + 1);
        if sessions.is_empty() || at - last_activity >= 15 * 60 {
            sessions.push(ArchiveSession {
                start: at,
                until: end,
                ..Default::default()
            });
        }
        last_activity = last_activity.max(last);
        let group = sessions.last_mut().unwrap();
        group.until = group.until.max(end);
        group.entries += 1;
        group.matches |= row.get::<_, bool>(5)?;
        let kind = row.get_ref(2)?.as_str()?;
        let source = row.get_ref(3)?.as_str()?;
        let rank = match kind {
            "note" => {
                group.notes += 1;
                if source == "me" { 4 } else { 3 }
            }
            "screen" => {
                group.screens += 1;
                if matches!(source, "screenshot" | "clipboard") {
                    3
                } else {
                    1
                }
            }
            "transcript" | "preview" => {
                group.passages += 1;
                2
            }
            _ => 1,
        };
        if rank > group.title_rank {
            if let Some(line) = row
                .get_ref(4)?
                .as_str()?
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
            {
                group.title = line.chars().take(100).collect();
                group.title_rank = rank;
            }
        }
    }
    sessions.retain(|g| g.matches);
    sessions.reverse();
    Ok(sessions)
}

pub fn copy_time_range(s: &Store, v: &Value) -> Result<Value> {
    use std::fmt::Write;
    let f = ArchiveQuery::parse(v)?;
    let (start, end) = f.selected.ok_or("Select a time range first")?;
    let stamp = |at: i64| {
        chrono::DateTime::from_timestamp(at, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%H:%M:%S %Z")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let mut text = format!("Daybook · {} · {} – {}\n", f.day, stamp(start), stamp(end));
    let mut count = 0usize;
    let mut q = s.db.prepare(&format!(
        "SELECT at,kind,source,text,json_array_length(details,'$.image_ids'),end_at FROM {RANGE_ENTRIES} AND {MATCH_ENTRY} ORDER BY at,id"
    ))?;
    let mut rows = q.query(params![start, end, f.kind, f.source, f.query])?;
    while let Some(r) = rows.next()? {
        let value = r.get_ref(3)?.as_str()?;
        let kind = r.get_ref(1)?.as_str()?;
        let source = r.get_ref(2)?.as_str()?;
        let body = value.trim();
        let attachments = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
        if body.is_empty() && kind != "screen" && attachments == 0 {
            continue;
        }
        if text.len().saturating_add(body.len()).saturating_add(128) > 2 * 1024 * 1024 {
            return Err(
                "This selection exceeds 2 MiB. Choose a shorter range to copy it in full.".into(),
            );
        }
        writeln!(
            text,
            "\n{}  {}",
            if source == "passive_screen" && r.get::<_, i64>(5)? > r.get::<_, i64>(0)? {
                format!("{} – {}", stamp(r.get(0)?), stamp(r.get(5)?))
            } else {
                stamp(r.get(0)?)
            },
            crate::reading::entry_label(kind, source)
        )?;
        if !body.is_empty() {
            writeln!(text, "{body}")?;
        }
        if attachments > 0 {
            writeln!(
                text,
                "[{attachments} {}]",
                if attachments == 1 {
                    "attached image"
                } else {
                    "attached images"
                }
            )?;
        }
        count += 1;
    }
    if count == 0 {
        return Err("No matching entries in the selected time range".into());
    }
    Ok(json!({"text":text,"count":count}))
}

fn add_image_ref(s: &Store, row: &mut Value) -> Result<()> {
    if row["kind"] == "screen" && row["source"] != "passive_screen" {
        let id = row["id"].as_i64().ok_or("Missing image entry ID")?;
        row["image_id"] = json!(crate::archive_image::retained_id(&s.db, &s.root, id)?);
    }
    Ok(())
}

pub fn timeline(s: &Store, v: &Value) -> Result<Value> {
    let f = ArchiveQuery::parse(v)?;
    let day = f.day;
    let (start, end) = f.selected.unwrap_or(f.bounds);
    let time = if f.selected.is_some() {
        RANGE_ENTRIES
    } else {
        DAY_ENTRIES
    };
    let before_at = v["before_at"].as_i64().unwrap_or(i64::MAX);
    let before_id = v["before_id"].as_i64().unwrap_or(i64::MAX);
    let mut q=s.db.prepare(&format!("SELECT id,at,end_at,kind,source,substr(text,1,1200),length(text)>1200,source_id,CASE WHEN kind='note' THEN json_extract(details,'$.image_ids[0]') END FROM {time} AND {MATCH_ENTRY} AND (at<?6 OR (at=?6 AND id<?7)) ORDER BY at DESC,id DESC LIMIT 101"))?;
    let mut rows=q.query_map(params![start,end,f.kind,f.source,f.query,before_at,before_id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"kind":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"truncated":r.get::<_,bool>(6)?,"source_id":r.get::<_,Option<i64>>(7)?,"note_cover":r.get::<_,Option<i64>>(8)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let more = rows.len() > 100;
    rows.truncate(100);
    for row in &mut rows {
        add_image_ref(s, row)?;
    }
    let cursor = if more {
        rows.last()
            .map(|v| json!({"before_at":v["at"],"before_id":v["id"]}))
    } else {
        None
    };
    let mut q=s.db.prepare("SELECT id,kind,lo,hi,status,substr(text,1,2000),error FROM jobs WHERE id IN (SELECT job_id FROM job_days WHERE day=?1) ORDER BY id DESC LIMIT 50")?;
    let jobs=q.query_map([day],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"lo":r.get::<_,i64>(2)?,"hi":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"error":r.get::<_,String>(6)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut result = json!({"day":day,"items":rows,"next":cursor,"jobs":jobs,"summary_limit":50});
    if v["sessions"] == true && v["before_id"].is_null() {
        result["sessions"] = serde_json::to_value(archive_sessions(s, &f)?)?;
    }
    if v["activity"] == true && v["before_id"].is_null() {
        let (start, end) = f.bounds;
        let mut bins = vec![0u32; ((end - start + 899) / 900) as usize];
        let mut q = s.db.prepare(&format!(
            "SELECT (at-?1)/900,count(*) FROM {DAY_ENTRIES} AND {MATCH_ENTRY} GROUP BY 1"
        ))?;
        let mut values = q.query(params![start, end, f.kind, f.source, f.query])?;
        while let Some(row) = values.next()? {
            if let Some(bin) = bins.get_mut(row.get::<_, u32>(0)? as usize) {
                *bin = row.get(1)?;
            }
        }
        result["activity"] = json!({"start":start,"end":end,"bins":bins});
    }
    Ok(result)
}
pub fn inspect(s: &Store, v: &Value) -> Result<Value> {
    let kind = v["kind"].as_str().ok_or("inspection kind missing")?;
    let id = v["id"]
        .as_i64()
        .filter(|v| *v > 0)
        .ok_or("positive id required")?;
    match kind {
        "source" => source(s, id),
        "observation" => {
            let mut item=s.db.query_row("SELECT id,at,end_at,kind,source,text,details,source_id FROM observations WHERE id=?1",[id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"kind":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"details":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or(Value::Null),"source_id":r.get::<_,Option<i64>>(7)?})))?;
            let mut ids = vec![];
            if let Some(i) = item["source_id"].as_i64() {
                ids.push(i);
            }
            if let Some(a) = item["details"]["source_ids"].as_array() {
                for i in a.iter().take(64).filter_map(|v| v.as_i64()) {
                    if !ids.contains(&i) {
                        ids.push(i)
                    }
                }
            }
            item["sources"] = json!(
                ids.into_iter()
                    .map(|i| source(s, i))
                    .collect::<Result<Vec<_>>>()?
            );
            add_image_ref(s, &mut item)?;
            item["image_retained"] = json!(item["image_id"].is_i64());
            Ok(item)
        }
        "job" => {
            let cursor = |key: &str| -> Result<i64> {
                match v.get(key) {
                    None | Some(Value::Null) => Ok(-1),
                    Some(x) => x
                        .as_i64()
                        .filter(|n| *n >= -1)
                        .ok_or_else(|| "invalid inspection cursor".into()),
                }
            };
            let parts_after = cursor("parts_after")?;
            let evidence_after = cursor("evidence_after")?;
            let mut item=s.db.query_row("SELECT kind,lo,hi,status,text,error,created FROM jobs WHERE id=?1",[id],|r|Ok(json!({"id":id,"kind":r.get::<_,String>(0)?,"lo":r.get::<_,i64>(1)?,"hi":r.get::<_,i64>(2)?,"status":r.get::<_,String>(3)?,"text":r.get::<_,String>(4)?,"error":r.get::<_,String>(5)?,"created":r.get::<_,i64>(6)?})))?;
            let mut q=s.db.prepare("SELECT part,length(CAST(input AS BLOB)),result IS NOT NULL,length(CAST(result AS BLOB)) FROM job_parts WHERE job_id=?1 AND part>?2 ORDER BY part LIMIT 101")?;
            item["parts"]=json!(q.query_map(params![id,parts_after],|r|Ok(json!({"part":r.get::<_,i64>(0)?,"input_bytes":r.get::<_,i64>(1)?,"complete":r.get::<_,bool>(2)?,"output_bytes":r.get::<_,Option<i64>>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?);
            let mut q =
                s.db.prepare("SELECT day FROM job_days WHERE job_id=?1 ORDER BY day")?;
            item["days"] = json!(
                q.query_map([id], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            );
            let sql = if item["kind"] == "meta" {
                "SELECT id,created,'summary',substr(text,1,300),'' FROM jobs WHERE id BETWEEN ?1 AND ?2 AND kind='incremental' AND status='done' AND id>?3 ORDER BY id LIMIT 101"
            } else {
                "SELECT id,at,kind,substr(text,1,300),source FROM journal_entries WHERE id BETWEEN ?1 AND ?2 AND kind IN ('transcript','screen','note') AND id>?3 ORDER BY id LIMIT 101"
            };
            let mut q = s.db.prepare(sql)?;
            item["evidence"]=json!(q.query_map(params![item["lo"].as_i64(),item["hi"].as_i64(),evidence_after],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?})))?.collect::<std::result::Result<Vec<_>,_>>()?);
            for (key, field, after) in [
                ("parts", "part", parts_after),
                ("evidence", "id", evidence_after),
            ] {
                let rows = item[key].as_array_mut().unwrap();
                let more = rows.len() > 100;
                rows.truncate(100);
                if key == "evidence" {
                    for row in rows.iter_mut() {
                        add_image_ref(s, row)?;
                    }
                }
                let first = rows
                    .first()
                    .and_then(|r| r[field].as_i64())
                    .unwrap_or(after.saturating_add(1));
                let next = if more {
                    rows.last().and_then(|r| r[field].as_i64())
                } else {
                    None
                };
                let previous: Vec<i64> = if key == "parts" {
                    let mut q=s.db.prepare("SELECT part FROM job_parts WHERE job_id=?1 AND part<?2 ORDER BY part DESC LIMIT 101")?;
                    q.query_map(params![id, first], |r| r.get(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?
                } else {
                    let reverse = sql
                        .replace("id>?3", "id<?3")
                        .replace("ORDER BY id LIMIT 101", "ORDER BY id DESC LIMIT 101");
                    let mut q = s.db.prepare(&reverse)?;
                    q.query_map(
                        params![item["lo"].as_i64(), item["hi"].as_i64(), first],
                        |r| r.get(0),
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?
                };
                let prev = if previous.is_empty() {
                    None
                } else {
                    Some(previous.get(100).copied().unwrap_or(-1))
                };
                item[format!("{key}_truncated")] = json!(more);
                item[format!("{key}_page")] = json!({"after":after,"next":next,"previous":prev});
            }
            item["inspection_limit"] = json!(100);
            Ok(item)
        }
        "part" => {
            let part = v["part"]
                .as_i64()
                .filter(|v| *v >= 0)
                .ok_or("part required")?;
            Ok(s.db.query_row("SELECT input,result FROM job_parts WHERE job_id=?1 AND part=?2",params![id,part],|r|Ok(json!({"id":id,"part":part,"kind":"part","input":r.get::<_,String>(0)?,"text":r.get::<_,Option<String>>(1)?})))?)
        }
        _ => Err("unknown inspection kind".into()),
    }
}
pub fn source(s: &Store, id: i64) -> Result<Value> {
    let mut v=s.db.query_row("SELECT at,end_at,track,path,status,error,attempts,retry_at FROM sources WHERE id=?1",[id],|r|Ok(json!({"id":id,"kind":"source","at":r.get::<_,i64>(0)?,"end":r.get::<_,i64>(1)?,"track":r.get::<_,String>(2)?,"path":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"error":r.get::<_,String>(5)?,"attempts":r.get::<_,i64>(6)?,"retry_at":r.get::<_,i64>(7)?})))?;
    let path = v["path"].as_str().unwrap_or("");
    let meta = if crate::worker::valid_relative(path, "audio") {
        std::fs::metadata(s.root.join(path)).ok()
    } else {
        None
    };
    v["storage"] = json!(if crate::live_audio::valid_path(path) {
        "memory (not saved)"
    } else {
        "disk"
    });
    v["retained"] = json!(meta.as_ref().is_some_and(|m| m.is_file()));
    v["bytes"] = json!(meta.map(|m| m.len()));
    v["capture_metadata"] =
        s.db.query_row(
            "SELECT details FROM source_metadata WHERE source_id=?1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    Ok(v)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reader_source_switch_keeps_saves_and_copies_confirmed_screen_ranges() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).unwrap();
        let day = "2026-03-05";
        let (midnight, _) = day_bounds(day).unwrap();
        let add = |a, b, kind, source, text| {
            s.observation(
                midnight + a,
                midnight + b,
                kind,
                source,
                text,
                &json!({}),
                None,
            )
            .unwrap()
        };
        add(-5, 0, "screen", "passive_screen", "Previous evening");
        add(1, 7, "transcript", "me", "My words");
        add(2, 9, "transcript", "remote", "Their words");
        add(3, 18, "screen", "passive_screen", "Price $125");
        add(4, 4, "screen", "screen", "Earlier OCR");
        add(5, 5, "note", "clipboard", "A pasted note");
        let image = add(6, 6, "screen", "screenshot", "");
        s.db.execute("INSERT INTO saved_images VALUES(?1,x'1234')", [image])
            .unwrap();
        add(7, 7, "screen", "clipboard", "Pasted image text");
        let read = |passive| {
            transcript(
                &s,
                &json!({"day":day,"passive_screen":passive,"with_saves":true}),
            )
            .unwrap()
        };
        let sources = |v: &Value| {
            v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["source"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sources(&read(false)),
            ["me", "remote", "clipboard", "screenshot", "clipboard"]
        );
        let screen = read(true);
        assert_eq!(
            sources(&screen),
            [
                "passive_screen",
                "me",
                "passive_screen",
                "screen",
                "clipboard",
                "screenshot",
                "clipboard"
            ]
        );
        let items = screen["items"].as_array().unwrap();
        assert!(items[2].get("image_id").is_none());
        assert_eq!(items[5]["image_id"], image);
        assert_eq!(
            sources(&transcript(&s, &json!({"day":day})).unwrap()),
            ["me", "remote"]
        );
        let range = |lo, hi| json!({"day":day,"from":midnight+lo,"until":midnight+hi,"source":"passive_screen"});
        let copy = copy_time_range(&s, &range(18i64, 19i64)).unwrap();
        assert!(copy["text"].as_str().unwrap().contains("Price $125"));
        assert!(copy["text"].as_str().unwrap().contains("00:00:18"));
        assert_eq!(copy["count"], 1);
        assert!(copy_time_range(&s, &range(19, 20)).is_err());
        for t in 20..140 {
            add(t, t + 1, "transcript", "remote", "More remote audio");
        }
        assert_eq!(read(true)["total"], 7);
        let latest = read(false);
        let mut before = latest["older"].clone();
        before["day"] = json!(day);
        before["with_saves"] = json!(true);
        let earlier = transcript(&s, &before).unwrap();
        assert_eq!(
            latest["items"].as_array().unwrap().len() + earlier["items"].as_array().unwrap().len(),
            125
        );
    }
    #[test]
    fn silent_passages_do_not_take_up_reading_pages_sessions_or_copied_ranges() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).unwrap();
        let day = "2026-11-17";
        let at = day_bounds(day).unwrap().0 + 3600;
        let note = s
            .observation(at, at, "note", "me", "Paper proof", &json!({}), None)
            .unwrap();
        for i in 1..180 {
            let (kind, source) = if i % 2 == 0 {
                ("preview", "me")
            } else {
                ("transcript", "remote")
            };
            s.observation(
                at + i,
                at + i + 1,
                kind,
                source,
                " \t\n\u{2003}",
                &json!({}),
                None,
            )
            .unwrap();
        }
        s.observation(
            at + 180,
            at + 182,
            "transcript",
            "me",
            "Hello",
            &json!({}),
            None,
        )
        .unwrap();
        s.observation(
            at + 183,
            at + 183,
            "screen",
            "screenshot",
            "",
            &json!({}),
            None,
        )
        .unwrap();
        let request = json!({"day":day,"from":at,"until":at+3600,"sessions":true,"activity":true});
        let timeline = timeline(&s, &request).unwrap();
        assert_eq!(timeline["items"].as_array().unwrap().len(), 3);
        assert_eq!(timeline["sessions"][0]["passages"], 1);
        assert_eq!(timeline["sessions"][0]["screens"], 1);
        assert_eq!(copy_time_range(&s, &request).unwrap()["count"], 3);
        let page = transcript(&s, &json!({"day":day})).unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["text"], "Hello");
        s.db.execute(
            "INSERT INTO jobs(kind,lo,hi,created) VALUES('incremental',?1,?2,?3)",
            params![note, 1000, at],
        )
        .unwrap();
        let detail = inspect(&s, &json!({"kind":"job","id":s.db.last_insert_rowid()})).unwrap();
        assert_eq!(detail["evidence"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn sessions_find_related_entries_across_pages_and_keep_real_breaks() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).unwrap();
        let day = "2026-11-17";
        let a = day_bounds(day).unwrap().0 + 9 * 3600;
        let add = |at, end, kind, source, text| {
            s.observation(a + at, a + end, kind, source, text, &json!({}), None)
                .unwrap();
        };
        add(0, 1800, "transcript", "me", "Opening remarks");
        for i in 1..=105 {
            add(i, i + 1, "transcript", "remote", "Discussion");
        }
        add(1850, 1850, "screen", "screenshot", "Route map");
        add(1860, 1860, "note", "me", "Harbor walk");
        add(2760, 2760, "note", "me", "Next project");
        let full = timeline(&s, &json!({"day":day,"sessions":true})).unwrap();
        assert_eq!(full["items"].as_array().unwrap().len(), 100);
        assert_eq!(full["sessions"].as_array().unwrap().len(), 2);
        let selected = timeline(
            &s,
            &json!({"day":day,"sessions":true, "query":"map", "source":"screenshot"}),
        )
        .unwrap();
        let groups = selected["sessions"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0],
            json!({"start":a,"until":a+1861,"title":"Harbor walk","entries":108,
            "notes":1,"screens":1,"passages":106})
        );
        let copied = copy_time_range(
            &s,
            &json!({"day":day,"from":groups[0]["start"],"until":groups[0]["until"]}),
        )
        .unwrap();
        assert_eq!(copied["count"], 108);
        assert!(!copied["text"].as_str().unwrap().contains("Next project"));
        assert_eq!(
            timeline(
                &s,
                &json!({"day":day,"sessions":true,"from":a+2500,"until":a+2800})
            )
            .unwrap()["sessions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn time_selection_uses_complete_entries_across_pages_and_keeps_chronology() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).unwrap();
        let day = "2026-11-17";
        let (start, _) = day_bounds(day).unwrap();
        let a = start + 9 * 3600;
        let b = a + 1800;
        s.observation(
            a - 5,
            a + 3,
            "transcript",
            "me",
            "opening words",
            &json!({}),
            None,
        )
        .unwrap();
        s.observation(
            a - 10,
            a,
            "transcript",
            "remote",
            "ends before range",
            &json!({}),
            None,
        )
        .unwrap();
        let long = "全文 ".repeat(650);
        for i in 0..105 {
            let value = if i == 104 {
                long.clone()
            } else {
                format!("Note {i:03}")
            };
            s.observation(a + i, a + i, "note", "clipboard", &value, &json!({}), None)
                .unwrap();
        }
        s.observation(b, b, "note", "me", "at exclusive end", &json!({}), None)
            .unwrap();
        let req = json!({"day":day,"from":a,"until":b,"activity":true});
        let page = timeline(&s, &req).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 100);
        assert!(page["next"].is_object());
        assert_eq!(
            page["activity"]["bins"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_u64)
                .sum::<u64>(),
            108
        );
        let out = copy_time_range(&s, &req).unwrap();
        assert_eq!(out["count"], 106);
        let text = out["text"].as_str().unwrap();
        assert!(
            text.contains(long.trim())
                && !text.contains("exclusive end")
                && !text.contains("ends before")
        );
        assert!(text.find("opening words").unwrap() < text.find("Note 000").unwrap());
        assert!(text.find("Note 000").unwrap() < text.find("Note 103").unwrap());
        let filtered = json!({"day":day,"from":a,"until":b,"kind":"note","source":"clipboard","query":"Note 031"});
        assert_eq!(copy_time_range(&s, &filtered).unwrap()["count"], 1);
        assert_eq!(
            timeline(&s, &filtered).unwrap()["items"][0]["text"],
            "Note 031"
        );
        for bad in [
            json!({"day":day}),
            json!({"day":day,"from":b,"until":a}),
            json!({"day":day,"from":a,"until":b,"query":"no hits"}),
        ] {
            assert!(copy_time_range(&s, &bad).is_err());
        }
    }
    #[test]
    fn reading_pages_preserve_full_day_and_provisional_identity() {
        let root = std::env::temp_dir().join(format!("daybook-reader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let day = "2026-11-17";
        let (start, _) = day_bounds(day).unwrap();
        for i in 1..=205 {
            s.observation(
                start + i / 2,
                start + i / 2 + 1,
                if i == 205 { "preview" } else { "transcript" },
                if i % 2 == 0 { "me" } else { "remote" },
                &format!("Passage {i}"),
                &json!({}),
                None,
            )
            .unwrap();
        }
        let latest = transcript(&s, &json!({"day":day})).unwrap();
        assert_eq!(latest["total"], 205);
        assert_eq!(latest["items"].as_array().unwrap().len(), 80);
        let mut page = latest.clone();
        let mut ids = Vec::new();
        loop {
            ids.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["id"].as_i64().unwrap()),
            );
            if page["older"].is_null() {
                break;
            }
            let mut q = page["older"].clone();
            q["day"] = json!(day);
            page = transcript(&s, &q).unwrap();
        }
        ids.sort();
        assert_eq!(ids, (1..=205).collect::<Vec<_>>());
        let mut q = page["newer"].clone();
        q["day"] = json!(day);
        let forward = transcript(&s, &q).unwrap();
        assert_eq!(forward["items"][0]["id"], 46);
        assert_eq!(forward["items"][79]["id"], 125);
        s.db.execute(
            "UPDATE observations SET kind='transcript',text='Final revised words' WHERE id=205",
            [],
        )
        .unwrap();
        let refreshed = transcript(&s, &json!({"day":day})).unwrap();
        assert_eq!(refreshed["items"][79]["id"], 205);
        assert_eq!(refreshed["items"][79]["text"], "Final revised words");
        assert_eq!(refreshed["total"], 205);
        s.db.execute(
            "UPDATE observations SET text=?1 WHERE id=205",
            ["word ".repeat(900)],
        )
        .unwrap();
        assert_eq!(
            transcript(&s, &json!({"day":day})).unwrap()["items"][79]["truncated"],
            true
        );
        assert!(transcript(&s, &json!({"day":"invalid"})).is_err());
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn live_keeps_last_words_until_new_preview_has_text() {
        let root = std::env::temp_dir().join(format!("daybook-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        s.observation(
            1,
            2,
            "transcript",
            "me",
            "Completed words",
            &json!({}),
            None,
        )
        .unwrap();
        s.observation(3, 4, "preview", "me", "", &json!({}), None)
            .unwrap();
        s.observation(3, 4, "preview", "remote", "Remote words", &json!({}), None)
            .unwrap();
        let v = live(&s).unwrap();
        assert_eq!(v["live_transcription"][0]["text"], "Completed words");
        assert_eq!(v["live_transcription"][1]["text"], "Remote words");
        s.db.execute(
            "UPDATE observations SET text='New words' WHERE source='me' AND kind='preview'",
            [],
        )
        .unwrap();
        assert_eq!(
            live(&s).unwrap()["live_transcription"][0]["text"],
            "New words"
        );
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn inspection_cursor_pages_cover_all_records_forward_backward_and_meta() {
        let root =
            std::env::temp_dir().join(format!("daybook-inspect-pages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        for i in 1..=205 {
            s.observation(i, i, "note", "me", "evidence", &json!({}), None)
                .unwrap();
            s.db.execute("INSERT INTO jobs(id,kind,lo,hi,status,created) VALUES(?1,'incremental',?1,?1,'done',1)",[i]).unwrap();
        }
        s.db.execute("INSERT INTO jobs(id,kind,lo,hi,created) VALUES(206,'incremental',1,205,1),(207,'meta',1,205,1)",[]).unwrap();
        for part in 0..205 {
            s.db.execute(
                "INSERT INTO job_parts(job_id,part,input) VALUES(206,?1,'input')",
                [part],
            )
            .unwrap();
        }
        for (job, key, field, start) in [
            (206, "evidence", "id", 1),
            (206, "parts", "part", 0),
            (207, "evidence", "id", 1),
        ] {
            let mut request = json!({"kind":"job","id":job});
            let mut seen = Vec::new();
            let mut pages = Vec::new();
            loop {
                let v = inspect(&s, &request).unwrap();
                pages.push(v.clone());
                seen.extend(
                    v[key]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| r[field].as_i64().unwrap()),
                );
                if let Some(next) = v[format!("{key}_page")]["next"].as_i64() {
                    request[format!("{key}_after")] = json!(next);
                } else {
                    break;
                }
            }
            assert_eq!(seen, (start..start + 205).collect::<Vec<_>>());
            assert_eq!(pages.len(), 3);
            for page in (1..pages.len()).rev() {
                request[format!("{key}_after")] =
                    pages[page][format!("{key}_page")]["previous"].clone();
                let v = inspect(&s, &request).unwrap();
                assert_eq!(v[key], pages[page - 1][key]);
            }
            assert!(pages[0][format!("{key}_page")]["previous"].is_null());
        }
        let independent = inspect(
            &s,
            &json!({"kind":"job","id":206,"parts_after":199,"evidence_after":100}),
        )
        .unwrap();
        assert_eq!(independent["parts"][0]["part"], 200);
        assert_eq!(independent["evidence"][0]["id"], 101);
        assert!(inspect(&s, &json!({"kind":"job","id":206,"parts_after":-2})).is_err());
        assert!(inspect(&s, &json!({"kind":"job","id":206,"evidence_after":"bad"})).is_err());
    }
    #[test]
    fn inspection_explicitly_reports_more_than_one_hundred_references_and_parts() {
        let root =
            std::env::temp_dir().join(format!("daybook-inspect-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        for i in 1..=101 {
            s.observation(i, i, "note", "me", "evidence", &json!({}), None)
                .unwrap();
        }
        s.db.execute(
            "INSERT INTO jobs(id,kind,lo,hi,created) VALUES(1,'incremental',1,101,1)",
            [],
        )
        .unwrap();
        for part in 0..101 {
            s.db.execute(
                "INSERT INTO job_parts(job_id,part,input) VALUES(1,?1,'input')",
                [part],
            )
            .unwrap();
        }
        let v = inspect(&s, &json!({"kind":"job","id":1})).unwrap();
        assert_eq!(v["parts"].as_array().unwrap().len(), 100);
        assert_eq!(v["evidence"].as_array().unwrap().len(), 100);
        assert_eq!(v["parts_truncated"], true);
        assert_eq!(v["evidence_truncated"], true);
        assert_eq!(
            inspect(&s, &json!({"kind":"part","id":1,"part":100})).unwrap()["input"],
            "input"
        );
        s.db.execute("DELETE FROM job_parts WHERE part=100", [])
            .unwrap();
        s.db.execute("UPDATE jobs SET hi=100 WHERE id=1", [])
            .unwrap();
        let v = inspect(&s, &json!({"kind":"job","id":1})).unwrap();
        assert_eq!(v["parts_truncated"], false);
        assert_eq!(v["evidence_truncated"], false);
    }
    #[test]
    fn paginated_timeline_is_chronological_and_inspection_resolves_retention() {
        let root = std::env::temp_dir().join(format!("daybook-inspect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let day = chrono::Local::now().date_naive().to_string();
        let (start, _) = day_bounds(&day).unwrap();
        for i in 0..105 {
            s.observation(
                start + i,
                start + i,
                "note",
                "me",
                &format!("note {i}"),
                &json!({}),
                None,
            )
            .unwrap();
        }
        let late = s
            .observation(
                start + 1,
                start + 1,
                "screen",
                "vision",
                &"🦀".repeat(1300),
                &json!({"observation_id":2}),
                None,
            )
            .unwrap();
        let a = timeline(&s, &json!({"day":day})).unwrap();
        assert_eq!(a["items"].as_array().unwrap().len(), 100);
        assert_eq!(a["items"][0]["text"], "note 104");
        assert!(!a["next"].is_null());
        let b=timeline(&s,&json!({"day":day,"before_at":a["next"]["before_at"],"before_id":a["next"]["before_id"]})).unwrap();
        assert_eq!(b["items"].as_array().unwrap().len(), 6);
        assert!(
            b["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == late)
        );
        let v = timeline(&s, &json!({"day":day,"source":"vision"})).unwrap();
        assert_eq!(v["items"][0]["truncated"], true);
        assert_eq!(
            inspect(&s, &json!({"kind":"observation","id":late})).unwrap()["text"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1300
        );
        s.add_source(start, start + 1, "me", "audio/a.f32").unwrap();
        std::fs::write(root.join("audio/a.f32"), vec![0u8; 64000]).unwrap();
        let sid =
            s.db.query_row("SELECT id FROM sources", [], |r| r.get::<_, i64>(0))
                .unwrap();
        let id = s
            .observation(
                start,
                start + 1,
                "transcript",
                "me",
                "hello",
                &json!({"source_ids":[sid]}),
                Some(sid),
            )
            .unwrap();
        let v = inspect(&s, &json!({"kind":"observation","id":id})).unwrap();
        assert_eq!(v["sources"].as_array().unwrap().len(), 1);
        assert_eq!(v["sources"][0]["retained"], true);
        std::fs::remove_file(root.join("audio/a.f32")).unwrap();
        assert_eq!(source(&s, sid).unwrap()["retained"], false);
        assert!(timeline(&s, &json!({"day":"../../file"})).is_err());
        assert!(timeline(&s, &json!({"day":day,"kind":"injected"})).is_err());
        assert!(inspect(&s, &json!({"kind":"source","id":-1})).is_err());
    }
    #[test]
    fn health_reports_measured_frames_and_unknown_billing() {
        let root = std::env::temp_dir().join(format!("daybook-health-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        s.add_source(now() - 10, now() - 9, "me", "audio/a.f32")
            .unwrap();
        std::fs::write(root.join("audio/a.f32"), vec![0u8; 64000]).unwrap();
        let h = health(&s, &Config::default(), &mut Sampler::default()).unwrap();
        assert_eq!(h["tracks"][0]["pcm_frames"], 16000);
        assert_eq!(h["tracks"][0]["pcm_seconds"], 1.0);
        assert!(h["actual_billed_usd"].is_null());
        assert!(h["native"].is_null());
        assert!(h["backend"]["cpu_core_percent"].is_null());
    }
}
