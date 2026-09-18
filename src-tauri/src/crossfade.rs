//! Crossfading between tracks: what the settings ask for, and when it stands aside.
//!
//! The mixing itself is the player crate's (`player::Crossfade`). This decides what to hand it:
//! the user's setting, except while the sleep timer waits for the end of the track, and whether a
//! particular pair of tracks may overlap at all.

use innertube::SongItem;
use player::Crossfade;

use crate::db::Db;
use crate::state::AppState;

/// The longest crossfade the setting offers. The player also caps it at a third of the track.
pub const MAX_SECS: f64 = 12.0;
/// The length a crossfade gets when it is switched on for the first time.
const DEFAULT_SECS: f64 = 6.0;

/// The setting as stored. Off unless switched on; the sweep filters and album rule default on,
/// since they are what makes it sound like a mix rather than two songs talking over each other.
pub fn from_settings(db: &Db) -> Crossfade {
    if db.get_setting("crossfade").as_deref() != Some("true") {
        return Crossfade::default();
    }
    let secs = db
        .get_setting("crossfade_secs")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(DEFAULT_SECS)
        .clamp(1.0, MAX_SECS);
    Crossfade { secs, filters: db.get_setting("crossfade_filters").as_deref() != Some("false") }
}

/// Hand the player what applies right now. Called when a crossfade setting changes and whenever
/// the sleep timer is armed, fired or cleared.
///
/// "Stop at the end of this track" is the exception. That timer fades the track out over its last
/// seconds and pauses just short of its end, and a crossfade would have started the next song
/// under it by then: the pause would land a few seconds into a track the user never chose to hear,
/// and the queue would already have moved on. So for as long as it is armed, tracks meet the
/// gapless way.
pub fn sync(state: &AppState) {
    let crossfade = if state.sleep_timer.end_of_track() {
        Crossfade::default()
    } else {
        from_settings(&state.db)
    };
    if let Err(e) = state.player.set_crossfade(crossfade) {
        tracing::warn!(error = %e, "crossfade: couldn't apply the setting");
    }
}

/// Whether the move from `current` to `next` may crossfade. Two tracks off the same album are left
/// gapless when the user asks for that: on a live album, a DJ mix or a symphony the tracks are one
/// piece of music split at the seams, and fading across a seam breaks it.
pub fn blends(db: &Db, current: &SongItem, next: &SongItem) -> bool {
    db.get_setting("crossfade_album_gapless").as_deref() == Some("false")
        || !same_album(current, next)
}

fn same_album(a: &SongItem, b: &SongItem) -> bool {
    match (&a.album_id, &b.album_id) {
        (Some(x), Some(y)) => x == y,
        // A row without the album's id (a local file, some radio rows) still names it.
        _ => a.album.is_some() && a.album == b.album && a.artists == b.artists,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(album: Option<&str>, album_id: Option<&str>, artists: &str) -> SongItem {
        SongItem {
            video_id: "v".into(),
            title: "t".into(),
            artists: artists.into(),
            album: album.map(Into::into),
            album_id: album_id.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn an_album_is_known_by_its_id_or_else_by_name_and_artist() {
        let a = song(Some("Sky Decade"), Some("MPREb_1"), "Sơn Tùng M-TP");
        assert!(same_album(&a, &song(Some("Sky Decade"), Some("MPREb_1"), "Sơn Tùng M-TP")));
        // Same title, different release: the ids decide.
        assert!(!same_album(&a, &song(Some("Sky Decade"), Some("MPREb_2"), "Sơn Tùng M-TP")));
        // No id on one side: the name has to match, and the artist with it ("Greatest Hits").
        assert!(same_album(
            &song(Some("Live"), None, "X"),
            &song(Some("Live"), Some("MPREb_3"), "X")
        ));
        assert!(!same_album(
            &song(Some("Greatest Hits"), None, "X"),
            &song(Some("Greatest Hits"), None, "Y")
        ));
        // Two singles with no album at all are not "the same album".
        assert!(!same_album(&song(None, None, "X"), &song(None, None, "X")));
    }
}
