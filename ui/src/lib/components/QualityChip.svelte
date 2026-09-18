<script lang="ts">
	// What the playing track is actually streaming at. Small, quiet, and only there when the
	// backend knows — a local file and a restored-but-never-played queue row both have nothing to
	// say, and an empty chip reads as a bug.
	import { quality } from '$lib/quality';
	import { t } from '$lib/i18n.svelte';

	let {
		codec,
		bitrate,
		/** The player bar is tight; the player view has room for the unit. */
		compact = false
	}: { codec?: string | null; bitrate?: number | null; compact?: boolean } = $props();

	const q = $derived(quality(codec, bitrate));
</script>

{#if q}
	<!-- 12px on the scale still fits the player bar: leading-none plus py-0.5 is 16px tall, the same
	     line box as the text-xs artist name beside it, so the row does not grow. -->
	<span
		class="shrink-0 rounded-md px-1.5 py-0.5 text-xs font-bold uppercase leading-none tracking-wide tabular-nums {q.tier ===
		'high'
			? 'bg-primary/15 text-primary'
			: q.tier === 'low'
				? 'bg-foreground/10 text-muted-foreground'
				: 'bg-foreground/10 text-foreground/80'}"
		title={q.kbps === null
			? q.codec
			: t('quality.tooltip', { codec: q.codec, kbps: String(q.kbps) })}
	>
		{q.label}{#if !compact && q.kbps !== null}<span class="ml-0.5 font-normal opacity-70">
				{t('quality.kbps')}</span
			>{/if}
	</span>
{/if}
