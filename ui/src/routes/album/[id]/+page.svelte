<script lang="ts">
    import { page } from "$app/state";
    import { imgReveal } from "$lib/imgreveal";
    import { goto } from "$app/navigation";
    import { HugeiconsIcon } from "@hugeicons/svelte";
    import {
        PlayIcon,
        MoreVerticalIcon,
        ShuffleIcon,
        PlayListAddIcon,
        Radio02Icon,
        ArrowUpNarrowWideIcon,
        ArrowDownWideNarrowIcon,
        DashboardSquare02Icon,
        Share08Icon,
        BookmarkAdd02Icon,
        BookmarkCheck02Icon,
    } from "@hugeicons/core-free-icons";
    import { download } from "$lib/downloads.svelte";
    import DownloadButton from "$lib/components/DownloadButton.svelte";
    import TrackRow from "$lib/components/TrackRow.svelte";
    import TrackSelectionBar from "$lib/components/TrackSelectionBar.svelte";
    import TrackSelectButton from "$lib/components/TrackSelectButton.svelte";
    import { trackSelection } from "$lib/selection.svelte";
    import TrackFilter, {
        filterTracks,
    } from "$lib/components/TrackFilter.svelte";
    import TrackRowSkeleton from "$lib/components/TrackRowSkeleton.svelte";
    import Shelf from "$lib/components/Shelf.svelte";
    import ErrorState from "$lib/components/ErrorState.svelte";
    import ArtistLine from "$lib/components/ArtistLine.svelte";
    import ExplicitIcon from "$lib/components/ExplicitIcon.svelte";
    import { Skeleton } from "$lib/components/ui/skeleton";
    import { Button } from "$lib/components/ui/button";
    import * as api from "$lib/api";
    import type { AlbumPage, BrowseItem } from "$lib/api";
    import {
        addPick,
        openShare,
        auth,
        enqueue,
        isSaved,
        playback,
        openAddManyToPlaylist,
        playFrom,
        startRadio,
        toast,
        noteLibrary,
        toggleSaved,
    } from "$lib/player.svelte";
    import { getCached, putCached } from "$lib/pagecache";
    import { thumb } from "$lib/thumb";
    import { anchorMenu, fitMenu, NO_ANCHOR, toBody } from "$lib/menu";
    import { t } from "$lib/i18n.svelte";

    let album = $state<AlbumPage | null>(null);
    let loading = $state(true);
    let error = $state<string | null>(null);
    let expanded = $state(false);
    // ⋯ options menu, `fixed` and anchored under the button so the header can't clip it.
    let menuOpen = $state(false);
    let anchor = $state(NO_ANCHOR);

    function openMenu(e: MouseEvent) {
        anchor = anchorMenu(e);
        menuOpen = true;
    }
    // Header filter box: matches title / artist / album.
    let query = $state("");

    const id = $derived(page.params.id ?? "");
    // The rows actually on screen. Identical to `album.items` with no query typed.
    const shown = $derived(filterTracks(album?.items ?? [], query));
    const selection = trackSelection(() => album?.items ?? [], () => shown, () => `${auth.epoch}:${id}`);
    // A local album has no YouTube playlist behind it: nothing to save or add to a playlist.
    // Playing, shuffling and Shortcuts all work exactly the same.
    const isLocal = $derived(api.isLocalId(id));
    const nowId = $derived(playback.now?.videoId);

    async function load(aid: string) {
        const key = `album:${aid}`;
        const hit = getCached<AlbumPage>(key);
        if (hit) {
            album = hit;
            loading = false;
        } else {
            loading = true;
            album = null;
        }
        error = null;
        expanded = false;
        query = "";
        try {
            const fresh = await api.getAlbum(aid);
            if (aid !== id) return; // superseded by navigation — drop the stale response
            album = fresh;
            putCached(key, fresh);
        } catch (e) {
            if (aid !== id) return;
            if (!hit) error = String(e);
        } finally {
            if (aid === id) loading = false;
        }
    }

    $effect(() => {
        if (id) load(id);
    });

    // A local track deleted off disk vanishes from the open page too, header count included: the
    // page is rebuilt from SQLite (already pruned) rather than patched, so nothing can go stale.
    // An album whose last file is gone has no page left to show — step back to the library.
    $effect(() => {
        const un = api.onLocalChanged(async (removed) => {
            const a = album;
            if (!isLocal || !a) return;
            const gone = new Set(removed);
            const items = a.items.filter((i) => !gone.has(i.video_id));
            if (items.length === a.items.length) return; // not this album
            a.items = items; // the row goes now; the refetch below repairs the header counts
            await load(id);
            if (!album?.items.length) goto("/library?tab=local");
        });
        return () => un.then((f) => f());
    });

    // This album as a card, for the sidebar's last-played sort and the Shortcuts grid.
    const asItem = (): BrowseItem => ({
        // A local artist opens this route too — it stays an artist on the Shortcuts grid, so the
        // tile keeps its circle (see browse.ts `hrefFor`).
        kind: id.startsWith(api.LOCAL_ARTIST_PREFIX) ? "artist" : "album",
        id,
        title: album?.title ?? t('common.album_singular'),
        subtitle: album?.artist,
        thumbnail: album?.thumbnail,
        // Recently played keeps this object as the card it draws, so without the flag an album
        // played from its own page would lose the mark it has everywhere else.
        explicit: album?.explicit,
    });

    // `start` indexes the rows on screen, which a filter narrows. The queue is always the whole
    // album: the search box finds a track, it doesn't decide what plays after it, so playing a
    // match has to leave the same queue behind as scrolling to that row would.
    function playAll(start: number | null) {
        if (!album) return;
        const at = start === null ? null : album.items.indexOf(shown[start]);
        playFrom(asItem(), album.items, at === -1 ? null : at, album.playlistId);
    }
    function radio() {
        if (!album?.playlistId) return;
        menuOpen = false;
        startRadio("playlist", album.playlistId, album.title);
    }
    function shuffle() {
        if (!album?.items.length) return;
        menuOpen = false;
        // Real order + shuffle flag — the backend shuffles (fresh each time, restorable).
        playFrom(asItem(), album.items, null, album.playlistId, true);
    }
    // Saved on YouTube (signed in) or on this machine (signed out, and anything saved before the
    // user ever signed in). The button reads both, so a local save can't show as "Save to library"
    // while its tile sits in the library.
    const savedHere = $derived(isSaved(id));
    const inLibrary = $derived((album?.inLibrary ?? false) || savedHere);

    // Signed in, saving an album is a "like" on its audio playlist: optimistic, the button flips now
    // and reverts if YouTube rejects it (mutating `album` updates the page cache, which holds this
    // same object). Signed out there is nobody to tell, so it goes in the local library instead.
    let savingLibrary = $state(false);
    async function toggleLibrary() {
        const a = album;
        if (!a || savingLibrary) return;
        const next = !inLibrary;
        if (!auth.account?.signedIn || !a.playlistId) {
            toggleSaved(asItem());
            toast.success(next ? t('library.saved_to_library') : t('toasts.removed_from_library'));
            return;
        }
        // Signed in: YouTube owns it from here. The local row is kept in step rather than dropped,
        // so a card's ⋯ menu elsewhere shows the album as being in the library (and it still renders
        // offline); `noteLibrary` flags it synced, so nothing offers a local-only removal.
        if (a.inLibrary === next) {
            noteLibrary(asItem(), next);
            toast.success(next ? t('library.saved_to_library') : t('toasts.removed_from_library'));
            return; // YouTube already agrees; only the local row had to move
        }
        a.inLibrary = next;
        savingLibrary = true;
        try {
            await api.setAlbumSaved(a.playlistId, next);
            noteLibrary(asItem(), next);
            toast.success(next ? t('library.saved_to_library') : t('toasts.removed_from_library'));
        } catch (e) {
            a.inLibrary = !next;
            toast.error(String(e));
        } finally {
            savingLibrary = false;
        }
    }

    function queue(next: boolean) {
        if (!album?.items.length) return;
        menuOpen = false;
        enqueue(album.items, next, album.title, album.continuation);
    }

    function downloadAll() {
        if (!album?.items.length) return;
        menuOpen = false;
        // A continuation means YouTube had more than this page carried. Queueing hands the token
        // to the backend, which walks it; downloading cannot, so say what was taken.
        if (album.continuation) toast.error(t("toasts.partial_playlist_added"));
        download(album.items);
    }

    function saveToPlaylist() {
        if (!album?.items.length) return;
        menuOpen = false;
        openAddManyToPlaylist(album.items);
    }

    // The header's round buttons that are components of their own (DownloadButton,
    // TrackSelectButton) take a class, not a variant, so the secondary icon-lg look is spelled out
    // here. The icon stays muted rather than taking the variant's text colour: both components add
    // `text-primary` for their "on" state, and a second text colour in the same list would win or
    // lose on stylesheet order instead of on purpose. The ⋯ button below matches them.
    const roundIcon =
        "flex size-10 cursor-pointer items-center justify-center rounded-full bg-secondary text-muted-foreground transition hover:bg-secondary/80 hover:text-foreground disabled:opacity-50";

    // A shelf's "See all" opens the same grid route the artist page uses.
    function showMore(s: { title: string; moreBrowseId?: string; moreParams?: string }) {
        const q = new URLSearchParams({ id: s.moreBrowseId!, title: s.title });
        if (s.moreParams) q.set("params", s.moreParams);
        goto(`/list?${q.toString()}`);
    }
</script>

{#if loading}
    <div class="flex flex-col gap-5 p-6 pt-10">
        <div class="flex items-end gap-5">
            <Skeleton class="h-28 w-28 shrink-0 rounded-xl" />
            <div class="flex-1 space-y-3">
                <Skeleton class="h-3 w-16 rounded" />
                <Skeleton class="h-10 w-1/2 rounded-lg" />
                <Skeleton class="h-4 w-40 rounded" />
            </div>
        </div>
        <div class="flex gap-3">
            <Skeleton class="h-11 w-28 rounded-full" />
            <Skeleton class="h-11 w-28 rounded-full" />
        </div>
    </div>
    <div class="p-6 pt-2">
        {#each Array(8) as _, i (i)}
            <TrackRowSkeleton hideThumb />
        {/each}
    </div>
{:else if error}
    <div class="p-6"><ErrorState message={error} onRetry={() => load(id)} /></div>
{:else if album}
    <!-- Header with the blurred album cover as a hero backdrop -->
    <div class="content-in relative overflow-hidden">
        {#if album.thumbnail}
            <!-- Blurred backdrop: blur-2xl destroys any detail a bigger source would carry, so
                 ask for the smallest thing that still reads as the cover's colours. -->
            {#key album.thumbnail}
                <img
                    src={thumb(album.thumbnail, 96)}
                    alt=""
                    class="absolute inset-0 h-full w-full art-wash scale-110 object-cover opacity-50 blur-2xl"
                    {@attach imgReveal}
                />
            {/key}
        {/if}
        <div
            class="absolute inset-0 bg-gradient-to-t from-background via-background/75 to-background/40"
        ></div>

        <div class="absolute right-6 top-6 z-10">
            <TrackFilter bind:value={query} placeholder={t("common.search_this_album")} />
        </div>

        <div class="relative flex flex-col gap-5 p-6 pt-10">
            <div class="flex items-end gap-5">
                <!-- Inline width/height so the size holds even against a stale dev-server CSS that -->
                <!-- hasn't regenerated a newly-used spacing utility (would fall back to intrinsic size). -->
                {#if album.thumbnail}
                    {#key album.thumbnail}
                        <img
                            src={thumb(album.thumbnail, 400)}
                            alt=""
                            style="width:7rem;height:7rem"
                            class="shrink-0 rounded-xl bg-muted object-cover transition-opacity duration-[var(--duration-fast)] ease-[var(--ease-in-out)]"
                            {@attach imgReveal}
                        />
                    {/key}
                {:else}
                    <div
                        style="width:7rem;height:7rem"
                        class="shrink-0 rounded-xl bg-muted"
                    ></div>
                {/if}
                <div class="min-w-0">
                    <div
                        class="text-sm text-muted-foreground"
                    >
                        {album.subtitle ?? t('common.album_singular')}
                    </div>
                    <h1
                        class="mt-1 font-heading text-4xl font-extrabold tracking-tight"
                    >
                        {album.title ?? t('common.album_singular')}
                    </h1>
                    <div
                        class="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground"
                    >
                        {#if album.explicit}
                            <ExplicitIcon class="h-4 w-4 shrink-0" />
                        {/if}
                        {#if album.artist}
                            <span
                                class="flex items-center gap-1.5 font-bold text-foreground"
                            >
                                {#if album.artistThumbnail}
                                    <img
                                        src={album.artistThumbnail}
                                        alt=""
                                        class="h-5 w-5 rounded-full object-cover"
                                    />
                                {/if}
                                <ArtistLine
                                    runs={album.artistRuns}
                                    text={album.artist}
                                />
                            </span>
                        {/if}
                        {#if album.secondSubtitle}
                            <span class="text-muted-foreground/60">•</span>
                            <span>{album.secondSubtitle}</span>
                        {/if}
                    </div>
                </div>
            </div>

            {#if album.description}
                <div class="max-w-2xl">
                    <p
                        class="text-sm text-foreground/80 {expanded
                            ? ''
                            : 'line-clamp-2'}"
                    >
                        {album.description}
                    </p>
                    <button
                        class="mt-1 cursor-pointer text-sm font-bold text-muted-foreground transition-colors hover:text-foreground hover:underline"
                        onclick={() => (expanded = !expanded)}
                    >
                        {expanded ? t("common.less") : t("common.more")}
                    </button>
                </div>
            {/if}

            <!-- Controls -->
            <div class="relative flex items-center gap-3">
                <Button
                    size="lg"
                    class="gap-2"
                    onclick={() => playAll(null)}
                    disabled={!album.items.length}
                >
                    <HugeiconsIcon icon={PlayIcon} class="h-4 w-4" /> {t("player.play")}
                </Button>
                <Button
                    variant="secondary"
                    size="lg"
                    class="gap-2"
                    onclick={shuffle}
                    disabled={!album.items.length}
                >
                    <HugeiconsIcon icon={ShuffleIcon} class="h-4 w-4" /> {t("common.shuffle")}
                </Button>
                <!-- Local albums are already in the Local tab; everything else is savable, signed
                     in or not. -->
                {#if !isLocal}
                    <Button
                        variant="secondary"
                        size="lg"
                        class="gap-2 {inLibrary
                            ? 'bg-primary/25 text-primary hover:bg-primary/30'
                            : ''}"
                        onclick={toggleLibrary}
                        disabled={savingLibrary}
                    >
                        <!-- altIcon/showAlt, not a ternary: `icon` is read once at mount. -->
                        <HugeiconsIcon
                            icon={BookmarkAdd02Icon}
                            altIcon={BookmarkCheck02Icon}
                            showAlt={inLibrary}
                            class="h-4 w-4"
                        />
                        {inLibrary ? t("library.in_library") : t("library.save_to_library")}
                    </Button>
                {/if}
                <!-- Beside Play, not inside the ⋯ menu: whether an album is on your device is
                     something you want to see, not something you open a menu to find out. -->
                {#if !isLocal}
                    <DownloadButton
                        items={album.items}
                        ondownload={downloadAll}
                        disabled={!album.items.length}
                        class={roundIcon}
                    />
                {/if}
                <TrackSelectButton {selection} class={roundIcon} />
                <!-- `size-5`, not h-5 w-5: Button sizes any svg without a size-* class to 16px, and
                     this glyph sits beside the 20px ones in the two buttons before it. -->
                <Button
                    variant="secondary"
                    size="icon-lg"
                    class="text-muted-foreground hover:text-foreground"
                    onclick={openMenu}
                    aria-label={t("a11y.more_options")}
                >
                    <HugeiconsIcon icon={MoreVerticalIcon} class="size-5" />
                </Button>

                {#if menuOpen}
                    <!-- Moved to <body>: the header clips its overflow, and a menu opened from the
                         bottom of it would be cut off. -->
                    <button
                        class="fixed inset-0 z-40 cursor-default"
                        onclick={() => (menuOpen = false)}
                        oncontextmenu={(e) => {
                            e.preventDefault();
                            menuOpen = false;
                        }}
                        aria-label={t("a11y.close_menu")}
                        {@attach toBody}
                    ></button>
                    <div
                        class="fixed z-50 min-w-56 animate-in rounded-lg glass p-1 text-popover-foreground duration-[var(--duration-quick)] ease-[var(--ease-smooth-out)] fade-in-0 zoom-in-[0.97]"
                        style={anchor.style}
                        {@attach toBody}
                        {@attach fitMenu(anchor)}
                    >
                        <button
                            class="menu-item"
                            onclick={() => queue(true)}
                        >
                            <HugeiconsIcon
                                icon={ArrowUpNarrowWideIcon}
                                class="h-4 w-4"
                            /> {t("player.play_next")}
                        </button>
                        <button
                            class="menu-item"
                            onclick={() => queue(false)}
                        >
                            <HugeiconsIcon
                                icon={ArrowDownWideNarrowIcon}
                                class="h-4 w-4"
                            /> {t("player.add_to_queue")}
                        </button>
                        <!-- The album's audio playlist is what a radio seeds from; an album page
                             without one (rare) has nothing to ask YouTube for. -->
                        {#if !isLocal && album.playlistId}
                            <button
                                class="menu-item"
                                onclick={radio}
                            >
                                <HugeiconsIcon
                                    icon={Radio02Icon}
                                    class="h-4 w-4"
                                /> {t("player.start_radio")}
                            </button>
                        {/if}
                        {#if !isLocal}
                            <button
                                class="menu-item"
                                onclick={saveToPlaylist}
                            >
                                <HugeiconsIcon
                                    icon={PlayListAddIcon}
                                    class="h-4 w-4"
                                /> {t("player.save_to_playlist")}
                            </button>
                        {/if}
                        <button
                            class="menu-item"
                            onclick={() => {
                                menuOpen = false;
                                addPick(asItem());
                            }}
                        >
                            <HugeiconsIcon
                                icon={DashboardSquare02Icon}
                                class="h-4 w-4"
                            /> {t("home.add_shortcut")}
                        </button>
                        {#if !isLocal}
                            <button
                                class="menu-item"
                                onclick={() => {
                                    menuOpen = false;
                                    openShare(asItem());
                                }}
                            >
                                <HugeiconsIcon
                                    icon={Share08Icon}
                                    class="h-4 w-4"
                                /> {t("player.share")}
                            </button>
                        {/if}
                    </div>
                {/if}
            </div>
        </div>
    </div>

    <!-- Numbered track list -->
    <div class="content-in p-6 pt-2">
        <TrackSelectionBar {selection} from={album.title} />
        {#each shown as item, i (JSON.stringify([item.video_id, i]))}
            <TrackRow
                song={item}
                {selection}
                selectionKey={selection.visibleKeys[i]}
                index={i}
                hideThumb
                showPlayCount
                active={item.video_id === nowId}
                onplay={() => playAll(i)}
                onAdd={isLocal ? undefined : () => openAddManyToPlaylist([item])}
            />
        {:else}
            <p class="p-4 text-sm text-muted-foreground">
                {query.trim()
                    ? t("library.no_matching_tracks", { query: query.trim() })
                    : t("library.empty_album")}
            </p>
        {/each}
    </div>

    <!-- Other versions of this release, and what sits near it. Ruled off from the tracks so the
         page reads as the album first and its surroundings second. -->
    {#if album.sections?.length}
        <div class="content-in mt-2 flex flex-col gap-8 px-6 pb-8 pt-8">
            {#each album.sections as section, i (i + ":" + section.title)}
                <Shelf
                    title={section.title}
                    items={section.items}
                    onMore={section.moreBrowseId
                        ? () => showMore(section)
                        : undefined}
                />
            {/each}
        </div>
    {/if}
{/if}
