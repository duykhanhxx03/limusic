//! Sleep timer: stop the music after a while.
//!
//! **Why this lives in Rust and not in a `setTimeout`.** The webview is not a reliable clock for
//! this. It is throttled when the window is hidden (opening the mini player hides the main one,
//! `mini.rs`), it is destroyed and rebuilt on a reload, and the whole point of the feature is that
//! it fires while nobody is looking at the screen. The deadline is held here and the pause is
//! issued here; the UI is only told when it is, so it can draw a countdown itself.
//!
//! The UI counts down from the deadline rather than being ticked once a second: a timer that only
//! exists to redraw a label is a wakeup per second for the life of the timer, and the webview can
//! work out `deadline - now` on its own.
//!
//! Two kinds of timer: a deadline, or "at the end of this song", which has no deadline because the
//! song can be paused, seeked or sped up; it watches the position instead. Either one fades the
//! music out over its last few seconds rather than cutting it off mid-note.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tauri::Emitter;

use crate::state::AppState;

/// When the music stops, as unix milliseconds. `None` = no timer.
///
/// `generation` is what makes re-arming safe: every `set`/`clear` bumps it, and the task that was
/// sleeping on the old deadline sees the mismatch when it wakes and does nothing. Without it,
/// setting 15 minutes and then changing to 30 would leave the first task alive to pause at 15.
#[derive(Default)]
pub struct SleepTimer {
    deadline_ms: std::sync::Mutex<Option<i64>>,
    generation: AtomicU64,
    /// Armed to stop at the end of the current track rather than at a deadline.
    end_of_track: AtomicBool,
    /// The end-of-track fade has started, so position ticks must not start another.
    fading: AtomicBool,
    /// The current track's length in seconds, as f64 bits. Fed from mpv's `duration`.
    duration_bits: AtomicU64,
}

/// How long the music takes to fade out before it pauses.
const FADE_MS: u64 = 5_000;
/// End-of-track pauses this far short of the end. Pausing on the last sample races the gapless
/// advance, and losing that race moves the queue to the next song before the pause lands.
/// Measured: with a 250 ms margin and the hold after it, the pause landed on the next track.
const END_MARGIN_MS: u64 = 400;
/// Silence held after the fade before the pause, so audio already queued for the output device
/// plays out at zero rather than being cut mid-ramp.
const HOLD_MS: u64 = 150;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl SleepTimer {
    /// The deadline, or `None`. Also `None` once it has passed, so a UI that asks late is not told
    /// about a timer that already fired.
    pub fn deadline(&self) -> Option<i64> {
        let d = (*self.deadline_ms.lock().ok()?)?;
        (d > now_ms()).then_some(d)
    }

    /// Armed to stop at the end of the current track.
    pub fn end_of_track(&self) -> bool {
        self.end_of_track.load(Ordering::SeqCst)
    }
}

/// What the UI is told: when, or "end of this track", or nothing.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub deadline: Option<i64>,
    pub end_of_track: bool,
}

pub fn status(state: &AppState) -> Status {
    Status {
        deadline: state.sleep_timer.deadline(),
        end_of_track: state.sleep_timer.end_of_track(),
    }
}

/// Arm the timer for `minutes` from now, replacing any existing one.
pub fn set(state: &Arc<AppState>, minutes: u32) {
    // An hour is not a meaningful upper bound (3h is a preset), but a day is: past that it is a
    // typo, and the sleep below would hold a task for the life of the process.
    let minutes = minutes.clamp(1, 24 * 60) as i64;
    let deadline = now_ms() + minutes * 60_000;
    let gen = arm(state, Some(deadline), false);

    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        // Absolute, not a plain sleep of the remaining duration: `tokio::time::sleep` counts
        // monotonic time, which keeps running across a suspend, so a laptop shut for an hour with
        // a 15-minute timer wakes up and pauses immediately — which is the right answer. It wakes
        // a fade's length early, so the music is silent exactly at the deadline.
        let wait = (deadline - (FADE_MS + HOLD_MS) as i64 - now_ms()).max(0) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        if state.sleep_timer.generation.load(Ordering::SeqCst) != gen {
            return; // re-armed or cleared while we slept
        }
        let left = (deadline - HOLD_MS as i64 - now_ms()).clamp(0, FADE_MS as i64) as u64;
        fade_out_and_fire(state, gen, left).await;
    });
}

/// Arm the timer for the end of the current track, replacing any existing one.
pub fn set_end_of_track(state: &Arc<AppState>) {
    arm(state, None, true);
}

/// mpv's length for the current track. The end-of-track timer measures against it.
pub fn on_duration(state: &AppState, secs: f64) {
    state.sleep_timer.duration_bits.store(secs.to_bits(), Ordering::Relaxed);
}

/// A position tick. With an end-of-track timer armed, starts the fade once the track is within a
/// fade's length of its end, timed to finish just short of it. Cheap when nothing is armed: two
/// atomic loads.
pub fn on_position(state: &Arc<AppState>, pos: f64) {
    let t = &state.sleep_timer;
    if !t.end_of_track.load(Ordering::Relaxed) || t.fading.load(Ordering::Relaxed) {
        return;
    }
    let duration = f64::from_bits(t.duration_bits.load(Ordering::Relaxed));
    if !(duration.is_finite() && duration > 0.0 && pos.is_finite()) {
        return;
    }
    let remaining_ms = ((duration - pos).max(0.0) * 1000.0) as u64;
    // The fade, the hold after it and the margin all have to fit before the end.
    if remaining_ms > FADE_MS + HOLD_MS + END_MARGIN_MS || t.fading.swap(true, Ordering::SeqCst) {
        return;
    }
    let gen = t.generation.load(Ordering::SeqCst);
    let fade = remaining_ms.saturating_sub(END_MARGIN_MS + HOLD_MS);
    tauri::async_runtime::spawn(fade_out_and_fire(state.clone(), gen, fade));
}

/// Disarm. Safe to call when nothing is armed.
pub fn clear(state: &Arc<AppState>) {
    arm(state, None, false);
}

/// Set the deadline and bump the generation, then tell the UI. Returns the new generation.
fn arm(state: &Arc<AppState>, deadline: Option<i64>, end_of_track: bool) -> u64 {
    if let Ok(mut d) = state.sleep_timer.deadline_ms.lock() {
        *d = deadline;
    }
    state.sleep_timer.end_of_track.store(end_of_track, Ordering::SeqCst);
    state.sleep_timer.fading.store(false, Ordering::SeqCst);
    let gen = state.sleep_timer.generation.fetch_add(1, Ordering::SeqCst) + 1;
    // A fade in progress sees the new generation and puts the volume back itself; doing it here
    // too covers the step it may be sleeping through.
    let _ = state.player.set_fade(1.0);
    emit(state);
    gen
}

/// Fade the music out over `fade_ms`, then pause. Abandoned (volume restored) as soon as the timer
/// is re-armed or cleared. The fade rides the player's fade factor, never the user's volume, so the
/// slider does not move and the next play is at the level they left.
async fn fade_out_and_fire(state: Arc<AppState>, gen: u64, fade_ms: u64) {
    let current = || state.sleep_timer.generation.load(Ordering::SeqCst) == gen;
    // Progress is read off the clock, not counted in steps: each step's property write and wakeup
    // add a little, and summed over a hundred steps that ran the pause past the end of the track.
    let started = std::time::Instant::now();
    let total = std::time::Duration::from_millis(fade_ms);
    let from = state.current_position();
    loop {
        if !current() {
            let _ = state.player.set_fade(1.0);
            return;
        }
        // The position jumped back: the track ended and the next one started under the fade
        // (end-of-track only, if a slow tick left too little margin). Stop now rather than let the
        // next song play out the rest of the ramp.
        if state.current_position() + 1.0 < from {
            let _ = state.player.set_fade(0.0);
            break;
        }
        let p = if total.is_zero() {
            1.0
        } else {
            (started.elapsed().as_secs_f64() / total.as_secs_f64()).min(1.0)
        };
        // Equal-power: the amplitude follows a quarter cosine, which sounds like a steady fade
        // where a linear ramp seems to hang at the top and then drop away.
        let _ = state.player.set_fade((p * std::f64::consts::FRAC_PI_2).cos());
        if p >= 1.0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(HOLD_MS)).await;
    if !current() {
        let _ = state.player.set_fade(1.0);
        return;
    }
    fire(&state);
}

/// Pause, disarm, and say so. Pausing rather than stopping: the queue and the position are exactly
/// where they were, so pressing play in the morning carries on rather than starting over.
fn fire(state: &Arc<AppState>) {
    let _ = state.player.pause();
    if let Ok(mut d) = state.sleep_timer.deadline_ms.lock() {
        *d = None;
    }
    state.sleep_timer.end_of_track.store(false, Ordering::SeqCst);
    state.sleep_timer.fading.store(false, Ordering::SeqCst);
    // Back to full level once the pause has landed, so pressing play in the morning isn't silent.
    // After a beat: audio already handed to the output device would otherwise play at full volume.
    let player_state = state.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(HOLD_MS)).await;
        let _ = player_state.player.set_fade(1.0);
    });
    // Not through `arm`: the generation must not move here, or a task armed in the same
    // millisecond would be invalidated by the very event it caused.
    emit(state);
    tracing::info!("sleep timer: paused playback");
}

pub fn emit(state: &Arc<AppState>) {
    let _ = state.app.emit("sleep-timer", status(state));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deadline in the past is the same as no timer: the UI must not draw a countdown for
    /// something that has already happened.
    #[test]
    fn a_passed_deadline_reads_as_no_timer() {
        let t = SleepTimer::default();
        *t.deadline_ms.lock().unwrap() = Some(now_ms() - 1000);
        assert_eq!(t.deadline(), None);
        *t.deadline_ms.lock().unwrap() = Some(now_ms() + 60_000);
        assert!(t.deadline().is_some());
        *t.deadline_ms.lock().unwrap() = None;
        assert_eq!(t.deadline(), None);
    }

    /// Every arm has to invalidate the one before it, or changing 15 minutes to 30 leaves the
    /// first task alive to pause at 15.
    #[test]
    fn re_arming_invalidates_the_previous_generation() {
        let t = SleepTimer::default();
        let a = t.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let b = t.generation.fetch_add(1, Ordering::SeqCst) + 1;
        assert_ne!(a, b);
    }
}
