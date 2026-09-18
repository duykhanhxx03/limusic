<script lang="ts">
	// The offline library. Reads the database and the disk only — no account, no connection — so it
	// is the one list that is guaranteed to work on a plane.
	//
	// Rows are turned into ordinary SongItems and played through the ordinary path: Rust swaps the
	// stream URL for the file in `AppState::resolve`, so queueing, gapless and shuffle all behave
	// exactly as they do online.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { Delete02Icon } from '@hugeicons/core-free-icons';
	import { convertFileSrc } from '@tauri-apps/api/core';
	import * as api from '$lib/api';
	import type { Downloaded, SongItem } from '$lib/api';
	import TrackRow from './TrackRow.svelte';
	import { dl, formatBytes, remove } from '$lib/downloads.svelte';
	import { openAddToPlaylist, playFrom, toast } from '$lib/player.svelte';
	import { rowWindow } from '$lib/rows';
	import { rowScroller } from '$lib/rows.svelte';
	import { t } from '$lib/i18n.svelte';

	// `$state.raw`: the list is only ever replaced whole, never edited in place, and a deep proxy
	// would put every read of every row through a trap (the same call LibrarySongs makes).
	let rows = $state.raw<Downloaded[]>([]);
	let loading = $state(true);

	async function load() {
		try {
			rows = await api.downloads();
		} catch (e) {
			toast.error(String(e));
		} finally {
			loading = false;
		}
	}

	// Reloads when a download finishes or is removed: `dl.version` is bumped for exactly those two,
	// and a reload is cheap (one database read, no network). Not `dl.saved`, which also changes
	// whenever any list finds rows that were already on disk. This tab's own rows do that on its
	// first open, so every visit used to load twice, and it reloaded again as other lists asked.
	$effect(() => {
		dl.version;
		load();
	});

	/** A stored row as the rest of the app understands it. The saved cover is a path, so it goes
	 *  through the asset protocol the way local artwork does. */
	const asSong = (d: Downloaded): SongItem => ({
		video_id: d.videoId,
		title: d.title,
		artists: d.artists,
		duration: d.duration ?? undefined,
		thumbnail: d.thumbnail ? convertFileSrc(d.thumbnail) : undefined
	});

	const songs = $derived(rows.map(asSong));
	const totalBytes = $derived(rows.reduce((n, d) => n + d.bytes, 0));

	// Windowed like LibrarySongs: only the rows around the viewport exist, and the rest are two
	// padded boxes (`rows.ts`). Downloads are what someone keeps for a flight, so this list can be
	// as long as a library, and every rendered row costs its own component, menu and nodes.
	// `attachWithin` because the scrolling box is the Library route's <main>, not this component.
	const sc = rowScroller();
	const win = $derived(
		rowWindow(sc.scrollTop - sc.offsetPx, sc.viewportPx, rows.length, sc.rowPx)
	);

	function play(index: number) {
		playFrom(
			{ kind: 'playlist', id: 'LIMUSIC_DOWNLOADS', title: t('downloads.title') },
			songs,
			index
		);
	}
</script>

{#if loading}
	<p class="text-sm text-muted-foreground">{t('common.loading')}</p>
{:else if !rows.length}
	<p class="text-sm text-muted-foreground">{t('downloads.empty')}</p>
{:else}
	<p class="mb-3 text-xs text-muted-foreground">
		{t('downloads.count', { count: rows.length })} · {formatBytes(totalBytes)}
	</p>
	<div {@attach sc.attachWithin}>
		<!-- data-rows: where the scroller measures row 0 from, since the tabs and the count above
		     scroll away with the list. `n` is the row's place in the whole list, not in the slice,
		     so play and add reach the right song. videoId alone is a safe key: it is the downloads
		     table's primary key. -->
		<div data-rows style="padding-top:{win.padTop}px;padding-bottom:{win.padBottom}px">
			{#each rows.slice(win.start, win.end) as d, i (d.videoId)}
				{@const n = win.start + i}
				<!-- data-row on the wrapper, not on the TrackRow: the delete button hangs off it, and
				     this box is what the scroller measures a row's real height from. -->
				<div data-row class="group/dl relative">
					<TrackRow
						song={songs[n]}
						onplay={() => play(n)}
						onAdd={() => openAddToPlaylist(songs[n])}
					/>
					<!-- Over the row rather than in the ⋯ menu: on this page removing is the action people
					     came for, and the menu's own Download entry already handles it everywhere else. -->
					<button
						class="absolute right-2 top-1/2 z-10 -translate-y-1/2 rounded-md p-1.5 text-muted-foreground opacity-0 transition-opacity hover:text-destructive group-hover/dl:opacity-100"
						onclick={() => remove(d.videoId)}
						title={t('downloads.remove')}
						aria-label={t('downloads.remove')}
					>
						<HugeiconsIcon icon={Delete02Icon} class="h-4 w-4" />
					</button>
				</div>
			{/each}
		</div>
	</div>
{/if}
