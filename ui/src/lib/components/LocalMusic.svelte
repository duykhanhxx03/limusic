<script lang="ts">
	// The Library page's Local tab: music that lives on this machine. Works signed out and offline,
	// because nothing here goes near YouTube (Rust `local.rs`). Albums open the normal album page
	// and songs play through the normal queue, so everything past this component is shared.
	import { onMount } from 'svelte';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { open } from '@tauri-apps/plugin-dialog';
	import {
		Add01Icon,
		Delete02Icon,
		DriveIcon,
		PlayIcon,
		RefreshIcon,
		ShuffleIcon
	} from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import * as Tabs from '$lib/components/ui/tabs';
	import MediaCard from './MediaCard.svelte';
	import MediaCardSkeleton from './MediaCardSkeleton.svelte';
	import ErrorState from './ErrorState.svelte';
	import TrackFilter from './TrackFilter.svelte';
	import TrackRow from './TrackRow.svelte';
	import * as api from '$lib/api';
	import { indexCards, indexSongs, match } from '$lib/localsearch';
	import { reveal } from '$lib/reveal.svelte';
	import { rowWindow } from '$lib/rows';
	import { rowScroller } from '$lib/rows.svelte';
	import { t } from '$lib/i18n.svelte';
	import {
		addLocalFolder,
		local,
		openPlayer,
		playback,
		removeLocalFolder,
		scanLocal,
		toast
	} from '$lib/player.svelte';

	// Files come and go while the app runs, so the tab rescans when you open it (a no-op scan is a
	// stat per file). The startup scan is what keeps deleted music out of the home grid.
	onMount(() => {
		scanLocal();
	});

	let view = $state('albums');

	// Filtering the collection. It is all in memory already, so this is a scan and not a request:
	// no debounce, no loading state, the lists narrow as you type. The one cost worth dodging is
	// Svelte's — `local` is `$state`, so reading `song.title` for thousands of songs on every
	// keystroke goes through as many proxy traps — so `localsearch.ts` flattens the text into plain
	// strings once per library change and each keystroke scans those. Reading `ix` at all is behind
	// the empty-query check below, so a library nobody searches never pays for the pass.
	let query = $state('');
	const ix = $derived({
		songs: indexSongs(local.songs),
		albums: indexCards(local.albums),
		artists: indexCards(local.artists)
	});
	const q = $derived(query.trim());
	const songs = $derived(q ? match(ix.songs, q) : local.songs);
	const albums = $derived(q ? match(ix.albums, q) : local.albums);
	const artists = $derived(q ? match(ix.artists, q) : local.artists);

	// A local collection can be thousands of files, and WebKitGTK does not enjoy thousands of rows.
	// The songs are windowed, the way LibrarySongs is: only the rows around the viewport exist, and
	// the rest are two padded boxes (`rows.ts`). This used to grow a page of 100 rows per approach
	// to the bottom and never shrink, so scrolling to the end of a big collection kept one live
	// TrackRow per file until the tab closed. Play all and Shuffle still take every song in the list.
	// `attachWithin` because the scrolling box is the Library route's <main>, not this component.
	const sc = rowScroller();
	const win = $derived(
		rowWindow(sc.scrollTop - sc.offsetPx, sc.viewportPx, songs.length, sc.rowPx)
	);

	// The album and artist grids are revealed in chunks, as the Library's own grids are: a
	// MediaCard carries a menu and deriveds of its own, and a collection with hundreds of albums
	// otherwise builds every card in one pass on the tab click. One per view, so switching between
	// them keeps each one's depth (the Library page's note on why one shared instance does not work).
	const rvAlbums = reveal();
	const rvArtists = reveal();
	// A narrower list starts from the first chunk again. `.pre` so the reset lands before the grid
	// renders the new list: a plain `$effect` runs after it, and would build the filtered grid
	// against the old depth and then tear the excess back down.
	$effect.pre(() => {
		q;
		rvAlbums.reset();
		rvArtists.reset();
	});

	const nowId = $derived(playback.now?.videoId);
	// The song list as one queue — what the Play/Shuffle buttons above it do, and what the queue
	// panel calls it. Not a `playFrom`: there's no page behind "the music on this disk", so it has
	// no business landing in recents or the sidebar's last-played order. Named after the tab, in the
	// app's language, since the queue panel shows it.
	const SOURCE = $derived(t('library.local_tab'));

	async function pickFolder() {
		const picked = await open({ directory: true, multiple: false, title: t('local.pick_folder_dialog') });
		if (typeof picked === 'string') await addLocalFolder(picked);
	}

	// The filtered list, not the whole library: Play all plays what the list shows.
	function playAll(shuffle: boolean) {
		if (!songs.length) return;
		openPlayer();
		api.playPlaylist(songs, null, undefined, SOURCE, shuffle);
	}

	async function forget(path: string) {
		await removeLocalFolder(path);
		toast.success(t('toasts.folder_removed'));
	}
</script>

<div class="flex flex-col gap-5">
	<!-- Folders -->
	<div class="rounded-xl bg-card/40 p-4">
		<div class="mb-3 flex items-center justify-between gap-3">
			<div class="min-w-0">
				<div class="flex items-center gap-2 font-bold">
					<HugeiconsIcon icon={DriveIcon} class="h-4 w-4" /> {t('local.folders')}
				</div>
				<p class="mt-0.5 text-xs text-muted-foreground">{t('local.folders_hint')}</p>
			</div>
			<div class="flex shrink-0 gap-2">
				<Button
					variant="ghost"
					size="sm"
					class="gap-2"
					disabled={local.loading || !local.folders.length}
					onclick={() => scanLocal()}
				>
					<HugeiconsIcon icon={RefreshIcon} class="h-4 w-4" />
					{local.loading ? t('local.scanning') : t('local.rescan')}
				</Button>
				<Button variant="outline" size="sm" class="gap-2" onclick={pickFolder}>
					<HugeiconsIcon icon={Add01Icon} class="h-4 w-4" /> {t('local.add_folder')}
				</Button>
			</div>
		</div>
		{#if local.folders.length}
			<ul class="flex flex-col gap-1">
				{#each local.folders as folder (folder)}
					<li class="flex items-center justify-between gap-3 rounded-lg px-2 py-1.5 hover:bg-accent/10">
						<span class="truncate font-mono text-xs text-muted-foreground" title={folder}>
							{folder}
						</span>
						<button
							class="flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted-foreground transition hover:bg-destructive/10 hover:text-destructive"
							aria-label={t('a11y.remove_folder')}
							onclick={() => forget(folder)}
						>
							<HugeiconsIcon icon={Delete02Icon} class="h-4 w-4" />
						</button>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-sm text-muted-foreground">{t('local.no_folders')}</p>
		{/if}
	</div>

	{#if local.error}
		<ErrorState message={local.error} onRetry={() => scanLocal()} />
	{:else if local.loading && !local.scanned}
		<div class="card-grid">
			{#each Array(6) as _, i (i)}
				<MediaCardSkeleton />
			{/each}
		</div>
	{:else if local.songs.length}
		<Tabs.Root bind:value={view}>
			<!-- The counts follow the filter, so the tabs say where the matches are. -->
			<div class="mb-4 flex flex-wrap items-center justify-between gap-3">
				<Tabs.List>
					<Tabs.Trigger value="albums">{t('local.albums_count', { count: albums.length })}</Tabs.Trigger>
					<Tabs.Trigger value="artists">{t('local.artists_count', { count: artists.length })}</Tabs.Trigger>
					<Tabs.Trigger value="songs">{t('local.songs_count', { count: songs.length })}</Tabs.Trigger>
				</Tabs.List>
				<TrackFilter bind:value={query} placeholder={t('common.search_your_music')} />
			</div>
			{#if q && !songs.length && !albums.length && !artists.length}
				<p class="text-sm text-muted-foreground">{t('local.nothing_matches_device', { query: q })}</p>
			{/if}
			<!-- Gated on `view`: bits-ui hides an inactive panel rather than unmounting it, so all three
			     views of the same collection would be built on every visit. -->
			<Tabs.Content value="albums">
				{#if view === 'albums'}
					<div class="card-grid content-in">
						{#each albums.slice(0, rvAlbums.count(albums.length)) as album (album.id)}
							<MediaCard item={album} />
						{/each}
					</div>
					<!-- Outside the grid, or it would be laid out as a cell. -->
					{#if rvAlbums.more(albums.length)}<div {@attach rvAlbums.sentinel}></div>{/if}
				{/if}
			</Tabs.Content>
			<Tabs.Content value="artists">
				{#if view === 'artists'}
					<div class="card-grid content-in">
						{#each artists.slice(0, rvArtists.count(artists.length)) as artist (artist.id)}
							<MediaCard item={artist} />
						{/each}
					</div>
					{#if rvArtists.more(artists.length)}<div {@attach rvArtists.sentinel}></div>{/if}
				{/if}
			</Tabs.Content>
			<Tabs.Content value="songs">
				{#if view === 'songs'}
					<div class="mb-3 flex gap-2">
						<Button
							size="lg"
							class="gap-2"
							disabled={!songs.length}
							onclick={() => playAll(false)}
						>
							<HugeiconsIcon icon={PlayIcon} class="h-4 w-4" /> {t('common.play_all')}
						</Button>
						<Button
							variant="secondary"
							size="lg"
							class="gap-2"
							disabled={!songs.length}
							onclick={() => playAll(true)}
						>
							<HugeiconsIcon icon={ShuffleIcon} class="h-4 w-4" /> {t('common.shuffle')}
						</Button>
					</div>
					<div class="content-in" {@attach sc.attachWithin}>
						<!-- data-rows: where the scroller measures row 0 from, since the folders, the
						     tabs and the buttons above scroll away with the list. data-row: where it
						     measures a row's real height from. `n` is the row's place in the whole
						     list, not in the slice, for its number and for where playback starts. -->
						<div data-rows style="padding-top:{win.padTop}px;padding-bottom:{win.padBottom}px">
							{#each songs.slice(win.start, win.end) as song, i (song.video_id)}
								{@const n = win.start + i}
								<div data-row>
									<TrackRow
										{song}
										index={n}
										active={song.video_id === nowId}
										onplay={() => {
											openPlayer();
											api.playPlaylist(songs, n, undefined, SOURCE);
										}}
									/>
								</div>
							{/each}
						</div>
					</div>
				{/if}
			</Tabs.Content>
		</Tabs.Root>
	{:else if local.folders.length}
		<p class="text-sm text-muted-foreground">{t('local.nothing_playable')}</p>
	{/if}
</div>
