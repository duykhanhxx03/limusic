<script module lang="ts">
	type Filter = 'all' | 'songs' | 'videos' | 'albums' | 'artists' | 'playlists';
	// Survive remounts (module scope), so coming back to /search — from a result you clicked, or
	// from the sidebar — shows the last search, under the filter it was being read through, instead
	// of a blank page. The results themselves come back from the page cache, so the rerun paints
	// instantly and just revalidates.
	let lastQuery = '';
	let lastFilter: Filter = 'all';
</script>

<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		ArrowUpLeft01Icon,
		Cancel01Icon,
		Clock01Icon,
		Search01Icon
	} from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { Skeleton } from '$lib/components/ui/skeleton';
	import MediaCard from '$lib/components/MediaCard.svelte';
	import MediaCardSkeleton from '$lib/components/MediaCardSkeleton.svelte';
	import MoodGrid from '$lib/components/MoodGrid.svelte';
	import SearchSuggest from '$lib/components/SearchSuggest.svelte';
	import TrackRow from '$lib/components/TrackRow.svelte';
	import TrackSelectionBar from '$lib/components/TrackSelectionBar.svelte';
	import TrackSelectButton from '$lib/components/TrackSelectButton.svelte';
	import { trackSelection } from '$lib/selection.svelte';
	import TrackRowSkeleton from '$lib/components/TrackRowSkeleton.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import Shelf from '$lib/components/Shelf.svelte';
	import * as api from '$lib/api';
	import type { BrowseItem, SearchResults, SongItem } from '$lib/api';
	import { getCached, putCached } from '$lib/pagecache';
	import { auth, openAddToPlaylist, playSong, prefs } from '$lib/player.svelte';
	import { asSong, searchKey, sharedSearchAll, type CachedSearch } from '$lib/browse';
	import { chipClass } from '$lib/chip';
	import { t } from '$lib/i18n.svelte';

	/** One filter's page: a track list (songs, videos) or a card grid (everything else). */
	type Filtered = { songs: SongItem[]; cards: BrowseItem[] };

	let query = $state(lastQuery);
	let filter = $state<Filter>(lastFilter);
	let inputRef = $state<HTMLInputElement | null>(null);
	let scroller = $state<HTMLElement | null>(null);

	// --- the "All" view: every category at once --------------------------------------------------
	let res = $state<SearchResults | null>(null);
	// The Songs shelf comes from the songs-filtered search, not from `res.songs`: an unfiltered
	// response gives a song row either its artist or its length, never both, so those rows land
	// duration-less. The filtered endpoint returns "Artist • Album • 3:58" on every row.
	let songs = $state<SongItem[]>([]);

	// --- one filter at a time ----------------------------------------------------------------------
	let filtered = $state<Filtered | null>(null);

	let searched = $state('');
	let searching = $state(false);
	let error = $state<string | null>(null);

	/** `filter:query` of the newest load, so an older one still in flight can't clobber it. */
	let latest = '';

	const isList = (f: Filter) => f === 'songs' || f === 'videos';

	async function loadAll(q: string) {
		const key = searchKey(q);
		// Possibly the typeahead's entry, which has no filtered songs yet: the Songs shelf shows the
		// unfiltered rows until the fresh ones below replace them.
		const hit = getCached<CachedSearch>(key);
		if (hit) {
			res = hit.res;
			songs = hit.songs;
			searched = q;
			searching = false;
		} else {
			searching = true;
		}
		try {
			// In parallel, and the filtered one may fail on its own: the shelf falls back to the
			// unfiltered rows rather than the whole search erroring out. The unfiltered one joins
			// the typeahead's request if Enter came while that was still out.
			const [fresh, freshSongs] = await Promise.all([
				sharedSearchAll(q),
				api.search(q).catch(() => [] as SongItem[])
			]);
			if (latest !== `all:${q}`) return;
			res = fresh;
			songs = freshSongs;
			searched = q;
			putCached(key, { res: fresh, songs: freshSongs } satisfies CachedSearch);
		} catch (e) {
			if (latest !== `all:${q}`) return;
			if (!hit) error = String(e);
		} finally {
			if (latest === `all:${q}`) searching = false;
		}
	}

	async function loadFiltered(q: string, f: Exclude<Filter, 'all'>) {
		const key = `searchmore:${f}:${q}`;
		const hit = getCached<Filtered>(key);
		filtered = hit;
		searching = !hit;
		if (hit) searched = q;
		try {
			const fresh: Filtered =
				f === 'songs'
					? { songs: await api.search(q), cards: [] }
					: f === 'videos'
						? { songs: await api.searchVideos(q), cards: [] }
						: { songs: [], cards: await api.searchCards(q, f) };
			if (latest !== `${f}:${q}`) return;
			filtered = fresh;
			searched = q;
			putCached(key, fresh);
		} catch (e) {
			if (latest !== `${f}:${q}`) return;
			if (!hit) error = String(e);
		} finally {
			if (latest === `${f}:${q}`) searching = false;
		}
	}

	function load(q: string, f: Filter) {
		latest = `${f}:${q}`;
		error = null;
		if (f === 'all') loadAll(q);
		else loadFiltered(q, f);
	}

	/** `record` is off only for the silent rerun on returning to the page: that is not a search
	 *  the user made, and bumping it to the top of their history would be a lie. */
	function runSearch(record = true) {
		const q = query.trim();
		if (!q) return;
		lastQuery = q;
		if (record) remember(q);
		load(q, filter);
	}

	/** A chip. Applies to the query on screen, not whatever is half-typed in the box. */
	function pick(f: Filter) {
		if (f === filter) return;
		filter = f;
		lastFilter = f;
		scroller?.scrollTo({ top: 0 });
		const q = searched || query.trim();
		if (q) load(q, f);
	}

	// --- history ---------------------------------------------------------------------------------
	let history = $state<string[]>([]);

	const refreshHistory = () =>
		api
			.getSearchHistory()
			.then((h) => (history = h))
			.catch(() => {});

	function remember(q: string) {
		// Optimistic, so the list is already right if the box is cleared before the write lands.
		history = [q, ...history.filter((h) => h.toLowerCase() !== q.toLowerCase())];
		api.addSearchHistory(q).catch(() => {});
	}

	function searchFor(q: string) {
		query = q;
		runSearch();
	}

	/** The ↖ on a row: put the search back in the box to change it, without running it. */
	function editQuery(q: string) {
		query = q;
		inputRef?.focus();
		// After the binding has written the value, or the cursor lands in front of it.
		queueMicrotask(() => inputRef?.setSelectionRange(q.length, q.length));
	}

	function forget(q: string) {
		history = history.filter((h) => h !== q);
		api.removeSearchHistory(q).catch(() => {});
	}

	function forgetAll() {
		history = [];
		api.removeSearchHistory().catch(() => {});
	}

	// "Clear all" asks first: it wipes every search at once and there is no undo, where the × on a
	// row only ever costs that one row.
	let confirmingClear = $state(false);
	function clearAll() {
		// bits-ui's Action is a plain button (only Cancel closes the dialog), so it closes here.
		confirmingClear = false;
		forgetAll();
	}

	// --- arriving ----------------------------------------------------------------------------------
	// Run the search when arriving with a ?q= (e.g. from the Home search box). Keyed on the URL
	// alone: typing a new query in the field must not look like a URL change and bounce us back.
	const urlQuery = $derived(page.url.searchParams.get('q') ?? '');
	let lastUrlQuery = '';
	$effect(() => {
		if (urlQuery && urlQuery !== lastUrlQuery) {
			lastUrlQuery = urlQuery;
			query = urlQuery;
			runSearch();
		}
	});

	// Arriving without a ?q= (back from a result, or the sidebar link): rerun whatever was last
	// searched. onMount, not the effect above, so a ?q= arrival still wins.
	onMount(() => {
		refreshHistory();
		if (!urlQuery && query) runSearch(false);
	});

	// With "hide music videos" on, a Videos search can only come back empty — so there is no chip,
	// and a remembered Videos filter falls back to All rather than stranding the page on nothing.
	$effect(() => {
		if (prefs.hideVideos && filter === 'videos') pick('all');
	});

	const filters = $derived(
		(
			[
				['all', t('common.all')],
				['songs', t('common.songs')],
				['videos', t('common.videos')],
				['albums', t('common.albums')],
				['artists', t('common.artists')],
				['playlists', t('common.playlists')]
			] as [Filter, string][]
		).filter(([f]) => f !== 'videos' || !prefs.hideVideos)
	);

	const songRows = $derived(songs.length ? songs : (res?.songs ?? []).map(asSong));
	const previewSongs = $derived(songRows.slice(0, 6));
	const selection = trackSelection(() => previewSongs, () => previewSongs, () => `${auth.epoch}:${searched}`);
	const listRows = $derived(filtered?.songs ?? []);
	const listSelection = trackSelection(
		() => listRows,
		() => listRows,
		() => `${auth.epoch}:${filter}:${searched}`
	);

	// Sections are horizontal card rows, except Songs which is a vertical list. `top` has no "show more".
	const sections = $derived(
		res
			? [
					{ key: 'top', label: t('common.top_results'), items: res.top, max: 4, more: false, list: false },
					{ key: 'songs', label: t('common.songs'), items: res.songs, max: 6, more: true, list: true },
					{ key: 'albums', label: t('common.albums'), items: res.albums, max: 5, more: true, list: false },
					{ key: 'artists', label: t('common.artists'), items: res.artists, max: 3, more: true, list: false },
					{ key: 'playlists', label: t('common.playlists'), items: res.playlists, max: 5, more: true, list: false }
				].filter((s) => (s.list ? songRows.length : s.items.length))
			: []
	);

	const typing = $derived(query.trim().length > 0);
</script>

<div class="flex h-full flex-col">
	<div class="p-6 pb-3">
		<h1 class="mb-4 font-heading text-2xl font-bold">{t('common.search')}</h1>
		<form
			class="flex max-w-xl gap-2"
			onsubmit={(e) => {
				e.preventDefault();
				runSearch();
			}}
		>
			<SearchSuggest
				bind:value={query}
				bind:inputRef
				placeholder={t('common.search_placeholder')}
				onpick={() => (lastQuery = query)}
			/>
			<Button type="submit" class="gap-2" disabled={searching}>
				<HugeiconsIcon icon={Search01Icon} class="h-4 w-4" />
				{searching ? t('common.searching') : t('common.search')}
			</Button>
		</form>
		<!-- The filters. Only while there is a search to filter: over an empty box they would be
		     six buttons that do nothing. -->
		{#if typing && searched}
			<div class="mt-4 flex gap-2 overflow-x-auto pb-1" role="group" aria-label={t('search.filters')}>
				{#each filters as [f, label] (f)}
					<button type="button" class={chipClass(filter === f)} aria-pressed={filter === f} onclick={() => pick(f)}>
						{label}
					</button>
				{/each}
				<!-- A track list's select button rides at the end of the filter row, where the list's
				     heading would be if it had one. -->
				{#if isList(filter) && listRows.length}
					<span class="flex-1"></span>
					<TrackSelectButton selection={listSelection} />
				{/if}
			</div>
		{/if}
		{#if error}<div class="mt-2"><ErrorState message={error} onRetry={() => runSearch(false)} /></div>{/if}
	</div>

	<div class="min-h-0 flex-1 overflow-y-auto p-6 pt-3" bind:this={scroller}>
		{#if !typing}
			<!-- An empty box, answered the way Spotify answers it: what you looked for before, then
			     every mood and genre to browse when you don't have a name to type. -->
			<div class="content-in flex flex-col gap-10">
				{#if history.length}
					<section class="max-w-xl">
						<div class="mb-2 flex items-center justify-between">
							<h2 class="font-heading text-2xl font-bold tracking-tight">{t('search.recent')}</h2>
							<button
								type="button"
								class="cursor-pointer text-sm font-bold text-muted-foreground transition-colors hover:text-foreground hover:underline"
								onclick={() => (confirmingClear = true)}
							>
								{t('search.clear_all')}
							</button>
						</div>
						<ul>
							{#each history as h (h)}
								<li class="group flex items-center rounded-lg transition-colors hover:bg-foreground/5">
									<button
										type="button"
										class="flex min-w-0 flex-1 cursor-pointer items-center gap-3 px-3 py-2 text-left text-sm"
										onclick={() => searchFor(h)}
									>
										<HugeiconsIcon icon={Clock01Icon} class="size-4 shrink-0 text-muted-foreground" />
										<span class="truncate">{h}</span>
									</button>
									<button
										type="button"
										class="cursor-pointer rounded-md p-2 text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
										title={t('search.edit')}
										aria-label={t('search.edit')}
										onclick={() => editQuery(h)}
									>
										<HugeiconsIcon icon={ArrowUpLeft01Icon} class="size-4" />
									</button>
									<button
										type="button"
										class="mr-1 cursor-pointer rounded-md p-2 text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
										title={t('search.remove')}
										aria-label={t('search.remove')}
										onclick={() => forget(h)}
									>
										<HugeiconsIcon icon={Cancel01Icon} class="size-4" />
									</button>
								</li>
							{/each}
						</ul>
					</section>
				{/if}
				<section>
					<h2 class="mb-4 font-heading text-2xl font-bold tracking-tight">{t('search.browse_all')}</h2>
					<MoodGrid groupHeading="mb-2 text-base font-bold text-muted-foreground" />
				</section>
			</div>
		{:else if filter !== 'all'}
			{#if searching}
				{#if isList(filter)}
					{#each Array(10) as _, i (i)}
						<TrackRowSkeleton />
					{/each}
				{:else}
					<div class="card-grid">
						{#each Array(12) as _, i (i)}
							<MediaCardSkeleton />
						{/each}
					</div>
				{/if}
			{:else if isList(filter)}
				{#if listRows.length}
					<div class="content-in">
						<TrackSelectionBar selection={listSelection} />
						{#each listRows as song, i (JSON.stringify([song.video_id, i]))}
							<TrackRow
								{song}
								selection={listSelection}
								selectionKey={listSelection.visibleKeys[i]}
								showPlayCount
								onplay={() => playSong(song)}
								onAdd={() => openAddToPlaylist(song)}
							/>
						{/each}
					</div>
				{:else if filtered}
					<p class="text-sm text-muted-foreground">{t('common.no_results', { query: searched })}</p>
				{/if}
			{:else if filtered?.cards.length}
				<div class="card-grid content-in">
					{#each filtered.cards as item (item.id + item.title)}
						<MediaCard {item} />
					{/each}
				</div>
			{:else if filtered}
				<p class="text-sm text-muted-foreground">{t('common.no_results', { query: searched })}</p>
			{/if}
		{:else if searching}
			<div class="flex flex-col gap-10">
				<section>
					<Skeleton class="mb-3 h-6 w-40 rounded" />
					{#each Array(5) as _, i (i)}
						<TrackRowSkeleton />
					{/each}
				</section>
				<section>
					<Skeleton class="mb-3 h-6 w-32 rounded" />
					<div class="flex gap-2 overflow-hidden pb-2">
						{#each Array(5) as _, i (i)}
							<div class="w-40 shrink-0"><MediaCardSkeleton /></div>
						{/each}
					</div>
				</section>
			</div>
		{:else if !res}
			<p class="text-sm text-muted-foreground">{t('common.search_prompt')}</p>
		{:else if !sections.length}
			<p class="text-sm text-muted-foreground">{t('common.no_results', { query: searched })}</p>
		{:else}
			<div class="content-in flex flex-col gap-10">
				{#each sections as sec (sec.key)}
					<section>
						<div class="mb-3 flex items-center justify-between">
							<h2 class="font-heading text-2xl font-bold tracking-tight">{sec.label}</h2>
							<div class="flex items-center gap-1">
								{#if sec.list}
									<TrackSelectButton {selection} />
								{/if}
								{#if sec.more}
									<!-- "Show more" is the category's own filter: the same list, one chip over. -->
									<button
										class="cursor-pointer text-sm font-bold text-muted-foreground transition-colors hover:text-foreground hover:underline"
										onclick={() => pick(sec.key as Filter)}
									>
										{t('common.show_more')}
									</button>
								{/if}
							</div>
						</div>
						{#if sec.list}
							<TrackSelectionBar {selection} />
							{#each previewSongs as song, i (JSON.stringify([song.video_id, i]))}
								<TrackRow
									{song}
									{selection}
									selectionKey={selection.visibleKeys[i]}
									showPlayCount
									onplay={() => playSong(song)}
									onAdd={() => openAddToPlaylist(song)}
								/>
							{/each}
						{:else}
							<Shelf items={sec.items.slice(0, sec.max)} />
						{/if}
					</section>
				{/each}
			</div>
		{/if}
	</div>
</div>

<AlertDialog.Root bind:open={confirmingClear}>
	<AlertDialog.Content>
		<AlertDialog.Header>
			<AlertDialog.Title>{t('search.clear_all_title')}</AlertDialog.Title>
			<AlertDialog.Description>{t('search.clear_all_desc')}</AlertDialog.Description>
		</AlertDialog.Header>
		<AlertDialog.Footer>
			<AlertDialog.Cancel>{t('common.cancel')}</AlertDialog.Cancel>
			<AlertDialog.Action onclick={clearAll}>{t('search.clear_all')}</AlertDialog.Action>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
