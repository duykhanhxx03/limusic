// The sleep timer as the UI sees it: a deadline from Rust, and a countdown derived from it.
//
// The ticker only runs while a timer is armed. A `setInterval` that exists purely to redraw a label
// is a wakeup a second for the life of the app otherwise, and this app is one people leave open.
//
// "End of this song" has no deadline to count down from (the song can be paused or seeked), so it
// runs no ticker either: the countdown is the track's own remaining time, which the player state
// already updates.
import * as api from './api';

export const sleep = $state({
	/** Unix ms, or null when no deadline is armed. Rust owns this; we only mirror it. */
	deadline: null as number | null,
	/** Armed to stop at the end of the current track instead of at a deadline. */
	endOfTrack: false,
	/** Milliseconds left on a deadline, or null. Recomputed by the ticker below. */
	remaining: null as number | null,
	/** The preset that was armed, so the menu can tick it. Null for a custom value or none. */
	minutes: null as number | null
});

let ticker: ReturnType<typeof setInterval> | undefined;

function retick() {
	clearInterval(ticker);
	ticker = undefined;
	if (sleep.deadline === null) {
		sleep.remaining = null;
		sleep.minutes = null;
		return;
	}
	const step = () => {
		const left = sleep.deadline === null ? null : sleep.deadline - Date.now();
		if (left === null || left <= 0) {
			// Rust has already paused and will emit; clearing here just stops the label at zero
			// rather than letting it count into negatives if that event is slow.
			sleep.deadline = null;
			sleep.remaining = null;
			sleep.minutes = null;
			clearInterval(ticker);
			ticker = undefined;
			return;
		}
		sleep.remaining = left;
	};
	step();
	ticker = setInterval(step, 1000);
}

function apply(status: api.SleepStatus) {
	sleep.deadline = status.deadline;
	sleep.endOfTrack = status.endOfTrack;
	retick();
}

export function setSleep(minutes: number) {
	// Optimistic: the deadline is arithmetic we can do here, and the event confirms it a moment
	// later. Without this the countdown does not appear until the round trip lands.
	apply({ deadline: Date.now() + minutes * 60_000, endOfTrack: false });
	sleep.minutes = minutes;
	api.setSleepTimer(minutes).catch(() => {});
}

export function setSleepEndOfTrack() {
	apply({ deadline: null, endOfTrack: true });
	api.setSleepTimerEndOfTrack().catch(() => {});
}

export function clearSleep() {
	apply({ deadline: null, endOfTrack: false });
	api.clearSleepTimer().catch(() => {});
}

/** Called once per window from initApp: ask for the current timer, then follow the events. */
export function initSleep(): Promise<() => void> {
	api.sleepTimer()
		.then(apply)
		.catch(() => {});
	return api.onSleepTimer((status) => {
		// A timer arriving from elsewhere (the other window, or the timer firing) has no preset
		// attached, so drop the tick rather than showing a stale one.
		if (status.deadline === null) sleep.minutes = null;
		apply(status);
	});
}
