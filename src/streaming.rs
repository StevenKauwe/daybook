//! Bounded per-track Speechworks sessions. Preview rows are revised in place and
//! excluded from compression until finalized. Durable source files permit replay.
use crate::store::{Result, Store};
use rusqlite::params;
use serde_json::json;
use speechworks_inference::{Model, StreamState};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
pub const REVISION: &str = "8d2690a99ab849d30b9d0fa70f6fda84883b2cef";
pub const PHRASE_FRAMES: usize = 20 * 16000;
const MAX_SOURCES: usize = 64;
const END_QUIET_SAMPLES: usize = 12000;
struct Buffered {
    id: i64,
    at: i64,
    end: i64,
    pcm: Vec<f32>,
}
struct Gate {
    detector: crate::voice::Detector,
    session: String,
    end: i64,
    day: Option<chrono::NaiveDate>,
    preroll: VecDeque<Buffered>,
}
struct Segment {
    state: StreamState,
    pcm: Vec<f32>,
    ids: Vec<i64>,
    row: i64,
    frames: usize,
    at: i64,
    end: i64,
    last: Instant,
    session: String,
}
#[derive(Default)]
pub struct Streams {
    me: Option<Segment>,
    remote: Option<Segment>,
    me_gate: Option<Gate>,
    remote_gate: Option<Gate>,
    inbox: crate::live_audio::Shared,
}
impl Streams {
    pub fn with_inbox(inbox: crate::live_audio::Shared) -> Self {
        Self {
            inbox,
            ..Self::default()
        }
    }
    fn slot(&mut self, track: &str) -> &mut Option<Segment> {
        if track == "me" {
            &mut self.me
        } else {
            &mut self.remote
        }
    }
    fn gate(&mut self, track: &str) -> &mut Option<Gate> {
        if track == "me" {
            &mut self.me_gate
        } else {
            &mut self.remote_gate
        }
    }
    pub fn feed(
        &mut self,
        s: &Store,
        model: &Model,
        id: i64,
        at: i64,
        end: i64,
        track: &str,
        session: &str,
        pcm: &[f32],
    ) -> Result<()> {
        if pcm.len() > 160000 || pcm.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
            return Err("invalid speech gate audio".into());
        }
        let discontinuity = self.gate(track).as_ref().is_some_and(|g| {
            g.session != session || at > g.end + 2 || at + 2 < g.end || source_day(at) != g.day
        });
        if discontinuity {
            self.finish(s, model, track)?;
            *self.gate(track) = None;
        }
        if self.gate(track).is_none() {
            *self.gate(track) = Some(Gate {
                detector: crate::voice::Detector::new()?,
                session: session.into(),
                end,
                day: source_day(at),
                preroll: VecDeque::new(),
            });
        }
        let continuing = self.slot(track).is_some();
        let activity = {
            let gate = self.gate(track).as_mut().unwrap();
            gate.end = end;
            gate.detector.process(pcm, continuing)?
        };
        s.state(&format!("voice_activity_{track}"),&json!({"at":crate::store::now(),"speech":activity.speech,"probability":activity.probability,"quiet_ms":activity.quiet_samples as f64/16.0,"detector":"Speechworks Silero VAD","revision":REVISION}).to_string())?;
        if !continuing && !activity.speech {
            // Keep one second of preroll independently from the admission queue, so
            // quiet inputs never consume inference or pin the queue indefinitely.
            release_source(s, &self.inbox, id, "no_speech")?;
            let gate = self.gate(track).as_mut().unwrap();
            let tail = &pcm[pcm.len().saturating_sub(16000)..];
            gate.preroll.push_back(Buffered {
                id,
                at: at.max(end - 1),
                end,
                pcm: tail.to_vec(),
            });
            while gate.preroll.len() > 2
                || gate.preroll.iter().map(|b| b.pcm.len()).sum::<usize>() > 16000
            {
                gate.preroll.pop_front();
            }
            return Ok(());
        }
        let pre = std::mem::take(&mut self.gate(track).as_mut().unwrap().preroll);
        for block in pre {
            self.transcribe(
                s, model, block.id, block.at, block.end, track, session, &block.pcm,
            )?;
        }
        self.transcribe(s, model, id, at, end, track, session, pcm)?;
        if activity.quiet_samples >= END_QUIET_SAMPLES {
            self.finish(s, model, track)?;
        }
        Ok(())
    }
    fn transcribe(
        &mut self,
        s: &Store,
        model: &Model,
        id: i64,
        at: i64,
        end: i64,
        track: &str,
        session: &str,
        pcm: &[f32],
    ) -> Result<()> {
        if pcm.is_empty() {
            s.db.execute("UPDATE sources SET status='done' WHERE id=?1", [id])?;
            return Ok(());
        }
        if pcm.len() > 160000 {
            return Err("stream source exceeds 10-second legacy admission".into());
        }
        let split = self.slot(track).as_ref().is_some_and(|x| {
            x.session != session
                || x.ids.len() >= MAX_SOURCES
                || x.frames + pcm.len() > PHRASE_FRAMES
                || at > x.end + 2
                || at < x.at
                || source_day(at) != source_day(x.at)
        });
        if split {
            self.finish(s, model, track)?;
        }
        if self.slot(track).is_none() {
            let row = s.observation(
                at,
                end,
                "preview",
                track,
                "",
                &json!({"final":false,"speechworks_revision":REVISION}),
                Some(id),
            )?;
            *self.slot(track) = Some(Segment {
                state: StreamState::new(),
                pcm: Vec::new(),
                ids: Vec::with_capacity(10),
                row,
                frames: 0,
                at,
                end,
                last: Instant::now(),
                session: session.into(),
            });
        }
        let x = self.slot(track).as_mut().unwrap();
        let compute_started = Instant::now();
        let text = if model.is_streaming() {
            pollster::block_on(model.transcribe_chunk(&mut x.state, pcm, false))?.text
        } else {
            x.pcm.extend_from_slice(pcm);
            String::new()
        };
        x.ids.push(id);
        x.frames += pcm.len();
        x.end = end;
        x.last = Instant::now();
        let tx = s.db.unchecked_transaction()?;
        tx.execute(
            "UPDATE sources SET status='streaming',error='' WHERE id=?1",
            [id],
        )?;
        tx.execute("UPDATE source_metadata SET details=json_set(details,'$.speech_gate','included') WHERE source_id=?1",[id])?;
        tx.execute(
            "UPDATE observations SET end_at=?2,text=?3,details=?4 WHERE id=?1",
            params![
                x.row,
                end,
                text,
                details(&x.ids, false, model.is_streaming()).to_string()
            ],
        )?;
        tx.commit()?;
        report(
            s,
            x,
            track,
            if model.is_streaming() {
                "preview"
            } else {
                "buffering"
            },
            compute_started.elapsed().as_secs_f64() * 1000.0,
            !text.is_empty(),
        )?;
        if x.frames >= PHRASE_FRAMES {
            self.finish(s, model, track)?;
        }
        Ok(())
    }
    fn finish(&mut self, s: &Store, model: &Model, track: &str) -> Result<()> {
        let inbox = self.inbox.clone();
        let Some(x) = self.slot(track).as_mut() else {
            return Ok(());
        };
        let compute_started = Instant::now();
        let text = if model.is_streaming() {
            pollster::block_on(model.transcribe_chunk(&mut x.state, &[], true))?.text
        } else if x.pcm.is_empty()
            || x.pcm.iter().map(|v| v * v).sum::<f32>() / (x.pcm.len() as f32) < 0.00001
        {
            String::new()
        } else {
            pollster::block_on(model.transcribe(&x.pcm))?.text
        };
        let tx = s.db.unchecked_transaction()?;
        tx.execute(
            "UPDATE observations SET kind='transcript',text=?2,details=?3 WHERE id=?1",
            params![
                x.row,
                text,
                details(&x.ids, true, model.is_streaming()).to_string()
            ],
        )?;
        let mut release = Vec::new();
        for id in &x.ids {
            let path: String =
                tx.query_row("SELECT path FROM sources WHERE id=?1", [id], |r| r.get(0))?;
            if path.starts_with("memory:") {
                release.push(path);
            }
            tx.execute(
                "UPDATE sources SET status=CASE WHEN path LIKE 'memory:%' THEN 'discarded' ELSE 'done' END,error='' WHERE id=?1",
                [id],
            )?;
        }
        tx.commit()?;
        for path in release {
            inbox.lock().unwrap().remove(&path);
        }
        report(
            s,
            x,
            track,
            "final",
            compute_started.elapsed().as_secs_f64() * 1000.0,
            true,
        )?;
        *self.slot(track) = None;
        Ok(())
    }
    pub fn flush(&mut self, s: &Store, model: &Model, force: bool) -> Result<()> {
        for track in ["me", "remote"] {
            if self
                .slot(track)
                .as_ref()
                .is_some_and(|x| force || x.last.elapsed() > Duration::from_secs(3))
            {
                if let Err(e) = self.finish(s, model, track) {
                    self.abandon(s, track)?;
                    return Err(e);
                }
            }
            if force {
                *self.gate(track) = None;
            }
        }
        Ok(())
    }
    pub fn abandon(&mut self, s: &Store, track: &str) -> Result<()> {
        *self.gate(track) = None;
        if let Some(x) = self.slot(track).take() {
            let tx = s.db.unchecked_transaction()?;
            for id in x.ids {
                tx.execute(
                    "UPDATE sources SET status='pending' WHERE id=?1 AND status='streaming'",
                    [id],
                )?;
            }
            tx.execute("UPDATE observations SET kind='preview_abandoned',details=json_set(details,'$.replay_required',1) WHERE id=?1 AND kind='preview'",[x.row])?;
            tx.commit()?;
        }
        Ok(())
    }
}
fn release_source(
    s: &Store,
    inbox: &crate::live_audio::Shared,
    id: i64,
    reason: &str,
) -> Result<()> {
    let tx = s.db.unchecked_transaction()?;
    let path: String = tx.query_row("SELECT path FROM sources WHERE id=?1", [id], |r| r.get(0))?;
    tx.execute("UPDATE sources SET status=CASE WHEN path LIKE 'memory:%' THEN 'discarded' ELSE 'done' END,error='' WHERE id=?1",[id])?;
    tx.execute("UPDATE source_metadata SET details=json_set(details,'$.speech_gate',?2) WHERE source_id=?1",params![id,reason])?;
    tx.commit()?;
    if path.starts_with("memory:") {
        inbox.lock().unwrap().remove(&path);
    }
    Ok(())
}
fn report(
    s: &Store,
    x: &Segment,
    track: &str,
    stage: &str,
    compute_ms: f64,
    has_result: bool,
) -> Result<()> {
    let captured =
        s.db.query_row(
            "SELECT details FROM source_metadata WHERE source_id=?1",
            [x.ids.last().copied().unwrap_or(0)],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok());
    let end = captured
        .as_ref()
        .and_then(|v| v["end"].as_f64())
        .unwrap_or(x.end as f64);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs_f64();
    let latency = if has_result && now >= end {
        Some((now - end) * 1000.0)
    } else {
        None
    };
    let v = json!({"stage":stage,"track":track,"at":now,"frames_in_phrase":x.frames,"source_ids":x.ids,"source_processing_ms":compute_ms,"source_end_to_result_ms":latency,"timing_estimated":captured.as_ref().is_none_or(|v|v["timing_estimated"]!=false),"latency_basis":"last source audio-end to result availability; not per-word speech latency; excludes accuracy claims"});
    s.state("inference_metrics", &v.to_string())?;
    if stage != "buffering" {
        s.state(&format!("speech_{stage}_metrics"), &v.to_string())?;
    }
    Ok(())
}
fn details(ids: &[i64], final_: bool, streaming: bool) -> serde_json::Value {
    json!({"source_ids":ids,"final":final_,"incremental_encoder":streaming,"speechworks_revision":REVISION,"speech_gate":"Speechworks Silero VAD; 1 s preroll; 0.75 s quiet endpoint; 20 s safety cap","timing":"capture-source intervals; no word alignment or diarization"})
}
fn source_day(at: i64) -> Option<chrono::NaiveDate> {
    chrono::DateTime::from_timestamp(at, 0).map(|t| t.with_timezone(&chrono::Local).date_naive())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silent_source_release_is_explicit_and_original_hallucinations_are_recoverable() {
        let root =
            std::env::temp_dir().join(format!("daybook-silent-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let inbox: crate::live_audio::Shared = Default::default();
        for (i, peak) in [(1, Some(-120.0)), (2, Some(-32.0)), (3, None)] {
            let path = format!("memory:23-{i}");
            s.admit_audio(&json!({"path":path,"track":"remote","at":100,"end":101,"frames":8000,"session":"fixture","level":{"peak_dbfs":peak}})).unwrap();
            let id = s.db.last_insert_rowid();
            inbox
                .lock()
                .unwrap()
                .insert(&path, vec![0; 32000], 120)
                .unwrap();
            s.observation(
                100,
                101,
                "transcript",
                "remote",
                "Original output preserved",
                &json!({"source_ids":[id]}),
                Some(id),
            )
            .unwrap();
            release_source(&s, &inbox, id, "no_speech").unwrap();
        }
        assert_eq!(s.pending(), 0);
        assert_eq!(inbox.lock().unwrap().stats()["pcm_bytes"], 0);
        assert_eq!(s.suppress_proven_silence().unwrap(), 1);
        assert_eq!(s.suppress_proven_silence().unwrap(), 0);
        let (text, details): (String, String) =
            s.db.query_row(
                "SELECT text,details FROM observations WHERE kind='transcript_suppressed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, "Original output preserved");
        assert!(details.contains("suppressed_reason"));
        assert_eq!(
            s.db.query_row(
                "SELECT count(*) FROM observations WHERE kind='transcript'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
    }
    #[test]
    #[ignore = "requires explicitly supplied local public model/audio fixtures"]
    fn real_model_streams_interleave_flush_and_recover() {
        let model_path = std::env::var("DAYBOOK_STREAM_MODEL").expect("local model fixture");
        let audio_path = std::env::var("DAYBOOK_STREAM_PCM").expect("local public PCM fixture");
        let raw = std::fs::read(audio_path).unwrap();
        let pcm: Vec<f32> = raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let model = pollster::block_on(Model::from_bytes_with(
            std::fs::read(model_path).unwrap(),
            speechworks_inference::LoadOptions {
                backend: if std::env::var("DAYBOOK_STREAM_BACKEND").as_deref() == Ok("auto") {
                    speechworks_inference::Backend::Auto
                } else {
                    speechworks_inference::Backend::Cpu
                },
                max_audio_samples: PHRASE_FRAMES,
                max_work_bytes: 256 * 1024 * 1024,
            },
        ))
        .unwrap();
        assert!(model.is_streaming());
        eprintln!(
            "ACTUAL_DEVICE {:?}; FALLBACK {:?}",
            model.device().unwrap(),
            model.fallback_reason()
        );
        let root =
            std::env::temp_dir().join(format!("daybook-stream-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root).unwrap();
        let mut streams = Streams::default();
        let base = crate::store::now();
        let mut durations = Vec::new();
        let expected: Vec<_> = pcm
            .chunks(PHRASE_FRAMES)
            .map(|x| pollster::block_on(model.transcribe(x)).unwrap().text)
            .collect();
        for (i, chunk) in pcm.chunks(16000).enumerate() {
            for track in ["me", "remote"] {
                let path = format!("audio/{track}-{i}.f32");
                s.add_source(base + i as i64, base + i as i64 + 1, track, &path)
                    .unwrap();
                let id = s.db.last_insert_rowid();
                let started = Instant::now();
                streams
                    .feed(
                        &s,
                        &model,
                        id,
                        base + i as i64,
                        base + i as i64 + 1,
                        track,
                        "fixture",
                        chunk,
                    )
                    .unwrap();
                durations.push(started.elapsed().as_secs_f64() * 1000.0);
                if let Some(x) = streams.slot(track) {
                    assert!(x.frames <= PHRASE_FRAMES);
                    assert!(x.ids.len() <= MAX_SOURCES);
                }
            }
            if i == 1 {
                assert_eq!(
                    s.db.query_row(
                        "SELECT count(*) FROM observations WHERE kind='preview'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                    2
                );
                assert!(s.prepare_job("incremental").unwrap().is_none());
            }
        }
        let flush_started = Instant::now();
        streams.flush(&s, &model, true).unwrap();
        let flush_ms = flush_started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(s.pending(), 0);
        for track in ["me", "remote"] {
            let mut q=s.db.prepare("SELECT text FROM observations WHERE kind='transcript' AND source=?1 ORDER BY id").unwrap();
            let actual: Vec<String> = q
                .query_map([track], |r| r.get(0))
                .unwrap()
                .map(|v| v.unwrap())
                .collect();
            assert_eq!(actual, expected);
        }
        durations.sort_by(f64::total_cmp);
        let reference = std::env::var("DAYBOOK_STREAM_REFERENCE").unwrap_or_default();
        let tokenize = |v: &str| {
            v.to_lowercase()
                .split_whitespace()
                .map(|w| {
                    w.chars()
                        .filter(|c| c.is_alphanumeric() || *c == '%')
                        .collect::<String>()
                })
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
        };
        let wanted = tokenize(&reference);
        let actual = tokenize(&expected.join(" "));
        let mut distance: Vec<usize> = (0..=actual.len()).collect();
        for (i, a) in wanted.iter().enumerate() {
            let mut diagonal = distance[0];
            distance[0] = i + 1;
            for (j, b) in actual.iter().enumerate() {
                let prior = distance[j + 1];
                distance[j + 1] = (diagonal + usize::from(a != b))
                    .min(distance[j] + 1)
                    .min(prior + 1);
                diagonal = prior;
            }
        }
        eprintln!(
            "BENCHMARK {}",
            json!({"audio_seconds":pcm.len() as f64/16000.0,"tracks":2,"updates":durations.len(),"warm_update_median_ms":durations[durations.len()/2],"warm_update_p95_ms":durations[(durations.len()*95/100).min(durations.len()-1)],"two_track_final_flush_ms":flush_ms,"normalized_word_errors":if wanted.is_empty(){None}else{Some(*distance.last().unwrap())},"reference_words":wanted.len(),"hypothesis":expected.join(" "),"scope":"public prerecorded fixture; model already loaded; chunk processing includes SQLite; not live speech latency or general accuracy"})
        );
        // Crash/replay boundary: provisional text is never treated as final.
        s.add_source(base + 30, base + 31, "me", "audio/replay.f32")
            .unwrap();
        let id = s.db.last_insert_rowid();
        streams
            .feed(
                &s,
                &model,
                id,
                base + 30,
                base + 31,
                "me",
                "replay",
                &pcm[..16000],
            )
            .unwrap();
        drop(streams);
        s.recover().unwrap();
        let mut streams = Streams::default();
        streams
            .feed(
                &s,
                &model,
                id,
                base + 30,
                base + 31,
                "me",
                "replay",
                &pcm[..16000],
            )
            .unwrap();
        streams.flush(&s, &model, true).unwrap();
        assert_eq!(
            s.db.query_row(
                "SELECT count(*) FROM observations WHERE kind='transcript' AND source_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(s.pending(), 0);
        assert!(streams.me.is_none() && streams.remote.is_none());
        for (n, session) in [(0, "before-stop"), (1, "after-stop")] {
            s.add_source(
                base + 40 + n,
                base + 41 + n,
                "me",
                &format!("audio/session-{n}.f32"),
            )
            .unwrap();
            let id = s.db.last_insert_rowid();
            streams
                .feed(
                    &s,
                    &model,
                    id,
                    base + 40 + n,
                    base + 41 + n,
                    "me",
                    session,
                    &pcm[..16000],
                )
                .unwrap();
        }
        assert_eq!(
            streams.me.as_ref().unwrap().ids.len(),
            1,
            "capture epoch change finalizes the old phrase even without a timestamp gap"
        );
        streams.flush(&s, &model, true).unwrap();
        eprintln!(
            "PASS: interleaved tracks match one-shot bounded phrases; previews excluded; force flush and crash replay finalize once; fixture {} frames",
            pcm.len()
        );
    }
}
