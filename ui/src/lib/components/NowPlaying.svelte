<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { fade, scale } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { slideUp } from '$lib/motion';
	import { beforeNavigate } from '$app/navigation';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		Maximize01Icon,
		Minimize01Icon,
		Mic01Icon,
		MusicNote01Icon,
		PlayIcon,
		PauseIcon,
		Queue01Icon,
		Video01Icon,
		VideoOffIcon,
		ArrowDown01Icon,
		FullScreenIcon
	} from '@hugeicons/core-free-icons';
	import * as Tabs from '$lib/components/ui/tabs';
	import * as api from '$lib/api';
	import { chooseNpTab, np, playback, ui } from '$lib/player.svelte';
	import { durationSecs, lyricsFor, peekLyrics } from '$lib/prefetch.svelte';
	import { canVideo, claimVideo, hasVideo, parkVideo, showVideo, video } from '$lib/video.svelte';
	import { appearance } from '$lib/theme.svelte';
	import { t } from '$lib/i18n.svelte';
	import { artworkLadder } from '$lib/thumb';
	import ArtworkSwap from './ArtworkSwap.svelte';
	import QueueList from './QueueList.svelte';
	import LyricsView from './LyricsView.svelte';

	// Off in settings, this view drops its tabs and the queue/lyrics panels stay in charge of both
	// (see +layout): they paint above this (z-30 over z-20), so all this needs is to hand back the
	// width they take at lg+ instead of letting them cover a third of the artwork. Below lg they're
	// a scrimmed overlay and there's nothing to shrink into. In tabbed mode both are always closed.
	let { queueOpen, lyricsOpen }: { queueOpen: boolean; lyricsOpen: boolean } = $props();
	const tabbed = $derived(appearance.tabbedPlayer);
	// ponytail: mirrors QueuePanel / LyricsPanel's w-80, keep in sync if those change.
	const panels = $derived(Number(queueOpen) + Number(lyricsOpen));
	const inset = $derived(['', 'lg:right-80', 'lg:right-[40rem]'][panels]);

	// Going somewhere means the user wants that page, not this one: minimise. The player bar brings
	// it back. beforeNavigate (not a pathname effect) so clicking the tab you're already on counts.
	beforeNavigate(() => (np.open = false));

	// Enlarged lyrics take the whole view, artwork column and tab strip included. A class swap
	// rather than unmounting the tabs: LyricsView must survive it or it refetches and loses its
	// scroll position.
	let big = $state(false);
	$effect(() => {
		if (np.tab !== 'lyrics') big = false; // nothing to enlarge on the queue tab
	});

	// The tab content waits for the slide to finish. Measured on the way in
	// (2026-09-17, a 20-track queue): mounting everything at once held the main thread 120–190 ms
	// before the first frame, and Svelte only creates a transition's real animation from a callback
	// on that thread, so the slide started late by exactly that — then the queue's rows forced a
	// layout mid-flight. The view that arrives is the cover and the controls; the list lands under
	// them a beat later, when nothing is moving any more. The timer covers a mount with no intro.
	let settled = $state(false);
	onMount(() => {
		const t = setTimeout(() => (settled = true), 700);
		return () => clearTimeout(t);
	});

	// Google's CDN doesn't serve every rewritten size for every image (see MediaCard), and at this
	// size a broken-image glyph *is* the page. So step down until one loads: crisp, then the size
	// proven everywhere else in the app, then the 120 the player bar is already showing for this
	// very track, and only then a music note. `ArtworkSwap` walks the ladder.
	let bgFailed = $state(false);
	$effect(() => {
		playback.now?.thumbnail; // re-arm on every track change
		bgFailed = false;
	});
	// Largest first, sized for the display; see `artworkLadder` for why a music video's thumbnail
	// needs its own list.
	const srcs = $derived(artworkLadder(playback.now?.thumbnail));

	// Clicking the artwork toggles playback, and flashes the action just taken over it so the click
	// visibly did something. Read `paused` before the toggle: the backend event that flips it is a
	// round trip away, and the icon has to be right on the frame the user clicked.
	let flash: 'play' | 'pause' | null = $state(null);
	let flashTimer: ReturnType<typeof setTimeout>;
	function toggle() {
		flash = playback.paused ? 'play' : 'pause';
		clearTimeout(flashTimer);
		flashTimer = setTimeout(() => (flash = null), 220);
		api.togglePause();
	}

	// --- the tab follows the lyrics ------------------------------------------------------------
	// The reader picks a tab and the view keeps to it (np.chosen, remembered across launches) —
	// except that "Lyrics" over a track that has none is an empty panel, so for that track the view
	// steps over to the queue, and steps back on the next track that has some. Stepping away waits
	// out NO_LYRICS_GRACE_MS from the track change: the change is already a lot of motion, and the
	// lookup often lands inside it anyway. Stepping back never waits.
	//
	// Keyed on the track alone. Picking Lyrics by hand over a lyric-less track is a request to see
	// that empty panel, and it is not overruled — not even by a grace timer already running, which
	// is what `np.choiceAt` is checked against.
	const NO_LYRICS_GRACE_MS = 2500;
	const readable = (l: api.Lyrics | null) => !!l && !l.instrumental && l.lines.length > 0;
	const trackId = $derived(playback.now?.videoId);

	$effect(() => {
		const id = trackId;
		if (!id || !tabbed) return;
		return untrack(() => {
			const now = playback.now;
			if (!now || np.chosen !== 'lyrics') return;
			const started = performance.now();
			let live = true;
			let timer: ReturnType<typeof setTimeout> | undefined;
			const settle = (l: api.Lyrics | null) => {
				if (!live) return;
				if (readable(l)) {
					np.tab = 'lyrics';
					return;
				}
				timer = setTimeout(
					() => {
						if (live && np.chosen === 'lyrics' && np.choiceAt < started) np.tab = 'queue';
					},
					Math.max(0, NO_LYRICS_GRACE_MS - (performance.now() - started))
				);
			};
			const warm = peekLyrics(id);
			if (warm !== undefined) settle(warm);
			else {
				// The same lookup LyricsView makes (and shares, see prefetch.svelte.ts): with the
				// queue on screen there is no LyricsView to ask, and this is how the view finds out
				// when to come back.
				const album = playback.queue.items[playback.queue.currentIndex]?.album;
				lyricsFor({
					videoId: id,
					title: now.title,
					artists: now.artists,
					album: album ?? undefined,
					duration: durationSecs(now.duration)
				}).then(settle, () => {});
			}
			return () => {
				live = false;
				clearTimeout(timer);
			};
		});
	});

	// --- reading mode --------------------------------------------------------------------------
	// Lyrics are read, not operated. With them on screen, the music playing and the reader's hands
	// off for IDLE_MS, everything that isn't the words goes: the collapse button, the tab strip,
	// the pointer, and the player bar (the layout slides it away on `ui.immersive`). Anything the
	// reader does — a nudge of the mouse, a key — brings it all straight back.
	const IDLE_MS = 8000;
	const canRest = $derived(tabbed && np.tab === 'lyrics' && !playback.paused);
	$effect(() => {
		if (!canRest) {
			ui.immersive = false;
			return;
		}
		let timer = setTimeout(() => (ui.immersive = true), IDLE_MS);
		const wake = () => {
			if (ui.immersive) ui.immersive = false;
			clearTimeout(timer);
			timer = setTimeout(() => (ui.immersive = true), IDLE_MS);
		};
		const events = ['pointermove', 'pointerdown', 'keydown', 'wheel'] as const;
		for (const e of events) window.addEventListener(e, wake, { passive: true, capture: true });
		return () => {
			clearTimeout(timer);
			for (const e of events) window.removeEventListener(e, wake, { capture: true });
			ui.immersive = false;
		};
	});
	// Fading chrome keeps its box (nothing reflows), but stops taking clicks while invisible.
	const chrome = $derived(
		`transition-opacity duration-[var(--duration-very-slow)] ${ui.immersive ? 'pointer-events-none opacity-0' : ''}`
	);

	// --- swipe the cover -----------------------------------------------------------------------
	// Drag the artwork sideways to change track: left for the next one, right for the one before,
	// the way a stack of covers would go. It follows the pointer 1:1, and springs back if let go
	// short of SWIPE_COMMIT of its own width. A press that never travels SWIPE_START stays a click,
	// which still toggles playback.
	const SWIPE_START = 8;
	const SWIPE_COMMIT = 0.22;
	let dragX = $state(0);
	let dragging = $state(false);
	let swipePointer = -1;
	let swipeFrom = 0;
	let swipeWidth = $state(1);
	// Set by a drag, read by the click the browser may still deliver on release.
	let swiped = false;

	function swipeDown(e: PointerEvent) {
		if (e.button !== 0 || (e.target as Element).closest('[data-no-swipe]')) return;
		swipePointer = e.pointerId;
		swipeFrom = e.clientX;
		swipeWidth = (e.currentTarget as HTMLElement).clientWidth || 1;
		swiped = false;
	}
	function swipeMove(e: PointerEvent) {
		if (e.pointerId !== swipePointer) return;
		const dx = e.clientX - swipeFrom;
		if (!dragging) {
			if (Math.abs(dx) < SWIPE_START) return;
			dragging = true;
			(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
		}
		dragX = dx;
	}
	function swipeEnd(e: PointerEvent) {
		if (e.pointerId !== swipePointer) return;
		swipePointer = -1;
		if (!dragging) return;
		const dx = dragX;
		dragging = false;
		dragX = 0;
		swiped = true;
		if (e.type === 'pointerup' && Math.abs(dx) > swipeWidth * SWIPE_COMMIT) {
			if (dx < 0) api.nextTrack();
			else api.prevTrack();
		}
	}
	function onArtClick() {
		if (swiped) {
			swiped = false;
			return;
		}
		toggle();
	}
</script>

<!-- Covers the page but not the sidebar (you navigate away to minimise) and not the player bar,
     which stays in charge of transport and paints above this on the way in and out.
     z-20 matches the highest a page uses for its own chrome (home's sticky mood chips) and wins the
     tie on DOM order, since <main> is static and its z-indexes land in the same stacking context.
     The player bar and the queue/lyrics panels come later/higher, so they still paint above.
     It sits inside the page's panel (see +layout), so it covers exactly the page. -->
<div
	in:slideUp={{ duration: 460 }}
	out:slideUp={{ duration: 340 }}
	onintroend={() => (settled = true)}
	style="left: 0; bottom: calc(-1 * var(--bar-h, 0px) - 0.5rem); padding-bottom: calc(var(--bar-h, 0px) + 2rem)"
	class="absolute top-0 right-0 z-20 flex justify-center overflow-hidden rounded-t-lg bg-background px-4 py-4 sm:px-6 sm:py-6 lg:px-10 {inset} {ui.immersive
		? 'cursor-none'
		: ''}"
>
	<!-- The view reaches down under the player bar (`bottom`), padded back up by the same amount so
	     nothing inside moves. The bar paints over that strip; the strip is only seen when the bar
	     slides away while lyrics are being read, and then it is this view's own background there,
	     not a bare band of page colour. -->
	<!-- The artwork itself, blurred to a wash, is the background: same trick as HomeHero, and it
	     needs no colour extraction (which a remote image would taint the canvas for anyway). The
	     120px variant is the one the player bar has already loaded for this track, so this costs
	     no request and nothing new to decode.
	     Two opacities because the wash sits on opposite grounds: over white it has to stay pale
	     enough for dark text, over near-black it can carry more colour before muted-foreground
	     stops reading. Turn them up together if it's too subtle.

	     Not while a video is playing. WebKitGTK re-runs this 40px blur for the damaged region on
	     every video frame, and the damaged region is the video, so the cost grows with the window.
	     Measured 2026-08-20 on a 1100px box, 720p30: with the wash 13 fps of video and 74ms UI
	     frames, without it 30 fps and 17ms. Layer promotion does not help (will-change,
	     translateZ(0) and contain:paint all measured as noise), and the cost tracks the blur
	     radius rather than the image. A video fills the view on its own, so there is nothing to
	     replace it with. -->
	{#if appearance.artworkBackground && !showVideo() && srcs[2] && !bgFailed}
		<!-- Crossfaded on a track change (400 ms each way, overlapping, so the room's colour turns
		     rather than blinking through the plain background). -->
		{#key srcs[2]}
			<img
				src={srcs[2]}
				alt=""
				decoding="async"
				onerror={() => (bgFailed = true)}
				in:fade={{ duration: 400 }}
				out:fade={{ duration: 400 }}
				class="pointer-events-none absolute inset-0 h-full w-full art-wash scale-110 object-cover opacity-30 blur-2xl dark:opacity-40"
			/>
		{/key}
	{/if}

	<!-- Collapse, top left. It used to live only as a chevron in the player bar's right cluster,
	     which is the far corner of the window from where your eye is while this view is open — and
	     the same corner as the queue and lyrics buttons, so it read as one of them. Top left is
	     where a full-screen view's way out belongs.
	     Positioned against the view, not the content column, so it stays put whatever the artwork
	     does to the layout. -->
	<button
		type="button"
		onclick={() => (np.open = false)}
		aria-label={t('player.minimize_player')}
		title={t('player.minimize_player')}
		class="absolute left-4 top-4 z-20 flex h-9 w-9 items-center justify-center rounded-full bg-foreground/8 text-muted-foreground transition-colors hover:bg-foreground/15 hover:text-foreground {chrome}"
	>
		<HugeiconsIcon icon={ArrowDown01Icon} class="h-5 w-5" />
	</button>
	<!-- Theater mode, beside the way out: this view grown to the whole screen. The top right is the
	     tab strip's and the enlarge-lyrics toggle's, so it goes with the collapse. -->
	<button
		type="button"
		onclick={() => (ui.theaterOpen = true)}
		aria-label={t('player.theater_mode')}
		title={t('player.theater_mode')}
		class="absolute left-[3.75rem] top-4 z-20 flex h-9 w-9 items-center justify-center rounded-full bg-foreground/8 text-muted-foreground transition-colors hover:bg-foreground/15 hover:text-foreground {chrome}"
	>
		<HugeiconsIcon icon={FullScreenIcon} class="h-[18px] w-[18px]" />
	</button>

	<!-- Capped and centred, so a wide window doesn't park the artwork in the middle of an empty half
	     with the tabs glued to the right edge. --art is the artwork's side: whichever is smaller of
	     the column's width and the height left over once the titlebar, the player bar and this
	     padding have had theirs, at 75% so the square doesn't dominate the view.
	     ponytail: 11rem is those three measured, not computed. The 0.75 leaves it plenty of slack
	     now, so only a much taller player bar would need it raised.

	     A video gets its own budget, and a wider cap to spend it in. 16:9 in the square's width
	     leaves half the height empty, so --vid is the width that spends the same leftover height
	     instead: height * 16 / 9, capped by the column (max-width can only shrink `w-full`). The
	     80rem cap exists to stop a square drifting into an empty half, which a video this wide
	     never does. -->
	<div
		class="relative flex w-full gap-6 xl:gap-10 {showVideo() ? 'max-w-[100rem]' : 'max-w-[80rem]'}"
		style="--art:calc(min(100%,100vh - 11rem) * 0.75); --vid:calc((100vh - 11rem) * 0.85 * 16 / 9)"
	>
		{#if !big}
			<!-- Centred against the full height of the column on the right. Below md there isn't room
			     for both columns, and the queue wins. Untabbed there is no second column, so the
			     artwork is the whole view at every width. -->
			<div
				class="min-w-0 items-center justify-center {tabbed ? 'hidden flex-[4] md:flex' : 'flex flex-1'}"
			>
				<!-- A div, not a button: the video-mode toggle has to be a sibling of the play/pause
				     button rather than nested inside it (nested buttons are invalid HTML and the
				     inner one never reliably gets the click). -->
				<!-- No wheel handler here any more: scrolling over the artwork used to be the volume, and
				     next to the lyrics column the same gesture meant two different things. The volume
				     slider in the player bar still takes the wheel, since the pointer has to be on it. -->
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div
					class="relative w-full touch-pan-y select-none {showVideo()
						? 'max-w-[var(--vid)]'
						: 'max-w-[var(--art)]'} {dragging
						? ''
						: 'transition-[translate,opacity] duration-[var(--duration-fast)] ease-[var(--ease-smooth-out)]'}"
					style="translate: {dragX}px 0; opacity: {1 - Math.min(Math.abs(dragX) / swipeWidth, 1) * 0.5}"
					onpointerdown={swipeDown}
					onpointermove={swipeMove}
					onpointerup={swipeEnd}
					onpointercancel={swipeEnd}
					ondragstart={(e) => e.preventDefault()}
				>
					<button
						type="button"
						onclick={onArtClick}
						aria-label={t('a11y.play_pause')}
						class="block w-full cursor-pointer"
					>
						{#if flash}
							<!-- No backdrop-blur: re-blurring the plate on every frame of the scale is what made
							     this stutter on WebKitGTK. Transform and opacity only. -->
							<div
								in:scale={{ start: 0.7, duration: 150, easing: cubicOut }}
								out:scale={{ start: 1.3, duration: 320, easing: cubicOut }}
								class="pointer-events-none absolute inset-0 z-10 flex items-center justify-center"
							>
								<div class="rounded-full bg-black/55 p-3.5 text-white">
									<!-- icon is frozen at mount, so swap via showAlt, not a ternary. -->
									<HugeiconsIcon
										icon={PauseIcon}
										altIcon={PlayIcon}
										showAlt={flash === 'play'}
										class="h-7 w-7"
									/>
								</div>
							</div>
						{/if}
						<!-- The picture is not built here. VideoSurface owns it so it survives this view
						     being closed, and this is where it gets moved to while the view is open.
						     `display: contents` so the wrapper generates no box of its own and the video's
						     `w-full` still resolves against the button. -->
						<div
							class="contents"
							{@attach (box: HTMLElement) => {
								claimVideo(box);
								return parkVideo;
							}}
						></div>
						<!-- The artwork, when the video above isn't the picture. Both arms carry the same
						     guard rather than nesting, so the branch below keeps its indentation. -->
						{#if !showVideo()}
							<ArtworkSwap {srcs} class="rounded-2xl">
								{#snippet fallback()}
									<div
										class="flex h-full w-full items-center justify-center bg-muted text-muted-foreground/40"
									>
										<HugeiconsIcon icon={MusicNote01Icon} class="h-16 w-16" />
									</div>
								{/snippet}
							</ArtworkSwap>
						{/if}
					</button>
					{#if canVideo()}
						<!-- Both directions, or there is no way back to the video. On a plate, since it sits
						     over whatever frame happens to be showing. It shows the choice, not what is on
						     screen yet: while a picture is still catching up to the music the artwork stays
						     up, and reading `showVideo()` here offered "show video" then — a click on which
						     turned the video off. -->
						<button
							type="button"
							data-no-swipe
							onclick={() => (video.want = !video.want)}
							aria-label={hasVideo() && video.want ? t('a11y.show_artwork') : t('a11y.show_video')}
							class="absolute right-3 top-3 z-10 cursor-pointer rounded-md bg-black/40 p-1.5 text-white/70 transition-colors hover:text-white {chrome}"
						>
							<!-- icon swap via altIcon/showAlt: `icon` is frozen at mount -->
							<HugeiconsIcon
								icon={Video01Icon}
								altIcon={VideoOffIcon}
								showAlt={hasVideo() && video.want}
								class="h-4 w-4"
							/>
						</button>
					{/if}
				</div>
			</div>
		{/if}

		{#if tabbed}
			<!-- Five parts to the artwork's four: the lyrics are what this view is read for, and at a fixed
			     22–26rem they wrapped nearly every line at 20px while the cover took the rest. -->
			<div class="flex min-h-0 min-w-0 flex-col {big ? 'flex-1' : 'w-full md:w-auto md:flex-[5]'}">
				<Tabs.Root
					value={np.tab}
					onValueChange={(v) => chooseNpTab(v as typeof np.tab)}
					class="min-h-0 flex-1"
				>
					<div class="flex items-center gap-2 {big ? 'justify-end' : ''} {chrome}">
						<!-- Same two glyphs the player bar uses for the queue and lyrics buttons. -->
						<Tabs.List class={big ? 'hidden' : 'flex-1'}>
							<Tabs.Trigger value="queue" class="gap-2.5">
								<HugeiconsIcon icon={Queue01Icon} class="h-4 w-4" /> {t('player.queue')}
							</Tabs.Trigger>
							<Tabs.Trigger value="lyrics" class="gap-2.5">
								<HugeiconsIcon icon={Mic01Icon} class="h-4 w-4" /> {t('player.lyrics')}
							</Tabs.Trigger>
						</Tabs.List>
						{#if np.tab === 'lyrics'}
							<button
								onclick={() => (big = !big)}
								class="cursor-pointer rounded-md p-1.5 text-muted-foreground transition-colors hover:text-foreground"
								aria-label={big ? t('player.shrink_lyrics') : t('player.enlarge_lyrics')}
							>
								<!-- icon swap via altIcon/showAlt: `icon` is frozen at mount -->
								<HugeiconsIcon
									icon={Maximize01Icon}
									altIcon={Minimize01Icon}
									showAlt={big}
									class="h-4 w-4"
								/>
							</button>
						{/if}
					</div>
					<!-- Only the open tab is mounted: bits-ui keeps inactive content in the DOM, which would
					     leave LyricsView fetching lyrics for every track you never asked to see. -->
					{#if np.tab === 'queue'}
						<Tabs.Content value="queue" class="flex min-h-0 flex-col">
							{#if settled}
								<div class="flex min-h-0 flex-1 flex-col" in:fade={{ duration: 160 }}>
									<QueueList />
								</div>
							{/if}
						</Tabs.Content>
					{:else}
						<Tabs.Content value="lyrics" class="flex min-h-0 flex-col">
							<!-- Not while theater mode is up: it covers this view and draws the same lines
							     itself, and this stage went on drawing its karaoke underneath, a second
							     canvas at the display rate. Coming back it mounts again from the shared
							     lyrics cache and lands on the sung line, as opening the tab does. -->
							{#if settled && !ui.theaterOpen}
								<div class="flex min-h-0 flex-1 flex-col" in:fade={{ duration: 160 }}>
									<LyricsView expanded={big} />
								</div>
							{/if}
						</Tabs.Content>
					{/if}
				</Tabs.Root>
			</div>
		{/if}
	</div>
</div>
