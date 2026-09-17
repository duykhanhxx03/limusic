// Run with `pnpm test` (node's built-in runner; Node ≥ 23 strips the types itself).
// Lives outside `src/` so svelte-check, which has no Node types, does not try to compile it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MediaClock } from '../src/lib/mediaclock.ts';

/** Deterministic PRNG so a failure reproduces. */
function rng(seed: number) {
	let s = seed >>> 0;
	return () => {
		s = (s * 1664525 + 1013904223) >>> 0;
		return s / 2 ** 32;
	};
}

/**
 * Reports in the shape the app actually gets them: every 250 ms, each delayed by a latency that is
 * usually small and sometimes large. The spread is the measured one — the old snapping clock saw
 * corrections from −43 to +51 ms on this path.
 */
function* reports(opts: { from: number; seconds: number; speed?: number; seed?: number }) {
	const speed = opts.speed ?? 1;
	const rand = rng(opts.seed ?? 7);
	for (let emit = 0; emit <= opts.seconds * 1000; emit += 250) {
		const latency = rand() < 0.7 ? rand() * 20 : 20 + rand() * 100;
		yield {
			position: opts.from + (emit / 1000) * speed,
			at: 10_000 + emit + latency,
			speed,
			paused: false,
			truth: (t: number) => opts.from + ((t - 10_000) / 1000) * speed
		};
	}
}

test('once settled, the estimate stays within a few frames of the truth', () => {
	const clock = new MediaClock();
	let worst = 0;
	for (const r of reports({ from: 30, seconds: 20 })) {
		clock.feed(r);
		if (r.at < 11_000) continue; // the first second is lock-in
		for (const dt of [0, 40, 120, 240]) {
			const t = r.at + dt;
			worst = Math.max(worst, Math.abs(clock.valueAt(t) - r.truth(t)));
		}
	}
	assert.ok(worst < 0.025, `worst error ${(worst * 1000).toFixed(1)} ms`);
});

test('it is steadier than snapping to each report, which is what it replaces', () => {
	const clock = new MediaClock();
	let snapWorst = 0;
	let clockWorst = 0;
	for (const r of reports({ from: 0, seconds: 20, seed: 3 })) {
		clock.feed(r);
		if (r.at < 11_000) continue;
		// The old behaviour: the report's value, taken as current at the moment it arrived.
		snapWorst = Math.max(snapWorst, Math.abs(r.position - r.truth(r.at)));
		clockWorst = Math.max(clockWorst, Math.abs(clock.valueAt(r.at) - r.truth(r.at)));
	}
	assert.ok(clockWorst * 3 < snapWorst, `clock ${clockWorst * 1000} ms vs snap ${snapWorst * 1000} ms`);
});

test('it never steps back by a visible amount while playing', () => {
	// Walked in wall-clock order, the way the app sees it: reports land when they land, and the
	// estimate is read every frame in between.
	const clock = new MediaClock();
	const pending = [...reports({ from: 5, seconds: 15, seed: 11 })];
	let prev = -Infinity;
	let worstBack = 0;
	for (let t = pending[0].at; pending.length || t < 25_000; t += 16) {
		while (pending.length && pending[0].at <= t) clock.feed(pending.shift()!);
		const v = clock.valueAt(t);
		if (t > 11_000) worstBack = Math.max(worstBack, prev - v);
		prev = v;
		if (!pending.length) break;
	}
	// A step back can only come from the window dropping its best report, and the runner-up is
	// never far behind it. A frame at 60 Hz is 16.7 ms; this has to stay well under one.
	assert.ok(worstBack < 0.008, `stepped back ${(worstBack * 1000).toFixed(2)} ms`);
});

test('a seek is a new timeline, taken at once rather than filtered toward', () => {
	const clock = new MediaClock();
	for (const r of reports({ from: 10, seconds: 3 })) clock.feed(r);
	const result = clock.feed({ position: 95, at: 13_500, speed: 1, paused: false });
	assert.equal(result, 'reset');
	assert.ok(Math.abs(clock.valueAt(13_500) - 95) < 1e-9);
});

test('pausing freezes the position, and resuming starts from the report that resumed it', () => {
	const clock = new MediaClock();
	for (const r of reports({ from: 40, seconds: 2 })) clock.feed(r);
	assert.equal(clock.feed({ position: 42.1, at: 12_200, speed: 1, paused: true }), 'reset');
	assert.equal(clock.valueAt(12_200), 42.1);
	assert.equal(clock.valueAt(19_000), 42.1, 'a paused clock does not run');
	assert.equal(clock.feed({ position: 42.1, at: 20_000, speed: 1, paused: false }), 'reset');
	assert.ok(Math.abs(clock.valueAt(21_000) - 43.1) < 1e-9);
});

test('tempo is honoured: at 1.5x the estimate runs 1.5x', () => {
	const clock = new MediaClock();
	let worst = 0;
	for (const r of reports({ from: 0, seconds: 10, speed: 1.5, seed: 5 })) {
		clock.feed(r);
		if (r.at < 11_000) continue;
		worst = Math.max(worst, Math.abs(clock.valueAt(r.at + 200) - r.truth(r.at + 200)));
	}
	assert.ok(worst < 0.03, `worst error ${(worst * 1000).toFixed(1)} ms at 1.5x`);
});

test('changing tempo resets rather than smearing the old rate into the new one', () => {
	const clock = new MediaClock();
	for (const r of reports({ from: 0, seconds: 2 })) clock.feed(r);
	assert.equal(clock.feed({ position: 2.05, at: 12_050, speed: 1.25, paused: false }), 'reset');
	assert.ok(Math.abs(clock.valueAt(13_050) - 3.3) < 1e-9);
});

test('a provisional resume is replaced by the first real report, not averaged with it', () => {
	const clock = new MediaClock();
	clock.feed({ position: 60, at: 10_000, speed: 1, paused: true });
	// Resumed at 20 s wall; audio actually came back 80 ms later than that guess assumes.
	clock.feed({ position: 60, at: 20_000, speed: 1, paused: false, provisional: true });
	assert.equal(clock.feed({ position: 60.17, at: 20_250, speed: 1, paused: false }), 'steady');
	assert.ok(Math.abs(clock.valueAt(20_250) - 60.17) < 1e-9, 'the real report wins outright');
});
