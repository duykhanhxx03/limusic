<script lang="ts">
	// A panel's draggable edge: the library's right edge, or (`edge="left"`) the right column's
	// left one.
	//
	// Pointer events with capture, not mouse events: capture means the drag keeps receiving moves
	// after the pointer leaves the 4px strip, which at drag speed it does immediately. Without it a
	// quick drag stops the moment the cursor outruns the handle.
	//
	// Writes go straight to the store (which clamps and persists); nothing is buffered until
	// release, so the sidebar tracks the pointer rather than jumping at the end.
	import { setRightPanelWidth, setSidebarWidth, ui } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	let { edge = 'right' }: { edge?: 'left' | 'right' } = $props();
	const left = $derived(edge === 'left');
	const width = $derived(left ? ui.rightPanelWidth : ui.sidebarWidth);
	const setWidth = (px: number) => (left ? setRightPanelWidth(px) : setSidebarWidth(px));

	let dragging = $state(false);

	function down(e: PointerEvent) {
		// Left button only: a right-click here belongs to the context menu, not to a resize.
		if (e.button !== 0) return;
		e.preventDefault();
		dragging = true;
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
	}

	function move(e: PointerEvent) {
		if (!dragging) return;
		// Measured from the panel's fixed edge, not from a delta: a delta accumulates rounding over a
		// long drag and drifts away from the pointer.
		const panel = (e.currentTarget as HTMLElement).parentElement;
		if (!panel) return;
		const box = panel.getBoundingClientRect();
		setWidth(left ? box.right - e.clientX : e.clientX - box.left);
	}

	function up(e: PointerEvent) {
		dragging = false;
		(e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
	}

	/** Keyboard: the handle is focusable, so the width is reachable without a pointer. */
	function key(e: KeyboardEvent) {
		const step = e.shiftKey ? 32 : 8;
		// Arrows move the edge: on the right column's left edge, left makes it wider.
		const grow = left ? 'ArrowLeft' : 'ArrowRight';
		const shrink = left ? 'ArrowRight' : 'ArrowLeft';
		if (e.key === grow) setWidth(width + step);
		else if (e.key === shrink) setWidth(width - step);
		else return;
		e.preventDefault();
	}
</script>

<!-- Hidden below `lg` and while collapsed: at those widths the rail is fixed (see layout.css), so a
     handle would drag a number nothing reads. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<!-- Both silenced on purpose. A focusable `separator` with `aria-valuenow` is the WAI-ARIA window
     splitter pattern — it *is* an interactive widget, and screen readers announce it as one. The
     rule only knows the static separator, so it flags the correct markup. Changing the role to
     something the linter likes (`button`, `slider`) would describe this worse, not better. -->
<div
	class="resizer absolute inset-y-0 z-20 w-2 cursor-col-resize {left ? '-left-1' : '-right-1'}"
	role="separator"
	aria-orientation="vertical"
	aria-label={t('a11y.resize_sidebar')}
	aria-valuenow={width}
	tabindex="0"
	onpointerdown={down}
	onpointermove={move}
	onpointerup={up}
	onpointercancel={up}
	onkeydown={key}
>
	<!-- The visible line is one pixel in the middle of a wider hit area: a 1px target is unhittable,
	     and a 2px-wide visible bar would read as a border the app does not otherwise have. -->
	<div
		class="mx-auto h-full w-px transition-colors {dragging
			? 'bg-primary'
			: 'bg-transparent hover:bg-foreground/20'}"
	></div>
</div>

<style>
	/* The breakpoint lives here rather than as a `lg:` utility, and matches the one in layout.css
	   that decides `--sidebar-w`. Two reasons: the handle must appear exactly when the width stops
	   being fixed, which is one fact that should not be spelled two ways; and a `lg:` variant on a
	   newly added file is only in the stylesheet once Tailwind has rescanned for it — during this
	   component's first run it had not, so `hidden` won and the handle was in the DOM at
	   `display:none`, which looks exactly like a drag that does nothing. */
	.resizer {
		display: none;
	}
	@media (min-width: 64rem) {
		.resizer {
			display: block;
		}
	}
</style>
