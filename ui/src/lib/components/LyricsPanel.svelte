<script lang="ts">
	// The lyrics page: the player bar's lyrics button puts the words over the whole main panel, in a
	// colour taken from the cover, the way Spotify does it. The library and the right column stay
	// where they are, so the queue can sit beside the words.
	//
	// The colour lives on this element alone. It changes with every track, and a root custom
	// property that does is exactly the whole-document restyle #217 was (see layout.css).
	import { fade } from 'svelte/transition';
	import { beforeNavigate } from '$app/navigation';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { Cancel01Icon } from '@hugeicons/core-free-icons';
	import LyricsView from './LyricsView.svelte';
	import { playback } from '$lib/player.svelte';
	import { artworkAccent } from '$lib/artcolor';
	import { hexToHsv, hsvToHex } from '$lib/color';
	import { thumb } from '$lib/thumb';
	import { t } from '$lib/i18n.svelte';

	let { onClose }: { onClose: () => void } = $props();

	// Going to a page means wanting to see it, not the words over it. beforeNavigate rather than a
	// pathname effect, so clicking the page you are already on closes this too.
	beforeNavigate(() => onClose());

	/** The cover's colour, brought to a depth white type reads on: its hue, a clear but not loud
	 *  saturation, and dark. A grey cover gives a grey page, which is what it should give. */
	let wash = $state<string | null>(null);
	$effect(() => {
		const url = thumb(playback.now?.thumbnail, 120);
		if (!url) {
			wash = null;
			return;
		}
		let live = true;
		artworkAccent(url).then((hex) => {
			const hsv = hex ? hexToHsv(hex) : null;
			if (!live || !hsv) return;
			wash = hsvToHex({ h: hsv.h, s: Math.min(0.7, Math.max(hsv.s, 0.4)), v: 0.38 });
		});
		return () => {
			live = false;
		};
	});
</script>

<section
	in:fade={{ duration: 200 }}
	out:fade={{ duration: 150 }}
	class="absolute inset-0 z-30 flex flex-col overflow-hidden rounded-lg bg-card text-white transition-[background-color] duration-700"
	style={wash ? `background-color: ${wash}` : undefined}
	aria-label={t('lyrics.title')}
>
	<button
		onclick={onClose}
		class="absolute right-4 top-4 z-10 flex size-9 cursor-pointer items-center justify-center rounded-full bg-black/20 text-white/80 transition-colors hover:bg-black/35 hover:text-white"
		aria-label={t('a11y.close_lyrics')}
		title={t('a11y.close_lyrics')}
	>
		<HugeiconsIcon icon={Cancel01Icon} class="h-4 w-4" />
	</button>
	<LyricsView page />
</section>
