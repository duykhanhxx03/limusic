<script lang="ts">
	// A ten-band graphic equalizer, from the player bar's ⋮ menu beside Tempo & Pitch.
	//
	// Unlike that one this *is* persisted: an EQ is a property of your speakers and your ears, not
	// of the track playing, so it survives a restart and is applied before the first track starts
	// (see the restore in lib.rs).
	//
	// Every change applies immediately — an equalizer with an Apply button is one nobody can tune
	// by ear, which is the only way anybody tunes one.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { ArrowReloadHorizontalIcon } from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import * as Dialog from '$lib/components/ui/dialog';
	import { Switch } from '$lib/components/ui/switch';
	import * as api from '$lib/api';
	import { PRESETS, bandLabel, matchPreset } from '$lib/eq';
	import { toast } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	let { open = $bindable(false) }: { open: boolean } = $props();

	const RANGE = 12; // ±dB, matching the clamp in commands.rs

	let bands = $state<number[]>([]);
	let gains = $state<number[]>([]);
	let preamp = $state(0);
	let enabled = $state(false);
	let loaded = $state(false);

	const activePreset = $derived(matchPreset(preamp, gains));

	// Load once, the first time it opens: the bands never change, and the stored state is only
	// interesting when someone is looking at it.
	$effect(() => {
		if (!open || loaded) return;
		loaded = true;
		Promise.all([api.equalizerBands(), api.equalizer()])
			.then(([b, eq]) => {
				bands = b;
				// Trust the band count over the stored gains: a settings row written by an older
				// build could be shorter, and a slider array that disagrees with the labels is worse
				// than a flat one.
				gains = b.map((_, i) => eq.gains[i] ?? 0);
				preamp = eq.preamp ?? 0;
				enabled = eq.enabled ?? false;
			})
			.catch(() => {});
	});

	let pending: ReturnType<typeof setTimeout> | undefined;
	/** Dragging a slider fires per pixel; the filters only need the value you stopped on. */
	function apply() {
		clearTimeout(pending);
		pending = setTimeout(() => {
			api.setEqualizer(enabled, preamp, gains).catch((e) => toast.error(String(e)));
		}, 120);
	}

	/** Moving anything is asking to hear it, so it switches the equalizer on. */
	function touch() {
		enabled = true;
		apply();
	}

	function usePreset(id: string) {
		const p = PRESETS.find((x) => x.id === id);
		if (!p) return;
		gains = bands.map((_, i) => p.gains[i] ?? 0);
		preamp = p.preamp;
		// Picking a preset is asking to hear it.
		enabled = true;
		apply();
	}

	function reset() {
		gains = bands.map(() => 0);
		preamp = 0;
		apply();
	}
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="gap-5 sm:max-w-2xl">
		<Dialog.Header class="space-y-1">
			<!-- pr-10 clears the dialog's close button, which is `absolute top-4 right-4` and so
			     overlaps anything the header puts in that corner. -->
			<div class="flex items-center justify-between gap-4 pr-10">
				<div>
					<Dialog.Title class="text-lg font-semibold">{t('eq.title')}</Dialog.Title>
					<Dialog.Description class="text-xs text-muted-foreground">
						{t('eq.desc')}
					</Dialog.Description>
				</div>
				<Switch
					checked={enabled}
					onCheckedChange={(v) => {
						enabled = v;
						apply();
					}}
					aria-label={t('eq.title')}
				/>
			</div>
		</Dialog.Header>

		<!-- Dimmed when off, but never locked. Locking it made the switch a step you had to find
		     before anything else responded: picking "Bass" on a fresh install did nothing at all.
		     Touching any control turns it on instead, which is what reaching for a slider means. -->
		<div class="transition-opacity {enabled ? '' : 'opacity-50'}">
			<div class="flex flex-wrap gap-1.5">
				{#each PRESETS as p (p.id)}
					<button
						class="rounded-full px-2.5 py-1 text-xs font-medium transition-colors {activePreset ===
						p.id
							? 'bg-primary text-primary-foreground'
							: 'bg-foreground/8 hover:bg-foreground/15'}"
						onclick={() => usePreset(p.id)}
					>
						{t(`eq.presets.${p.id}` as 'eq.presets.flat')}
					</button>
				{/each}
			</div>

			<!-- Vertical sliders, one per band. `writing-mode: vertical-lr` + `direction: rtl` is how
			     a range input is turned upright without a transform; a rotated element keeps its
			     original hit box, which puts the drag target in the wrong place. -->
			<div class="mt-5 flex items-end justify-between gap-1">
				{#each bands as hz, i (hz)}
					<div class="flex flex-1 flex-col items-center gap-2">
						<span class="text-[10px] tabular-nums text-muted-foreground">
							{gains[i] > 0 ? '+' : ''}{gains[i]}
						</span>
						<input
							type="range"
							min={-RANGE}
							max={RANGE}
							step="1"
							bind:value={gains[i]}
							oninput={touch}
							aria-label="{bandLabel(hz)} Hz"
							class="eq-slider h-52 w-3 cursor-pointer accent-primary"
						/>
						<span class="text-[10px] tabular-nums text-muted-foreground">{bandLabel(hz)}</span>
					</div>
				{/each}
			</div>

			<div class="mt-5 flex items-center gap-3">
				<span class="w-16 shrink-0 text-xs text-muted-foreground">{t('eq.preamp')}</span>
				<input
					type="range"
					min={-RANGE}
					max={RANGE}
					step="1"
					bind:value={preamp}
					oninput={touch}
					aria-label={t('eq.preamp')}
					class="h-1.5 flex-1 cursor-pointer accent-primary"
				/>
				<span class="w-10 text-right text-xs tabular-nums">
					{preamp > 0 ? '+' : ''}{preamp} dB
				</span>
			</div>
		</div>

		<Dialog.Footer>
			<Button variant="ghost" size="sm" class="gap-2" onclick={reset}>
				<HugeiconsIcon icon={ArrowReloadHorizontalIcon} class="h-4 w-4" />
				{t('eq.reset')}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>

<style>
	/* Upright without a transform: a rotated range input keeps the hit box it had lying down, so
	   the pointer grabs empty space beside it.
	   Both properties, not one. `writing-mode: vertical-lr` is the modern spelling and WebKitGTK
	   accepts it, but it leaves the *track* unpainted — measured: the thumbs appeared and nothing
	   else. `-webkit-appearance: slider-vertical` is the old one that actually builds a vertical
	   track there, so it does the work and the modern property stays for engines that dropped it. */
	.eq-slider {
		-webkit-appearance: slider-vertical;
		appearance: slider-vertical;
		writing-mode: vertical-lr;
		direction: rtl;
	}
	/* The track, drawn explicitly: with `slider-vertical` WebKitGTK falls back to the platform
	   theme, which on a dark background is a barely visible groove. */
	.eq-slider::-webkit-slider-runnable-track {
		width: 0.375rem;
		border-radius: 999px;
		background: color-mix(in oklab, var(--color-foreground) 15%, transparent);
	}
</style>
