<script lang="ts">
	// Custom titlebar. The window runs undecorated by default (tauri.conf `decorations: false`);
	// with a system frame (`win.chrome`, issue #65) this bar stays as a toolbar and only drops its
	// own window buttons, since everything else on it is app navigation, not window management.
	// Everything on the bar is a drag region except the buttons; double-click maximizes (Tauri's
	// drag region itself).
	//
	// Three clusters. Left: the app icon and back/forward. Centre, floating over the flow so it is
	// centred on the window: home, search, history — the strip you navigate from, wherever you are.
	// Right: account (sign in/out, its own component), then the app-level buttons, then a separator
	// and minimize / maximize / close — per the design, the window controls sit with the rest but
	// visually apart.
	import { afterNavigate, goto } from '$app/navigation';
	import { page } from '$app/state';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		ArrowLeft01Icon,
		ArrowRight01Icon,
		MinusSignIcon,
		SquareIcon,
		Cancel01Icon,
		MinimizeScreenIcon,
		CameraVideoIcon,
		Link04Icon,
		Home01Icon,
		HistoryIcon,
		Search01Icon
	} from '@hugeicons/core-free-icons';
	import AccountMenu from './AccountMenu.svelte';
	import SearchSuggest from './SearchSuggest.svelte';
	import { appIcon } from '$lib/appicon.svelte';
	import { openMiniPlayer, playback, ui } from '$lib/player.svelte';
	import { win } from '$lib/win.svelte';
	import { t } from '$lib/i18n.svelte';

	// `w` is this window; `win` (imported) is the shared frame state.
	const w = getCurrentWindow();

	// Back/forward. `depth` is how many history entries deep the session is, `deepest` how far it
	// has ever been, so both buttons grey out instead of doing nothing. popstate carries a signed
	// delta (the mouse's side buttons come through here); anything else is a push, which wipes the
	// entries ahead of us.
	let depth = $state(0);
	let deepest = $state(0);
	afterNavigate((nav) => {
		if (nav.type === 'enter') depth = deepest = 0;
		else if (nav.delta !== undefined) depth = Math.max(0, depth + nav.delta);
		else deepest = depth += 1;
	});

	// Search lives on the bar rather than on the home page, so it is in the same place from inside a
	// playlist, an album or the library — the point of a command strip.
	let searchQuery = $state('');
	function goSearch() {
		if (!searchQuery.trim()) return;
		goto(`/search?${new URLSearchParams({ q: searchQuery }).toString()}`);
	}
</script>

<!-- `relative` makes this a stacking context, so the account/window dropdowns inside it are capped
     at this z — it must outrank the panels below (LyricsPanel/QueuePanel, z-30). Theater mode is
     the exception: it sits at z-40 so dialogs (z-50) can open over it, so the bar has to duck
     under it instead of the other way round. -->
<header
	data-tauri-drag-region
	class="relative {ui.theaterOpen ? 'z-0' : 'z-50'} flex h-12 shrink-0 select-none items-center justify-between bg-card"
>
	<!-- Centred on the *window*, not on the gap between the two clusters: this is the thing the eye
	     goes to, and the right cluster is three times the width of the left, so centring it in the
	     flow would park it visibly off to one side.

	     The horizontal guard is the width of the widest cluster — the right one, which grows at `lg`
	     where the account name appears — so the field shrinks as the window narrows instead of
	     sliding under the window controls. At the 900px minimum window that still leaves it ~290px.

	     pointer-events-none on the layer, auto on the group: the layer spans the whole bar, and
	     without that the dead space either side of the field would stop being a drag region. -->
	<div
		class="pointer-events-none absolute inset-x-0 flex h-full items-center justify-center px-[19rem] lg:px-[26rem]"
	>
		<!-- data-tauri-drag-region so the gaps between the three controls still drag the window. Bare,
		     not `deep`: `deep` would make every descendant a drag handle, and the suggestion panel
		     hanging below this group would then pull the window out from under the pointer. -->
		<div
			data-tauri-drag-region
			class="pointer-events-auto flex w-full max-w-lg items-center gap-2"
		>
			<!-- Home. The sidebar has one as well, and deliberately: this is the one that stays in the
			     same place however deep into a playlist you are. -->
			<a
				href="/"
				title={t('nav.home')}
				aria-label={t('nav.home')}
				class="flex size-8 shrink-0 items-center justify-center rounded-full transition-colors {page
					.url.pathname === '/'
					? 'bg-primary/15 text-primary'
					: 'bg-foreground/10 text-muted-foreground hover:bg-foreground/20 hover:text-foreground'}"
			>
				<HugeiconsIcon icon={Home01Icon} class="h-4 w-4" />
			</a>

			<!-- Must be a <form>: SearchSuggest falls through to onsubmit for a bare Enter and for its
			     own "all results" row (see the note at the top of that component). -->
			<form
				class="relative min-w-0 flex-1"
				onsubmit={(e) => {
					e.preventDefault();
					goSearch();
				}}
			>
				<HugeiconsIcon
					icon={Search01Icon}
					class="pointer-events-none absolute left-3 top-1/2 z-10 h-4 w-4 -translate-y-1/2 text-muted-foreground"
				/>
				<!-- The panel is wider than the field and centred under it with a margin rather than a
				     translate: the open animation animates `transform`, so a -translate-x-1/2 here would
				     be overwritten for the length of it and the panel would slide sideways into place. -->
				<SearchSuggest
					bind:value={searchQuery}
					placeholder={t('common.search')}
					inputClass="h-8 rounded-full pl-9"
					panelClass="left-1/2 -ml-[13rem] w-[26rem]"
				/>
			</form>

			<a
				href="/history"
				title={t('nav.history')}
				aria-label={t('nav.history')}
				class="flex size-8 shrink-0 items-center justify-center rounded-full transition-colors {page
					.url.pathname === '/history'
					? 'bg-primary/15 text-primary'
					: 'bg-foreground/10 text-muted-foreground hover:bg-foreground/20 hover:text-foreground'}"
			>
				<HugeiconsIcon icon={HistoryIcon} class="h-4 w-4" />
			</a>
		</div>
	</div>

	<!-- macOS overlay style floats the traffic lights over the top-left of the webview, so the row
	     starts clear of them. 70px is the standard reservation for the three buttons. -->
	<div
		data-tauri-drag-region
		class="flex h-full items-center {win.chrome === 'overlay' ? 'pl-[70px]' : ''}"
	>
		<!-- pointer-events-none: the logo is decoration; clicks on it should drag the window. -->
		<img src={appIcon.src} alt="" class="pointer-events-none ml-3 mr-1 h-4 w-4" />
		<!-- Bigger and heavier than the icons on the right: these are navigation, and at their
		     weight the arrow read as decoration and got missed. -->
		<button
			class="flex h-full w-9 items-center justify-center text-foreground/80 transition-colors hover:bg-accent/10 hover:text-foreground disabled:pointer-events-none disabled:opacity-25"
			onclick={() => history.back()}
			disabled={depth === 0}
			title={t('common.back')}
			aria-label={t('common.back')}
		>
			<HugeiconsIcon icon={ArrowLeft01Icon} strokeWidth={2.5} class="h-5 w-5" />
		</button>
		<button
			class="flex h-full w-9 items-center justify-center text-foreground/80 transition-colors hover:bg-accent/10 hover:text-foreground disabled:pointer-events-none disabled:opacity-25"
			onclick={() => history.forward()}
			disabled={depth === deepest}
			title={t('common.forward')}
			aria-label={t('common.forward')}
		>
			<HugeiconsIcon icon={ArrowRight01Icon} strokeWidth={2.5} class="h-5 w-5" />
		</button>
	</div>

	<div data-tauri-drag-region class="flex h-full items-center">
		<!-- Account first, then the app-level buttons, then the window controls. The drag region lives
		     on <header> only, so these children are ordinary buttons — don't add the attribute here. -->
		<AccountMenu />
		<div class="mx-1.5 h-4 w-px bg-foreground/15"></div>

		<!-- Paste a YouTube Music link and go to it: the only way into a playlist that is shared by
		     link and never appears in search or the library (#63). -->
		<button
			class="flex h-full w-8 items-center justify-center text-muted-foreground transition-colors hover:bg-accent/10 hover:text-foreground"
			onclick={() => (ui.linkOpen = true)}
			title={t('dialogs.link.title')}
			aria-label={t('dialogs.link.title')}
		>
			<HugeiconsIcon icon={Link04Icon} class="h-4 w-4" />
		</button>

		<!-- Theater mode: fullscreen, cover and lyrics, nothing else. Next to the mini player because
		     the pair are the same idea in opposite directions (shrink the app / become the screen),
		     and disabled with nothing playing, since there'd be nothing to show. -->
		<button
			class="flex h-full w-8 items-center justify-center text-muted-foreground transition-colors hover:bg-accent/10 hover:text-foreground disabled:pointer-events-none disabled:opacity-25"
			onclick={() => (ui.theaterOpen = true)}
			disabled={!playback.now}
			title={t('player.theater_mode')}
			aria-label={t('player.theater_mode')}
		>
			<HugeiconsIcon icon={CameraVideoIcon} class="h-4 w-4" />
		</button>

		<!-- Mini player: hides the app to the tray and hands over to the floating widget (mini.rs).
		     It sits with the app-level buttons rather than the window controls because it swaps what
		     you're using, not the size of this window. -->
		<button
			class="flex h-full w-8 items-center justify-center text-muted-foreground transition-colors hover:bg-accent/10 hover:text-foreground"
			onclick={openMiniPlayer}
			title={t('a11y.toggle_mini')}
			aria-label={t('a11y.toggle_mini')}
		>
			<HugeiconsIcon icon={MinimizeScreenIcon} class="h-4 w-4" />
		</button>

		{#if win.chrome === 'off'}
			<div class="mx-1.5 h-4 w-px bg-foreground/15"></div>

			<button
				class="flex h-full w-11 items-center justify-center text-muted-foreground transition-colors hover:bg-accent/10 hover:text-foreground"
				onclick={() => w.minimize()}
				aria-label={t('common.minimize')}
			>
				<HugeiconsIcon icon={MinusSignIcon} class="h-4 w-4" />
			</button>
			<button
				class="flex h-full w-11 items-center justify-center text-muted-foreground transition-colors hover:bg-accent/10 hover:text-foreground"
				onclick={() => w.toggleMaximize()}
				aria-label={t('common.maximize')}
			>
				<HugeiconsIcon icon={SquareIcon} class="h-3.5 w-3.5" />
			</button>
			<button
				class="flex h-full w-11 items-center justify-center text-muted-foreground transition-colors hover:text-destructive"
				onclick={() => w.close()}
				aria-label={t('common.close')}
			>
				<HugeiconsIcon icon={Cancel01Icon} class="h-4 w-4" />
			</button>
		{:else}
			<!-- The system frame owns closing; leave a little air before its own buttons. -->
			<div class="w-2"></div>
		{/if}
	</div>
</header>
