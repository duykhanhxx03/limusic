<script lang="ts">
	// "Stop the music in…". The deadline is Rust's (sleep.rs); this only arms it and draws what is
	// left, which is why closing the window or opening the mini player does not lose the timer.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { AlarmClockIcon, Tick02Icon } from '@hugeicons/core-free-icons';
	import { Input } from '$lib/components/ui/input';
	import { sleep, setSleep, setSleepEndOfTrack, clearSleep } from '$lib/sleep.svelte';
	import { playback } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	let open = $state(false);
	let customValue = $state('');

	const PRESETS = [15, 30, 60, 180];

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

	/** What is left before the music stops: the deadline's countdown, or with "end of this song" the
	 *  song's own remaining time, which the player state already keeps current. */
	const left = $derived(
		sleep.endOfTrack
			? Math.max(0, (playback.duration - playback.position) * 1000)
			: sleep.remaining
	);
	const armed = $derived(sleep.remaining !== null || sleep.endOfTrack);
	const label = $derived(left !== null && armed ? fmt(left) : null);
</script>

<!-- Closes on any click outside. Top level: `<svelte:window>` cannot sit inside an element. -->
<svelte:window
	onclick={(e) => {
		if (open && !(e.target as Element)?.closest?.('[data-sleep]')) open = false;
	}}
/>

<div class="relative flex h-full items-center" data-sleep>
	<button
		class="flex h-full items-center gap-1 px-2 text-muted-foreground transition-colors hover:bg-accent/10 hover:text-foreground {armed
			? 'text-primary'
			: ''}"
		onclick={() => (open = !open)}
		aria-expanded={open}
		title={t('sleep.title')}
		aria-label={t('sleep.title')}
	>
		<HugeiconsIcon icon={AlarmClockIcon} class="h-4 w-4" />
		{#if label}
			<!-- tabular-nums so the countdown does not shuffle the buttons beside it every second. -->
			<span class="text-[11px] font-medium tabular-nums">{label}</span>
		{/if}
	</button>

	{#if open}
		<div class="glass absolute right-0 top-full z-50 mt-1 w-56 overflow-hidden rounded-xl py-1">
			<p class="px-3 py-1.5 text-[11px] font-semibold uppercase text-muted-foreground">
				{t('sleep.title')}
			</p>
			{#each PRESETS as m (m)}
				<button
					class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm transition-colors hover:bg-accent/10"
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
				class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm transition-colors hover:bg-accent/10"
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

			<form class="flex items-center gap-2 px-3 py-2" onsubmit={armCustom}>
				<Input
					bind:value={customValue}
					type="number"
					min="1"
					max="1440"
					class="h-7 text-sm"
					placeholder={t('sleep.custom_placeholder')}
					aria-label={t('sleep.custom')}
				/>
				<button
					type="submit"
					class="shrink-0 rounded-md bg-primary px-2 py-1 text-xs font-medium text-primary-foreground disabled:opacity-40"
					disabled={!customValue}
				>
					{t('sleep.set')}
				</button>
			</form>

			{#if armed}
				<div class="h-2"></div>
				<button
					class="w-full px-3 py-1.5 text-left text-sm text-destructive transition-colors hover:bg-accent/10"
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
</div>
