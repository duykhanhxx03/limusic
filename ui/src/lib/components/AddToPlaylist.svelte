<script lang="ts">
	import * as Dialog from '$lib/components/ui/dialog';
	import * as api from '$lib/api';
	import type { BrowseItem } from '$lib/api';
	import { t } from '$lib/i18n.svelte';
	import {
		ui,
		auth,
		toast,
		bumpLibraryTrackCount,
		lastPlaylistAdd,
		notePlaylistAdd,
		noteSavedIn
	} from '$lib/player.svelte';

	let playlists = $state<BrowseItem[]>([]);
	let loading = $state(false);
	let filter = $state('');
	let box = $state<HTMLInputElement | null>(null);
	// `autofocus` is unreliable on an element inserted after load (and mid-transition), so focus it
	// ourselves the frame it exists: the modal opens ready to type.
	$effect(() => {
		box?.focus();
	});
	// ponytail: plain substring, not fuzzy. A library is tens of playlists, and "rap" finding
	// "Rap Caviar" is what issue #100 actually asked for.
	const matches = $derived(
		playlists.filter((p) => p.title.toLowerCase().includes(filter.trim().toLowerCase()))
	);

	// Fetch the library playlists fresh each time the picker opens (cheap; picks up new playlists).
	// On Repeat and Liked Music are dropped: On Repeat is built from local play counts, and Liked
	// Music takes likes rather than playlist edits (YouTube 400s the add). The command boundary
	// refuses both too, but a target you can tap and can't use is the bug.
	$effect(() => {
		if (ui.addSongs) {
			loading = true;
			filter = '';
			api
				.getLibrary()
				.then(
					(p) =>
						(playlists = p.filter(
							(i) => i.id !== api.ON_REPEAT_ID && i.id !== api.LIKED_MUSIC_ID
						))
				)
				.catch((e) => toast.error(String(e)))
				.finally(() => (loading = false));
		}
	});

	function close() {
		ui.addSongs = null;
	}

	async function pick(pl: BrowseItem) {
		if (ui.addPending) return;
		const songs = ui.addSongs;
		close();
		if (!songs?.length) return;
		if (songs.some((s) => api.isLocalId(s.video_id))) {
			toast.error(t('selection.local_playlist'));
			return;
		}
		const epoch = auth.epoch;
		ui.addPending = true;
		const added: typeof songs = [];
		// The new row's setVideoId for each of `added`, index for index.
		const rowIds: (string | undefined)[] = [];
		const confirmed: typeof songs = [];
		let failure: string | null = null;
		try {
			// Sequential — bulk selection can contain thousands of rows; avoid concurrent writes.
			// YouTube refuses a track the playlist already holds, so only the ones it accepted get
			// counted and drawn: an optimistic row for a refused add is a row that can never be
			// removed (no setVideoId behind it) until the app restarts.
			for (const song of songs) {
				if (epoch !== auth.epoch) break;
				try {
					const res = await api.addToPlaylist(pl.id, song.video_id);
					if (res.added) {
						added.push(song);
						rowIds.push(res.set_video_id);
					}
					confirmed.push(song);
				} catch (e) {
					failure = String(e);
					break;
				}
			}
			// A switched account owns different caches. Stop the batch and never patch those caches.
			if (epoch !== auth.epoch) {
				toast.error(t('selection.account_changed'));
				return;
			}
			const dupes = confirmed.length - added.length;
			// Every song, not just the accepted ones: a refusal means the playlist already holds it,
			// so its "saved" mark is right either way.
			noteSavedIn(pl.id, confirmed.map((s) => s.video_id));
			if (added.length) {
				bumpLibraryTrackCount(pl.id, added.length);
				notePlaylistAdd(pl.id, added);
				// `notePlaylistAdd` strips the setVideoId a song arrives with, which belongs to the
				// list it came from. The ones YouTube just answered belong to this playlist, so they
				// go back on: an open page of it can offer "Remove" on the new rows straight away.
				lastPlaylistAdd.songs = lastPlaylistAdd.songs.map((s, i) => ({
					...s,
					set_video_id: rowIds[i]
				}));
			}
			if (failure !== null) {
				toast.error(t('selection.playlist_partial', {
					added: added.length, playlist: pl.title, duplicates: dupes,
					remaining: songs.length - confirmed.length, error: failure
				}));
			} else if (!added.length) {
				toast(
					dupes > 1
						? t('toasts.already_in_all', { count: dupes, playlist: pl.title })
						: t('toasts.already_in', { playlist: pl.title })
				);
			} else if (dupes) {
				toast.success(
					t('toasts.added_to_playlist_dupes', { count: added.length, playlist: pl.title, dupes })
				);
			} else {
				toast.success(
					added.length > 1
						? t('toasts.added_songs', { count: added.length, playlist: pl.title })
						: t('toasts.added_one', { playlist: pl.title })
				);
			}
		} catch (e) {
			toast.error(String(e));
		} finally {
			ui.addPending = false;
		}
	}
</script>

<!-- The picker's state lives in `ui.addSongs`, so the dialog's own open flag is derived from it and
     every dismissal (Esc, backdrop, ✕) routes through `close()`. -->
<Dialog.Root bind:open={() => ui.addSongs !== null, (v) => !v && close()}>
	<!-- Flex, not the primitive's grid: the list has to shrink into the capped height and scroll. -->
	<Dialog.Content class="flex max-h-[32rem] flex-col gap-4 sm:max-w-sm">
		<Dialog.Header>
			<Dialog.Title>{t('player.add_to_playlist')}</Dialog.Title>
		</Dialog.Header>
		<div class="flex min-h-0 flex-1 flex-col">
			{#if playlists.length > 1}
				<input
					bind:this={box}
					bind:value={filter}
					placeholder={t('library.search_playlists')}
					onkeydown={(e) => e.key === 'Enter' && matches[0] && pick(matches[0])}
					class="mb-2 w-full rounded-lg bg-input px-3 py-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
				/>
			{/if}
			{#if loading}
				<p class="p-2 text-sm text-muted-foreground">{t('common.loading')}</p>
			{:else if matches.length}
				<div class="min-h-0 flex-1 overflow-y-auto">
					{#each matches as pl (pl.id)}
						<button
							class="flex w-full items-center gap-3 rounded-lg p-2 text-left hover:bg-accent/10"
							onclick={() => pick(pl)}
						>
							{#if pl.thumbnail}
								<img src={pl.thumbnail} alt="" class="h-10 w-10 rounded-md object-cover" />
							{:else}
								<div class="h-10 w-10 rounded-md bg-muted"></div>
							{/if}
							<div class="min-w-0">
								<div class="truncate text-base">{pl.title}</div>
								{#if pl.subtitle}
									<div class="truncate text-xs text-muted-foreground">{pl.subtitle}</div>
								{/if}
							</div>
						</button>
					{/each}
				</div>
			{:else}
				<p class="p-2 text-sm text-muted-foreground">
					{filter.trim() ? t('common.no_matches') : t('library.no_playlists_create')}
				</p>
			{/if}
		</div>
	</Dialog.Content>
</Dialog.Root>
