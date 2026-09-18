<script module lang="ts">
	import { ON_REPEAT_ID } from '$lib/api';
	import type { BrowseItem } from '$lib/api';
	import { thumb } from '$lib/thumb';

	/**
	 * The cover a tile draws, at the size it loads it, or nothing (On Repeat draws an icon). Home's
	 * header reads its colour from this same URL, so a hover costs no request the tile didn't make.
	 */
	export function tileArt(item: BrowseItem): string | undefined {
		return item.id === ON_REPEAT_ID ? undefined : thumb(item.thumbnail, 240);
	}
</script>

<script lang="ts">
	// The home grid the user curates (was "Quick Picks" — renamed: YouTube Music has a shelf by that
	// name and it isn't this one). It holds what was put in it, in the order it was dragged into, plus
	// On Repeat once that has enough songs (the only tile the app suggests, and removing it is
	// permanent). Unlike before it renders even when empty — a section that hides itself is a section
	// nobody discovers. Logic in $lib/personal.ts.
	//
	// Not square cards: these were 5.5rem tiles and every label came out as "アプソリュ…" over a
	// subtitle that was "Simo Hypers •…" fifteen times. A shortcut is a thing you already know, so
	// what it owes you is its *name* at a size you can read, not another piece of cover art competing
	// with the shelves below. Hence wide tiles with the art flush to the leading edge, four to a row.
	//
	// Spotify's quick-access grid is the model for how it reads: no heading, eight tiles over the
	// colour at the top of the page. Fewer than eight shortcuts and the grid is topped up with what
	// was played recently (`fill`), which the caller then leaves out of "Jump back in" below.
	//
	// ponytail: drag is the only way to reorder (no keyboard equivalent). Add/remove/open all work
	// from the keyboard; wire arrow-key moves onto the tiles if anyone actually needs it.
	import { onDestroy } from 'svelte';
	import { flip } from 'svelte/animate';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		Add01Icon,
		PlayIcon,
		MusicNote01Icon,
		UserIcon,
		ListRestartIcon
	} from '@hugeicons/core-free-icons';
	import ShortcutPicker from './ShortcutPicker.svelte';
	import { openItem, playItem } from '$lib/browse';
	import { library, personal, placePick } from '$lib/player.svelte';
	import { freshen, MAX_PICKS } from '$lib/personal';
	import { getDragItem, isDragItem, setDragItem } from '$lib/dnd';
	import { warmAccent } from '$lib/artcolor';
	import { t } from '$lib/i18n.svelte';
	import ItemMenu from './ItemMenu.svelte';

	// `picking` opens the "add a shortcut" picker; the button for it lives in home's greeting row,
	// with Edit home, now that the grid has no heading to carry them. `onhover` is told which tile
	// is being looked at (`null` once none is), so the colour at the top of home can follow it.
	let {
		fill = [],
		picking = $bindable(false),
		onhover
	}: {
		fill?: BrowseItem[];
		picking?: boolean;
		onhover?: (item: BrowseItem | null) => void;
	} = $props();

	// Tiles are stored as a snapshot of the card, so a playlist that has gained tracks since it was
	// pinned would keep showing the old count; `freshen` overlays the live library row (#67).
	const picks = $derived(personal.picks.map((p) => freshen(p, library.items)));
	// Where a drop would land: the id of the tile it goes in front of, `null` for the end of the grid,
	// `undefined` when no drag of ours is over the section at all.
	let before = $state<string | null | undefined>(undefined);
	let busy = $state<string | null>(null); // id of the tile whose fetch-then-play is in flight
	// Google's CDN 404s some rewritten sizes; a dead thumb degrades to a placeholder icon rather than
	// the browser's broken-image glyph. Keyed by URL, not by tile id: a tile's stored cover is a
	// snapshot that can have expired since it was pinned, and `freshen` swaps in the live library's
	// URL a moment after startup. Keyed by id, that first failure stuck to the tile and the new URL
	// was never tried, so the shortcut sat blank until navigating away and back remounted this (#138).
	let failed = $state<Record<string, boolean>>({});

	// --- the header's colour follows the tile being looked at (HomeFeed turns it into a colour) ---
	// Crossing the 0.5rem gap between two tiles is a leave and then an enter, so a leave waits a
	// moment before it counts: reported at once, the header set off back to its own colour and
	// turned round again on every hop across the grid.
	const LEAVE_GRACE = 120;
	let leaving: ReturnType<typeof setTimeout> | undefined;
	// Plain, not $state: nothing renders from it. A drag picks a tile up to move it, which is not
	// looking at it, so the header lets go at the start of one and ignores the grid until it ends.
	let dragging = false;

	function hoverOn(item: BrowseItem) {
		if (!onhover || picking || dragging) return;
		clearTimeout(leaving);
		onhover(item);
	}
	function hoverOff() {
		if (!onhover) return;
		clearTimeout(leaving);
		leaving = setTimeout(() => onhover?.(null), LEAVE_GRACE);
	}
	/** Let go now, without the grace a leave gets. */
	function unhover() {
		clearTimeout(leaving);
		onhover?.(null);
	}
	/** Pointer and keyboard focus alike: Tab through the grid and the header follows it too. On the
	 *  tile's wrapper rather than the tile, so its ⋯ button (a sibling of the tile) still counts.
	 *  Either is also proof that a drag is over, since neither a pointer crossing a tile with no
	 *  button held nor focus arriving from the keyboard can happen while one is live. The drag's own
	 *  end is not enough to go on: a `dragend` WebKitGTK swallows, or one from a tile unmounted
	 *  mid-drag, never arrives (`$lib/dnd.ts`), and home stays mounted, so one lost event left the
	 *  header deaf to the grid for the rest of the session. */
	const hoverable = (item: BrowseItem) => ({
		onpointerenter: (e: PointerEvent) => {
			if (e.buttons === 0) dragging = false;
			hoverOn(item);
		},
		onpointerleave: hoverOff,
		onfocusin: () => {
			dragging = false;
			hoverOn(item);
		},
		onfocusout: hoverOff
	});
	$effect(() => {
		if (picking) unhover();
	});
	// The grid goes when a mood chip replaces the feed, and a colour it put up goes with it.
	onDestroy(unhover);
	// Decode every tile's cover while nothing is happening, so a hover has its colour on the frame
	// it lands instead of a fetch and a decode later. Here rather than in HomeFeed because this is
	// what knows which covers are on the grid, after `freshen` has swapped in the live ones.
	$effect(() => {
		if (!onhover) return;
		for (const item of [...picks, ...fill]) {
			const art = tileArt(item);
			if (art) warmAccent(art);
		}
	});

	// One handler for the whole section: the tile under the cursor carries its id in `data-pick`, so
	// hovering anywhere else (the header, the empty dropzone, past the last row) means "append".
	// The gaps *between* tiles are the exception — they belong to the grid, and reading them as
	// "append" made the marker teleport to the end of the row every time the cursor crossed one, so
	// there they hold whatever the last tile decided.
	function targetId(e: DragEvent): string | null {
		const el = e.target as HTMLElement | null;
		const tile = el?.closest('[data-pick]');
		if (tile) return tile.getAttribute('data-pick');
		return el?.closest('[data-grid]') ? (before ?? null) : null;
	}

	function over(e: DragEvent) {
		if (!isDragItem(e)) return; // a file or a link — leave it to the page
		e.preventDefault(); // required, or the drop is refused
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
		before = targetId(e);
	}

	function drop(e: DragEvent) {
		// Here as well as on `dragend`: a recent dropped in here stops being a recent, so its tile is
		// gone by the time the drag ends, and a detached node's `dragend` never reaches the window.
		dragging = false;
		const beforeId = before ?? targetId(e);
		before = undefined;
		const item = getDragItem(e);
		if (!item) return;
		e.preventDefault();
		placePick(item, beforeId);
	}

	async function play(item: BrowseItem) {
		if (busy) return;
		busy = item.id;
		try {
			await playItem(item);
		} finally {
			busy = null;
		}
	}
</script>

<!-- A drag cancelled with Esc never reaches `drop`, and only fires `dragleave` if the pointer happens
     to be moving — without this the marker stays painted until the next drag. -->
<svelte:window
	ondragend={() => {
		before = undefined;
		dragging = false;
	}}
/>

{#snippet tile(item: BrowseItem, pick: boolean)}
	{@const round = item.kind === 'artist'}
	{@const onRepeat = item.id === ON_REPEAT_ID}
	{@const art = tileArt(item)}
	<!-- Where the drop lands: a bar down the leading edge of the tile it goes in front of. -->
	{#if pick && before === item.id}
		<div class="absolute -left-1 bottom-0 top-0 z-20 w-0.5 rounded-full bg-primary"></div>
	{/if}
	<div
		class="flex h-14 cursor-pointer items-center gap-3 overflow-hidden rounded-md bg-foreground/[0.08] pr-2 text-left transition-colors hover:bg-foreground/[0.16]"
		role="button"
		tabindex="0"
		draggable="true"
		ondragstart={(e) => {
			setDragItem(e, item);
			dragging = true;
			unhover();
		}}
		onclick={() => openItem(item)}
		onkeydown={(e) => {
			if (e.target !== e.currentTarget) return;
			if (e.key === 'Enter' || e.key === ' ') {
				e.preventDefault();
				openItem(item);
			}
		}}
		title={item.subtitle ? `${item.title} — ${item.subtitle}` : item.title}
	>
		<!-- Art bleeds into the tile's leading edge (a circle can't, so an artist's is inset). -->
		<div
			class="relative shrink-0 overflow-hidden bg-muted {round
				? 'ml-1.5 h-11 w-11 rounded-full'
				: 'h-14 w-14'}"
		>
			{#if art && !failed[art]}
				<img
					src={art}
					alt=""
					class="h-full w-full object-cover"
					loading="lazy"
					draggable="false"
					onerror={() => (failed = { ...failed, [art]: true })}
				/>
			{:else}
				<div
					class="flex h-full w-full items-center justify-center {onRepeat
						? 'bg-primary/15 text-primary'
						: 'text-muted-foreground/50'}"
				>
					<!-- altIcon/showAlt, not a third ternary: `icon` is read once at mount. -->
					<HugeiconsIcon
						icon={round ? UserIcon : MusicNote01Icon}
						altIcon={ListRestartIcon}
						showAlt={onRepeat}
						class={onRepeat ? 'h-6 w-6' : 'h-5 w-5'}
					/>
				</div>
			{/if}
		</div>
		<!-- Two lines of name, the full width of the tile: the controls below float over its end
		     on hover rather than holding a gutter open the rest of the time. -->
		<div class="line-clamp-2 min-w-0 flex-1 text-sm leading-tight font-bold">{item.title}</div>
		{#if !round}
			<!-- Play at the far end, Spotify's place for it: the tile's click opens, this plays. -->
			<button
				class="absolute right-2 top-1/2 flex size-8 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full bg-primary text-primary-foreground opacity-0 shadow-[0_6px_12px_rgb(0_0_0/0.3)] transition-[opacity,scale] hover:scale-105 focus-visible:opacity-100 group-hover/pick:opacity-100"
				class:animate-pulse={busy === item.id}
				disabled={busy === item.id}
				aria-label={t('a11y.play_item', { title: item.title })}
				onclick={(e) => {
					e.stopPropagation();
					play(item);
				}}
			>
				<HugeiconsIcon icon={PlayIcon} class="h-4 w-4 fill-current" />
			</button>
		{/if}
	</div>
	<!-- The ⋯ carries Remove from shortcuts (and Add, on a recent). Right-click opens it too. -->
	<ItemMenu
		{item}
		triggerClass="absolute top-1/2 z-10 flex h-7 w-7 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full bg-background/80 text-muted-foreground opacity-0 transition hover:bg-background hover:text-foreground focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring group-hover/pick:opacity-100 {round
			? 'right-2'
			: 'right-11'}"
	/>
{/snippet}

<section>
	<div
		role="group"
		aria-label={t('home.shortcuts')}
		ondragover={over}
		ondrop={drop}
		ondragleave={(e) => {
			// Only when the pointer leaves the section itself, not on every hop between its children —
			// and measured against the box, not `relatedTarget`, which WebKit leaves null on drag
			// events. That made every hop between a tile's cover, its title and the next tile read as a
			// real exit, so the marker blinked out from under the cursor while it was still inside.
			const r = e.currentTarget.getBoundingClientRect();
			if (e.clientX < r.left || e.clientX >= r.right || e.clientY < r.top || e.clientY >= r.bottom)
				before = undefined;
		}}
	>
		{#if !picks.length && !fill.length}
			<!-- Empty: one wide drop target, not a heading plus a paragraph plus a row of ghost tiles.
			     This sits at the top of home for anyone who never uses the feature, so it earns its
			     space by being a single line and an obvious place to drop something. -->
			<button
				onclick={() => (picking = true)}
				class="flex w-full cursor-pointer items-center gap-3 rounded-md p-3 text-left transition-colors {before ===
				null
					? 'bg-primary/10'
					: 'bg-foreground/[0.08] hover:bg-foreground/[0.16]'}"
			>
				<span
					class="flex h-10 w-10 shrink-0 items-center justify-center rounded-md bg-foreground/10 text-muted-foreground"
				>
					<HugeiconsIcon icon={Add01Icon} class="h-5 w-5" />
				</span>
				<span class="min-w-0">
					<span class="block text-sm font-bold">{t('home.add_shortcut')}</span>
					<span class="block text-xs text-muted-foreground">
						{t('home.shortcuts_desc')}
					</span>
				</span>
			</button>
		{:else}
			<!-- Four to a row where there is room for four names, fewer where there isn't: the column
			     is a quarter of the width but never under 12rem. -->
			<div
				data-grid
				class="grid gap-2"
				style="grid-template-columns: repeat(auto-fill, minmax(max(12rem, calc((100% - 1.5rem) / 4)), 1fr))"
			>
				{#each picks as item (item.id)}
					<!-- group/pick, not `group`: nested unnamed groups would fire each other's hovers. -->
					<div
						class="group/pick relative"
						data-ctx
						data-pick={item.id}
						animate:flip={{ duration: 200 }}
						{...hoverable(item)}
					>
						{@render tile(item, true)}
					</div>
				{/each}

				<!-- The append slot only exists while a drag is actually heading for the end of the
				     shortcuts, so nothing dangles off the last row the rest of the time. -->
				{#if before === null && picks.length < MAX_PICKS}
					<div
						class="flex h-14 items-center justify-center rounded-md bg-primary/10 text-sm font-bold text-primary"
					>
						{t('home.add_to_end')}
					</div>
				{/if}

				{#each fill as item (item.id)}
					<div class="group/pick relative" data-ctx {...hoverable(item)}>
						{@render tile(item, false)}
					</div>
				{/each}
			</div>
		{/if}
	</div>
</section>

{#if picking}
	<ShortcutPicker onclose={() => (picking = false)} />
{/if}
