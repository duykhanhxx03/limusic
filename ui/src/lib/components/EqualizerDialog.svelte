<script lang="ts">
	// A ten-band graphic equalizer, from the player bar's ⋮ menu beside Tempo & Pitch.
	//
	// Unlike that one this *is* persisted: an EQ is a property of your speakers and your ears, not
	// of the track playing, so it survives a restart and is applied before the first track starts
	// (see the restore in lib.rs).
	//
	// Every change applies immediately — an equalizer with an Apply button is one nobody can tune
	// by ear, which is the only way anybody tunes one.
	//
	// Three ways to set the curve, all writing the same ten gains and preamp: a preset, a headphone
	// correction from AutoEq (autoeq.rs), or drawing it (EqCurve). The dialog names whichever one the
	// curve still is, and "Custom" once it has been moved off it.
	import { tick } from 'svelte';
	import { slide } from 'svelte/transition';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		ArrowDown01Icon,
		CloudDownloadIcon,
		HeadphonesIcon,
		Loading03Icon,
		Search01Icon,
		Tick02Icon
	} from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import * as Dialog from '$lib/components/ui/dialog';
	import * as Select from '$lib/components/ui/select';
	import { Switch } from '$lib/components/ui/switch';
	import EqCurve from './EqCurve.svelte';
	import * as api from '$lib/api';
	import {
		GAIN_RANGE,
		PREAMP_MAX,
		PREAMP_MIN,
		PRESETS,
		formatDb,
		headroom,
		matchPreset,
		sameCurve
	} from '$lib/eq';
	import { toast } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	let { open = $bindable(false) }: { open: boolean } = $props();

	let bands = $state<number[]>([]);
	let gains = $state<number[]>([]);
	let preamp = $state(0);
	let enabled = $state(false);
	/** The last correction chosen. Kept after the curve moves off it, so moving back names it again. */
	let profile = $state<api.EqProfile | null>(null);
	let loaded = $state(false);

	const activePreset = $derived(matchPreset(preamp, gains));
	const activeProfile = $derived(
		profile && Math.abs(profile.preamp - preamp) < 0.05 && sameCurve(profile.gains, gains)
			? profile
			: null
	);

	// Load once, the first time it opens: the bands never change, and the stored state is only
	// interesting when someone is looking at it.
	$effect(() => {
		if (!open || loaded) return;
		loaded = true;
		Promise.all([api.equalizerBands(), api.equalizer()])
			.then(([b, eq]) => {
				bands = b;
				// Trust the band count over the stored gains: a settings row written by an older
				// build could be shorter, and a curve that disagrees with the labels is worse than a
				// flat one.
				gains = b.map((_, i) => eq.gains[i] ?? 0);
				preamp = Math.min(PREAMP_MAX, Math.max(PREAMP_MIN, eq.preamp ?? 0));
				enabled = eq.enabled ?? false;
				profile = eq.profile ?? null;
			})
			.catch(() => {});
	});

	let pending: ReturnType<typeof setTimeout> | undefined;
	/** Dragging fires per pixel; the filters only need the value you stopped on. */
	function apply() {
		clearTimeout(pending);
		pending = setTimeout(() => {
			const p = activeProfile ? $state.snapshot(activeProfile) : null;
			api
				.setEqualizer(enabled, preamp, $state.snapshot(gains), p)
				.catch((e) => toast.error(String(e)));
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
		preamp = headroom(p.gains);
		touch();
	}

	function reset() {
		gains = bands.map(() => 0);
		preamp = 0;
		apply();
	}

	// --- AutoEq ---------------------------------------------------------------------------------

	let pickerOpen = $state(false);
	let query = $state('');
	let results = $state<api.AutoEqEntry[]>([]);
	/** idle → loading → ready | failed. The index is fetched (or revalidated) when the picker opens. */
	let index = $state<'idle' | 'loading' | 'ready' | 'failed'>('idle');
	let indexSize = $state(0);
	let applying = $state<string | null>(null);
	let searchEl = $state<HTMLInputElement | null>(null);

	let searchTimer: ReturnType<typeof setTimeout> | undefined;
	let searchSeq = 0;
	function search(delay = 150) {
		clearTimeout(searchTimer);
		searchTimer = setTimeout(async () => {
			const seq = ++searchSeq;
			const q = query.trim();
			const hits = q ? await api.autoEqSearch(q).catch(() => []) : [];
			// A slower answer to an older query must not replace a newer one.
			if (seq === searchSeq) results = hits;
		}, delay);
	}

	async function togglePicker() {
		pickerOpen = !pickerOpen;
		if (!pickerOpen) return;
		await tick();
		searchEl?.focus();
		if (index === 'loading' || index === 'ready') return;
		index = 'loading';
		api
			.autoEqRefresh()
			.then((n) => {
				indexSize = n;
				index = 'ready';
				search(0);
			})
			.catch(() => (index = 'failed'));
	}

	async function choose(entry: api.AutoEqEntry) {
		if (applying) return;
		applying = entry.path;
		try {
			const curve = await api.autoEqCurve(entry.path);
			const g = bands.map((_, i) =>
				Math.min(GAIN_RANGE, Math.max(-GAIN_RANGE, curve.gains[i] ?? 0))
			);
			const p = Math.min(PREAMP_MAX, Math.max(PREAMP_MIN, curve.preamp));
			gains = g;
			preamp = p;
			profile = {
				path: entry.path,
				name: entry.name,
				source: entry.source,
				rig: entry.rig,
				preamp: p,
				gains: g
			};
			entry.cached = true;
			touch();
			pickerOpen = false;
		} catch {
			toast.error(t('eq.autoeq.failed'));
		} finally {
			applying = null;
		}
	}

	const emptyMessage = $derived.by(() => {
		if (index === 'failed' && !results.length) return t('eq.autoeq.unavailable');
		if (index === 'loading' && !results.length) return t('eq.autoeq.loading');
		if (query.trim()) return t('eq.autoeq.no_match', { query: query.trim() });
		return indexSize
			? t('eq.autoeq.hint_count', { count: indexSize.toLocaleString() })
			: t('eq.autoeq.hint');
	});

	const subtitle = (e: { source: string; rig: string | null }) =>
		e.rig ? `${e.source} · ${e.rig}` : e.source;

	const AUTOEQ_URL = 'https://github.com/jaakkopasanen/AutoEq';
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="gap-5 sm:max-w-2xl">
		<Dialog.Header class="space-y-1">
			<!-- pr-10 clears the dialog's close button, which is `absolute top-4 right-4` and so
			     overlaps anything the header puts in that corner. -->
			<div class="flex items-center justify-between gap-4 pr-10">
				<div>
					<Dialog.Title>{t('eq.title')}</Dialog.Title>
					<Dialog.Description class="mt-1">
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

		<div class="min-w-0">
			<div class="grid gap-2 sm:grid-cols-2">
				<Select.Root
					type="single"
					value={activePreset ?? ''}
					onValueChange={(v) => v && usePreset(v)}
				>
					<Select.Trigger class="w-full" aria-label={t('eq.preset')}>
						<span class="flex-1 truncate text-left">
							{activePreset
								? t(`eq.presets.${activePreset}` as 'eq.presets.flat')
								: t('eq.custom')}
						</span>
					</Select.Trigger>
					<Select.Content class="max-h-72">
						{#each PRESETS as p (p.id)}
							{@const label = t(`eq.presets.${p.id}` as 'eq.presets.flat')}
							<Select.Item value={p.id} {label}>{label}</Select.Item>
						{/each}
					</Select.Content>
				</Select.Root>

				<button
					class="flex h-9 min-w-0 cursor-pointer items-center gap-2 rounded-4xl bg-input px-3 text-sm transition-colors outline-none hover:bg-input/70 focus-visible:ring-[3px] focus-visible:ring-ring"
					aria-expanded={pickerOpen}
					onclick={togglePicker}
				>
					<HugeiconsIcon icon={HeadphonesIcon} class="size-4 shrink-0 text-muted-foreground" />
					<span class="min-w-0 flex-1 truncate text-left {activeProfile ? '' : 'text-muted-foreground'}">
						{activeProfile ? activeProfile.name : t('eq.autoeq.pick')}
					</span>
					<HugeiconsIcon
						icon={ArrowDown01Icon}
						class="size-4 shrink-0 text-muted-foreground transition-transform duration-[var(--duration-fast)] {pickerOpen
							? 'rotate-180'
							: ''}"
					/>
				</button>
			</div>

			{#if pickerOpen}
				<div
					class="mt-2 rounded-2xl bg-foreground/5 p-2"
					transition:slide={{ duration: 250 }}
				>
					<div class="relative">
						<HugeiconsIcon
							icon={Search01Icon}
							class="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
						/>
						<input
							bind:this={searchEl}
							bind:value={query}
							oninput={() => search()}
							type="search"
							placeholder={t('eq.autoeq.search')}
							aria-label={t('eq.autoeq.search')}
							class="h-9 w-full rounded-xl bg-background/70 pr-9 pl-9 text-sm outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
						/>
						{#if index === 'loading'}
							<HugeiconsIcon
								icon={Loading03Icon}
								class="absolute top-1/2 right-3 size-4 -translate-y-1/2 animate-spin text-muted-foreground"
							/>
						{/if}
					</div>

					<div class="mt-1 max-h-56 overflow-y-auto">
						{#each results as r (r.path)}
							{@const current = activeProfile?.path === r.path}
							<button
								class="flex w-full cursor-pointer items-center gap-3 rounded-xl px-3 py-2 text-left transition-colors hover:bg-foreground/8 disabled:cursor-wait {current
									? 'bg-primary/10'
									: ''}"
								disabled={applying !== null}
								onclick={() => choose(r)}
							>
								<span class="min-w-0 flex-1">
									<span class="block truncate text-sm">{r.name}</span>
									<span class="block truncate text-xs text-muted-foreground">{subtitle(r)}</span>
								</span>
								{#if applying === r.path}
									<HugeiconsIcon
										icon={Loading03Icon}
										class="size-4 shrink-0 animate-spin text-muted-foreground"
									/>
								{:else if current}
									<HugeiconsIcon icon={Tick02Icon} class="size-4 shrink-0 text-primary" />
								{:else if !r.cached}
									<span title={t('eq.autoeq.not_cached')} class="shrink-0 text-muted-foreground/60">
										<HugeiconsIcon icon={CloudDownloadIcon} class="size-4" />
									</span>
								{/if}
							</button>
						{:else}
							<p class="px-3 py-5 text-center text-xs text-muted-foreground">{emptyMessage}</p>
						{/each}
					</div>

					<p class="mt-1 px-3 pb-1 text-xs text-muted-foreground">
						{t('eq.autoeq.credit_before')}<button
							class="cursor-pointer underline-offset-2 hover:text-foreground hover:underline"
							onclick={() => api.openExternal(AUTOEQ_URL).catch(() => {})}>AutoEq</button
						>{t('eq.autoeq.credit_after')}
					</p>
				</div>
			{/if}

			<!-- Dimmed when off, but never locked. Locking it made the switch a step you had to find
			     before anything else responded: picking "Bass" on a fresh install did nothing at all.
			     Touching any control turns it on instead, which is what reaching for a slider means.
			     The pickers above stay at full strength: a list you read to choose from shouldn't
			     look disabled, and choosing from it turns the equalizer on anyway. -->
			<div class="mt-5 transition-opacity duration-[var(--duration-fast)] {enabled ? '' : 'opacity-50'}">
				{#if bands.length}
					<EqCurve {bands} bind:gains onchange={touch} />
				{:else}
					<div class="h-[216px]"></div>
				{/if}

				{#if activeProfile}
					<p class="mt-3 truncate text-xs text-muted-foreground" transition:slide={{ duration: 150 }}>
						{t('eq.autoeq.applied', { name: activeProfile.name, source: subtitle(activeProfile) })}
					</p>
				{/if}

				<div class="mt-4 flex items-center gap-3">
					<span class="w-16 shrink-0 text-xs text-muted-foreground">{t('eq.preamp')}</span>
					<input
						type="range"
						min={PREAMP_MIN}
						max={PREAMP_MAX}
						step="0.1"
						bind:value={preamp}
						oninput={touch}
						aria-label={t('eq.preamp')}
						class="h-1.5 flex-1 cursor-pointer accent-primary"
					/>
					<span class="w-16 text-right text-xs tabular-nums">{formatDb(preamp)} dB</span>
				</div>
			</div>
		</div>

		<Dialog.Footer>
			<!-- Reset does not close, like Tempo & Pitch: you reset to hear the difference. -->
			<Button variant="ghost" size="lg" onclick={reset}>{t('eq.reset')}</Button>
			<Button size="lg" onclick={() => (open = false)}>{t('common.done')}</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
