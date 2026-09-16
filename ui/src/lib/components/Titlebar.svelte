<script lang="ts">
	// Custom titlebar. The window runs undecorated by default (tauri.conf `decorations: false`);
	// with a system frame (`win.chrome`, issue #65) this bar stays as a toolbar and only drops its
	// own window buttons, since everything else on it is app navigation, not window management.
	// Everything on the bar is a drag region except the buttons; double-click maximizes (Tauri's
	// drag region itself). Right cluster: account (sign in/out, its own component), then the
	// app-level buttons, then a separator and minimize / maximize / close — per the design, the
	// window controls sit with the rest but visually apart.
	import { afterNavigate } from '$app/navigation';
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
		UserGroup02Icon,
		Link04Icon
	} from '@hugeicons/core-free-icons';
	import AccountMenu from './AccountMenu.svelte';
	import { appIcon } from '$lib/appicon.svelte';
	import { openMiniPlayer, playback, ui } from '$lib/player.svelte';
	import { win } from '$lib/win.svelte';
	import { lt } from '$lib/lt.svelte';
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
</script>

<!-- `relative` makes this a stacking context, so the account/window dropdowns inside it are capped
     at this z — it must outrank the panels below (LyricsPanel/QueuePanel, z-30). Theater mode is
     the exception: it sits at z-40 so dialogs (z-50) can open over it, so the bar has to duck
     under it instead of the other way round. -->
<header
	data-tauri-drag-region
	class="relative {ui.theaterOpen ? 'z-0' : 'z-50'} flex h-9 shrink-0 select-none items-center justify-between border-b border-border/60 bg-background"
>
	<span
		class="pointer-events-none absolute inset-x-0 text-center text-xs font-medium tracking-wide text-muted-foreground"
	>
		Limusic
	</span>

	<!-- macOS overlay style floats the traffic lights over the top-left of the webview, so the row
	     starts clear of them. 70px is the standard reservation for the three buttons. -->
	<div class="flex h-full items-center {win.chrome === 'overlay' ? 'pl-[70px]' : ''}">
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

	<div class="flex h-full items-center">
		<!-- Account first, then the app-level buttons, then the window controls. The drag region lives
		     on <header> only, so these children are ordinary buttons — don't add the attribute here. -->
		<AccountMenu />
		<div class="mx-1.5 h-4 w-px bg-border"></div>

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

		<!-- Opens the same modal as the home hero's button (one dialog, mounted in +layout). -->
		<button
			class="flex h-full w-8 items-center justify-center text-muted-foreground transition-colors hover:bg-accent/10 hover:text-foreground {lt.role !==
			'none'
				? 'text-primary'
				: ''}"
			onclick={() => (ui.ltOpen = true)}
			title={t('nav.listen_together')}
			aria-label={t('nav.listen_together')}
		>
			<span class="relative">
				<HugeiconsIcon icon={UserGroup02Icon} class="h-4 w-4" />
				{#if lt.role !== 'none'}
					<!-- A live-status dot with a ping behind it: two layers, because animate-ping
					     scales and fades the element it's on, so a lone dot would blink out. -->
					<span class="absolute -right-0.5 -top-0.5 h-1.5 w-1.5">
						<span class="absolute inset-0 animate-ping rounded-full bg-emerald-500 opacity-75"
						></span>
						<span class="absolute inset-0 rounded-full bg-emerald-500 ring-[1.5px] ring-background"
						></span>
					</span>
				{/if}
			</span>
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
			<div class="mx-1.5 h-4 w-px bg-border"></div>

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
