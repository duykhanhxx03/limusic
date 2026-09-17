/**
 * What the next track will need on screen, fetched and decoded before it starts.
 *
 * A track change used to be paid for in the frames after it: measured on 2026-09-17 with the
 * player view on its lyrics tab, the cover stayed blank for up to 880 ms while its image loaded,
 * and the lyrics were a skeleton for 450–1300 ms while they were fetched. Both are knowable a
 * whole track in advance — the queue says what comes next — so this does that work during the
 * track before, and the swap has nothing left to wait for.
 *
 * Two shared caches come out of it, and the views read the same ones, so a warm entry is simply
 * what they find: decoded images (by URL) and lyrics (by video id).
 */
import * as api from './api';
import { artworkLadder, isMissingThumb, thumb } from './thumb';

// --- images -----------------------------------------------------------------------------------

/** Decoded images, oldest first. Holding the element keeps its decoded pixels alive in WebKit's
 *  memory cache, which is what lets an `<img>` with the same URL paint on its first frame. */
const images = new Map<string, { img: HTMLImageElement; ok: Promise<boolean> }>();
const IMAGE_LIMIT = 12;

/** Load and decode `url`. False when it fails, or when it is YouTube's grey stand-in for a
 *  thumbnail rendition the video does not have (see `isMissingThumb`). */
export function preloadImage(url: string): Promise<boolean> {
	const hit = images.get(url);
	if (hit) return hit.ok;
	const img = new Image();
	img.decoding = 'async';
	img.src = url;
	const ok = img
		.decode()
		.then(() => !isMissingThumb(img))
		.catch(() => false);
	images.set(url, { img, ok });
	if (images.size > IMAGE_LIMIT) images.delete(images.keys().next().value!);
	return ok;
}

const artwork = new Map<string, Promise<string | null>>();

/** The first rung of an artwork ladder that loads, decoded and ready to paint; null if none does. */
export function resolveArtwork(ladder: (string | undefined)[]): Promise<string | null> {
	const key = ladder.join('\n');
	const hit = artwork.get(key);
	if (hit) return hit;
	const p = (async () => {
		for (const url of ladder) if (url && (await preloadImage(url))) return url;
		return null;
	})();
	artwork.set(key, p);
	if (artwork.size > IMAGE_LIMIT) artwork.delete(artwork.keys().next().value!);
	return p;
}

// --- lyrics -----------------------------------------------------------------------------------

export type LyricsQuery = {
	videoId: string;
	title: string;
	artists: string;
	album?: string;
	/** The track's own length in seconds. */
	duration?: number;
};

const lyricsPending = new Map<string, Promise<api.Lyrics | null>>();
const lyricsSettled = new Map<string, api.Lyrics | null>();
const LYRICS_LIMIT = 16;

/** Lyrics for a track, fetched at most once per session unless forgotten. Rejections are not
 *  cached: the next ask tries again. */
export function lyricsFor(q: LyricsQuery): Promise<api.Lyrics | null> {
	const hit = lyricsPending.get(q.videoId);
	if (hit) return hit;
	const p = api.getLyrics(q).then(
		(l) => {
			lyricsSettled.set(q.videoId, l);
			if (lyricsSettled.size > LYRICS_LIMIT) lyricsSettled.delete(lyricsSettled.keys().next().value!);
			return l;
		},
		(e) => {
			lyricsPending.delete(q.videoId);
			throw e;
		}
	);
	lyricsPending.set(q.videoId, p);
	if (lyricsPending.size > LYRICS_LIMIT) lyricsPending.delete(lyricsPending.keys().next().value!);
	return p;
}

/** The lyrics already in hand for a track: `undefined` if they are not (yet), `null` if the track
 *  has none. Lets a view skip its loading state entirely on a warm track. */
export function peekLyrics(videoId: string): api.Lyrics | null | undefined {
	return lyricsSettled.get(videoId);
}

/** Drop everything cached here, e.g. when the lyrics provider setting changes what the answer is. */
export function forgetLyrics() {
	lyricsPending.clear();
	lyricsSettled.clear();
}

/** How many lyrics views are on screen. Lyrics are only worth prefetching while someone is reading
 *  them: a track change with no lyrics view open would spend a lookup nobody sees. */
let lyricsViews = 0;
export function watchingLyrics(): () => void {
	lyricsViews++;
	return () => {
		lyricsViews--;
	};
}

// --- the next track ---------------------------------------------------------------------------

/** `"3:21"` / `"1:02:03"` to seconds. */
export function durationSecs(d?: string): number | undefined {
	if (!d) return undefined;
	const parts = d.split(':').map(Number);
	if (!parts.length || parts.some(Number.isNaN)) return undefined;
	return parts.reduce((a, b) => a * 60 + b, 0);
}

/**
 * Warm what `next` will show. The player bar's small thumbnail always (it is always on screen and
 * costs a few KB); the big artwork only while a view that shows it is open, since that one is a
 * 720–1280 px image; lyrics only while a lyrics view is.
 */
export function warmNext(next: api.SongItem | undefined, artworkOnScreen: boolean) {
	if (!next || api.isLocalId(next.video_id)) return;
	const small = thumb(next.thumbnail, 120);
	if (small) void preloadImage(small);
	if (artworkOnScreen && next.thumbnail) void resolveArtwork(artworkLadder(next.thumbnail));
	if (lyricsViews > 0) {
		lyricsFor({
			videoId: next.video_id,
			title: next.title,
			artists: next.artists,
			album: next.album,
			duration: durationSecs(next.duration)
		}).catch(() => {});
	}
}
