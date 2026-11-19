use crate::{
    screen_captures,
    screen_layout::Layout,
    screen_sample::ScreenSample,
    store::{Result, Store},
};
use rusqlite::params;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default)]
pub struct Tracker {
    last: Option<ScreenSample>,
    visible: HashMap<String, i64>,
    layout: Option<(i64, Layout)>,
    image: Option<i64>,
    pub delay: Option<&'static str>,
    pub error: Option<String>,
}

/// macOS exports dispatch pressure flags here. A warning still allows a single screen
/// check. A critical signal or a real queue of audio waiting for inference takes priority.
pub fn delay_reason(s: &Store, pressure: i32) -> Result<Option<&'static str>> {
    const DISPATCH_MEMORYPRESSURE_CRITICAL: i32 = 0x04;
    if pressure & DISPATCH_MEMORYPRESSURE_CRITICAL != 0 {
        return Ok(Some("Waiting for critical memory pressure to ease."));
    }
    // Streaming blocks have already reached the recognizer and can wait for the
    // end of a phrase. Count only blocks ready and still waiting to be processed.
    let backlogged: bool = s.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sources
                       WHERE status='pending' AND retry_at<=?1 LIMIT 1 OFFSET 4)",
        [crate::store::now()],
        |row| row.get(0),
    )?;
    Ok(backlogged.then_some("Waiting for queued audio to finish processing."))
}

pub fn observe(s: &Store, v: &Value, interval: u64) -> Result<()> {
    let sample: ScreenSample = serde_json::from_value(v["sample"].clone())?;
    let mut tracker = s.passive.borrow_mut();
    if tracker
        .last
        .as_ref()
        .is_some_and(|p| p.session == sample.session && p.sequence >= sample.sequence)
    {
        return Ok(());
    }
    let continuous = tracker
        .last
        .as_ref()
        .is_some_and(|p| sample.follows(p, interval));
    let unchanged = v["type"] == "screen_unchanged";
    if unchanged && !continuous {
        return Err("Fresh screen text is needed after a capture gap".into());
    }
    let tx = s.db.unchecked_transaction()?;
    let mut next = HashMap::new();
    let layout = if unchanged {
        None
    } else {
        Some(screen_captures::read_layout(v)?)
    };
    let reused = tracker.layout.as_ref().filter(|(_, previous)| {
        continuous && layout.as_ref().is_none_or(|current| current == previous)
    });
    let layout_id = if let Some((id, _)) = reused {
        tx.execute(
            "UPDATE observations SET end_at=?1 WHERE id=?2 AND end_at<?1",
            params![sample.at, id],
        )?;
        *id
    } else {
        let current = layout.as_ref().ok_or("Missing previous screen layout")?;
        let id = s.observation(
            sample.at,
            sample.at,
            "screen_layout",
            "capture",
            &current.text(),
            &json!({"capture_group":true}),
            None,
        )?;
        tx.execute(
            "INSERT INTO passive_layouts(observation_id,geometry) VALUES(?1,?2)",
            params![id, serde_json::to_string(current)?],
        )?;
        id
    };
    tx.execute(
        "INSERT INTO passive_captures(at,layout_id) VALUES(?1,?2)",
        params![sample.at, layout_id],
    )?;
    let capture_id = tx.last_insert_rowid();
    let image = if unchanged {
        tracker.image
    } else if let Some(encoded) = v["jpeg"].as_str() {
        use base64::Engine;
        if encoded.len() > 2_796_204 {
            return Err("Screen image exceeds 2 MiB".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        if bytes.len() > 2 * 1024 * 1024
            || !bytes.starts_with(&[0xff, 0xd8, 0xff])
            || !bytes.ends_with(&[0xff, 0xd9])
        {
            return Err("Invalid passive screenshot".into());
        }
        tx.execute("INSERT INTO passive_images(jpeg) VALUES(?1)", [bytes])?;
        Some(tx.last_insert_rowid())
    } else {
        None
    };
    if let Some(id) = image {
        tx.execute(
            "INSERT INTO passive_capture_images(capture_id,image_id) VALUES(?1,?2)",
            [capture_id, id],
        )?;
    }
    // Count checks, including unchanged ones. Only unreferenced pixels expire.
    tx.execute("DELETE FROM passive_capture_images WHERE capture_id<=(SELECT id FROM passive_captures ORDER BY id DESC LIMIT 1 OFFSET 50)",[])?;
    tx.execute(
        "DELETE FROM passive_images WHERE id NOT IN (SELECT image_id FROM passive_capture_images)",
        [],
    )?;
    if let Some(current) = &layout {
        for line in &current.lines {
            let key = line.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if next.contains_key(&key) {
                continue;
            }
            let previous = tracker.visible.get(&key).copied().filter(|_| continuous);
            let id = match previous {
                Some(id) => id,
                None => s.observation(
                    sample.at,
                    sample.at,
                    "screen",
                    "passive_screen",
                    &line.text,
                    &json!({"capture_layout":layout_id}),
                    None,
                )?,
            };
            next.insert(key, id);
        }
    }
    let visible = if unchanged { &tracker.visible } else { &next };
    if !visible.is_empty() {
        let ids: Vec<_> = visible.values().copied().collect();
        tx.execute("UPDATE observations SET end_at=?1 WHERE end_at<?1 AND id IN (SELECT value FROM json_each(?2))",
            params![sample.at,serde_json::to_string(&ids)?])?;
    }
    tx.commit()?;
    if let Some(current) = layout {
        tracker.layout = Some((layout_id, current));
        tracker.visible = next;
    }
    tracker.image = image;
    tracker.last = Some(sample);
    tracker.error = None;
    Ok(())
}

/// Browse by first sighting so extending a live visit cannot move it across pages.
pub fn history(s: &Store, request: &Value) -> Result<Value> {
    let query = request["query"].as_str().unwrap_or("").trim();
    if query.chars().count() > 200 {
        return Err("Search up to 200 characters of screen text.".into());
    }
    let cursor = serde_json::from_value::<Option<[i64; 2]>>(request["before"].clone())?
        .unwrap_or([i64::MAX, i64::MIN]);
    let mut stmt = s.db.prepare_cached(
        "SELECT id,at,max(at,end_at),substr(text,1,2048),length(text)>2048
         FROM observations WHERE kind='screen' AND source IN ('passive_screen','screen','vision')
         AND at<=?1 AND (at<?1 OR id>?2) AND trim(text)<>'' AND (?3='' OR instr(lower(text),lower(?3))>0)
         ORDER BY at DESC,id ASC LIMIT 51",
    )?;
    let mut items = stmt
        .query_map(params![cursor[0], cursor[1], query], |row| {
            Ok(json!({
                "id":row.get::<_,i64>(0)?, "at":row.get::<_,i64>(1)?, "end":row.get::<_,i64>(2)?,
                "text":row.get::<_,String>(3)?, "truncated":row.get::<_,bool>(4)?
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = items.len() > 50;
    items.truncate(50);
    let next = items
        .last()
        .filter(|_| more)
        .map(|p| json!([p["at"], p["id"]]));
    Ok(json!({"items":items, "next":next}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_checks_continue_during_warnings_and_wait_only_for_ready_audio() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        for pressure in [0, 1, 2] {
            assert_eq!(delay_reason(&s, pressure).unwrap(), None);
        }
        for id in 1..=8 {
            s.db.execute(
                "INSERT INTO sources(id,at,end_at,track,path,status) VALUES(?1,0,1,'me',?2,'streaming')",
                params![id, format!("audio-{id}")],
            ).unwrap();
        }
        s.db.execute("UPDATE sources SET status='pending' WHERE id<=4", [])
            .unwrap();
        s.db.execute(
            "UPDATE sources SET status='pending',retry_at=unixepoch()+60 WHERE id=5",
            [],
        )
        .unwrap();
        assert_eq!(delay_reason(&s, 2).unwrap(), None);
        s.db.execute("UPDATE sources SET retry_at=0 WHERE id=5", [])
            .unwrap();
        assert_eq!(
            delay_reason(&s, 2).unwrap(),
            Some("Waiting for queued audio to finish processing.")
        );
        assert_eq!(
            delay_reason(&s, 4).unwrap(),
            Some("Waiting for critical memory pressure to ease.")
        );
        s.db.execute("UPDATE sources SET status='streaming' WHERE id=1", [])
            .unwrap();
        assert_eq!(delay_reason(&s, 2).unwrap(), None);
    }

    #[test]
    fn capture_page_reports_failures_until_a_check_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let config = crate::Config::default();
        let inbox = crate::live_audio::Shared::default();
        let request = json!({"query":"no matching capture"});
        let error = json!({"type":"screen_error","message":"Screen capture timed out."});
        crate::ingest(&s, &config, &error, &inbox).unwrap();
        let page = screen_captures::page(&s, &request).unwrap();
        assert_eq!(page["capture_error"], "Screen capture timed out.");
        assert!(page["last_check"].is_null());
        crate::ingest(&s, &config, &frame(1, 123, "display", Some("")), &inbox).unwrap();
        let page = screen_captures::page(&s, &request).unwrap();
        assert_eq!(page["items"], json!([]));
        assert_eq!(page["last_check"], 123);
        assert!(page["capture_error"].is_null());
    }

    #[test]
    fn history_paginates_equal_times_and_updates_a_visit_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let add = |source, text: &str| {
            s.observation(100, 100, "screen", source, text, &json!({}), None)
                .unwrap()
        };
        add("clipboard", "pasted private sample");
        add("screenshot", "manual picture");
        add("screen", "");
        let old = add("screen", "Legacy text");
        let long = add("vision", &format!("{} Needle", "a".repeat(3000)));
        for n in 0..53 {
            add("passive_screen", &format!("Sample line {n}"));
        }
        let first = history(&s, &json!({})).unwrap();
        let second = history(&s, &json!({"before":first["next"]})).unwrap();
        let all: Vec<_> = [
            first["items"].as_array().unwrap(),
            second["items"].as_array().unwrap(),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(all.len(), 55);
        assert_eq!(
            all.iter()
                .map(|i| i["id"].as_i64().unwrap())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            55
        );
        assert_eq!(all.first().unwrap()["id"], old);
        assert!(second["next"].is_null());
        s.db.execute("UPDATE observations SET end_at=150 WHERE id=?1", [long])
            .unwrap();
        let match_page = history(&s, &json!({"query":"needle"})).unwrap();
        let hit = &match_page["items"][0];
        assert_eq!(hit["id"], long);
        assert_eq!(hit["at"], 100);
        assert_eq!(hit["end"], 150);
        assert_eq!(hit["truncated"], true);
        assert_eq!(
            history(&s, &json!({"query":"manual"})).unwrap()["items"],
            json!([])
        );
    }

    fn frame(seq: i64, at: i64, window: &str, text: Option<&str>) -> Value {
        let mut value = json!({"type":"screen_unchanged","sample":{"sequence":seq,"at":at,"session":"a","target":window}});
        if let Some(text) = text {
            value["type"] = json!("screen");
            value["text"] = json!(text);
        }
        value
    }

    fn visits(s: &Store) -> Vec<(String, i64, i64)> {
        s.db.prepare(
            "SELECT text,at,end_at FROM observations WHERE source='passive_screen' ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    #[test]
    fn repeated_frames_scrolling_and_edits_keep_one_visit_per_visible_line() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mut first = frame(1, 100, "w", Some("Header\nPrice $120\nHeader"));
        first["words"] =
            json!([{"line":0,"text":"Header","x":0.2},{"line":1,"text":"$120","x":0.8}]);
        // Automatic frames never inherit legacy retention or provider inputs.
        first["path"] = json!("images/unexpected.jpg");
        first["vision_pending"] = json!(true);
        observe(&s, &first, 5).unwrap();
        for f in [
            frame(2, 105, "w", None),
            frame(3, 110, "w", Some("Price   $120\nHeader")),
            frame(4, 115, "w", Some("Header\nPrice $125")),
            frame(5, 120, "w", Some("  \n")),
            frame(6, 125, "w", Some("Header")),
            frame(7, 130, "w", None),
        ] {
            observe(&s, &f, 5).unwrap();
        }
        assert_eq!(
            visits(&s),
            [
                ("Header".into(), 100, 115),
                ("Price $120".into(), 100, 110),
                ("Price $125".into(), 115, 115),
                ("Header".into(), 125, 130)
            ]
        );
        let details: String =
            s.db.query_row(
                "SELECT details FROM observations WHERE text='Price $120'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let details: Value = serde_json::from_str(&details).unwrap();
        assert!(details["capture_layout"].as_i64().is_some());
        assert!(details.get("path").is_none() && details.get("vision_pending").is_none());
        assert_eq!(
            s.db.query_row("SELECT count(*) FROM saved_images", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            std::fs::read_dir(dir.path().join("images"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn gaps_targets_and_recorder_restarts_end_visits_at_the_last_sighting() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        for f in [
            frame(1, 100, "one", Some("A")),
            frame(2, 105, "one", None),
            frame(3, 110, "two", Some("A")),
            frame(4, 115, "one", Some("A")),
        ] {
            observe(&s, &f, 5).unwrap();
        }
        assert!(observe(&s, &frame(6, 120, "one", None), 5).is_err());
        observe(&s, &frame(6, 120, "one", Some("A")), 5).unwrap();
        observe(&s, &frame(7, 134, "one", Some("A")), 5).unwrap();
        observe(&s, &frame(6, 125, "one", Some("late")), 5).unwrap();
        observe(&s, &frame(7, 134, "one", Some("duplicate")), 5).unwrap();
        let mut restart = frame(1, 139, "one", Some("A"));
        restart["sample"]["session"] = json!("new recorder");
        observe(&s, &restart, 5).unwrap();
        assert_eq!(
            visits(&s),
            [
                (100, 105),
                (110, 110),
                (115, 115),
                (120, 120),
                (134, 134),
                (139, 139)
            ]
            .map(|(a, b)| ("A".into(), a, b))
        );
    }

    #[test]
    fn a_failed_transaction_can_retry_the_same_sample() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        observe(&s, &frame(1, 100, "w", Some("A")), 5).unwrap();
        s.db.execute_batch("CREATE TRIGGER fail_update BEFORE UPDATE OF end_at ON observations BEGIN SELECT raise(ABORT,'fixture'); END;").unwrap();
        let edit = frame(2, 105, "w", Some("A\nB"));
        assert!(observe(&s, &edit, 5).is_err());
        assert_eq!(visits(&s), [("A".into(), 100, 100)]);
        s.db.execute_batch("DROP TRIGGER fail_update").unwrap();
        observe(&s, &edit, 5).unwrap();
        assert_eq!(visits(&s), [("A".into(), 100, 105), ("B".into(), 105, 105)]);
    }
}
