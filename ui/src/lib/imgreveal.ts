// Fade a cover in when it decodes, instead of letting it pop onto its placeholder.
//
// Every cover in the app is `loading="lazy"`, so the browser paints nothing until the bytes are
// there and then swaps the whole image in on one frame. One card doing that is a blink; a shelf
// or a grid doing it is a dozen blinks at a dozen different moments, which is what reads as the
// images "not arriving smoothly". Fading each one from its `bg-muted` box spreads that swap over
// a few frames and lets the scattered arrivals blend instead of snapping.
//
// The element keeps its own `transition-*` classes — this only decides *when* opacity is 0, so an
// image that already has a hover transform keeps it (list `opacity` in that element's transition
// property, e.g. `transition-[transform,opacity]`).

/**
 * Attachment for an `<img>`: hold it transparent until it loads.
 *
 * A cached image is already `complete` at mount, and fading that in would add a blink where the
 * browser had none — so those are left alone and appear immediately, which is the whole point of
 * having them cached.
 */
export function imgReveal(node: HTMLImageElement) {
	if (node.complete && node.naturalWidth > 0) return;

	node.dataset.revealing = '';
	const done = () => delete node.dataset.revealing;
	// `error` too: a broken cover falls back to an icon or a retried src, and neither should be
	// stuck behind an opacity that never clears.
	node.addEventListener('load', done, { once: true });
	node.addEventListener('error', done, { once: true });

	return () => {
		node.removeEventListener('load', done);
		node.removeEventListener('error', done);
	};
}
