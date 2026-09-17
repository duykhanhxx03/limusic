import type { TransitionConfig } from 'svelte/transition';

/**
 * `cubic-bezier()` as a Svelte easing function, for transitions whose curve has to match a CSS one.
 *
 * Svelte bakes a transition into Web Animations keyframes at 60 steps a second, evaluating the
 * easing in JS, so a curve from `layout.css` cannot be passed through by name.
 */
export function cubicBezier(x1: number, y1: number, x2: number, y2: number): (t: number) => number {
	const bx = (t: number) => ((1 - 3 * x2 + 3 * x1) * t + (3 * x2 - 6 * x1)) * t * t + 3 * x1 * t;
	const by = (t: number) => ((1 - 3 * y2 + 3 * y1) * t + (3 * y2 - 6 * y1)) * t * t + 3 * y1 * t;
	const dx = (t: number) => (3 - 9 * x2 + 9 * x1) * t * t + (6 * x2 - 12 * x1) * t + 3 * x1;
	return (x) => {
		if (x <= 0) return 0;
		if (x >= 1) return 1;
		// Newton first, bisection if the slope is too flat to trust.
		let t = x;
		for (let i = 0; i < 6; i++) {
			const err = bx(t) - x;
			const d = dx(t);
			if (Math.abs(err) < 1e-5) return by(t);
			if (Math.abs(d) < 1e-6) break;
			t -= err / d;
		}
		let lo = 0;
		let hi = 1;
		t = x;
		for (let i = 0; i < 30; i++) {
			const v = bx(t);
			if (Math.abs(v - x) < 1e-5) break;
			if (v < x) lo = t;
			else hi = t;
			t = (lo + hi) / 2;
		}
		return by(t);
	};
}

/**
 * A full-height sheet coming up or going down: iOS's sheet curve. Unlike the app's default
 * `--ease-smooth-out` (quint-like), it does not spend three quarters of the distance in the first
 * few frames, which on a view the height of the window reads as a jump rather than a slide.
 */
export const sheet = cubicBezier(0.32, 0.72, 0, 1);

/**
 * A full-window view sliding up from below, fading in as it comes, and back down on the way out.
 *
 * What `fly={{ y: '100%' }}` does, without the one thing that made it slow here: `fly` starts by
 * reading `getComputedStyle(node)` for the element's own opacity and transform, which forces a
 * style and layout pass over the whole view on the first frame. Profiled on the player view's
 * collapse (2026-09-17), that read was 26 of the 34 ms of script the collapse cost. The views this
 * is for have no transform or opacity of their own to preserve, so there is nothing to read.
 */
export function slideUp(
	_node: Element,
	{ duration = 460, easing = sheet }: { duration?: number; easing?: (t: number) => number } = {}
): TransitionConfig {
	return {
		duration,
		easing,
		css: (t, u) => `transform: translateY(${(u * 100).toFixed(3)}%); opacity: ${t.toFixed(3)}`
	};
}

/**
 * A line of text replaced by another in place — a track title changing, say. The transition tokens'
 * text swap: 150 ms, ease-in-out, rising 4px out of a 2px blur. Only the incoming text animates;
 * the outgoing one is gone on the same frame, since two titles overlapping is the one thing a
 * swap must not show.
 */
export function textSwap(_node: Element): TransitionConfig {
	return {
		duration: 150,
		easing: (t) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2),
		css: (t, u) =>
			`opacity: ${t}; transform: translateY(${(u * 4).toFixed(2)}px); filter: blur(${(u * 2).toFixed(2)}px)`
	};
}
