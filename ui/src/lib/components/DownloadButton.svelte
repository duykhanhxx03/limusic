<script lang="ts">
	// The download control for a whole album or playlist, as an icon beside Play — where Spotify
	// puts it, and where it can show its own state without being opened first.
	//
	// Four states, because "downloaded" is not a boolean for a list: none, some (a download that was
	// interrupted, or tracks added since), running, and all. The middle one has to be visible or the
	// button lies about what is on disk.
	//
	// While it runs the button is a *cancel*: that is what the square inside the ring means, and
	// leaving no way to stop a fifty-track download is not an option.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { Download04Icon, Tick02Icon } from '@hugeicons/core-free-icons';
	import type { SongItem } from '$lib/api';
	import { cancel, dl, isDownloading, noteSaved, trackFraction } from '$lib/downloads.svelte';
	import { t } from '$lib/i18n.svelte';

	let {
		/** What this page holds right now. May be a partial list on a paginated playlist — `ondownload`
		 *  is what loads the rest, so this is only used for the *state*, never as the download list. */
		items,
		/** Loads every page if it has to, then downloads. Owned by the page. */
		ondownload,
		disabled = false,
		/** The two pages style their header buttons differently (shadcn ghost on one, a filled circle
		 *  on the other), so the shape comes from the caller and only the state lives here. */
		class: className = ''
	}: {
		items: SongItem[];
		ondownload: () => void;
		disabled?: boolean;
		class?: string;
	} = $props();

	const ids = $derived(items.map((i) => i.video_id).filter(Boolean));

	// Ask Rust which of these are already on disk. Runs when the list changes, which covers arriving
	// on the page and a continuation page landing.
	$effect(() => {
		if (ids.length) noteSaved(ids);
	});

	const savedCount = $derived(ids.filter((id) => dl.saved.has(id)).length);
	const busy = $derived(ids.some((id) => isDownloading(id)));
	const all = $derived(ids.length > 0 && savedCount === ids.length);

	/** The track being fetched right now, if it is one of ours. Downloads run one at a time. */
	const current = $derived(ids.find((id) => isDownloading(id)));

	/**
	 * How far through the whole list we are, 0..1.
	 *
	 * Counted in *tracks*, with the running one contributing its own fraction — not in bytes, which
	 * nobody knows up front: only the track being fetched has a length, and a fifty-track playlist
	 * would otherwise sit at 2% until the first one finished.
	 *
	 * A track whose size is not known yet contributes 0 rather than flipping the whole ring into a
	 * spinner. That flip was the flicker: every track begins with one length-less event, so at a
	 * second a track the ring changed mode several times a second.
	 */
	const raw = $derived(
		ids.length ? Math.min(1, (savedCount + (current ? trackFraction(current) : 0)) / ids.length) : 0
	);

	// Monotonic while a run is going. The parts it is built from update at different moments —
	// `saved` on the done event, the fraction on a progress event — so a frame can land between
	// them where the running track has reset to 0 but the count has not caught up, and the ring
	// would step backwards. Reset when a new run starts.
	let floor = $state(0);
	$effect(() => {
		if (!busy) floor = 0;
		else if (raw > floor) floor = raw;
	});
	const progress = $derived(busy ? Math.max(raw, floor) : raw);

	// A 20px ring inside the button. `r` and the circumference have to agree or the arc is the
	// wrong length, so the circumference is computed rather than written out.
	const R = 9;
	const C = 2 * Math.PI * R;

	const label = $derived(
		busy
			? t('downloads.cancel')
			: all
				? t('downloads.saved_all')
				: savedCount > 0
					? t('downloads.save_rest', { count: ids.length - savedCount })
					: t('downloads.save_all')
	);
</script>

<button
	type="button"
	aria-label={label}
	title={label}
	disabled={disabled || all}
	onclick={() => (busy ? cancel() : ondownload())}
	class="relative disabled:cursor-default {all ? 'text-primary' : ''} {className}"
>
	{#if busy}
		<!-- The ring: a track behind and an arc in front, rotated so it starts at twelve o'clock
		     rather than three. `stroke-dashoffset` is the only thing that animates, and it
		     composites.
		     320ms is not a token: it is a constraint, not a taste. Progress events arrive every
		     250ms (the throttle in downloads.rs), and the tween has to outlast the gap or the arc
		     reaches each value and then waits, which is the stepping this replaced.
		     `--ease-linear`, not the usual `--ease-smooth-out`: an eased progress arc decelerates
		     into every update, which reads as stalling. Progress advances at a steady rate. -->
		<svg class="absolute inset-0 m-auto h-5 w-5 -rotate-90" viewBox="0 0 24 24" aria-hidden="true">
			<circle cx="12" cy="12" r={R} fill="none" stroke="currentColor" stroke-width="2" opacity="0.25" />
			<circle
				cx="12"
				cy="12"
				r={R}
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				class="text-primary"
				stroke-dasharray={C}
				stroke-dashoffset={C * (1 - progress)}
				style="transition: stroke-dashoffset 320ms var(--ease-linear)"
			/>
		</svg>
		<!-- A square inside the ring, not the download arrow: this is the stop control, and an arrow
		     under a progress ring reads as "start" — the one thing it must not mean here. -->
		<span class="h-2 w-2 rounded-[1px] bg-primary"></span>
	{:else}
		<!-- altIcon/showAlt, not a ternary: `icon` is read once at mount. -->
		<HugeiconsIcon icon={Download04Icon} altIcon={Tick02Icon} showAlt={all} class="h-5 w-5" />
		<!-- Part-way there: a dot rather than a count, which at a glance says "not finished" without
		     needing to be read. Not while running — the ring already says that. -->
		{#if !all && savedCount > 0}
			<span class="absolute right-1.5 top-1.5 h-1.5 w-1.5 rounded-full bg-primary"></span>
		{/if}
	{/if}
</button>
