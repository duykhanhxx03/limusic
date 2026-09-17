import { convertFileSrc } from '@tauri-apps/api/core';

// Rewrite a Google image URL to (about) the pixel size a slot actually renders, so WebKitGTK
// doesn't decode a 544px (or 1080p) image for a 40px row. Only lh3/yt3 googleusercontent-style
// URLs carry the size in the URL (`=w544-h544` / `=s576` suffixes); anything else (notably
// i.ytimg.com path-variant thumbs, where other sizes can 404) is returned unchanged.
export function thumb(url: string | undefined | null, px: number): string | undefined {
	if (!url) return undefined;
	// Local library artwork is a path on this machine, not a URL. The webview can't open a bare
	// path, so hand it through Tauri's asset protocol. Kept here rather than at the command
	// boundary so what gets stored (queue, Shortcuts) stays the real path — which is also what
	// MPRIS needs.
	if (url.startsWith('/') || /^[A-Za-z]:[\\/]/.test(url)) return convertFileSrc(url);
	if (/=w\d+-h\d+/.test(url)) return url.replace(/=w\d+-h\d+/, `=w${px}-h${px}`);
	if (/=s\d+/.test(url)) return url.replace(/=s\d+/, `=s${px}`);
	return url;
}

/**
 * The pixel size to ask the CDN for so a box `cssPx` wide is sharp on *this* display.
 *
 * Every artwork request used to be a fixed number, which is right at 1x and soft everywhere else:
 * a 720px cover in a 700px box looks correct on a 1x screen and visibly mushy on a 2x one, where
 * the same box is 1400 device pixels. The player view and theater mode are the two places big
 * enough for that to show.
 *
 * Capped at 1200: beyond it Google generally hands back the original rather than a larger render,
 * so asking for more costs bytes and decode for no extra detail. Rounded to a step of 80 so a
 * window drag does not mint a new URL per pixel and throw away the cache on every frame.
 */
export function thumbPx(cssPx: number, max = 1200): number {
	const dpr = typeof window === 'undefined' ? 1 : Math.min(window.devicePixelRatio || 1, 3);
	const want = Math.min(max, Math.max(120, Math.round(cssPx * dpr)));
	return Math.ceil(want / 80) * 80;
}

/** A video thumbnail on YouTube's image CDN: `i.ytimg.com/vi/<id>/<rendition>.jpg`, or `vi_webp`. */
const YTIMG = /^https:\/\/i\d?\.ytimg\.com\/vi(?:_webp)?\/([\w-]{11})\//;

/**
 * Artwork for the views that show it big (the player view, theater mode), largest first. Each view
 * steps down the list when a size fails.
 *
 * A Google image URL is resized by rewriting its size spec, which `thumb` does. A video thumbnail on
 * i.ytimg.com cannot be: YouTube keeps a fixed set of renditions per video, and the one a response
 * carries is typically `hqdefault.jpg?sqp=…`, a signed 400×225 crop. Squared and stretched over a
 * 600px cover that is 225 real pixels, which is why music videos looked smeared next to albums.
 * Any HD upload also has `maxresdefault` and `hq720`, both 1280×720 and 16:9 (SimpMusic asks for
 * `maxresdefault` the same way). `sddefault` and `hqdefault` are skipped on purpose: they are 4:3
 * with the video letterboxed inside, so a square crop of them shows black bars. The URL the
 * response carried stays last, since it always exists.
 */
export function artworkLadder(url: string | undefined | null): (string | undefined)[] {
	const id = url?.match(YTIMG)?.[1];
	if (url && id) {
		return [
			`https://i.ytimg.com/vi/${id}/maxresdefault.jpg`,
			`https://i.ytimg.com/vi/${id}/hq720.jpg`,
			url
		];
	}
	// The top size follows the display: 720 into a 2x box is soft. The two below it stay fixed, as
	// fallbacks for a CDN that will not serve the first size.
	return [thumbPx(720), 400, 120].map((px) => thumb(url, px));
}

/**
 * Whether an `<img>` from `artworkLadder` got YouTube's stand-in instead of the rendition. A video
 * without an HD upload answers `maxresdefault`/`hq720` with a 404 whose body is a 120×90 grey
 * placeholder, and WebKitGTK decodes that and fires `load`, not `error` (checked 2026-09-17 on
 * `jNQXAC9IVRw`). So a missing size is recognised by what arrived.
 */
export function isMissingThumb(img: EventTarget): boolean {
	return (
		img instanceof HTMLImageElement &&
		img.naturalWidth <= 120 &&
		/\/(maxresdefault|hq720)\.jpg$/.test(img.currentSrc || img.src)
	);
}
