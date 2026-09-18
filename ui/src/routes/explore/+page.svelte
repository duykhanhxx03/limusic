<script lang="ts">
	// Explore: the part of YouTube Music that isn't about you. Home is built from what this account
	// has played; this page is the moods, the new releases and the charts, which are the same for
	// everyone in a country — and the only surface here that works before you have any history.
	//
	// Two independent fetches, rendered independently: a charts failure (or a country with no
	// charts at all) must not take the shelves above it down with it, and the charts reload on
	// their own whenever the country changes.
	import { onMount } from 'svelte';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { ChartAverageIcon, MusicNote01Icon } from '@hugeicons/core-free-icons';
	import { Skeleton } from '$lib/components/ui/skeleton';
	import SectionHeading from '$lib/components/SectionHeading.svelte';
	import * as Select from '$lib/components/ui/select';
	import Shelf from '$lib/components/Shelf.svelte';
	import MediaCardSkeleton from '$lib/components/MediaCardSkeleton.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import * as api from '$lib/api';
	import type { ChartsPage, ExplorePage, HomeSection } from '$lib/api';
	import { moreHref } from '$lib/browse';
	import { chipClass } from '$lib/chip';
	import { goto } from '$app/navigation';
	import { getCached, putCached } from '$lib/pagecache';
	import { prefs, toast } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	let explore = $state<ExplorePage | null>(null);
	let exploreError = $state<string | null>(null);
	let charts = $state<ChartsPage | null>(null);
	let chartsError = $state<string | null>(null);
	let chartsLoading = $state(true);

	const EXPLORE_KEY = 'explore';
	const chartsKey = (country: string) => `charts:${country}`;

	async function loadExplore() {
		const hit = getCached<ExplorePage>(EXPLORE_KEY);
		if (hit) explore = hit;
		exploreError = null;
		try {
			const fresh = await api.getExplore();
			explore = fresh;
			putCached(EXPLORE_KEY, fresh);
		} catch (e) {
			if (!explore) exploreError = String(e);
		}
	}

	/** `country` empty = let YouTube pick from the IP; the response says which it picked. */
	async function loadCharts(country: string) {
		const hit = getCached<ChartsPage>(chartsKey(country));
		if (hit) charts = hit;
		chartsError = null;
		chartsLoading = !hit;
		try {
			const fresh = await api.getCharts(country || undefined);
			if (country !== prefs.chartsCountry) return; // superseded by another pick
			charts = fresh;
			putCached(chartsKey(country), fresh);
		} catch (e) {
			if (country !== prefs.chartsCountry) return;
			if (!hit) chartsError = String(e);
		} finally {
			if (country === prefs.chartsCountry) chartsLoading = false;
		}
	}

	/** The country menu only exists once a charts response has landed, so the picker keeps showing
	 *  the last list while the next country loads rather than collapsing to nothing. */
	const countries = $derived(charts?.countries ?? []);
	// What the picker displays: the choice if there is one, else the country YouTube chose for us.
	const country = $derived(prefs.chartsCountry || charts?.selected || '');
	const countryName = $derived(countries.find((c) => c.code === country)?.name ?? '');

	/** Picking writes the preference and nothing else: the effect below is watching it, and is the
	 *  one place the charts are fetched from. */
	async function pickCountry(code: string) {
		if (!code || code === country) return;
		prefs.chartsCountry = code;
		try {
			await api.setSetting('charts_country', code);
		} catch (e) {
			toast.error(String(e));
		}
	}

	function openMood(title: string, params: string) {
		goto(`/explore/mood?params=${encodeURIComponent(params)}&title=${encodeURIComponent(title)}`);
	}

	const onMore = (s: HomeSection) => (s.moreBrowseId ? () => goto(moreHref(s)) : undefined);

	onMount(loadExplore);
	// Reruns on a pick, and once more if the saved country arrives after the page did (settings
	// hydrate asynchronously at startup, so a cold start that lands straight here sees '' first).
	$effect(() => {
		loadCharts(prefs.chartsCountry);
	});
</script>

<div class="p-6">
	<h1 class="mb-6 font-heading text-2xl font-bold">{t('nav.explore')}</h1>

	{#if exploreError}
		<ErrorState message={exploreError} onRetry={loadExplore} />
	{:else if !explore}
		<div class="mb-10 flex gap-2 overflow-hidden">
			{#each ['w-24', 'w-20', 'w-28', 'w-16', 'w-24', 'w-32', 'w-20', 'w-24'] as w, i (i)}
				<Skeleton class="h-9 shrink-0 rounded-full {w}" />
			{/each}
		</div>
		{#each Array(2) as _, s (s)}
			<section class="mb-10" aria-hidden="true">
				<Skeleton class="mb-3 h-5 w-40 rounded" />
				<div class="flex gap-2 overflow-hidden pb-2">
					{#each Array(6) as _, i (i)}
						<div class="w-40 shrink-0"><MediaCardSkeleton /></div>
					{/each}
				</div>
			</section>
		{/each}
	{:else}
		<div class="content-in flex flex-col gap-10">
			{#if explore.moods.length}
				<!-- One line, scrolled sideways like every other row on the page: thirty-seven pills
				     wrapped into six rows pushed the actual content below the fold. The full list,
				     in YouTube's own groups, is one click away. -->
				<section>
					<SectionHeading
						title={t('explore.moods')}
						icon={MusicNote01Icon}
						onMore={() => goto('/explore/moods')}
						moreLabel={t('common.see_all')}
					/>
					<div class="flex gap-2 overflow-x-auto pb-2">
						{#each explore.moods as mood (mood.params)}
							<button
								type="button"
								onclick={() => openMood(mood.title, mood.params)}
								class={chipClass()}
							>
								{mood.title}
							</button>
						{/each}
					</div>
				</section>
			{/if}

			{#each explore.sections as section (section.title)}
				<Shelf
					title={section.title}
					items={section.items}
					queueAll={false}
					onMore={onMore(section)}
				/>
			{/each}
		</div>
	{/if}

	<!-- Charts. Its own heading row, because the country picker belongs to this block alone and
	     nothing above it changes when the country does. -->
	<section class="mt-10">
		<div class="mb-3 flex items-center gap-3">
			<h2 class="font-heading text-2xl font-bold tracking-tight">{t('explore.charts')}</h2>
			<span class="flex-1"></span>
			{#if countries.length}
				<Select.Root type="single" value={country} onValueChange={pickCountry}>
					<Select.Trigger class="w-44 shrink-0" aria-label={t('explore.country')}>
						<span class="flex-1 truncate text-left">{countryName || country}</span>
					</Select.Trigger>
					<Select.Content class="max-h-72">
						{#each countries as c (c.code)}
							<Select.Item value={c.code} label={c.name}>{c.name}</Select.Item>
						{/each}
					</Select.Content>
				</Select.Root>
			{/if}
		</div>
		{#if chartsError}
			<ErrorState message={chartsError} onRetry={() => loadCharts(prefs.chartsCountry)} />
		{:else if chartsLoading}
			<div class="flex gap-2 overflow-hidden pb-2" aria-hidden="true">
				{#each Array(6) as _, i (i)}
					<div class="w-40 shrink-0"><MediaCardSkeleton /></div>
				{/each}
			</div>
		{:else if charts?.sections.length}
			<div class="content-in flex flex-col gap-10">
				{#each charts.sections as section (section.title)}
					<Shelf
						title={section.title}
						items={section.items}
						queueAll={false}
						onMore={onMore(section)}
					/>
				{/each}
			</div>
		{:else}
			<!-- A real answer for the smaller countries: YouTube publishes no chart for them. -->
			<p class="flex items-center gap-2 text-sm text-muted-foreground">
				<HugeiconsIcon icon={ChartAverageIcon} class="size-4" />
				{t('explore.no_charts')}
			</p>
		{/if}
	</section>
</div>
