<script lang="ts" module>
	let webgl: boolean | undefined;
	/** A real GPU context is to be had (not a software rasteriser). Probed once per page. */
	function webglAvailable(): boolean {
		if (webgl === undefined) {
			try {
				const c = document.createElement('canvas');
				const gl = c.getContext('webgl2', { failIfMajorPerformanceCaveat: true });
				webgl = !!gl;
				gl?.getExtension('WEBGL_lose_context')?.loseContext();
			} catch {
				webgl = false;
			}
		}
		return webgl;
	}
</script>

<script lang="ts">
	import { untrack } from 'svelte';
	import { fade, scale } from 'svelte/transition';
	import * as api from '$lib/api';
	import { playback, prefs } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';
	import { MediaClock } from '$lib/mediaclock';
	import LyricsOffset from './LyricsOffset.svelte';
	import { durationSecs, lyricsFor, peekLyrics, watchingLyrics } from '$lib/prefetch.svelte';

	// `expanded` only sizes the type and centres the column. The owner of the extra room (the side
	// panel, or the now-playing view) decides how much there is. Toggling it must not remount this
	// component, or the lyrics refetch and the scroll position is lost.
	// `compact` is the mini-player: a ~220px column with no room for the source footer or a
	// scrollbar. It only shrinks the type and chrome; the sync/auto-scroll logic is identical.
	// `page` is the lyrics page over the main panel: a left-aligned column of readable width, in the
	// side panel's type (the owner paints the background).
	let {
		expanded = false,
		compact = false,
		page = false
	}: { expanded?: boolean; compact?: boolean; page?: boolean } = $props();

	let lyrics = $state<api.Lyrics | null>(null);
	let loading = $state(true);
	let scroller: HTMLElement | undefined = $state();

	// videoId of the fetch whose result is (or will be) shown — guards stale responses.
	let requested = '';

	$effect(() => {
		const now = playback.now;
		if (!now) {
			requested = '';
			lyrics = null;
			loading = false;
			return;
		}
		if (now.videoId === requested) return;
		const id = (requested = now.videoId);
		// Usually already here: the track before this one fetched them (prefetch.svelte.ts). Then
		// there is no loading state at all, and the lyrics change is one swap rather than
		// lyrics → skeleton → lyrics.
		const warm = untrack(() => peekLyrics(id));
		if (warm !== undefined) {
			lyrics = warm;
			loading = false;
			hasScrolled = false;
			return;
		}
		loading = true;
		lyrics = null;
		// Album isn't in now-playing, but the queue item usually has it — better LRCLIB matching.
		const album = playback.queue.items[playback.queue.currentIndex]?.album;
		lyricsFor({
			videoId: id,
			title: now.title,
			artists: now.artists,
			album: album ?? undefined,
			// The track's own length — NOT playback.duration, which still holds the previous
			// track's value for a moment after a track change.
			duration: durationSecs(now.duration)
		})
			.then((l) => {
				if (requested !== id) return;
				lyrics = l;
				loading = false;
				hasScrolled = false; // first positioning on a new track is an instant jump
			})
			.catch(() => {
				if (requested !== id) return;
				loading = false;
			});
	});

	const lines = $derived(lyrics?.synced ? lyrics.lines : []);

	// While this view is up, the next track's lyrics are worth fetching ahead (prefetch.svelte.ts).
	$effect(() => watchingLyrics());

	// A fetch that lands inside a beat never shows a skeleton: flashing one for 100 ms between two
	// sets of lyrics reads as a flicker, not as loading. Past the beat it is real waiting.
	let skeletonDue = $state(false);
	$effect(() => {
		if (!loading) {
			skeletonDue = false;
			return;
		}
		const timer = setTimeout(() => (skeletonDue = true), 250);
		return () => clearTimeout(timer);
	});
	// Whether the lyrics on screen are the WebGL kind, as of the last answer. The canvas stays
	// mounted through the next track's loading instead of being torn down for a skeleton and built
	// again — a new WebGL context per track change was the longest frame of the change (up to
	// 229 ms) — and the stage fades the old lines out and the new ones in.
	let canvasShown = $state(false);
	$effect(() => {
		if (!loading) canvasShown = !!(lyrics?.synced && !lyrics.instrumental);
	});

	// Word-synced lyrics are drawn with WebGL wherever there is room and a GPU (`LyricsCanvas`,
	// loaded on first use so Pixi stays out of the startup bundle); the DOM rows below are the
	// fallback, and still what the mini player uses.
	type CanvasProps = typeof import('./LyricsCanvas.svelte').default;
	let Canvas = $state.raw<CanvasProps | null>(null);
	let canvasFailed = $state(false);
	const useCanvas = $derived(!compact && !canvasFailed && webglAvailable());
	$effect(() => {
		if (!useCanvas || Canvas || !lines.length) return;
		import('./LyricsCanvas.svelte')
			.then((mod) => (Canvas = mod.default))
			.catch(() => (canvasFailed = true));
	});

	// --- time -------------------------------------------------------------------------------
	//
	// How this view keeps time and draws motion is the whole difference between smooth and not, so
	// it is worth being exact about what was measured.
	//
	// The first version rebased an interpolated position on each of mpv's ~4 Hz reports. Each rebase
	// moved the clock by 17 ms on average, up to 115 ms, a quarter of them backwards — a sweep edge
	// jolting 20 px four times a second. `MediaClock` now turns those reports into a steady estimate
	// (see its header): p99 ≈ 11 ms of error, never a visible step back.
	//
	// How the motion is drawn was measured too, on one fixed stretch of one song, counting frames on
	// which the sung line visibly changed: the old per-frame Svelte render 70%; Web Animations and a
	// hand-driven `transform` sweep 26–40%, stalling mid-word over a dozen times in eight seconds
	// (WebKitGTK does not reliably repaint a transform change under the rows' blur); a gradient whose
	// position is written each frame 67%, with the soft edge the old one lacked. So: one frame loop
	// reads the clock and writes to the few things actually moving — the sung word's `--p`, a wave
	// settling, the dots — only while one of them is, and never through Svelte state.

	const clock = new MediaClock();

	let lastReportAt = -1;
	let lastOffset = 0;
	$effect(() => {
		// The lyrics offset shifts the clock the lyrics read, not the playback it follows: a line cued
		// at T lights up when the audio reaches T + offset. Seeking still goes to the cue itself.
		const offset = prefs.lyricsOffsetMs / 1000;
		const position = playback.position - offset;
		const at = playback.positionAt;
		const speed = playback.speed;
		const paused = playback.paused;
		untrack(() => {
			const now = performance.now();
			const shifted = offset !== lastOffset;
			lastOffset = offset;
			const fresh = at !== lastReportAt || shifted;
			lastReportAt = at;
			if (shifted) clock.reset();
			if (fresh) {
				clock.feed({ position, at: at || now, speed, paused });
			} else if (paused !== clock.isPaused || speed !== clock.rate) {
				// Pause, resume or a tempo change that no report has spoken for yet. Continue from where
				// the clock already is rather than from the last report, which is up to 250 ms old;
				// provisional, so mpv's next report replaces the guess instead of being filtered by it.
				clock.feed({ position: clock.valueAt(now), at: now, speed, paused, provisional: true });
			} else {
				return;
			}
			step();
		});
	});

	/** Media milliseconds now, per the clock. */
	function nowMs(): number {
		return clock.valueAt(performance.now()) * 1000;
	}

	// --- where the song is ----------------------------------------------------------------------

	let activeIndex = $state(-1);

	/** How long a hole between cues has to be before it reads as an interlude rather than a breath.
	 *  Apple shows its three dots over roughly this; shorter than it and they would flash on every
	 *  verse break. */
	const INTERLUDE_MS = 5000;

	/** The instrumental stretch the playhead is in, anchored on the line it precedes. Covers the
	 *  intro too — the stretch before the first cue is the one every lyrics view leaves blank, and
	 *  it is the longest silence in most songs. */
	let interlude = $state<{ at: number; from: number; to: number } | null>(null);

	function lineEnd(i: number): number {
		const l = lines[i];
		if (!l) return 0;
		const lastWord = l.words?.[l.words.length - 1];
		return lastWord?.end_ms ?? l.end_time_ms ?? l.time_ms ?? 0;
	}

	function lineAt(ms: number): number {
		let i = -1;
		for (let j = 0; j < lines.length; j++) {
			const cue = lines[j].time_ms;
			if (cue === undefined) continue;
			if (cue > ms) break;
			i = j;
		}
		return i;
	}

	function interludeAt(i: number, ms: number) {
		const to = lines[i + 1]?.time_ms;
		if (to === undefined) return null;
		const from = i < 0 ? 0 : lineEnd(i);
		if (to - from < INTERLUDE_MS || ms < from || ms >= to) return null;
		return { at: i + 1, from, to };
	}

	let timer: ReturnType<typeof setTimeout> | undefined;

	/**
	 * Bring the discrete state up to the clock — which line, whether an interlude, which words are
	 * sung — then sleep until the next moment one of those changes. The moments are all in the data,
	 * so this is a timer aimed at the next one, not a loop polling for it. The continuous part (the
	 * sweep itself) is the frame loop's, and this kicks it.
	 */
	function step() {
		clearTimeout(timer);
		timer = undefined;
		if (!lines.length) return;
		const ms = nowMs();
		const i = lineAt(ms);
		if (i !== activeIndex) activeIndex = i;
		const gap = interludeAt(i, ms);
		if ((gap?.at ?? -1) !== (interlude?.at ?? -1) || gap?.from !== interlude?.from) interlude = gap;
		markWords(ms);
		kick();
		if (clock.isPaused) return;
		const next = nextChange(i, ms);
		if (next === Infinity) return;
		// +2 ms so the timer lands just past the boundary rather than a hair before it.
		timer = setTimeout(step, Math.max(4, (next - ms) / clock.rate + 2));
	}

	function nextChange(i: number, ms: number): number {
		let next = Infinity;
		const cue = lines[i + 1]?.time_ms;
		if (cue !== undefined && cue > ms) next = cue;
		const from = i < 0 ? 0 : lineEnd(i);
		if (from > ms && cue !== undefined && cue - from >= INTERLUDE_MS) next = Math.min(next, from);
		for (const w of lines[i]?.words ?? []) {
			if (w.start_ms > ms) next = Math.min(next, w.start_ms);
			else if (isHeld(w) && w.end_ms > ms) next = Math.min(next, w.end_ms);
		}
		return next;
	}

	$effect(() => () => {
		clearTimeout(timer);
		cancelAnimationFrame(frameId);
	});

	// A new set of lyrics is a new timeline for everything anchored to the old one.
	$effect(() => {
		void lines;
		untrack(() => {
			retireAll();
			activeIndex = -1;
			interlude = null;
			step();
		});
	});

	// --- motion ---------------------------------------------------------------------------------
	//
	// A word is one span. On the sung line its text is painted with a gradient (clipped to the
	// glyphs) whose soft edge sits at `--p`, the word's progress, which the frame loop writes. The
	// styling lives in the component stylesheet below; this side only decides the numbers.

	type Slot = { el: HTMLElement; start: number; end: number; p: number };
	type Dot = { el: HTMLElement; start: number; end: number; p: number };
	type Wave = { from: number; at: number; delay: number };

	/** The sung line's words, in order. */
	let slots: Slot[] = [];
	let slotLine = -1;
	let dots: Dot[] = [];
	const waving = new Map<HTMLElement, Wave>();
	let frameId = 0;

	/** Rows settle into place over the slow-emphasis token, staggered 28 ms a line below the sung
	 *  one. Read once: the tokens are static. */
	const WAVE_MS =
		typeof document === 'undefined'
			? 500
			: parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--duration-very-slow')) || 500;
	const STAGGER_MS = 28;

	/** `--ease-smooth-out` is `cubic-bezier(0.22, 1, 0.36, 1)`, the standard ease-out-quint curve, so
	 *  the rows below are the same motion the rest of the app's tokens describe. */
	const easeOut = (t: number) => 1 - Math.pow(1 - t, 5);

	function kick() {
		if (!frameId) frameId = requestAnimationFrame(frame);
	}

	function frame(ts: number) {
		frameId = 0;
		if (paint(clock.valueAt(ts) * 1000, ts)) kick();
	}

	const clamp01 = (x: number) => (x < 0 ? 0 : x > 1 ? 1 : x);

	/** Write everything that moves for media time `ms` at wall time `ts`. True while anything is
	 *  still in motion — the loop's only condition for running another frame. */
	function paint(ms: number, ts: number): boolean {
		const playing = !clock.isPaused;
		let moving = false;

		for (const s of slots) {
			const p = s.end > s.start ? clamp01((ms - s.start) / (s.end - s.start)) : ms >= s.start ? 1 : 0;
			if (p !== s.p) {
				s.p = p;
				s.el.style.setProperty('--p', p.toFixed(4));
			}
			if (playing && p < 1) moving = true;
		}

		for (const d of dots) {
			const p = clamp01((ms - d.start) / (d.end - d.start));
			if (Math.abs(p - d.p) > 0.002) {
				d.p = p;
				d.el.style.opacity = String(0.25 + p * 0.75);
				d.el.style.transform = `scale(${0.8 + p * 0.35})`;
			}
			if (playing && p < 1) moving = true;
		}

		for (const [el, w] of waving) {
			const t = (ts - w.at - w.delay) / WAVE_MS;
			if (t >= 1) {
				el.style.transform = '';
				waving.delete(el);
				continue;
			}
			el.style.transform = `translateY(${t <= 0 ? w.from : w.from * (1 - easeOut(t))}px)`;
			moving = true;
		}
		return moving;
	}

	/** Row elements by line index, kept by an attachment so nothing queries the whole list. */
	const rowEls: (HTMLElement | undefined)[] = [];
	/** State, not a plain variable: the focus effect has to hear when the dots arrive, because they
	 *  mount in the same update that sets the interlude, after that effect has already looked. */
	let dotsEl: HTMLElement | undefined = $state();

	function rowAttach(i: number) {
		return (el: HTMLElement) => {
			rowEls[i] = el;
			return () => {
				if (rowEls[i] === el) rowEls[i] = undefined;
				waving.delete(el);
			};
		};
	}

	let activeWordEls: HTMLElement[] = [];

	/** A word long enough to be held rather than passed through — Apple lets those glow. */
	function isHeld(w: api.LyricWord): boolean {
		return w.end_ms - w.start_ms >= 900;
	}

	function clearSlots(list: Slot[]) {
		for (const s of list) s.el.style.removeProperty('--p');
	}

	function retireAll() {
		clearSlots(slots);
		for (const el of rowEls) el?.removeAttribute('data-lit');
		slots = [];
		slotLine = -1;
		for (const el of activeWordEls) delete el.dataset.glow;
		activeWordEls = [];
	}

	/** Hand the sweep from the line that was sung to the one that is. */
	function activate(i: number) {
		if (i === slotLine) return;
		const leaving = slots;
		const leavingLine = slotLine;
		for (const el of activeWordEls) delete el.dataset.glow;
		slots = [];
		activeWordEls = [];
		slotLine = i;
		// Only a lit row paints its words with the sweep gradient; every other row is plain text.
		// The rows around it are blurred, and blurring renders a row's content off-screen first, so
		// the less there is to render in them the better — and a gradient on 60 lines nobody is
		// singing is 60 things to render for nothing.
		const leavingRow = rowEls[leavingLine];
		rowEls[i]?.setAttribute('data-lit', '');
		// The leaving line keeps its gradient while its colour eases to the dim one (the gradient is
		// drawn in `currentColor`, so it eases along); only then is it dropped, so nothing snaps.
		// Unless that same line has been sung again meanwhile (a seek back into it): its words are
		// then live slots once more, and resetting them would wipe the part already swept.
		setTimeout(() => {
			if (slotLine === leavingLine) return;
			clearSlots(leaving);
			leavingRow?.removeAttribute('data-lit');
		}, 520);

		const row = rowEls[i];
		const words = lines[i]?.words;
		if (!row || !words?.length) return;
		row.querySelectorAll<HTMLElement>('[data-w]').forEach((el) => {
			const k = Number(el.dataset.w);
			const w = words[k];
			if (!w) return;
			activeWordEls[k] = el;
			slots.push({ el, start: w.start_ms, end: w.end_ms, p: -1 });
		});
		const ms = nowMs();
		markWords(ms);
		paint(ms, performance.now());
		kick();
	}

	/** The one word state that is a class, not a position: glowing while held. */
	function markWords(ms: number) {
		const words = lines[slotLine]?.words;
		if (!words) return;
		for (let k = 0; k < activeWordEls.length; k++) {
			const el = activeWordEls[k];
			const w = words[k];
			if (!el || !w) continue;
			if (isHeld(w) && ms >= w.start_ms && ms < w.end_ms) el.dataset.glow = '';
			else delete el.dataset.glow;
		}
	}

	// Runs after the DOM has the new active row, which the sweep needs to find its words.
	$effect(() => {
		const i = activeIndex;
		void lines;
		untrack(() => activate(i));
	});

	function dotsAttach(el: HTMLElement) {
		dotsEl = el;
		const gap = untrack(() => interlude);
		if (gap) {
			const third = (gap.to - gap.from) / 3;
			dots = [...el.querySelectorAll<HTMLElement>('[data-dot]')].map((d, k) => ({
				el: d,
				start: gap.from + k * third,
				end: gap.from + (k + 1) * third,
				p: -1
			}));
			kick();
		}
		return () => {
			if (dotsEl === el) dotsEl = undefined;
			dots = [];
			waving.delete(el);
		};
	}

	// --- focus and scroll -----------------------------------------------------------------------
	//
	// Apple does not glide a scrollbar. The list jumps to where it is going and each line then eases
	// the rest of the way in, the ones further down a beat later than the ones above — the wave.
	// Done the same way here: `scrollTop` is set outright and every visible row is given the offset
	// it *appeared* to be at, which the frame loop eases back to zero with a stagger. `offsetTop`
	// rather than a bounding rect for the maths, so a wave still settling from the last line cannot
	// skew where the next one aims.

	const reducedMotion =
		typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

	// Auto-scroll pauses while the user is scrolling (wheel/touch/scrollbar), resumes after 3s.
	// Tracked via input events, not `scroll`, so our own scrolling doesn't trip it.
	let userScrollUntil = 0;
	let hasScrolled = false;
	/** While the user reads around, nothing is blurred: depth of field is for following along. */
	let browsing = $state(false);
	let browseTimer: ReturnType<typeof setTimeout> | undefined;
	function onUserScroll() {
		userScrollUntil = Date.now() + 3000;
		browsing = true;
		clearTimeout(browseTimer);
		browseTimer = setTimeout(() => (browsing = false), 3000);
		// Land any wave in flight: a row still easing in would slide under the pointer.
		for (const el of waving.keys()) el.style.transform = '';
		waving.clear();
	}
	$effect(() => () => clearTimeout(browseTimer));

	/** Where a row currently appears relative to its resting place, mid-wave. */
	function offsetOf(el: HTMLElement, ts: number): number {
		const w = waving.get(el);
		if (!w) return 0;
		const t = (ts - w.at - w.delay) / WAVE_MS;
		return t <= 0 ? w.from : t >= 1 ? 0 : w.from * (1 - easeOut(t));
	}

	function focus(target: HTMLElement, animate: boolean, anchor: number) {
		if (!scroller) return;
		const bias = expanded ? 0.38 : 0.5;
		const max = scroller.scrollHeight - scroller.clientHeight;
		const top = Math.min(
			max,
			Math.max(0, target.offsetTop - (scroller.clientHeight - target.offsetHeight) * bias)
		);
		const delta = top - scroller.scrollTop;
		if (Math.abs(delta) < 0.5) return;

		const rows: [number, HTMLElement][] = [];
		rowEls.forEach((el, k) => el && rows.push([k, el]));
		if (dotsEl) rows.push([anchor + 0.5, dotsEl]);

		const ts = performance.now();
		if (!animate || reducedMotion) {
			for (const [, el] of rows) el.style.transform = '';
			waving.clear();
			scroller.scrollTop = top;
			return;
		}

		const from = scroller.scrollTop;
		const viewTop = Math.min(from, top) - 80;
		const viewBottom = Math.max(from, top) + scroller.clientHeight + 80;
		for (const [k, el] of rows) {
			const carried = offsetOf(el, ts);
			const y = el.offsetTop;
			if (y + el.offsetHeight <= viewTop || y >= viewBottom) {
				waving.delete(el);
				el.style.transform = '';
				continue;
			}
			const below = k - anchor;
			waving.set(el, {
				from: carried + delta,
				at: ts,
				delay: below > 0 ? Math.min(below, 8) * STAGGER_MS : 0
			});
		}
		scroller.scrollTop = top;
		// Paint the offsets in the same task as the scroll, so no frame shows the list jumped
		// without them.
		paint(nowMs(), ts);
		kick();
	}

	let wasExpanded: boolean | undefined;

	$effect(() => {
		const i = activeIndex;
		const gap = interlude;
		const dots = dotsEl;
		void lines;
		// Re-centre after the layout width/font changes, and jump rather than glide across it.
		// (Also fires on the first run, where both values are already at their defaults.)
		if (expanded !== wasExpanded) {
			wasExpanded = expanded;
			hasScrolled = false;
			userScrollUntil = 0;
		}
		untrack(() => {
			if (!scroller || Date.now() < userScrollUntil) return;
			// During an interlude the dots are what is being "sung", so they take the focus. Before the
			// first line with no dots to show, the top of the lyrics does: seeking back to the intro
			// used to leave the view parked wherever the song had been.
			const target = gap && dots ? dots : i >= 0 ? rowEls[i] : rowEls[0];
			if (!target) return;
			focus(target, hasScrolled, i);
			hasScrolled = true;
		});
	});

	function seekTo(line: api.LyricLine) {
		if (line.time_ms === undefined) return;
		const secs = line.time_ms / 1000;
		playback.position = secs; // optimistic — the mpv tick confirms
		playback.positionAt = performance.now();
		userScrollUntil = 0; // jump the view along with the seek
		api.seek(secs);
	}

	/** Apple's depth of field: the further a line is from the one being sung, the further back it
	 *  sits — dimmer, and in the big view softer. Blur is spent only within eight lines of the
	 *  sung one, which is more than a panel shows; `filter` puts each element it touches on its own
	 *  layer, and this repo has already had to pull backdrop filters out of the home feed over it. */
	function depth(distance: number): string {
		if (distance === 0) return '';
		if (browsing) return 'opacity:0.85';
		// Starts well below the sung line's unsung words (55% of full colour): at 0.84 the next line
		// read as bright as the words still to come on the current one, so nothing marked which
		// line was being sung until the sweep reached it.
		const fade = Math.max(0.25, 0.72 - (distance - 1) * 0.12);
		const blur =
			expanded && distance <= 8
				? `;filter:blur(${Math.min(distance * 0.6, 2.4).toFixed(2)}px)`
				: '';
		return `opacity:${fade.toFixed(2)}${blur}`;
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -- handlers only detect scroll intent -->
<div
	bind:this={scroller}
	onwheel={onUserScroll}
	ontouchmove={onUserScroll}
	onpointerdown={onUserScroll}
	class="relative min-h-0 flex-1 overflow-y-auto {compact
		? 'px-2 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden'
		: expanded
			? 'px-10 py-6'
			: page
				? 'px-14 py-8'
				: 'px-5 py-6'}"
>
	{#snippet skeleton()}
		<div class="space-y-3" in:fade={{ duration: 150 }}>
			{#each { length: 8 } as _, i (i)}
				<div class="h-5 animate-pulse rounded bg-muted" style="width:{55 + ((i * 17) % 40)}%"></div>
			{/each}
		</div>
	{/snippet}
	{#if useCanvas && (loading ? canvasShown : !!lyrics?.synced && !lyrics.instrumental)}
		{#if Canvas}
			<Canvas
				{lines}
				active={activeIndex}
				{interlude}
				{clock}
				{expanded}
				{page}
				onseek={seekTo}
				onfail={() => (canvasFailed = true)}
			/>
		{/if}
		{#if loading && skeletonDue}
			<div class="absolute inset-0 {expanded ? 'px-10 py-6' : page ? 'px-14 py-8' : 'px-5 py-6'}">
				{@render skeleton()}
			</div>
		{/if}
	{:else if loading}
		{#if skeletonDue}{@render skeleton()}{/if}
	{:else if lyrics?.instrumental}
		<p class="py-8 text-center text-lg text-muted-foreground">{t('lyrics.instrumental')} ♪</p>
	{:else if lyrics && lyrics.synced}
		<!-- Bottom padding only, so the last lines can still center-scroll. A matching top padding
		     would put half a panel of void above line 1, which is all you see until the song has
		     played far enough to scroll past it (issue #201). Instead the opening lines sit at the
		     top and centering starts once there is room above, the way every other lyrics view
		     behaves. -->
		<div class="pb-[55vh] {expanded ? 'mx-auto max-w-3xl' : page ? 'max-w-2xl' : ''}">
			{#each lines as line, i (i)}
				{@const isActive = i === activeIndex}
				{#if interlude?.at === i}
					<!-- The three dots Apple shows over an instrumental stretch: each fills over its third
					     of the gap, and the group breathes while it waits. -->
					<!-- Two boxes because both want `transform`: the outer one takes the wave with the other
					     rows, the inner one breathes. On one element the wave would override the breath
					     and hand it back with a jump when it finished. -->
					<div
						class={expanded ? 'py-4' : 'py-2'}
						out:scale={{ start: 0.6, duration: 300 }}
						{@attach dotsAttach}
					>
						<div
							class="lyr-dots flex w-fit items-center {expanded ? 'gap-2.5' : 'gap-2'}"
							style:animation-play-state={playback.paused ? 'paused' : 'running'}
						>
							{#each [0, 1, 2] as dot (dot)}
								<span
									data-dot={dot}
									class="rounded-full bg-foreground {expanded ? 'size-3' : 'size-2'}"
								></span>
							{/each}
						</div>
					</div>
				{/if}
				<div
					class="lyr-row"
					data-active={isActive || undefined}
					style={depth(Math.abs(i - activeIndex))}
					{@attach rowAttach(i)}
				>
					<button
						onclick={() => seekTo(line)}
						class="lyr-line block w-full origin-left cursor-pointer text-left font-heading font-bold
							{expanded
							? 'py-3 text-[clamp(1.75rem,3.2vw,2.75rem)] leading-[1.18] tracking-[-0.02em]'
							: compact
								? 'py-1 text-sm leading-snug'
								: 'py-2 text-xl leading-snug'}
							{isActive
								? page
									? 'text-white'
									: 'text-foreground'
								: page
									? 'text-white/60'
									: 'text-muted-foreground'}"
					>
						{#if line.words && line.words.length > 0}
							<span class="flex flex-wrap items-baseline">
								{#each line.words as word, wIdx (wIdx)}
									{@const text = word.text.trimEnd()}
									<span
										class="lyr-word {word.text.endsWith(' ') ? 'mr-[0.26em]' : ''}"
										data-w={wIdx}>{text}</span
									>
								{/each}
							</span>
						{:else}
							<span>{line.text || '♪'}</span>
						{/if}

						{#if line.translation}
							<p class="mt-1 text-sm font-normal italic tracking-wide opacity-80">
								{line.translation}
							</p>
						{/if}
					</button>
				</div>
			{/each}
		</div>
	{:else if lyrics}
		<div
			class="space-y-2 leading-relaxed {page ? 'text-white/90' : 'text-foreground/90'} {expanded
				? 'mx-auto max-w-3xl text-xl'
				: compact
					? 'text-xs'
					: page
						? 'max-w-2xl text-lg'
						: 'text-base'}"
		>
			{#each lyrics.lines as line, i (i)}
				{#if line.text}
					<div>
						<p>{line.text}</p>
						{#if line.translation}
							<p class="text-xs italic text-muted-foreground">{line.translation}</p>
						{/if}
					</div>
				{:else}
					<div class="h-4"></div>
				{/if}
			{/each}
		</div>
	{:else}
		<p class="py-8 text-center text-sm text-muted-foreground">{t('lyrics.none_found')}</p>
	{/if}
</div>
{#if lyrics && !loading && !compact}
	<!-- The offset control sits with the attribution: both are about where these lyrics came from
	     and how well they fit. It stays out of the way until the pointer is there, and stays shown
	     while it is set to anything but zero, so a shifted view never looks like a sync bug. -->
	<div class="group/lyrfoot flex min-h-10 items-center justify-between gap-3 px-4 py-1.5 text-xs text-muted-foreground">
		<p class="min-w-0 truncate">
			{lyrics.source.startsWith('Source:') ? lyrics.source : t('lyrics.source', { source: lyrics.source })}
		</p>
		{#if lyrics.synced && !lyrics.instrumental}
			<div
				class="shrink-0 transition-opacity duration-[var(--duration-fast)] focus-within:opacity-100 group-hover/lyrfoot:opacity-100 {prefs.lyricsOffsetMs ===
				0
					? 'opacity-0'
					: ''}"
			>
				<LyricsOffset size="sm" />
			</div>
		{/if}
	</div>
{/if}

<style>
	/* Focus changes ride the motion tokens: depth (opacity, blur) and the sung line's colour all
	   ease together, so a line coming into focus is one movement, not several. */
	.lyr-row {
		transition:
			opacity var(--duration-slow) var(--ease-smooth-out),
			filter var(--duration-slow) var(--ease-smooth-out);
	}
	.lyr-row:hover {
		opacity: 1 !important;
		filter: none !important;
	}
	.lyr-line {
		transition: color var(--duration-slow) var(--ease-smooth-out);
	}

	/* No scale on the sung line and no lift on sung words here, though both are the look. WebKitGTK
	   re-rasterises DOM text on the pixel grid every frame of a sub-pixel transform, so each of them
	   read as the lyrics trembling: a frame burst of a line change showed the band stepping ±0.4–1 px
	   back and forth for 37 frames. The WebGL view (`LyricsCanvas`) has both; this is its fallback. */
	.lyr-word {
		display: inline-block;
		white-space: nowrap;
	}

	/* The sweep, on lit rows only (the sung line, and the one it just left while that fades).
	   `--p` is the word's progress, written by the frame loop. The edge is a soft band 0.7em wide
	   rather than a hard stop, and it is walked from just before the word to just past it, so 0
	   is fully unsung and 1 fully sung with no half-lit first letter.

	   Both stops are `currentColor`, so when a line stops being sung and its colour eases to the
	   dim one, the sung text eases with it; dropping the gradient afterwards changes nothing on
	   screen.

	   A gradient rather than a moving transform, and that was measured, not assumed: with depth-of-
	   field blur on the rows, WebKitGTK repainted a transform-driven sweep on 22% of frames and
	   stalled mid-word 14 times in 8 seconds, while a paint-driven one updated on 63–65%. */
	:global([data-lit]) .lyr-word {
		--edge: calc(var(--p, 0) * (100% + 0.7em) - 0.35em);
		background-image: linear-gradient(
			90deg,
			currentColor calc(var(--edge) - 0.35em),
			color-mix(in oklab, currentColor 55%, transparent) calc(var(--edge) + 0.35em)
		);
		-webkit-background-clip: text;
		background-clip: text;
		-webkit-text-fill-color: transparent;
	}
	/* Only held notes glow, and only while held: a glow on every word is a line that glows. A shadow
	   still draws under transparent fill — it comes off the glyph outline, not the paint. */
	:global([data-lit]) .lyr-word:global([data-glow]) {
		text-shadow: 0 0 0.45em color-mix(in oklab, currentColor 50%, transparent);
	}
	:global([data-lit]) .lyr-word {
		transition: text-shadow var(--duration-slow) ease;
	}

	.lyr-dots {
		transform-origin: left center;
		animation: lyr-breathe 1.6s ease-in-out infinite alternate;
	}
	@keyframes lyr-breathe {
		from {
			transform: scale(1);
		}
		to {
			transform: scale(1.08);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.lyr-dots {
			animation: none;
		}
		.lyr-word,
		.lyr-line {
			transition: none;
		}
	}
</style>
