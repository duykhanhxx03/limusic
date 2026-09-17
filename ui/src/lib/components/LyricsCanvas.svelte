<script lang="ts">
	// The WebGL half of `LyricsView`: same lines, same clock, same decisions about which line is
	// sung, drawn by `LyricStage` instead of the DOM. See the header of `lyricstage.ts` for why.
	// Mounted only where WebGL is up; if the context is lost it asks the owner to fall back.
	import { onMount, untrack } from 'svelte';
	import type * as api from '$lib/api';
	import type { MediaClock } from '$lib/mediaclock';
	import { playback } from '$lib/player.svelte';
	import { LyricStage, type Interlude } from '$lib/lyricstage';

	let {
		lines,
		active,
		interlude,
		clock,
		expanded,
		onseek,
		onfail
	}: {
		lines: api.LyricLine[];
		active: number;
		interlude: Interlude | null;
		clock: MediaClock;
		expanded: boolean;
		onseek: (line: api.LyricLine) => void;
		onfail: () => void;
	} = $props();

	let host: HTMLElement;
	let stage = $state.raw<LyricStage | null>(null);

	onMount(() => {
		let gone = false;
		LyricStage.create({
			host,
			clock,
			expanded,
			onSeek: (i) => {
				const line = untrack(() => lines[i]);
				if (line) onseek(line);
			},
			onLost: () => onfail()
		})
			.then((created) => {
				if (gone) created.destroy();
				else stage = created;
			})
			.catch((e) => {
				console.warn('lyrics: WebGL stage unavailable, using the DOM view', e);
				if (!gone) onfail();
			});
		return () => {
			gone = true;
			stage?.destroy();
			stage = null;
		};
	});

	// In this order on the first run: the lines, then which of them is sung.
	$effect(() => {
		const s = stage;
		const l = lines;
		if (s) untrack(() => s.setLines(l));
	});
	$effect(() => {
		const s = stage;
		const a = active;
		const g = interlude;
		if (s) untrack(() => s.setActive(a, g));
	});
	$effect(() => {
		const s = stage;
		const e = expanded;
		if (s) untrack(() => s.setExpanded(e));
	});
	// Any clock input — a report, a seek, pause, resume — may start motion the loop had stopped for.
	$effect(() => {
		const s = stage;
		void playback.position;
		void playback.positionAt;
		void playback.paused;
		void playback.speed;
		if (s) untrack(() => s.wake());
	});
</script>

<div bind:this={host} class="absolute inset-0 overflow-hidden text-foreground"></div>
<!-- The canvas is opaque to assistive tech and the keyboard; the lines stay reachable here. -->
<ol class="sr-only">
	{#each lines as line, i (i)}
		<li>
			<button onclick={() => onseek(line)} aria-current={i === active ? 'true' : undefined}>
				{line.text}
			</button>
		</li>
	{/each}
</ol>
