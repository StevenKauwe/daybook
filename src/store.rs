use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub struct Store {
    pub db: Connection,
    pub root: PathBuf,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root.join("audio"))?;
        std::fs::create_dir_all(root.join("images"))?;
        let db = Connection::open(root.join("daybook.sqlite"))?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
 CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS observations(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,end_at INTEGER NOT NULL,kind TEXT NOT NULL,source TEXT NOT NULL,text TEXT NOT NULL,details TEXT NOT NULL,source_id INTEGER);
 CREATE TABLE IF NOT EXISTS saved_images(observation_id INTEGER PRIMARY KEY REFERENCES observations(id) ON DELETE CASCADE,png BLOB NOT NULL);
 CREATE TABLE IF NOT EXISTS sources(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,end_at INTEGER NOT NULL,track TEXT NOT NULL,path TEXT UNIQUE NOT NULL,status TEXT NOT NULL DEFAULT 'pending',error TEXT NOT NULL DEFAULT '',attempts INTEGER NOT NULL DEFAULT 0,retry_at INTEGER NOT NULL DEFAULT 0);
 CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY,kind TEXT NOT NULL,lo INTEGER NOT NULL,hi INTEGER NOT NULL,status TEXT NOT NULL DEFAULT 'pending',text TEXT NOT NULL DEFAULT '',error TEXT NOT NULL DEFAULT '',retry_at INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,UNIQUE(kind,lo,hi));
 CREATE TABLE IF NOT EXISTS provider_activity(id INTEGER PRIMARY KEY,started INTEGER NOT NULL,role TEXT NOT NULL,model TEXT NOT NULL,origin TEXT NOT NULL,is_local INTEGER NOT NULL,status TEXT NOT NULL,elapsed_ms INTEGER,input_bytes INTEGER NOT NULL,output_bytes INTEGER,reserved_units INTEGER NOT NULL,prompt_tokens INTEGER,completion_tokens INTEGER,estimated_usd REAL);
 CREATE TABLE IF NOT EXISTS provider_daily(day TEXT PRIMARY KEY,reported_prompt INTEGER NOT NULL DEFAULT 0,reported_completion INTEGER NOT NULL DEFAULT 0,estimated_usd REAL NOT NULL DEFAULT 0,priced_calls INTEGER NOT NULL DEFAULT 0,completed_calls INTEGER NOT NULL DEFAULT 0,reported_calls INTEGER NOT NULL DEFAULT 0);
 CREATE TABLE IF NOT EXISTS budget(day TEXT PRIMARY KEY,requests INTEGER NOT NULL,images INTEGER NOT NULL,tokens INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS source_metadata(source_id INTEGER PRIMARY KEY REFERENCES sources(id),details TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS source_sessions(source_id INTEGER PRIMARY KEY REFERENCES sources(id),session TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS job_parts(job_id INTEGER NOT NULL REFERENCES jobs(id),part INTEGER NOT NULL,input TEXT NOT NULL,result TEXT,PRIMARY KEY(job_id,part));
 CREATE TABLE IF NOT EXISTS export_dirty(day TEXT PRIMARY KEY);
 CREATE TABLE IF NOT EXISTS job_days(job_id INTEGER NOT NULL REFERENCES jobs(id),day TEXT NOT NULL,PRIMARY KEY(job_id,day));
 CREATE INDEX IF NOT EXISTS obs_at ON observations(at); CREATE INDEX IF NOT EXISTS source_status ON sources(status,id);
 CREATE INDEX IF NOT EXISTS obs_pending_ocr ON observations(id) WHERE kind='screen' AND json_extract(details,'$.ocr_status')='pending';
 CREATE INDEX IF NOT EXISTS obs_live ON observations(source,id DESC) WHERE kind IN ('preview','transcript');
 CREATE INDEX IF NOT EXISTS obs_transcript_at ON observations(at,id) WHERE kind IN ('preview','transcript');
 CREATE INDEX IF NOT EXISTS source_speech_gate ON source_metadata(json_extract(details,'$.speech_gate'));")?;
        let has_reported:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('provider_daily') WHERE name='reported_calls')",[],|r|r.get(0))?;
        if !has_reported {
            db.execute(
                "ALTER TABLE provider_daily ADD COLUMN reported_calls INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let migrated: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM state WHERE key='exports_source_dates_v3')",
            [],
            |r| r.get(0),
        )?;
        if !migrated {
            let body="
 INSERT OR IGNORE INTO export_dirty VALUES(date(NEW.created,'unixepoch','localtime'));
 INSERT OR IGNORE INTO export_dirty SELECT day FROM job_days WHERE job_id=NEW.id;
 DELETE FROM job_days WHERE job_id=NEW.id;
 INSERT OR IGNORE INTO job_days SELECT NEW.id,date(at,'unixepoch','localtime') FROM observations WHERE NEW.kind='incremental' AND id BETWEEN NEW.lo AND NEW.hi AND kind IN ('transcript','screen','note');
 INSERT OR IGNORE INTO job_days SELECT NEW.id,date(max(at,end_at-1),'unixepoch','localtime') FROM observations WHERE NEW.kind='incremental' AND id BETWEEN NEW.lo AND NEW.hi AND kind IN ('transcript','screen','note');
 INSERT OR IGNORE INTO job_days SELECT NEW.id,d.day FROM job_days d JOIN jobs j ON j.id=d.job_id WHERE NEW.kind='meta' AND j.kind='incremental' AND j.id BETWEEN NEW.lo AND NEW.hi;
 INSERT OR IGNORE INTO job_days SELECT NEW.id,date(NEW.created,'unixepoch','localtime') WHERE NOT EXISTS(SELECT 1 FROM job_days WHERE job_id=NEW.id);
 INSERT OR IGNORE INTO export_dirty SELECT day FROM job_days WHERE job_id=NEW.id;";
            db.execute_batch(&format!("BEGIN IMMEDIATE;
 DROP TRIGGER IF EXISTS job_export_updated; DROP TRIGGER IF EXISTS job_export_inserted;
 CREATE TRIGGER job_export_updated AFTER UPDATE OF status,text ON jobs WHEN NEW.status='done' BEGIN {body} END;
 CREATE TRIGGER job_export_inserted AFTER INSERT ON jobs WHEN NEW.status='done' BEGIN {body} END;
 UPDATE jobs SET text=text WHERE status='done' AND kind='incremental';
 UPDATE jobs SET text=text WHERE status='done' AND kind!='incremental';
 INSERT OR IGNORE INTO state VALUES('exports_source_dates_v3','1'); COMMIT;"))?;
        }
        Ok(Self {
            db,
            root: root.into(),
        })
    }
    pub fn recover(&self) -> Result<()> {
        let lost:i64=self.db.query_row("SELECT count(*) FROM sources WHERE path LIKE 'memory:%' AND status IN ('pending','running','streaming')",[],|r|r.get(0))?;
        if lost > 0 {
            self.db.execute("UPDATE sources SET status='lost',error='Unfinished in-memory audio was lost when the process exited' WHERE path LIKE 'memory:%' AND status IN ('pending','running','streaming')",[])?;
            self.event(&format!("AUDIO LOST ON RESTART: {lost} unfinished in-memory blocks cannot be replayed. Saved transcript text remains; enable disk recovery only if needed."))?;
        }
        self.db.execute("UPDATE provider_activity SET status='interrupted; outcome unknown' WHERE status='running'",[])?;
        self.db.execute(
            "UPDATE sources SET status='done' WHERE status IN ('compressing','expiring_done')",
            [],
        )?;
        self.db.execute(
            "UPDATE sources SET status='pending' WHERE status IN ('running','streaming','expiring_pending')",
            [],
        )?;
        self.db.execute("UPDATE observations SET kind='preview_abandoned',details=json_set(details,'$.replay_required',1) WHERE kind='preview'",[])?;
        self.db.execute(
            "UPDATE jobs SET status='pending' WHERE status='running'",
            [],
        )?;
        Ok(())
    }
    pub fn state(&self, k: &str, v: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO state VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE state.value<>excluded.value",
            params![k, v],
        )?;
        Ok(())
    }
    pub fn get(&self, k: &str) -> String {
        self.db
            .query_row("SELECT value FROM state WHERE key=?1", [k], |r| r.get(0))
            .unwrap_or_default()
    }
    pub fn observation(
        &self,
        at: i64,
        end: i64,
        kind: &str,
        source: &str,
        text: &str,
        details: &Value,
        sid: Option<i64>,
    ) -> Result<i64> {
        self.db.execute("INSERT INTO observations(at,end_at,kind,source,text,details,source_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![at,end,kind,source,text,details.to_string(),sid])?;
        Ok(self.db.last_insert_rowid())
    }
    pub fn event(&self, text: &str) -> Result<()> {
        self.observation(now(), now(), "event", "system", text, &json!({}), None)?;
        self.state("notice", text)
    }
    /// Preserve originals for inspection, but exclude proven silent-audio hallucinations
    /// from the live reader and future compression. Missing/legacy levels are never guessed.
    pub fn suppress_proven_silence(&self) -> Result<usize> {
        if self.get("silent_transcript_review_v1") == "done" {
            return Ok(0);
        }
        let n=self.db.execute("UPDATE observations SET kind='transcript_suppressed',details=json_set(details,'$.suppressed_reason','Every source block measured at the -120 dBFS signal floor; original recognizer output retained for inspection') WHERE kind='transcript' AND text!='' AND json_array_length(details,'$.source_ids')>0 AND NOT EXISTS(SELECT 1 FROM json_each(observations.details,'$.source_ids') ids LEFT JOIN source_metadata m ON m.source_id=ids.value WHERE coalesce(json_type(m.details,'$.level.peak_dbfs'),'null') NOT IN ('real','integer') OR json_extract(m.details,'$.level.peak_dbfs')>-120)",[])?;
        if n > 0 {
            self.event(&format!("Suppressed {n} past silent-audio transcript(s) from reading and future summaries. Original text and sources remain available under Archive → Suppressed audio."))?;
        }
        self.state("silent_transcript_review_v1", "done")?;
        Ok(n)
    }
    #[cfg(test)]
    pub fn add_source(&self, at: i64, end: i64, track: &str, path: &str) -> Result<()> {
        self.add_source_session(at, end, track, path, "")
    }
    pub fn add_source_session(
        &self,
        at: i64,
        end: i64,
        track: &str,
        path: &str,
        session: &str,
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO sources(at,end_at,track,path) VALUES(?1,?2,?3,?4)",
            params![at, end, track, path],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO source_sessions SELECT id,?2 FROM sources WHERE path=?1",
            params![path, session],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn admit_audio(&self, v: &Value) -> Result<()> {
        let path = v["path"].as_str().ok_or("source path missing")?;
        let loss = v["loss_reason"].as_str();
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO sources(at,end_at,track,path,status,error) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                v["at"].as_f64().unwrap_or(now() as f64).floor() as i64,
                v["end"].as_f64().unwrap_or(now() as f64).ceil() as i64,
                v["track"].as_str().ok_or("track missing")?,
                path,
                if loss.is_some() { "lost" } else { "pending" },
                loss.unwrap_or("")
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO source_sessions SELECT id,?2 FROM sources WHERE path=?1",
            params![path, v["session"].as_str().unwrap_or("")],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO source_metadata SELECT id,?2 FROM sources WHERE path=?1",
            params![path, v.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn source_metadata(&self, path: &str, v: &Value) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO source_metadata SELECT id,?2 FROM sources WHERE path=?1",
            params![path, v.to_string()],
        )?;
        Ok(())
    }
    pub fn pending(&self) -> usize {
        self.db
            .query_row(
                "SELECT count(*) FROM sources WHERE status IN ('pending','running','streaming')",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize
    }
    pub fn recent(&self) -> Result<Value> {
        let mut q=self.db.prepare("SELECT id,at,end_at,kind,source,text,details,source_id FROM observations ORDER BY id DESC LIMIT 100")?;
        let rows=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"kind":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"details":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or(Value::Null),"source_id":r.get::<_,Option<i64>>(7)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(json!(rows))
    }
    pub fn summary_chars(&self) -> i64 {
        self.db
            .query_row(
                "SELECT coalesce(sum(length(text)),0) FROM jobs WHERE status='done'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }
    pub fn search(&self, text: &str) -> Result<Value> {
        let mut q=self.db.prepare("SELECT id,at,kind,source,text,source_id FROM observations WHERE instr(lower(text),lower(?1))>0 ORDER BY id DESC LIMIT 100")?;
        let rows=q.query_map([text],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"source":r.get::<_,String>(3)?,"text":r.get::<_,String>(4)?,"source_id":r.get::<_,Option<i64>>(5)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(json!(rows))
    }
    pub fn summaries(&self) -> Result<Value> {
        let mut q = self.db.prepare(
            "SELECT id,kind,lo,hi,status,text,error FROM jobs ORDER BY id DESC LIMIT 50",
        )?;
        Ok(json!(q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"lo":r.get::<_,i64>(2)?,"hi":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"error":r.get::<_,String>(6)?})))?.collect::<std::result::Result<Vec<_>,_>>()?))
    }
    pub fn reserve(&mut self, c: &crate::config::Config, images: u64, tokens: u64) -> Result<()> {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let tx = self.db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO budget VALUES(?1,0,0,0)", [&day])?;
        let (r, i, t): (i64, i64, i64) = tx.query_row(
            "SELECT requests,images,tokens FROM budget WHERE day=?1",
            [&day],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if r + 1 > c.daily_requests as i64
            || i + images as i64 > c.daily_images as i64
            || t + tokens as i64 > c.daily_token_reservation as i64
        {
            return Err("daily request/image/token reservation budget exhausted".into());
        }
        tx.execute(
            "UPDATE budget SET requests=requests+1,images=images+?2,tokens=tokens+?3 WHERE day=?1",
            params![day, images as i64, tokens as i64],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn prepare_job(&self, kind: &str) -> Result<Option<i64>> {
        let existing=self.db.query_row("SELECT id FROM jobs WHERE kind=?1 AND status IN ('pending','running','failed') ORDER BY id LIMIT 1",[kind],|r|r.get(0));
        if let Ok(id) = existing {
            return Ok(Some(id));
        }
        let lo: i64 = self.db.query_row(
            "SELECT coalesce(max(hi),0)+1 FROM jobs WHERE kind=?1 AND status='done'",
            [kind],
            |r| r.get(0),
        )?;
        let sql = if kind == "meta" {
            "SELECT id,length(CAST(text AS BLOB)),'meta' FROM jobs WHERE kind='incremental' AND status='done' AND id>=?1 ORDER BY id LIMIT 30"
        } else {
            "SELECT id,length(CAST(text AS BLOB)),date(at,'unixepoch','localtime') FROM observations WHERE kind IN ('transcript','screen','note') AND id>=?1 AND id<min(coalesce((SELECT min(id) FROM observations WHERE kind='preview'),9223372036854775807),coalesce((SELECT min(id) FROM observations WHERE kind='screen' AND json_extract(details,'$.ocr_status')='pending'),9223372036854775807)) ORDER BY id LIMIT 80"
        };
        let mut q = self.db.prepare(sql)?;
        let mut bytes = 0;
        let mut hi = None;
        let mut day = None;
        for row in q.query_map([lo], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (id, len, source_day) = row?;
            if day.as_ref().is_some_and(|d| d != &source_day) {
                break;
            }
            day = Some(source_day);
            if hi.is_some() && bytes + len + 128 > 62000 {
                break;
            }
            bytes += len + 128;
            hi = Some(id);
        }
        let Some(hi) = hi else { return Ok(None) };
        self.db.execute(
            "INSERT OR IGNORE INTO jobs(kind,lo,hi,created) VALUES(?1,?2,?3,?4)",
            params![kind, lo, hi, now()],
        )?;
        Ok(Some(self.db.last_insert_rowid()))
    }
    /// Snapshot bounded provider inputs once; completed parts survive job retries/crashes.
    pub fn prepare_parts(&self, id: i64, kind: &str, lo: i64, hi: i64) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM job_parts WHERE job_id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(());
        }
        let sql = if kind == "meta" {
            "SELECT id,created,'summary',text FROM jobs WHERE id BETWEEN ?1 AND ?2 AND kind='incremental' AND status='done' ORDER BY id"
        } else {
            "SELECT id,at,source,text FROM observations WHERE id BETWEEN ?1 AND ?2 AND kind IN ('transcript','screen','note') ORDER BY id"
        };
        let mut q = tx.prepare(sql)?;
        let mut input = String::new();
        let mut part = 0;
        for row in q.query_map(params![lo, hi], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })? {
            let (source_id, at, source, text) = row?;
            let mut offset = 0;
            loop {
                let prefix = format!("[{source_id} @{at} {source} bytes {offset}] ");
                if prefix.len() + 5 > 64000 {
                    return Err("source metadata exceeds provider input limit".into());
                }
                if input.len() + prefix.len() + 5 > 64000 {
                    tx.execute(
                        "INSERT INTO job_parts(job_id,part,input) VALUES(?1,?2,?3)",
                        params![id, part, input],
                    )?;
                    part += 1;
                    input.clear();
                }
                let mut end = text
                    .len()
                    .min(offset + 64000 - input.len() - prefix.len() - 1);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                input.push_str(&prefix);
                input.push_str(&text[offset..end]);
                input.push('\n');
                offset = end;
                if offset == text.len() {
                    break;
                }
            }
        }
        if !input.is_empty() {
            tx.execute(
                "INSERT INTO job_parts(job_id,part,input) VALUES(?1,?2,?3)",
                params![id, part, input],
            )?;
        }
        drop(q);
        tx.commit()?;
        Ok(())
    }
    pub fn next_part(&self, id: i64) -> Result<Option<(i64, String)>> {
        use rusqlite::OptionalExtension;
        Ok(self.db.query_row("SELECT part,input FROM job_parts WHERE job_id=?1 AND result IS NULL ORDER BY part LIMIT 1",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)
    }
    pub fn complete_part(&self, id: i64, part: i64, text: &str) -> Result<()> {
        self.db.execute(
            "UPDATE job_parts SET result=?3 WHERE job_id=?1 AND part=?2 AND result IS NULL",
            params![id, part, text],
        )?;
        Ok(())
    }
    pub fn part_output(&self, id: i64) -> Result<String> {
        let mut q = self
            .db
            .prepare("SELECT result FROM job_parts WHERE job_id=?1 ORDER BY part")?;
        let mut text = String::new();
        for row in q.query_map([id], |r| r.get::<_, Option<String>>(0))? {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&row?.ok_or("summary part still pending")?);
        }
        Ok(text)
    }
    pub fn export(&self) -> Result<()> {
        use std::io::Write;
        // Dirty dates are inserted in the same transaction as completion. A crash
        // between completion and rename leaves a durable repair request.
        let today = chrono::Local::now().date_naive().to_string();
        self.db
            .execute("INSERT OR IGNORE INTO export_dirty VALUES(?1)", [&today])?;
        loop {
            use rusqlite::{OptionalExtension, Transaction, TransactionBehavior};
            // The API and the worker can export concurrently. Reserve this writer
            // before choosing a day or touching its temporary file. Another writer
            // can then wait normally instead of invalidating a read snapshot.
            let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
            let day: Option<String> = tx
                .query_row(
                    "SELECT day FROM export_dirty ORDER BY day LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(day) = day else {
                break;
            };
            let path = self.root.join(format!("{day}.txt"));
            let temp = path.with_extension("tmp");
            let mut f = std::fs::File::create(&temp)?;
            writeln!(
                f,
                "Daybook — {day}\nTimes are Unix UTC. References resolve in daybook.sqlite. Summaries do not replace sources.\n"
            )?;
            let mut q=tx.prepare("SELECT id,kind,lo,hi,text,(SELECT group_concat(day,', ') FROM job_days WHERE job_id=jobs.id) FROM jobs WHERE status='done' AND id IN (SELECT job_id FROM job_days WHERE day=?1) ORDER BY id")?;
            for row in q.query_map([&day],|r|Ok(format!("Summary #{} ({}, source IDs {}–{})\nSource dates: {}. If multiple dates are listed, this summary contains cross-day evidence and is included on each date.\n{}\n\n",r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(5)?,r.get::<_,String>(4)?)))? {
                f.write_all(row?.as_bytes())?;
            }
            f.sync_all()?;
            std::fs::rename(temp, path)?;
            drop(q);
            tx.execute("DELETE FROM export_dirty WHERE day=?1", [&day])?;
            tx.commit()?;
        }
        Ok(())
    }
}

pub fn disk_bytes(root: &Path) -> u64 {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_symlink() {
                0
            } else if p.is_dir() {
                disk_bytes(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> Store {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "daybook-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        Store::open(&p).unwrap()
    }
    #[test]
    fn overlapping_exports_and_recorder_writes_finish_with_a_clean_day() {
        let s = store();
        let peer = Store::open(&s.root).unwrap();
        let recorder = Store::open(&s.root).unwrap();
        let start = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let start = &start;
            for store in [s, peer] {
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..30 {
                        store.export().unwrap();
                    }
                });
            }
            start.wait();
            for i in 0..120 {
                recorder
                    .state("concurrent_heartbeat", &i.to_string())
                    .unwrap();
            }
        });
        assert_eq!(
            recorder
                .db
                .query_row("SELECT count(*) FROM export_dirty", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let day = chrono::Local::now().date_naive();
        let text = std::fs::read_to_string(recorder.root.join(format!("{day}.txt"))).unwrap();
        assert!(text.contains(&day.to_string()));
        assert!(!recorder.root.join(format!("{day}.tmp")).exists());
    }
    #[test]
    fn large_vision_observation_does_not_block_later_material() {
        let s = store();
        s.observation(
            now(),
            now(),
            "screen",
            "vision",
            &"v".repeat(63000),
            &json!({"observation_id":7}),
            None,
        )
        .unwrap();
        s.observation(now(), now(), "note", "me", "later action", &json!({}), None)
            .unwrap();
        assert!(
            s.prepare_job("incremental").unwrap().is_some(),
            "63 KB vision output must make progress"
        );
    }
    #[test]
    fn recovered_previous_day_job_appears_in_its_daily_export() {
        let s = store();
        let yesterday = chrono::Local::now().date_naive().pred_opt().unwrap();
        let created = yesterday
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap()
            .timestamp();
        s.db.execute(
            "INSERT INTO jobs(kind,lo,hi,status,created) VALUES('incremental',1,1,'running',?1)",
            [created],
        )
        .unwrap();
        s.recover().unwrap();
        s.db.execute(
            "UPDATE jobs SET status='done',text='recovered yesterday evidence'",
            [],
        )
        .unwrap();
        s.export().unwrap();
        let path = s.root.join(format!("{yesterday}.txt"));
        assert!(
            path.exists(),
            "recovered job must update its original daily file"
        );
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("recovered yesterday evidence")
        );
    }
    #[test]
    fn max_utf8_input_parts_resume_without_loss_or_replaying_completed_parts() {
        let s = store();
        let original = "🦀".repeat(16000); // Provider accepts exactly 64,000 bytes.
        s.observation(
            now(),
            now(),
            "screen",
            "vision",
            &original,
            &json!({}),
            None,
        )
        .unwrap();
        s.observation(now(), now(), "note", "me", "later action", &json!({}), None)
            .unwrap();
        let id = s.prepare_job("incremental").unwrap().unwrap();
        s.prepare_parts(id, "incremental", 1, 1).unwrap();
        let (part, input) = s.next_part(id).unwrap().unwrap();
        assert!(input.len() <= 64000);
        let mut reconstructed = input
            .split_once("] ")
            .unwrap()
            .1
            .trim_end_matches('\n')
            .to_string();
        s.complete_part(id, part, "first completed result").unwrap();
        s.db.execute("UPDATE jobs SET status='running' WHERE id=?1", [id])
            .unwrap();
        s.recover().unwrap();
        s.prepare_parts(id, "incremental", 1, 1).unwrap();
        let (part2, input2) = s.next_part(id).unwrap().unwrap();
        assert_eq!(part2, part + 1);
        assert!(input2.len() <= 64000);
        assert!(input2.starts_with("[1 @") && input2.contains("vision bytes "));
        reconstructed.push_str(input2.split_once("] ").unwrap().1.trim_end_matches('\n'));
        assert_eq!(
            reconstructed, original,
            "all UTF-8 source bytes must reach summary inputs"
        );
        s.complete_part(id, part2, "second completed result")
            .unwrap();
        assert!(s.next_part(id).unwrap().is_none());
        assert_eq!(
            s.part_output(id).unwrap(),
            "first completed result\n\nsecond completed result"
        );
        s.db.execute(
            "UPDATE jobs SET status='done',text=?2 WHERE id=?1",
            params![id, "m".repeat(64000)],
        )
        .unwrap();
        let next = s.prepare_job("incremental").unwrap().unwrap();
        assert_ne!(next, id);
        let meta = s.prepare_job("meta").unwrap().unwrap();
        s.prepare_parts(meta, "meta", id, id).unwrap();
        assert!(s.next_part(meta).unwrap().unwrap().1.len() <= 64000);
    }
    #[test]
    fn dirty_daily_exports_survive_reopen_and_do_not_mix_dates() {
        let s = store();
        let root = s.root.clone();
        let yesterday = chrono::Local::now().date_naive().pred_opt().unwrap();
        let old = yesterday
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap()
            .timestamp();
        s.db.execute("INSERT INTO jobs(kind,lo,hi,status,text,created) VALUES('incremental',1,1,'done','old evidence',?1)",[old]).unwrap();
        s.db.execute("INSERT INTO jobs(kind,lo,hi,status,text,created) VALUES('incremental',2,2,'done','today evidence',?1)",[now()]).unwrap();
        // Simulate old-version DB requiring one-time export repair.
        s.db.execute("DELETE FROM export_dirty", []).unwrap();
        s.db.execute("DELETE FROM state WHERE key='exports_source_dates_v3'", [])
            .unwrap();
        drop(s);
        let s = Store::open(&root).unwrap();
        s.export().unwrap();
        let old_text = std::fs::read_to_string(root.join(format!("{yesterday}.txt"))).unwrap();
        let today_text = std::fs::read_to_string(
            root.join(format!("{}.txt", chrono::Local::now().date_naive())),
        )
        .unwrap();
        assert!(old_text.contains("old evidence") && !old_text.contains("today evidence"));
        assert!(today_text.contains("today evidence") && !today_text.contains("old evidence"));
        s.export().unwrap();
        assert_eq!(
            old_text,
            std::fs::read_to_string(root.join(format!("{yesterday}.txt"))).unwrap()
        );
        assert_eq!(
            s.db.query_row("SELECT count(*) FROM export_dirty", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn failed_export_keeps_dirty_checkpoint_for_retry() {
        let s = store();
        s.db.execute("INSERT INTO jobs(kind,lo,hi,status,text,created) VALUES('incremental',1,1,'done','durable result',?1)",[now()]).unwrap();
        let path = s
            .root
            .join(format!("{}.txt", chrono::Local::now().date_naive()));
        std::fs::create_dir(&path).unwrap(); // Force atomic rename failure.
        assert!(s.export().is_err());
        assert!(
            s.db.query_row("SELECT count(*) FROM export_dirty", [], |r| r
                .get::<_, i64>(0))
                .unwrap()
                > 0
        );
        std::fs::remove_dir(&path).unwrap();
        s.export().unwrap();
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("durable result")
        );
    }
    #[test]
    fn source_dates_split_midnight_and_label_legacy_cross_day_summaries() {
        let s = store();
        let today = chrono::Local::now().date_naive();
        let yesterday = today.pred_opt().unwrap();
        let old = yesterday
            .and_hms_opt(23, 59, 58)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap()
            .timestamp();
        let new = today
            .and_hms_opt(0, 0, 2)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap()
            .timestamp();
        s.observation(
            old,
            old + 1,
            "note",
            "me",
            "before midnight",
            &json!({}),
            None,
        )
        .unwrap();
        s.observation(
            new,
            new + 1,
            "note",
            "me",
            "after midnight",
            &json!({}),
            None,
        )
        .unwrap();
        let first = s.prepare_job("incremental").unwrap().unwrap();
        assert_eq!(
            s.db.query_row("SELECT hi FROM jobs WHERE id=?1", [first], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        s.db.execute(
            "UPDATE jobs SET status='done',text='old only' WHERE id=?1",
            [first],
        )
        .unwrap();
        let second = s.prepare_job("incremental").unwrap().unwrap();
        s.db.execute(
            "UPDATE jobs SET status='done',text='new only' WHERE id=?1",
            [second],
        )
        .unwrap();
        s.export().unwrap();
        let a = std::fs::read_to_string(s.root.join(format!("{yesterday}.txt"))).unwrap();
        let b = std::fs::read_to_string(s.root.join(format!("{today}.txt"))).unwrap();
        assert!(a.contains("old only") && !a.contains("new only"));
        assert!(b.contains("new only") && !b.contains("old only"));
        // A legacy combined summary cannot be truthfully divided after the fact.
        s.db.execute("INSERT INTO jobs(kind,lo,hi,status,text,created) VALUES('incremental',1,2,'done','legacy mixed evidence',?1)",[now()]).unwrap();
        let meta = s.prepare_job("meta").unwrap().unwrap();
        s.db.execute(
            "UPDATE jobs SET status='done',text='cross-day consolidation' WHERE id=?1",
            [meta],
        )
        .unwrap();
        s.export().unwrap();
        for date in [yesterday, today] {
            let out = std::fs::read_to_string(s.root.join(format!("{date}.txt"))).unwrap();
            assert!(
                out.contains("legacy mixed evidence")
                    && out.contains("cross-day consolidation")
                    && out.contains("cross-day evidence")
            );
        }
    }
    #[test]
    fn live_preview_blocks_compression_cursor_until_finalized_or_recovered() {
        let s = store();
        s.add_source(now(), now() + 1, "me", "audio/test.f32")
            .unwrap();
        s.db.execute("UPDATE sources SET status='streaming'", [])
            .unwrap();
        s.observation(
            now(),
            now() + 1,
            "preview",
            "me",
            "draft",
            &json!({}),
            Some(1),
        )
        .unwrap();
        s.observation(now(), now(), "note", "me", "later note", &json!({}), None)
            .unwrap();
        assert!(s.prepare_job("incremental").unwrap().is_none());
        assert_eq!(s.pending(), 1);
        s.recover().unwrap();
        assert_eq!(s.recent().unwrap()[1]["kind"], "preview_abandoned");
        assert_eq!(
            s.db.query_row("SELECT status FROM sources WHERE id=1", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "pending"
        );
        assert!(s.prepare_job("incremental").unwrap().is_some());
    }
    #[test]
    fn budgets_persist_failed_requests() {
        let mut s = store();
        let c = crate::config::Config {
            daily_requests: 1,
            ..Default::default()
        };
        s.reserve(&c, 0, 10).unwrap();
        assert!(s.reserve(&c, 0, 10).is_err());
    }
    #[test]
    fn checkpoint_idempotence_and_recovery() {
        let s = store();
        s.observation(
            1,
            2,
            "transcript",
            "me",
            "Decision: ship tomorrow",
            &json!({}),
            None,
        )
        .unwrap();
        let id = s.prepare_job("incremental").unwrap().unwrap();
        assert_eq!(Some(id), s.prepare_job("incremental").unwrap());
        s.db.execute("UPDATE jobs SET status='running'", [])
            .unwrap();
        s.recover().unwrap();
        assert_eq!(Some(id), s.prepare_job("incremental").unwrap());
        s.db.execute("UPDATE jobs SET status='done'", []).unwrap();
        assert!(s.prepare_job("incremental").unwrap().is_none());
    }
    #[test]
    fn retained_source_link() {
        let s = store();
        s.add_source(1, 2, "me", "audio/test.f32").unwrap();
        s.observation(1, 2, "transcript", "me", "hi", &json!({}), Some(1))
            .unwrap();
        assert_eq!(s.recent().unwrap()[0]["source_id"], 1);
    }
}
