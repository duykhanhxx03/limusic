<script lang="ts">
	import { page } from '$app/state';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		LibraryIcon,
		Add01Icon,
		PinIcon,
		MusicNote01Icon,
		ListRestartIcon,
		Search01Icon,
		Cancel01Icon,
		ArrowRight01Icon,
		Menu01Icon,
		VolumeHighIcon
	} from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Dialog from '$lib/components/ui/dialog';
	import { ON_REPEAT_ID, type BrowseItem } from '$lib/api';
	import { thumb } from '$lib/thumb';
	import { chipClass } from '$lib/chip';
	import { anchorMenu, fitMenu, NO_ANCHOR, toBody } from '$lib/menu';
	import SidebarResizer from './SidebarResizer.svelte';
	import ChipRail from './ChipRail.svelte';
	import PlaylistMenu from './PlaylistMenu.svelte';
	import {
		auth,
		library,
		personal,
		playback,
		ui,
		createLibraryPlaylist,
		loadLibraryExtras,
		toggleSidebar,
		toast
	} from '$lib/player.svelte';
	import { mergeSaved, orderLibrary } from '$lib/personal';
	import { t } from '$lib/i18n.svelte';

	// The library panel: everything you keep — playlists, albums, artists — in one list, the way
	// Spotify's "Your Library" does it. Where to *go* (home, search, explore) lives in the titlebar.

	type Kind = 'playlist' | 'album' | 'artist';
	type Sort = 'recent' | 'alpha';

	// Albums and artists come from their own library endpoints; ask for them once there is an
	// account to ask about. The Library page shares the same store, so neither fetches twice.
	$effect(() => {
		if (auth.account?.signedIn) loadLibraryExtras();
	});

	let kind = $state<Kind | null>(null);
	let sort = $state<Sort>(readSort());
	let query = $state('');
	let searching = $state(false);

	function readSort(): Sort {
		try {
			return localStorage.getItem('library_sort') === 'alpha' ? 'alpha' : 'recent';
		} catch {
			return 'recent';
		}
	}
	function chooseSort(s: Sort) {
		sort = s;
		sortOpen = false;
		try {
			localStorage.setItem('library_sort', s);
		} catch {
			/* a convenience, not state */
		}
	}

	const byKind = $derived({
		playlist: mergeSaved(personal, library.items, 'playlist'),
		album: mergeSaved(personal, library.albums ?? [], 'album'),
		artist: mergeSaved(personal, library.artists ?? [], 'artist')
	});
	// Only the kinds there is something of get a chip; one chip alone would filter nothing.
	const kinds = $derived((['playlist', 'album', 'artist'] as const).filter((k) => byKind[k].length));

	/** Folded for matching: case, Vietnamese tone marks and đ all fall away. */
	const fold = (s: string) =>
		s.normalize('NFD').replace(/\p{M}/gu, '').replace(/đ/gi, 'd').toLowerCase();

	// Pinned first (in pin order), then by the chosen order. Recents is last played, which is what
	// the list always was; A–Z keeps the pins on top too, as Spotify does.
	const rows = $derived.by(() => {
		const pool = kind ? byKind[kind] : [...byKind.playlist, ...byKind.album, ...byKind.artist];
		let list = orderLibrary(pool, personal);
		if (sort === 'alpha') {
			const pinned = list.filter((i) => personal.pins.includes(i.id));
			const rest = list
				.filter((i) => !personal.pins.includes(i.id))
				.sort((a, b) => a.title.localeCompare(b.title));
			list = [...pinned, ...rest];
		}
		const q = fold(query.trim());
		return q ? list.filter((i) => fold(`${i.title} ${i.subtitle ?? ''}`).includes(q)) : list;
	});

	const kindLabel = (k: BrowseItem['kind']) =>
		k === 'album'
			? t('library.kind_album')
			: k === 'artist'
				? t('library.kind_artist')
				: t('library.kind_playlist');

	/** "Playlist • 25 songs", "Album • Sơn Tùng M-TP", "Artist": the kind, then the one detail that
	 *  tells two rows apart. YouTube's own line repeats the kind for albums and lists an owner and a
	 *  count for playlists, of which the count is the part that fits. */
	function rowLine(item: BrowseItem): string {
		const label = kindLabel(item.kind);
		const parts = (item.subtitle ?? '')
			.split('•')
			.map((p) => p.trim())
			.filter((p) => p && fold(p) !== fold(label) && !/^(album|single|ep|playlist|artist)$/i.test(p));
		const detail =
			item.kind === 'playlist' ? (parts.filter((p) => /\d/.test(p)).at(-1) ?? parts[0]) : parts[0];
		return item.kind === 'artist' || !detail ? label : `${label} • ${detail}`;
	}

	const href = (item: BrowseItem) =>
		item.kind === 'album'
			? `/album/${encodeURIComponent(item.id)}`
			: item.kind === 'artist'
				? `/artist/${encodeURIComponent(item.id)}`
				: `/playlist/${encodeURIComponent(item.id)}`;
	const isOpen = (item: BrowseItem) =>
		decodeURIComponent(page.url.pathname) === decodeURIComponent(href(item));
	// The queue only knows the title of what it was started from, so that is what is matched.
	const isPlaying = (item: BrowseItem) =>
		!!playback.now && !!playback.queue.sourceName && playback.queue.sourceName === item.title;

	// Sort menu.
	let sortOpen = $state(false);
	let sortAnchor = $state(NO_ANCHOR);

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

	// Manual collapse is a large-screen preference: below lg the panel is already the rail of covers,
	// so `wide()` has nothing to add there. Every expanded style is an `lg:` class, so collapsing is
	// just not emitting them. The flag lives in `ui` because the shell's width rule reads it too.
	const collapsed = $derived(ui.sidebarCollapsed);
	const wide = (cls: string) => (collapsed ? '' : cls);
	const signedIn = $derived(!!auth.account?.signedIn);
</script>

{#snippet cover(item: BrowseItem, size: string)}
	<div
		class="relative {size} shrink-0 overflow-hidden bg-muted {item.kind === 'artist'
			? 'rounded-full'
			: 'rounded-md'}"
	>
		{#if item.thumbnail && item.id !== ON_REPEAT_ID}
			<img src={thumb(item.thumbnail, 96)} alt="" class="h-full w-full object-cover" loading="lazy" />
		{:else}
			<!-- On Repeat has no artwork by nature: icon tile, same as its card. -->
			<div
				class="flex h-full w-full items-center justify-center {item.id === ON_REPEAT_ID
					? 'bg-primary/15 text-primary'
					: 'text-muted-foreground/50'}"
			>
				<!-- altIcon/showAlt, not a ternary: `icon` is read once at mount. -->
				<HugeiconsIcon
					icon={MusicNote01Icon}
					altIcon={ListRestartIcon}
					showAlt={item.id === ON_REPEAT_ID}
					class="h-5 w-5"
				/>
			</div>
		{/if}
	</div>
{/snippet}

<!-- `relative` so the drag handle can pin itself to the right edge. Width comes from the shell's
     `--sidebar-w`, which is where the responsive and collapsed rules live. -->
<aside class="panel relative flex h-full shrink-0 flex-col" style="width: var(--sidebar-w)">
	<!-- Header: the title collapses the panel to its rail (Spotify's gesture), the + creates. -->
	<div
		class="flex shrink-0 items-center gap-2 px-3 pt-3 pb-2 {collapsed
			? 'flex-col'
			: 'flex-col lg:flex-row lg:justify-between lg:pl-4'}"
	>
		<button
			type="button"
			onclick={toggleSidebar}
			title={collapsed ? t('a11y.expand_sidebar') : t('a11y.collapse_sidebar')}
			class="group flex h-9 min-w-0 cursor-pointer items-center gap-3 rounded-md px-2 text-muted-foreground transition-colors hover:text-foreground {wide(
				'lg:-ml-2'
			)}"
		>
			<HugeiconsIcon icon={LibraryIcon} class="h-6 w-6 shrink-0" />
			<span class="hidden truncate font-heading text-base font-bold {wide('lg:inline')}">
				{t('nav.library')}
			</span>
		</button>
		<div class="flex items-center gap-1">
			<!-- Creating one is a YouTube write action, so it needs an account. -->
			{#if signedIn}
				<Button
					variant="secondary"
					size="sm"
					class="h-8 rounded-full px-3 {collapsed ? 'w-8 px-0' : 'max-lg:w-8 max-lg:px-0'}"
					onclick={() => (dialogOpen = true)}
					title={t('nav.new_playlist')}
					aria-label={t('nav.new_playlist')}
				>
					<HugeiconsIcon icon={Add01Icon} strokeWidth={2} class="h-4 w-4" />
					<span class="hidden {wide('lg:inline')}">{t('library.create')}</span>
				</Button>
			{/if}
			<a
				href="/library"
				title={t('library.open_full')}
				aria-label={t('library.open_full')}
				class="hidden size-8 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground {wide(
					'lg:flex'
				)}"
			>
				<HugeiconsIcon icon={ArrowRight01Icon} class="h-4 w-4" />
			</a>
		</div>
	</div>

	{#if !collapsed}
		<!-- Filters. Hidden on the rail, and below lg, where the panel is the rail anyway. -->
		{#if kinds.length > 1}
			<div class="hidden shrink-0 px-4 pb-2 lg:block">
				<ChipRail class="items-center gap-2">
					{#if kind}
						<button
							type="button"
							onclick={() => (kind = null)}
							aria-label={t('library.clear_filter')}
							class="flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full bg-foreground/10 transition-colors hover:bg-foreground/15"
						>
							<HugeiconsIcon icon={Cancel01Icon} class="h-4 w-4" />
						</button>
					{/if}
					{#each kinds as k (k)}
						{#if !kind || kind === k}
							<button
								type="button"
								onclick={() => (kind = kind === k ? null : k)}
								aria-pressed={kind === k}
								class="{chipClass(kind === k)} whitespace-nowrap"
							>
								{k === 'playlist'
									? t('library.playlists_tab')
									: k === 'album'
										? t('library.albums_tab')
										: t('library.artists_tab')}
							</button>
						{/if}
					{/each}
				</ChipRail>
			</div>
		{/if}

		<!-- Find and order. -->
		{#if rows.length || query}
			<div class="hidden shrink-0 items-center justify-between gap-2 px-2 pb-1 lg:flex">
				{#if searching}
					<div class="relative min-w-0 flex-1">
						<HugeiconsIcon
							icon={Search01Icon}
							class="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
						/>
						<!-- svelte-ignore a11y_autofocus -->
						<input
							bind:value={query}
							autofocus
							placeholder={t('library.search_library')}
							onkeydown={(e) => {
								if (e.key === 'Escape') {
									query = '';
									searching = false;
								}
							}}
							onblur={() => {
								if (!query) searching = false;
							}}
							class="h-8 w-full rounded-md bg-foreground/10 pr-2 pl-8 text-sm outline-none placeholder:text-muted-foreground"
						/>
					</div>
				{:else}
					<button
						type="button"
						onclick={() => (searching = true)}
						title={t('library.search_library')}
						aria-label={t('library.search_library')}
						class="flex size-8 cursor-pointer items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
					>
						<HugeiconsIcon icon={Search01Icon} class="h-4 w-4" />
					</button>
				{/if}
				<button
					type="button"
					onclick={(e) => {
						sortAnchor = anchorMenu(e, { align: "right" });
						sortOpen = !sortOpen;
					}}
					class="flex h-8 shrink-0 cursor-pointer items-center gap-2 rounded-md px-2 text-sm text-muted-foreground transition-colors hover:text-foreground"
				>
					{sort === 'alpha' ? t('library.sort_alpha') : t('library.sort_recent')}
					<HugeiconsIcon icon={Menu01Icon} class="h-4 w-4" />
				</button>
			</div>
		{/if}
	{/if}

	<!-- The list. On the rail (collapsed, or below lg) only the covers. -->
	<div class="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
		{#each rows as item, i (item.kind + item.id)}
			{@const pinned = personal.pins.includes(item.id)}
			{@const playing = isPlaying(item)}
			<!-- The ⋯ is a sibling of the link, not a child: a <button> inside an <a> is invalid HTML.
			     pr-10 keeps the title clear of the button that overlays the row on hover. -->
			<div class="group/row relative" data-ctx>
				<a
					href={href(item)}
					title={item.title}
					class="flex items-center justify-center gap-3 rounded-md p-2 transition-colors {wide(
						'lg:justify-start lg:pr-10'
					)} {isOpen(item) ? 'bg-foreground/10' : 'hover:bg-foreground/5'}"
				>
					{@render cover(item, 'size-12')}
					<div class="hidden min-w-0 flex-1 {wide('lg:block')}">
						<div class="truncate text-base leading-snug {playing ? 'text-primary' : ''}">
							{item.title}
						</div>
						<div class="flex min-w-0 items-center gap-1.5 text-sm leading-snug text-muted-foreground">
							{#if pinned}
								<HugeiconsIcon
									icon={PinIcon}
									class="h-3.5 w-3.5 shrink-0 text-primary"
									aria-label={t('library.pinned')}
								/>
							{/if}
							<span class="truncate">{rowLine(item)}</span>
						</div>
					</div>
					{#if playing}
						<HugeiconsIcon
							icon={VolumeHighIcon}
							class="hidden h-4 w-4 shrink-0 text-primary group-hover/row:hidden {wide('lg:block')}"
						/>
					{/if}
				</a>
				{#if !collapsed}
					<div class="hidden lg:block"><PlaylistMenu {item} /></div>
				{/if}
			</div>
			{#if sort === 'recent' && !query && pinned && !personal.pins.includes(rows[i + 1]?.id ?? '')}
				<div class="h-2"></div>
			{/if}
		{:else}
			{#if library.loading}
				<p class="hidden px-2 py-1.5 text-xs text-muted-foreground {wide('lg:block')}">
					{t('common.loading')}
				</p>
			{:else if query}
				<p class="hidden px-2 py-1.5 text-xs text-muted-foreground {wide('lg:block')}">
					{t('library.no_matches', { query })}
				</p>
			{:else if !signedIn}
				<!-- Signed out with nothing saved on this machine: say what the panel is for. -->
				<div class="mx-1 mt-2 hidden rounded-lg bg-foreground/5 p-4 {wide('lg:block')}">
					<p class="text-base font-bold">{t('library.empty_title')}</p>
					<p class="mt-1 text-sm text-muted-foreground">{t('library.empty_signed_out')}</p>
				</div>
			{/if}
		{/each}
	</div>

	<SidebarResizer />
</aside>

{#if sortOpen}
	<button
		class="fixed inset-0 z-40 cursor-default"
		onclick={() => (sortOpen = false)}
		aria-label={t('a11y.close_menu')}
	></button>
	<div
		class="fixed z-50 min-w-56 animate-in rounded-lg glass p-1 text-popover-foreground duration-[var(--duration-quick)] ease-[var(--ease-smooth-out)] fade-in-0 zoom-in-[0.97]"
		style={sortAnchor.style}
		{@attach toBody}
		{@attach fitMenu(sortAnchor)}
	>
		<p class="px-3 pt-2 pb-1 text-xs font-bold text-muted-foreground">{t('library.sort_by')}</p>
		{#each [['recent', t('library.sort_recent')], ['alpha', t('library.sort_alpha')]] as [key, label] (key)}
			<button
				type="button"
				onclick={() => chooseSort(key as Sort)}
				class="menu-item justify-between {sort ===
				key
					? 'text-primary'
					: ''}"
			>
				{label}
			</button>
		{/each}
	</div>
{/if}

<Dialog.Root bind:open={dialogOpen}>
	<Dialog.Content class="sm:max-w-md">
		<Dialog.Header>
			<Dialog.Title>{t('dialogs.edit_playlist.new_title')}</Dialog.Title>
			<Dialog.Description>{t('dialogs.edit_playlist.new_desc')}</Dialog.Description>
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
				<Button type="button" variant="ghost" size="lg" onclick={() => (dialogOpen = false)}>{t('common.cancel')}</Button>
				<Button type="submit" size="lg" disabled={creating || !newTitle.trim()}>
					{creating ? t('common.loading') : t('common.create')}
				</Button>
			</Dialog.Footer>
		</form>
	</Dialog.Content>
</Dialog.Root>
