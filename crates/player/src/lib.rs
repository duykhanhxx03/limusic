//! libmpv wrapper. context/14. YouTube-agnostic: takes a fully-resolved URL + headers, never
//! a videoId. Gapless via mpv's internal playlist (1-track lookahead fed by the orchestrator), or a
//! crossfade across two mpv instances when one is set ([`Crossfade`]).

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use libmpv2::events::{Event, EventContext, PropertyData};
use libmpv2::{Format, Mpv};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

mod silence;

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

/// How two tracks meet. `secs` 0 is off: the next track follows gaplessly through mpv's own
/// playlist, exactly as before crossfading existed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Crossfade {
    /// How long the outgoing and incoming tracks overlap.
    pub secs: f64,
    /// DJ-style: the outgoing track closes down through a low-pass while the incoming one opens up
    /// through a high-pass, so for most of the overlap the two sit at different ends of the
    /// spectrum instead of two basslines and two vocals on top of each other.
    pub filters: bool,
}

impl Crossfade {
    fn on(&self) -> bool {
        self.secs > 0.0
    }

    fn sweeps(&self) -> bool {
        self.on() && self.filters
    }
}

/// With less than this left of the outgoing track, an overlap isn't worth starting: the next track
/// goes to mpv's playlist and follows gaplessly instead. Happens when its stream resolved late.
const MIN_OVERLAP_SECS: f64 = 0.5;

/// How long before its overlap the next track is loaded on the other deck, paused and silent.
/// Opening a stream takes up to a second or so; loaded ahead, the track comes in on the beat of the
/// ramp instead of partway up it.
const CUE_AHEAD_SECS: f64 = 5.0;

/// Leading silence shorter than this isn't worth a seek.
const MIN_LEAD_SECS: f64 = 0.1;

/// How many tracks' silence is remembered: the playing one, the next, and a few either side.
const SILENCE_MEMORY: usize = 32;

/// Where the sweeps start and end. The low-pass closes down to the bassline; the high-pass opens
/// from hi-hats and voice down to below anything audible. Both move exponentially: pitch is heard
/// on a log scale, and a linear sweep would spend most of the overlap in the top octave.
const LOW_FROM_HZ: f64 = 20_000.0;
const LOW_TO_HZ: f64 = 200.0;
const HIGH_FROM_HZ: f64 = 1_000.0;
const HIGH_TO_HZ: f64 = 20.0;
/// The low-pass corner while it is switched out (`mix` 0), where it makes no difference to the
/// sound. Chosen to be valid at any sample rate a music file has: a biquad asked for a corner above
/// the Nyquist frequency is rejected, and retuning one that was rejected crashes libavfilter
/// (reproduced with an 8 kHz file). For the same reason every retune stays below
/// `NYQUIST_MARGIN` of the deck's own sample rate.
const LOW_REST_HZ: f64 = 3_000.0;
const NYQUIST_MARGIN: f64 = 0.45;

/// One deck's two sweep filters. `mix` 0 passes the input through untouched (ffmpeg's biquads
/// output `filtered·mix + input·(1−mix)`), which is how both sit in the chain outside an overlap:
/// always there, so that starting one only retunes them with `af-command`. Adding them to the
/// chain of a track that is playing would rebuild the chain mid-stream, which can click.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sweep {
    low_hz: f64,
    low_mix: f64,
    high_hz: f64,
    high_mix: f64,
}

impl Sweep {
    const FLAT: Sweep =
        Sweep { low_hz: LOW_REST_HZ, low_mix: 0.0, high_hz: HIGH_TO_HZ, high_mix: 0.0 };

    /// The outgoing track's, `p` (0–1) of the way through the overlap.
    fn closing(p: f64) -> Sweep {
        Sweep { low_hz: glide(LOW_FROM_HZ, LOW_TO_HZ, p), low_mix: 1.0, ..Self::FLAT }
    }

    /// The incoming track's.
    fn opening(p: f64) -> Sweep {
        Sweep { high_hz: glide(HIGH_FROM_HZ, HIGH_TO_HZ, p), high_mix: 1.0, ..Self::FLAT }
    }

    fn filters(&self) -> [String; 2] {
        [
            format!("@{LOW_LABEL}:lavfi=[lowpass=f={:.0}:m={}]", self.low_hz, self.low_mix),
            format!("@{HIGH_LABEL}:lavfi=[highpass=f={:.0}:m={}]", self.high_hz, self.high_mix),
        ]
    }
}

const LOW_LABEL: &str = "xflow";
const HIGH_LABEL: &str = "xfhigh";

/// `from` to `to` exponentially, `p` of the way.
fn glide(from: f64, to: f64, p: f64) -> f64 {
    from * (to / from).powf(p.clamp(0.0, 1.0))
}

/// Equal-power levels `(outgoing, incoming)` at `p` of the way through an overlap: a quarter
/// cosine and a quarter sine, whose squares sum to one. A linear crossfade dips by 3 dB in the
/// middle, which two unrelated tracks make audible.
fn overlap_levels(p: f64) -> (f64, f64) {
    let a = p.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2;
    (a.cos(), a.sin())
}

/// The player. Two mpv instances ("decks") behind one face: the app sees one position, one
/// duration and one stream of [`PlayerEvent`]s, always the active deck's. The second deck only
/// plays during a crossfade: the next track is cued on it, paused, shortly before the current one
/// nears its end, then starts while the current one fades out; after that the decks have swapped
/// roles. With crossfading off it never loads anything.
///
/// Each deck's event loop runs on its own OS thread and feeds [`Mix::on_event`], which decides
/// what reaches the channel taken once via [`Player::take_events`].
pub struct Player {
    mix: Arc<Mix>,
    events: Option<UnboundedReceiver<PlayerEvent>>,
}

struct Mix {
    decks: [Arc<Mpv>; 2],
    state: std::sync::Mutex<MixState>,
    tx: UnboundedSender<PlayerEvent>,
    /// Measures the silence at a track's ends (`silence.rs`).
    probe: std::sync::mpsc::Sender<silence::Job>,
}

/// The silence at a track's two ends, as far as it has been measured.
#[derive(Debug, Default, Clone, Copy)]
struct Silence {
    head: Measure,
    tail: Measure,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Measure {
    #[default]
    Unasked,
    Pending,
    Known(f64),
    Unknown,
}

impl Silence {
    fn at(&mut self, end: silence::End) -> &mut Measure {
        match end {
            silence::End::Head => &mut self.head,
            silence::End::Tail => &mut self.tail,
        }
    }
}

impl Measure {
    fn secs(self) -> Option<f64> {
        match self {
            Measure::Known(s) => Some(s),
            _ => None,
        }
    }
}

/// The next track, held back from mpv's playlist so it can start early on the other deck.
struct Next {
    url: String,
    gain_db: Option<f64>,
}

/// An overlap in progress. It is driven by the outgoing deck's own clock, so it pauses with the
/// music, follows the playback speed, and ends exactly as that track does.
struct Overlap {
    /// The outgoing deck.
    from: usize,
    /// Its position when the overlap began, and how long the overlap runs.
    start: f64,
    len: f64,
}

struct MixState {
    /// The deck the app is talking to: its position, its duration, its track.
    active: usize,
    /// `(loudness gain dB per deck, pitch semitones, equalizer)`. mpv's `af` is one global chain
    /// per deck, so everything that writes to it has to be re-applied together: a bare
    /// `set_property("af", ...)` from any one of them would drop the others' filters.
    gain: [Option<f64>; 2],
    semitones: i32,
    eq: Option<Equalizer>,
    sweep: [Sweep; 2],
    /// What each deck's chain was last built from, sweeps aside (those are retuned in place). A
    /// retune that changes nothing is skipped: re-setting `af` rebuilds the chain mid-stream.
    chain_key: [String; 2],
    /// The user's volume (0–100, perceptual) and a fade factor on top of it (0.0–1.0, amplitude).
    /// Kept apart so a fade (the sleep timer's) never becomes the user's level: the slider and the
    /// saved volume only ever see the first, and ending a fade puts the second back to 1.
    volume: i64,
    fade: f64,
    /// Each deck's level in an overlap (1 outside one).
    level: [f64; 2],
    loop_file: bool,
    crossfade: Crossfade,
    next: Option<Next>,
    /// The deck holding the next track, loaded and paused, waiting for its overlap.
    cued: Option<usize>,
    overlap: Option<Overlap>,
    /// A deck the mix itself started a track on, until that track starts. mpv is briefly idle in
    /// between, and the app asking [`Player::is_idle`] right then must not read it as a stall.
    handoff: Option<usize>,
    duration: [f64; 2],
    /// Each deck's sample rate (0 until known), which bounds the sweeps' corners.
    rate: [f64; 2],
    /// Each deck's file, as mpv reports it.
    path: [Option<String>; 2],
    /// Where to seek a deck to once its file has loaded: past the silence a cued track opens on.
    /// mpv refuses a seek on a deck that has no file yet, which a deck being cued doesn't.
    lead_in: [Option<f64>; 2],
    /// Silence at the ends of recent tracks, by URL, for the crossfade to skip.
    silence: HashMap<String, Silence>,
    /// The headers last applied, for the silence probe's own fetches.
    user_agent: Option<String>,
    header_fields: String,
    paused: [bool; 2],
    idle: [bool; 2],
    playing: bool,
}

/// What a deck's event loop reports to the mix.
enum DeckEvent {
    Position(f64),
    Duration(f64),
    Rate(i64),
    Path(String),
    Loaded,
    Paused(bool),
    Idle(bool),
    Started,
    Ended,
    Failed(String),
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

        let decks = [Arc::new(new_deck(cache_dir)?), Arc::new(new_deck(cache_dir)?)];
        let (tx, rx) = unbounded_channel();
        let pcm = std::path::Path::new(cache_dir).join("crossfade-probe.pcm");
        let mix = Arc::new_cyclic(|mix: &Weak<Mix>| Mix {
            probe: {
                let mix = mix.clone();
                silence::spawn(pcm, move |m| {
                    if let Some(mix) = mix.upgrade() {
                        mix.measured(m);
                    }
                })
            },
            decks,
            state: std::sync::Mutex::new(MixState {
                active: 0,
                gain: [None; 2],
                semitones: 0,
                eq: None,
                sweep: [Sweep::FLAT; 2],
                chain_key: Default::default(),
                volume: 100,
                fade: 1.0,
                level: [1.0; 2],
                loop_file: false,
                crossfade: Crossfade::default(),
                next: None,
                cued: None,
                overlap: None,
                handoff: None,
                duration: [0.0; 2],
                rate: [0.0; 2],
                path: [None, None],
                lead_in: [None, None],
                silence: HashMap::new(),
                user_agent: None,
                header_fields: String::new(),
                // mpv reports the initial value of an observed property immediately; these are
                // what it will say before anything is loaded: `pause: false`, `idle-active: true`.
                paused: [false; 2],
                idle: [true; 2],
                playing: false,
            }),
            tx,
        });
        for deck in 0..2 {
            let ev = EventContext::new(mix.decks[deck].ctx);
            ev.disable_deprecated_events().ok();
            ev.observe_property("time-pos", Format::Double, 0)?;
            ev.observe_property("duration", Format::Double, 1)?;
            ev.observe_property("pause", Format::Flag, 2)?;
            ev.observe_property("idle-active", Format::Flag, 3)?;
            ev.observe_property("audio-params/samplerate", Format::Int64, 4)?;
            ev.observe_property("path", Format::String, 5)?;
            let mix = mix.clone();
            std::thread::Builder::new()
                .name(format!("mpv-events-{deck}"))
                .spawn(move || event_loop(ev, deck, mix))
                .expect("spawn mpv event thread");
        }
        Ok(Player { mix, events: Some(rx) })
    }

    /// Take the event receiver (once).
    pub fn take_events(&mut self) -> Option<UnboundedReceiver<PlayerEvent>> {
        self.events.take()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MixState> {
        self.mix.state.lock().unwrap()
    }

    /// Load and play a fresh URL, replacing the playlist. context/14. Cuts an overlap short: the
    /// user picked something, and the outgoing track has no business carrying on under it.
    pub fn load(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        gain_db: Option<f64>,
    ) -> Result<(), Error> {
        let mut s = self.lock();
        self.mix.end_overlap(&mut s);
        self.mix.uncue(&mut s);
        s.next = None;
        s.handoff = None;
        let d = s.active;
        s.lead_in[d] = None;
        self.mix.apply_headers(&mut s, headers)?;
        s.gain[d] = gain_db;
        s.sweep[d] = Sweep::FLAT;
        s.level[d] = 1.0;
        self.mix.apply_af(&mut s, d, true)?;
        self.mix.apply_volume(&s, d)?;
        self.mix.decks[d].command("loadfile", &[&quoted(url), "replace"])?;
        Ok(())
    }

    /// Hand over the next track (the 1-track lookahead). context/14. With a crossfade set and
    /// `blend` true, it is held here and started on the other deck as the current track nears its
    /// end, at its own loudness `gain_db`. Otherwise it joins mpv's playlist for a gapless
    /// transition, and the orchestrator applies the gain when the track ends, as before.
    ///
    /// Note: mpv's `http-header-fields`/`user-agent` are global properties, so appended tracks
    /// inherit the currently-set headers. Phase 1 direct-URL clients need no per-track cookies,
    /// so this is fine; per-track header divergence is a Phase 2+ concern (WEB_REMIX `&pot=`).
    pub fn enqueue(&self, url: &str, gain_db: Option<f64>, blend: bool) -> Result<(), Error> {
        let mut s = self.lock();
        if blend && s.crossfade.on() {
            s.next = Some(Next { url: url.to_owned(), gain_db });
            self.mix.ask(&mut s, url, silence::End::Head);
            self.mix.ask(&mut s, url, silence::End::Tail);
            return Ok(());
        }
        s.next = None;
        self.mix.decks[s.active].command("loadfile", &[&quoted(url), "append"])?;
        Ok(())
    }

    /// Drop the next track (e.g. when the user jumps to a new track or the queue changes).
    pub fn clear_playlist(&self) -> Result<(), Error> {
        let mut s = self.lock();
        s.next = None;
        self.mix.uncue(&mut s);
        self.mix.decks[s.active].command("playlist-clear", &[])?;
        Ok(())
    }

    /// True when nothing is loaded (playlist exhausted or the last load failed). The orchestrator
    /// uses this after a track ends/fails to tell "advanced into the lookahead" apart from
    /// "stalled — load the next track explicitly". An overlap is an advance: the incoming track
    /// is starting, and the outgoing one is still playing.
    pub fn is_idle(&self) -> bool {
        let s = self.lock();
        if s.overlap.is_some() || s.handoff.is_some() {
            return false;
        }
        self.mix.decks[s.active].get_property::<bool>("idle-active").unwrap_or(true)
    }

    pub fn play(&self) -> Result<(), Error> {
        self.mix.set_paused(&self.lock(), false)
    }

    pub fn pause(&self) -> Result<(), Error> {
        self.mix.set_paused(&self.lock(), true)
    }

    pub fn toggle(&self) -> Result<(), Error> {
        let s = self.lock();
        let paused = self.mix.decks[s.active].get_property::<bool>("pause")?;
        self.mix.set_paused(&s, !paused)
    }

    /// Loop the current file seamlessly (repeat-one). mpv restarts the file at EOF *without*
    /// emitting end-file, so the queue logic upstream never advances while this is on — by design.
    /// Only the active deck loops: an outgoing track that looped would never finish fading out.
    pub fn set_loop_file(&self, on: bool) -> Result<(), Error> {
        let mut s = self.lock();
        s.loop_file = on;
        self.mix.decks[s.active].set_property("loop-file", if on { "inf" } else { "no" })?;
        Ok(())
    }

    /// Absolute seek in seconds, in the active track. An overlap carries on: its clock is the
    /// outgoing track's, which a seek in the new one doesn't touch.
    pub fn seek(&self, position_secs: f64) -> Result<(), Error> {
        let s = self.lock();
        self.mix.decks[s.active].command("seek", &[&position_secs.to_string(), "absolute"])?;
        Ok(())
    }

    /// Set output volume (0–100). The slider percent is perceptual, not mpv's raw scale:
    /// mpv cubes its `volume` property (gain = (v/100)³), which makes a 10-step drag near
    /// the bottom jump ~18 dB while the same drag near the top moves ~3 dB. Map the percent
    /// onto a 60 dB loudness range instead (see [`perceptual_to_mpv`]), so steps stay roughly
    /// the same size and the bottom of the slider is actually quiet rather than just near-floor.
    pub fn set_volume(&self, volume: i64) -> Result<(), Error> {
        let mut s = self.lock();
        s.volume = volume;
        self.mix.apply_volumes(&s)
    }

    /// Scale the output by `amplitude` (0.0–1.0) without touching the user's volume. 1.0 ends a
    /// fade. Nothing observes mpv's `volume`, so the UI's slider stays where the user left it.
    pub fn set_fade(&self, amplitude: f64) -> Result<(), Error> {
        let mut s = self.lock();
        s.fade = amplitude.clamp(0.0, 1.0);
        self.mix.apply_volumes(&s)
    }

    /// Apply a per-track loudness gain (dB) as an mpv `volume` audio filter. context/14. Kept
    /// YouTube-agnostic: the caller computes the gain from `loudnessDb` (see `state::loudness_gain`);
    /// this just applies whatever dB it's handed, to the active deck.
    ///
    /// `af` is a **global** mpv property, not a per-playlist-entry one, so a gaplessly-advanced
    /// track keeps whatever the last [`Self::load`] set. The orchestrator has to call this itself
    /// on a gapless advance or every track after the first plays at the first track's gain. (A
    /// track that came in through a crossfade already has its own; the same value again is a no-op.)
    // ponytail: set on advance, so the head of a gapless track carries the old gain for the event
    // round-trip (a few ms) and the filter chain reinits mid-stream. If that ever clicks audibly,
    // keep one labelled filter (`af=@gain:lavfi=[volume=0dB]`) and retune it with `af-command`.
    pub fn set_gain(&self, gain_db: Option<f64>) -> Result<(), Error> {
        let mut s = self.lock();
        let d = s.active;
        s.gain[d] = gain_db;
        self.mix.apply_af(&mut s, d, false)
    }

    /// The graphic equalizer. `gains` is one value in dB per band of [`EQ_BANDS`], `preamp` is an
    /// overall trim. `None` turns it off entirely, which is not the same as all-zero gains: zeros
    /// still build ten biquads that the audio has to pass through for no effect.
    pub fn set_equalizer(&self, eq: Option<Equalizer>) -> Result<(), Error> {
        let mut s = self.lock();
        let previous = std::mem::replace(&mut s.eq, eq);
        if let Err(e) = self.mix.apply_af_all(&mut s) {
            // Same rollback as `set_pitch`: mpv rejects the whole chain on a bad filter, loudness
            // gain and pitch included, so put back what was working rather than leave the track
            // playing dry.
            s.eq = previous;
            let _ = self.mix.apply_af_all(&mut s);
            return Err(e);
        }
        Ok(())
    }

    /// Tempo, 0.25–2.0. Pitch is unaffected: `audio-pitch-correction` (mpv's default) time-stretches
    /// rather than resamples, so this is Metrolist's `PlaybackParameters.speed` exactly.
    pub fn set_speed(&self, speed: f64) -> Result<(), Error> {
        self.mix.set_all("speed", speed.clamp(0.25, 2.0))
    }

    /// Pitch shift in semitones, −12..=12 (one octave either way), via the rubberband filter.
    /// Independent of [`Self::set_speed`]: rubberband takes over the time-stretch mpv would
    /// otherwise do with scaletempo2, and shifts pitch on top of it.
    // ponytail: native `rubberband` only. A libmpv built without librubberband errors out and the
    // command surfaces that to the user; wire the `lavfi=[rubberband=pitch=...]` fallback if a
    // Windows/macOS build ever turns up without it.
    pub fn set_pitch(&self, semitones: i32) -> Result<(), Error> {
        let wanted = semitones.clamp(-12, 12);
        let mut s = self.lock();
        let previous = std::mem::replace(&mut s.semitones, wanted);
        if let Err(e) = self.mix.apply_af_all(&mut s) {
            // No librubberband in this build: mpv rejects the *whole* chain, loudness gain
            // included, so put the old value back rather than leave every later set_gain failing.
            // (mpv never applied the bad chain, so this restores what is already playing.)
            s.semitones = previous;
            let _ = self.mix.apply_af_all(&mut s);
            return Err(if wanted == 0 { e } else { Error::NoPitchFilter });
        }
        Ok(())
    }

    /// How tracks meet from here on. Turning it off hands a held next track to mpv's playlist, so
    /// it still follows gaplessly; an overlap already running finishes, and a track already cued
    /// starts when the current one ends.
    pub fn set_crossfade(&self, crossfade: Crossfade) -> Result<(), Error> {
        let mut s = self.lock();
        s.crossfade = Crossfade { secs: crossfade.secs.max(0.0), ..crossfade };
        if !s.crossfade.on() {
            if let Some(next) = s.next.take() {
                self.mix.decks[s.active].command("loadfile", &[&quoted(&next.url), "append"])?;
            }
        }
        // Switched on mid-track: the playing track's end and the next one's start still have time
        // to be measured.
        if let Some(url) = s.path[s.active].clone() {
            self.mix.ask(&mut s, &url, silence::End::Tail);
        }
        if let Some(url) = s.next.as_ref().map(|n| n.url.clone()) {
            self.mix.ask(&mut s, &url, silence::End::Head);
            self.mix.ask(&mut s, &url, silence::End::Tail);
        }
        // The sweep filters join or leave both chains.
        self.mix.apply_af_all(&mut s)
    }
}

/// One mpv instance, configured for audio.
fn new_deck(cache_dir: &str) -> Result<Mpv, Error> {
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
    // Tests play real audio through the mix, and must not do it out of the speakers.
    #[cfg(test)]
    mpv.set_property("ao", "null")?;
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
    Ok(mpv)
}

impl Mix {
    fn set_all<T: libmpv2::SetData + Copy>(&self, name: &str, value: T) -> Result<(), Error> {
        for deck in &self.decks {
            deck.set_property(name, value)?;
        }
        Ok(())
    }

    /// Pause or resume the music: every deck but a cued one, which waits for its overlap.
    fn set_paused(&self, s: &MixState, paused: bool) -> Result<(), Error> {
        for (d, deck) in self.decks.iter().enumerate() {
            if s.cued != Some(d) {
                deck.set_property("pause", paused)?;
            }
        }
        Ok(())
    }

    fn apply_headers(
        &self,
        s: &mut MixState,
        headers: &HashMap<String, String>,
    ) -> Result<(), Error> {
        // User-Agent has its own mpv property; everything else joins http-header-fields.
        let fields: String = headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("user-agent"))
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(",");
        let ua = headers.get("User-Agent").or_else(|| headers.get("user-agent"));
        for deck in &self.decks {
            if let Some(ua) = ua {
                deck.set_property("user-agent", ua.as_str())?;
            }
            deck.set_property("http-header-fields", fields.as_str())?;
        }
        if ua.is_some() {
            s.user_agent = ua.cloned();
        }
        s.header_fields = fields;
        Ok(())
    }

    /// Have the silence at one end of `url` measured, unless it has been or is being. Only while
    /// crossfading: nothing else uses it.
    fn ask(&self, s: &mut MixState, url: &str, end: silence::End) {
        if !s.crossfade.on() {
            return;
        }
        if s.silence.len() >= SILENCE_MEMORY && !s.silence.contains_key(url) {
            s.silence.clear();
        }
        let measure = s.silence.entry(url.to_owned()).or_default().at(end);
        if *measure != Measure::Unasked {
            return;
        }
        *measure = Measure::Pending;
        let job = silence::Job {
            url: url.to_owned(),
            end,
            user_agent: s.user_agent.clone(),
            header_fields: s.header_fields.clone(),
        };
        let _ = self.probe.send(job);
    }

    fn measured(&self, m: silence::Measured) {
        let mut s = self.state.lock().unwrap();
        if let Some(entry) = s.silence.get_mut(&m.url) {
            *entry.at(m.end) = m.secs.map_or(Measure::Unknown, Measure::Known);
        }
    }

    /// Measured silence at one end of `url`.
    fn silence_at(s: &MixState, url: Option<&str>, end: silence::End) -> Option<f64> {
        let mut entry = *s.silence.get(url?)?;
        entry.at(end).secs()
    }

    fn apply_volume(&self, s: &MixState, d: usize) -> Result<(), Error> {
        self.decks[d].set_property("volume", faded_volume(s.volume, s.fade * s.level[d]))?;
        Ok(())
    }

    fn apply_volumes(&self, s: &MixState) -> Result<(), Error> {
        (0..2).try_for_each(|d| self.apply_volume(s, d))
    }

    /// Build deck `d`'s chain from the state and hand it to mpv, unless it would be the chain the
    /// deck already has (`force` hands it over regardless, for a deck about to load a track).
    fn apply_af(&self, s: &mut MixState, d: usize, force: bool) -> Result<(), Error> {
        let base =
            af_chain(&AfState { gain_db: s.gain[d], semitones: s.semitones, eq: s.eq.clone() });
        let key = format!("{base}|{}", s.crossfade.sweeps());
        if !force && s.chain_key[d] == key {
            return Ok(());
        }
        let mut chain: Vec<String> = Vec::new();
        if !base.is_empty() {
            chain.push(base);
        }
        if s.crossfade.sweeps() {
            chain.extend(s.sweep[d].filters());
        }
        self.decks[d].set_property("af", chain.join(",").as_str())?;
        s.chain_key[d] = key;
        Ok(())
    }

    fn apply_af_all(&self, s: &mut MixState) -> Result<(), Error> {
        (0..2).try_for_each(|d| self.apply_af(s, d, false))
    }

    /// Retune deck `d`'s sweep filters in place. Corners stay below the deck's Nyquist frequency
    /// (see `LOW_REST_HZ`), and with its sample rate not known yet nothing is sent at all.
    fn retune(&self, s: &mut MixState, d: usize, sweep: Sweep) {
        s.sweep[d] = sweep;
        let limit = s.rate[d] * NYQUIST_MARGIN;
        if !s.crossfade.sweeps() || limit < HIGH_FROM_HZ {
            return;
        }
        let deck = &self.decks[d];
        let hz = |f: f64| format!("{:.0}", f.min(limit));
        // Corner before mix, so a filter being switched in is already where it should be. Errors
        // are ignored: a deck between tracks has no chain to talk to, and the next chain it builds
        // starts from `s.sweep`.
        let _ = deck.command("af-command", &[LOW_LABEL, "f", &hz(sweep.low_hz)]);
        let _ = deck.command("af-command", &[LOW_LABEL, "m", &sweep.low_mix.to_string()]);
        let _ = deck.command("af-command", &[HIGH_LABEL, "f", &hz(sweep.high_hz)]);
        let _ = deck.command("af-command", &[HIGH_LABEL, "m", &sweep.high_mix.to_string()]);
    }

    /// One event from `deck`, turned into what the app hears. Returns false once nobody listens.
    fn on_event(&self, deck: usize, ev: DeckEvent) -> bool {
        let mut s = self.state.lock().unwrap();
        let mut out = Vec::new();
        let outgoing = s.overlap.as_ref().is_some_and(|o| o.from == deck);
        match ev {
            DeckEvent::Position(p) if outgoing => self.step_overlap(&mut s, p),
            DeckEvent::Position(p) if deck == s.active => {
                out.push(PlayerEvent::Position(p));
                self.maybe_overlap(&mut s, deck, p, &mut out);
            }
            DeckEvent::Duration(d) => {
                s.duration[deck] = d;
                if deck == s.active {
                    out.push(PlayerEvent::Duration(d));
                }
            }
            DeckEvent::Rate(r) => s.rate[deck] = r as f64,
            DeckEvent::Path(path) => {
                // A new track on the deck that is playing: where its music ends decides when the
                // crossfade out of it starts.
                if deck == s.active {
                    self.ask(&mut s, &path, silence::End::Tail);
                }
                s.path[deck] = Some(path);
            }
            DeckEvent::Paused(p) => s.paused[deck] = p,
            DeckEvent::Idle(i) => s.idle[deck] = i,
            DeckEvent::Started if s.handoff == Some(deck) => s.handoff = None,
            DeckEvent::Loaded => {
                if let Some(lead) = s.lead_in[deck].take() {
                    let _ = self.decks[deck].command("seek", &[&lead.to_string(), "absolute"]);
                }
            }
            DeckEvent::Ended if outgoing => self.end_overlap(&mut s),
            DeckEvent::Ended if deck == s.active => {
                // The track ended before its overlap could start (its length was off, or the next
                // one resolved in the last moments): the next track starts now, at full level.
                if s.cued.is_some() {
                    self.start_cued(&mut s, deck, None, &mut out);
                } else {
                    if let Some(next) = s.next.take() {
                        self.cut_to(&mut s, deck, next);
                    }
                    out.push(PlayerEvent::TrackEnded);
                }
            }
            DeckEvent::Failed(msg) => {
                if s.handoff == Some(deck) {
                    s.handoff = None;
                }
                if outgoing {
                    self.end_overlap(&mut s);
                } else if s.cued == Some(deck) {
                    // The next track's stream is dead. Nothing is cued any more, so when the
                    // current track ends the app finds the player idle and loads the next one
                    // itself, which re-resolves it.
                    s.cued = None;
                    tracing::warn!(error = %msg, "crossfade: the cued track failed to load");
                } else if deck == s.active {
                    out.push(PlayerEvent::TrackFailed(msg));
                }
            }
            // The other deck, between tracks, or a start nobody is waiting on.
            _ => {}
        }
        // Playing is either deck playing: through an overlap both are, and the handoff between
        // them must not read as a stop and a start. A cued deck is paused, so it doesn't count.
        let now = (0..2).any(|d| !s.paused[d] && !s.idle[d]);
        if now != s.playing {
            s.playing = now;
            out.push(PlayerEvent::Playing(now));
        }
        // Sent under the lock, so events from the two decks reach the app in the order the mix
        // decided on them: the outgoing track's end before anything from the incoming one.
        out.into_iter().all(|e| self.tx.send(e).is_ok())
    }

    /// A position tick from the active deck. Cues the next track a little ahead of the crossfade,
    /// and starts the overlap once the track is within the crossfade of its end.
    fn maybe_overlap(&self, s: &mut MixState, deck: usize, pos: f64, out: &mut Vec<PlayerEvent>) {
        if s.overlap.is_some() || s.loop_file || !s.crossfade.on() {
            return;
        }
        let duration = s.duration[deck];
        if !(duration > 0.0 && pos.is_finite()) {
            return;
        }
        // The music ends where the silence after it starts, and the overlap ends with the music.
        // (Not when that would be most of the track: then the measurement found something odd.)
        let tail = Self::silence_at(s, s.path[deck].as_deref(), silence::End::Tail)
            .filter(|&t| t < duration / 2.0)
            .unwrap_or(0.0);
        let end = duration - tail;
        // A third of the track at most: a crossfade longer than that isn't a transition any more.
        let len = s.crossfade.secs.min(end / 3.0);
        let left = end - pos;
        if s.cued.is_none() && left <= len + CUE_AHEAD_SECS {
            let Some(next) = s.next.take() else { return };
            if left < MIN_OVERLAP_SECS {
                let _ = self.decks[deck].command("loadfile", &[&quoted(&next.url), "append"]);
                return;
            }
            self.cue(s, deck, next);
        }
        if s.cued.is_some() && left <= len {
            self.start_cued(
                s,
                deck,
                Some(Overlap { from: deck, start: pos, len: left.max(1e-3) }),
                out,
            );
        }
    }

    /// Load `next` on the deck that isn't `from`, paused and silent.
    fn cue(&self, s: &mut MixState, from: usize, next: Next) {
        let to = 1 - from;
        let lead = Self::silence_at(s, Some(&next.url), silence::End::Head)
            .filter(|&l| l >= MIN_LEAD_SECS);
        s.gain[to] = next.gain_db;
        s.sweep[to] = if s.crossfade.sweeps() { Sweep::opening(0.0) } else { Sweep::FLAT };
        s.level[to] = 0.0;
        s.duration[to] = 0.0;
        let deck = &self.decks[to];
        let cued =
            self.apply_af(s, to, true).and_then(|()| self.apply_volume(s, to)).and_then(|()| {
                deck.set_property("pause", true)?;
                deck.set_property("loop-file", "no")?;
                deck.command("loadfile", &[&quoted(&next.url), "replace"])?;
                Ok(())
            });
        match cued {
            Ok(()) => {
                s.cued = Some(to);
                s.handoff = Some(to);
                // In where the sound starts: a second of silence at the top would be a second of
                // the overlap with nothing coming in.
                s.lead_in[to] = lead;
            }
            Err(e) => {
                tracing::warn!(error = %e, "crossfade: couldn't cue the next track, going gapless");
                s.level[to] = 1.0;
                let _ = self.decks[from].command("loadfile", &[&quoted(&next.url), "append"]);
            }
        }
    }

    /// Start the cued track and make its deck the active one: under `overlap`, or at full level
    /// when there is none. To the app the current track has ended: the queue advances, and the
    /// new track's length (reported while it was cued) is announced as it would be on a load.
    fn start_cued(
        &self,
        s: &mut MixState,
        from: usize,
        overlap: Option<Overlap>,
        out: &mut Vec<PlayerEvent>,
    ) {
        let Some(to) = s.cued.take() else { return };
        match &overlap {
            Some(o) => tracing::info!(secs = o.len, "crossfade: overlap started"),
            None => tracing::info!("crossfade: the track ended early, cut to the cued one"),
        }
        if overlap.is_none() {
            s.level[to] = 1.0;
            let _ = self.apply_volume(s, to);
            self.retune(s, to, Sweep::FLAT);
        }
        // Its leading silence may have been measured only after it was cued (the next track
        // resolved late, or the user jumped close to the end): skip it now, before it plays, or
        // as soon as the file is open if it isn't yet.
        if let Some(lead) = Self::silence_at(s, s.path[to].as_deref(), silence::End::Head)
            .filter(|&l| l >= MIN_LEAD_SECS)
        {
            match self.decks[to].get_property::<f64>("time-pos") {
                Ok(at) if at < lead - MIN_LEAD_SECS => {
                    let _ = self.decks[to].command("seek", &[&lead.to_string(), "absolute"]);
                }
                Ok(_) => {}
                Err(_) => s.lead_in[to] = Some(lead),
            }
        }
        // Playing on if the music is: an overlap can begin paused, from a seek into its window.
        let _ = self.decks[to].set_property("pause", s.paused[from]);
        s.overlap = overlap;
        s.active = to;
        out.push(PlayerEvent::TrackEnded);
        if s.duration[to] > 0.0 {
            out.push(PlayerEvent::Duration(s.duration[to]));
        }
    }

    /// Drop a cued track: the queue changed under it, or something else is being played.
    fn uncue(&self, s: &mut MixState) {
        let Some(c) = s.cued.take() else { return };
        s.lead_in[c] = None;
        let _ = self.decks[c].command("stop", &[]);
        s.level[c] = 1.0;
        if s.handoff == Some(c) {
            s.handoff = None;
        }
    }

    /// A position tick from the outgoing deck: move both levels (and sweeps) along.
    fn step_overlap(&self, s: &mut MixState, pos: f64) {
        let Some(o) = &s.overlap else { return };
        let (from, to) = (o.from, 1 - o.from);
        let p = ((pos - o.start) / o.len).clamp(0.0, 1.0);
        (s.level[from], s.level[to]) = overlap_levels(p);
        let _ = self.apply_volume(s, from);
        let _ = self.apply_volume(s, to);
        self.retune(s, from, Sweep::closing(p));
        self.retune(s, to, Sweep::opening(p));
        if p >= 1.0 {
            self.end_overlap(s);
        }
    }

    /// Finish an overlap now: the outgoing deck stops, the incoming one is at full level. A no-op
    /// when none is running.
    fn end_overlap(&self, s: &mut MixState) {
        let Some(o) = s.overlap.take() else { return };
        let (from, to) = (o.from, 1 - o.from);
        let _ = self.decks[from].command("stop", &[]);
        s.level[to] = 1.0;
        let _ = self.apply_volume(s, to);
        self.retune(s, to, Sweep::FLAT);
        self.retune(s, from, Sweep::FLAT);
    }

    /// Start `next` on `deck` straight away, replacing what it had.
    fn cut_to(&self, s: &mut MixState, deck: usize, next: Next) {
        s.lead_in[deck] = None;
        s.gain[deck] = next.gain_db;
        s.sweep[deck] = Sweep::FLAT;
        s.level[deck] = 1.0;
        let started =
            self.apply_af(s, deck, true).and_then(|()| self.apply_volume(s, deck)).and_then(|()| {
                Ok(self.decks[deck].command("loadfile", &[&quoted(&next.url), "replace"])?)
            });
        match started {
            Ok(()) => s.handoff = Some(deck),
            Err(e) => tracing::warn!(error = %e, "couldn't start the held next track"),
        }
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

fn event_loop(mut ev: EventContext, deck: usize, mix: Arc<Mix>) {
    // Playback state is derived from two properties, never polled: mpv answers `mpv_get_property`
    // synchronously on its core lock, so asking it from the app's async event pump can stall that
    // pump exactly when mpv is busiest (a gapless transition opening the next stream) — and a
    // stalled pump stops draining mpv's events, so track-end is never handled and playback wedges.
    // These arrive as events; nothing has to ask. `pause` alone would be a trap: it starts out
    // `false` and a `loadfile` doesn't touch it, so `idle-active` is the one that says a file began
    // (see `PlayerEvent::Playing`).
    loop {
        let e = match ev.wait_event(1.0) {
            Some(Ok(event)) => match event {
                Event::PropertyChange {
                    name: "time-pos", change: PropertyData::Double(p), ..
                } => DeckEvent::Position(p),
                Event::PropertyChange {
                    name: "duration", change: PropertyData::Double(d), ..
                } => DeckEvent::Duration(d),
                Event::PropertyChange { name: "pause", change: PropertyData::Flag(p), .. } => {
                    DeckEvent::Paused(p)
                }
                Event::PropertyChange {
                    name: "idle-active",
                    change: PropertyData::Flag(i),
                    ..
                } => DeckEvent::Idle(i),
                Event::PropertyChange {
                    name: "audio-params/samplerate",
                    change: PropertyData::Int64(r),
                    ..
                } => DeckEvent::Rate(r),
                Event::PropertyChange { name: "path", change: PropertyData::Str(p), .. } => {
                    DeckEvent::Path(p.to_owned())
                }
                Event::StartFile => DeckEvent::Started,
                Event::FileLoaded => DeckEvent::Loaded,
                // STOP/QUIT/REDIRECT are deliberate (loadfile replace, shutdown, the end of an
                // overlap) — ignored. ERROR never reaches this arm: libmpv2 surfaces
                // end-file-with-error as Err from wait_event (see below).
                Event::EndFile(reason) if reason as i32 == EOF => DeckEvent::Ended,
                _ => continue,
            },
            // libmpv2 routes MPV_EVENT_END_FILE with an error (dead URL, 403, bad format) through
            // here instead of Event::EndFile — in our usage (no async get/set/command replies) an
            // Err from wait_event *is* a failed track.
            Some(Err(e)) => DeckEvent::Failed(friendly_error(&e)),
            None => continue,
        };
        // Receiver dropped ⇒ player gone ⇒ stop the thread.
        if !mix.on_event(deck, e) {
            break;
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
    use super::{
        af_chain, faded_volume, glide, overlap_levels, perceptual_to_mpv, quoted, AfState,
        Equalizer, Sweep, EQ_BANDS,
    };

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
        let af = || p.mix.decks[0].get_property::<String>("af").unwrap();

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
        let lavf = p.mix.decks[0].get_property::<String>("stream-lavf-o").unwrap();
        assert!(lavf.contains("reconnect_streamed=1"), "reconnect options missing: {lavf}");
        let vol = || p.mix.decks[0].get_property::<f64>("volume").unwrap();
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

    #[test]
    fn an_overlap_holds_the_power_level() {
        // Equal power: the two levels' squares always sum to one, so the pair never dips.
        for i in 0..=20 {
            let (out, inc) = overlap_levels(i as f64 / 20.0);
            assert!((out * out + inc * inc - 1.0).abs() < 1e-12);
        }
        assert_eq!(overlap_levels(0.0), (1.0, 0.0));
        let (out, inc) = overlap_levels(1.0);
        assert!(out.abs() < 1e-12 && (inc - 1.0).abs() < 1e-12);
        // Past either end it holds, rather than swinging back.
        assert_eq!(overlap_levels(-0.5), (1.0, 0.0));
        assert_eq!(overlap_levels(1.5), overlap_levels(1.0));
    }

    #[test]
    fn the_sweeps_glide_on_a_log_scale() {
        assert_eq!(glide(20_000.0, 200.0, 0.0), 20_000.0);
        assert!((glide(20_000.0, 200.0, 1.0) - 200.0).abs() < 1e-9);
        // Halfway is the geometric midpoint: as many octaves behind as ahead.
        assert!((glide(20_000.0, 200.0, 0.5) - 2_000.0).abs() < 1e-9);
        // Outside an overlap both filters are in the chain and pass the audio through untouched.
        assert_eq!(
            Sweep::FLAT.filters(),
            [
                "@xflow:lavfi=[lowpass=f=3000:m=0]".to_string(),
                "@xfhigh:lavfi=[highpass=f=20:m=0]".to_string()
            ]
        );
        assert_eq!(Sweep::opening(0.0).filters()[1], "@xfhigh:lavfi=[highpass=f=1000:m=1]");
        assert_eq!(Sweep::closing(1.0).filters()[0], "@xflow:lavfi=[lowpass=f=200:m=1]");
    }

    /// A mono 16-bit WAV of a sine: a source whose length mpv knows up front, which a crossfade
    /// needs (it starts a fixed time before the end).
    fn tone(dir: &std::path::Path, hz: f64, secs: f64, rate: u32) -> String {
        wav(dir, &format!("{hz}"), &[(hz, secs)], rate)
    }

    /// A WAV of `(hz, secs)` segments, where 0 Hz is silence.
    fn wav(dir: &std::path::Path, name: &str, segments: &[(f64, f64)], rate: u32) -> String {
        let n: u32 = segments.iter().map(|&(_, secs)| (secs * rate as f64) as u32).sum();
        let mut wav = Vec::with_capacity(44 + 2 * n as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + 2 * n).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(2 * n).to_le_bytes());
        for &(hz, secs) in segments {
            for i in 0..(secs * rate as f64) as u32 {
                let x = (i as f64 / rate as f64 * hz * std::f64::consts::TAU).sin() * 8_000.0;
                wav.extend_from_slice(&(x as i16).to_le_bytes());
            }
        }
        let path = dir.join(format!("{name}.wav"));
        std::fs::write(&path, wav).unwrap();
        path.to_str().unwrap().to_owned()
    }

    /// Everything the player sends until `stop` matches one, or `None` after `secs`.
    fn until(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<super::PlayerEvent>,
        secs: f64,
        stop: impl Fn(&super::PlayerEvent) -> bool,
    ) -> Option<Vec<super::PlayerEvent>> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(e) => {
                    let done = stop(&e);
                    seen.push(e);
                    if done {
                        return Some(seen);
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        None
    }

    fn last_position(events: &[super::PlayerEvent]) -> f64 {
        events
            .iter()
            .rev()
            .find_map(|e| match e {
                super::PlayerEvent::Position(p) => Some(*p),
                _ => None,
            })
            .unwrap_or(0.0)
    }

    /// The mix, driven through real libmpv on the null audio output: two tones cross, the app is
    /// told the first one ended when the overlap starts rather than at its end, the second deck
    /// takes over, and a transition asked to stay gapless goes through mpv's playlist. Played at double speed to keep the
    /// test short; the overlap runs on media time, so it scales with it.
    #[test]
    fn a_crossfade_hands_over_early_and_a_held_pair_stays_gapless() {
        use super::{Crossfade, Player, PlayerEvent};
        use std::collections::HashMap;

        let dir = std::env::temp_dir().join("limusic-crossfade-test");
        std::fs::create_dir_all(&dir).unwrap();
        // The first at 8 kHz: its low-pass sweeps from 20 kHz, far above that file's Nyquist
        // frequency, which crashed libavfilter until the corners were bounded by the sample rate.
        let a = tone(&dir, 440.0, 4.0, 8_000);
        let (b, c) = (tone(&dir, 660.0, 4.0, 48_000), tone(&dir, 550.0, 4.0, 44_100));
        let mut p = Player::new(dir.to_str().unwrap()).expect("libmpv");
        let mut rx = p.take_events().unwrap();
        p.set_speed(2.0).unwrap();
        p.set_crossfade(Crossfade { secs: 1.5, filters: true }).unwrap();
        let ended = |e: &PlayerEvent| matches!(e, PlayerEvent::TrackEnded);

        // 1. A crossfade. The overlap is a third of a 4 s track at most, so it starts 1.33 s out.
        p.load(&a, &HashMap::new(), None).unwrap();
        p.play().unwrap();
        p.enqueue(&b, Some(-3.0), true).unwrap();
        let seen = until(&mut rx, 5.0, ended).expect("the first track never handed over");
        let at = last_position(&seen);
        assert!((2.4..3.5).contains(&at), "handed over at {at}, not 1.33 s before the end");
        // Mid-overlap: the player is busy, not stalled, and the new track is the active one,
        // at its own loudness and with its high-pass in the chain.
        assert!(!p.is_idle());
        {
            let s = p.lock();
            assert_eq!(s.active, 1);
            assert!(s.overlap.is_some());
            // The sweeps are bounded by each deck's sample rate, so it has to be known.
            assert_eq!(s.rate, [8_000.0, 48_000.0]);
        }
        let af1 = p.mix.decks[1].get_property::<String>("af").unwrap();
        assert!(af1.contains("volume=-3dB") && af1.contains("highpass"), "incoming chain: {af1}");
        // Its length was learned while it was cued and is announced now, as a load would.
        let next = until(&mut rx, 1.0, |e| matches!(e, PlayerEvent::Duration(_))).unwrap();
        assert!(!next.iter().any(ended));
        // Both decks sound at once somewhere in the middle.
        let vol = |d: usize| p.mix.decks[d].get_property::<f64>("volume").unwrap();
        let mut both = false;
        while p.lock().overlap.is_some() {
            both |= vol(0) > 0.0 && vol(1) > 0.0;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(both, "the two tracks never overlapped");
        // Afterwards the outgoing deck is stopped and the incoming one at full level.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(p.mix.decks[0].get_property::<bool>("idle-active").unwrap());
        assert_eq!(vol(1), super::perceptual_to_mpv(100));
        // Nothing left to cross into: the second track plays to its end, and the player idles.
        until(&mut rx, 5.0, ended).expect("the second track never ended");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(p.is_idle());

        // 2. A pair held together (an album, say) goes through mpv's playlist: no early handover,
        // and the other deck never loads.
        p.load(&a, &HashMap::new(), None).unwrap();
        p.play().unwrap();
        p.enqueue(&c, None, false).unwrap();
        until(&mut rx, 5.0, ended).expect("the gapless track never ended");
        let s = p.lock();
        assert!(s.overlap.is_none());
        assert!(p.mix.decks[1 - s.active].get_property::<bool>("idle-active").unwrap());
        drop(s);

        // 3. Loading something else mid-overlap cuts the outgoing track off at once.
        p.load(&a, &HashMap::new(), None).unwrap();
        p.enqueue(&b, None, true).unwrap();
        until(&mut rx, 5.0, ended).expect("no handover");
        let from = p.lock().overlap.as_ref().map(|o| o.from).expect("an overlap is running");
        p.load(&c, &HashMap::new(), None).unwrap();
        assert!(p.lock().overlap.is_none());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(p.mix.decks[from].get_property::<bool>("idle-active").unwrap());
    }

    /// The overlap covers the music, not the quiet around it: a track that ends on two seconds of
    /// silence hands over two seconds early, and one that opens on a second of it comes in at the
    /// first sound. Both ends are measured from the files themselves, on the probe's own mpv.
    #[test]
    fn a_crossfade_skips_the_silence_around_the_music() {
        use super::{Crossfade, Player, PlayerEvent};
        use std::collections::HashMap;

        let dir = std::env::temp_dir().join("limusic-crossfade-silence-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = wav(&dir, "ends-quiet", &[(440.0, 3.0), (0.0, 2.0)], 44_100);
        let b = wav(&dir, "starts-quiet", &[(0.0, 1.0), (660.0, 3.0)], 44_100);
        let mut p = Player::new(dir.to_str().unwrap()).expect("libmpv");
        let mut rx = p.take_events().unwrap();
        p.set_speed(2.0).unwrap();
        p.set_crossfade(Crossfade { secs: 1.5, filters: false }).unwrap();
        p.load(&a, &HashMap::new(), None).unwrap();
        p.play().unwrap();
        p.enqueue(&b, None, true).unwrap();
        // Both measurements land long before they are needed.
        let measured = |url: &str, end| {
            let s = p.lock();
            super::Mix::silence_at(&s, Some(url), end)
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while measured(&a, super::silence::End::Tail).is_none()
            || measured(&b, super::silence::End::Head).is_none()
        {
            assert!(std::time::Instant::now() < deadline, "the silence was never measured");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let tail = measured(&a, super::silence::End::Tail).unwrap();
        let head = measured(&b, super::silence::End::Head).unwrap();
        assert!((tail - 2.0).abs() < 0.06, "tail {tail}");
        assert!((head - 1.0).abs() < 0.06, "head {head}");

        // The music ends at 3 s; the overlap is a third of that, so it starts at 2 s rather than
        // 1.5 s before the file's end at 5 s.
        let ended = |e: &PlayerEvent| matches!(e, PlayerEvent::TrackEnded);
        let seen = until(&mut rx, 5.0, ended).expect("no handover");
        let at = last_position(&seen);
        assert!(
            (1.8..2.4).contains(&at),
            "handed over at {at}, not a second before the music ends"
        );
        // The incoming track starts where its sound does.
        let first = until(&mut rx, 2.0, |e| matches!(e, PlayerEvent::Position(_))).unwrap();
        let from = last_position(&first);
        assert!(from >= 0.9, "the incoming track started at {from}, in its silence");
        // And the outgoing one stops with its music, not at the end of its file two seconds later.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while p.lock().overlap.is_some() {
            assert!(std::time::Instant::now() < deadline, "the overlap never ended");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(p.mix.decks[0].get_property::<bool>("idle-active").unwrap());
        let out = p.mix.decks[1].get_property::<f64>("time-pos").unwrap();
        assert!(out < 3.0, "the outgoing deck ran on into its silence (incoming at {out})");
    }
}
