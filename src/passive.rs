use crate::{
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
    if !unchanged {
        let text = v["text"].as_str().ok_or("Missing passive OCR text")?;
        let mut boxes: HashMap<usize, Vec<&Value>> = HashMap::new();
        for word in v["words"].as_array().into_iter().flatten().take(1000) {
            if let Some(line) = word["line"].as_u64() {
                boxes.entry(line as usize).or_default().push(word);
            }
        }
        for (line, original) in text.lines().take(200).enumerate() {
            let key = original.split_whitespace().collect::<Vec<_>>().join(" ");
            if key.is_empty() || next.contains_key(&key) {
                continue;
            }
            let previous = tracker.visible.get(&key).copied().filter(|_| continuous);
            let id = match previous {
                Some(id) => id,
                None => s.observation(sample.at, sample.at, "screen", "passive_screen", original.trim(),
                    &json!({"words":boxes.remove(&line).unwrap_or_default(),"coordinates":"normalized bottom-left; word boxes at first sighting"}), None)?,
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
    if !unchanged {
        tracker.visible = next;
    }
    tracker.last = Some(sample);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(details["words"], json!([{"line":1,"text":"$120","x":0.8}]));
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
