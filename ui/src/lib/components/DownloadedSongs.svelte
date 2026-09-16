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
	import { t } from '$lib/i18n.svelte';

	let rows = $state<Downloaded[]>([]);
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

	// Reloads when a download finishes: `dl.saved` changing is the signal, and it is cheap (one
	// database read, no network).
	$effect(() => {
		dl.saved.size;
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
	{#each rows as d, i (d.videoId)}
		<div class="group/dl relative">
			<TrackRow
				song={songs[i]}
				onplay={() => play(i)}
				onAdd={() => openAddToPlaylist(songs[i])}
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
{/if}
