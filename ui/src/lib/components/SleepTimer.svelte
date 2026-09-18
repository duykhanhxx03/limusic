<script lang="ts" module>
	import { sleep } from '$lib/sleep.svelte';
	import { playback } from '$lib/player.svelte';

	/** "1:04:09" past an hour, "14:59" under it — the shape a countdown is read in. */
	function fmt(ms: number) {
		const s = Math.max(0, Math.round(ms / 1000));
		const h = Math.floor(s / 3600);
		const m = Math.floor((s % 3600) / 60);
		const sec = s % 60;
		return h > 0
			? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`
			: `${m}:${String(sec).padStart(2, '0')}`;
	}

	/** What is left before the music stops, formatted, or `null` with no timer armed: the deadline's
	 *  countdown, or with "end of this song" the song's own remaining time, which the player state
	 *  already keeps current. Shared by the player bar's chip and the ⋮ menu's entry. */
	export function sleepLeft(): string | null {
		if (sleep.endOfTrack) return fmt(Math.max(0, (playback.duration - playback.position) * 1000));
		return sleep.remaining !== null ? fmt(sleep.remaining) : null;
	}
</script>

<script lang="ts">
	// "Stop the music in…". The deadline is Rust's (sleep.rs); this only arms it and draws what is
	// left, which is why closing the window or opening the mini player does not lose the timer.
	//
	// A menu that opens off the player: the ⋮ beside the track (Spotify keeps it there too) or the
	// countdown chip the bar shows while a timer runs. It used to be a button of its own in the
	// titlebar, where it read as a setting of the app rather than of what is playing.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { Tick02Icon } from '@hugeicons/core-free-icons';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import { setSleep, setSleepEndOfTrack, clearSleep } from '$lib/sleep.svelte';
	import { fitMenu, toBody, type Anchor } from '$lib/menu';
	import { t } from '$lib/i18n.svelte';

	let { open = $bindable(false), anchor }: { open?: boolean; anchor: Anchor } = $props();

	let customValue = $state('');
	const PRESETS = [15, 30, 60, 180];
	const armed = $derived(sleep.remaining !== null || sleep.endOfTrack);

	function arm(minutes: number) {
		setSleep(minutes);
		open = false;
		customValue = '';
	}
	function armCustom(e: SubmitEvent) {
		e.preventDefault();
		const n = Number.parseInt(customValue, 10);
		if (Number.isFinite(n) && n > 0) arm(n);
	}
</script>

{#if open}
	<button
		class="fixed inset-0 z-40 cursor-default"
		onclick={() => (open = false)}
		oncontextmenu={(e) => {
			e.preventDefault();
			open = false;
		}}
		aria-label={t('a11y.close_menu')}
		{@attach toBody}
	></button>
	<div
		class="fixed z-50 w-64 animate-in rounded-lg glass p-1 text-popover-foreground duration-[var(--duration-quick)] ease-[var(--ease-smooth-out)] fade-in-0 zoom-in-[0.97]"
		style={anchor.style}
		{@attach toBody}
		{@attach fitMenu(anchor)}
	>
		<p class="px-3 pt-2 pb-1 text-xs font-bold text-muted-foreground">
			{t('sleep.title')}{#if armed}<span class="ml-1 tabular-nums text-primary">· {sleepLeft()}</span>{/if}
		</p>
		{#each PRESETS as m (m)}
			<button
				class="menu-item"
				onclick={() => arm(m)}
			>
				<span class="w-3.5">
					{#if sleep.minutes === m}<HugeiconsIcon icon={Tick02Icon} class="h-3.5 w-3.5" />{/if}
				</span>
				{m < 60
					? t('sleep.minutes', { count: m })
					: m === 60
						? t('sleep.hour')
						: t('sleep.hours', { count: m / 60 })}
			</button>
		{/each}
		<button
			class="menu-item"
			onclick={() => {
				setSleepEndOfTrack();
				open = false;
			}}
		>
			<span class="w-3.5">
				{#if sleep.endOfTrack}<HugeiconsIcon icon={Tick02Icon} class="h-3.5 w-3.5" />{/if}
			</span>
			{t('sleep.end_of_track')}
		</button>

		<div class="menu-sep"></div>
		<!-- h-8 on the field: the same height as the small button beside it. -->
		<form class="flex items-center gap-2 px-3 py-2" onsubmit={armCustom}>
			<Input
				bind:value={customValue}
				type="number"
				min="1"
				max="1440"
				class="h-8 text-sm"
				placeholder={t('sleep.custom_placeholder')}
				aria-label={t('sleep.custom')}
			/>
			<Button type="submit" size="sm" class="shrink-0" disabled={!customValue}>
				{t('sleep.set')}
			</Button>
		</form>

		{#if armed}
			<div class="menu-sep"></div>
			<button
				class="menu-item text-destructive"
				onclick={() => {
					clearSleep();
					open = false;
				}}
			>
				{t('sleep.off')}
			</button>
		{/if}
	</div>
{/if}
