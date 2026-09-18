// Offline downloads as the UI sees them: which tracks are saved, and how far the running one has
// got. Rust owns the files and the index; this mirrors just enough to draw a button.
import * as api from './api';
import { toast } from './player.svelte';
import { t } from './i18n.svelte';

export const dl = $state({
	/** videoIds that are fully saved. */
	saved: new Set<string>(),
	/** videoId → bytes so far and the total, for whatever is downloading. Empty when nothing is.
	 *  Both numbers, not a ready-made fraction: `total` is 0 until the first ranged response comes
	 *  back with a Content-Range, and a caller has to be able to tell "0 of unknown" from "0%". */
	progress: {} as Record<string, { received: number; total: number }>,
	/** Bytes on disk, for the settings row. */
	bytes: 0,
	/** Bumped when a download finishes or is removed, and at no other time. It is what the
	 *  Downloads tab reloads on. `saved` would be the wrong signal: it also changes whenever any list
	 *  in the app finds out that some of its rows were already on disk, and the tab's own rows do
	 *  exactly that the first time it opens. */
	version: 0
});

export const isSaved = (videoId: string) => dl.saved.has(videoId);
export const isDownloading = (videoId: string) => videoId in dl.progress;

/** How far through one track, 0..1. Zero while the total is still unknown — that is a fraction of
 *  one track out of a whole list, so treating it as "no progress yet" is invisible, where flipping
 *  the whole ring into a spinner for it was not. */
export function trackFraction(videoId: string): number {
	const p = dl.progress[videoId];
	if (!p || p.total <= 0) return 0;
	return Math.min(1, p.received / p.total);
}

/** Already asked about, so a row that is genuinely not downloaded does not re-ask on every
 *  re-render. An id leaves it when its download finishes or is removed, the only two ways its
 *  answer can change. Shared by the per-row batch and the whole-list `noteSaved` calls, so a page
 *  whose Download button has just asked about every track does not have its rows ask again. */
let asked = new Set<string>();

/** Ask Rust which of these are saved and fold the answer in. Cheap enough to call per page. */
export async function noteSaved(videoIds: string[]) {
	const unknown = videoIds.filter((id) => id && !dl.saved.has(id) && !asked.has(id));
	if (!unknown.length) return;
	// Marked before the answer, not after: rows that mount while it is in flight would otherwise
	// send the same question again.
	for (const id of unknown) asked.add(id);
	try {
		const have = await api.downloadedIds(unknown);
		// Reassigned rather than mutated: a `Set` is not deeply reactive, so `add` alone would not
		// re-render the buttons that read it. Only when something was found, though. Every row in
		// the app reads `dl.saved`, so a reassignment re-runs all of them, and most answers are
		// "none of these", which changes nothing.
		if (have.length) dl.saved = new Set([...dl.saved, ...have]);
	} catch {
		// Offline or mid-restart: the buttons just show "not saved", which is recoverable. Not
		// remembered as asked, so the next time these ids come up they are asked about again.
		for (const id of unknown) asked.delete(id);
	}
}

// Rows ask about themselves, one at a time, from every list in the app. Batched so a hundred of
// them is one command rather than a hundred: the frame that mounts a playlist would otherwise fire
// a hundred round trips, and the answer for all of them is one query.
let wanted = new Set<string>();
let batch: ReturnType<typeof setTimeout> | undefined;

/** Ask whether this track is saved. Cheap to call from anywhere, including per row. */
export function requestSaved(videoId: string) {
	if (!videoId || dl.saved.has(videoId) || asked.has(videoId)) return;
	wanted.add(videoId);
	clearTimeout(batch);
	batch = setTimeout(() => {
		const ids = [...wanted];
		wanted = new Set();
		// `noteSaved` marks them as asked before it awaits anything, so nothing can slip in twice.
		noteSaved(ids);
	}, 50);
}

export function download(items: api.SongItem[]) {
	const pending = items.filter((i) => !dl.saved.has(i.video_id));
	if (!pending.length) {
		// Every track was already saved. Silence here reads as a dead button, and "downloading…"
		// would be a lie.
		if (items.length) toast.success(t('downloads.already_all'));
		return;
	}
	// Shown as starting straight away: the first byte can be a couple of seconds off (the stream
	// has to be resolved first) and a button that does nothing for that long reads as broken.
	for (const i of pending) dl.progress[i.video_id] = { received: 0, total: 0 };
	// An album's worth of tracks downloads one at a time and the only per-track feedback is inside
	// a menu nobody is looking at, so say how many were taken on.
	if (pending.length > 1) toast.success(t('downloads.queued', { count: pending.length }));
	api.downloadTracks(pending).catch((e) => {
		for (const i of pending) delete dl.progress[i.video_id];
		toast.error(String(e));
	});
}

export function cancel() {
	// Cleared here rather than waiting for the event: the click has to feel like it landed, and the
	// event confirms it a moment later anyway.
	dl.progress = {};
	api.cancelDownloads().catch(() => {});
}

export async function remove(videoId: string) {
	await api.removeDownload(videoId);
	// Only when it was known to be saved: Settings' Remove all comes through here once per
	// download, and a copy of the set per call re-runs every row in the app each time.
	if (dl.saved.has(videoId)) dl.saved = new Set([...dl.saved].filter((id) => id !== videoId));
	// So a row re-asks and loses its marker rather than keeping a stale yes.
	asked.delete(videoId);
	dl.version++;
	refreshSize();
}

export function refreshSize() {
	api.downloadsSize()
		.then((b) => (dl.bytes = b))
		.catch(() => {});
}

/** One subscription per window, from initApp. */
export function initDownloads(): Promise<() => void> {
	refreshSize();
	const unsubs: Promise<() => void>[] = [
		api.onDownloadProgress((p) => {
			if (p.done) {
				delete dl.progress[p.videoId];
				dl.saved = new Set([...dl.saved, p.videoId]);
				asked.delete(p.videoId);
				dl.version++;
				refreshSize();
				return;
			}
			dl.progress[p.videoId] = { received: p.received, total: p.total };
		}),
		api.onDownloadFailed((p) => {
			delete dl.progress[p.videoId];
		}),
		// Cancelled, or simply finished: either way nothing is running, so the queued-but-never-
		// started entries have to go or every button stays stuck showing a ring.
		api.onDownloadsCancelled(() => (dl.progress = {})),
		api.onDownloadsIdle(() => (dl.progress = {}))
	];
	return Promise.all(unsubs).then((fns) => () => fns.forEach((f) => f()));
}

/** "4.2 MB" — for the settings row and the downloads list. */
export function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	const units = ['KB', 'MB', 'GB'];
	let v = n / 1024;
	let i = 0;
	while (v >= 1024 && i < units.length - 1) {
		v /= 1024;
		i++;
	}
	return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}
