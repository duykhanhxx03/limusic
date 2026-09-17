<script lang="ts">
	// The Library page's Songs tab (and, with `uploads`, its Uploads tab): one flat list with a
	// Shuffle all over the whole thing (issue #73).
	//
	// `FEmusic_liked_videos` is YouTube's own Library ▸ Songs despite the name, and
	// `FEmusic_library_privately_owned_tracks` is Uploads ▸ Songs. Both browse like any other
	// playlist, so this reads them through `get_playlist` and the Rust side gains nothing.
	// What pins that: `library_songs_browse_returns_tracks` in crates/innertube/tests/live_smoke.rs.
	import { onMount } from 'svelte';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		CloudUploadIcon,
		MusicNote01Icon,
		PlayIcon,
		ShuffleIcon
	} from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import TrackFilter, { filterTracks } from './TrackFilter.svelte';
	import TrackRow from './TrackRow.svelte';
	import TrackRowSkeleton from './TrackRowSkeleton.svelte';
	import ErrorState from './ErrorState.svelte';
	import * as api from '$lib/api';
	import type { SongItem } from '$lib/api';
	import { getCached, putCached, LIBRARY_SONGS_KEY } from '$lib/pagecache';
	import { thumb } from '$lib/thumb';
	import {
		openAddToPlaylist,
		openPlayer,
		playback,
		removeSongFromLibrary,
		songLibraryRemoval
	} from '$lib/player.svelte';
	import { rowWindow } from '$lib/rows';
	import { rowScroller } from '$lib/rows.svelte';
	import { t } from '$lib/i18n.svelte';

	// The same tab, pointed at a different browse id: Library ▸ Songs by default, or the tracks the
	// user uploaded to YouTube Music themselves. Both browse like a headerless playlist and page the
	// same way, so the only differences are the id and the words around it.
	// `limit` turns this into a preview (the Uploads ▸ All tab): the first few rows and a See all,
	// with no paging, so what sits below it on that page stays reachable.
	let {
		uploads = false,
		limit,
		onSeeAll
	}: { uploads?: boolean; limit?: number; onSeeAll?: () => void } = $props();
	const BROWSE_ID = $derived(uploads ? api.LIBRARY_UPLOADS_ID : api.LIBRARY_SONGS_ID);

	// Cached like every other browse page, so switching tabs (or leaving the Library and coming
	// back) paints the list instead of refetching it and losing every page you scrolled in.
	const KEY = $derived(uploads ? 'library:uploads' : LIBRARY_SONGS_KEY);
	type Cached = { items: SongItem[]; continuation?: string };

	// `$state.raw`, same reason as the playlist page: a deep proxy puts every read of every row
	// through a trap, and this list is as long as someone's library.
	let songs = $state.raw<SongItem[]>([]);
	let token = $state<string | undefined>(undefined);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let loadingMore = $state(false);
	let moreError = $state(false);

	// Windowed, not sliced. This used to render a growing page of rows — 100, then 200, then 300 —
	// which meant a five-thousand-song library eventually held thousands of live `TrackRow`
	// components, each with its own menu and deriveds. `content-visibility` spares their layout and
	// their paint but not the node, the style or the component, and the scroll went to pieces
	// somewhere past a thousand.
	//
	// Now only the rows around the viewport exist and the rest are two padded boxes (`rows.ts`) —
	// the same thing the playlist page does. The blocker the old comment here named ("that wants
	// its own scroller and this tab scrolls with the page") is gone: `attachWithin` walks up to the
	// page's `<main>` instead of needing a scroller of its own.
	const sc = rowScroller();

	// No debounce, unlike the playlist page's box: the walk a query kicks off is the same walk
	// whatever you typed, so delaying it only delays the answer, and one pass over a raw array of
	// a few thousand rows is well under a millisecond.
	let query = $state('');
	const filtering = $derived(!!query.trim());
	const shownSongs = $derived(filterTracks(songs, query));

	const nowId = $derived(playback.now?.videoId);
	// No total: this browse carries no header, so the only number there is is "how many pages have
	// been scrolled in", which reads as 25+ on a library of thousands. Match counts are honest (the
	// filter walks every page), and say so while that walk is still running.
	const line = $derived(
		filtering
			? token && !moreError
				? t('library.matching_songs_so_far', { count: shownSongs.length.toLocaleString() })
				: t('library.matching_songs', { count: shownSongs.length.toLocaleString() })
			: uploads
				? t('library.every_upload')
				: t('library.every_song_saved')
	);
	// Four covers for the mosaic. Distinct ones: a library that opens on six tracks off the same
	// album would otherwise draw the same sleeve four times.
	const covers = $derived([
		...new Set(songs.slice(0, 60).flatMap((s) => (s.thumbnail ? [s.thumbnail] : [])))
	]);

	// A rewritten thumbnail size Google's CDN doesn't serve 404s, and a decorative backdrop has to
	// degrade to nothing rather than a broken-image glyph (same guard as HomeHero).
	let artFailed = $state(false);
	$effect(() => {
		covers[0]; // re-arm when the artwork changes
		artFailed = false;
	});

	// The queue this tab builds. Not a `playFrom`: there is no page behind "the songs in your
	// library", so it has no business landing in recents or the sidebar's last-played order.
	const SOURCE = $derived(uploads ? 'Your uploads' : 'Your songs');

	onMount(() => {
		const cached = getCached<Cached>(KEY);
		if (cached) {
			songs = cached.items;
			token = cached.continuation;
			loading = false;
			return;
		}
		load();
	});

	function cache() {
		putCached(KEY, { items: songs, continuation: token } satisfies Cached);
	}

	async function load() {
		loading = true;
		error = null;
		moreError = false;
		try {
			const page = await api.getPlaylist(BROWSE_ID);
			songs = page.items;
			token = page.continuation;
			cache();
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	// One request at a time, shared: the pager and the filter walk below both ask for "the next
	// page", and a walk that started while the sentinel's fetch was in flight would otherwise see
	// no new rows land and give up with pages still unread.
	let inflight: Promise<void> | null = null;
	function loadMore(): Promise<void> {
		if (moreError || !token) return Promise.resolve();
		inflight ??= fetchMore().finally(() => (inflight = null));
		return inflight;
	}

	async function fetchMore() {
		const t = token;
		loadingMore = true;
		try {
			const more = await api.getPlaylistMore(t!);
			if (token !== t) return; // reloaded under us
			songs = [...songs, ...more.items];
			// An empty page would leave the sentinel in view with nothing left to show: that's the end.
			token = more.items.length ? more.continuation : undefined;
			cache();
		} catch {
			// Stop auto-loading and offer a retry, rather than spinning on a sentinel in view.
			moreError = true;
		} finally {
			loadingMore = false;
		}
	}

	// A filter can only match rows that have arrived, and a narrowed list never pushes the sentinel
	// back into view, so nothing else would ever fetch the rest: search has to cover the library,
	// not the pages scrolled so far. One walk at a time (`walking` is deliberately not `$state` —
	// it guards the effect, it shouldn't re-run it).
	let walking = false;
	$effect(() => {
		if (!filtering || !token || moreError || walking) return;
		walking = true;
		(async () => {
			while (token && !moreError) {
				const before = songs.length;
				await loadMore();
				if (songs.length === before) break; // no progress, and nothing left to try
			}
			walking = false;
		})();
	});

	// Only the rows around the viewport are rendered; the rest are two padded boxes (`rows.ts`).
	const win = $derived(
		rowWindow(sc.scrollTop - sc.offsetPx, sc.viewportPx, shownSongs.length, sc.rowPx)
	);

	// One page per approach to the bottom: the observer only fires as the sentinel enters view, and
	// the rows that land push it back out. It fetches now rather than also growing a DOM cap —
	// with the window there is no cap to grow, so reaching the bottom means exactly one thing.
	function sentinel(node: HTMLElement) {
		const io = new IntersectionObserver(([e]) => e.isIntersecting && loadMore(), {
			rootMargin: '600px 0px'
		});
		io.observe(node);
		return () => io.disconnect();
	}

	// The whole library, never the filtered view: a filter finds a song, it doesn't decide what
	// plays after it. The pages that haven't arrived ride along as the token, which the backend
	// walks into the queue behind what's playing (mixing them into the unplayed tail on shuffle),
	// so Shuffle all is a shuffle of the library and not of the first 25 songs.
	// Only where there is a way out: YouTube sends no menu for a song that is in this list because
	// its album is saved, and the album is what holds it. The Uploads tab is a different list
	// entirely, and removing from it would be deleting the upload.
	const removable = (song: SongItem) => !uploads && !!songLibraryRemoval(song);

	// Drop the row on success only, and write the shortened list back to the page cache: coming
	// back to the tab paints from there, and a row that is gone from YouTube must not reappear.
	async function remove(song: SongItem) {
		if (!(await removeSongFromLibrary(song))) return;
		songs = songs.filter((s) => s !== song);
		cache();
	}

	function play(start: number | null, shuffle = false) {
		if (!songs.length) return;
		openPlayer(shuffle ? undefined : songs[start ?? 0]);
		api.playPlaylist(songs, start, undefined, SOURCE, shuffle, token);
	}
</script>

{#if loading}
	<div class="mb-4 h-36 animate-pulse rounded-2xl bg-card/40"></div>
	{#each Array(8) as _, i (i)}
		<TrackRowSkeleton />
	{/each}
{:else if error}
	<ErrorState message={error} onRetry={load} />
{:else}
	<!-- The header of the list rather than a page header: a rounded band the covers of your own
	     library tint, so the tab has a face without pretending to be a playlist page. -->
	<div class="relative mb-4 overflow-hidden rounded-2xl bg-card">
		{#if covers[0] && !artFailed}
			<!-- 96px: blur-2xl throws away every detail bigger than a few pixels anyway (HomeHero). -->
			<img
				src={thumb(covers[0], 96)}
				alt=""
				class="pointer-events-none absolute inset-0 h-full w-full art-wash scale-110 object-cover opacity-60 blur-2xl"
				onerror={() => (artFailed = true)}
			/>
		{/if}
		<div class="absolute inset-0 bg-gradient-to-r from-background via-background/80 to-background/40"></div>
		<div class="relative flex flex-wrap items-center gap-4 p-4">
			{#if covers.length >= 4}
				<div class="grid h-28 w-28 shrink-0 grid-cols-2 grid-rows-2 overflow-hidden rounded-xl">
					{#each covers.slice(0, 4) as cover (cover)}
						<img src={thumb(cover, 400)} alt="" class="h-full w-full object-cover" />
					{/each}
				</div>
			{:else if covers.length}
				<img src={thumb(covers[0], 400)} alt="" class="h-28 w-28 shrink-0 rounded-xl object-cover" />
			{:else}
				<div class="flex h-28 w-28 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
					<HugeiconsIcon icon={uploads ? CloudUploadIcon : MusicNote01Icon} class="h-10 w-10" />
				</div>
			{/if}
			<div class="min-w-0 flex-1">
				<h2 class="font-heading text-2xl font-bold tracking-tight">
					{uploads ? t('library.uploads_tab') : t('common.songs')}
				</h2>
				<p class="mt-0.5 text-sm text-muted-foreground">
					{line}
				</p>
				<div class="mt-3 flex flex-wrap items-center gap-2">
					<Button class="gap-2 rounded-full" disabled={!songs.length} onclick={() => play(null, true)}>
						<HugeiconsIcon icon={ShuffleIcon} class="h-4 w-4" /> {t('common.shuffle_all')}
					</Button>
					<Button
						variant="outline"
						class="gap-2 rounded-full"
						disabled={!songs.length}
						onclick={() => play(null)}
					>
						<HugeiconsIcon icon={PlayIcon} class="h-4 w-4" /> {t('common.play_all')}
					</Button>
				</div>
			</div>
			<TrackFilter bind:value={query} placeholder={t('common.search_your_songs')} />
		</div>
	</div>

	{#if shownSongs.length}
		<!-- `attachWithin`, not `attach`: the scrolling box is the route's <main>, not anything this
		     component owns. -->
		<div class="content-in" {@attach sc.attachWithin}>
			<!-- data-rows: where the scroller measures row 0 from, since the header above scrolls
			     away with the list. data-row: where it measures a row's real height from.
			     Keyed on the id *and* the position: nothing stops the same song sitting in a library
			     twice, and a repeated key is a crash. -->
			<div
				data-rows
				style={limit ? undefined : `padding-top:${win.padTop}px;padding-bottom:${win.padBottom}px`}
			>
				{#each limit ? shownSongs.slice(0, limit) : shownSongs.slice(win.start, win.end) as song, i (song.video_id + (limit ? i : win.start + i))}
					<div data-row>
						<TrackRow
							{song}
							index={limit ? i : win.start + i}
							active={song.video_id === nowId}
							inLibraryList={!uploads}
							onplay={() => play(songs.indexOf(song))}
							onAdd={() => openAddToPlaylist(song)}
							onRemove={removable(song) ? () => remove(song) : undefined}
							removeLabel={t('library.remove_from_library')}
						/>
					</div>
				{/each}
			</div>
		</div>
	{:else if filtering}
		<p class="text-sm text-muted-foreground">
			{t('library.no_tracks_match_loading', {
				query: query.trim(),
				loading: token && !moreError ? t('library.still_loading') : ''
			})}
		</p>
	{:else}
		<p class="text-sm text-muted-foreground">
			{uploads ? t('library.no_uploads') : t('library.no_library_songs')}
		</p>
	{/if}

	{#if limit}
		{#if shownSongs.length > limit}
			<Button variant="outline" size="sm" class="mt-2 rounded-full" onclick={onSeeAll}>
				{t('common.see_all')}
			</Button>
		{/if}
	{:else if moreError}
		<div class="p-3 text-center">
			<Button variant="outline" size="sm" onclick={() => ((moreError = false), loadMore())}>
				{loadingMore ? t('common.loading') : t('common.try_again')}
			</Button>
		</div>
	{:else if token}
		<div aria-busy={loadingMore}>
			<div {@attach sentinel}></div>
			{#if loadingMore}
				{#each Array(4) as _, i (i)}
					<TrackRowSkeleton />
				{/each}
			{/if}
		</div>
	{/if}
{/if}
