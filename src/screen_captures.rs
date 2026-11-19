//! Browse individual automatic checks; identical consecutive checks share one layout.
use crate::{
    screen_layout::{Bounds, Layout, Line},
    store::{Result, Store},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

fn bounds(v: &Value) -> Option<Bounds> {
    let x = v["x"].as_f64()?;
    let y = v["y"].as_f64()?;
    let w = v["width"].as_f64()?;
    let h = v["height"].as_f64()?;
    let left = x.clamp(0., 1.) as f32;
    let bottom = y.clamp(0., 1.) as f32;
    let width = (x + w).clamp(0., 1.) as f32 - left;
    let height = (y + h).clamp(0., 1.) as f32 - bottom;
    (width > 0. && height > 0.).then_some([left, bottom, width, height])
}

fn read_line(text: &str, rect: Option<Bounds>, words: &[&Value]) -> Line {
    let tokens: Vec<_> = text.split_whitespace().collect();
    let mut placed = Vec::new();
    let mut cursor = 0;
    for &word in words {
        let Some(rect) = bounds(word) else { continue };
        let index = match word["index"].as_u64() {
            Some(i) => usize::try_from(i).ok(),
            None => word["text"]
                .as_str()
                .and_then(|value| tokens[cursor..].iter().position(|w| *w == value))
                .map(|i| i + cursor),
        };
        if let Some(i) = index.filter(|&i| i >= cursor && i < tokens.len()) {
            placed.push((i, rect));
            cursor = i + 1;
        }
    }
    let bounds = rect.or_else(|| {
        if placed.is_empty() || placed.len() != tokens.len() {
            return None;
        }
        let [mut x, mut y, w, h] = placed[0].1;
        let (mut right, mut top) = (x + w, y + h);
        for (_, [a, b, w, h]) in &placed[1..] {
            x = x.min(*a);
            y = y.min(*b);
            right = right.max(a + w);
            top = top.max(b + h);
        }
        Some([x, y, right - x, top - y])
    });
    Line {
        text: text.trim().into(),
        bounds,
        words: placed,
    }
}

pub(super) fn read_layout(v: &Value) -> Result<Layout> {
    let text = v["text"].as_str().ok_or("Missing passive OCR text")?;
    if text.len() > 16_000 {
        return Err("Passive screen text exceeds 16,000 bytes.".into());
    }
    let pixels = serde_json::from_value::<[u32; 2]>(v["pixels"].clone())
        .ok()
        .filter(|p| p[0] > 0 && p[1] > 0);
    let mut lines: Vec<_> = text
        .lines()
        .take(200)
        .map(|s| (s, None, Vec::new()))
        .collect();
    for item in v["line_boxes"].as_array().into_iter().flatten().take(200) {
        if let Some(line) = item["line"]
            .as_u64()
            .and_then(|i| lines.get_mut(i as usize))
        {
            line.1 = bounds(item);
        }
    }
    for item in v["words"].as_array().into_iter().flatten().take(1000) {
        if let Some(line) = item["line"]
            .as_u64()
            .and_then(|i| lines.get_mut(i as usize))
        {
            line.2.push(item);
        }
    }
    Ok(Layout {
        ocr_failed: v["ocr_failed"] == true,
        pixels,
        lines: lines
            .into_iter()
            .filter(|(s, _, _)| !s.trim().is_empty())
            .map(|(s, b, w)| read_line(s, b, &w))
            .collect(),
    })
}

const LEGACY: &str = "kind='screen' AND source IN ('passive_screen','screen','vision')
    AND json_extract(details,'$.capture_layout') IS NULL AND trim(text)<>''";
const PAGE: usize = 30;

pub fn page(s: &Store, request: &Value) -> Result<Value> {
    let query = request["query"].as_str().unwrap_or("").trim();
    if query.chars().count() > 200 {
        return Err("Search up to 200 characters of screen text.".into());
    }
    let [at, id] = serde_json::from_value::<Option<[i64; 2]>>(request["before"].clone())?
        .unwrap_or([i64::MAX, i64::MAX]);
    // Limit each ordered branch before combining the two generations of recordings.
    let sql = format!("WITH recent AS (
      SELECT c.id,c.at,substr(o.text,1,90) AS excerpt,EXISTS(SELECT 1 FROM passive_capture_images WHERE capture_id=c.id) AS image_available FROM passive_captures c
      JOIN observations o ON o.id=c.layout_id WHERE (c.at,c.id)<(?1,?2)
      AND (?3='' OR instr(lower(o.text),lower(?3))>0) ORDER BY c.at DESC,c.id DESC LIMIT 31
    ), earlier AS (
      SELECT -min(id) AS capture_id,at FROM observations WHERE {LEGACY} AND at<=?1
      GROUP BY at HAVING (at,-min(id))<(?1,?2) AND (?3='' OR max(instr(lower(text),lower(?3)))>0)
      ORDER BY at DESC LIMIT 31
    ) SELECT id,at,excerpt,image_available FROM recent UNION ALL
      SELECT e.capture_id,e.at,substr(o.text,1,90),0 FROM earlier e JOIN observations o ON o.id=-e.capture_id
      ORDER BY 2 DESC,1 DESC LIMIT 31");
    let mut q = s.db.prepare_cached(&sql)?;
    let mut items = q.query_map(params![at,id,query], |r| Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"excerpt":r.get::<_,String>(2)?,"image_available":r.get::<_,bool>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = items.len() > PAGE;
    items.truncate(PAGE);
    let next = items
        .last()
        .filter(|_| more)
        .map(|v| json!([v["at"], v["id"]]));
    let oldest: Option<i64> = s.db.query_row(
        "SELECT min(capture_id) FROM passive_capture_images",
        [],
        |r| r.get(0),
    )?;
    Ok(json!({"items":items,"next":next,"oldest_image":oldest}))
}

fn observation_capture(s: &Store, id: i64) -> Result<i64> {
    let group: Option<i64> = s.db.query_row("SELECT CASE kind WHEN 'screen_layout' THEN id ELSE json_extract(details,'$.capture_layout') END FROM observations WHERE id=?1",[id],|r|r.get(0)).optional()?.flatten();
    if let Some(group) = group {
        // Prefer a surviving screenshot from this passage when its first check has expired.
        return Ok(s.db.query_row(
            "SELECT coalesce((SELECT capture_id FROM passive_previews WHERE layout_id=?1),
            (SELECT min(id) FROM passive_captures WHERE layout_id=?1))",
            [group],
            |r| r.get(0),
        )?);
    }
    let sql = format!(
        "SELECT -min(id) FROM observations WHERE {LEGACY} AND at=(SELECT at FROM observations WHERE id=?1 AND {LEGACY})"
    );
    s.db.query_row(&sql, [id], |r| r.get::<_, Option<i64>>(0))?
        .ok_or_else(|| "This entry has no passive screen capture.".into())
}

pub fn detail(s: &Store, request: &Value) -> Result<Value> {
    let id = match request["observation"].as_i64() {
        Some(observation) => observation_capture(s, observation)?,
        None => request["id"].as_i64().ok_or("Choose a screen capture.")?,
    };
    if id > 0 {
        let (at,geometry,available) = s.db.query_row("SELECT c.at,l.geometry,EXISTS(SELECT 1 FROM passive_capture_images WHERE capture_id=c.id) FROM passive_captures c JOIN passive_layouts l ON l.observation_id=c.layout_id WHERE c.id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?)))?;
        return Ok(
            json!({"id":id,"at":at,"legacy":false,"image_available":available,"layout":serde_json::from_str::<Layout>(&geometry)?}),
        );
    }
    let representative = id
        .checked_neg()
        .filter(|v| *v > 0)
        .ok_or("Invalid capture.")?;
    let at: i64 = s.db.query_row(
        &format!("SELECT at FROM observations WHERE id=?1 AND {LEGACY}"),
        [representative],
        |r| r.get(0),
    )?;
    let mut q = s.db.prepare_cached(&format!(
        "SELECT text,details FROM observations WHERE at=?1 AND {LEGACY} ORDER BY id"
    ))?;
    let mut rows = q.query([at])?;
    let mut layout = Layout::default();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        let text: String = row.get(0)?;
        let details: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
        let words: Vec<_> = details["words"]
            .as_array()
            .into_iter()
            .flatten()
            .take(1000)
            .collect();
        let single_line = text.trim().lines().count() == 1;
        for (i, line) in text
            .lines()
            .enumerate()
            .filter(|(_, s)| !s.trim().is_empty())
        {
            if layout.lines.len() >= 200 {
                truncated = true;
                break;
            }
            let matching: Vec<_> = words
                .iter()
                .copied()
                .filter(|w| single_line || w["line"].as_u64() == Some(i as u64))
                .collect();
            layout.lines.push(read_line(line, None, &matching));
        }
        if truncated {
            break;
        }
    }
    Ok(json!({"id":id,"at":at,"legacy":true,"truncated":truncated,"layout":layout}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parallel, passive};
    fn sample(seq: i64, at: i64, x: f32) -> Value {
        json!({"type":"screen","sample":{"session":"fixture","target":"transient window","sequence":seq,"at":at},
            "text":"Open\nProject notes\nOpen", "pixels":[1280,800],
            "line_boxes":[{"line":0,"x":x,"y":0.8,"width":0.08,"height":0.04},
                {"line":1,"x":0.4,"y":0.5,"width":0.35,"height":0.04},
                {"line":2,"x":0.8,"y":0.1,"width":0.08,"height":0.04}],
            "words":[{"line":1,"index":0,"x":0.4,"y":0.5,"width":0.1,"height":0.04},
                {"line":1,"index":1,"x":0.65,"y":0.5,"width":0.1,"height":0.04}]})
    }

    #[test]
    fn records_each_check_and_preserves_movement_duplicates_and_copy_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mut same = sample(2, 105, 0.1);
        same["type"] = json!("screen_unchanged");
        let moved = sample(4, 115, 0.3);
        let mut blank = sample(5, 120, 0.3);
        blank["text"] = json!("  ");
        for frame in [
            sample(1, 100, 0.1),
            same,
            sample(3, 110, 0.1),
            moved.clone(),
            moved,
            blank,
        ] {
            passive::observe(&s, &frame, 5).unwrap();
        }
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        let page = page(&s, &json!({})).unwrap();
        let ids: Vec<_> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids.len(), 5);
        let get = |id| detail(&s, &json!({"id":id})).unwrap();
        assert_eq!(get(ids[0])["layout"]["lines"], json!([]));
        let first = &get(ids[4])["layout"];
        assert_eq!(first["pixels"], json!([1280, 800]));
        assert_eq!(first["lines"].as_array().unwrap().len(), 3);
        assert_eq!(first["lines"][2]["text"], first["lines"][0]["text"]);
        assert_ne!(first["lines"][2]["bounds"], first["lines"][0]["bounds"]);
        assert_eq!(first["lines"][1]["words"][1][0], 1);
        assert_eq!(&get(ids[3])["layout"], first);
        assert_eq!(&get(ids[2])["layout"], first);
        assert_ne!(
            &get(ids[1])["layout"]["lines"][0]["bounds"],
            &first["lines"][0]["bounds"]
        );
        let count = |sql| s.db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT count(*) FROM passive_layouts"), 3);
        assert_eq!(
            count("SELECT count(*) FROM observations WHERE source='passive_screen'"),
            2
        );
        let req = json!({"range":{"from":100,"until":121},"passive":true});
        let timeline = parallel::window(&s, &req).unwrap();
        let rows = timeline["passages"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0]["at"].as_i64(), rows[0]["end"].as_i64()),
            (Some(100), Some(110))
        );
        assert_eq!(rows[1]["text"], "Open\nProject notes\nOpen");
        assert_eq!(
            detail(&s, &json!({"observation":rows[1]["id"]})).unwrap()["id"],
            ids[1]
        );
        let visit = &passive::history(&s, &json!({"query":"Project"})).unwrap()["items"][0];
        assert_eq!(
            detail(&s, &json!({"observation":visit["id"]})).unwrap()["id"],
            ids[4]
        );
        assert_eq!(visit["end"], 115);
        let copy = parallel::copy(&s, &req).unwrap();
        assert_eq!(copy["count"], 2);
        assert_eq!(
            copy["text"]
                .as_str()
                .unwrap()
                .matches("Open\nProject notes\nOpen")
                .count(),
            2
        );
        assert_eq!(count("SELECT count(*) FROM saved_images"), 0);
    }

    #[test]
    fn legacy_groups_only_first_sightings_and_paginates_across_generations() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let legacy=s.observation(100,140,"screen","passive_screen","Two words",
            &json!({"words":[{"line":7,"text":"words","x":0.55,"y":0.4,"width":0.12,"height":0.03}]}),None).unwrap();
        let other = s
            .observation(
                100,
                110,
                "screen",
                "passive_screen",
                "Needle",
                &json!({}),
                None,
            )
            .unwrap();
        let image = s
            .observation(
                100,
                100,
                "screen",
                "clipboard",
                "Unrelated",
                &json!({}),
                None,
            )
            .unwrap();
        for i in 0..34 {
            passive::observe(&s, &sample(i + 1, 100 + i * 5, 0.1), 5).unwrap();
        }
        let first = page(&s, &json!({})).unwrap();
        let second = page(&s, &json!({"before":first["next"]})).unwrap();
        let all: std::collections::HashSet<_> = first["items"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["items"].as_array().unwrap())
            .map(|r| r["id"].as_i64().unwrap())
            .collect();
        assert_eq!(all.len(), 35);
        assert_eq!(
            second["items"].as_array().unwrap().last().unwrap()["id"],
            -legacy
        );
        let hit = &page(&s, &json!({"query":"needle"})).unwrap()["items"][0];
        assert_eq!(hit["id"], -legacy);
        let group = detail(&s, &json!({"observation":other})).unwrap();
        assert_eq!(group["id"], -legacy);
        assert_eq!(group["legacy"], true);
        assert_eq!(group["layout"]["pixels"], Value::Null);
        let lines = group["layout"]["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0]["bounds"].is_null());
        assert_eq!(lines[0]["words"][0][0], 1);
        assert_eq!(lines[1]["text"], "Needle");
        assert!(detail(&s, &json!({"observation":image})).is_err());
    }

    #[test]
    fn last_fifty_screen_images_survive_restart_and_expire_independently_of_text() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let fresh = |seq: i64| {
            let mut v = sample(seq, seq * 5, 0.2);
            v["jpeg"] = json!(STANDARD.encode([0xff, 0xd8, 0xff, seq as u8, 0xff, 0xd9]));
            v
        };
        passive::observe(&s, &fresh(1), 5).unwrap();
        for i in 2..=52 {
            let mut v = fresh(i);
            if i % 2 == 0 {
                v["type"] = json!("screen_unchanged");
                v.as_object_mut().unwrap().remove("jpeg");
            }
            passive::observe(&s, &v, 5).unwrap();
        }
        let count = |s: &Store, sql| s.db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count(&s, "SELECT count(*) FROM passive_capture_images"), 50);
        assert_eq!(count(&s, "SELECT count(*) FROM passive_images"), 25);
        assert_eq!(count(&s, "SELECT count(*) FROM passive_layouts"), 1);
        assert_eq!(count(&s, "SELECT count(*) FROM saved_images"), 0);
        let group = count(&s, "SELECT min(observation_id) FROM passive_layouts");
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(detail(&s, &json!({"observation":group})).unwrap()["id"], 3);
        assert_eq!(
            detail(&s, &json!({"id":2})).unwrap()["image_available"],
            false
        );
        assert_eq!(
            detail(&s, &json!({"id":2})).unwrap()["layout"]["lines"][1]["text"],
            "Project notes"
        );
        assert_eq!(
            detail(&s, &json!({"id":3})).unwrap()["image_available"],
            true
        );
        let image = |capture| {
            s.db.query_row("SELECT jpeg FROM passive_images i JOIN passive_capture_images c ON c.image_id=i.id WHERE c.capture_id=?1",[capture],|r|r.get::<_,Vec<u8>>(0)).unwrap()
        };
        assert_eq!(image(3), image(4));
        assert_ne!(image(4), image(5));
        let mut blank = fresh(53);
        blank["text"] = json!("");
        passive::observe(&s, &blank, 5).unwrap();
        let empty = detail(&s, &json!({"id":53})).unwrap();
        assert_eq!(empty["image_available"], true);
        assert_eq!(empty["layout"]["lines"], json!([]));
        assert_eq!(
            count(&s, "SELECT min(capture_id) FROM passive_capture_images"),
            4
        );
        let mut failed = fresh(54);
        failed["jpeg"] = json!("invalid jpeg");
        assert!(passive::observe(&s, &failed, 5).is_err());
        assert_eq!(count(&s, "SELECT count(*) FROM passive_captures"), 53);
        passive::observe(&s, &fresh(54), 5).unwrap();
        assert_eq!(
            count(&s, "SELECT min(capture_id) FROM passive_capture_images"),
            5
        );
    }

    #[test]
    fn malformed_boxes_are_omitted_and_exported_geometry_has_no_native_metadata() {
        let mut v = sample(1, 100, 0.1);
        v["pixels"] = json!([0, 800]);
        v["line_boxes"] = json!([{"line":0,"x":-0.02,"y":0.5,"width":0.12,"height":0.05},
            {"line":1,"x":0.1,"y":0.1,"width":-0.1,"height":0.1}]);
        v["words"] = json!([{"line":1,"index":99,"x":0.1,"y":0.1,"width":0.1,"height":0.1},
            {"line":2,"text":"Open","x":0.1,"y":0.1,"width":0,"height":0.1}]);
        let layout = read_layout(&v).unwrap();
        assert_eq!(layout.pixels, None);
        let clipped = layout.lines[0].bounds.unwrap();
        assert_eq!(clipped[0], 0.);
        assert!((clipped[2] - 0.1).abs() < 0.00001);
        assert!(
            layout.lines[1..]
                .iter()
                .all(|l| l.bounds.is_none() && l.words.is_empty())
        );
        assert!(
            !serde_json::to_string(&layout)
                .unwrap()
                .contains("transient window")
        );
    }
}
