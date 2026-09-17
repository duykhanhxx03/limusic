<script lang="ts">
	// The big artwork, changing tracks without a blank. The old picture stays until the new one is
	// decoded, then the new one crossfades in over it. Before this the `<img>` simply took the new
	// URL: a 720–1280 px image nothing else had loaded, so the cover went empty for as long as the
	// fetch took (up to 880 ms measured) and then snapped in.
	//
	// Normally the wait is nothing: the track before this one decoded the next cover already
	// (prefetch.svelte.ts). The first rung of the ladder that loads wins, same order as before.
	import type { Snippet } from 'svelte';
	import type { TransitionConfig } from 'svelte/transition';
	import { resolveArtwork } from '$lib/prefetch.svelte';

	let {
		srcs,
		class: className = '',
		fallback
	}: {
		/** Largest first (`artworkLadder`). The last entry is also painted underneath while nothing
		 *  is decoded yet, since it is the small one the player bar has already loaded. */
		srcs: (string | undefined)[];
		class?: string;
		/** Shown when no rung loads at all. */
		fallback?: Snippet;
	} = $props();

	let shown = $state<string | null>(null);
	let failed = $state(false);

	$effect(() => {
		const ladder = srcs;
		let live = true;
		resolveArtwork(ladder).then((url) => {
			if (!live) return;
			failed = !url;
			if (url) shown = url;
		});
		return () => {
			live = false;
		};
	});

	/** In: the transition tokens' icon swap — 250 ms, ease-in-out, from a 2px blur. */
	function swapIn(_node: Element): TransitionConfig {
		return {
			duration: 250,
			easing: (t) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2),
			css: (t) => `opacity: ${t}; filter: blur(${((1 - t) * 2).toFixed(2)}px)`
		};
	}
	/** Out: stay put under the incoming picture for as long as it takes to cover it. Fading both at
	 *  once dips through to whatever is behind halfway through. */
	function holdOut(_node: Element): TransitionConfig {
		return { duration: 250, css: () => '' };
	}
</script>

<div
	class="relative aspect-square w-full overflow-hidden bg-cover {className}"
	style={srcs[srcs.length - 1] ? `background-image:url(${srcs[srcs.length - 1]})` : undefined}
>
	{#if failed && fallback}
		{@render fallback()}
	{:else if shown}
		{#key shown}
			<img
				src={shown}
				alt=""
				decoding="async"
				class="absolute inset-0 h-full w-full object-cover"
				in:swapIn
				out:holdOut
			/>
		{/key}
	{/if}
</div>
