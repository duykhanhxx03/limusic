<script lang="ts">
	// Nudge the lyrics earlier or later against the audio. For output that reaches the ears late
	// (Bluetooth adds 150–300 ms): the words light up before you hear them, and this is the fix.
	// Shared by the lyrics footer and Settings, so both show the same number and move it the same way.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { MinusSignIcon, PlusSignIcon } from '@hugeicons/core-free-icons';
	import { prefs, setLyricsOffset, LYRICS_OFFSET_LIMIT_MS } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	let { size = 'md' }: { size?: 'sm' | 'md' } = $props();

	/** A tenth of a second a click: under that nobody can tell, over it one click overshoots. */
	const STEP = 100;

	const label = $derived.by(() => {
		const s = prefs.lyricsOffsetMs / 1000;
		if (s === 0) return '0.0 s';
		return `${s > 0 ? '+' : '−'}${Math.abs(s).toFixed(1)} s`;
	});

	const btn = $derived(
		size === 'sm'
			? 'flex size-6 items-center justify-center rounded-full'
			: 'flex size-8 items-center justify-center rounded-full bg-foreground/8'
	);
</script>

<div class="flex items-center {size === 'sm' ? 'gap-0.5' : 'gap-1.5'}">
	<button
		class="{btn} cursor-pointer transition-colors hover:bg-foreground/15 disabled:opacity-40"
		disabled={prefs.lyricsOffsetMs <= -LYRICS_OFFSET_LIMIT_MS}
		onclick={() => setLyricsOffset(prefs.lyricsOffsetMs - STEP)}
		aria-label={t('lyrics.offset_earlier')}
		title={t('lyrics.offset_earlier')}
	>
		<HugeiconsIcon icon={MinusSignIcon} class={size === 'sm' ? 'size-3' : 'size-3.5'} />
	</button>
	<!-- The number resets to zero on click: the one value anyone wants to get back to exactly. -->
	<button
		class="cursor-pointer rounded-full px-1.5 text-center tabular-nums transition-colors hover:text-foreground {size ===
		'sm'
			? 'min-w-11 text-[11px]'
			: 'min-w-14 text-xs'} {prefs.lyricsOffsetMs === 0 ? '' : 'text-primary'}"
		onclick={() => setLyricsOffset(0)}
		title={t('lyrics.offset_reset')}
		aria-label="{t('lyrics.offset')}: {label}. {t('lyrics.offset_reset')}"
	>
		{label}
	</button>
	<button
		class="{btn} cursor-pointer transition-colors hover:bg-foreground/15 disabled:opacity-40"
		disabled={prefs.lyricsOffsetMs >= LYRICS_OFFSET_LIMIT_MS}
		onclick={() => setLyricsOffset(prefs.lyricsOffsetMs + STEP)}
		aria-label={t('lyrics.offset_later')}
		title={t('lyrics.offset_later')}
	>
		<HugeiconsIcon icon={PlusSignIcon} class={size === 'sm' ? 'size-3' : 'size-3.5'} />
	</button>
</div>
