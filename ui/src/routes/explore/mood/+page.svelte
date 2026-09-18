<script lang="ts">
	// One mood or genre from Explore: YouTube's own playlist carousels for it ("Coffee shop blends",
	// "Chilled", …). The response has the home feed's shape, so the shelves are drawn the same way.
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import Shelf from '$lib/components/Shelf.svelte';
	import MediaCardSkeleton from '$lib/components/MediaCardSkeleton.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import { Skeleton } from '$lib/components/ui/skeleton';
	import * as api from '$lib/api';
	import type { HomePage, HomeSection } from '$lib/api';
	import { moreHref } from '$lib/browse';
	import { getCached, putCached } from '$lib/pagecache';
	import { reveal } from '$lib/reveal.svelte';
	import { t } from '$lib/i18n.svelte';

	let mood = $state<HomePage | null>(null);
	let error = $state<string | null>(null);
	// A mood is fifteen shelves of up to fifty cards; render them a few at a time rather than
	// mounting several hundred covers on arrival.
	const rv = reveal(4, 4);

	const params = $derived(page.url.searchParams.get('params') ?? '');
	const title = $derived(page.url.searchParams.get('title') ?? '');

	async function load(p: string) {
		const key = `mood:${p}`;
		const hit = getCached<HomePage>(key);
		mood = hit;
		rv.reset();
		error = null;
		try {
			const fresh = await api.getMood(p);
			if (p !== params) return; // superseded by another chip
			mood = fresh;
			rv.reset();
			putCached(key, fresh);
		} catch (e) {
			if (p !== params) return;
			if (!hit) error = String(e);
		}
	}

	const onMore = (s: HomeSection) => (s.moreBrowseId ? () => goto(moreHref(s)) : undefined);

	$effect(() => {
		if (params) load(params);
	});
</script>

<div class="p-6">
	<h1 class="mb-6 font-heading text-2xl font-bold">{title || t('nav.explore')}</h1>
	{#if error}
		<ErrorState message={error} onRetry={() => load(params)} />
	{:else if !mood}
		{#each Array(3) as _, s (s)}
			<section class="mb-10" aria-hidden="true">
				<Skeleton class="mb-3 h-5 w-40 rounded" />
				<div class="flex gap-2 overflow-hidden pb-2">
					{#each Array(6) as _, i (i)}
						<div class="w-40 shrink-0"><MediaCardSkeleton /></div>
					{/each}
				</div>
			</section>
		{/each}
	{:else if mood.sections.length}
		<div class="content-in flex flex-col gap-10">
			{#each mood.sections.slice(0, rv.count(mood.sections.length)) as section (section.title)}
				<Shelf
					title={section.title}
					items={section.items}
					queueAll={false}
					onMore={onMore(section)}
				/>
			{/each}
		</div>
		{#if rv.more(mood.sections.length)}<div {@attach rv.sentinel}></div>{/if}
	{:else}
		<p class="text-sm text-muted-foreground">{t('common.nothing_here')}</p>
	{/if}
</div>
