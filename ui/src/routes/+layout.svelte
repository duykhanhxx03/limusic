<script lang="ts">
	import './layout.css';
	import favicon from '$lib/assets/favicon.svg';
	import { ModeWatcher, mode, setMode } from 'mode-watcher';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		CheckmarkCircle02Icon,
		AlertCircleIcon,
		InformationCircleIcon
	} from '@hugeicons/core-free-icons';
	import { browser } from '$app/environment';
	import { page } from '$app/state';
	import { afterNavigate, beforeNavigate } from '$app/navigation';
	import { onMount, tick } from 'svelte';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import {
		appearance,
		applyArtworkAccent,
		prewarmArtworkAccent,
		refreshArtworkAccent,
		initTheme,
		LIGHT_MODE
	} from '$lib/theme.svelte';
	import { loadAppIcon } from '$lib/appicon.svelte';
	import { thumb } from '$lib/thumb';
	import { t } from '$lib/i18n.svelte';
	import { blockForeignDrag, dragScroll } from '$lib/dnd';
	import { suppressNative } from '$lib/menu';
	import Sidebar from '$lib/components/Sidebar.svelte';
	import Titlebar from '$lib/components/Titlebar.svelte';
	import ResizeBorders from '$lib/components/ResizeBorders.svelte';
	import PlayerBar from '$lib/components/PlayerBar.svelte';
	import QueuePanel from '$lib/components/QueuePanel.svelte';
	import RightPanel from '$lib/components/RightPanel.svelte';
	import LyricsPanel from '$lib/components/LyricsPanel.svelte';
	import AddToPlaylist from '$lib/components/AddToPlaylist.svelte';
	import SettingsDialog from '$lib/components/SettingsDialog.svelte';
	import ShareDialog from '$lib/components/ShareDialog.svelte';
	import ChannelPicker from '$lib/components/ChannelPicker.svelte';
	import LinkDialog from '$lib/components/LinkDialog.svelte';
	import MiniPlayer from '$lib/components/MiniPlayer.svelte';
	import NowPlaying from '$lib/components/NowPlaying.svelte';
	import TheaterMode from '$lib/components/TheaterMode.svelte';
	import VideoSurface from '$lib/components/VideoSurface.svelte';
	import CommandPalette from '$lib/components/CommandPalette.svelte';
	import KeyboardShortcuts from '$lib/components/KeyboardShortcuts.svelte';
	import HomeFeed from '$lib/components/HomeFeed.svelte';
	import { Button } from '$lib/components/ui/button';
	import {
		auth,
		chooseNpTab,
		dismissToast,
		initApp,
		np,
		playback,
		toggleRightPanel,
		ui
	} from '$lib/player.svelte';
	import { win, initWin } from '$lib/win.svelte';
	import { initZoom } from '$lib/zoom';
	import { initShortcuts } from '$lib/shortcuts';
	import { initErrorLog } from '$lib/errlog';
	import {
		updateState,
		installUpdate,
		openDownloadPage,
		checkForUpdatesQuiet,
		QUIET_INTERVAL_MS
	} from '$lib/updater.svelte';

	let { children } = $props();
	// Queue and lyrics toggle independently and both float over the page rather than docking into
	// it — two docked columns squeezed the content down to an unusable strip. At lg+ they sit side
	// by side over the content; narrower, they stack (see QueuePanel / LyricsPanel).
	let queueOpen = $state(false);
	let lyricsOpen = $state(false);
	/** The player bar's height, for `--bar-h` (see the root element). */
	let barHeight = $state(0);
	// Two ways the now-playing view and these panels can divide the same two buttons, picked in
	// settings (#62). Tabbed (the default): the view carries queue and lyrics itself, so the panels
	// step aside for it and the bar's buttons switch its tabs. Off: these are the only owner, the
	// buttons always mean the panels, and the panels float over that view like they float over a
	// page, so opening it costs you nothing you had open.
	const tabbed = $derived(np.open && appearance.tabbedPlayer);
	$effect(() => {
		if (tabbed) queueOpen = lyricsOpen = false;
	});
	// Tabbed, the lyrics button opens the player view on its lyrics tab — from anywhere, not only
	// while the view is up — and closes it when that tab is what is showing. There used to be a
	// lyrics page of its own over the content as well, which was a second place for the same words.
	// Untabbed the view has no lyrics column, so that page is still how the words get shown.
	function toggleLyrics() {
		if (!appearance.tabbedPlayer) {
			lyricsOpen = !lyricsOpen;
		} else if (np.open && np.tab === 'lyrics') {
			np.open = false;
		} else {
			np.open = true;
			// Picked once the view is up: a tab chosen before it mounts reads as a choice older than
			// the track's lyrics lookup, which the view overrules when there turn out to be none
			// (see NowPlaying). This button is exactly the choice it must not overrule.
			tick().then(() => chooseNpTab('lyrics'));
		}
	}

	// The right column (now playing, or the queue) docks from xl up, as Spotify's does. Narrower, it
	// would squeeze the page to a strip, so there the queue floats over the page as it always did.
	// It steps aside while the full now-playing view is open, which shows all of it and more — but
	// hidden, not unmounted: taken out, it came back on close with every cover reloading and the
	// artist fetched again.
	let docked = $state(false);
	$effect(() => {
		const mq = matchMedia('(min-width: 80rem)');
		docked = mq.matches;
		const on = () => (docked = mq.matches);
		mq.addEventListener('change', on);
		return () => mq.removeEventListener('change', on);
	});
	$effect(() => {
		if (docked) queueOpen = false;
	});
	const rightMounted = $derived(docked && !!ui.rightPanel && !!playback.now);
	const rightOpen = $derived(rightMounted && !np.open);

	// "Adapt colors to artwork": re-run on every track change and on the toggle itself. The 120px
	// cover is the one the player bar has already loaded, so this costs no extra request.
	$effect(() => {
		applyArtworkAccent(
			appearance.artworkAccent ? thumb(playback.now?.thumbnail, 120) : null
		);
	});
	// The accent is banded against the active theme (a cover's colour that reads on a light page is
	// mud on a dark one, #137), so flipping light/dark has to re-derive it from the same cover.
	$effect(() => {
		mode.current;
		refreshArtworkAccent();
	});
	// Same colour, one track early. Reading it off the queue instead of the track change means the
	// palette starts moving on the frame the artwork swaps, not after a fetch and a decode.
	$effect(() => {
		if (!appearance.artworkAccent) return;
		const q = playback.queue;
		prewarmArtworkAccent(thumb(q.items[q.currentIndex + 1]?.thumbnail, 120));
	});

	// The mini player runs this same SPA in a second window (Rust `mini.rs`), so the window label is
	// what tells the two apart: `mini` gets the widget instead of the app chrome, and none of the
	// routes below it are ever rendered. Constant for the window's lifetime.
	const isMini = browser && getCurrentWindow().label === 'mini';

	// Apply the saved accent color before the first paint (ssr=false → nothing renders until now).
	if (browser) initTheme();

	// Home is kept alive. Rendered as an ordinary route it was torn down on every trip away and
	// rebuilt on the way back: the mood chip reset to All, the pages loaded further down were gone,
	// the scroll position went to the top, and the rebuild itself was the ~200 ms hitch measured on
	// every arrival at `/`. It mounts on the first visit and after that is only hidden.
	const onHome = $derived(page.url.pathname === '/');
	let homeMounted = $state(page.url.pathname === '/');
	$effect(() => {
		if (onHome) homeMounted = true;
	});
	// <main> is the scroller every page shares, so home's place in it is kept here. A route that
	// unmounts starts the next page at the top by collapsing the content; a hidden home collapses
	// nothing, so the page after it is put at the top explicitly.
	let mainEl = $state<HTMLElement | null>(null);
	let homeScroll = 0;
	beforeNavigate(({ from }) => {
		if (from?.url?.pathname === '/' && mainEl) homeScroll = mainEl.scrollTop;
	});
	afterNavigate(({ from, to }) => {
		// `from` exists with a null `url` on some navigations (the first one after hydration), and a
		// throw in here aborts SvelteKit's client navigation into a full page load — which would
		// unmount the very home this is trying to keep.
		const fromPath = from?.url?.pathname;
		if (!mainEl || !fromPath) return;
		const leaving = fromPath === '/';
		const arriving = to?.url?.pathname === '/';
		if (arriving && !leaving) mainEl.scrollTop = homeScroll;
		else if (leaving && !arriving) mainEl.scrollTop = 0;
	});
	/** A click on a link to the page already open (Home in the sidebar while on Home) navigates
	 *  nowhere, so it scrolls that page back to the top instead: the tab-bar habit from every
	 *  mobile app, and the quickest way back up a long feed. */
	function scrollToTopOnSamePage(e: MouseEvent) {
		if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
		const a = (e.target as Element | null)?.closest?.('a[href]') as HTMLAnchorElement | null;
		if (!a || !mainEl || a.target === '_blank') return;
		const url = new URL(a.href, location.href);
		if (url.origin !== location.origin) return;
		if (url.pathname !== page.url.pathname || url.search !== page.url.search) return;
		if (mainEl.scrollTop > 0) mainEl.scrollTo({ top: 0, behavior: 'smooth' });
	}

	// The custom app icon (#173) is a file on disk, so the titlebar has to ask Rust for it.
	if (browser) loadAppIcon();

	// Wire the Tauri event bridge once for the whole app; teardown on destroy. Check for an update
	// on every app open (silent unless one exists).
	onMount(() => {
		// Light mode is hidden (LIGHT_MODE): a "light" or "system" saved before it was is replaced
		// once, here. ModeWatcher, a child, has already applied what was saved by the time a
		// parent's onMount runs, so this is the last word; from the next launch on, the saved
		// value is "dark" and the first frame is dark too.
		if (!LIGHT_MODE) setMode('dark');
		// Before the mini-window bail-out: both windows run this SPA and both can throw.
		initErrorLog();
		if (isMini) {
			// The widget gets the transport keys too. No zoom: it is a fixed-size card.
			const teardownMiniApp = initApp(true);
			const teardownMiniKeys = initShortcuts(true);
			return () => {
				teardownMiniApp();
				teardownMiniKeys();
			};
		}
		// First: it reveals the window (see initWin).
		const teardownWin = initWin();
		checkForUpdatesQuiet();
		// Repeat while the app stays open: ✕ hides to tray by default, so this component can stay
		// mounted for days and a mount-only check would never see a release published in between.
		const updateTimer = setInterval(checkForUpdatesQuiet, QUIET_INTERVAL_MS);
		const teardownApp = initApp();
		const teardownZoom = initZoom();
		const teardownShortcuts = initShortcuts();
		return () => {
			clearInterval(updateTimer);
			teardownApp();
			teardownWin();
			teardownZoom();
			teardownShortcuts();
		};
	});
</script>

<!-- oncontextmenu: the app's own menus handle their right-click and stop the event, so anything
     that reaches the window is a place where WebKit would have offered back / reload / inspect.
     Text fields and selections keep the native menu (see `suppressNative`). -->
<svelte:document onclick={scrollToTopOnSamePage} />
<svelte:window
	ondragover={blockForeignDrag}
	ondrop={blockForeignDrag}
	oncontextmenu={suppressNative}
/>

<svelte:head><link rel="icon" href={favicon} /></svelte:head>
<!-- With light mode hidden (LIGHT_MODE, theme.svelte.ts): dark whatever the OS says. -->
<ModeWatcher defaultMode={LIGHT_MODE ? 'system' : 'dark'} track={LIGHT_MODE} />

<!-- The mini player is the whole window when it is the window: no titlebar, no sidebar, no routes,
     and no toasts (a banner would cover most of a 560x180 widget). -->
{#if isMini}
	<MiniPlayer />
{:else}
	<!-- The window itself is transparent; this root paints the background and, when not maximized,
	     rounds the corners (the compositor can't round an undecorated window for us). Theater mode
	     counts as maximized here: it is fullscreen, and rounding it clips the corners of a view that
	     is meant to reach every edge (#139). With a system frame (win.chrome) the compositor rounds
	     for us, so ours would only fight it.
	     12px, not `rounded-lg`: that resolves to --radius, which every theme sets differently, so
	     the window corner used to change with the theme. This is the GNOME/Adwaita value (#65). -->
	<!-- --bar-h: the player bar's measured height, which the now-playing view reaches down under so
	     that, when the bar slides away while lyrics are being read, the view's own background is
	     what is left there rather than a bare strip. -->
	<div
		class="app-canvas flex h-screen flex-col overflow-hidden text-foreground {win.maximized ||
		ui.theaterOpen ||
		win.chrome !== 'off'
			? ''
			: 'rounded-[12px]'}"
		style="--bar-h:{barHeight}px"
	>
		<ResizeBorders />
		<Titlebar />
		<!-- Three panels on the canvas: the library, the page, and the now-playing column. `--sidebar-w`
		     is the one place the library's width is decided; the responsive rule lives in CSS
		     (layout.css): below `lg` it is a rail of covers whatever the stored width says. -->
		<div
			class="app-shell relative flex min-h-0 flex-1 gap-2 px-2"
			data-sidebar={ui.sidebarCollapsed ? 'collapsed' : 'open'}
			style="--sidebar-open:{ui.sidebarWidth}px"
		>
			<Sidebar />
			<!-- The page's panel. `relative`: the now-playing view and the lyrics and queue panels are
			     overlays inside it, so they cover exactly the page and nothing has to know how wide the
			     library is. -->
			<div class="relative min-w-0 flex-1">
				<!-- dragScroll: dragging a card up to home's Shortcuts grid has to be possible from
				     anywhere in the feed, so aiming at the top edge scrolls this container while the drag
				     is in flight. -->
				<main
					bind:this={mainEl}
					class="panel h-full overflow-y-auto overflow-x-hidden"
					{@attach dragScroll}
				>
					<!-- Remount the current page on sign-in/out so it refetches with the new account. -->
					{#key auth.epoch}
						<!-- `contents` while home is the page, so its layout is exactly what the route drew;
						     `hidden` otherwise, which keeps it mounted and costs no layout or paint. -->
						{#if homeMounted}
							<div class={onHome ? 'contents' : 'hidden'}>
								<HomeFeed />
							</div>
						{/if}
						{@render children()}
					{/key}
				</main>
				{#if np.open && playback.now}<NowPlaying {queueOpen} {lyricsOpen} />{/if}
				<!-- The lyrics page covers the page's panel; a floating queue (narrow windows) goes over it. -->
				{#if lyricsOpen}<LyricsPanel onClose={() => (lyricsOpen = false)} />{/if}
				{#if queueOpen}<QueuePanel onClose={() => (queueOpen = false)} />{/if}
			</div>
			{#if rightMounted}
				<div class={rightOpen ? 'contents' : 'hidden'}><RightPanel /></div>
			{/if}
			<!-- Always mounted, unlike the player view: it owns the one <video> element, which has to
			     keep playing while the view is closed. It renders nothing but a zero-sized parking
			     container until the view borrows the picture. -->
			<VideoSurface />
		</div>
		{#if playback.now}
			<!-- Slides up from its own height on first play; leaves instantly (bar removal is rare).
			     z-20 on the wrapper, not the bar: the intro's transform makes this a stacking context,
			     so a z on the footer inside would be trapped under it. The now-playing view is z-20 and
			     earlier in the DOM, which is what puts it behind the bar as it slides in and out. -->
			<div
				class="relative z-20"
				in:fly={{ y: 64, duration: 250, easing: cubicOut }}
				bind:clientHeight={barHeight}
			>
				<!-- Its own element for the immersive slide: the wrapper above carries the intro's
				     transform, and the two would fight over one. Translate only, so nothing reflows and
				     the lyrics above don't move a pixel as it goes. -->
				<div
					class="transition-[translate,opacity] duration-[var(--duration-very-slow)] ease-[var(--ease-smooth-out)] {ui.immersive
						? 'pointer-events-none translate-y-full opacity-0'
						: ''}"
				>
					<PlayerBar
						onToggleQueue={() =>
							tabbed
								? chooseNpTab('queue')
								: docked
									? toggleRightPanel('queue')
									: (queueOpen = !queueOpen)}
						queueOpen={tabbed
							? np.tab === 'queue'
							: docked
								? rightOpen && ui.rightPanel === 'queue'
								: queueOpen}
						onTogglePanel={docked && !np.open ? () => toggleRightPanel('playing') : undefined}
						panelOpen={rightOpen && ui.rightPanel === 'playing'}
						onToggleLyrics={toggleLyrics}
						lyricsOpen={appearance.tabbedPlayer ? np.open && np.tab === 'lyrics' : lyricsOpen}
					/>
				</div>
			</div>
		{/if}
	</div>

	<!-- Theater mode covers everything, titlebar included, and puts the window in fullscreen for as
	     long as it is mounted. Nothing playing means nothing to show, and that guard is also what
	     closes it (and leaves fullscreen) when the queue runs out. -->
	{#if ui.theaterOpen && playback.now}<TheaterMode />{/if}

	<CommandPalette />
	<KeyboardShortcuts />
	<AddToPlaylist />
	<ShareDialog />
	<SettingsDialog />
	<ChannelPicker />
	<LinkDialog />

	<!-- The two notification banners below run at z-[100]. Dialogs and menus sit at z-50 and portal to
	     <body>, so a z-50 banner loses the tie on DOM order and hides behind an open modal. -->
	{#if updateState.available}
		<div
			transition:fly={{ y: 16, duration: 220, easing: cubicOut }}
			class="fixed bottom-24 left-1/2 z-[100] flex -translate-x-1/2 items-center gap-3 rounded-lg glass px-4 py-2 text-sm"
		>
			<span>{t('settings.about.update_available', { version: updateState.available.version })}</span>
			{#if updateState.canInstall}
				<Button size="sm" onclick={installUpdate} disabled={updateState.installing}>
					{updateState.installing ? t('common.loading') : t('settings.about.install_update')}
				</Button>
			{:else}
				<!-- Packaged build (.rpm, AUR): the updater can only rewrite an AppImage, so send them
				     to the releases page and let their package manager do it. -->
				<Button size="sm" onclick={openDownloadPage}>{t('settings.about.download_page')}</Button>
			{/if}
			{#if !updateState.installing}
				<button
					class="text-muted-foreground hover:text-foreground"
					aria-label={t('common.close')}
					onclick={() => (updateState.available = null)}>✕</button
				>
			{/if}
		</div>
	{/if}

	{#if ui.toast}
		{@const t = ui.toast}
		<div
			in:fly={{ y: 16, duration: 150, easing: cubicOut }}
			out:fly={{ y: 16, duration: 350, easing: cubicOut }}
			class="fixed bottom-40 left-1/2 z-[100] flex -translate-x-1/2 items-center gap-2 rounded-lg glass px-4 py-2 text-sm"
		>
			<!-- Three branches instead of a ternary on `icon`: HugeiconsIcon freezes `icon` at mount, so a
			     new toast replacing a visible one would keep the old glyph. -->
			{#if t.kind === 'success'}
				<HugeiconsIcon icon={CheckmarkCircle02Icon} class="h-4 w-4 shrink-0 text-primary" />
			{:else if t.kind === 'error'}
				<HugeiconsIcon icon={AlertCircleIcon} class="h-4 w-4 shrink-0 text-destructive" />
			{:else}
				<HugeiconsIcon
					icon={InformationCircleIcon}
					class="h-4 w-4 shrink-0 text-muted-foreground"
				/>
			{/if}
			{t.msg}
			{#if t.action}
				{@const action = t.action}
				<button
					type="button"
					class="-my-1 ml-1 cursor-pointer rounded-md px-2 py-1 text-sm font-bold text-primary transition-colors hover:bg-primary/10"
					onclick={() => {
						action.run();
						dismissToast();
					}}
				>
					{action.label}
				</button>
			{/if}
		</div>
	{/if}
{/if}
