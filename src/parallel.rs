//! Time windows for the native parallel transcript. Display density never limits copying.
use crate::{
    inspect::RANGE_ENTRIES,
    store::{Result, Store, now},
    transcript_range::*,
};
use rusqlite::{OptionalExtension, Row, params};
use serde_json::{Value, json};
use std::fmt::Write;

const TRACKS: &str = "(kind IN ('transcript','preview') AND source IN ('me','remote'))
    OR (kind='screen' AND source IN ('screenshot','clipboard','screen','passive_screen','vision')
        AND (trim(text)<>'' OR source IN ('screenshot','clipboard')))
    OR (kind='note' AND source IN ('me','clipboard')
        AND (trim(text)<>'' OR json_array_length(details,'$.image_ids')>0))";
const LANES: &str = "CASE
    WHEN kind IN ('transcript','preview') THEN CASE source WHEN 'me' THEN 'mic' ELSE 'system' END
    WHEN kind='screen' AND source IN ('screen','passive_screen','vision') THEN 'passive'
    ELSE 'saves' END";
fn passage_fields(limit: u16) -> String {
    format!(
        "id,at,end_at,kind,source,substr(text,1,{limit}),length(text)>{limit},
        CASE WHEN kind='screen' AND source IN ('screenshot','clipboard')
             THEN (SELECT observation_id FROM saved_images WHERE observation_id=visible.id)
             WHEN kind='note' THEN json_extract(details,'$.image_ids[0]') END"
    )
}
const ORDER: &str = " ORDER BY at,id";
const DISPLAY: usize = 240;
fn scope() -> String {
    format!(
        "WITH visible AS (SELECT *,{LANES} AS lane FROM {RANGE_ENTRIES} AND ({TRACKS}) AND lane IN ('mic','saves',?3)) "
    )
}
fn middle(v: &Value) -> &str {
    if v["passive"] == true {
        "passive"
    } else {
        "system"
    }
}
fn requested_span(v: &Value, key: &str) -> Result<Span> {
    let s: Span = serde_json::from_value(v[key].clone())?;
    if !s.valid() {
        return Err("Choose a valid time range".into());
    }
    Ok(s)
}
fn passage(row: &Row<'_>) -> rusqlite::Result<Passage> {
    Ok(Passage {
        id: row.get(0)?,
        at: row.get(1)?,
        end: row.get(2)?,
        kind: row.get(3)?,
        source: row.get(4)?,
        text: row.get(5)?,
        truncated: row.get(6)?,
        image_id: row.get(7)?,
    })
}
fn lane_key(lane: Lane) -> &'static str {
    match lane {
        Lane::Mic => "mic",
        Lane::System => "system",
        Lane::Passive => "passive",
        Lane::Saves => "saves",
    }
}
fn bins(s: &Store, r: Span, mid: &str, n: i64, overview: bool) -> Result<Vec<Cluster>> {
    let step = (r.len() + n - 1) / n;
    let sql = scope()
        + ", grouped AS (
            SELECT lane, (max(at,?1)-?1)/?4 AS slot, min(id) AS example, count(*) AS n,
                   min(max(at,?1)) AS lo, max(min(max(end_at,at+1),?2)) AS hi
            FROM visible GROUP BY lane,slot
        )
        SELECT entry.kind,entry.source,slot,n,lo,hi,substr(entry.text,1,130)
        FROM grouped JOIN observations entry ON entry.id=grouped.example ORDER BY slot,grouped.lane";
    let mut q = s.db.prepare_cached(&sql)?;
    Ok(q.query_map(params![r.from, r.until, mid, step], |row| {
        let from = r.from + row.get::<_, i64>(2)? * step;
        Ok(Cluster {
            lane: Lane::of(row.get_ref(0)?.as_str()?, row.get_ref(1)?.as_str()?).unwrap(),
            range: if overview {
                let a: i64 = row.get(4)?;
                Span {
                    from: a,
                    until: row.get::<_, i64>(5)?.max(a + 1),
                }
            } else {
                Span {
                    from,
                    until: (from + step).min(r.until),
                }
            },
            count: row.get::<_, i64>(3)? as u64,
            excerpt: row.get(6)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}
fn receipt(s: &Store, r: Span) -> Result<Option<i64>> {
    let sample =
        s.db.query_row(
            "SELECT at,end_at FROM sources WHERE end_at>=?1 AND end_at<?2
         AND status NOT IN ('lost','expired') ORDER BY end_at DESC LIMIT 1",
            params![r.from, r.until],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((at, end)) = sample else {
        return Ok(None);
    };
    let overlaps: bool = s.db.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {RANGE_ENTRIES} AND kind IN ('preview','transcript'))"
        ),
        params![at, end.max(at) + 1],
        |r| r.get(0),
    )?;
    Ok((!overlaps).then_some(end))
}
fn read_window(s: &Store, r: Span, mid: &str, context: bool) -> Result<(Vec<Passage>, bool)> {
    let fields = passage_fields(if context { 320 } else { 1200 });
    let sql = scope() + "SELECT " + &fields + " FROM visible" + ORDER + " LIMIT 241";
    let mut q = s.db.prepare_cached(&sql)?;
    let rows = q
        .query_map(params![r.from, r.until, mid], passage)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > DISPLAY {
        Ok((vec![], true))
    } else {
        Ok((rows, false))
    }
}
pub fn window(s: &Store, v: &Value) -> Result<Value> {
    let now = now().clamp(MIN_TIME + 1, MAX_TIME - 1);
    let mid = middle(v);
    let edge = |order: &str| -> Result<Option<(i64, i64)>> {
        let sql = format!(
            "SELECT at,end_at FROM journal_entries WHERE ({TRACKS}) AND ({LANES}) IN ('mic','saves',?1)
             ORDER BY at {order},id {order} LIMIT 1"
        );
        Ok(s.db
            .query_row(&sql, [mid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
    };
    let first = edge("ASC")?.map(|r| r.0).unwrap_or(now);
    let last = edge("DESC")?.map(|r| r.1.max(r.0)).unwrap_or(now);
    let span_sql = format!(
        "SELECT end_at FROM journal_entries WHERE end_at>at AND end_at>?1 AND ({TRACKS})
         AND ({LANES}) IN ('mic','saves',?2) ORDER BY end_at DESC LIMIT 1"
    );
    let latest_span: Option<i64> =
        s.db.query_row(&span_sql, params![last, mid], |r| r.get(0))
            .optional()?;
    let last = latest_span
        .unwrap_or(last)
        .clamp(MIN_TIME + 1, MAX_TIME - 1);
    let extent = Span {
        from: first.clamp(MIN_TIME, last),
        until: last + 1,
    };
    let seconds = v["seconds"]
        .as_i64()
        .unwrap_or(60)
        .clamp(1, MAX_TIME - MIN_TIME);
    let range = if v["range"].is_null() {
        // Leave reading room after the final line when revisiting a stopped recording.
        let end = if s.get("active") == "true" {
            now + 1
        } else {
            last + (seconds / 6).clamp(1, 10)
        };
        Span::ending(end, seconds)
    } else {
        requested_span(v, "range")?
    };
    let overview = if !v["overview"].is_null() {
        requested_span(v, "overview")?
    } else {
        let overview_len = (range.len().saturating_mul(10))
            .max(600)
            .min(extent.len().max(range.len()));
        let left = range
            .from
            .saturating_sub((overview_len - range.len()) / 2)
            .max(extent.from.min(range.from));
        let end = (left + overview_len)
            .min(extent.until.max(range.until))
            .max(range.until);
        Span::ending(end, overview_len)
    };
    let (passages, dense) = read_window(s, range, mid, false)?;
    let clusters = if dense {
        bins(s, range, mid, 64, false)?
    } else {
        vec![]
    };
    let (nearby, nearby_grouped) = if overview == range {
        (vec![], dense)
    } else {
        read_window(s, overview, mid, true)?
    };
    let window = Window {
        range,
        extent,
        overview,
        bands: bins(s, overview, mid, 96, true)?,
        passages,
        clusters,
        quiet_at: receipt(s, range)?,
        nearby,
        nearby_grouped,
    };
    Ok(serde_json::to_value(window)?)
}
pub fn passages(s: &Store, v: &Value) -> Result<Value> {
    let r = requested_span(v, "range")?;
    let b = requested_span(v, "bucket")?;
    let lane: Lane = serde_json::from_value(v["lane"].clone())?;
    let sql = scope()
        + "SELECT "
        + &passage_fields(1200)
        + " FROM visible WHERE lane=?4 AND max(at,?1)>=?5 AND max(at,?1)<?6 AND (at,id)>(?7,?8)"
        + ORDER
        + " LIMIT 61";
    let mut q = s.db.prepare_cached(&sql)?;
    let mut items = q
        .query_map(
            params![
                r.from,
                r.until,
                middle(v),
                lane_key(lane),
                b.from,
                b.until,
                v["after_at"].as_i64().unwrap_or(i64::MIN),
                v["after_id"].as_i64().unwrap_or(i64::MIN)
            ],
            passage,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = items.len() > 60;
    items.truncate(60);
    Ok(json!({"items":items,"more":more}))
}
pub fn copy(s: &Store, v: &Value) -> Result<Value> {
    let r = requested_span(v, "range")?;
    let selected = v["selected"] == true;
    let picks: Picks =
        serde_json::from_value(v.get("picks").cloned().unwrap_or_else(|| json!({})))?;
    let lane: Option<Lane> = serde_json::from_value(v["lane"].clone())?;
    let date = |at| {
        chrono::DateTime::from_timestamp(at, 0)
            .map(|d| d.with_timezone(&chrono::Local).date_naive())
    };
    let multi_day = date(r.from) != date(r.until - 1);
    let stamp = |t: i64| {
        chrono::DateTime::from_timestamp(t, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format(if multi_day {
                        "%Y-%m-%d %H:%M:%S %Z"
                    } else {
                        "%H:%M:%S"
                    })
                    .to_string()
            })
            .unwrap_or_default()
    };
    let sql = scope()
        + "SELECT id,at,end_at,kind,source,text,0,NULL FROM visible WHERE (?4 IS NULL OR lane=?4)"
        + ORDER;
    let mut q = s.db.prepare_cached(&sql)?;
    let mut text = String::new();
    let mut count = 0;
    let mut rows = q.query(params![r.from, r.until, middle(v), lane.map(lane_key)])?;
    while let Some(row) = rows.next()? {
        let p = passage(row)?;
        let pick = if selected {
            picks.get(&p)
        } else {
            Some(Pick::Full)
        };
        let Some(pick) = pick else { continue };
        let body = match pick {
            Pick::Chars([a, b]) => p
                .body()
                .chars()
                .skip(a)
                .take(b.saturating_sub(a))
                .collect::<String>(),
            Pick::Full => p.body().to_owned(),
            Pick::Skip => continue,
        };
        if body.trim().is_empty() {
            continue;
        }
        if text.len() + body.len() + 128 > 2 * 1024 * 1024 {
            return Err("Choose a shorter range to copy it in full (2 MiB maximum).".into());
        }
        let mut time = stamp(p.at);
        if p.lane() == Lane::Passive && p.end > p.at {
            time.push('–');
            time.push_str(&stamp(p.end))
        }
        writeln!(text, "[{time}] {}: {}", p.label(), body.trim())?;
        count += 1;
    }
    if count == 0 {
        return Err("This selection has no passages to copy.".into());
    }
    Ok(json!({"text":text.trim_end(),"count":count}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn add(s: &Store, at: i64, end: i64, kind: &str, source: &str, text: &str) -> i64 {
        s.observation(at, end, kind, source, text, &json!({}), None)
            .unwrap()
    }
    #[test]
    fn simultaneous_sources_spans_full_text_and_unicode_selections_share_the_same_window() {
        let t = tempfile::tempdir().unwrap();
        let s = Store::open(t.path()).unwrap();
        let mic = add(&s, 100, 111, "transcript", "me", "Hello 世界");
        add(&s, 100, 112, "transcript", "remote", "Overlapping words");
        add(&s, 84, 100, "screen", "passive_screen", "Visible earlier");
        add(&s, 87, 100, "transcript", "me", "Finished before");
        let pic = add(&s, 103, 103, "screen", "screenshot", "");
        s.db.execute("INSERT INTO saved_images VALUES(?1,x'1234')", [pic])
            .unwrap();
        add(&s, 108, 108, "note", "clipboard", "Pasted note");
        add(&s, 120, 120, "note", "clipboard", "Next range");
        let long = "Reference ".repeat(240);
        add(&s, 106, 106, "screen", "clipboard", &long);
        let mut request = json!({"range":{"from":100,"until":120}});
        let audio: Window = serde_json::from_value(window(&s, &request).unwrap()).unwrap();
        assert_eq!(
            audio.passages.iter().map(|p| p.lane()).collect::<Vec<_>>(),
            [
                Lane::Mic,
                Lane::System,
                Lane::Saves,
                Lane::Saves,
                Lane::Saves
            ]
        );
        assert_eq!(audio.passages[2].image_id, Some(pic));
        assert!(audio.passages[3].truncated);
        let text = copy(&s, &request).unwrap();
        assert!(text["text"].as_str().unwrap().contains(long.trim()));
        request["passive"] = json!(true);
        let passive: Window = serde_json::from_value(window(&s, &request).unwrap()).unwrap();
        assert_eq!(passive.passages[0].text, "Visible earlier");
        assert_eq!(passive.passages[0].end, 100);
        assert!(passive.passages.iter().all(|p| p.lane() != Lane::System));
        let mut picks = Picks::default();
        picks.lanes.insert(Lane::Saves);
        picks.entries.insert(pic, Pick::Skip);
        picks.entries.insert(mic, Pick::Chars([6, 8]));
        request["selected"] = json!(true);
        request["picks"] = json!(picks);
        let copied = copy(&s, &request).unwrap();
        let text = copied["text"].as_str().unwrap();
        assert_eq!(copied["count"], 3);
        assert!(text.lines().next().unwrap().ends_with("Microphone: 世界"));
        assert!(!text.contains("Saved screenshot") && !text.contains("Overlapping"));
        request["lane"] = json!("mic");
        assert_eq!(copy(&s, &request).unwrap()["count"], 1);
    }
    #[test]
    fn zoomed_out_groups_account_for_every_row_and_drilldown_pages_tied_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::open(tmp.path()).unwrap();
        for i in 0..320 {
            add(&s, 100, 103, "transcript", "me", &format!("Passage {i}"));
        }
        add(&s, 150, 150, "note", "clipboard", "A saved note");
        let req = json!({"range":{"from":10,"until":610}});
        let w: Window = serde_json::from_value(window(&s, &req).unwrap()).unwrap();
        assert!(w.passages.is_empty());
        assert_eq!(w.clusters.iter().map(|g| g.count).sum::<u64>(), 321);
        let g = w.clusters.iter().find(|g| g.lane == Lane::Mic).unwrap();
        let mut d = json!({"range":w.range,"bucket":g.range,"lane":"mic"});
        let mut ids = vec![];
        loop {
            let p = passages(&s, &d).unwrap();
            for r in p["items"].as_array().unwrap() {
                ids.push(r["id"].as_i64().unwrap());
            }
            if p["more"] != true {
                break;
            }
            let tail = p["items"].as_array().unwrap().last().unwrap();
            d["after_at"] = tail["at"].clone();
            d["after_id"] = tail["id"].clone();
        }
        assert_eq!(ids, (1..=320).collect::<Vec<_>>());
        assert_eq!(copy(&s, &req).unwrap()["count"], 321);
        assert_eq!(
            copy(&s, &json!({"range":w.range,"lane":"mic"})).unwrap()["count"],
            320
        );
    }
    #[test]
    fn stopped_launch_finds_actual_history_and_quiet_requires_received_audio() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::open(tmp.path()).unwrap();
        add(&s, 500, 510, "transcript", "me", "Recent speech");
        add(
            &s,
            9000,
            9000,
            "screen",
            "passive_screen",
            "Later screen-only work",
        );
        let data = |v: &Value| serde_json::from_value::<Window>(window(&s, v).unwrap()).unwrap();
        let latest = data(&json!({}));
        assert_eq!(latest.range.len(), 60);
        assert_eq!(latest.passages[0].text, "Recent speech");
        assert_eq!(
            data(&json!({"passive":true})).passages[0].text,
            "Later screen-only work"
        );
        let quiet = json!({"range":{"from":515,"until":550}});
        assert_eq!(data(&quiet).quiet_at, None);
        s.db.execute("INSERT INTO sources(at,end_at,track,path,status) VALUES(530,531,'me','memory:fixture','done')",[]).unwrap();
        assert_eq!(data(&quiet).quiet_at, Some(531));
        add(&s, 528, 533, "transcript", "remote", "Heard here");
        assert_eq!(data(&quiet).quiet_at, None);
        assert!(window(&s, &json!({"range":{"from":1,"until":1}})).is_err());
        let huge = data(&json!({"range":{"from":MIN_TIME,"until":MAX_TIME}}));
        assert_eq!(huge.range.len(), MAX_TIME - MIN_TIME);
    }
}
