/**
 * A smooth estimate of where playback is *right now*, built from mpv's sparse position reports.
 *
 * mpv's position reaches the webview about four times a second, and each report is stale by
 * however long it spent crossing the event pump, the IPC bridge and the webview's event loop. The
 * lyrics view used to snap its clock to every report as it arrived. Measured over 160 reports:
 * each snap moved the clock by 17 ms on average and up to 115 ms, and one in four moved it
 * *backwards*. On a word being swept across 150 px in 300 ms that is a 20 px jolt, four times a
 * second, a quarter of them in reverse — which is the stutter, not the animation.
 *
 * The fix rests on one observation: a report is only ever *late*, never early. If `p` is the
 * position mpv read and `L` the delay before the webview saw it, then at arrival the real position
 * was `p + L` with `L ≥ 0`. So among recent reports, the one implying the *latest* position is the
 * one that travelled fastest, and it is the best estimate there is. That is the classic minimum-
 * latency filter clock-sync protocols use, and it holds steady where a mean would wobble with every
 * slow report.
 *
 * Two refinements keep it honest:
 *  - It is not the maximum of *all* reports but the second-highest of the last [`WINDOW`] (four
 *    seconds). The window lets real drift between the audio clock and the monotonic clock through;
 *    taking the runner-up means one report that is somehow early cannot drag the lyrics ahead.
 *    A first version leaked the maximum downward over time instead, and a run of slow reports
 *    — 1.5 s of them in the test — let it sag 39 ms. The window rides that out, because the fast
 *    reports from before the run are still in it.
 *  - Anything that is not ordinary playback — a seek, pause, resume, a tempo change, the first
 *    report of a track — resets it outright ([`RESET`]), since the filter's premise ("the same
 *    timeline, reported late") no longer holds across those.
 *
 * Pure and framework-free on purpose: this is the one piece of the lyrics view whose correctness is
 * arithmetic, so it is the piece with tests (`ui/unit/mediaclock.test.ts`, `pnpm test`).
 */

/** A report further than this from the running estimate is a new timeline, not a late sample. */
export const RESET = 0.3; // seconds
/** Reports kept: sixteen at mpv's four a second is four seconds of history. */
export const WINDOW = 16;
/** Below this many reports the fastest is used as is; see `feed`. */
export const MIN_FOR_RUNNER_UP = 6;

export interface PositionReport {
	/** Media position mpv reported, in seconds. */
	position: number;
	/** `performance.now()` when the report reached the webview, in milliseconds. */
	at: number;
	/** Playback rate (tempo). Media time advances this many seconds per wall second. */
	speed: number;
	paused: boolean;
	/** A guess rather than a report — the moment playback resumed, say, when mpv has not yet
	 *  said where it is. It sets the clock running but is not kept as evidence, so the first real
	 *  report replaces it instead of being filtered against it. */
	provisional?: boolean;
}

export type FeedResult = 'reset' | 'steady';

export class MediaClock {
	/** Media seconds minus scaled wall seconds: `media = at/1000 * speed + offset`. */
	private offset = 0;
	/** Recent candidate offsets, oldest first. */
	private recent: number[] = [];
	private speed = 1;
	private paused = true;
	private frozenAt = 0;
	private primed = false;

	/** Take one report. `'reset'` means the timeline changed and anything synced to the clock should
	 *  re-sync; `'steady'` means it only refined an estimate it already had. */
	feed(r: PositionReport): FeedResult {
		const candidate = r.position - (r.at / 1000) * r.speed;
		const changedMode = !this.primed || r.speed !== this.speed || r.paused !== this.paused;
		const expected = this.primed ? this.valueAt(r.at) : NaN;

		if (changedMode || r.paused || !(Math.abs(r.position - expected) <= RESET)) {
			this.speed = r.speed;
			this.paused = r.paused;
			this.recent = r.provisional ? [] : [candidate];
			this.offset = candidate;
			this.frozenAt = r.position;
			this.primed = true;
			return 'reset';
		}

		this.recent.push(candidate);
		if (this.recent.length > WINDOW) this.recent.shift();
		const sorted = [...this.recent].sort((a, b) => b - a);
		// The runner-up only once there is a field to choose from. Right after a reset, with two or
		// three reports in hand, the runner-up is as likely as not a slow one: taking it made a seek
		// land 95 ms behind for its first second. An early outlier needs company to matter anyway.
		this.offset = sorted[this.recent.length < MIN_FOR_RUNNER_UP ? 0 : 1];
		return 'steady';
	}

	/** The estimated media position at `now` (a `performance.now()` value), in seconds. */
	valueAt(now: number): number {
		if (this.paused) return this.frozenAt;
		return (now / 1000) * this.speed + this.offset;
	}

	get rate(): number {
		return this.speed;
	}

	get isPaused(): boolean {
		return this.paused;
	}
}
