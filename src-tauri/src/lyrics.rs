//! Lyrics fetching. Provider chain (plan `graceful-kindling`):
//!
//! 0. **SimpMusic Lyrics** (`api-lyrics.simpmusic.org`) → looked up by the YouTube videoId itself,
//!    so there is no title/artist/length guessing at all, and most entries are word-synced. First
//!    for both reasons, and behind the `lyrics_simpmusic` setting for the same reason Boidu is:
//!    first means it sees every track played.
//! 1. **Boidu** (`lyrics-api.boidu.dev`) → word-level timings, which nothing else here returns and
//!    the karaoke sweep needs. First because of that, and behind the `lyrics_boidu` setting
//!    because first also means it sees every track played.
//! 2. **LRCLIB** `/api/get` (exact match) → synced LRC lyrics. Free, no key, best coverage —
//!    what Metrolist defaults to.
//! 3. **YouTube Music timed** — `next(videoId)` → lyrics browseId → mobile-client browse
//!    (`timedLyricsData`). The same real-time lyrics the YTM app shows.
//! 4. **Netease / QQ / Kugou** → synced LRC, plus translations from Netease. Search hits are
//!    matched on length (`best_by_duration`); these catalogues rank remixes next to originals.
//! 5. Plain fallbacks: LRCLIB fuzzy search → LRCLIB plain (from step 2's response) → YT plain
//!    (WEB_REMIX browse) → the fuzzy search's plain text.
//!
//! Results are cached in SQLite (`lyrics_cache`): hits forever, "no lyrics" verdicts for 24h.
//! A run where every provider merely *errored* (offline) caches nothing, so lyrics come back
//! when the network does. Everything is best-effort — a lyrics failure is never a user error.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// How long a cached "no lyrics found" verdict suppresses refetching.
const MISS_TTL_SECS: i64 = 24 * 3600;

const LRCLIB_ROOT: &str = "https://lrclib.net/api";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// One display line. `time_ms` present ⇔ the line is synced (a plain-lyrics response has none).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time_ms: Option<u64>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<LyricWord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<String>,
}

impl LyricLine {
    pub fn simple(time_ms: Option<u64>, text: String) -> Self {
        Self { time_ms, end_time_ms: None, text, words: None, translation: None }
    }
}

/// What the UI gets (and what `lyrics_cache` stores as JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lyrics {
    /// Attribution shown in the panel footer ("LRCLIB", "Musixmatch", …).
    pub source: String,
    pub synced: bool,
    #[serde(default)]
    pub instrumental: bool,
    pub lines: Vec<LyricLine>,
}

pub struct LyricsRequest {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    /// Track length in seconds (mpv's), tightens LRCLIB matching. `None`/0 when unknown yet.
    pub duration: Option<f64>,
}

/// `LIMUSIC_LYRICS_ONLY=<boidu|netease|qq|kugou>` pins the chain to that one provider and bypasses
/// the cache both ways. The last three sit below Boidu, LRCLIB and YouTube Music, so on a normal
/// catalogue nothing ever reaches them and they cannot be exercised by just playing tracks.
///
/// Unset (the default) leaves the chain exactly as it ships. Testing aid, not a user setting.
fn forced_provider() -> Option<String> {
    std::env::var("LIMUSIC_LYRICS_ONLY").ok().filter(|s| !s.is_empty())
}

/// Cache-through entry point for the `get_lyrics` command.
pub async fn get_lyrics(state: &AppState, req: LyricsRequest) -> Option<Lyrics> {
    let now = now_secs();
    let video_id = req.video_id.clone();
    let forced = forced_provider();
    if forced.is_none() {
        if let Some(cached) = state.db.get_lyrics(&video_id, now, MISS_TTL_SECS) {
            return cached.and_then(|json| serde_json::from_str(&json).ok());
        }
    }
    let (lyrics, cacheable) = fetch(state, req).await;
    if cacheable && forced.is_none() {
        let json = lyrics.as_ref().and_then(|l| serde_json::to_string(l).ok());
        state.db.put_lyrics(&video_id, json.as_deref(), now);
    }
    lyrics
}

/// Run the provider chain. Second value: cache the outcome — true only when the track's duration
/// was known (LRCLIB matching is loose without it and lands on wrong *cuts* of the song, lyrics
/// seconds off the audio) AND some provider answered definitively (found / not-found) rather
/// than merely erroring (offline must not poison the cache with a 24h "no lyrics").
async fn fetch(state: &AppState, mut req: LyricsRequest) -> (Option<Lyrics>, bool) {
    let mut definitive = false;

    // 0. `next()` up front: it carries the lyrics browseId AND — via its seed item — the exact
    //    length of the cut this videoId plays. The queue item often has no duration (card plays;
    //    stream-cache replays skip /player entirely), and duration is what keeps LRCLIB from
    //    matching a differently-timed cut, so resolve it here where it's always available.
    //    A local file has no videoId to ask about — its duration came off the file itself, and
    //    YouTube has no lyrics browseId for it. Skip straight to LRCLIB (title + artist), which is
    //    the only provider that can answer for it anyway.
    let next = if crate::local::is_local_song(&req.video_id) {
        None
    } else {
        match state
            .it
            .next(state.clients.get(innertube::METADATA_CLIENT).unwrap(), Some(&req.video_id), None)
            .await
        {
            Ok(n) => Some(n),
            Err(e) => {
                tracing::debug!(error = %e, "lyrics: next() failed");
                None
            }
        }
    };
    let browse_id = next.as_ref().and_then(|n| n.lyrics_browse_id.clone());
    if req.duration.is_none() {
        req.duration = next.as_ref().and_then(|n| {
            let item = n.items.iter().find(|i| i.video_id == req.video_id)?;
            duration_str_secs(item.duration.as_deref()?)
        });
    }
    let req = &req;

    // Pinned to one provider: run it alone and report whatever it says, hit or miss, so a silent
    // fallthrough to LRCLIB can't be mistaken for the pinned provider working. Sits below the
    // duration lookup above on purpose, so the match tightening gets exercised too.
    if let Some(only) = forced_provider() {
        let hit = match only.as_str() {
            "boidu" => boidu_get(req).await,
            "netease" => netease_get(req).await,
            "qq" => qqmusic_get(req).await,
            "kugou" => kugou_get(req).await,
            other => {
                tracing::warn!(provider = other, "LIMUSIC_LYRICS_ONLY: unknown provider");
                Ok(None)
            }
        };
        match &hit {
            Ok(Some(l)) => tracing::info!(provider = only, lines = l.lines.len(), "pinned: hit"),
            Ok(None) => tracing::info!(provider = only, "pinned: no lyrics"),
            Err(e) => tracing::warn!(provider = only, error = %e, "pinned: failed"),
        }
        return (hit.ok().flatten(), false);
    }

    // 0. SimpMusic Lyrics, keyed by the videoId: an exact answer for this very upload, where every
    //    provider below is matching a title and a length and can land on another cut. A local
    //    file has no videoId to look up.
    if !crate::local::is_local_song(&req.video_id)
        && state.db.get_setting("lyrics_simpmusic").as_deref() != Some("false")
    {
        if let Ok(Some(l)) = simpmusic_get(req).await {
            return (Some(l), true);
        }
    }

    // 1. Boidu, ahead of LRCLIB because it is the only provider here that returns word-level
    //    timings, and those are what the karaoke sweep renders. Going first also means it is the
    //    one provider that sees every track played rather than only the ones LRCLIB misses, so it
    //    is behind a setting. Off falls straight through to the chain as it was before.
    if state.db.get_setting("lyrics_boidu").as_deref() != Some("false") {
        if let Ok(Some(l)) = boidu_get(req).await {
            return (Some(l), req.duration.is_some());
        }
    }

    // 2. LRCLIB exact match.
    let lr = lrclib_get(req).await;
    if let Ok(hit) = &lr {
        definitive = true;
        if let Some(l) = hit.as_ref().and_then(lrclib_to_lyrics) {
            if l.synced || l.instrumental {
                return (Some(l), req.duration.is_some());
            }
        }
    }

    // 3. YouTube Music timed lyrics.
    if next.is_some() {
        definitive = true; // a next() answer with no lyrics tab IS "YT has no lyrics"
    }
    if let (Some(bid), Some(client)) =
        (&browse_id, state.clients.get(innertube::LYRICS_TIMED_CLIENT))
    {
        match state.it.lyrics_timed(client, bid).await {
            Ok(lines) if !lines.is_empty() => {
                return (
                    Some(Lyrics {
                        source: "YouTube Music".into(),
                        synced: true,
                        instrumental: false,
                        lines: lines
                            .into_iter()
                            .map(|l| LyricLine::simple(Some(l.time_ms), l.text))
                            .collect(),
                    }),
                    true,
                );
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "lyrics: timed browse failed"),
        }
    }

    // 4. Netease Cloud Music provider (synced + word timestamps + translations)
    if let Ok(Some(l)) = netease_get(req).await {
        return (Some(l), req.duration.is_some());
    }

    // 5. QQ Music provider
    if let Ok(Some(l)) = qqmusic_get(req).await {
        return (Some(l), req.duration.is_some());
    }

    // 6. Kugou provider
    if let Ok(Some(l)) = kugou_get(req).await {
        return (Some(l), req.duration.is_some());
    }

    // 3. LRCLIB fuzzy search — a synced fuzzy match still beats any plain text, so it outranks
    //    the plain tier below. (YT lyrics are region-licensed and can be entirely absent.)
    let searched = lrclib_search(req).await;
    if let Ok(hit) = &searched {
        definitive = true;
        if let Some(l) = hit.as_ref().and_then(lrclib_to_lyrics).filter(|l| l.synced) {
            return (Some(l), req.duration.is_some());
        }
    }

    // --- plain tier -------------------------------------------------------------------------

    // 4a. Plain from LRCLIB's exact match.
    if let Ok(Some(hit)) = &lr {
        if let Some(l) = plain_from_text(hit.plain_lyrics.as_deref(), "LRCLIB") {
            return (Some(l), req.duration.is_some());
        }
    }

    // 4b. Plain from YT (WEB_REMIX).
    if let Some(bid) = &browse_id {
        if let Some(client) = state.clients.get(innertube::METADATA_CLIENT) {
            match state.it.lyrics_plain(client, bid).await {
                Ok(Some(p)) => {
                    // Footer is YT's own attribution ("Source: Musixmatch") — surface it.
                    let source = p.footer.unwrap_or_else(|| "YouTube Music".into());
                    if let Some(l) = plain_from_text(Some(&p.text), &source) {
                        return (Some(l), true);
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::debug!(error = %e, "lyrics: plain browse failed"),
            }
        }
    }

    // 4c. Plain from the fuzzy search.
    if let Ok(Some(hit)) = &searched {
        if let Some(l) = lrclib_to_lyrics(hit) {
            return (Some(l), req.duration.is_some());
        }
    }

    (None, definitive)
}

// --- LRCLIB (https://lrclib.net/docs) -------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibTrack {
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
}

/// LRCLIB asks integrations to identify themselves via User-Agent.
const LRCLIB_UA: &str =
    concat!("Limusic v", env!("CARGO_PKG_VERSION"), " (https://github.com/duykhanhxx03/limusic)");

/// A GET to LRCLIB, carrying the two things this API wants from us: who we are, and a bound on how
/// long we will wait. Both used to be baked into a client of our own.
fn get(url: String) -> reqwest::RequestBuilder {
    crate::http::client().get(url).header("User-Agent", LRCLIB_UA).timeout(Duration::from_secs(15))
}

/// `/api/get`: exact signature match. `Ok(None)` = definitive "not in LRCLIB" (404);
/// `Err` = transport trouble (don't cache a negative off it).
async fn lrclib_get(req: &LyricsRequest) -> Result<Option<LrclibTrack>, reqwest::Error> {
    let mut q: Vec<(&str, String)> =
        vec![("track_name", req.title.clone()), ("artist_name", req.artists.clone())];
    if let Some(album) = &req.album {
        q.push(("album_name", album.clone()));
    }
    if let Some(d) = req.duration.filter(|d| *d > 0.0) {
        q.push(("duration", format!("{}", d.round() as i64)));
    }
    let resp = get(format!("{LRCLIB_ROOT}/get")).query(&q).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(resp.error_for_status()?.json().await?))
}

/// `/api/search`: fuzzy fallback. Prefers a synced candidate whose duration is within ±5s of
/// ours (when known); returns the best or `Ok(None)`.
async fn lrclib_search(req: &LyricsRequest) -> Result<Option<LrclibTrack>, reqwest::Error> {
    let q = [("track_name", req.title.as_str()), ("artist_name", req.artists.as_str())];
    let list: Vec<LrclibTrack> = get(format!("{LRCLIB_ROOT}/search"))
        .query(&q)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ours = req.duration.filter(|d| *d > 0.0);
    // Distance from our track's length; unknown-length candidates rank last but aren't excluded.
    let dist = |t: &LrclibTrack| match (ours, t.duration) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => f64::INFINITY,
    };
    let close = |t: &LrclibTrack| ours.is_none() || dist(t) <= 5.0;
    let synced = |t: &LrclibTrack| t.synced_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty());
    // Prefer the synced candidate whose duration is CLOSEST to ours — LRCLIB carries multiple
    // cuts of popular tracks, and a 4s-different cut plays lyrics 4s off the audio.
    let mut best_synced: Option<(f64, LrclibTrack)> = None;
    let mut best_plain: Option<LrclibTrack> = None;
    for t in list {
        if !close(&t) {
            continue;
        }
        if synced(&t) {
            let d = dist(&t);
            if best_synced.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best_synced = Some((d, t));
            }
        } else if best_plain.is_none() {
            best_plain = Some(t);
        }
    }
    Ok(best_synced.map(|(_, t)| t).or(best_plain))
}

/// Best `Lyrics` an LRCLIB track yields: instrumental > synced > plain > nothing.
fn lrclib_to_lyrics(t: &LrclibTrack) -> Option<Lyrics> {
    if t.instrumental {
        return Some(Lyrics {
            source: "LRCLIB".into(),
            synced: false,
            instrumental: true,
            lines: Vec::new(),
        });
    }
    if let Some(lrc) = t.synced_lyrics.as_deref().filter(|s| !s.trim().is_empty()) {
        let lines = parse_lrc(lrc);
        if !lines.is_empty() {
            return Some(Lyrics {
                source: "LRCLIB".into(),
                synced: true,
                instrumental: false,
                lines,
            });
        }
    }
    plain_from_text(t.plain_lyrics.as_deref(), "LRCLIB")
}

/// Plain text → un-timed lines (blank lines kept as stanza breaks).
fn plain_from_text(text: Option<&str>, source: &str) -> Option<Lyrics> {
    let text = text?.trim();
    if text.is_empty() {
        return None;
    }
    Some(Lyrics {
        source: source.to_owned(),
        synced: false,
        instrumental: false,
        lines: text.lines().map(|l| LyricLine::simple(None, l.trim_end().to_owned())).collect(),
    })
}

/// A provider's parsed lines as a result, or `None` when there was nothing to show.
///
/// `synced` is derived from the lines rather than asserted by the caller. TTML without `begin`
/// attributes, and JSON items carrying text but no time, both parse to real lines with no cue.
/// Declaring those synced puts the UI in its synced view, where no line ever highlights (none has
/// a cue to pass) and clicking one to seek does nothing: lyrics that look broken, rather than
/// lyrics that read as plain text.
fn from_parsed(source: &str, lines: Vec<LyricLine>) -> Option<Lyrics> {
    if lines.is_empty() {
        return None;
    }
    Some(Lyrics {
        source: source.to_owned(),
        // Any cue at all: an LRC with untimed credit or stanza lines is still a synced lyric.
        synced: lines.iter().any(|l| l.time_ms.is_some()),
        instrumental: false,
        lines,
    })
}

// --- LRC parsing ----------------------------------------------------------------------------

/// Parse LRC text (`[mm:ss.xx] line`) into sorted lines. Handles multiple timestamps per line
/// (`[t1][t2]text` — the line repeats at both cues) and skips metadata tags (`[ar:…]`).
/// Timestamped empty lines are kept: they're instrumental gaps the UI can show as such.
fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let mut out = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let Some(end) = after.find(']') else { break };
            match parse_lrc_time(&after[..end]) {
                Some(ms) => {
                    times.push(ms);
                    rest = after[end + 1..].trim_start();
                }
                // Not a timestamp: a metadata tag ([ar:…] — no times yet, line skipped) or
                // bracketed lyric text ("[Chorus]" — keep it as the line's text).
                None => break,
            }
        }
        for &ms in &times {
            out.push(LyricLine::simple(Some(ms), rest.to_owned()));
        }
    }
    out.sort_by_key(|l| l.time_ms);
    out
}

/// `mm:ss`, `mm:ss.xx`, or `mm:ss.xxx` → milliseconds.
fn parse_lrc_time(tag: &str) -> Option<u64> {
    let (m, rest) = tag.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let (s, frac) = match rest.split_once('.') {
        Some((s, f)) => (s, Some(f)),
        None => (rest, None),
    };
    let s: u64 = s.trim().parse().ok()?;
    let ms = match frac {
        Some(f) => {
            let digits: String = f.chars().filter(char::is_ascii_digit).take(3).collect();
            let val: u64 = digits.parse().ok()?;
            match digits.len() {
                1 => val * 100,
                2 => val * 10,
                _ => val,
            }
        }
        None => 0,
    };
    Some((m * 60 + s) * 1000 + ms)
}

/// `"3:21"` / `"1:02:03"` → seconds.
fn duration_str_secs(s: &str) -> Option<f64> {
    let mut total: u64 = 0;
    for part in s.split(':') {
        total = total * 60 + part.trim().parse::<u64>().ok()?;
    }
    (total > 0).then_some(total as f64)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- Additional Providers (minilyricsv2 & LyricsPlus) --------------------------------------

/// Boidu provider (boidu.dev / Better Lyrics API)
async fn boidu_get(req: &LyricsRequest) -> Result<Option<Lyrics>, reqwest::Error> {
    let mut q: Vec<(&str, String)> = vec![("s", req.title.clone()), ("a", req.artists.clone())];
    if let Some(album) = &req.album {
        q.push(("al", album.clone()));
    }
    if let Some(d) = req.duration.filter(|d| *d > 0.0) {
        q.push(("d", format!("{}", d.round() as i64)));
    }

    let url = "https://lyrics-api.boidu.dev/getLyrics";
    tracing::debug!(title = %req.title, artist = %req.artists, "lyrics: querying Boidu provider");
    let resp: serde_json::Value = match crate::http::client()
        .get(url)
        .query(&q)
        .header("User-Agent", LRCLIB_UA)
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(error = %e, "lyrics: Boidu json parse failed");
                return Ok(None);
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "lyrics: Boidu request failed");
            return Ok(None);
        }
    };

    let lrc_str = resp
        .get("ttml")
        .or_else(|| resp.get("syncedLyrics"))
        .or_else(|| resp.get("lyrics"))
        .or_else(|| resp.get("lrc"))
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    return Some(s.to_string());
                }
            }
            if v.is_array() {
                return serde_json::to_string(v).ok();
            }
            None
        });

    let hit = lrc_str.and_then(|lrc| from_parsed("Boidu", parse_lrc_or_ttml(&lrc)));
    match &hit {
        Some(l) => tracing::debug!(count = l.lines.len(), synced = l.synced, "lyrics: Boidu hit"),
        None => tracing::debug!("lyrics: Boidu returned no lines"),
    }
    Ok(hit)
}

/// How far a search hit's length may sit from the track we're actually playing. Same tolerance the
/// LRCLIB search above uses, for the same reason.
const MATCH_TOLERANCE_SECS: f64 = 5.0;

/// Pick the search hit closest in length to what we're playing, rejecting anything further off than
/// `MATCH_TOLERANCE_SECS`.
///
/// The three providers below rank remixes, live cuts and radio edits right next to the original
/// (Kugou's top hit for "Shape of You" is a 263s edit of a 233s song, and Netease ranks a 231s
/// remix second), so taking whatever came back first plays lyrics seconds out of step with the
/// audio. Closest-match rather than first-within-tolerance matters: the remix is often inside the
/// window too, and only the distance separates it from the real cut.
///
/// With no length on our side there is nothing to check, so the first hit stands. A candidate whose
/// own length is missing ranks last but is not dropped — if a provider renames the field we want
/// degraded matching, not a provider that silently returns nothing.
fn best_by_duration<T>(
    ours: Option<f64>,
    cands: &[T],
    secs: impl Fn(&T) -> Option<f64>,
) -> Option<&T> {
    let Some(ours) = ours.filter(|d| *d > 0.0) else {
        return cands.first();
    };
    cands
        .iter()
        .map(|c| (secs(c).map_or(f64::INFINITY, |d| (d - ours).abs()), c))
        .filter(|(d, _)| *d <= MATCH_TOLERANCE_SECS || d.is_infinite())
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| c)
}

/// Netease Cloud Music provider (supports LRC, word timestamps, & translations)
async fn netease_get(req: &LyricsRequest) -> Result<Option<Lyrics>, reqwest::Error> {
    let query = format!("{} {}", req.title, req.artists);
    // POST `/api/search/get`, not GET `/api/search/get/web`: the latter now answers with an
    // encrypted hex blob instead of JSON, which parsed to "no hit" and left this provider dead.
    let resp: serde_json::Value = match crate::http::client()
        .post("https://music.163.com/api/search/get")
        .form(&[("s", query.as_str()), ("type", "1"), ("limit", "5"), ("offset", "0")])
        .header("User-Agent", LRCLIB_UA)
        .header("Referer", "https://music.163.com/")
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(j) => j,
            Err(_) => return Ok(None),
        },
        Err(_) => return Ok(None),
    };

    let songs = resp
        .pointer("/result/songs")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or_default();
    // Netease reports track length in milliseconds.
    let hit =
        best_by_duration(req.duration, songs, |s| Some(s.get("duration")?.as_f64()? / 1000.0));
    let Some(id) = hit.and_then(|s| s.get("id")).and_then(|v| v.as_u64()) else {
        return Ok(None);
    };

    let lyric_url = format!("https://music.163.com/api/song/lyric?id={id}&lv=1&kv=1&tv=-1");
    let l_resp: serde_json::Value = match crate::http::client()
        .get(&lyric_url)
        .header("User-Agent", LRCLIB_UA)
        .header("Referer", "https://music.163.com/")
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(j) => j,
            Err(_) => return Ok(None),
        },
        Err(_) => return Ok(None),
    };

    let lrc_str = l_resp.pointer("/lrc/lyric").and_then(|v| v.as_str());
    let klyric_str = l_resp.pointer("/klyric/lyric").and_then(|v| v.as_str());
    let tlyric_str = l_resp.pointer("/tlyric/lyric").and_then(|v| v.as_str());

    if let Some(lrc) = lrc_str {
        let mut lines = parse_lrc_or_ttml(lrc);
        if let Some(klrc) = klyric_str {
            let klines = parse_lrc_or_ttml(klrc);
            lines = lrc_mux(lines, klines);
        }
        if let Some(tlrc) = tlyric_str {
            attach_translations(&mut lines, &parse_lrc(tlrc));
        }
        return Ok(from_parsed("Netease Cloud Music", lines));
    }
    Ok(None)
}

/// How far a translation line's cue may sit from the original line it belongs to.
const TRANSLATION_SLACK_MS: u64 = 1000;

/// Put each translation line under the original line it translates, or none at all.
///
/// Matched to the nearest unused original line within a second, not on an identical cue: Netease
/// often writes the translation's timestamps a few centiseconds off the lyric's, and an exact match
/// silently dropped those. But a translation file can also be out of step with the lyric it came
/// with, timed against another cut, and then the nearest line is the wrong line. So the whole set is
/// refused when more than a quarter of its lines find no partner: a translation under the wrong
/// line reads as broken, and none reads as a song that simply has no translation. SimpMusic applies
/// the same 1 s / 25% rule before it trusts a translation.
fn attach_translations(lines: &mut [LyricLine], translations: &[LyricLine]) -> bool {
    let wanted: Vec<(u64, &str)> = translations
        .iter()
        .filter_map(|t| Some((t.time_ms?, t.text.trim())))
        .filter(|(_, text)| !text.is_empty())
        .collect();
    if wanted.is_empty() {
        return false;
    }
    let mut taken = vec![false; lines.len()];
    let mut pairs = Vec::with_capacity(wanted.len());
    for &(t, text) in &wanted {
        let best = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| !taken[*i] && !l.text.trim().is_empty())
            .filter_map(|(i, l)| Some((i, l.time_ms?.abs_diff(t))))
            .filter(|(_, d)| *d <= TRANSLATION_SLACK_MS)
            .min_by_key(|(_, d)| *d);
        if let Some((i, _)) = best {
            taken[i] = true;
            pairs.push((i, text));
        }
    }
    let missed = wanted.len() - pairs.len();
    if missed * 4 > wanted.len() {
        tracing::debug!(missed, of = wanted.len(), "lyrics: translation out of step, dropped");
        return false;
    }
    for (i, text) in pairs {
        lines[i].translation = Some(text.to_owned());
    }
    true
}

// --- SimpMusic Lyrics (https://github.com/maxrave-dev/lyrics) -------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimpMusicEntry {
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    rich_sync_lyrics: Option<String>,
    #[serde(default)]
    plain_lyric: Option<String>,
    #[serde(default)]
    duration_seconds: Option<f64>,
    #[serde(default)]
    vote: i64,
}

/// `GET /v1/{videoId}`. A 404 is a definitive "no entry"; everything else that isn't lyrics is
/// transport trouble, reported as `Err` so it isn't cached as a miss.
///
/// The database is crowd-sourced, so an entry is checked before it is shown: its length has to be
/// within tolerance of the track (when both are known), and among several entries for one video the
/// most up-voted wins.
async fn simpmusic_get(req: &LyricsRequest) -> Result<Option<Lyrics>, reqwest::Error> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        data: Vec<SimpMusicEntry>,
    }
    let url = format!("https://api-lyrics.simpmusic.org/v1/{}", urlencoding::encode(&req.video_id));
    let resp = crate::http::client()
        .get(url)
        .header("User-Agent", LRCLIB_UA)
        .timeout(Duration::from_secs(8))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body: Resp = resp.error_for_status()?.json().await?;
    let ours = req.duration.filter(|d| *d > 0.0);
    let hit = body
        .data
        .into_iter()
        .filter(|e| match (ours, e.duration_seconds.filter(|d| *d > 0.0)) {
            (Some(a), Some(b)) => (a - b).abs() <= MATCH_TOLERANCE_SECS,
            _ => true,
        })
        .max_by_key(|e| e.vote);
    Ok(hit.and_then(|e| simpmusic_to_lyrics(&e)))
}

/// Word-synced over line-synced over plain, whichever the entry actually carries.
fn simpmusic_to_lyrics(e: &SimpMusicEntry) -> Option<Lyrics> {
    const SOURCE: &str = "SimpMusic Lyrics";
    let present =
        |s: &Option<String>| s.as_deref().filter(|s| !s.trim().is_empty()).map(str::to_owned);
    if let Some(rich) = present(&e.rich_sync_lyrics) {
        if let Some(l) = from_parsed(SOURCE, clean_lines(parse_rich_sync(&rich))) {
            return Some(l);
        }
    }
    if let Some(lrc) = present(&e.synced_lyrics) {
        let lines = parse_lrc(&decode_entities(&lrc));
        if let Some(l) = from_parsed(SOURCE, clean_lines(lines)) {
            return Some(l);
        }
    }
    plain_from_text(present(&e.plain_lyric).map(|p| decode_entities(&p)).as_deref(), SOURCE)
}

/// SimpMusic's word-synced format, one line per lyric line:
///
/// ```text
/// [00:07.12] <00:07.12>Baby, <00:08.22>you <00:08.74>can <00:10.44>lights
/// [00:09.544]v1:<00:09.544>If <00:09.768>you <00:11.711>
/// ```
///
/// Each `<t>` is when the word *after* it starts; a trailing `<t>` with no word is when the last one
/// ends. That is the opposite of how `parse_elrc` reads a tag (as the end of the text before it),
/// so it gets its own parser rather than a flag on that one. `v1:` is a singer marker and is
/// dropped. A last word with no closing tag ends where the next line starts.
fn parse_rich_sync(text: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let Some(line) = parse_lrc(raw).into_iter().next() else { continue };
        let Some(start) = line.time_ms else { continue };
        let body = strip_voice_marker(&line.text);
        let mut words: Vec<LyricWord> = Vec::new();
        let mut plain = String::new();
        let mut rest = body;
        let mut pending: Option<u64> = None;
        let mut closed_at: Option<u64> = None;
        loop {
            let tag = rest.find('<').and_then(|open| {
                let close = rest[open..].find('>')? + open;
                Some((open, close, parse_lrc_time(&rest[open + 1..close])?))
            });
            let (text_end, next) = match tag {
                Some((open, close, t)) => (open, Some((close, t))),
                None => (rest.len(), None),
            };
            // Entities are decoded per chunk, after the tags are split off: decoding the whole line
            // first would turn a `&lt;` in the lyric into a `<` that reads as the start of a tag.
            let chunk = decode_entities(&rest[..text_end]);
            if !chunk.is_empty() {
                plain.push_str(&chunk);
                match pending.take() {
                    Some(at) => words.push(LyricWord { text: chunk, start_ms: at, end_ms: at }),
                    // Text ahead of the first tag belongs to the line's own cue.
                    None if words.is_empty() && !chunk.trim().is_empty() => {
                        words.push(LyricWord { text: chunk, start_ms: start, end_ms: start })
                    }
                    None => {
                        if let Some(w) = words.last_mut() {
                            w.text.push_str(&chunk);
                        }
                    }
                }
            }
            let Some((close, t)) = next else { break };
            // A word's end is the next word's start.
            if let Some(w) = words.last_mut() {
                if w.end_ms <= w.start_ms {
                    w.end_ms = t.max(w.start_ms);
                }
            }
            if rest[close + 1..].trim().is_empty() {
                closed_at = Some(t);
            }
            pending = Some(t);
            rest = &rest[close + 1..];
        }
        let text = plain.trim().to_owned();
        let has_words = words.len() > 1 || closed_at.is_some();
        lines.push(LyricLine {
            time_ms: Some(start),
            end_time_ms: closed_at,
            text,
            words: has_words.then_some(words),
            translation: None,
        });
    }
    lines.sort_by_key(|l| l.time_ms);
    // Words still open at the end of their line run to the next line's cue, or a beat past their
    // start at the end of the song.
    let next_starts: Vec<Option<u64>> =
        (0..lines.len()).map(|i| lines.get(i + 1).and_then(|n| n.time_ms)).collect();
    for (line, next) in lines.iter_mut().zip(next_starts) {
        if let Some(words) = &mut line.words {
            if let Some(last) = words.last_mut() {
                if last.end_ms <= last.start_ms {
                    last.end_ms =
                        next.filter(|n| *n > last.start_ms).unwrap_or(last.start_ms + 600);
                }
            }
        }
    }
    lines
}

/// `v1:` / `v2:` ahead of a line's first tag: which singer has the line. Nothing here draws that.
fn strip_voice_marker(s: &str) -> &str {
    let t = s.trim_start();
    match t.split_once(':') {
        Some((v, rest))
            if v.len() <= 3 && v.starts_with('v') && v[1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            rest
        }
        _ => s,
    }
}

/// Community-synced lyrics carry the syncer's signature as a lyric line ("Synced by Noob.exe" at
/// 00:00). It isn't part of the song, and as the first line it would be what the view opens on.
fn clean_lines(lines: Vec<LyricLine>) -> Vec<LyricLine> {
    lines.into_iter().filter(|l| !is_credit_line(&l.text)).collect()
}

fn is_credit_line(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    ["synced by", "sync by", "lyrics synced by", "timed by", "transcribed by", "lyrics by"]
        .iter()
        .any(|p| t.starts_with(p))
}

/// The HTML entities community lyrics arrive with (`don&#x27;t`, `&amp;`). Unknown entities are
/// left as written.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let decoded = after.find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = &after[1..end];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((ch, end))
        });
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// QQ Music provider
async fn qqmusic_get(req: &LyricsRequest) -> Result<Option<Lyrics>, reqwest::Error> {
    let query = format!("{} {}", req.title, req.artists);
    let search_url = format!(
        "https://c.y.qq.com/soso/fcgi-bin/client_search_cp?w={}&format=json",
        urlencoding::encode(&query)
    );
    let resp: serde_json::Value = match crate::http::client()
        .get(&search_url)
        .header("User-Agent", LRCLIB_UA)
        .header("Referer", "https://y.qq.com/")
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(j) => j,
            Err(_) => return Ok(None),
        },
        Err(_) => return Ok(None),
    };

    let songs = resp
        .pointer("/data/song/list")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or_default();
    // QQ reports track length in whole seconds, as `interval`.
    let hit = best_by_duration(req.duration, songs, |s| s.get("interval")?.as_f64());
    let Some(mid) = hit.and_then(|s| s.get("songmid")).and_then(|v| v.as_str()) else {
        return Ok(None);
    };

    let lyric_url = format!(
        "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg?songmid={mid}&format=json&nobase64=1"
    );
    let l_resp: serde_json::Value = match crate::http::client()
        .get(&lyric_url)
        .header("User-Agent", LRCLIB_UA)
        .header("Referer", "https://y.qq.com/")
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(j) => j,
            Err(_) => return Ok(None),
        },
        Err(_) => return Ok(None),
    };

    let mut lyric_raw = l_resp.get("lyric").and_then(|v| v.as_str()).unwrap_or("");
    let decoded;
    if let Ok(bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, lyric_raw)
    {
        if let Ok(s) = String::from_utf8(bytes) {
            decoded = s;
            lyric_raw = &decoded;
        }
    }
    Ok(from_parsed("QQ Music", parse_lrc_or_ttml(lyric_raw)))
}

/// Kugou provider
async fn kugou_get(req: &LyricsRequest) -> Result<Option<Lyrics>, reqwest::Error> {
    let query = format!("{} {}", req.title, req.artists);
    let search_url = format!(
        "https://songsearch.kugou.com/song_search_v2?keyword={}&page=1&pagesize=5",
        urlencoding::encode(&query)
    );
    let resp: serde_json::Value =
        match crate::http::client().get(&search_url).timeout(Duration::from_secs(8)).send().await {
            Ok(r) => match r.json().await {
                Ok(j) => j,
                Err(_) => return Ok(None),
            },
            Err(_) => return Ok(None),
        };

    let songs = resp
        .pointer("/data/lists")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or_default();
    // Kugou reports track length in whole seconds, as `Duration`.
    let hit = best_by_duration(req.duration, songs, |s| s.get("Duration")?.as_f64());
    let Some(h) = hit.and_then(|s| s.get("FileHash")).and_then(|v| v.as_str()) else {
        return Ok(None);
    };

    // `hash=`, not `h=`: the latter is not a parameter this endpoint knows, so it answered
    // "paramter_error: empty hash and keyword" for every track and the provider never returned
    // anything at all.
    let krc_url = format!("https://krcs.kugou.com/search?ver=1&man=yes&client=mobi&hash={h}");
    let krc_resp: serde_json::Value =
        match crate::http::client().get(&krc_url).timeout(Duration::from_secs(8)).send().await {
            Ok(r) => match r.json().await {
                Ok(j) => j,
                Err(_) => return Ok(None),
            },
            Err(_) => return Ok(None),
        };

    let id = krc_resp.pointer("/candidates/0/id").and_then(|v| v.as_str());
    let accesskey = krc_resp.pointer("/candidates/0/accesskey").and_then(|v| v.as_str());
    let (Some(id_str), Some(key_str)) = (id, accesskey) else {
        return Ok(None);
    };

    let dl_url = format!(
        "https://lyrics.kugou.com/download?ver=1&client=pc&id={id_str}&accesskey={key_str}&fmt=lrc"
    );
    let dl_resp: serde_json::Value =
        match crate::http::client().get(&dl_url).timeout(Duration::from_secs(8)).send().await {
            Ok(r) => match r.json().await {
                Ok(j) => j,
                Err(_) => return Ok(None),
            },
            Err(_) => return Ok(None),
        };

    let b64_content = dl_resp.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if let Ok(bytes) =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64_content)
    {
        if let Ok(lrc_str) = String::from_utf8(bytes) {
            return Ok(from_parsed("Kugou", parse_lrc_or_ttml(&lrc_str)));
        }
    }
    Ok(None)
}

// --- TTML / AAML / eLRC Parsing & LRCMux ----------------------------------------------------

fn parse_time_val(v: &serde_json::Value) -> Option<u64> {
    if let Some(f) = v.as_f64() {
        if f < 500.0 {
            Some((f * 1000.0) as u64)
        } else {
            Some(f as u64)
        }
    } else if let Some(u) = v.as_u64() {
        if u < 500 {
            Some(u * 1000)
        } else {
            Some(u)
        }
    } else if let Some(s) = v.as_str() {
        if let Ok(f) = s.parse::<f64>() {
            if f < 500.0 {
                Some((f * 1000.0) as u64)
            } else {
                Some(f as u64)
            }
        } else {
            None
        }
    } else {
        None
    }
}

fn parse_lrc_or_ttml(text: &str) -> Vec<LyricLine> {
    let trimmed = text.trim();

    // 1. JSON Array / KPOE / LyricsPlus format
    if (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && (trimmed.contains("\"text\"")
            || trimmed.contains("\"time\"")
            || trimmed.contains("\"words\"")
            || trimmed.contains("\"start\"")
            || trimmed.contains("\"startTime\""))
    {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let mut out = Vec::new();
            let arr_opt = val
                .as_array()
                .or_else(|| val.get("lyrics").and_then(|v| v.as_array()))
                .or_else(|| val.get("lines").and_then(|v| v.as_array()))
                .or_else(|| val.get("element").and_then(|v| v.as_array()));
            if let Some(arr) = arr_opt {
                for item in arr {
                    let line_text = item
                        .get("text")
                        .or_else(|| item.get("words"))
                        .or_else(|| item.get("line"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let time_val = item
                        .get("time")
                        .or_else(|| item.get("startTime"))
                        .or_else(|| item.get("start"))
                        .or_else(|| item.get("t"))
                        .and_then(parse_time_val);

                    // Parse inner word array if present
                    let mut words = Vec::new();
                    if let Some(w_arr) = item.get("words").and_then(|v| v.as_array()) {
                        for w in w_arr {
                            let w_text = w
                                .get("text")
                                .or_else(|| w.get("word"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let w_start = w
                                .get("startTime")
                                .or_else(|| w.get("start"))
                                .or_else(|| w.get("time"))
                                .and_then(parse_time_val)
                                .or(time_val);
                            let w_end = w
                                .get("endTime")
                                .or_else(|| w.get("end"))
                                .and_then(parse_time_val)
                                .or_else(|| w_start.map(|s| s + 500));
                            if let (Some(b), Some(e)) = (w_start, w_end) {
                                words.push(LyricWord { text: w_text, start_ms: b, end_ms: e });
                            }
                        }
                    }

                    if !line_text.is_empty() || time_val.is_some() {
                        out.push(LyricLine {
                            time_ms: time_val,
                            end_time_ms: None,
                            text: line_text,
                            words: if !words.is_empty() { Some(words) } else { None },
                            translation: None,
                        });
                    }
                }
                if !out.is_empty() {
                    out.sort_by_key(|l| l.time_ms);
                    return out;
                }
            }
        }
    }

    // 2. TTML / AAML XML
    if trimmed.starts_with('<') || trimmed.contains("<p ") || trimmed.contains("<tt") {
        let ttml_lines = parse_ttml_aaml(trimmed);
        if !ttml_lines.is_empty() {
            return ttml_lines;
        }
    }

    // 3. LRC / eLRC
    parse_elrc(text)
}

/// TTML and Apple Music AAML XML parser
fn parse_ttml_aaml(xml: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    let mut pos = 0;
    while let Some(p_start) = xml[pos..].find("<p") {
        let abs_p_start = pos + p_start;
        let Some(p_tag_end) = xml[abs_p_start..].find('>') else {
            break;
        };
        let abs_p_tag_end = abs_p_start + p_tag_end;
        let p_tag_str = &xml[abs_p_start..abs_p_tag_end + 1];

        let Some(p_close) = xml[abs_p_tag_end..].find("</p>") else {
            break;
        };
        let abs_p_close = abs_p_tag_end + p_close;
        let inner_str = &xml[abs_p_tag_end + 1..abs_p_close];

        pos = abs_p_close + 4;

        let line_begin = parse_xml_attr(p_tag_str, "begin").and_then(|s| parse_ttml_time(&s));
        let line_end = parse_xml_attr(p_tag_str, "end").and_then(|s| parse_ttml_time(&s));

        let mut words: Vec<LyricWord> = Vec::new();
        let mut span_pos = 0;
        let mut plain_text_buf = String::new();

        while let Some(s_start) = inner_str[span_pos..].find("<span") {
            let abs_s_start = span_pos + s_start;
            let Some(s_tag_end) = inner_str[abs_s_start..].find('>') else {
                break;
            };
            let abs_s_tag_end = abs_s_start + s_tag_end;
            let s_tag_str = &inner_str[abs_s_start..abs_s_tag_end + 1];

            let before = strip_xml_tags(&inner_str[span_pos..abs_s_start]);
            if !before.is_empty() {
                plain_text_buf.push_str(&before);
                if let Some(last_w) = words.last_mut() {
                    last_w.text.push_str(&before);
                }
            }

            let Some(s_close) = inner_str[abs_s_tag_end..].find("</span>") else {
                break;
            };
            let abs_s_close = abs_s_tag_end + s_close;
            let w_text = strip_xml_tags(&inner_str[abs_s_tag_end + 1..abs_s_close]);

            let w_begin =
                parse_xml_attr(s_tag_str, "begin").and_then(|s| parse_ttml_time(&s)).or(line_begin);
            let w_end =
                parse_xml_attr(s_tag_str, "end").and_then(|s| parse_ttml_time(&s)).or(line_end);

            if let (Some(b), Some(e)) = (w_begin, w_end) {
                if !w_text.is_empty() {
                    words.push(LyricWord { text: w_text.clone(), start_ms: b, end_ms: e });
                }
            }
            plain_text_buf.push_str(&w_text);
            span_pos = abs_s_close + 7;
        }

        if span_pos < inner_str.len() {
            plain_text_buf.push_str(&strip_xml_tags(&inner_str[span_pos..]));
        }

        let words_opt = if !words.is_empty() { Some(words) } else { None };
        let full_text = plain_text_buf.trim().to_string();
        if !full_text.is_empty() || line_begin.is_some() {
            lines.push(LyricLine {
                time_ms: line_begin,
                end_time_ms: line_end,
                text: full_text,
                words: words_opt,
                translation: None,
            });
        }
    }
    lines.sort_by_key(|l| l.time_ms);
    lines
}

/// Enhanced LRC parser (line timestamps + word inline timestamp tags)
fn parse_elrc(lrc: &str) -> Vec<LyricLine> {
    let mut base_lines = parse_lrc(lrc);
    for line in &mut base_lines {
        if line.text.contains('<') || line.text.contains('(') {
            let mut words = Vec::new();
            let mut text_buf = String::new();
            let mut last_ms = line.time_ms.unwrap_or(0);

            let mut pos = 0;
            let text_bytes = line.text.as_bytes();
            while pos < text_bytes.len() {
                if text_bytes[pos] == b'<' {
                    if let Some(end_idx) = line.text[pos..].find('>') {
                        let tag = &line.text[pos + 1..pos + end_idx];
                        if let Some(w_ms) = parse_lrc_time(tag) {
                            pos += end_idx + 1;
                            let next_tag_idx = line.text[pos..]
                                .find('<')
                                .map(|i| pos + i)
                                .unwrap_or(line.text.len());
                            let w_str = &line.text[pos..next_tag_idx];
                            text_buf.push_str(w_str);
                            words.push(LyricWord {
                                text: w_str.to_string(),
                                start_ms: last_ms,
                                end_ms: w_ms,
                            });
                            last_ms = w_ms;
                            pos = next_tag_idx;
                            continue;
                        }
                    }
                }
                // `pos` is a BYTE offset, so step by the character's own width. Indexing it as a
                // char offset silently mangles every non-ASCII line (and the CJK providers below
                // are where word timings mostly come from). Every other jump above lands on an
                // ASCII `<`/`>`, so slicing here is always on a char boundary.
                let ch = line.text[pos..].chars().next().unwrap_or(' ');
                text_buf.push(ch);
                pos += ch.len_utf8();
            }

            if !words.is_empty() {
                line.text = text_buf.trim().to_string();
                line.words = Some(words);
            }
        }
    }
    base_lines
}

/// LRCMux multiplexer: merges line lyrics with word timing or translations
fn lrc_mux(mut primary: Vec<LyricLine>, word_source: Vec<LyricLine>) -> Vec<LyricLine> {
    if word_source.is_empty() {
        return primary;
    }
    for p in &mut primary {
        let Some(p_time) = p.time_ms else {
            continue;
        };
        let best = word_source.iter().find(|ws| {
            if let Some(ws_time) = ws.time_ms {
                (p_time as i64 - ws_time as i64).abs() <= 800
            } else {
                false
            }
        });
        if let Some(ws) = best {
            if p.words.is_none() && ws.words.is_some() {
                p.words = ws.words.clone();
            }
            if p.translation.is_none() && ws.translation.is_some() {
                p.translation = ws.translation.clone();
            }
            if p.end_time_ms.is_none() && ws.end_time_ms.is_some() {
                p.end_time_ms = ws.end_time_ms;
            }
        }
    }
    primary
}

fn parse_ttml_time(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix("ms") {
        return rest.parse::<u64>().ok();
    }
    if let Some(rest) = s.strip_suffix('s') {
        let secs: f64 = rest.parse().ok()?;
        return Some((secs * 1000.0) as u64);
    }
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 3 {
            let h: u64 = parts[0].parse().ok()?;
            let m: u64 = parts[1].parse().ok()?;
            let secs: f64 = parts[2].parse().ok()?;
            return Some((h * 3600 + m * 60) * 1000 + (secs * 1000.0) as u64);
        } else if parts.len() == 2 {
            let m: u64 = parts[0].parse().ok()?;
            let secs: f64 = parts[1].parse().ok()?;
            return Some(m * 60 * 1000 + (secs * 1000.0) as u64);
        }
    }
    let secs: f64 = s.parse().ok()?;
    Some((secs * 1000.0) as u64)
}

fn parse_xml_attr(tag: &str, attr: &str) -> Option<String> {
    let pattern = format!("{attr}=\"");
    if let Some(idx) = tag.find(&pattern) {
        let start = idx + pattern.len();
        let end = tag[start..].find('"')?;
        return Some(tag[start..start + end].to_string());
    }
    let pattern_single = format!("{attr}='");
    if let Some(idx) = tag.find(&pattern_single) {
        let start = idx + pattern_single.len();
        let end = tag[start..].find('\'')?;
        return Some(tag[start..start + end].to_string());
    }
    None
}

fn strip_xml_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_lrc() {
        let lrc = "[ar:Fleetwood Mac]\n[00:27.93] Listen to the wind blow\n[00:31.16] Watch the sun rise\n";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_ms, Some(27930));
        assert_eq!(lines[0].text, "Listen to the wind blow");
        assert_eq!(lines[1].time_ms, Some(31160));
    }

    #[test]
    fn multi_timestamp_line_repeats() {
        let lines = parse_lrc("[00:10.00][01:10.00]la la la");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_ms, Some(10000));
        assert_eq!(lines[1].time_ms, Some(70000));
        assert!(lines.iter().all(|l| l.text == "la la la"));
    }

    #[test]
    fn keeps_bracketed_lyric_text_and_gap_lines() {
        let lines = parse_lrc("[00:05.5][Chorus] yeah\n[00:20.123]\n[00:30] plain seconds");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].time_ms, Some(5500));
        assert_eq!(lines[0].text, "[Chorus] yeah");
        assert_eq!(lines[1].time_ms, Some(20123));
        assert_eq!(lines[1].text, "");
        assert_eq!(lines[2].time_ms, Some(30000));
    }

    #[test]
    fn plain_text_splits_lines() {
        let l = plain_from_text(Some("one\ntwo\n\nthree"), "LRCLIB").unwrap();
        assert!(!l.synced);
        assert_eq!(l.lines.len(), 4);
        assert_eq!(l.lines[2].text, "");
    }

    #[test]
    fn parses_ttml_aaml_word_timestamps() {
        let xml = r#"<tt><p begin="00:10.500" end="00:14.200"><span begin="00:10.500" end="00:11.200">Hello </span><span begin="00:11.200" end="00:12.100">world </span></p></tt>"#;
        let lines = parse_ttml_aaml(xml);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].time_ms, Some(10500));
        assert_eq!(lines[0].end_time_ms, Some(14200));
        assert_eq!(lines[0].text, "Hello world");
        let words = lines[0].words.as_ref().unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello ");
        assert_eq!(words[0].start_ms, 10500);
        assert_eq!(words[0].end_ms, 11200);
    }

    #[test]
    fn parses_elrc_inline_word_timestamps() {
        let lrc = "[00:10.50]<00:10.50>Hello <00:11.20>world";
        let lines = parse_elrc(lrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].time_ms, Some(10500));
        assert_eq!(lines[0].text, "Hello world");
        let words = lines[0].words.as_ref().unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello ");
        assert_eq!(words[1].text, "world");
    }

    #[test]
    fn elrc_keeps_non_ascii_text_intact() {
        // Text before the first word tag goes through the char-by-char path, which used to walk
        // byte offsets as if they were char offsets and shredded anything multi-byte.
        let lines = parse_elrc("[00:12.00]私は<00:12.50>歌う");
        assert_eq!(lines[0].text, "私は歌う");
        let words = lines[0].words.as_ref().unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "歌う");
        assert_eq!(words[0].end_ms, 12500);
    }

    #[test]
    fn from_parsed_derives_synced_from_the_lines() {
        // TTML with no `begin` parses to real lines carrying no cue. Declaring those synced is
        // what put the UI in a synced view whose highlight could never move.
        let untimed = parse_lrc_or_ttml("<tt><body><div><p>no timing here</p></div></body></tt>");
        assert!(!untimed.is_empty());
        assert!(!from_parsed("X", untimed).unwrap().synced);

        // Same for a JSON payload whose items carry text but no time.
        let json = parse_lrc_or_ttml(r#"[{"text":"one"},{"text":"two"}]"#);
        assert!(!json.is_empty());
        assert!(!from_parsed("X", json).unwrap().synced);

        let timed = parse_lrc_or_ttml("[00:01.00]one\n[00:02.00]two");
        assert!(from_parsed("X", timed).unwrap().synced);

        assert!(from_parsed("X", Vec::new()).is_none());
    }

    /// Real Kugou/Netease search shapes: the original is not first, and a remix sits inside the
    /// tolerance window, so only closest-match picks the right cut.
    #[test]
    fn best_by_duration_skips_remixes_and_wrong_cuts() {
        let secs = |t: &(f64, &str)| Some(t.0);
        // Kugou's actual top hit for "Shape of You" is a 263s edit of a 233s song.
        let kugou = [(263.0, "wrong cut"), (251.0, "dj edit"), (233.0, "original")];
        assert_eq!(best_by_duration(Some(233.0), &kugou, secs).unwrap().1, "original");
        // Netease ranks a 231s remix second; both are within 5s, distance breaks the tie.
        let netease = [(233.7, "original"), (231.2, "stormzy remix")];
        assert_eq!(best_by_duration(Some(233.0), &netease, secs).unwrap().1, "original");
        // Nothing close enough beats a wrong answer.
        assert!(best_by_duration(Some(233.0), &kugou[..2], secs).is_none());
        // No length on our side: nothing to check, first hit stands.
        assert_eq!(best_by_duration(None, &kugou, secs).unwrap().1, "wrong cut");
        // A hit with no length of its own still gets used rather than silently dropped.
        let unknown = [(0.0, "no duration")];
        let none = |_: &(f64, &str)| None;
        assert_eq!(best_by_duration(Some(233.0), &unknown, none).unwrap().1, "no duration");
    }

    #[test]
    fn simpmusic_rich_sync_times_each_word_from_its_own_tag() {
        let rich = "[00:00.00] Synced by Noob.exe\n\
            [00:07.12] <00:07.12>Baby, <00:08.22>don&#x27;t <00:08.74>lights\n\
            [00:09.544]v1:<00:09.544>If <00:09.768>you <00:11.711>\n\
            [00:12.00] \n\
            [00:13.00] <00:13.00>a &lt;b&gt; <00:13.50>c";
        let entry = SimpMusicEntry {
            synced_lyrics: None,
            rich_sync_lyrics: Some(rich.into()),
            plain_lyric: None,
            duration_seconds: Some(200.0),
            vote: 0,
        };
        let l = simpmusic_to_lyrics(&entry).unwrap();
        assert!(l.synced);
        // The syncer's signature is gone; the empty gap line stays.
        assert_eq!(l.lines.len(), 4);

        let first = &l.lines[0];
        assert_eq!(first.text, "Baby, don't lights");
        let w = first.words.as_ref().unwrap();
        assert_eq!(w.len(), 3);
        // Each word starts at its own tag and ends where the next one starts.
        assert_eq!((w[0].text.as_str(), w[0].start_ms, w[0].end_ms), ("Baby, ", 7120, 8220));
        assert_eq!((w[1].text.as_str(), w[1].start_ms, w[1].end_ms), ("don't ", 8220, 8740));
        // The last word has no closing tag, so it runs to the next line's cue.
        assert_eq!((w[2].start_ms, w[2].end_ms), (8740, 9544));

        let voiced = &l.lines[1];
        assert_eq!(voiced.text, "If you");
        let w = voiced.words.as_ref().unwrap();
        assert_eq!((w[1].start_ms, w[1].end_ms), (9768, 11711));
        assert_eq!(voiced.end_time_ms, Some(11711));

        assert_eq!(l.lines[2].text, "");
        // An entity that decodes to `<` is lyric text, not a tag.
        assert_eq!(l.lines[3].text, "a <b> c");
    }

    #[test]
    fn simpmusic_falls_back_from_word_to_line_to_plain() {
        let mut e = SimpMusicEntry {
            synced_lyrics: Some("[00:01.00] one\n[00:02.00] two &amp; three".into()),
            rich_sync_lyrics: Some("   ".into()),
            plain_lyric: Some("plain".into()),
            duration_seconds: None,
            vote: 0,
        };
        let l = simpmusic_to_lyrics(&e).unwrap();
        assert!(l.synced && l.lines[1].words.is_none());
        assert_eq!(l.lines[1].text, "two & three");
        e.synced_lyrics = None;
        let l = simpmusic_to_lyrics(&e).unwrap();
        assert!(!l.synced);
        assert_eq!(l.source, "SimpMusic Lyrics");
    }

    #[test]
    fn helpers_for_community_lyrics() {
        assert_eq!(strip_voice_marker("v1:<00:01.00>hi"), "<00:01.00>hi");
        assert_eq!(strip_voice_marker("<00:01.00>v1: no"), "<00:01.00>v1: no");
        assert_eq!(strip_voice_marker("Verse: words"), "Verse: words");
        assert!(is_credit_line("  Lyrics synced by someone"));
        assert!(!is_credit_line("Synchronized hearts"));
        assert_eq!(
            decode_entities("a &#39;b&#x27; &unknown; & c &#128512;"),
            "a 'b' &unknown; & c 😀"
        );
    }

    #[test]
    fn translations_attach_to_the_nearest_line_or_not_at_all() {
        let lines = || {
            vec![
                LyricLine::simple(Some(1000), "one".into()),
                LyricLine::simple(Some(5000), "two".into()),
                LyricLine::simple(Some(9000), "three".into()),
                LyricLine::simple(Some(13000), "four".into()),
            ]
        };
        // A few centiseconds off each cue still pairs, where an exact match found nothing.
        let mut l = lines();
        let close = parse_lrc("[00:01.04]un\n[00:04.97]deux\n[00:09.02]trois\n[00:13.30]quatre");
        assert!(attach_translations(&mut l, &close));
        assert_eq!(l[1].translation.as_deref(), Some("deux"));
        assert_eq!(l[3].translation.as_deref(), Some("quatre"));

        // One stray line out of four is tolerated (exactly 25%)...
        let mut l = lines();
        let one_off = parse_lrc("[00:01.00]un\n[00:05.00]deux\n[00:09.00]trois\n[00:20.00]quatre");
        assert!(attach_translations(&mut l, &one_off));
        assert!(l[3].translation.is_none());

        // ...but a translation timed against another cut is refused whole.
        let mut l = lines();
        let shifted = parse_lrc("[00:03.00]un\n[00:07.00]deux\n[00:11.00]trois\n[00:15.00]quatre");
        assert!(!attach_translations(&mut l, &shifted));
        assert!(l.iter().all(|x| x.translation.is_none()));
    }

    #[test]
    fn lrc_mux_combines_lines_and_word_sources() {
        let primary = vec![LyricLine::simple(Some(10000), "Hello world".into())];
        let word_source = vec![LyricLine {
            time_ms: Some(10100),
            end_time_ms: Some(14000),
            text: "Hello world".into(),
            words: Some(vec![LyricWord { text: "Hello ".into(), start_ms: 10100, end_ms: 12000 }]),
            translation: Some("Halo dunia".into()),
        }];
        let muxed = lrc_mux(primary, word_source);
        assert_eq!(muxed.len(), 1);
        assert!(muxed[0].words.is_some());
        assert_eq!(muxed[0].translation.as_deref(), Some("Halo dunia"));
    }

    /// Are the external providers still alive? Hits all four for real, so it is NOT in the default
    /// run (context/17: network tests are opt-in, or `cargo test` fails offline):
    ///   cargo test -p limusic-app --lib -- --ignored --nocapture
    ///
    /// This exists because a provider that is *broken* and a provider that simply *has no lyrics
    /// for this track* both return `Ok(None)`, and nothing else in the chain can tell them apart:
    /// each one just falls through to the next. Netease and Kugou both shipped in PR #13 querying
    /// endpoints that answered an error for every track, and stayed unnoticed for exactly that
    /// reason. Run this after touching a provider, and whenever lyrics quietly get worse.
    ///
    /// **Read the output, don't just trust the pass.** It fails only when *every* provider is
    /// silent, because a single "no hit" is not proof of breakage: these are third-party
    /// catalogues, they drop tracks, and Kugou in particular throttles by IP and answers
    /// `total: 0` to everything for a while rather than returning an error. A provider that is
    /// genuinely dead prints "no hit" on every track you try, run after run.
    #[tokio::test]
    #[ignore = "hits four live lyrics APIs"]
    async fn providers_are_alive() {
        let req = LyricsRequest {
            video_id: "test".into(),
            title: "Shape of You".into(),
            artists: "Ed Sheeran".into(),
            album: None,
            duration: Some(233.0),
        };
        let mut alive = 0;
        for (name, hit) in [
            ("Boidu", boidu_get(&req).await),
            ("Netease Cloud Music", netease_get(&req).await),
            ("QQ Music", qqmusic_get(&req).await),
            ("Kugou", kugou_get(&req).await),
        ] {
            match hit {
                Ok(Some(l)) => {
                    println!("{name}: {} lines, synced={}", l.lines.len(), l.synced);
                    assert_eq!(l.source, name);
                    assert!(!l.lines.is_empty());
                    alive += 1;
                }
                // Transport errors never reach here: the providers collapse them into Ok(None),
                // which is exactly why a dead one is invisible in normal use.
                Ok(None) => println!("{name}: NO HIT"),
                Err(e) => println!("{name}: ERROR {e}"),
            }
        }
        assert!(alive > 0, "no lyrics from any provider (offline?)");

        // Boidu is the only provider carrying per-word timings, and the karaoke sweep renders
        // nothing without them. Checked here rather than in its own test: a second live test runs
        // concurrently with this one, and the added latency alone was enough to trip another
        // provider's 8s timeout and fail the run.
        let boidu = boidu_get(&req).await.unwrap().expect("Boidu hit");
        assert!(boidu.lines.iter().any(|l| l.words.is_some()));

        // SimpMusic Lyrics looks up by videoId, so it needs a real one: Dua Lipa, "Levitating".
        let sm = LyricsRequest { video_id: "OsfAnsMY21M".into(), duration: Some(203.0), ..req };
        let sm = simpmusic_get(&sm).await.unwrap().expect("SimpMusic Lyrics hit");
        println!("SimpMusic Lyrics: {} lines, synced={}", sm.lines.len(), sm.synced);
        assert!(sm.lines.iter().any(|l| l.words.is_some()));
    }
}
