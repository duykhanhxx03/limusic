// What a BrowseItem does when you click it. One implementation for every surface that renders a
// browse result — cards, the home recents rail, anything after them — so "open" and "play whole
// thing" can never drift apart between two components.
import { goto } from '$app/navigation';
import * as api from './api';
import type { BrowseItem, SearchResults, SongItem } from './api';
import { getCached, putCached } from './pagecache';
import { t } from './i18n.svelte';
import { enqueue, playFrom, playSong, toast, touchPick } from './player.svelte';

/**
 * A song card carries everything a queue entry needs; the ⋯ menus take this shape. The one mapping
 * for every card surface (search rows, home shelves, carousels): a card's `subtitle` is already the
 * artist alone for songs, and that string is what the player bar and the OS media widget show, so
 * a second copy of this that drifts shows the wrong artist.
 */
export const asSong = (i: BrowseItem): SongItem => ({
	video_id: i.id,
	title: i.title,
	artists: i.subtitle ?? '',
	// The links belong to the same line as `artists`; without them a card played from search or a
	// home shelf reaches the player bar (and the row menu) with an artist you can't click.
	artist_runs: i.artistRuns,
	artist_id: i.artistRuns?.find((r) => r.id)?.id,
	duration: i.duration,
	play_count: i.playCount,
	thumbnail: i.thumbnail,
	explicit: i.explicit,
	// Without this a card played from a shelf reaches the orchestrator as an ordinary track and
	// gets the anonymous fallback chain, which can never stream an upload.
	is_upload: i.isUpload
});

/** Where a non-song item lives. Songs have no page — they play. */
export const hrefFor = (i: BrowseItem): string =>
	// A local artist is drawn as an artist (circle, no play button) but opens the album route:
	// there is no channel behind files on disk, so the real artist page has nothing to show.
	i.id.startsWith(api.LOCAL_ARTIST_PREFIX)
		? `/album/${encodeURIComponent(i.id)}`
		: i.kind === 'artist'
			? `/artist/${encodeURIComponent(i.id)}`
			: i.kind === 'album'
				? `/album/${encodeURIComponent(i.id)}`
				: `/playlist/${encodeURIComponent(i.id)}`;

/**
 * Where a shelf's "See all" goes. Most point at a browse grid (`FE…`), which the /list page
 * renders as a wall of cards; the charts' Trending row points at an actual playlist instead, and
 * sending that through /list would flatten a 100-track chart into unplayable cards.
 */
export function moreHref(section: {
	title: string;
	moreBrowseId?: string;
	moreParams?: string;
}): string {
	const id = section.moreBrowseId ?? '';
	if (id.startsWith('FE')) {
		const q = new URLSearchParams({ id, title: section.title });
		if (section.moreParams) q.set('params', section.moreParams);
		return `/list?${q.toString()}`;
	}
	const kind = /^(VL)?MPRE/.test(id) ? 'album' : id.startsWith('UC') ? 'artist' : 'playlist';
	return hrefFor({ kind, id, title: section.title } as BrowseItem);
}

/** Primary click: a song plays, everything else opens its page. */
export function openItem(item: BrowseItem): void {
	// A click counts as "used" for Shortcuts eviction wherever the item was rendered. No-op unless
	// this item is actually on the grid.
	touchPick(item.id);
	if (item.kind === 'song') playSong(asSong(item));
	else goto(hrefFor(item));
}

/**
 * Play the whole thing without opening it. A song is immediate; a playlist/album has to be fetched
 * first, so callers own the in-flight state (the pulsing button). `shuffle` starts it shuffled
 * (the card menu's shuffle button). Never throws — a failure toasts and the user stays where they
 * were.
 */
export async function playItem(item: BrowseItem, shuffle = false): Promise<void> {
	touchPick(item.id);
	if (item.kind === 'song') {
		playSong(asSong(item));
		return;
	}
	try {
		if (item.kind === 'album') {
			const album = await api.getAlbum(item.id);
			await playFrom(item, album.items, null, album.playlistId ?? undefined, shuffle);
		} else {
			const pl = await api.getPlaylist(item.id);
			// `sourceId` seeds autoplay off that playlist's radio. On Repeat is local, so there is
			// no radio for it. Pass none and let autoplay seed off the last video instead. The
			// continuation hands the rest of the playlist (past this first page) to the backend.
			await playFrom(
				item,
				pl.items,
				null,
				item.id === api.ON_REPEAT_ID ? undefined : item.id,
				shuffle,
				pl.continuation
			);
		}
	} catch {
		toast.error(t('toasts.could_not_play'));
	}
}

/**
 * Queue the whole thing without opening it: "Play next" (`next`) or "Add to queue". A song is one
 * track; an album/playlist is fetched first, so callers own the in-flight state (same contract as
 * `playItem`). Never throws — a failure toasts.
 *
 * The playlist's next-page token rides along: the backend walks the rest of a long playlist into
 * the queue in the background rather than the UI chaining pages before anything is queued.
 */
export async function enqueueItem(item: BrowseItem, next: boolean): Promise<void> {
	if (item.kind === 'song') {
		await enqueue([asSong(item)], next);
		return;
	}
	try {
		if (item.kind === 'album') {
			const album = await api.getAlbum(item.id);
			await enqueue(album.items, next, album.title ?? item.title, album.continuation);
		} else {
			const pl = await api.getPlaylist(item.id);
			await enqueue(pl.items, next, pl.title ?? item.title, pl.continuation);
		}
	} catch {
		toast.error(t('toasts.could_not_queue'));
	}
}

/**
 * What the page cache holds under `searchKey(q)`: the unfiltered results, plus the songs-filtered
 * rows the search page's Songs shelf is drawn from. Both the typeahead and the search page write
 * this key, so both write this one shape: a reader handed any other shape draws nothing (the search
 * page would sit on its "search for…" prompt, the typeahead would come back empty). `songs` is
 * empty when only the typeahead has searched, and the shelf then falls back to `res.songs`.
 */
export type CachedSearch = { res: SearchResults; songs: SongItem[] };

export const searchKey = (q: string) => `search:${q}`;

// `search_all` requests still out, by query. The typeahead fires once typing pauses and Enter
// usually lands while that request is still in flight; sharing it means the search page waits on
// the preview's answer instead of sending the same search a second time.
const inFlight = new Map<string, Promise<SearchResults>>();

/** `api.searchAll`, with one request shared by every caller asking for the same query at once. */
export function sharedSearchAll(q: string): Promise<SearchResults> {
	let p = inFlight.get(q);
	if (!p) {
		p = api.searchAll(q).finally(() => inFlight.delete(q));
		inFlight.set(q, p);
	}
	return p;
}

/**
 * The handful of rows a typeahead shows for a query: one top hit, then a spread across the
 * categories rather than six songs. Cache-first, and it fills the same `searchKey(q)` entry the
 * search page reads, so running a previewed query paints its results at once instead of a blank
 * page. Shared by the search field (SearchSuggest) and the Ctrl+K palette, which is what keeps the
 * two showing the same rows.
 */
export async function searchPreview(q: string): Promise<BrowseItem[]> {
	const key = searchKey(q);
	let res = getCached<CachedSearch>(key)?.res;
	if (!res) {
		res = await sharedSearchAll(q);
		// Only into an empty slot: the search page may have stored the full entry while this was in
		// flight, and replacing it would throw away the filtered songs it fetched.
		if (!getCached(key)) putCached(key, { res, songs: [] } satisfies CachedSearch);
	}
	const out: BrowseItem[] = [];
	const seen = new Set<string>();
	const take = (from: BrowseItem[], n: number) => {
		for (const i of from) {
			if (n <= 0) break;
			if (seen.has(i.id)) continue;
			seen.add(i.id);
			out.push(i);
			n--;
		}
	};
	take(res.top, 1);
	take(res.songs, 3);
	take(res.artists, 1);
	take(res.albums, 1);
	take(res.playlists, 1);
	return out;
}
