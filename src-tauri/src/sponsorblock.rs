//! SponsorBlock (sponsor.ajay.app): skip the parts of a music video that aren't music — the talking
//! intro, the skit in the middle, the credits and the "subscribe" outro. Only `music_offtopic`,
//! the category the project keeps for exactly this ("Music: Non-Music Section"); sponsor reads and
//! the rest belong to ordinary videos, not to songs.
//!
//! Asked by hash prefix: the first four hex digits of sha256(videoId) name a bucket of videos, the
//! server returns the segments for the whole bucket, and ours is picked out here. So the service
//! learns which bucket a track fell in, never the track.
//!
//! A segment is skipped once per play, and only when playback runs into it. Seeking into one is
//! going there on purpose, so it plays; seeking back over one that was skipped plays it too. That
//! is also what the toast's undo relies on.
//!
//! Behind the `sponsorblock` setting (on unless set to "false"). Local files have no videoId and
//! are never asked about.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tauri::Emitter;

use crate::state::AppState;

const API: &str = "https://sponsor.ajay.app/api/skipSegments";
/// Shorter than this is not worth a seek: the jump is more jarring than the second it saves.
const MIN_SEGMENT_SECS: f64 = 1.0;
/// Position moving further than this between two ticks is a seek, not playback.
const JUMP_SECS: f64 = 2.0;
/// A segment made against a cut this much longer or shorter than the one playing is for another
/// upload of the video (a re-edit, a region cut), and its times would land in the wrong places.
const DURATION_SLACK_SECS: f64 = 3.0;
/// An outro skip stops this far short of the end, so the track ends the ordinary way — gapless
/// advance, repeat-one — instead of seeking into the end of the file.
const END_GUARD_SECS: f64 = 0.3;
/// Segments per video kept for the session: a replay or a repeat asks nothing.
const CACHE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    start: f64,
    end: f64,
    /// The length of the upload the segment was drawn on, 0 when the submitter's client didn't say.
    video_duration: f64,
}

#[derive(Default)]
struct Current {
    video_id: String,
    segments: Vec<Segment>,
    /// Per segment: skipped already, or entered by a seek — either way, not to be skipped now.
    done: Vec<bool>,
    last_pos: Option<f64>,
    duration: f64,
}

#[derive(Default)]
pub struct SponsorBlock {
    current: Mutex<Current>,
    cache: Mutex<HashMap<String, Vec<Segment>>>,
}

fn enabled(state: &AppState) -> bool {
    state.db.get_setting("sponsorblock").as_deref() != Some("false")
}

/// A new track is playing (fresh play or gapless advance): forget the last one's segments and look
/// up this one's.
pub fn track_changed(state: &Arc<AppState>, video_id: &str) {
    {
        let mut c = state.sponsorblock.current.lock().unwrap();
        *c = Current { video_id: video_id.to_owned(), ..Default::default() };
    }
    if let Some(segments) = cached(state, video_id) {
        install(state, video_id, segments);
        return;
    }
    let state = state.clone();
    let video_id = video_id.to_owned();
    tauri::async_runtime::spawn(async move {
        if let Some(segments) = lookup(&state, &video_id).await {
            install(&state, &video_id, segments);
        }
    });
}

/// The video's non-music sections as `(start, end)` seconds, for the lyrics: a lyric sung inside
/// one is timed to some other cut of the song (lyrics.rs). Shares the skipper's cache, so the track
/// that is about to play is asked about once for both. Empty when switched off, for a local file,
/// or when the service can't be reached — "no evidence", never an error.
pub async fn non_music(state: &AppState, video_id: &str) -> Vec<(f64, f64)> {
    let segments = match cached(state, video_id) {
        Some(s) => s,
        None => lookup(state, video_id).await.unwrap_or_default(),
    };
    segments.into_iter().map(|s| (s.start, s.end)).collect()
}

fn cached(state: &AppState, video_id: &str) -> Option<Vec<Segment>> {
    state.sponsorblock.cache.lock().unwrap().get(video_id).cloned()
}

/// Ask the service, cache the answer. `None` when not asked (off, local file) or it failed.
async fn lookup(state: &AppState, video_id: &str) -> Option<Vec<Segment>> {
    if !enabled(state) || crate::local::is_local_song(video_id) {
        return None;
    }
    match fetch(video_id).await {
        Ok(segments) => {
            let mut cache = state.sponsorblock.cache.lock().unwrap();
            if cache.len() >= CACHE_LIMIT {
                cache.clear(); // ponytail: crude, but a session rarely plays 64 MVs
            }
            cache.insert(video_id.to_owned(), segments.clone());
            Some(segments)
        }
        // Offline, or the service is down: the video plays whole, which is what it did before.
        Err(e) => {
            tracing::debug!(error = %e, video_id, "sponsorblock: lookup failed");
            None
        }
    }
}

fn install(state: &AppState, video_id: &str, segments: Vec<Segment>) {
    let mut c = state.sponsorblock.current.lock().unwrap();
    if c.video_id != video_id {
        return; // the track changed while the lookup was out
    }
    if !segments.is_empty() {
        tracing::debug!(video_id, count = segments.len(), "sponsorblock: segments");
    }
    c.done = vec![false; segments.len()];
    c.segments = segments;
}

/// mpv's length for the current track, which segments are checked against.
pub fn on_duration(state: &AppState, secs: f64) {
    state.sponsorblock.current.lock().unwrap().duration = secs;
}

/// A position tick. Cheap when the track has no segments: one lock, one empty check.
pub fn on_position(state: &Arc<AppState>, pos: f64) {
    if !pos.is_finite() {
        return;
    }
    let skip = step(&mut state.sponsorblock.current.lock().unwrap(), pos);
    let Some((segment, to)) = skip else { return };
    tracing::info!(from = pos, to, "sponsorblock: skipping a non-music section");
    if let Err(e) = state.player.seek(to) {
        tracing::warn!(error = %e, "sponsorblock: seek failed");
        return;
    }
    let _ =
        state.app.emit("sponsor-skipped", serde_json::json!({ "start": segment.start, "end": to }));
}

/// The decision for one tick: where to seek, if anywhere. Every segment is decided once — skipped
/// when playback runs into it, left alone when a seek lands in it — and never again that play.
fn step(c: &mut Current, pos: f64) -> Option<(Segment, f64)> {
    let jumped = c.last_pos.is_some_and(|last| (pos - last).abs() > JUMP_SECS);
    c.last_pos = Some(pos);
    let duration = c.duration;
    for i in 0..c.segments.len() {
        let s = c.segments[i];
        if c.done[i] || pos < s.start || pos >= s.end - 0.5 {
            continue;
        }
        c.done[i] = true;
        if jumped || !fits(&s, duration) {
            return None; // landed here by a seek, or drawn on another cut: leave it
        }
        let to = if duration > 0.0 { s.end.min(duration - END_GUARD_SECS) } else { s.end };
        if to <= pos {
            return None;
        }
        // Our own seek is not the user's: the next tick lands far from this one.
        c.last_pos = Some(to);
        return Some((s, to));
    }
    None
}

/// Whether a segment belongs to the cut that is playing. Unknown on either side passes: most
/// segments carry no length at all, and the check is there to catch the ones that say they were
/// drawn on a different upload.
fn fits(s: &Segment, duration: f64) -> bool {
    s.video_duration <= 0.0
        || duration <= 0.0
        || (s.video_duration - duration).abs() <= DURATION_SLACK_SECS
}

async fn fetch(video_id: &str) -> Result<Vec<Segment>, reqwest::Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Video {
        #[serde(rename = "videoID")]
        video_id: String,
        #[serde(default)]
        segments: Vec<Raw>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        segment: [f64; 2],
        #[serde(default)]
        video_duration: f64,
    }
    let digest = Sha256::digest(video_id.as_bytes());
    let prefix: String = digest.iter().take(2).map(|b| format!("{b:02x}")).collect();
    let resp = crate::http::client()
        .get(format!("{API}/{prefix}"))
        .query(&[("categories", r#"["music_offtopic"]"#), ("actionTypes", r#"["skip"]"#)])
        .timeout(Duration::from_secs(8))
        .send()
        .await?;
    // 404 is the service's "nothing in this bucket".
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let videos: Vec<Video> = resp.error_for_status()?.json().await?;
    Ok(segments_for(videos.into_iter().map(|v| (v.video_id, v.segments)), video_id, |r| {
        (r.segment[0], r.segment[1], r.video_duration)
    }))
}

/// Our video's usable segments out of a bucket, in playback order.
fn segments_for<R>(
    bucket: impl Iterator<Item = (String, Vec<R>)>,
    video_id: &str,
    fields: impl Fn(&R) -> (f64, f64, f64),
) -> Vec<Segment> {
    let mut out: Vec<Segment> = bucket
        .filter(|(id, _)| id == video_id)
        .flat_map(|(_, segs)| segs)
        .map(|r| {
            let (start, end, video_duration) = fields(&r);
            Segment { start, end, video_duration }
        })
        .filter(|s| s.start.is_finite() && s.end.is_finite() && s.end - s.start >= MIN_SEGMENT_SECS)
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64) -> Segment {
        Segment { start, end, video_duration: 0.0 }
    }

    /// The bucket holds other videos' segments; only ours, long enough to be worth it, in order.
    #[test]
    fn picks_our_segments_out_of_the_bucket() {
        let bucket = vec![
            ("other".to_owned(), vec![(0.0, 30.0, 0.0)]),
            ("ours".to_owned(), vec![(258.2, 262.7, 0.0), (0.0, 12.1, 262.7), (100.0, 100.4, 0.0)]),
        ];
        let got = segments_for(bucket.into_iter(), "ours", |r| *r);
        assert_eq!(got.len(), 2, "the 0.4 s one is not worth a seek");
        assert_eq!((got[0].start, got[0].end), (0.0, 12.1));
        assert_eq!(got[1].start, 258.2);
    }

    fn playing(segments: Vec<Segment>, duration: f64) -> Current {
        Current {
            video_id: "v".into(),
            done: vec![false; segments.len()],
            segments,
            last_pos: None,
            duration,
        }
    }

    /// Ticks the way mpv sends them: a step at a time, so no two are further apart than a seek.
    fn play(c: &mut Current, from: f64, to: f64) -> Vec<f64> {
        let mut seeks = Vec::new();
        let mut p = from;
        while p < to {
            if let Some((_, target)) = step(c, p) {
                seeks.push(target);
                p = target;
            }
            p += 0.25;
        }
        seeks
    }

    #[test]
    fn an_intro_is_skipped_once_as_playback_runs_into_it() {
        let mut c = playing(vec![seg(0.0, 12.1)], 262.7);
        assert_eq!(play(&mut c, 0.0, 20.0), [12.1]);
        // Back to the start by hand (the toast's undo): it plays this time.
        assert_eq!(step(&mut c, 0.0), None);
        assert_eq!(play(&mut c, 0.25, 20.0), Vec::<f64>::new());
    }

    #[test]
    fn a_seek_into_a_segment_is_left_to_play() {
        let mut c = playing(vec![seg(100.0, 110.0)], 262.7);
        assert!(play(&mut c, 50.0, 51.0).is_empty());
        assert_eq!(step(&mut c, 105.0), None, "the user went there");
        assert!(play(&mut c, 105.25, 115.0).is_empty(), "and is not thrown out of it a tick later");
    }

    #[test]
    fn segments_that_arrive_late_still_catch_the_intro() {
        let mut c = playing(Vec::new(), 262.7);
        assert!(play(&mut c, 0.0, 3.0).is_empty());
        c.segments = vec![seg(0.0, 12.1)];
        c.done = vec![false];
        assert_eq!(play(&mut c, 3.0, 20.0), [12.1], "no seek happened, playback ran on into it");
    }

    #[test]
    fn an_outro_stops_short_of_the_end_so_the_track_ends_normally() {
        let mut c = playing(vec![seg(258.2, 262.7)], 262.7);
        let seeks = play(&mut c, 257.0, 262.0);
        assert_eq!(seeks.len(), 1);
        assert!((seeks[0] - (262.7 - END_GUARD_SECS)).abs() < 1e-9);
    }

    #[test]
    fn a_segment_for_another_cut_is_not_skipped() {
        let s = Segment { start: 0.0, end: 12.0, video_duration: 262.7 };
        let mut c = playing(vec![s], 192.0);
        assert!(play(&mut c, 0.0, 20.0).is_empty());
    }

    /// The live service, by prefix, for a music video with a known intro and outro. Pins the
    /// response shape and the filtering against the real bucket.
    ///   cargo test -p limusic-app sponsorblock_is_alive -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "hits the live SponsorBlock API"]
    async fn sponsorblock_is_alive() {
        let got = fetch("knW7-x7Y7RE").await.expect("lookup");
        eprintln!("segments: {got:?}");
        assert!(
            got.iter().any(|s| s.start < 1.0 && s.end > 10.0),
            "the intro is gone from the answer"
        );
        assert!(
            fetch("7yGIITDXQTU").await.expect("lookup").is_empty(),
            "a video with none has none"
        );
    }

    #[test]
    fn a_segment_drawn_on_another_cut_does_not_fit() {
        let s = Segment { start: 0.0, end: 12.0, video_duration: 262.7 };
        assert!(fits(&s, 263.0));
        assert!(!fits(&s, 192.0), "ten seconds off is another upload");
        assert!(fits(&seg(0.0, 12.0), 192.0), "no length on the segment: nothing to check");
        assert!(fits(&s, 0.0), "no length from mpv yet: nothing to check");
    }
}
