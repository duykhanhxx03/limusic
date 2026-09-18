<script lang="ts">
	// A row of chips that may be wider than its room: home's moods, the library's kinds. It scrolls
	// sideways with no scrollbar, the edge it continues past fades out, and on hover an arrow pages
	// it. Cut off square with nothing else, the last chip read as a broken one rather than as the
	// start of more; a scrollbar under a row of pills read as a bug.
	import type { Snippet } from 'svelte';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { ArrowLeft01Icon, ArrowRight01Icon } from '@hugeicons/core-free-icons';
	import { t } from '$lib/i18n.svelte';

	/** `class` goes on the scrolling row, for its gap and padding. The arrows sit at its top, level
	 *  with 2rem chips, so padding belongs below the chips, not above them. */
	let { children, class: cls = '' }: { children: Snippet; class?: string } = $props();

	let row = $state<HTMLDivElement | null>(null);
	let canLeft = $state(false);
	let canRight = $state(false);

	function measure() {
		if (!row) return;
		const { scrollLeft, clientWidth, scrollWidth } = row;
		canLeft = scrollLeft > 4;
		canRight = scrollLeft + clientWidth < scrollWidth - 4;
	}

	const watch = (el: HTMLElement) => {
		const ro = new ResizeObserver(measure);
		ro.observe(el);
		// The chips themselves: chips arriving, or a filter that hides the others, change how wide the
		// content is without changing the row's box, which is all the observer above sees.
		const mo = new MutationObserver(measure);
		mo.observe(el, { childList: true, subtree: true, characterData: true });
		el.addEventListener('scroll', measure, { passive: true });
		return () => {
			ro.disconnect();
			mo.disconnect();
			el.removeEventListener('scroll', measure);
		};
	};

	// A mask, not a gradient laid over the row: home's row sits on the hero's wash above the fold,
	// and a page-coloured fade would show as a block on it.
	const mask = $derived(
		`linear-gradient(to right, ${canLeft ? 'transparent, #000 3rem' : '#000, #000'}, ${canRight ? '#000 calc(100% - 3rem), transparent' : '#000, #000'})`
	);

	function page(dir: 1 | -1) {
		row?.scrollBy({ left: dir * Math.round(row.clientWidth * 0.7), behavior: 'smooth' });
	}

	const arrow =
		'absolute top-0 flex size-8 cursor-pointer items-center justify-center rounded-full plate text-foreground opacity-0 shadow-md transition hover:scale-105 focus-visible:opacity-100 group-hover/chips:opacity-100';
</script>

<div class="group/chips relative min-w-0">
	<div
		bind:this={row}
		{@attach watch}
		class="no-scrollbar flex overflow-x-auto {cls}"
		style="mask-image: {mask}; -webkit-mask-image: {mask}"
	>
		{@render children()}
	</div>
	{#if canLeft}
		<button aria-label={t('a11y.scroll_left')} onclick={() => page(-1)} class="{arrow} left-0">
			<HugeiconsIcon icon={ArrowLeft01Icon} class="h-4 w-4" />
		</button>
	{/if}
	{#if canRight}
		<button aria-label={t('a11y.scroll_right')} onclick={() => page(1)} class="{arrow} right-0">
			<HugeiconsIcon icon={ArrowRight01Icon} class="h-4 w-4" />
		</button>
	{/if}
</div>
