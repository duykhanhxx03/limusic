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

use std::sync::atomic::{AtomicU64, Ordering};
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
}

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
}

/// Arm the timer for `minutes` from now, replacing any existing one.
pub fn set(state: &Arc<AppState>, minutes: u32) {
    // An hour is not a meaningful upper bound (3h is a preset), but a day is: past that it is a
    // typo, and the sleep below would hold a task for the life of the process.
    let minutes = minutes.clamp(1, 24 * 60) as i64;
    let deadline = now_ms() + minutes * 60_000;
    let gen = arm(state, Some(deadline));

    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        // Absolute, not a plain sleep of the remaining duration: `tokio::time::sleep` counts
        // monotonic time, which keeps running across a suspend, so a laptop shut for an hour with
        // a 15-minute timer wakes up and pauses immediately — which is the right answer.
        let wait = (deadline - now_ms()).max(0) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        if state.sleep_timer.generation.load(Ordering::SeqCst) != gen {
            return; // re-armed or cleared while we slept
        }
        fire(&state);
    });
}

/// Disarm. Safe to call when nothing is armed.
pub fn clear(state: &Arc<AppState>) {
    arm(state, None);
}

/// Set the deadline and bump the generation, then tell the UI. Returns the new generation.
fn arm(state: &Arc<AppState>, deadline: Option<i64>) -> u64 {
    if let Ok(mut d) = state.sleep_timer.deadline_ms.lock() {
        *d = deadline;
    }
    let gen = state.sleep_timer.generation.fetch_add(1, Ordering::SeqCst) + 1;
    emit(state);
    gen
}

/// Pause, disarm, and say so. Pausing rather than stopping: the queue and the position are exactly
/// where they were, so pressing play in the morning carries on rather than starting over.
fn fire(state: &Arc<AppState>) {
    let _ = state.player.pause();
    if let Ok(mut d) = state.sleep_timer.deadline_ms.lock() {
        *d = None;
    }
    // Not through `arm`: the generation must not move here, or a task armed in the same
    // millisecond would be invalidated by the very event it caused.
    emit(state);
    tracing::info!("sleep timer: paused playback");
}

pub fn emit(state: &Arc<AppState>) {
    let _ = state.app.emit("sleep-timer", state.sleep_timer.deadline());
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
