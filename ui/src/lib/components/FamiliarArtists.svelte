<script lang="ts">
	// The artists you actually play, ranked by the play counts this machine has been keeping.
	//
	// It used to be a text list beside a "flower" of avatars floating in space, and the flower was
	// decoration that carried no information: the same five faces again, arranged by trigonometry.
	// What makes this section worth a slot is the one number no YouTube shelf has, your own play
	// count, and the old layout spent both columns hiding it behind subscriber counts that are
	// identical for every user on earth.
	//
	// So: a leaderboard. #1 gets a poster, the rest are rows whose background is filled in
	// proportion to their plays, which turns the list into a bar chart you read without noticing.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { UserLove02Icon, UserIcon, UserStar01Icon } from '@hugeicons/core-free-icons';
	import SectionHeading from './SectionHeading.svelte';
	import { Skeleton } from '$lib/components/ui/skeleton';
	import PlaylistMenu from './PlaylistMenu.svelte';
	import * as api from '$lib/api';
	import type { Account, ArtistPage, BrowseItem } from '$lib/api';
	import { thumb } from '$lib/thumb';
	import { getCached, putCached } from '$lib/pagecache';
	import { topArtistIds } from '$lib/personal';
	import { auth, personal, toast } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	// ponytail: one browse call per artist, because the subscriber count and the subscribe state
	// only exist on the artist page. Six of them, shared with the artist route's cache (same key),
	// fired once per mount. Cut the count before reaching for a batch endpoint that doesn't exist.
	const COUNT = 6; // one poster + five rows
	const MIN = 3; // fewer familiar artists than this and the section isn't worth a slot
	const SLACK = 2; // spare ids, fetched only for the ones whose page comes back unparseable

	/**
	 * What the section draws of an artist, and all that is remembered of one between launches.
	 *
	 * The play count is carried on the row rather than looked up from `personal` at render time,
	 * because the two ids are not the same one: we browse by the id the play counter recorded
	 * (`id`), and the page comes back carrying the *subscribe button's* channel (an artist's
	 * official channel, which is often a different `UC…`). Keying the count off `channelId`
	 * therefore read 0 for everyone except the artists whose page failed to parse and fell back to
	 * the browse id.
	 */
	type Card = Pick<ArtistPage, 'channelId' | 'name' | 'thumbnail' | 'subscribers' | 'subscribed'> & {
		id: string;
	};
	/** A row on screen. `page` is missing while the row is still the one remembered from last time. */
	type Familiar = Card & { plays: number; page?: ArtistPage };

	// The rows as they were last drawn, so a launch paints the section at once instead of holding a
	// skeleton for the half second or more the artist pages take. The fresh pages replace them when
	// they land. Kept per channel because each row's subscribe state is; the rest is the same for
	// everyone on this machine. `last` is whose rows were stored last: on a cold start this mounts
	// before the account has been read, and that account is nearly always the one from last time.
	// localStorage, like `personal`: nothing outside this webview reads it.
	const STORE = 'limusic:familiar';
	type Stored = { last?: string; by: Record<string, Card[]> };
	/** Whose rows these are: a channel, not the Google login, since each brand channel subscribes
	 *  on its own. */
	const owner = (a: Account) => (a.signedIn ? (a.channelId ?? a.handle ?? null) : 'guest');
	/** Only rows of the shape `remember` writes: localStorage is user-writable. */
	const isCard = (r: Card | null): r is Card =>
		typeof r?.id === 'string' &&
		typeof r.channelId === 'string' &&
		typeof r.name === 'string' &&
		(r.thumbnail == null || typeof r.thumbnail === 'string');

	const ids = topArtistIds(personal, COUNT + SLACK);
	const playsOf = (id: string) => personal.artists[id]?.count ?? 0;
	/** A sign-in or out remounts this (the layout keys the page on the epoch); see `remember`. */
	const epoch = auth.epoch;

	function readStore(): Stored {
		try {
			const s = JSON.parse(localStorage.getItem(STORE) ?? 'null');
			if (s && typeof s.by === 'object' && s.by) return s;
		} catch {
			// Unreadable: start again. The pages are about to be fetched anyway.
		}
		return { by: {} };
	}

	/** The remembered rows for today's top ids, in today's order, with today's play counts. */
	function recall(): Familiar[] {
		const s = readStore();
		const who = auth.account ? owner(auth.account) : s.last;
		const rows = who != null && Array.isArray(s.by[who]) ? s.by[who].filter(isCard) : [];
		const byId = new Map(rows.map((r) => [r.id, r]));
		return ids
			.flatMap((id) => {
				const r = byId.get(id);
				return r ? [{ ...r, subscribed: !!r.subscribed, plays: playsOf(id) }] : [];
			})
			.slice(0, COUNT);
	}

	const remembered = recall();
	// Raw: the list is only ever replaced whole, and a fetched row carries its full artist page,
	// which there is no reason to wrap in proxies.
	let artists = $state.raw<Familiar[]>(remembered);
	/** Nothing worth drawing yet. Remembered rows count, so a launch normally starts with this false. */
	let loading = $state(remembered.length < MIN);
	/** Subscribe state per channel, optimistic — seeded from the remembered rows, then the pages. */
	let subs = $state<Record<string, boolean>>(
		Object.fromEntries(remembered.map((r) => [r.channelId, r.subscribed]))
	);
	let subBusy = $state<string | null>(null);
	/** Channels subscribed or unsubscribed since mount. A page fetched before the click carries the
	 *  state from before it, so landing must not undo it. */
	const toggled = new Set<string>();
	/** 0 = sized thumb, 1 = the original URL, 2 = give up and draw the icon. Same ladder as MediaCard. */
	let attempt = $state<Record<string, number>>({});

	const top = $derived(artists[0]);
	const rest = $derived(artists.slice(1));
	// The two-column layout needs enough rows to stand beside a 4:5 poster. With one or two it
	// stretched them down the poster's whole height, leaving a row, a gap you could park a car in,
	// and another row. Below this the poster gets shorter and the rows stack from the top at their
	// natural height instead of being spread to fill.
	const sparse = $derived(rest.length < 4);
	// The bars are relative to #1. A floor, because a heavy favourite makes everything below it a
	// sliver: the ranking is the information, the width is only the feel of it.
	const busiest = $derived(Math.max(1, top?.plays ?? 1));
	const share = (a: Familiar) => Math.max(14, Math.round((a.plays / busiest) * 100));

	const src = (a: Card) => ((attempt[a.channelId] ?? 0) === 0 ? thumb(a.thumbnail, 400) : a.thumbnail);
	const hasArt = (a: Card) => !!a.thumbnail && (attempt[a.channelId] ?? 0) < 2;
	const imgFailed = (a: Card) => {
		const n = attempt[a.channelId] ?? 0;
		attempt = { ...attempt, [a.channelId]: n === 0 && thumb(a.thumbnail, 400) !== a.thumbnail ? 1 : 2 };
	};

	async function fetchArtist(id: string): Promise<ArtistPage | null> {
		const key = `artist:${id}`;
		const hit = getCached<ArtistPage>(key);
		if (hit) return hit;
		try {
			const page = await api.getArtist(id);
			putCached(key, page);
			return page;
		} catch {
			return null; // one dead channel doesn't cost the section
		}
	}

	/** One ranked artist as a row, or null when its page failed or never parsed. */
	async function fetchFamiliar(id: string): Promise<Familiar | null> {
		const page = await fetchArtist(id);
		// A page with no name never parsed (no header, so no art and no subscriber count either); it
		// would sit in the list as "Unknown Artist" over a placeholder icon. Drop it and let a spare
		// id take the slot.
		if (!page?.name) return null;
		const { channelId, name, thumbnail, subscribers, subscribed } = page;
		return { id, channelId, name, thumbnail, subscribers, subscribed, plays: playsOf(id), page };
	}

	/** Store the rows on show, with their subscribe state as it stands, for the next launch to paint. */
	function remember() {
		// Not once the account has changed: this instance is on its way out, and the account the
		// rows would be filed under is not the one they were fetched for.
		if (epoch !== auth.epoch || !auth.account) return;
		const who = owner(auth.account);
		if (who == null) return;
		const s = readStore();
		s.by[who] = artists.map(({ id, channelId, name, thumbnail, subscribers }) => ({
			id,
			channelId,
			name,
			thumbnail,
			subscribers,
			subscribed: !!subs[channelId]
		}));
		s.last = who;
		try {
			localStorage.setItem(STORE, JSON.stringify(s));
		} catch {
			// Quota or a locked store: the next launch shows the skeleton, as every launch once did.
		}
	}

	onMount(async () => {
		if (ids.length < MIN) {
			loading = false;
			return;
		}
		// The six on show first, all at once, and a spare only for each of those that comes back
		// unusable. The spares used to go out with the rest: eight browses on every launch, for two
		// pages that were almost never needed. Every spare ranks below all six, so appending the ones
		// that land keeps the list in play-count order.
		const pages: Familiar[] = [];
		let next = 0;
		while (pages.length < COUNT && next < ids.length) {
			const batch = ids.slice(next, next + COUNT - pages.length);
			next += batch.length;
			for (const row of await Promise.all(batch.map(fetchFamiliar))) if (row) pages.push(row);
		}
		// Not one page came back, which is most likely no network. The remembered rows are still the
		// best answer there is, so they stay on screen and in the store.
		if (!pages.length) {
			loading = false;
			return;
		}
		// Set once, in play-count order: filling the list artist by artist would reflow the feed
		// under the reader as each request lands. Usually these are the channels already on screen
		// from last time, so the swap only refreshes them in place.
		artists = pages;
		subs = Object.fromEntries(
			pages.map((p) => [p.channelId, toggled.has(p.channelId) ? subs[p.channelId] : p.subscribed])
		);
		loading = false;
		remember();
	});

	const asItem = (a: Card): BrowseItem => ({
		kind: 'artist',
		id: a.channelId,
		title: a.name ?? t('common.artist_singular'),
		subtitle: a.subscribers,
		thumbnail: a.thumbnail
	});

	const open = (a: Card) => goto(`/artist/${encodeURIComponent(a.channelId)}`);
	const rank = (i: number) => String(i + 1).padStart(2, '0');

	async function toggleSub(a: Familiar) {
		if (subBusy) return;
		const next = !subs[a.channelId];
		subBusy = a.channelId;
		toggled.add(a.channelId);
		subs = { ...subs, [a.channelId]: next };
		try {
			await api.subscribe(a.channelId, next);
			// Keep the cache truthful. A remembered row has no page of its own to write, so it patches
			// whatever page is already cached for the channel; with none cached, there is nothing stale.
			const key = `artist:${a.channelId}`;
			const page = a.page ?? getCached<ArtistPage>(key);
			if (page) putCached(key, { ...page, subscribed: next });
			remember();
			toast.success(next ? t('artist.subscribed') : t('artist.subscribe'));
		} catch (e) {
			subs = { ...subs, [a.channelId]: !next };
			toast.error(String(e));
		} finally {
			subBusy = null;
		}
	}
</script>

{#snippet subButton(a: Familiar, onDark: boolean)}
	<button
		class="flex h-8 w-8 shrink-0 cursor-pointer items-center justify-center rounded-full transition-colors {onDark
			? 'bg-black/55 hover:bg-black/70'
			: 'hover:bg-accent/10'} {subs[a.channelId]
			? 'text-primary'
			: onDark
				? 'text-white/70 hover:text-white'
				: 'text-muted-foreground hover:text-foreground'}"
		class:animate-pulse={subBusy === a.channelId}
		aria-label={subs[a.channelId]
			? t('artist.unsubscribe_from', { name: a.name ?? '' })
			: t('artist.subscribe_to', { name: a.name ?? '' })}
		onclick={(e) => {
			e.stopPropagation();
			toggleSub(a);
		}}
	>
		<HugeiconsIcon icon={UserLove02Icon} class="h-5 w-5" />
	</button>
{/snippet}

{#snippet avatar(a: Card, iconClass: string)}
	{#if hasArt(a)}
		<img
			src={src(a)}
			alt=""
			class="h-full w-full object-cover object-[center_22%]"
			loading="lazy"
			draggable="false"
			onerror={() => imgFailed(a)}
		/>
	{:else}
		<div class="flex h-full w-full items-center justify-center text-muted-foreground/40">
			<HugeiconsIcon icon={UserIcon} class={iconClass} />
		</div>
	{/if}
{/snippet}

{#if loading ? ids.length >= MIN : artists.length >= MIN}
	<section>
		<SectionHeading title={t('home.familiar_artists')} icon={UserStar01Icon} />
		<div class="grid gap-5 md:grid-cols-[15rem_1fr] lg:grid-cols-[18rem_1fr] md:gap-7">
			<!-- The poster. Tall on desktop where it sits beside the rows; letterboxed on a narrow
			     window, where a 3:4 frame at full width would be a full screen of one face. -->
			{#if loading || !top}
				<Skeleton class="aspect-[16/9] w-full rounded-2xl md:aspect-[4/5]" />
			{:else}
				<div
					class="group relative aspect-[16/9] w-full {sparse ? 'md:aspect-square' : 'md:aspect-[4/5]'}"
					data-ctx
				>
					<div
						class="relative h-full w-full cursor-pointer overflow-hidden rounded-2xl bg-muted"
						role="button"
						tabindex="0"
						onclick={() => open(top)}
						onkeydown={(e) => {
							if (e.target !== e.currentTarget) return;
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								open(top);
							}
						}}
						title={top.name ?? t('common.artist_singular')}
					>
						<div class="h-full w-full transition-transform duration-[var(--duration-fast)] ease-[var(--ease-smooth-out)] group-hover:scale-[1.05]">
							{@render avatar(top, 'h-10 w-10')}
						</div>
						<div
							class="pointer-events-none absolute inset-0 bg-gradient-to-t from-black/90 via-black/25 to-transparent"
						></div>
						<div class="pointer-events-none absolute inset-x-0 bottom-0 p-4">
							<div class="font-heading text-xs font-bold tracking-widest text-primary">
								{rank(0)}
							</div>
							<div class="mt-1 line-clamp-2 font-heading text-xl font-bold leading-tight text-white">
								{top.name ?? t('common.unknown_artist')}
							</div>
							<div class="mt-1 truncate text-xs text-white/60">
								{t('library.play_count', { count: top.plays })}{top.subscribers
									? ` · ${top.subscribers}`
									: ''}
							</div>
						</div>
					</div>
					<div class="absolute right-2 top-2 flex items-center gap-1">
						{@render subButton(top, true)}
						<PlaylistMenu
							item={asItem(top)}
							showPin={false}
							vertical
							iconClass="h-5 w-5"
							triggerClass="flex h-8 w-8 shrink-0 cursor-pointer items-center justify-center rounded-full bg-black/55 text-white/70 transition hover:bg-black/70 hover:text-white"
						/>
					</div>
				</div>
			{/if}

			<!-- Ranks two and down. Each row's fill is its share of the leader's plays, so the shape of
			     what you listen to is in the background of the list rather than in a second widget. -->
			<div class="flex flex-col gap-1 {sparse ? 'justify-start' : 'justify-between'}">
				{#if loading || !top}
					{#each Array(Math.min(ids.length, COUNT) - 1) as _, i (i)}
						<div class="flex items-center gap-3 p-2" aria-hidden="true">
							<Skeleton class="h-3 w-5 rounded" />
							<Skeleton class="h-11 w-11 shrink-0 rounded-full" />
							<div class="flex min-w-0 flex-1 flex-col gap-1.5">
								<Skeleton class="h-3.5 w-32 rounded" />
								<Skeleton class="h-3 w-20 rounded" />
							</div>
						</div>
					{/each}
				{:else}
					{#each rest as a, i (a.channelId)}
						<div
							class="group/row relative flex cursor-pointer items-center gap-3 overflow-hidden rounded-xl p-2 text-left"
							role="button"
							tabindex="0"
							data-ctx
							onclick={() => open(a)}
							onkeydown={(e) => {
								if (e.target !== e.currentTarget) return;
								if (e.key === 'Enter' || e.key === ' ') {
									e.preventDefault();
									open(a);
								}
							}}
						>
							<div
								class="pointer-events-none absolute inset-y-0 left-0 rounded-xl bg-gradient-to-r from-primary/[0.14] to-primary/[0.03] transition-[width,opacity] duration-[var(--duration-fast)] ease-out group-hover/row:from-primary/25 group-hover/row:to-primary/[0.06]"
								style="width:{share(a)}%"
							></div>
							<div
								class="relative w-5 shrink-0 text-center font-heading text-xs font-bold text-muted-foreground/60 transition-colors group-hover/row:text-primary"
							>
								{rank(i + 1)}
							</div>
							<div class="relative h-11 w-11 shrink-0 overflow-hidden rounded-full bg-muted">
								{@render avatar(a, 'h-5 w-5')}
							</div>
							<div class="relative min-w-0 flex-1">
								<div class="truncate text-base">{a.name ?? t('common.unknown_artist')}</div>
								<div class="truncate text-xs text-muted-foreground">
									{t('library.play_count', { count: a.plays })}
								</div>
							</div>
							<div class="relative flex shrink-0 items-center gap-0.5">
								{@render subButton(a, false)}
								<PlaylistMenu
									item={asItem(a)}
									showPin={false}
									vertical
									iconClass="h-5 w-5"
									triggerClass="flex h-8 w-8 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted-foreground transition hover:bg-accent/10 hover:text-foreground"
								/>
							</div>
						</div>
					{/each}
				{/if}
			</div>
		</div>
	</section>
{/if}
