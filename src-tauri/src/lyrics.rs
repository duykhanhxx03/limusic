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
//!    matched on title, then length (`best_match`); these catalogues rank remixes next to
//!    originals, and a wide search returns unrelated songs of exactly the right length.
//! 5. Plain fallbacks: LRCLIB fuzzy search → LRCLIB plain (from step 2's response) → YT plain
//!    (WEB_REMIX browse) → the fuzzy search's plain text.
//!
//! Results are cached in SQLite (`lyrics_cache`): hits forever, "no lyrics" verdicts for 24h.
//! A run where every provider merely *errored* (offline) caches nothing, so lyrics come back
//! when the network does. Everything is best-effort — a lyrics failure is never a user error.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

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
    /// A music video rather than the audio track, when something has already said which. Lyrics
    /// are mostly timed on the audio release, which the video runs longer than (`on_this_video`).
    pub is_video: Option<bool>,
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
        if let Ok(Some(mut l)) = simpmusic_get(state, req).await {
            // An entry can set a word's syllables down as words of their own, and only a text that
            // spells the song out tells the two apart (`join_syllables`). YouTube's own lyrics for
            // the track are one, from the service that is playing it anyway.
            if let (Some(bid), Some(client)) =
                (&browse_id, state.clients.get(innertube::METADATA_CLIENT))
            {
                if let Ok(Some(p)) = state.it.lyrics_plain(client, bid).await {
                    join_syllables(&mut l.lines, &p.text);
                }
            }
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
    /// The search endpoint's hits carry it; checked there, like every other catalogue search.
    #[serde(default)]
    track_name: Option<String>,
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
    let q = [("track_name", req.title.as_str()), ("artist_name", lead_artist(&req.artists))];
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
        // Fuzzy means fuzzy: the right length is not enough on its own (see `best_match`).
        if !close(&t) || !same_title(&req.title, t.track_name.as_deref().unwrap_or("")) {
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
fn from_parsed(source: &str, mut lines: Vec<LyricLine>) -> Option<Lyrics> {
    if lines.is_empty() {
        return None;
    }
    for line in &mut lines {
        if let Some(words) = line.words.as_mut() {
            tidy_words(words);
        }
        if line.words.as_ref().is_some_and(Vec::is_empty) {
            line.words = None;
        }
        // The line's own text, for everything that shows a line rather than its words (the mini
        // player, a copied lyric): the same spaced sources left runs of spaces in it too.
        if line.text.contains("  ") {
            line.text = line.text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    Some(Lyrics {
        source: source.to_owned(),
        // Any cue at all: an LRC with untimed credit or stanza lines is still a synced lyric.
        synced: lines.iter().any(|l| l.time_ms.is_some()),
        instrumental: false,
        lines,
    })
}

/// One space between two words, however the source spelled it, and none where it had none.
///
/// Word-timed sources disagree about where the space goes: after the word (`<t>Một <t>người`), on
/// both sides of it (`<t> Uhm, <t>`), or as a run of spaces between two words with a timestamp of
/// its own (`<t>   <t>`). Both renderers draw a word's text as it stands and add their own gap
/// after a word that ends in a space — so a leading space was measured into the word, a
/// spaces-only "word" became a gap of its own, and a line read as if every word were set apart.
/// Here each word loses its surrounding whitespace, a whitespace-only word goes, and a word keeps a
/// single trailing space when anything separated it from the next one. CJK, which has no spaces
/// between its words, stays joined.
fn tidy_words(words: &mut Vec<LyricWord>) {
    let mut out: Vec<LyricWord> = Vec::with_capacity(words.len());
    // Whitespace seen since the last word kept.
    let mut gap = false;
    for w in words.drain(..) {
        let text = w.text.trim();
        if text.is_empty() {
            gap = true;
            continue;
        }
        if gap || w.text.starts_with(char::is_whitespace) {
            if let Some(prev) = out.last_mut() {
                prev.text.push(' ');
            }
        }
        gap = w.text.ends_with(char::is_whitespace);
        out.push(LyricWord { text: text.to_owned(), ..w });
    }
    *words = out;
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

/// Pick the search hit that is our song: the same title, then the closest in length to what we're
/// playing, rejecting anything further off than `MATCH_TOLERANCE_SECS`.
///
/// The title comes first because length alone names no song. A catalogue search can go wide —
/// YouTube's localized artist line ("COOLKID, RHYDER và BAN") once sent QQ off to every song called
/// "Cool Kid" — and in a page of unrelated songs one will be the right length by chance: that is
/// how a Vietnamese track got Oliver Jiang's "酷小孩Cool Kid", at exactly 192 seconds. No lyrics
/// is a better answer than someone else's.
///
/// Closest-match rather than first-within-tolerance matters because these catalogues rank remixes,
/// live cuts and radio edits right next to the original (Kugou's top hit for "Shape of You" is a
/// 263s edit of a 233s song, and Netease ranks a 231s remix second): the remix often has the same
/// title once its "(Remix)" is dropped, and sits inside the window too, so only the distance
/// separates it from the real cut.
///
/// With no length on our side there is nothing to measure, so the first same-titled hit stands. A
/// candidate whose own length is missing ranks last but is not dropped — if a provider renames the
/// field we want degraded matching, not a provider that silently returns nothing.
fn best_match<'a, T>(
    title: &str,
    ours: Option<f64>,
    cands: &'a [T],
    title_of: impl Fn(&'a T) -> Option<&'a str>,
    secs: impl Fn(&T) -> Option<f64>,
) -> Option<&'a T> {
    let named = cands.iter().filter(|c| same_title(title, title_of(c).unwrap_or("")));
    let Some(ours) = ours.filter(|d| *d > 0.0) else {
        return named.into_iter().next();
    };
    named
        .map(|c| (secs(c).map_or(f64::INFINITY, |d| (d - ours).abs()), c))
        .filter(|(d, _)| *d <= MATCH_TOLERANCE_SECS || d.is_infinite())
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| c)
}

/// A song title reduced to what two catalogues agree on. Accents fold away — a catalogue that files
/// "Chịu Cách Mình Nói Thua" as "Chiu Cach Minh Noi Thua" is naming the same song — and so do
/// bracketed asides ("(Official Video)", "(feat. X)", "[MV]", "【…】", a search's `<em>` highlight)
/// and everything that is not a letter or a digit, spaces included. CJK stays as it is: those
/// characters are letters, and they are the title.
fn title_key(s: &str) -> String {
    let mut depth = 0u32;
    let mut out = String::new();
    for c in s.nfd() {
        match c {
            '(' | '[' | '{' | '<' | '（' | '【' | '「' | '『' => depth += 1,
            ')' | ']' | '}' | '>' | '）' | '】' | '」' | '』' => {
                depth = depth.saturating_sub(1)
            }
            _ if depth > 0 => {}
            'đ' | 'Đ' => out.push('d'),
            c if unicode_normalization::char::is_combining_mark(c) => {}
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => {}
        }
    }
    out
}

/// Whether a catalogue's title names the song we asked for (see `best_match`). Containment either
/// way, so "Song" still finds "Song - From 'The Film'" and the reverse; but a key under four
/// characters has to match outright, or "Go" would claim "Gorgeous". A title with nothing left to
/// compare on our side blocks nothing; a hit with no title of its own is not trusted.
fn same_title(ours: &str, theirs: &str) -> bool {
    let (a, b) = (title_key(ours), title_key(theirs));
    if a.is_empty() {
        return true;
    }
    if b.is_empty() {
        return false;
    }
    let (short, long) = if a.chars().count() <= b.chars().count() { (&a, &b) } else { (&b, &a) };
    short == long || (short.chars().count() >= 4 && long.contains(short.as_str()))
}

/// The lead artist out of YouTube's artist line, for a catalogue search. YouTube joins the names in
/// the content language — "A, B & C", "A, B và C", "A, B und C" — and the joining word is noise to
/// a search engine that has never seen it: "và" is what sent QQ looking for "Cool Kid". The lead
/// artist and the title find the song on every catalogue here; a language whose joining word is
/// not listed only costs a query some precision, never correctness, which `same_title` guards.
fn lead_artist(artists: &str) -> &str {
    const JOINS: [&str; 14] = [
        ",", "、", " & ", " x ", " X ", " feat.", " feat ", " ft.", " và ", " and ", " und ",
        " et ", " y ", " e ",
    ];
    let cut = JOINS.iter().filter_map(|j| artists.find(j)).min().unwrap_or(artists.len());
    artists[..cut].trim()
}

/// What a catalogue search is asked for: the title and the lead artist.
fn search_query(req: &LyricsRequest) -> String {
    format!("{} {}", req.title, lead_artist(&req.artists)).trim().to_owned()
}

/// Netease Cloud Music provider (supports LRC, word timestamps, & translations)
async fn netease_get(req: &LyricsRequest) -> Result<Option<Lyrics>, reqwest::Error> {
    let query = search_query(req);
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
    let hit = best_match(
        &req.title,
        req.duration,
        songs,
        |s| s.get("name")?.as_str(),
        |s| Some(s.get("duration")?.as_f64()? / 1000.0),
    );
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
async fn simpmusic_get(
    state: &AppState,
    req: &LyricsRequest,
) -> Result<Option<Lyrics>, reqwest::Error> {
    let Some(e) = simpmusic_entry(req).await? else { return Ok(None) };
    let timed = [&e.rich_sync_lyrics, &e.synced_lyrics]
        .iter()
        .any(|s| s.as_deref().is_some_and(|s| !s.trim().is_empty()));
    // Only timed lyrics can be timed to the wrong cut, so only they cost the lookup.
    let non_music =
        if timed { crate::sponsorblock::non_music(state, &req.video_id).await } else { Vec::new() };
    // What the video is only matters when SponsorBlock can't place the song and the entry has a
    // later timeline to move onto, which is rare enough to ask YouTube when nobody has said yet.
    let is_video = match req.is_video {
        Some(v) => v,
        None if non_music.is_empty() && simpmusic_later_by(&e).is_some() => {
            is_music_video(state, &req.video_id).await
        }
        None => false,
    };
    let l = simpmusic_lyrics(&e, &non_music, is_video);
    if l.is_none() && timed {
        tracing::debug!(video_id = %req.video_id, "lyrics: SimpMusic timed for another cut, skipped");
    }
    Ok(l)
}

/// The entry for this video: of those within `MATCH_TOLERANCE_SECS` of its length, the best voted.
async fn simpmusic_entry(req: &LyricsRequest) -> Result<Option<SimpMusicEntry>, reqwest::Error> {
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
    Ok(body
        .data
        .into_iter()
        .filter(|e| match (ours, e.duration_seconds.filter(|d| *d > 0.0)) {
            (Some(a), Some(b)) => (a - b).abs() <= MATCH_TOLERANCE_SECS,
            _ => true,
        })
        .max_by_key(|e| e.vote))
}

/// Word-synced over line-synced over plain, whichever the entry carries — and, for the timed ones,
/// whichever fits the video (`on_this_video`). `None` when its timing belongs to another cut, so
/// the chain moves on to a provider that has this one.
fn simpmusic_lyrics(
    e: &SimpMusicEntry,
    non_music: &[(f64, f64)],
    is_video: bool,
) -> Option<Lyrics> {
    let (rich, synced) = simpmusic_timed(e);
    if rich.is_none() && synced.is_none() {
        return simpmusic_plain(e);
    }
    from_parsed(SIMPMUSIC, on_this_video(rich, synced, non_music, is_video)?)
}

const SIMPMUSIC: &str = "SimpMusic Lyrics";

/// How far the entry's line-synced form runs behind its word-synced one, when it does by a fixed
/// shift (`shared_offset`).
fn simpmusic_later_by(e: &SimpMusicEntry) -> Option<i64> {
    let (Some(rich), Some(synced)) = simpmusic_timed(e) else { return None };
    shared_offset(&vocal_cues(&rich), &vocal_cues(&synced)).filter(|&d| d > 0)
}

/// Whether YouTube Music calls `video_id` a music video. Its watch queue opens on the video itself,
/// typed. A request that fails answers no, which leaves the lyrics as the entry has them.
async fn is_music_video(state: &AppState, video_id: &str) -> bool {
    let Some(client) = state.clients.get(innertube::METADATA_CLIENT) else { return false };
    match state.it.next(client, Some(video_id), None).await {
        Ok(next) => next.items.iter().any(|i| i.video_id == video_id && i.is_video),
        Err(e) => {
            tracing::debug!(video_id, error = %e, "lyrics: couldn't ask what the video is");
            false
        }
    }
}

/// An entry's two timed forms, parsed: word-synced and line-synced. Either may be missing.
fn simpmusic_timed(e: &SimpMusicEntry) -> (Option<Vec<LyricLine>>, Option<Vec<LyricLine>>) {
    fn present(s: &Option<String>) -> Option<&str> {
        s.as_deref().filter(|s| !s.trim().is_empty())
    }
    let rich = present(&e.rich_sync_lyrics)
        .map(|r| clean_lines(parse_rich_sync(r)))
        .filter(|l| !l.is_empty());
    let synced = present(&e.synced_lyrics)
        .map(|lrc| clean_lines(parse_lrc(&decode_entities(lrc))))
        .filter(|l| !l.is_empty());
    (rich, synced)
}

fn simpmusic_plain(e: &SimpMusicEntry) -> Option<Lyrics> {
    let plain = e.plain_lyric.as_deref().filter(|s| !s.trim().is_empty());
    plain_from_text(plain.map(decode_entities).as_deref(), SIMPMUSIC)
}

/// The space taken back out of words an entry split into syllables — "e nough" → "enough", "an ti
/// dote" → "antidote" — going by `reference`, a text of the same song that spells them out.
///
/// Some SimpMusic entries time a word syllable by syllable and set every syllable down as a word of
/// its own, in the word sync, the line sync and the plain text alike: Olivia Rodrigo's "the cure"
/// reads "Why can't it ever be e nough?" and "I'm un rav eled" in all three, so nothing in the
/// entry tells "e nough" from two words. The reference decides: of the ways to group a line's
/// pieces into words, the one that leaves the fewest the reference never writes, with the fewest
/// joins that takes. Pieces that are each a word of the reference on their own are never joined,
/// so "may be" stays apart in a song that also sings "maybe", and a hyphenated word of the
/// reference counts as its parts: joining "skin tight" because the reference has "skin-tight"
/// would only lose the hyphen. The syllables keep their own timing; only the space between them
/// goes, and both renderers draw pieces with no space between them as one word.
fn join_syllables(lines: &mut [LyricLine], reference: &str) {
    fn key(s: &str) -> String {
        s.nfc().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
    }
    let known: HashSet<String> = reference
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '‐' | '–' | '—'))
        .map(key)
        .collect();
    let unknown = |k: &str| !k.is_empty() && !known.contains(k);
    for line in lines {
        // Each piece with the space after it, if it has one. A line with no word timing splits at
        // its spaces.
        let mut pieces: Vec<String> = match &line.words {
            Some(words) => words.iter().map(|w| w.text.clone()).collect(),
            None => line.text.split_whitespace().map(|p| format!("{p} ")).collect(),
        };
        let keys: Vec<String> = pieces.iter().map(|p| key(p)).collect();
        // The best grouping of the first `i` pieces: (words the reference never writes, joins,
        // where its last group starts).
        let mut best = vec![(usize::MAX, usize::MAX, 0); pieces.len() + 1];
        best[0] = (0, 0, 0);
        for end in 1..=pieces.len() {
            let mut run = String::new();
            let mut all_known = true;
            for start in (0..end).rev() {
                let single = start + 1 == end;
                // Punctuation on its own is nobody's syllable.
                if !single && (keys[start].is_empty() || keys[end - 1].is_empty()) {
                    break;
                }
                run.insert_str(0, &keys[start]);
                all_known &= !unknown(&keys[start]);
                if !single && (all_known || unknown(&run)) {
                    continue;
                }
                let (missed, joins, _) = best[start];
                let grouped = (missed + usize::from(unknown(&run)), joins + end - start - 1, start);
                best[end] = best[end].min(grouped);
            }
        }
        let mut joined = false;
        let mut end = pieces.len();
        while end > 0 {
            let start = best[end].2;
            for p in &mut pieces[start..end - 1] {
                let len = p.trim_end().len();
                joined |= len < p.len();
                p.truncate(len);
            }
            end = start;
        }
        if !joined {
            continue;
        }
        line.text = pieces.concat().trim_end().to_owned();
        if let Some(words) = &mut line.words {
            for (w, p) in words.iter_mut().zip(pieces) {
                w.text = p;
            }
        }
    }
}

/// A shift below this is noise between two people's timing of the same cut, not two cuts.
const CUT_OFFSET_MIN_MS: u64 = 1000;
/// How close a shifted line has to land on a line of the other timeline to count as the same line.
const LINE_MATCH_MS: u64 = 500;
/// Lines compared when looking for a shared offset: enough to be sure, few enough to be cheap.
const OFFSET_SAMPLE: usize = 20;
/// How far a video's opening scene may disagree with the timelines' offset and still stand for it.
const SCENE_AGREE_MS: u64 = 1500;

/// The word-synced lines if they fit the video that is playing, moved onto it if they were timed
/// on another cut of the song, or the line-synced ones — or `None` when nothing in the entry fits.
///
/// SimpMusic's entries are community-made per video, and the two timed forms in one entry do not
/// always come from the same cut. For Sơn Tùng's "Đừng Làm Trái Tim Anh Đau" music video, the
/// words were timed against the 281 s audio release while the lines follow the 326 s video, whose
/// first 16.4 s are a spoken scene: every word ran 16.4 s ahead of the singing. Two pieces of
/// evidence settle which timeline belongs to the video:
///
/// - A lyric can't be sung inside a section SponsorBlock marks as not music. Timing that puts two
///   or more lines there was made on another cut.
/// - A music video is its song plus scenes around it, so of two timelines a fixed shift apart,
///   the later one is the video's. (Checked on six Vietnamese music videos: it picks the timing
///   LRCLIB's video-length entries agree with in both cases where the forms disagree.)
///
/// The second holds without SponsorBlock too, as long as this is known to be a music video:
/// "Muộn Rồi Mà Sao Còn" has no SponsorBlock sections, and its words run on the 276 s release while
/// the video opens with 8.91 s of scene (measured by cross-correlating the two audio tracks). The
/// entry's English line translation follows the video, and the shared offset comes out at 9.03 s.
/// On an audio track, or with no second timeline to compare, the word-synced lines stand, which
/// is what every entry did before this existed.
fn on_this_video(
    rich: Option<Vec<LyricLine>>,
    synced: Option<Vec<LyricLine>>,
    non_music: &[(f64, f64)],
    is_video: bool,
) -> Option<Vec<LyricLine>> {
    if non_music.is_empty() {
        let later = match (&rich, &synced) {
            (Some(r), Some(s)) if is_video => {
                shared_offset(&vocal_cues(r), &vocal_cues(s)).filter(|&d| d > 0)
            }
            _ => None,
        };
        return match later {
            Some(shift) => rich.map(|mut r| {
                shift_lines(&mut r, shift);
                r
            }),
            None => rich.or(synced),
        };
    }
    let fits = |lines: &[LyricLine], shift: i64| sung_in_non_music(lines, shift, non_music) < 2;
    if let (Some(r), Some(s)) = (&rich, &synced) {
        if let Some(shift) = shared_offset(&vocal_cues(r), &vocal_cues(s)) {
            let (here, moved) = (fits(r, 0), fits(r, shift));
            let take_moved = match (here, moved) {
                (true, true) => shift > 0,
                (here, moved) if here != moved => moved,
                _ => return None, // neither timeline fits this video
            };
            let mut r = rich.unwrap();
            if take_moved {
                shift_lines(&mut r, opening_scene(non_music, shift));
            }
            return Some(r);
        }
    }
    if let Some(r) = rich.filter(|r| fits(r, 0)) {
        return Some(r);
    }
    synced.filter(|s| fits(s, 0))
}

/// The shift onto a video that opens on a non-music scene: that scene's end, when SponsorBlock
/// marks one from the top and it agrees with `shift` to within `SCENE_AGREE_MS`; `shift` otherwise.
///
/// Both measure how long the video runs before the release's first sample, but the offset between
/// two timelines is only as tight as the looser of them, and the second form is often a translation
/// or captions. Against the audio offsets measured by cross-correlating four videos with their
/// releases, the scene's end was off by 0.02–0.21 s and the timelines' offset by up to 0.75 s
/// ("bad guy", whose second form is closed captions).
fn opening_scene(non_music: &[(f64, f64)], shift: i64) -> i64 {
    if shift <= 0 {
        return shift;
    }
    non_music
        .iter()
        .find(|&&(start, _)| start <= 1.0)
        .map(|&(_, end)| (end * 1000.0).round() as i64)
        .filter(|end| end.abs_diff(shift) <= SCENE_AGREE_MS)
        .unwrap_or(shift)
}

/// Cue times of the lines that are sung (a cue and some text), in ms.
fn vocal_cues(lines: &[LyricLine]) -> Vec<u64> {
    lines.iter().filter(|l| !l.text.trim().is_empty()).filter_map(|l| l.time_ms).collect()
}

/// The fixed shift (ms, added to `a`) that puts most of `a`'s lines onto `b`'s, when there is one
/// worth acting on: at least a second, and agreed on by seven in ten of the lines compared. Two
/// timings of one cut disagree by less than that, and two unrelated line sets agree on no shift.
/// Matched by time, not by index or text, because the two forms need not have the same lines —
/// or the same language: one entry pairs Vietnamese words with an English line translation.
fn shared_offset(a: &[u64], b: &[u64]) -> Option<i64> {
    let a = &a[..a.len().min(OFFSET_SAMPLE)];
    let b = &b[..b.len().min(OFFSET_SAMPLE + 5)];
    if a.len() < 5 || b.is_empty() {
        return None;
    }
    // For a shift: how many of `a`'s lines land on one of `b`'s, and how far off they land in all.
    let fit = |d: i64| {
        let misses = a
            .iter()
            .filter_map(|&x| b.iter().map(|&y| (x as i64 + d - y as i64).unsigned_abs()).min());
        misses.filter(|&m| m <= LINE_MATCH_MS).fold((0usize, 0u64), |(n, sum), m| (n + 1, sum + m))
    };
    // Most lines matched, then the tightest match. The second matters for evenly spaced lines: a
    // shift of whole lines lands most of them *near* a line too, and only the true shift lands
    // them on one. (Smallest shift was the tie-break at first, and it picked such an alias.)
    let mut best: Option<(usize, u64, i64)> = None;
    for &x in a {
        for &y in b {
            let d = y as i64 - x as i64;
            let (n, miss) = fit(d);
            if best.is_none_or(|(bn, bm, _)| n > bn || (n == bn && miss < bm)) {
                best = Some((n, miss, d));
            }
        }
    }
    let (n, _, d) = best?;
    (d.unsigned_abs() >= CUT_OFFSET_MIN_MS && n * 10 >= a.len() * 7).then_some(d)
}

/// How many sung lines, moved by `shift` ms, fall inside a non-music section (half a second in
/// from either edge, so a line that starts as the music does is not counted against it).
fn sung_in_non_music(lines: &[LyricLine], shift: i64, non_music: &[(f64, f64)]) -> usize {
    vocal_cues(lines)
        .into_iter()
        .map(|t| (t as i64 + shift) as f64 / 1000.0)
        .filter(|t| non_music.iter().any(|&(a, b)| *t > a + 0.5 && *t < b - 0.5))
        .count()
}

/// Move every cue in `lines` — line starts, line ends, words — by `shift` ms.
fn shift_lines(lines: &mut [LyricLine], shift: i64) {
    let mv = |t: u64| (t as i64 + shift).max(0) as u64;
    for l in lines {
        l.time_ms = l.time_ms.map(mv);
        l.end_time_ms = l.end_time_ms.map(mv);
        for w in l.words.iter_mut().flatten() {
            w.start_ms = mv(w.start_ms);
            w.end_ms = mv(w.end_ms);
        }
    }
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
    let query = search_query(req);
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
    let hit = best_match(
        &req.title,
        req.duration,
        songs,
        |s| s.get("songname")?.as_str(),
        |s| s.get("interval")?.as_f64(),
    );
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
    let query = search_query(req);
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
    let hit = best_match(
        &req.title,
        req.duration,
        songs,
        |s| s.get("SongName")?.as_str(),
        |s| s.get("Duration")?.as_f64(),
    );
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
    fn best_match_skips_remixes_and_wrong_cuts() {
        type Hit = (f64, &'static str, &'static str);
        fn pick(d: Option<f64>, c: &[Hit]) -> Option<&'static str> {
            best_match("Shape of You", d, c, |t| Some(t.1), |t| Some(t.0)).map(|t| t.2)
        }
        // Kugou's actual top hit for "Shape of You" is a 263s edit of a 233s song.
        let kugou = [
            (263.0, "Shape of You", "wrong cut"),
            (251.0, "Shape of You (DJ PULLER版)", "dj edit"),
            (233.0, "Shape of You", "original"),
        ];
        assert_eq!(pick(Some(233.0), &kugou), Some("original"));
        // Netease ranks a 231s remix second; both are within 5s, distance breaks the tie.
        let netease =
            [(233.7, "Shape of You", "original"), (231.2, "Shape of You (Remix)", "remix")];
        assert_eq!(pick(Some(233.0), &netease), Some("original"));
        // Nothing close enough beats a wrong answer.
        assert_eq!(pick(Some(233.0), &kugou[..2]), None);
        // No length on our side: nothing to measure, the first same-titled hit stands.
        assert_eq!(pick(None, &kugou), Some("wrong cut"));
        // A hit with no length of its own still gets used rather than silently dropped.
        let unknown: [Hit; 1] = [(0.0, "Shape of You", "no duration")];
        let hit = best_match("Shape of You", Some(233.0), &unknown, |t| Some(t.1), |_| None);
        assert_eq!(hit.map(|t| t.2), Some("no duration"));
    }

    /// The search that went wrong, as QQ answered it: "và" in the artist line sent it after "Cool
    /// Kid", and one of those songs is exactly as long as ours. Length alone picked it.
    #[test]
    fn a_same_length_stranger_is_not_our_song() {
        let qq = [
            (237.0, "Cool Kids"),
            (153.0, "The Cool Kid"),
            (192.0, "酷小孩Cool Kid"),
            (240.0, "50 CUỘC GỌI NHỠ"),
        ];
        let title = "Chịu cách mình nói thua";
        assert!(best_match(title, Some(192.0), &qq, |t| Some(t.1), |t| Some(t.0)).is_none());
        // The same song under a catalogue's capitals, without its accents, or with an aside.
        for theirs in [
            "Chịu Cách Mình Nói Thua",
            "Chiu Cach Minh Noi Thua",
            "Chịu Cách Mình Nói Thua (Official Audio)",
        ] {
            assert!(same_title(title, theirs), "{theirs}");
        }
        assert!(same_title("Shape of You", "<em>Shape</em> of You"));
        assert!(same_title("告白气球", "告白气球 (Live)"));
        assert!(!same_title("Go", "Gorgeous"), "a short title has to match outright");
        assert!(!same_title("Nơi Này Có Anh", ""), "a hit with no title is not trusted");
    }

    #[test]
    fn a_catalogue_search_asks_for_the_lead_artist() {
        assert_eq!(lead_artist("COOLKID, RHYDER và BAN"), "COOLKID");
        assert_eq!(lead_artist("Sơn Tùng M-TP & Tyga"), "Sơn Tùng M-TP");
        assert_eq!(lead_artist("Sơn Tùng M-TP và Tyga"), "Sơn Tùng M-TP");
        assert_eq!(lead_artist("Ed Sheeran"), "Ed Sheeran");
    }

    /// SimpMusic's spaced rich sync, as it came for "Đừng Làm Trái Tim Anh Đau": spaces around every
    /// word and a timed run of three between them. Drawn as parsed, every word stood apart.
    #[test]
    fn word_spacing_is_one_space_whatever_the_source_did() {
        let words = |raw: &str| -> Vec<String> {
            let l = from_parsed("X", parse_rich_sync(raw)).unwrap();
            l.lines[0].words.as_ref().unwrap().iter().map(|w| w.text.clone()).collect()
        };
        let text =
            |raw: &str| from_parsed("X", parse_rich_sync(raw)).unwrap().lines[0].text.clone();
        assert_eq!(
            text("[00:43.81] <00:43.81> Uhm, <00:44.20>   <00:44.25> đau <00:44.38>   <00:44.43> hết <00:44.55>"),
            "Uhm, đau hết"
        );
        assert_eq!(
            words("[00:43.81] <00:43.81> Uhm, <00:44.20>   <00:44.25> đau <00:44.38>   <00:44.43> hết <00:44.55>"),
            ["Uhm, ", "đau ", "hết"]
        );
        // The compact form was already right and stays so.
        assert_eq!(
            words("[00:33.84] <00:33.84>Một <00:34.03>người <00:34.25>nắm"),
            ["Một ", "người ", "nắm"]
        );
        // No spaces in the source, none added.
        assert_eq!(
            words("[00:01.00] <00:01.00>我<00:01.30>爱<00:01.60>你<00:02.00>"),
            ["我", "爱", "你"]
        );
    }

    /// Lines at these cues (seconds), each with one word, the way a rich-sync entry parses.
    fn timed(cues: &[f64]) -> Vec<LyricLine> {
        cues.iter()
            .map(|&t| {
                let ms = (t * 1000.0).round() as u64;
                LyricLine {
                    time_ms: Some(ms),
                    end_time_ms: Some(ms + 900),
                    text: "la".into(),
                    words: Some(vec![LyricWord {
                        text: "la".into(),
                        start_ms: ms,
                        end_ms: ms + 900,
                    }]),
                    translation: None,
                }
            })
            .collect()
    }
    fn first_cue(lines: &[LyricLine]) -> f64 {
        lines[0].time_ms.unwrap() as f64 / 1000.0
    }
    /// A realistic run of verse cues starting at `start`.
    fn verse(start: f64) -> Vec<f64> {
        (0..24).map(|i| start + i as f64 * 3.7 + if i % 3 == 1 { 0.4 } else { 0.0 }).collect()
    }

    /// "Đừng Làm Trái Tim Anh Đau", the music video: the words were timed on the 281 s audio
    /// release (first line 30.37 s), the lines on the 326 s video (46.78 s), whose first 16.4 s
    /// SponsorBlock marks as not music. Both fit; the later one is the video's.
    #[test]
    fn words_timed_on_the_audio_release_move_onto_the_video() {
        let rich = verse(30.37);
        let synced: Vec<f64> = rich.iter().map(|t| t + 16.41).collect();
        let non_music = [(0.0, 16.43), (292.84, 325.66)];
        let out =
            on_this_video(Some(timed(&rich)), Some(timed(&synced)), &non_music, true).unwrap();
        // By the opening scene's end, 16.43 s. The audio tracks put the song 16.48 s in.
        assert!((first_cue(&out) - 46.80).abs() < 0.01, "got {}", first_cue(&out));
        // The words move with their lines: the karaoke sweep follows the new timing too.
        assert_eq!(out[0].words.as_ref().unwrap()[0].start_ms, out[0].time_ms.unwrap());
        // Without SponsorBlock the video still runs later than the release its words were timed on.
        let out = on_this_video(Some(timed(&rich)), Some(timed(&synced)), &[], true).unwrap();
        assert!((first_cue(&out) - 46.78).abs() < 0.01, "got {}", first_cue(&out));
        // On an audio track there is no such rule, and nothing is moved.
        let out = on_this_video(Some(timed(&rich)), Some(timed(&synced)), &[], false).unwrap();
        assert!((first_cue(&out) - 30.37).abs() < 0.01);
    }

    /// "bad guy", the music video: the second form is closed captions, loose enough that the two
    /// timelines' offset comes out at 13.33 s. The song starts 14.08 s into the video (measured on
    /// the audio tracks), and SponsorBlock's opening scene ends at 14.1 s.
    #[test]
    fn the_opening_scene_places_the_song_more_tightly_than_loose_lines() {
        let rich = [
            14.03, 17.6, 21.24, 24.81, 28.33, 31.87, 34.77, 39.01, 41.9, 43.52, 45.23, 47.0, 49.37,
            50.52, 52.29, 54.23, 56.28, 66.92, 74.6, 78.22,
        ];
        let captions = [
            3.33, 5.1, 6.6, 9.1, 13.36, 27.46, 28.56, 31.16, 32.13, 34.43, 35.56, 38.16, 41.56,
            42.6, 45.2, 46.26, 48.6, 49.56, 52.26, 55.16, 57.13, 58.43, 60.23, 62.43, 63.66,
        ];
        let ms = |v: &[f64]| v.iter().map(|t| (t * 1000.0).round() as u64).collect::<Vec<_>>();
        assert_eq!(shared_offset(&ms(&rich), &ms(&captions)), Some(13330));
        let non_music = [(0.0, 14.1), (194.0, 205.9)];
        let out =
            on_this_video(Some(timed(&rich)), Some(timed(&captions)), &non_music, true).unwrap();
        assert!((first_cue(&out) - (14.03 + 14.08)).abs() < 0.1, "got {}", first_cue(&out));
        // A scene that disagrees with the timelines by more than a second and a half is something
        // else (a spoken intro over music, say), and the timelines' own offset stands.
        let out = on_this_video(Some(timed(&rich)), Some(timed(&captions)), &[(0.0, 11.0)], true)
            .unwrap();
        assert!((first_cue(&out) - (14.03 + 13.33)).abs() < 0.01, "got {}", first_cue(&out));
    }

    /// "Muộn Rồi Mà Sao Còn", the music video: no SponsorBlock sections at all. The words are the
    /// 276 s release's (29.05 s), the English line translation follows the 288 s video. The video's
    /// singing starts 8.91 s after the release's, measured by cross-correlating the two audio
    /// tracks; the offset the translation gives is within 0.15 s of that.
    #[test]
    fn a_music_video_without_sponsorblock_takes_the_later_timeline() {
        let rich = [
            29.05, 31.6, 35.34, 37.32, 39.17, 41.4, 43.14, 45.07, 46.82, 49.78, 52.85, 54.46,
            56.31, 58.17, 60.27, 62.39, 64.01, 65.29, 67.81, 69.68,
        ];
        let english = [
            38.43, 40.89, 42.86, 44.61, 46.41, 48.14, 50.47, 52.15, 53.95, 55.85, 59.11, 61.8,
            63.46, 65.28, 67.38, 69.2, 71.23, 73.07, 76.89, 78.68, 80.68, 82.52, 84.59, 86.36,
            88.23,
        ];
        let out = on_this_video(Some(timed(&rich)), Some(timed(&english)), &[], true).unwrap();
        assert!((first_cue(&out) - (29.05 + 8.91)).abs() < 0.15, "got {}", first_cue(&out));
        // The Vietnamese words are what shows, only moved.
        assert_eq!(out.len(), rich.len());
    }

    /// "Chạy Ngay Đi": the same disagreement the other way round. The words (28.3 s, which
    /// LRCLIB's video-length entries agree with) are the later timeline, so they stay put.
    #[test]
    fn words_already_on_the_video_stay_put() {
        let rich = verse(28.3);
        let mut synced: Vec<f64> = rich.iter().map(|t| t - 13.2).collect();
        synced.insert(0, 6.01); // a line the words don't have
        let out =
            on_this_video(Some(timed(&rich)), Some(timed(&synced)), &[(0.0, 10.2)], true).unwrap();
        assert!(
            (first_cue(&out) - 28.3).abs() < 0.01,
            "got {} of {:?}",
            first_cue(&out),
            out.iter().take(3).map(|l| l.time_ms).collect::<Vec<_>>()
        );
    }

    /// "See tình": words sung inside the spoken intro can't be the video's timing. The lines fit,
    /// so they are what shows.
    #[test]
    fn words_sung_over_a_spoken_scene_give_way_to_lines_that_fit() {
        let rich = verse(0.17);
        let synced = verse(43.0);
        let non_music = [(0.0, 33.5), (209.7, 236.5)];
        let out =
            on_this_video(Some(timed(&rich)), Some(timed(&synced)), &non_music, true).unwrap();
        assert!((first_cue(&out) - 43.0).abs() < 0.01, "got {}", first_cue(&out));
        // And with nothing that fits, the provider steps aside for the next one.
        assert!(on_this_video(Some(timed(&rich)), None, &non_music, true).is_none());
    }

    #[test]
    fn two_timings_of_one_cut_are_not_a_shift() {
        let a = verse(21.05);
        let b: Vec<f64> = a.iter().map(|t| t - 0.37).collect();
        let ms = |v: &[f64]| v.iter().map(|t| (t * 1000.0).round() as u64).collect::<Vec<_>>();
        assert_eq!(shared_offset(&ms(&a), &ms(&b)), None, "0.37 s is two people timing one cut");
        let c: Vec<f64> = a.iter().map(|t| t + 16.41).collect();
        assert_eq!(shared_offset(&ms(&a), &ms(&c)), Some(16410));
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
        let l = simpmusic_lyrics(&entry, &[], false).unwrap();
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
        let l = simpmusic_lyrics(&e, &[], false).unwrap();
        assert!(l.synced && l.lines[1].words.is_none());
        assert_eq!(l.lines[1].text, "two & three");
        e.synced_lyrics = None;
        let l = simpmusic_lyrics(&e, &[], false).unwrap();
        assert!(!l.synced);
        assert_eq!(l.source, "SimpMusic Lyrics");
    }

    /// SimpMusic's entry for Olivia Rodrigo's "the cure", every syllable a word of its own, against
    /// YouTube's lyrics for it (LyricFind's), which spell the words out.
    #[test]
    fn syllables_set_down_as_words_join_back_into_the_word() {
        let reference = "I thought I found the antidote this time\r\nBut I'm unraveled\r\n\
            Why can't it ever be enough?\r\nMaybe it may be\r\nSkin-tight";
        let rich = "[00:26.11] <00:26.11>I <00:26.36>thought <00:26.61>I <00:26.78>found <00:27.05>the <00:27.25>an <00:28.45>ti <00:28.72>dote <00:29.73>this <00:30.10>time\n\
            [02:12.80] <02:12.80>But <02:13.11>I'm <02:13.31>un <02:13.66>rav <02:14.16>eled\n\
            [03:24.23] <03:24.23>Why <03:24.64>can't <03:25.24>it <03:25.55>ever <03:26.10>be <03:26.54>e <03:27.01>nough?\n\
            [03:30.00] <03:30.00>It <03:30.20>may <03:30.40>be <03:30.60>e <03:30.80>nough\n\
            [03:31.00] <03:31.00>Skin <03:31.40>tight";
        let mut l = from_parsed("X", parse_rich_sync(rich)).unwrap();
        join_syllables(&mut l.lines, reference);
        let text: Vec<&str> = l.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            text,
            [
                "I thought I found the antidote this time",
                "But I'm unraveled",
                "Why can't it ever be enough?",
                // Both words of the reference, however they concatenate.
                "It may be enough",
                // Its hyphenated word is two.
                "Skin tight",
            ]
        );
        // Each syllable is still timed from its own tag.
        let w = l.lines[0].words.as_ref().unwrap();
        let w: Vec<(&str, u64)> = w.iter().map(|w| (w.text.as_str(), w.start_ms)).collect();
        assert_eq!(&w[4..8], [("the ", 27050), ("an", 27250), ("ti", 28450), ("dote ", 28720)]);

        // A line with no word timing, as the entry's line sync has it.
        let line = || from_parsed("X", parse_lrc("[03:39.47] It's not e nough")).unwrap().lines;
        let mut lines = line();
        join_syllables(&mut lines, "It's not enough");
        assert_eq!(lines[0].text, "It's not enough");
        // A reference with nothing to say leaves the line as it was.
        let mut lines = line();
        join_syllables(&mut lines, "");
        assert_eq!(lines[0].text, "It's not e nough");
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
            is_video: None,
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
        let entry = simpmusic_entry(&sm).await.unwrap().expect("SimpMusic Lyrics entry");
        let sm = simpmusic_lyrics(&entry, &[], false).expect("SimpMusic Lyrics hit");
        println!("SimpMusic Lyrics: {} lines, synced={}", sm.lines.len(), sm.synced);
        assert!(sm.lines.iter().any(|l| l.words.is_some()));
    }
}
