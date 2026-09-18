<script lang="ts" module>
	/** Lines that have had their automatic pass this session, oldest first. Module-level so a
	 *  remount doesn't replay one: the right panel remounts its title every time it opens, and the
	 *  player bar remounts both of its lines on every track change, a repeat of the same one
	 *  included. Keyed by the text alone, so a title that already went by in the player bar stays
	 *  still when the right panel opens on it afterwards (hovering still scrolls it). Capped, oldest
	 *  out: forgetting a line from two hundred tracks ago costs it one more pass, nothing else. */
	const played = new Set<string>();
	const PLAYED_MAX = 200;

	function remember(line: string) {
		if (played.has(line)) return;
		if (played.size >= PLAYED_MAX) played.delete(played.values().next().value!);
		played.add(line);
	}
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import { playback } from '$lib/player.svelte';

	let {
		text,
		children,
		class: cls = ''
	}: {
		/** The line to scroll. A new value starts over: still, then a pass of its own. */
		text: string;
		/** Renders in place of plain `text` (artist links); `text` still identifies the line. */
		children?: Snippet;
		class?: string;
	} = $props();

	/** Blank space between the text and the copy chasing it, in px. */
	const GAP = 40;
	/** In px/s. The duration follows from it, so a long line takes longer instead of moving faster. */
	const SPEED = 30;
	/** How long a new line holds still before its pass, in ms: long enough to read how it starts,
	 *  and for the track change's own motion to settle before this adds more. */
	const DELAY = 2000;
	/** How long the pointer rests on a still line before it scrolls, in ms. A pointer crossing the
	 *  player bar on its way to the controls doesn't set it off, and one that came to click an
	 *  artist gets its click in before anything moves. */
	const HOVER_DELAY = 500;

	let box = $state<HTMLElement>();
	/** Full text width in px once it is wider than the box; 0 when it fits and nothing scrolls. */
	let width = $state(0);
	/** What the line is scrolling for right now, or null while it is still. The scrolling track only
	 *  exists for the length of a pass, since every frame it runs composites the whole window
	 *  (layout.css has the cost); the rest of the time the line is a plain ellipsis. */
	let pass = $state<'auto' | 'hover' | null>(null);
	let hovered = $state(false);
	let hoverTimer: ReturnType<typeof setTimeout> | undefined;

	/** An automatic pass stands still while the music is paused, and while the pointer is on it:
	 *  the artist line's names are links, and a moving one is hard to hit. Held rather than
	 *  ended, so it doesn't snap back, and a held animation has no frames left to draw. A hover
	 *  pass is the one the pointer asked for, so neither holds it. */
	const held = $derived(pass === 'auto' && (playback.paused || hovered));

	/** `text` compared by value. A caller can hand over a prop that re-reads a whole object, such as
	 *  the right panel's `now.title`, so a new `playback.now` carrying the same title would otherwise
	 *  re-run everything below: a pass in progress snaps back to the ellipsis and waits out the
	 *  delay again. That happens on an ordinary play, when the real track replaces the stand-in
	 *  shown while it resolved. A derived that comes out the same string wakes nothing. */
	const line = $derived(text);

	$effect(() => {
		line; // remeasure whenever the line changes
		pass = null; // and start the new one still, to wait for its own pass
		const el = box;
		if (!el) return;
		const measure = () => {
			const span = el.querySelector('[data-mq]');
			// A user who asked the OS to minimise motion gets the plain truncated line.
			const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
			const w = span ? span.scrollWidth : 0;
			const next = !still && w > el.clientWidth + 1 ? w : 0;
			width = next;
			if (!next) pass = null; // widened mid-pass: nothing left to scroll
		};
		measure();
		// The player bar's title column stretches with the window, so a resize can hide or
		// reveal the overflow on its own.
		const ro = new ResizeObserver(measure);
		ro.observe(el);
		return () => ro.disconnect();
	});

	// The automatic pass: one per line per session (see `played`), a moment after the line appears,
	// and not while the music is paused, so a restored queue sitting paused at startup doesn't
	// scroll at all until it plays.
	$effect(() => {
		if (!width || pass || playback.paused || played.has(line)) return;
		const t = setTimeout(() => (pass = 'auto'), DELAY);
		return () => clearTimeout(t);
	});

	// Resting the pointer on a still line scrolls it once more, for anyone who wants to read the rest
	// after the automatic pass has gone by. Once per visit: a pointer parked on the title would
	// otherwise keep the window compositing for as long as it sat there. A line already moving is
	// left to `held`; an automatic pass that only looks still (paused with the music, or begun
	// during the wait and held by this very pointer) is taken over, so it reads the same as a
	// still line.
	function enter() {
		hovered = true;
		const moving = pass === 'hover' || (pass === 'auto' && !playback.paused);
		if (!width || moving) return;
		hoverTimer = setTimeout(() => {
			if (width) pass = 'hover';
		}, HOVER_DELAY);
	}
	function leave() {
		hovered = false;
		clearTimeout(hoverTimer);
	}
	$effect(() => () => clearTimeout(hoverTimer));

	/** The pass is over: the line goes back to its ellipsis, and won't scroll by itself again. */
	function ended(e: AnimationEvent) {
		if (e.target !== e.currentTarget) return;
		remember(line);
		pass = null;
	}
</script>

{#snippet body()}{#if children}{@render children()}{:else}{text}{/if}{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions -- hover only starts or holds the scroll -->
<div
	bind:this={box}
	class="overflow-hidden {cls}"
	onpointerenter={enter}
	onpointerleave={leave}
>
	{#if width && pass}
		<!-- The copy is what the pass scrolls into view: by the time the first one has moved its own
		     width plus the gap, the second sits exactly where it started, so swapping this back for
		     the ellipsis below when the pass ends is not a visible jump, and the line only ever
		     travels one way. `inert` keeps any links in the copy out of the tab order and
		     unclickable. The gap is a margin, not padding, so a hover underline drawn on this span
		     stops at the text.
		     It is positioned, not laid out, on purpose. The title and the artist line share one
		     shrink-to-fit column, so a second in-flow copy would double this line's max-content
		     width, widen the column, and un-truncate the very text that asked to scroll: both
		     lines then flip between scrolling and still, resizing each other as they go. Out of
		     flow, scrolling and truncating measure exactly the same, which is also what lets every
		     pass swap between the two without the column noticing. -->
		<div
			class="marquee relative w-max"
			style="--marquee-dx:{width + GAP}px;animation-duration:{((width + GAP) / SPEED).toFixed(1)}s"
			style:animation-play-state={held ? 'paused' : null}
			onanimationend={ended}
		>
			<span class="block whitespace-nowrap" data-mq>{@render body()}</span>
			<span
				class="absolute left-full top-0 whitespace-nowrap"
				style="margin-left:{GAP}px"
				aria-hidden="true"
				inert>{@render body()}</span
			>
		</div>
	{:else}
		<span class="block truncate" data-mq>{@render body()}</span>
	{/if}
</div>
