//! How much silence a track opens and closes on, so a crossfade can overlap the music rather than
//! the quiet around it. A song that ends on four seconds of nothing would otherwise spend most of
//! a six-second crossfade fading out silence, and one that opens on a second of it would come in
//! a second late.
//!
//! Measured by decoding the first or last seconds of the file on a third mpv instance that plays
//! into a raw PCM file (`ao=pcm`), which it does as fast as it can decode: 30 s of audio takes a
//! few tens of milliseconds, plus the fetch. The samples come out as 8 kHz mono 16-bit, plenty to
//! tell sound from silence.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, Instant};

use libmpv2::events::{Event, EventContext, PropertyData};
use libmpv2::{Format, Mpv};

/// Which end of the track to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum End {
    Head,
    Tail,
}

pub(crate) struct Job {
    pub url: String,
    pub end: End,
    /// The headers the decks were last given; the probe fetches the same streams.
    pub user_agent: Option<String>,
    pub header_fields: String,
}

/// A finished measurement: seconds of silence at that end, or `None` when there was nothing to
/// go on (the fetch failed, or the head never got loud).
pub(crate) struct Measured {
    pub url: String,
    pub end: End,
    pub secs: Option<f64>,
}

/// How much of each end is decoded. The head only has to reach the first sound; the tail has room
/// for a long fade into a long silence.
const HEAD_SECS: f64 = 15.0;
const TAIL_SECS: f64 = 30.0;
const RATE: f64 = 8_000.0;
/// 50 ms at `RATE`.
const WINDOW: usize = 400;
/// A window quieter than this (RMS, dBFS) is silence: below the noise floor of most masters, and
/// inaudible under a track fading in over it. A soft intro, or the last seconds of a fade-out, is
/// louder than this and counts as music.
const SILENCE_DBFS: f64 = -45.0;
/// A measurement that hasn't finished by then is abandoned: the crossfade goes by the track's
/// length, as it would without one.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Start the measuring thread. Jobs run one at a time; each result goes to `done`. The mpv
/// instance is only created with the first job, so with crossfading off this costs one idle
/// thread.
pub(crate) fn spawn(pcm: PathBuf, done: impl Fn(Measured) + Send + 'static) -> Sender<Job> {
    let (tx, rx) = channel::<Job>();
    std::thread::Builder::new()
        .name("mpv-silence".into())
        .spawn(move || {
            let mut probe: Option<(Mpv, EventContext)> = None;
            while let Ok(job) = rx.recv() {
                if probe.is_none() {
                    probe = open(&pcm)
                        .inspect_err(|e| tracing::warn!(error = %e, "crossfade: no silence probe"))
                        .ok();
                }
                let secs = probe.as_mut().and_then(|(mpv, ev)| measure(mpv, ev, &job, &pcm));
                tracing::debug!(end = ?job.end, secs, "crossfade: silence measured");
                done(Measured { url: job.url, end: job.end, secs });
            }
        })
        .expect("spawn silence probe thread");
    tx
}

fn open(pcm: &Path) -> Result<(Mpv, EventContext), libmpv2::Error> {
    let mpv = Mpv::new()?;
    crate::quiet_builtins(&mpv);
    for (k, v) in [
        ("vid", "no"),
        ("ao", "pcm"),
        ("ao-pcm-waveheader", "no"),
        ("audio-format", "s16"),
        ("audio-samplerate", "8000"),
        ("audio-channels", "mono"),
        ("demuxer-max-bytes", "4MiB"),
    ] {
        mpv.set_property(k, v)?;
    }
    mpv.set_property("ao-pcm-file", pcm.to_string_lossy().as_ref())?;
    let ev = EventContext::new(mpv.ctx);
    ev.disable_deprecated_events().ok();
    ev.observe_property("idle-active", Format::Flag, 0)?;
    Ok((mpv, ev))
}

fn measure(mpv: &Mpv, ev: &mut EventContext, job: &Job, pcm: &Path) -> Option<f64> {
    let _ = std::fs::remove_file(pcm);
    if let Some(ua) = &job.user_agent {
        mpv.set_property("user-agent", ua.as_str()).ok()?;
    }
    mpv.set_property("http-header-fields", job.header_fields.as_str()).ok()?;
    let (start, end) = match job.end {
        End::Head => ("none".to_owned(), HEAD_SECS.to_string()),
        // A negative start counts from the end; a file shorter than that is decoded whole.
        End::Tail => (format!("-{TAIL_SECS}"), "none".to_owned()),
    };
    mpv.set_property("start", start.as_str()).ok()?;
    mpv.set_property("end", end.as_str()).ok()?;
    mpv.command("loadfile", &[&crate::quoted(&job.url), "replace"]).ok()?;
    // The file is complete once mpv is idle again: the audio output is closed, and with it the
    // PCM file, only then.
    let ok = wait_until_idle(ev, Instant::now() + TIMEOUT);
    if ok != Some(true) {
        if ok.is_none() {
            // Timed out: stop it, and let its last events drain so the next job doesn't read them
            // as its own.
            let _ = mpv.command("stop", &[]);
            wait_until_idle(ev, Instant::now() + Duration::from_secs(2));
        }
        return None;
    }
    let bytes = std::fs::read(pcm).ok()?;
    let samples: Vec<i16> =
        bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    match job.end {
        End::Head => leading(&samples),
        End::Tail => Some(trailing(&samples)),
    }
}

/// Wait for the file to end and mpv to go idle: `Some(true)` when it played to the end,
/// `Some(false)` when it failed, `None` on timeout.
fn wait_until_idle(ev: &mut EventContext, deadline: Instant) -> Option<bool> {
    let mut ended: Option<bool> = None;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match ev.wait_event(left.as_secs_f64()) {
            Some(Ok(Event::EndFile(_))) => ended = Some(true),
            // libmpv2 reports an end-file with an error this way (see `event_loop`).
            Some(Err(_)) => ended = Some(false),
            Some(Ok(Event::PropertyChange {
                name: "idle-active",
                change: PropertyData::Flag(true),
                ..
            })) if ended.is_some() => return ended,
            _ => {}
        }
    }
}

fn loud(window: &[i16]) -> bool {
    let power = window.iter().map(|&s| (s as f64 / 32_768.0).powi(2)).sum::<f64>();
    let rms = (power / window.len().max(1) as f64).sqrt();
    20.0 * rms.max(1e-9).log10() > SILENCE_DBFS
}

/// Seconds of silence before the first window with sound in it. `None` when there is none, which
/// says nothing about where the music starts.
fn leading(samples: &[i16]) -> Option<f64> {
    let first = samples.chunks(WINDOW).position(loud)?;
    Some((first * WINDOW) as f64 / RATE)
}

/// Seconds of silence after the last window with sound in it: all of it when none has any, the
/// silence being at least that long.
fn trailing(samples: &[i16]) -> f64 {
    let windows: Vec<bool> = samples.chunks(WINDOW).map(loud).collect();
    let sound_ends = match windows.iter().rposition(|&l| l) {
        Some(i) => ((i + 1) * WINDOW).min(samples.len()),
        None => 0,
    };
    (samples.len() - sound_ends) as f64 / RATE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of a sine at `amp` (full scale 1.0) at `RATE`, or silence at amp 0.
    fn tone(secs: f64, amp: f64) -> Vec<i16> {
        (0..(secs * RATE) as usize)
            .map(|i| {
                ((i as f64 / RATE * 440.0 * std::f64::consts::TAU).sin() * amp * 32_767.0) as i16
            })
            .collect()
    }

    #[test]
    fn silence_is_measured_from_either_end() {
        let track = [tone(1.0, 0.0), tone(3.0, 0.3), tone(2.0, 0.0)].concat();
        assert_eq!(leading(&track), Some(1.0));
        assert_eq!(trailing(&track), 2.0);
        // Straight into the music, and straight out of it.
        let tight = tone(2.0, 0.3);
        assert_eq!(leading(&tight), Some(0.0));
        assert_eq!(trailing(&tight), 0.0);
    }

    #[test]
    fn a_quiet_ending_is_music_and_hiss_is_not() {
        // −40 dBFS RMS: the last seconds of a fade-out, still heard in a quiet room.
        let fade = [tone(1.0, 0.3), tone(2.0, 0.014)].concat();
        assert_eq!(trailing(&fade), 0.0);
        // −60 dBFS: a noise floor, not music.
        let hiss = [tone(1.0, 0.3), tone(2.0, 0.0014)].concat();
        assert_eq!(trailing(&hiss), 2.0);
    }

    #[test]
    fn all_silence_has_no_start_and_a_tail_as_long_as_it_is() {
        let nothing = tone(3.0, 0.0);
        assert_eq!(leading(&nothing), None);
        assert_eq!(trailing(&nothing), 3.0);
        assert_eq!(trailing(&[]), 0.0);
    }
}
