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
        let mut q=s.db.prepare("SELECT id,at,end_at,kind,substr(text,1,4000) FROM observations WHERE source=?1 AND kind IN ('preview','transcript') AND text!='' ORDER BY id DESC LIMIT 1")?;
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
    let mut q = s.db.prepare(&format!("SELECT id,at,end_at,kind,source,substr(text,1,4000),length(text)>4000 FROM observations WHERE at>=?1 AND at<?2 AND kind IN ('preview','transcript') AND text!='' AND (at{cmp}?3 OR (at=?3 AND id{cmp}?4)) ORDER BY at {order},id {order} LIMIT 80"))?;
    let mut rows=q.query_map(params![start,end,at,id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"kind":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"truncated":r.get::<_,bool>(6)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if !newer {
        rows.reverse();
    }
    let total:i64=s.db.query_row("SELECT count(*) FROM observations WHERE at>=?1 AND at<?2 AND kind IN ('preview','transcript') AND text!=''",params![start,end],|r|r.get(0))?;
    let mut older = Value::Null;
    let mut later = Value::Null;
    if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
        for (row, cmp, cursor) in [(first, "<", &mut older), (last, ">", &mut later)] {
            let a = row["at"].as_i64().unwrap();
            let i = row["id"].as_i64().unwrap();
            let more:bool=s.db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM observations WHERE at>=?1 AND at<?2 AND kind IN ('preview','transcript') AND text!='' AND (at{cmp}?3 OR (at=?3 AND id{cmp}?4)))"),params![start,end,a,i],|r|r.get(0))?;
            if more {
                *cursor = if cmp == "<" {
                    json!({"before_at":a,"before_id":i})
                } else {
                    json!({"after_at":a,"after_id":i})
                };
            }
        }
    }
    Ok(json!({"day":day,"items":rows,"older":older,"newer":later,"total":total,"limit":80}))
}
pub fn timeline(s: &Store, v: &Value) -> Result<Value> {
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
            "system",
            "clipboard",
            "screenshot",
        ]
        .contains(&source)
    {
        return Err("invalid timeline filter".into());
    }
    let before_at = v["before_at"].as_i64().unwrap_or(i64::MAX);
    let before_id = v["before_id"].as_i64().unwrap_or(i64::MAX);
    let mut q=s.db.prepare("SELECT id,at,end_at,kind,source,substr(text,1,1200),length(text)>1200,source_id FROM observations WHERE at>=?1 AND at<?2 AND ((?3='all' AND kind IN ('transcript','preview','note','screen')) OR kind=?3) AND (?4='all' OR source=?4) AND instr(lower(text),lower(?5))>0 AND (at<?6 OR (at=?6 AND id<?7)) ORDER BY at DESC,id DESC LIMIT 101")?;
    let mut rows=q.query_map(params![start,end,kind,source,query,before_at,before_id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"kind":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"truncated":r.get::<_,bool>(6)?,"source_id":r.get::<_,Option<i64>>(7)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let more = rows.len() > 100;
    rows.truncate(100);
    let cursor = if more {
        rows.last()
            .map(|v| json!({"before_at":v["at"],"before_id":v["id"]}))
    } else {
        None
    };
    let mut q=s.db.prepare("SELECT id,kind,lo,hi,status,substr(text,1,2000),error FROM jobs WHERE id IN (SELECT job_id FROM job_days WHERE day=?1) ORDER BY id DESC LIMIT 50")?;
    let jobs=q.query_map([day],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"lo":r.get::<_,i64>(2)?,"hi":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"error":r.get::<_,String>(6)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(json!({"day":day,"items":rows,"next":cursor,"jobs":jobs,"summary_limit":50}))
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
                for i in a.iter().take(32).filter_map(|v| v.as_i64()) {
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
            let image = item["details"]["path"].as_str().unwrap_or("");
            item["image_retained"] = json!(if item["details"]["saved_image"] == true {
                s.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM saved_images WHERE observation_id=?1)",
                    [id],
                    |r| r.get::<_, bool>(0),
                )?
            } else {
                crate::worker::valid_relative(image, "images") && s.root.join(image).is_file()
            });
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
                "SELECT id,created,'summary',substr(text,1,300) FROM jobs WHERE id BETWEEN ?1 AND ?2 AND kind='incremental' AND status='done' AND id>?3 ORDER BY id LIMIT 101"
            } else {
                "SELECT id,at,kind,substr(text,1,300) FROM observations WHERE id BETWEEN ?1 AND ?2 AND kind IN ('transcript','screen','note') AND id>?3 ORDER BY id LIMIT 101"
            };
            let mut q = s.db.prepare(sql)?;
            item["evidence"]=json!(q.query_map(params![item["lo"].as_i64(),item["hi"].as_i64(),evidence_after],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?);
            for (key, field, after) in [
                ("parts", "part", parts_after),
                ("evidence", "id", evidence_after),
            ] {
                let rows = item[key].as_array_mut().unwrap();
                let more = rows.len() > 100;
                rows.truncate(100);
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
