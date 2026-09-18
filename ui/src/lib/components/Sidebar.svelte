<script lang="ts">
	import { page } from '$app/state';
	import { scale } from 'svelte/transition';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		Home01Icon,
		Search01Icon,
		Compass01Icon,
		LibraryIcon,
		Settings01Icon,
		Sun01Icon,
		Moon02Icon,
		Add01Icon,
		PinIcon,
		MusicNote01Icon,
		ListRestartIcon,
		SquareArrowLeft01Icon,
		SquareArrowRight01Icon
	} from '@hugeicons/core-free-icons';
	import { toggleMode } from 'mode-watcher';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Dialog from '$lib/components/ui/dialog';
	import { ON_REPEAT_ID, type BrowseItem } from '$lib/api';
	import { thumb } from '$lib/thumb';
	import SidebarResizer from './SidebarResizer.svelte';
	import PlaylistMenu from './PlaylistMenu.svelte';
	import {
		auth,
		library,
		personal,
		ui,
		createLibraryPlaylist,
		toggleSidebar,
		toast
	} from '$lib/player.svelte';
	import { mergeSaved, orderLibrary } from '$lib/personal';
	import { t } from '$lib/i18n.svelte';

	const nav = $derived([
		{ href: '/', label: t('nav.home'), icon: Home01Icon },
		{ href: '/search', label: t('nav.search'), icon: Search01Icon },
		{ href: '/explore', label: t('nav.explore'), icon: Compass01Icon },
		{ href: '/library', label: t('nav.library'), icon: LibraryIcon }
	]);
	const isActive = (href: string) =>
		href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);

	// Pinned first (in pin order), then everything else by last played. Derived here rather than in
	// the shared `library` store so the Library page keeps YouTube's own ordering. Playlists saved
	// on this machine sit in the same list: signed out they are the only ones there.
	const playlists = $derived(
		orderLibrary(mergeSaved(personal, library.items, 'playlist'), personal)
	);
	// How many of the leading rows are pinned — a rule under the last one explains the split.
	const pinnedCount = $derived(playlists.filter((p) => personal.pins.includes(p.id)).length);

	// YTM's library subtitle is "Owner • 20 tracks" and the rail is too narrow for both, so keep the
	// count and drop the rest. Subtitles without a number (albums: "Album • Artist") stay whole.
	const rowSubtitle = (s?: string) =>
		s
			?.split('•')
			.map((p) => p.trim())
			.filter((p) => /\d/.test(p))
			.at(-1) ?? s;

	const playlistHref = (item: BrowseItem) =>
		item.kind === 'album'
			? `/album/${encodeURIComponent(item.id)}`
			: item.kind === 'artist'
				? `/artist/${encodeURIComponent(item.id)}`
				: `/playlist/${encodeURIComponent(item.id)}`;

	// New-playlist dialog (mirrors the Library page).
	let dialogOpen = $state(false);
	let newTitle = $state('');
	let creating = $state(false);
	async function createNew() {
		const title = newTitle.trim();
		if (!title || creating) return;
		creating = true;
		try {
			await createLibraryPlaylist(title);
			toast.success(t('toasts.playlist_created', { title }));
			newTitle = '';
			dialogOpen = false;
		} catch (e) {
			toast.error(String(e));
		} finally {
			creating = false;
		}
	}

	// Account lives in the titlebar now — see AccountMenu.svelte.

	// Manual collapse is a large-screen preference: below lg the rail is already collapsed by the
	// breakpoint, so the button is hidden there and `wide()` has nothing to drop. Every expanded
	// style is an `lg:` class, so collapsing is just not emitting them. The flag lives in `ui`
	// because the overlays that offset by the sidebar's width read it too.
	const collapsed = $derived(ui.sidebarCollapsed);
	const wide = (cls: string) => (collapsed ? '' : cls);
</script>

<!-- `relative` so the drag handle can pin itself to the right edge. Width comes from the shell's
     `--sidebar-w` rather than a utility class, because the overlays read the same variable. -->
<aside
	class="relative flex h-full shrink-0 flex-col bg-sidebar p-3 text-sidebar-foreground"
	style="width: var(--sidebar-w)"
>
	<!-- Top to bottom: where to go, what you keep, the app's own switches. The switches used to sit
	     alone on a row above this one — once the titlebar took over the app's name there was nothing
	     beside them, and the rail read as a gap with two icons in it. -->
	<nav class="flex flex-col gap-1">
		{#each nav as n (n.href)}
			<a
				href={n.href}
				title={n.label}
				class="group relative flex items-center justify-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-colors {wide(
					'lg:justify-start'
				)} {isActive(n.href)
					? 'bg-primary/10 text-primary'
					: 'text-sidebar-foreground/70 hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'}"
			>
				{#if isActive(n.href)}
					<span
						transition:scale={{ duration: 200, start: 0.4 }}
						class="absolute left-0 top-1/2 h-5 w-1 -translate-y-1/2 rounded-r-full bg-primary"
					></span>
				{/if}
				<HugeiconsIcon
					icon={n.icon}
					class="h-5 w-5 shrink-0 transition-transform duration-[var(--duration-fast)] ease-[var(--ease-smooth-out)] group-hover:scale-110"
				/>
				<span class="hidden {wide('lg:inline')}">{n.label}</span>
			</a>
		{/each}
	</nav>

	<!-- Playlists. Hidden on the icon rail (needs labels; matches YTM's collapsed rail). flex-1 lets
	     the list fill the space and scroll. Signed out the section still appears once there is
	     something in it: On Repeat, or a playlist saved on this machine. -->
	{#if auth.account?.signedIn || playlists.length}
		<div class="mt-5 hidden min-h-0 flex-1 flex-col {wide('lg:flex')}">
			<div class="mb-1 flex h-8 items-center justify-between pl-3 pr-1">
				<span class="text-xs font-semibold tracking-wide text-muted-foreground">
					{t('library.playlists_tab')}
				</span>
				<!-- Creating one is a YouTube write action, so it needs an account. -->
				{#if auth.account?.signedIn}
					<Button
						variant="ghost"
						size="icon-sm"
						class="hover:text-primary"
						onclick={() => (dialogOpen = true)}
						title={t('nav.new_playlist')}
						aria-label={t('nav.new_playlist')}
					>
						<HugeiconsIcon icon={Add01Icon} strokeWidth={2} class="h-4 w-4" />
					</Button>
				{/if}
			</div>
			<div class="min-h-0 flex-1 overflow-y-auto">
				{#each playlists as pl, i (pl.id)}
					<!-- The ⋯ is a sibling of the link, not a child: a <button> inside an <a> is invalid
					     HTML. pr-9 keeps the title clear of the button that overlays the row on hover. -->
					<div class="group/row relative" data-ctx>
						<a
							href={playlistHref(pl)}
							title={pl.title}
							class="flex items-center gap-2.5 rounded-lg py-1.5 pl-2 pr-9 transition-colors hover:bg-sidebar-accent/50"
						>
							<div
								class="relative h-10 w-10 shrink-0 overflow-hidden bg-muted {pl.kind === 'artist'
									? 'rounded-full'
									: 'rounded-md'}"
							>
								{#if pl.thumbnail && pl.id !== ON_REPEAT_ID}
									<img
										src={thumb(pl.thumbnail, 96)}
										alt=""
										class="h-full w-full object-cover"
										loading="lazy"
									/>
								{:else}
									<!-- On Repeat has no artwork by nature: icon tile, same as its card. -->
									<div
										class="flex h-full w-full items-center justify-center {pl.id === ON_REPEAT_ID
											? 'bg-primary/10 text-primary'
											: 'text-muted-foreground/50'}"
									>
										<!-- altIcon/showAlt, not a ternary: `icon` is read once at mount. -->
										<HugeiconsIcon
											icon={MusicNote01Icon}
											altIcon={ListRestartIcon}
											showAlt={pl.id === ON_REPEAT_ID}
											class={pl.id === ON_REPEAT_ID ? 'h-5 w-5' : 'h-4 w-4'}
										/>
									</div>
								{/if}
							</div>
							{#if personal.pins.includes(pl.id)}
								<span
									class="absolute left-9 top-0.5 flex h-4 w-4 items-center justify-center rounded-full bg-primary text-primary-foreground"
								>
									<HugeiconsIcon icon={PinIcon} class="h-2.5 w-2.5" />
								</span>
							{/if}
							<div class="min-w-0 flex-1">
								<div class="truncate text-[13px] font-medium">{pl.title}</div>
								{#if pl.subtitle}
									<div class="truncate text-xs text-muted-foreground">{rowSubtitle(pl.subtitle)}</div>
								{/if}
							</div>
						</a>
						<PlaylistMenu item={pl} />
					</div>
					{#if pinnedCount && i === pinnedCount - 1}
						<div class="h-3"></div>
					{/if}
				{:else}
					{#if library.loading}
						<p class="px-3 py-1.5 text-xs text-muted-foreground">{t('common.loading')}</p>
					{/if}
				{/each}
			</div>
		</div>

		<Dialog.Root bind:open={dialogOpen}>
			<Dialog.Content class="sm:max-w-md">
				<Dialog.Header>
					<Dialog.Title>{t('dialogs.edit_playlist.new_title')}</Dialog.Title>
					<Dialog.Description>{t('dialogs.edit_playlist.desc_placeholder')}</Dialog.Description>
				</Dialog.Header>
				<form
					class="flex flex-col gap-4"
					onsubmit={(e) => {
						e.preventDefault();
						createNew();
					}}
				>
					<Input bind:value={newTitle} placeholder={t('dialogs.edit_playlist.name_placeholder')} autofocus />
					<Dialog.Footer>
						<Button type="button" variant="outline" onclick={() => (dialogOpen = false)}>{t('common.cancel')}</Button>
						<Button type="submit" disabled={creating || !newTitle.trim()}>
							{creating ? t('common.loading') : t('common.create')}
						</Button>
					</Dialog.Footer>
				</form>
			</Dialog.Content>
		</Dialog.Root>
	{/if}

	<!-- Pinned to the bottom whether or not the playlists above are there to push it. One row when
	     there is room for labels, a column on the 64px rail. -->
	<div
		class="mt-auto flex items-center gap-1 pt-3 {collapsed
			? 'flex-col'
			: 'flex-col lg:flex-row'}"
	>
		<button
			onclick={() => (ui.settingsOpen = true)}
			title={t('nav.settings')}
			class="group flex items-center justify-center gap-3 rounded-lg px-3 py-2 text-sm font-medium text-sidebar-foreground/70 transition-colors hover:bg-sidebar-accent/50 hover:text-sidebar-foreground {wide(
				'lg:flex-1 lg:justify-start'
			)}"
		>
			<HugeiconsIcon
				icon={Settings01Icon}
				class="h-5 w-5 shrink-0 transition-transform duration-[var(--duration-fast)] ease-[var(--ease-smooth-out)] group-hover:scale-110"
			/>
			<span class="hidden {wide('lg:inline')}">{t('nav.settings')}</span>
		</button>
		<Button
			variant="ghost"
			size="icon-sm"
			class="hover:text-primary"
			onclick={toggleMode}
			title={t('a11y.toggle_theme')}
			aria-label={t('a11y.toggle_theme')}
		>
			<HugeiconsIcon icon={Sun01Icon} strokeWidth={2} class="h-4 w-4 dark:hidden" />
			<HugeiconsIcon icon={Moon02Icon} strokeWidth={2} class="hidden h-4 w-4 dark:block" />
		</Button>
		<!-- Hidden below lg: the rail is collapsed by the breakpoint there, so there is nothing to toggle. -->
		<Button
			variant="ghost"
			size="icon-sm"
			class="hidden hover:text-primary lg:inline-flex"
			onclick={toggleSidebar}
			title={collapsed ? t('a11y.expand_sidebar') : t('a11y.collapse_sidebar')}
			aria-label={collapsed ? t('a11y.expand_sidebar') : t('a11y.collapse_sidebar')}
		>
			<!-- altIcon/showAlt, not a ternary: `icon` is read once at mount. -->
			<HugeiconsIcon
				icon={SquareArrowLeft01Icon}
				altIcon={SquareArrowRight01Icon}
				showAlt={collapsed}
				strokeWidth={2}
				class="h-4 w-4"
			/>
		</Button>
	</div>

	<SidebarResizer />
</aside>
