//! libmpv wrapper. context/14. YouTube-agnostic: takes a fully-resolved URL + headers, never
//! a videoId. Gapless via mpv's internal playlist (1-track lookahead fed by the orchestrator).

use std::collections::HashMap;
use std::sync::Arc;

use libmpv2::events::{Event, EventContext, PropertyData};
use libmpv2::{Format, Mpv};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("mpv: {0}")]
    Mpv(#[from] libmpv2::Error),
    /// mpv refused a chain carrying the pitch filter, which means this libmpv was built without
    /// librubberband. Its own answer is `Raw(-9)`, so it needs saying in words: this one reaches
    /// the user as a toast.
    #[error("Pitch shifting isn't available in this build")]
    NoPitchFilter,
}

/// Events pumped from mpv's event thread. context/14 §player surface.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Position(f64),
    Duration(f64),
    /// Playback started or stopped, emitted only on a real change.
    ///
    /// Derived from mpv's `pause` **and** `idle-active`, because `pause` alone is a trap: it starts
    /// out `false` and a `loadfile` doesn't touch it, so starting a track sets `false` → `false`
    /// and fires **no** property event at all. `idle-active` is the one that actually flips when a
    /// file starts (and when the playlist runs dry). Anything reading playback state off `pause`
    /// alone never hears that a track began, and only recovers on a manual pause/unpause.
    Playing(bool),
    /// One track finished normally (EOF) — orchestrator advances the queue.
    TrackEnded,
    /// One track died (end-file with error, e.g. its URL 403'd). mpv may have auto-advanced
    /// into the next playlist entry or gone idle — the orchestrator asks [`Player::is_idle`].
    TrackFailed(String),
    Error(String),
}

/// mpv end-file reasons (from `mpv_end_file_reason`).
const EOF: i32 = 0;

/// User-facing message for a failed track — raw mpv codes ("Raw(-13)") mean nothing to users.
fn friendly_error(e: &libmpv2::Error) -> String {
    use libmpv2::mpv_error;
    match e {
        libmpv2::Error::Loadfile { error } => friendly_error(error),
        libmpv2::Error::Raw(code) => match *code {
            mpv_error::LoadingFailed => {
                "Couldn't load this track — YouTube rejected the stream link".to_owned()
            }
            mpv_error::NothingToPlay => "This stream contains no playable audio".to_owned(),
            mpv_error::UnknownFormat => "Unrecognized audio format".to_owned(),
            mpv_error::AoInitFailed => "Couldn't start audio output".to_owned(),
            other => format!("Playback failed (mpv error {other})"),
        },
        other => format!("Playback failed ({other})"),
    }
}

/// The player. Wraps `Arc<Mpv>` (Send+Sync); the event loop runs on a dedicated OS thread and
/// pumps [`PlayerEvent`]s into a channel taken once via [`Player::take_events`].
pub struct Player {
    mpv: Arc<Mpv>,
    events: Option<UnboundedReceiver<PlayerEvent>>,
    /// `(loudness gain dB, pitch semitones)`. mpv's `af` is one global chain, so the two things
    /// that write to it have to be re-applied together: a bare `set_property("af", ...)` from
    /// either one would drop the other's filter.
    af: std::sync::Mutex<AfState>,
    /// The user's volume (0–100, perceptual) and a fade factor on top of it (0.0–1.0, amplitude).
    /// Kept apart so a fade (the sleep timer's) never becomes the user's level: the slider and
    /// the saved volume only ever see the first, and ending a fade puts the second back to 1.
    volume: std::sync::Mutex<(i64, f64)>,
}

impl Player {
    /// Create a player with a disk audio cache under `cache_dir` (the audio-bytes tier, context/14).
    pub fn new(cache_dir: &str) -> Result<Self, Error> {
        // libmpv requires LC_NUMERIC=="C" to parse internal option values; Tauri/GTK's init
        // resets the process locale from the system locale first, which makes mpv_create()
        // return null (ponytail: locale reset only, revisit if other LC_* categories start
        // tripping mpv too).
        unsafe {
            libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr());
        }

        // Mirror the Phase-0 spike: create, then set_property (setting some options during the
        // pre-init phase returns PROPERTY_NOT_FOUND on this mpv build).
        let mpv = Mpv::new()?;
        mpv.set_property("vid", "no")?; // audio only
        mpv.set_property("gapless-audio", "yes")?;
        // If no audio device can be opened, play into nothing rather than failing the file.
        // Default is `no`, which turns "the device went away" — unplugging headphones, a Bluetooth
        // set dropping, a USB DAC pulled — into a failed track: the queue treats it as a dead URL
        // and moves on, so you come back to find it has walked through the rest of the album in
        // silence. With this, playback continues and plugging back in picks it up.
        mpv.set_property("audio-fallback-to-null", "yes")?;
        mpv.set_property("cache", "yes")?;
        mpv.set_property("cache-on-disk", "yes")?;
        mpv.set_property("demuxer-cache-dir", cache_dir)?;
        // The demuxer runs at mpv's browser-sized defaults otherwise: 150 MiB forward and 50 MiB
        // back, per open file, and the gapless lookahead keeps two open across every transition.
        // This is audio only (`vid=no` above), so a whole 5-minute Opus track is about 4 MB and
        // those ceilings only ever reserve headroom nothing uses. 32 MiB forward is several tracks
        // of read-ahead; 8 MiB back is minutes of backward-seek without a refetch.
        mpv.set_property("demuxer-max-bytes", 32 * 1024 * 1024_i64)?;
        mpv.set_property("demuxer-max-back-bytes", 8 * 1024 * 1024_i64)?;
        // Reconnect a stream that drops mid-track instead of ending it. Without these, ffmpeg's
        // http reader treats a reset connection as end of file: the track stops where the network
        // blinked and the queue moves on as if it had finished. `reconnect_streamed` covers
        // googlevideo's responses, which ffmpeg considers non-seekable streams; the delay cap keeps
        // a real outage from retrying forever before the failure is reported.
        mpv.set_property(
            "stream-lavf-o",
            "reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_delay_max=30",
        )?;
        let mpv = Arc::new(mpv);

        let (tx, rx) = unbounded_channel();
        let ev = EventContext::new(mpv.ctx);
        ev.disable_deprecated_events().ok();
        ev.observe_property("time-pos", Format::Double, 0)?;
        ev.observe_property("duration", Format::Double, 1)?;
        ev.observe_property("pause", Format::Flag, 2)?;
        ev.observe_property("idle-active", Format::Flag, 3)?;

        std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || event_loop(ev, tx))
            .expect("spawn mpv event thread");

        Ok(Player {
            mpv,
            events: Some(rx),
            af: std::sync::Mutex::new(AfState::default()),
            volume: std::sync::Mutex::new((100, 1.0)),
        })
    }

    /// Take the event receiver (once).
    pub fn take_events(&mut self) -> Option<UnboundedReceiver<PlayerEvent>> {
        self.events.take()
    }

    /// Load and play a fresh URL, replacing the playlist. context/14.
    pub fn load(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        gain_db: Option<f64>,
    ) -> Result<(), Error> {
        self.apply_headers(headers)?;
        self.set_gain(gain_db)?;
        self.mpv.command("loadfile", &[&quoted(url), "replace"])?;
        Ok(())
    }

    /// Append the next track for a gapless transition (the 1-track lookahead). context/14.
    ///
    /// Note: mpv's `http-header-fields`/`user-agent` are global properties, so appended tracks
    /// inherit the currently-set headers. Phase 1 direct-URL clients need no per-track cookies,
    /// so this is fine; per-track header divergence is a Phase 2+ concern (WEB_REMIX `&pot=`).
    pub fn enqueue(&self, url: &str) -> Result<(), Error> {
        self.mpv.command("loadfile", &[&quoted(url), "append"])?;
        Ok(())
    }

    /// Clear the mpv playlist (e.g. when the user jumps to a new track).
    pub fn clear_playlist(&self) -> Result<(), Error> {
        self.mpv.command("playlist-clear", &[])?;
        Ok(())
    }

    /// True when mpv has nothing loaded (playlist exhausted or the last load failed). The
    /// orchestrator uses this after a track ends/fails to tell "gaplessly advanced into the
    /// lookahead" apart from "stalled — load the next track explicitly".
    pub fn is_idle(&self) -> bool {
        self.mpv.get_property::<bool>("idle-active").unwrap_or(true)
    }

    pub fn play(&self) -> Result<(), Error> {
        self.mpv.set_property("pause", false)?;
        Ok(())
    }

    pub fn pause(&self) -> Result<(), Error> {
        self.mpv.set_property("pause", true)?;
        Ok(())
    }

    pub fn toggle(&self) -> Result<(), Error> {
        self.mpv.command("cycle", &["pause"])?;
        Ok(())
    }

    /// Loop the current file seamlessly (repeat-one). mpv restarts the file at EOF *without*
    /// emitting end-file, so the queue logic upstream never advances while this is on — by design.
    pub fn set_loop_file(&self, on: bool) -> Result<(), Error> {
        self.mpv.set_property("loop-file", if on { "inf" } else { "no" })?;
        Ok(())
    }

    /// Absolute seek in seconds.
    pub fn seek(&self, position_secs: f64) -> Result<(), Error> {
        self.mpv.command("seek", &[&position_secs.to_string(), "absolute"])?;
        Ok(())
    }

    /// Set output volume (0–100). The slider percent is perceptual, not mpv's raw scale:
    /// mpv cubes its `volume` property (gain = (v/100)³), which makes a 10-step drag near
    /// the bottom jump ~18 dB while the same drag near the top moves ~3 dB. Map the percent
    /// onto a 60 dB loudness range instead (see [`perceptual_to_mpv`]), so steps stay roughly
    /// the same size and the bottom of the slider is actually quiet rather than just near-floor.
    pub fn set_volume(&self, volume: i64) -> Result<(), Error> {
        let fade = {
            let mut v = self.volume.lock().unwrap();
            v.0 = volume;
            v.1
        };
        self.mpv.set_property("volume", faded_volume(volume, fade))?;
        Ok(())
    }

    /// Scale the output by `amplitude` (0.0–1.0) without touching the user's volume. 1.0 ends a
    /// fade. Nothing observes mpv's `volume`, so the UI's slider stays where the user left it.
    pub fn set_fade(&self, amplitude: f64) -> Result<(), Error> {
        let volume = {
            let mut v = self.volume.lock().unwrap();
            v.1 = amplitude.clamp(0.0, 1.0);
            v
        };
        self.mpv.set_property("volume", faded_volume(volume.0, volume.1))?;
        Ok(())
    }

    fn apply_headers(&self, headers: &HashMap<String, String>) -> Result<(), Error> {
        // User-Agent has its own mpv property; everything else joins http-header-fields.
        if let Some(ua) = headers.get("User-Agent").or_else(|| headers.get("user-agent")) {
            self.mpv.set_property("user-agent", ua.as_str())?;
        }
        let fields: String = headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("user-agent"))
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(",");
        self.mpv.set_property("http-header-fields", fields.as_str())?;
        Ok(())
    }

    /// Apply a per-track loudness gain (dB) as an mpv `volume` audio filter. context/14. Kept
    /// YouTube-agnostic: the caller computes the gain from `loudnessDb` (see `state::loudness_gain`);
    /// this just applies whatever dB it's handed.
    ///
    /// `af` is a **global** mpv property, not a per-playlist-entry one, so a gaplessly-advanced
    /// track keeps whatever the last [`Self::load`] set. The orchestrator has to call this itself
    /// on a gapless advance or every track after the first plays at the first track's gain.
    // ponytail: set on advance, so the head of a gapless track carries the old gain for the event
    // round-trip (a few ms) and the filter chain reinits mid-stream. If that ever clicks audibly,
    // keep one labelled filter (`af=@gain:lavfi=[volume=0dB]`) and retune it with `af-command`.
    pub fn set_gain(&self, gain_db: Option<f64>) -> Result<(), Error> {
        self.af.lock().unwrap().gain_db = gain_db;
        self.apply_af()
    }

    /// The graphic equalizer. `gains` is one value in dB per band of [`EQ_BANDS`], `preamp` is an
    /// overall trim. `None` turns it off entirely, which is not the same as all-zero gains: zeros
    /// still build ten biquads that the audio has to pass through for no effect.
    pub fn set_equalizer(&self, eq: Option<Equalizer>) -> Result<(), Error> {
        let previous = std::mem::replace(&mut self.af.lock().unwrap().eq, eq);
        if let Err(e) = self.apply_af() {
            // Same rollback as `set_pitch`: mpv rejects the whole chain on a bad filter, loudness
            // gain and pitch included, so put back what was working rather than leave the track
            // playing dry.
            self.af.lock().unwrap().eq = previous;
            let _ = self.apply_af();
            return Err(e);
        }
        Ok(())
    }

    /// Tempo, 0.25–2.0. Pitch is unaffected: `audio-pitch-correction` (mpv's default) time-stretches
    /// rather than resamples, so this is Metrolist's `PlaybackParameters.speed` exactly.
    pub fn set_speed(&self, speed: f64) -> Result<(), Error> {
        self.mpv.set_property("speed", speed.clamp(0.25, 2.0))?;
        Ok(())
    }

    /// Pitch shift in semitones, −12..=12 (one octave either way), via the rubberband filter.
    /// Independent of [`Self::set_speed`]: rubberband takes over the time-stretch mpv would
    /// otherwise do with scaletempo2, and shifts pitch on top of it.
    // ponytail: native `rubberband` only. A libmpv built without librubberband errors out and the
    // command surfaces that to the user; wire the `lavfi=[rubberband=pitch=...]` fallback if a
    // Windows/macOS build ever turns up without it.
    pub fn set_pitch(&self, semitones: i32) -> Result<(), Error> {
        let wanted = semitones.clamp(-12, 12);
        let previous = std::mem::replace(&mut self.af.lock().unwrap().semitones, wanted);
        if let Err(e) = self.apply_af() {
            // No librubberband in this build: mpv rejects the *whole* chain, loudness gain
            // included, so put the old value back rather than leave every later set_gain failing.
            // (mpv never applied the bad chain, so this restores what is already playing.)
            self.af.lock().unwrap().semitones = previous;
            let _ = self.apply_af();
            return Err(if wanted == 0 { e } else { Error::NoPitchFilter });
        }
        Ok(())
    }

    fn apply_af(&self) -> Result<(), Error> {
        let state = self.af.lock().unwrap().clone();
        self.mpv.set_property("af", af_chain(&state).as_str())?;
        Ok(())
    }
}

/// The whole `af` chain: loudness gain, then pitch. Empty when neither is in play, so the default
/// path stays exactly the filterless one it was before pitch existed.
/// The centre frequencies of the ten bands, the ISO octave set every graphic EQ uses. Fixed on
/// purpose: a slider the user can move sideways as well as up is a parametric EQ, which is a
/// different tool and a much larger UI.
pub const EQ_BANDS: [u32; 10] = [31, 62, 125, 250, 500, 1_000, 2_000, 4_000, 8_000, 16_000];

/// Every band's Q. See the note in `af_chain`.
pub const EQ_Q: f64 = 1.41;

/// Ten band gains in dB, plus an overall trim.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Equalizer {
    pub preamp_db: f64,
    pub gains_db: [f64; EQ_BANDS.len()],
}

#[derive(Debug, Clone, Default)]
struct AfState {
    gain_db: Option<f64>,
    semitones: i32,
    eq: Option<Equalizer>,
}

fn af_chain(state: &AfState) -> String {
    let AfState { gain_db, semitones, eq } = state;
    let (gain_db, semitones) = (*gain_db, *semitones);
    let mut chain = Vec::new();
    // Loudness gain and the EQ preamp are the same operation, so they ride in one `volume` rather
    // than two: every filter in the chain is another pass over the samples.
    let preamp = eq.as_ref().map(|e| e.preamp_db).unwrap_or(0.0);
    let total = gain_db.unwrap_or(0.0) + preamp;
    if gain_db.is_some() || preamp != 0.0 {
        chain.push(format!("lavfi=[volume={total}dB]"));
    }
    if let Some(e) = eq {
        for (i, &f) in EQ_BANDS.iter().enumerate() {
            let g = e.gains_db[i];
            // A 0 dB band is a biquad that does nothing; skipping it is free and keeps a mostly
            // flat EQ from costing ten filters.
            if g == 0.0 {
                continue;
            }
            // Q √2 (one octave between the -3 dB points), the width AutoEq computes its
            // fixed-band corrections for (autoeq.rs in the app). Those are ten gains that assume
            // this exact filter shape; at the wider Q 1.0 used before, neighbouring bands overlap
            // more and the same gains overshoot the correction.
            // One lavfi per band rather than one graph with commas in it: `af` splits on commas
            // too, and the existing chain already joins that way.
            chain.push(format!("lavfi=[equalizer=f={f}:t=q:w={EQ_Q}:g={g}]"));
        }
    }
    if semitones != 0 {
        // Semitones → frequency multiplier (equal temperament).
        chain.push(format!(
            "{}=pitch-scale={}",
            pitch_filter(),
            2f64.powf(semitones as f64 / 12.0)
        ));
    }
    chain.join(",")
}

/// Test seam. Set it to reproduce a libmpv built without librubberband: mpv then rejects the whole
/// `af` chain, loudness gain included, which is the failure [`Player::set_pitch`] rolls back from.
/// A machine that has the filter can't reach that path any other way. Not compiled into the app.
#[cfg(test)]
static NO_RUBBERBAND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pitch_filter() -> &'static str {
    #[cfg(test)]
    if NO_RUBBERBAND.load(std::sync::atomic::Ordering::Relaxed) {
        return "rubberband_this_build_does_not_have";
    }
    "rubberband"
}

fn event_loop(mut ev: EventContext, tx: tokio::sync::mpsc::UnboundedSender<PlayerEvent>) {
    // Playback state is derived from two properties, never polled: mpv answers `mpv_get_property`
    // synchronously on its core lock, so asking it from the app's async event pump can stall that
    // pump exactly when mpv is busiest (a gapless transition opening the next stream) — and a
    // stalled pump stops draining mpv's events, so track-end is never handled and playback wedges.
    // These arrive as events; nothing has to ask.
    //
    // mpv reports the initial value of an observed property immediately, so both are seeded here
    // before anything is loaded: `pause: false`, `idle-active: true` ⇒ not playing.
    let mut paused = false;
    let mut idle = true;
    let mut playing = false;
    loop {
        match ev.wait_event(1.0) {
            Some(Ok(event)) => {
                let out = match event {
                    Event::PropertyChange {
                        name: "time-pos",
                        change: PropertyData::Double(p),
                        ..
                    } => Some(PlayerEvent::Position(p)),
                    Event::PropertyChange {
                        name: "duration",
                        change: PropertyData::Double(d),
                        ..
                    } => Some(PlayerEvent::Duration(d)),
                    Event::PropertyChange {
                        name: "pause", change: PropertyData::Flag(p), ..
                    } => {
                        paused = p;
                        None
                    }
                    Event::PropertyChange {
                        name: "idle-active",
                        change: PropertyData::Flag(i),
                        ..
                    } => {
                        idle = i;
                        None
                    }
                    Event::EndFile(reason) => match reason as i32 {
                        EOF => Some(PlayerEvent::TrackEnded),
                        // STOP/QUIT/REDIRECT are deliberate (loadfile replace, shutdown) — ignore.
                        // ERROR never reaches this arm: libmpv2 surfaces end-file-with-error as
                        // Err from wait_event (see below).
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(e) = out {
                    // Receiver dropped ⇒ player gone ⇒ stop the thread.
                    if tx.send(e).is_err() {
                        break;
                    }
                }
                // A gapless advance never touches either property, so no spurious stop/start is
                // emitted between tracks.
                let now = !paused && !idle;
                if now != playing {
                    playing = now;
                    if tx.send(PlayerEvent::Playing(now)).is_err() {
                        break;
                    }
                }
            }
            Some(Err(e)) => {
                // libmpv2 routes MPV_EVENT_END_FILE with an error (dead URL, 403, bad format)
                // through here instead of Event::EndFile — in our usage (no async get/set/command
                // replies) an Err from wait_event *is* a failed track.
                if tx.send(PlayerEvent::TrackFailed(friendly_error(&e))).is_err() {
                    break;
                }
            }
            None => {}
        }
    }
}

/// mpv's `volume` for a perceptual level scaled by an amplitude factor. mpv cubes the property
/// (gain = (v/100)³), so an amplitude `a` is a factor of ∛a on the property.
fn faded_volume(volume: i64, amplitude: f64) -> f64 {
    perceptual_to_mpv(volume) * amplitude.clamp(0.0, 1.0).cbrt()
}

/// Quote a filename/URL for mpv's command parser.
///
/// libmpv2's `command` builds one space-joined string and hands it to `mpv_command_string`, which
/// splits it back apart on whitespace. So `loadfile /music/My music/a, b.mp3 replace` reaches mpv
/// as six arguments and fails with INVALID_PARAMETER (-4) — which is every local file whose path
/// has a space in it. Inside double quotes mpv only treats `\` specially, so escaping those two
/// characters is the whole job (verified against libmpv: quotes, commas, `$` and backslashes all
/// round-trip byte for byte through `playlist/0/filename`).
fn quoted(arg: &str) -> String {
    format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Slider percent → mpv `volume` value, over a 60 dB range. mpv applies gain = (v/100)³,
/// i.e. 60·log10(v/100) dB, so v = 100·10^(−(1−s/100)^1.5) yields −60·(1−s/100)^1.5 dB:
/// 50% is −21 dB, 25% is −39 dB, 1% is −59 dB. 0 stays a hard mute.
///
/// The 1.5 exponent buys the low end its range without moving anyone's saved setting much
/// (the old linear-in-dB curve put 50% at −20 dB, this one at −21). Steps are 0.9 dB at the
/// quiet end and 0.3 dB near the top, which is the right way round: fine control is wanted
/// where a dB is loud, and the bottom of the slider needs to reach somewhere quiet.
fn perceptual_to_mpv(percent: i64) -> f64 {
    if percent <= 0 {
        return 0.0;
    }
    100.0 * 10f64.powf(-(1.0 - percent.min(100) as f64 / 100.0).powf(1.5))
}

#[cfg(test)]
mod tests {
    use super::{af_chain, faded_volume, perceptual_to_mpv, quoted, AfState, Equalizer, EQ_BANDS};

    fn chain(gain_db: Option<f64>, semitones: i32, eq: Option<Equalizer>) -> String {
        af_chain(&AfState { gain_db, semitones, eq })
    }

    #[test]
    fn gain_and_pitch_share_one_chain() {
        // The bug this exists for: either setter clobbering the other's filter.
        assert_eq!(chain(None, 0, None), "");
        assert_eq!(chain(Some(-3.5), 0, None), "lavfi=[volume=-3.5dB]");
        assert_eq!(chain(None, 12, None), "rubberband=pitch-scale=2");
        assert_eq!(chain(Some(-6.0), -12, None), "lavfi=[volume=-6dB],rubberband=pitch-scale=0.5");
        // One semitone up is the twelfth root of two.
        assert!(chain(None, 1, None).ends_with("1.0594630943592953"));
    }

    #[test]
    fn a_flat_equalizer_adds_no_filters() {
        // All-zero is not the same as off in the UI, but it must cost the same: ten biquads doing
        // nothing is ten passes over every sample.
        let flat = Equalizer::default();
        assert_eq!(chain(None, 0, Some(flat)), "");
    }

    #[test]
    fn only_the_bands_that_were_moved_become_filters() {
        let mut eq = Equalizer::default();
        eq.gains_db[0] = 6.0; // 31 Hz
        eq.gains_db[9] = -3.0; // 16 kHz
        let c = chain(None, 0, Some(eq));
        assert_eq!(
            c,
            "lavfi=[equalizer=f=31:t=q:w=1.41:g=6],lavfi=[equalizer=f=16000:t=q:w=1.41:g=-3]"
        );
    }

    #[test]
    fn the_preamp_folds_into_the_loudness_volume() {
        // Two `volume` filters would be two passes for one multiplication.
        let eq = Equalizer { preamp_db: -2.0, ..Default::default() };
        assert_eq!(chain(Some(-4.0), 0, Some(eq.clone())), "lavfi=[volume=-6dB]");
        // And a preamp alone still produces one.
        assert_eq!(chain(None, 0, Some(eq)), "lavfi=[volume=-2dB]");
    }

    #[test]
    fn the_band_list_is_the_iso_octave_set() {
        // The UI draws a slider per entry and the settings row stores one gain per entry, so the
        // length is part of the contract.
        assert_eq!(EQ_BANDS.len(), 10);
        assert_eq!(EQ_BANDS[0], 31);
        assert_eq!(EQ_BANDS[9], 16_000);
    }

    /// Everything above is string-building; this drives a real libmpv and reads `af` back out of
    /// it, because the questions that matter ("is the gain still in the chain", "what does mpv keep
    /// when it rejects a chain") are answered by mpv, not by us. Nothing is played, so no audio
    /// device is opened. One test rather than four: `NO_RUBBERBAND` is process-global and cargo
    /// runs tests in parallel.
    #[test]
    fn mpv_keeps_the_gain_through_pitch_changes_and_failures() {
        use super::{Error, Player, NO_RUBBERBAND};
        use std::sync::atomic::Ordering;

        let dir = std::env::temp_dir().join("limusic-af-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = Player::new(dir.to_str().unwrap()).expect("libmpv");
        let af = || p.mpv.get_property::<String>("af").unwrap();

        // 1. Loudness normalization, then a pitch round trip. The gain has to survive both steps.
        p.set_gain(Some(-7.7)).unwrap();
        assert!(af().contains("volume=-7.7dB"), "gain missing: {}", af());
        p.set_pitch(2).unwrap();
        assert!(af().contains("volume=-7.7dB"), "pitch dropped the gain: {}", af());
        assert!(af().contains("rubberband"), "pitch missing: {}", af());
        p.set_pitch(0).unwrap();
        assert!(af().contains("volume=-7.7dB"), "reset dropped the gain: {}", af());
        assert!(!af().contains("rubberband"), "pitch 0 left a filter behind: {}", af());

        // 2. Gapless advance: the orchestrator retunes the gain for the next track (state.rs, the
        // `lookahead_gain` take). A pitch the user set must not fall out of the chain when it does.
        p.set_pitch(-5).unwrap();
        p.set_gain(Some(-2.5)).unwrap();
        assert!(af().contains("volume=-2.5dB"), "retune missed: {}", af());
        assert!(af().contains("rubberband"), "retune dropped the pitch: {}", af());
        p.set_pitch(0).unwrap();

        // 3. A libmpv without librubberband. mpv rejects the chain wholesale, so this is also the
        // case where loudness normalization could silently disappear.
        let before = af();
        NO_RUBBERBAND.store(true, Ordering::Relaxed);
        let err = p.set_pitch(3).unwrap_err();
        NO_RUBBERBAND.store(false, Ordering::Relaxed);
        // The user is told, in words. mpv's own answer is `Raw(-9)`, which says nothing.
        assert!(matches!(err, Error::NoPitchFilter), "rejection must surface: {err}");
        assert_eq!(err.to_string(), "Pitch shifting isn't available in this build");
        // mpv never applied the bad chain, and the rollback re-applied the good one either way.
        assert_eq!(af(), before, "a rejected pitch changed the live chain");
        assert!(af().contains("volume=-2.5dB"), "normalization lost: {}", af());
        // And the rolled-back state is clean: the next per-track retune is gain-only, not a
        // permanently poisoned chain that fails from here on.
        p.set_gain(Some(-4.0)).unwrap();
        let after = af(); // mpv hands the chain back in its own escaped form, hence `contains`
        assert!(after.contains("volume=-4dB"), "retune after a rejection failed: {after}");
        assert!(!after.contains("rubberband"), "stored pitch survived the rollback: {after}");

        // 4. An AutoEq correction: fractional gains, a preamp below the bands' −12 dB, Q 1.41.
        // mpv has to take every band of it, or the headphone picker applies a chain that fails.
        let hd600 = Equalizer {
            preamp_db: -12.5,
            gains_db: [6.9, 3.3, -1.1, -1.6, 0.6, -0.8, 0.1, -1.0, 3.9, -6.5],
        };
        p.set_equalizer(Some(hd600)).unwrap();
        let live = af();
        assert!(live.contains("volume=-16.5dB"), "preamp not folded into the gain: {live}");
        assert_eq!(live.matches("equalizer").count(), 10, "a band was dropped: {live}");
        assert!(live.contains("f=31:t=q:w=1.41:g=6.9"), "31 Hz band wrong: {live}");
        assert!(live.contains("f=16000:t=q:w=1.41:g=-6.5"), "16 kHz band wrong: {live}");
        p.set_equalizer(None).unwrap();
        assert!(!af().contains("equalizer"), "turning it off left filters: {}", af());

        // 5. The reconnect options are accepted, and a fade never becomes the user's volume.
        let lavf = p.mpv.get_property::<String>("stream-lavf-o").unwrap();
        assert!(lavf.contains("reconnect_streamed=1"), "reconnect options missing: {lavf}");
        let vol = || p.mpv.get_property::<f64>("volume").unwrap();
        p.set_volume(60).unwrap();
        let user = vol();
        p.set_fade(0.0).unwrap();
        assert_eq!(vol(), 0.0);
        p.set_volume(70).unwrap(); // a volume change mid-fade stays faded
        assert_eq!(vol(), 0.0);
        p.set_fade(1.0).unwrap();
        assert!(vol() > user, "ending the fade should land on the new user volume");
    }

    #[test]
    fn paths_survive_mpvs_command_parser() {
        // The bug this exists for: a space used to end the argument.
        assert_eq!(quoted("/music/My music/a, b.mp3"), "\"/music/My music/a, b.mp3\"");
        // Only backslash and double quote mean anything inside the quotes.
        assert_eq!(quoted(r#"/m/say "hi".mp3"#), r#""/m/say \"hi\".mp3""#);
        assert_eq!(quoted(r"C:\Music\x.mp3"), r#""C:\\Music\\x.mp3""#);
        // A stream URL is unchanged apart from the wrapper.
        assert_eq!(quoted("https://x/y?a=1&b=2"), "\"https://x/y?a=1&b=2\"");
    }

    #[test]
    fn a_fade_scales_amplitude_not_the_property() {
        // mpv cubes `volume`, so half the amplitude is ∛0.5 of the property, not half of it.
        let full = perceptual_to_mpv(80);
        assert_eq!(faded_volume(80, 1.0), full);
        assert_eq!(faded_volume(80, 0.0), 0.0);
        let half = faded_volume(80, 0.5);
        assert!(((half / 100.0).powi(3) / (full / 100.0).powi(3) - 0.5).abs() < 1e-9);
        // Out of range factors clamp rather than boost or go negative.
        assert_eq!(faded_volume(80, 2.0), full);
        assert_eq!(faded_volume(80, -1.0), 0.0);
    }

    #[test]
    fn volume_curve() {
        let db = |s| 60.0 * (perceptual_to_mpv(s) / 100.0).log10();
        assert_eq!(perceptual_to_mpv(0), 0.0); // hard mute, not just very quiet
        assert_eq!(perceptual_to_mpv(100), 100.0);
        assert!((db(50) + 21.21).abs() < 0.01);
        // The point of the curve: 1% has somewhere to go. The old 40 dB range bottomed out
        // here, which left anyone listening quietly pinned to the floor.
        assert!((db(1) + 59.10).abs() < 0.01);
        // Monotonic, and finer steps at the loud end than the quiet one.
        assert!((1..=100).all(|s| perceptual_to_mpv(s) > perceptual_to_mpv(s - 1)));
        assert!(db(100) - db(99) < db(2) - db(1));
    }
}
