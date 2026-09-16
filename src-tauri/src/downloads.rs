//! Offline downloads: keep a track's audio on disk so it plays with no network.
//!
//! **This rides the local-music path, not a second playback path.** A finished download is a file,
//! and `AppState::resolve` hands mpv a file path for it exactly as it does for a track in a local
//! folder (`local.rs`). So queueing, gapless, shuffle, media keys and the mini player all work on a
//! downloaded track without knowing it was ever a stream — and none of them can break offline in a
//! way that still works online.
//!
//! **Why the metadata is copied into the row.** The point of the feature is that it works with
//! nothing to fetch, so the title, artists, duration and artwork have to be on disk too. The
//! artwork is downloaded beside the audio for the same reason: an `i.ytimg.com` URL is no use on a
//! plane.
//!
//! One at a time, deliberately. Downloading a 200-track playlist eight ways at once is how you get
//! rate-limited, and the queue is usually being listened to while it downloads — the bandwidth is
//! better spent on the stream that is playing.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::StreamExt;
use innertube::SongItem;
use tauri::Emitter;
use tokio::io::AsyncWriteExt;

use crate::db::Downloaded;
use crate::state::AppState;

/// How much to ask for at a time. 10 MiB is what every downloader settles on: big enough that the
/// per-request overhead disappears, small enough that a failure costs one chunk rather than a whole
/// album track.
const CHUNK: u64 = 10 * 1024 * 1024;

/// `Content-Range: bytes 0-10485759/4187353` → 4187353.
fn content_range_total(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?
        .rsplit('/')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Where the files live: `<app data>/downloads`.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("downloads")
}

/// A downloaded track's synthetic path is a real path, so nothing else needs a prefix the way
/// local files do — but the *videoId* still has to be a YouTube one, because that is what the
/// queue, the library and the like button all key on. Only `resolve` ever swaps in the file.
fn audio_path(dir: &Path, video_id: &str, mime: &str) -> PathBuf {
    // From the container, not the codec: mpv opens by content, but a sensible extension is what
    // makes the folder browsable and what other players need if someone copies a file out.
    let ext = if mime.contains("webm") || mime.contains("opus") {
        "webm"
    } else if mime.contains("mp4") || mime.contains("m4a") || mime.contains("mp4a") {
        "m4a"
    } else {
        "bin"
    };
    dir.join(format!("{video_id}.{ext}"))
}

fn art_path(dir: &Path, video_id: &str) -> PathBuf {
    dir.join(format!("{video_id}.jpg"))
}

/// What the UI is told while a download runs.
fn emit(state: &Arc<AppState>, video_id: &str, received: u64, total: u64, done: bool) {
    let _ = state.app.emit(
        "download-progress",
        serde_json::json!({
            "videoId": video_id,
            "received": received,
            // 0 when the server sent no Content-Length: the UI shows a spinner rather than a bar
            // that would otherwise jump to 100% and sit there.
            "total": total,
            "done": done,
        }),
    );
}

fn emit_failed(state: &Arc<AppState>, video_id: &str, error: &str) {
    let _ = state
        .app
        .emit("download-failed", serde_json::json!({ "videoId": video_id, "error": error }));
}

/// Queue `items` for download. Returns immediately; progress arrives as events.
///
/// Already-downloaded tracks and local files are skipped rather than refused: "download this
/// album" on an album you half have should finish it, not error.
pub fn enqueue(state: &Arc<AppState>, items: Vec<SongItem>) {
    let state = state.clone();
    // The generation this batch belongs to. `cancel` bumps it, so every check below fails at once
    // and the queue unwinds — no channel, no join handle, and nothing to leak if the window shuts.
    let gen = state.download_gen.load(Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        for item in items {
            if state.download_gen.load(Ordering::SeqCst) != gen {
                break;
            }
            if crate::local::is_local_song(&item.video_id) {
                continue;
            }
            if is_downloaded(&state, &item.video_id) {
                continue;
            }
            match one(&state, &item, gen).await {
                Ok(()) => {}
                // Cancelling is not a failure: the user asked. Saying so would put an error toast
                // on the screen for doing exactly what the button offered.
                Err(e) if e == CANCELLED => break,
                Err(e) => {
                    tracing::warn!(video_id = %item.video_id, error = %e, "download failed");
                    emit_failed(&state, &item.video_id, &e);
                }
            }
        }
        let _ = state.app.emit("downloads-idle", ());
    });
}

/// Sentinel for "the user cancelled", so the caller can tell it from a real failure.
const CANCELLED: &str = "__cancelled__";

/// Stop whatever is downloading and drop the rest of the queue. Safe when nothing is running.
///
/// Partial files are cleaned up by the worker as it unwinds: a `.part` left behind would be dead
/// weight that nothing ever looks at again, since `path_of` only ever returns the finished name.
pub fn cancel(state: &Arc<AppState>) {
    state.download_gen.fetch_add(1, Ordering::SeqCst);
    tracing::info!("downloads: cancelled");
    let _ = state.app.emit("downloads-cancelled", ());
}

/// True when the row exists *and* the file is still there. Both, because a user who cleared the
/// folder by hand would otherwise get a library full of tracks that cannot play.
pub fn is_downloaded(state: &Arc<AppState>, video_id: &str) -> bool {
    match state.db.download(video_id) {
        Some(d) => Path::new(&d.path).is_file(),
        None => false,
    }
}

/// The file for a downloaded track, if it is really there, with the codec and bitrate it was saved
/// at. `AppState::resolve` calls this.
///
/// The format travels with the path because the quality badge has to work offline too: a local
/// file carries no YouTube format, and reading it back off the row is the only way the badge still
/// says "Opus 158" on a track that has not touched the network since it was saved.
pub fn playable(state: &AppState, video_id: &str) -> Option<(String, Option<String>, Option<i64>)> {
    let d = state.db.download(video_id)?;
    Path::new(&d.path).is_file().then_some((d.path, d.audio_mime, d.audio_bitrate))
}

async fn one(state: &Arc<AppState>, item: &SongItem, gen: u64) -> Result<(), String> {
    let video_id = &item.video_id;
    // The same resolve the player uses, so a download is the same bytes the stream would have been
    // — including the quality setting and the client fallback chain.
    let data =
        state.resolve_for_download(video_id, item.is_upload).await.map_err(|e| format!("{e:?}"))?;

    let folder = dir(&state.data_dir);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let mime = data.audio_mime.clone().unwrap_or_default();
    let target = audio_path(&folder, video_id, &mime);
    // Written to a partial file and renamed at the end: a download interrupted by a crash or a
    // quit must not leave a half file that `path_of` would happily hand to mpv.
    let partial = target.with_extension("part");

    let mut file = tokio::fs::File::create(&partial).await.map_err(|e| e.to_string())?;
    let mut received: u64 = 0;
    let mut total: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    emit(state, video_id, 0, 0, false);

    // Ranged, in chunks, not one open-ended GET.
    //
    // googlevideo throttles an unbounded GET hard: measured on a normal track, a plain
    // `client.get(url)` streamed at ~34 KB/s, which is two and a half minutes for a four-minute
    // song and useless for an album. The same URL asked for in bounded ranges comes down at line
    // speed. This is the same property the orchestrator already relies on in the other direction
    // (see the note about rustypipe URLs in `AppState::resolve`) — mpv never sends Range, which is
    // why those URLs cannot be cached for it.
    loop {
        if state.download_gen.load(Ordering::SeqCst) != gen {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(CANCELLED.to_owned());
        }
        let end = received + CHUNK - 1;
        let resp = crate::http::client()
            .get(&data.stream_url)
            .header(reqwest::header::RANGE, format!("bytes={received}-{end}"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        // The total comes from `Content-Range: bytes a-b/total` on the first answer. A server that
        // ignored the Range replies 200 with the whole body and no Content-Range; that still works,
        // it just arrives in one pass.
        let ranged = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if total == 0 {
            total = content_range_total(&resp).or_else(|| resp.content_length()).unwrap_or(0);
        }

        let mut stream = resp.bytes_stream();
        let mut this_chunk: u64 = 0;
        while let Some(part) = stream.next().await {
            // Inside the byte loop too: a 10 MiB chunk on a slow line is many seconds, and a cancel
            // that only took effect at the chunk boundary would feel ignored.
            if state.download_gen.load(Ordering::SeqCst) != gen {
                drop(file);
                let _ = std::fs::remove_file(&partial);
                return Err(CANCELLED.to_owned());
            }
            let part = part.map_err(|e| e.to_string())?;
            file.write_all(&part).await.map_err(|e| e.to_string())?;
            received += part.len() as u64;
            this_chunk += part.len() as u64;
            // Throttled: a Tauri event per part is thousands of webview wakeups for one track, and
            // the bar cannot show more than a few a second anyway.
            if last_emit.elapsed() >= std::time::Duration::from_millis(250) {
                last_emit = std::time::Instant::now();
                emit(state, video_id, received, total, false);
            }
        }
        // Done when the server said how big it is and we have it all, when it ignored the Range
        // (one pass, whole file), or when a chunk came back short — which is what the last one
        // does even if `total` was never known.
        if !ranged || (total > 0 && received >= total) || this_chunk < CHUNK {
            break;
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;

    // Artwork beside it, best effort: a missing cover is a worse-looking row, not a broken one.
    let art = match item.thumbnail.as_deref() {
        Some(url) if url.starts_with("http") => save_art(&folder, video_id, url).await,
        _ => None,
    };

    state.db.put_download(&Downloaded {
        video_id: video_id.clone(),
        path: target.to_string_lossy().into_owned(),
        // Prefer the row's own metadata over the stream's: the row is what the user saw when they
        // asked for it, and `/player` titles are sometimes the raw upload name.
        title: if item.title.is_empty() {
            data.title.clone().unwrap_or_else(|| video_id.clone())
        } else {
            item.title.clone()
        },
        artists: if item.artists.is_empty() {
            data.artists.clone().unwrap_or_default()
        } else {
            item.artists.clone()
        },
        thumbnail: art,
        duration: item.duration.clone().or_else(|| data.duration.clone()),
        bytes: received as i64,
        audio_mime: data.audio_mime,
        audio_bitrate: data.audio_bitrate,
        added_at: crate::db::now_secs(),
    });
    emit(state, video_id, received, total.max(received), true);
    tracing::info!(video_id = %video_id, bytes = received, "downloaded");
    Ok(())
}

async fn save_art(folder: &Path, video_id: &str, url: &str) -> Option<String> {
    let bytes = crate::http::client().get(url).send().await.ok()?.bytes().await.ok()?;
    let path = art_path(folder, video_id);
    std::fs::write(&path, &bytes).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Forget a download: the row and both files.
pub fn remove(state: &Arc<AppState>, video_id: &str) {
    if let Some(d) = state.db.download(video_id) {
        let _ = std::fs::remove_file(&d.path);
        if let Some(t) = &d.thumbnail {
            let _ = std::fs::remove_file(t);
        }
    }
    state.db.delete_download(video_id);
    let _ = state.app.emit("downloads-changed", ());
}

/// Total bytes on disk, for the storage row in settings.
pub fn total_bytes(state: &AppState) -> i64 {
    state.db.downloads().iter().map(|d| d.bytes).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extension follows the container so the folder stays browsable and a file copied out
    /// still opens elsewhere.
    #[test]
    fn the_extension_follows_the_container() {
        let d = Path::new("/tmp/dl");
        assert!(audio_path(d, "abc", "audio/webm; codecs=\"opus\"").ends_with("abc.webm"));
        assert!(audio_path(d, "abc", "audio/mp4; codecs=\"mp4a.40.2\"").ends_with("abc.m4a"));
        // Anything unrecognised still gets a file rather than a path with no extension.
        assert!(audio_path(d, "abc", "audio/weird").ends_with("abc.bin"));
        assert!(audio_path(d, "abc", "").ends_with("abc.bin"));
    }

    /// Audio and artwork must not collide: they share a stem and are told apart by extension.
    #[test]
    fn artwork_sits_beside_the_audio_without_clashing() {
        let d = Path::new("/tmp/dl");
        let audio = audio_path(d, "xyz", "audio/webm");
        let art = art_path(d, "xyz");
        assert_ne!(audio, art);
        assert_eq!(art.file_name().unwrap(), "xyz.jpg");
    }
}
