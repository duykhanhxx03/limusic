<script lang="ts">
	// "Share": the YTM link for a song/album/playlist/artist, with the artwork and title so you can
	// see what you're about to send. Opened from any ⋯ menu via `openShare`, mounted once in the
	// layout like AddToPlaylist.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { Copy01Icon, Tick02Icon, Alert02Icon } from '@hugeicons/core-free-icons';
	import * as api from '$lib/api';
	import type { BrowseItem, PlaylistPage } from '$lib/api';
	import { copyText } from '$lib/clipboard';
	import { getCached, invalidateCached, putCached } from '$lib/pagecache';
	import * as Dialog from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
	import { Switch } from '$lib/components/ui/switch';
	import { thumb } from '$lib/thumb';
	import { ui, toast } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	// Playlist browseIds carry a `VL` prefix that the watch/playlist URLs don't take.
	function shareUrl(item: BrowseItem): string {
		const id = item.id.replace(/^VL/, '');
		if (item.kind === 'song') return `https://music.youtube.com/watch?v=${id}`;
		if (item.kind === 'artist') return `https://music.youtube.com/channel/${id}`;
		// Albums only ever reach us as an `MPRE…` browseId, so link the browse page rather than the
		// `OLAK5uy_…` playlist URL YouTube's own share sheet hands out. Both resolve to the album.
		if (item.kind === 'album') return `https://music.youtube.com/browse/${id}`;
		return `https://music.youtube.com/playlist?list=${id}`;
	}

	// The dialog keeps rendering through its closing fade after `ui.share` is cleared, so the body
	// draws from the last target instead of going blank mid-animation. `last` is a plain variable
	// on purpose: it is only a memory for this derived, never something to react to.
	let last: BrowseItem | null = null;
	const shown = $derived((last = ui.share ?? last));

	const url = $derived(shown ? shareUrl(shown) : '');
	let copied = $state(false);

	// Left alone, the dialog focuses its first tabbable element on open, which is the read-only
	// link field: its select-on-focus would highlight the URL every time, and a focused input
	// swallows Space, so the play/pause shortcut would stop working while the dialog is up. Focus
	// goes to the panel itself instead, which keeps it inside the dialog for the focus trap.
	let content = $state<HTMLElement | null>(null);

	// A private playlist's link 404s for everyone else, so the modal has to say so before the link
	// is sent. Only playlists carry a privacy setting, and only YouTube's edit header reports it,
	// which means only playlists the signed-in user owns ever resolve to anything here.
	let privacy = $state<string | undefined>(undefined);
	let owned = $state(false);
	// Keeps the toggle on screen after it has been flipped public, so the flip can be undone.
	let wasPrivate = $state(false);
	// Liked Music reports as owned but has no editable privacy (same carve-out as the playlist page).
	const canToggle = $derived(owned && shown?.id.replace(/^VL/, '') !== 'LM');

	$effect(() => {
		const item = ui.share;
		// Closing leaves the last answer in place, so the warning and the switch don't vanish from
		// under the closing fade. The next open resets them in the same flush, before it paints.
		if (!item) return;
		privacy = undefined;
		owned = false;
		wasPrivate = false;
		if (item.kind !== 'playlist') return;
		const key = `playlist:${item.id}`;
		const apply = (p: PlaylistPage) => {
			// The modal may have been closed or retargeted while the fetch was in flight.
			if (ui.share?.id !== item.id) return;
			privacy = p.privacy;
			owned = p.owned;
			wasPrivate = p.privacy === 'PRIVATE';
		};
		const hit = getCached<PlaylistPage>(key);
		if (hit) return apply(hit);
		// No lighter endpoint exists: privacy rides along with the track list. The result is cached
		// under the same key the playlist page reads, so the fetch is not wasted.
		api
			.getPlaylist(item.id)
			.then((p) => {
				putCached(key, p);
				apply(p);
			})
			// A playlist we cannot read (signed out, someone else's) gets no warning: it is not
			// private, or it would not have been visible in the first place.
			.catch(() => {});
	});

	async function setPublic(next: boolean) {
		const item = ui.share;
		if (!item) return;
		const before = privacy;
		privacy = next ? 'PUBLIC' : 'PRIVATE';
		try {
			await api.editPlaylistDetails(item.id, { public: next });
			// ponytail: the playlist page keeps its own copy of `privacy` for the edit dialog, so drop
			// the cached page rather than plumbing a patch through. Worst case its Public switch reads
			// stale until the next visit, and a Save that doesn't touch it sends nothing.
			invalidateCached(`playlist:${item.id}`);
		} catch (e) {
			privacy = before;
			toast.error(String(e));
		}
	}

	function close() {
		ui.share = null;
		copied = false;
	}

	function copy() {
		copyText(url).then(
			() => {
				copied = true;
				setTimeout(() => (copied = false), 1500);
			},
			() => toast.error(t('toasts.could_not_copy_link'))
		);
	}
</script>

<!-- Driven by `ui.share` (set by `openShare`); every dismissal (Esc, backdrop, ✕) goes through
     `close()` so the copied tick resets with it. -->
<Dialog.Root bind:open={() => ui.share !== null, (v) => !v && close()}>
	<Dialog.Content
		class="sm:max-w-md"
		bind:ref={content}
		onOpenAutoFocus={(e) => {
			e.preventDefault();
			content?.focus();
		}}
	>
		<Dialog.Header>
			<Dialog.Title>{t('dialogs.share.title')}</Dialog.Title>
		</Dialog.Header>

		{#if shown}
			<div>
				<div class="flex items-center gap-4">
					{#if shown.thumbnail}
						<img
							src={thumb(shown.thumbnail, 400)}
							alt=""
							class="h-20 w-20 shrink-0 object-cover {shown.kind === 'artist'
								? 'rounded-full'
								: 'rounded-lg'}"
						/>
					{:else}
						<div class="h-20 w-20 shrink-0 rounded-lg bg-muted"></div>
					{/if}
					<div class="min-w-0">
						<div class="truncate text-base font-bold">{shown.title}</div>
						{#if shown.subtitle}
							<div class="truncate text-sm text-muted-foreground">{shown.subtitle}</div>
						{/if}
					</div>
				</div>

				<div class="mt-4 flex items-center gap-2 rounded-lg bg-muted/60 py-1 pl-3 pr-1">
					<input
						class="min-w-0 flex-1 bg-transparent py-1 text-sm text-muted-foreground outline-none"
						value={url}
						readonly
						onfocus={(e) => e.currentTarget.select()}
					/>
					<Button size="sm" onclick={copy} aria-label={t('dialogs.share.copy_link')}>
						<!-- icon swap via altIcon/showAlt: `icon` is frozen at mount -->
						<HugeiconsIcon icon={Copy01Icon} altIcon={Tick02Icon} showAlt={copied} class="h-4 w-4" />
						{copied ? t('common.done') : t('dialogs.share.copy_link')}
					</Button>
				</div>

				{#if privacy === 'PRIVATE'}
					<div class="mt-3 flex items-start gap-2 text-xs text-amber-600 dark:text-amber-500">
						<HugeiconsIcon icon={Alert02Icon} class="mt-px h-4 w-4 shrink-0" />
						<p>{t('dialogs.share.private_note')}</p>
					</div>
				{/if}

				{#if wasPrivate && canToggle}
					<div class="mt-3 flex items-center justify-between gap-4 rounded-lg bg-muted px-3 py-2.5">
						<div class="min-w-0">
							<div class="text-sm font-bold">{t('common.public')}</div>
							<p class="text-xs text-muted-foreground">
								{privacy === 'PUBLIC'
									? t('dialogs.share.public_link_on')
									: t('dialogs.share.public_link_off')}
							</p>
						</div>
						<Switch
							checked={privacy === 'PUBLIC'}
							onCheckedChange={setPublic}
							aria-label={t('a11y.public_playlist')}
						/>
					</div>
				{/if}
			</div>
		{/if}
	</Dialog.Content>
</Dialog.Root>
