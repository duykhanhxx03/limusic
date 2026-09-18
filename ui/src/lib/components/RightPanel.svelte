<script lang="ts" module>
	import * as api from '$lib/api';
	import type { ArtistPage } from '$lib/api';

	// "About the artist": the lead artist's page, fetched once per artist per session. Out here, not
	// in the instance, so closing and reopening the column does not fetch it again. A failure leaves
	// the card out rather than showing an error for something nobody asked for.
	const artists = new Map<string, Promise<ArtistPage | null>>();
</script>

<script lang="ts">
	// The column to the right of the page, as in Spotify: the track that is playing — its cover at
	// the size of the column, who it is by, what comes next — or the queue, which used to float over
	// the page from the same side. Docked rather than floating: at this width it leaves the page
	// room to be read, which two floating panels at once did not (see +layout).
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		Cancel01Icon,
		FavouriteIcon,
		FullScreenIcon,
		PlayIcon
	} from '@hugeicons/core-free-icons';
	import { thumb } from '$lib/thumb';
	import { imgReveal } from '$lib/imgreveal';
	import {
		auth,
		np,
		playback,
		toggleNowPlayingLike,
		toggleRightPanel,
		ui
	} from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';
	import ArtistLine from './ArtistLine.svelte';
	import Marquee from './Marquee.svelte';
	import QueueList from './QueueList.svelte';
	import SidebarResizer from './SidebarResizer.svelte';

	const now = $derived(playback.now);
	const view = $derived(ui.rightPanel);

	// The next track in the queue, for the "Next in queue" card.
	const next = $derived(playback.queue.items[playback.queue.currentIndex + 1]);

	let artist = $state<ArtistPage | null>(null);
	$effect(() => {
		const id = view === 'playing' ? now?.artistId : undefined;
		artist = null;
		if (!id) return;
		let hit = artists.get(id);
		if (!hit) {
			hit = api.getArtist(id).catch(() => null);
			artists.set(id, hit);
		}
		hit.then((a) => {
			if (now?.artistId === id) artist = a;
		});
	});
	// Local files and uploads have no artist page to go to.
	const artistHref = $derived(
		now?.artistId ? `/artist/${encodeURIComponent(now.artistId)}` : undefined
	);

	// Follow, optimistic: the button flips at once and flips back if YouTube says no.
	let following = $state(false);
	$effect(() => {
		following = artist?.subscribed ?? false;
	});
	async function toggleFollow() {
		const a = artist;
		if (!a) return;
		const want = !following;
		following = want;
		try {
			await api.subscribe(a.channelId, want);
			a.subscribed = want;
		} catch {
			following = !want;
		}
	}
</script>

<!-- Width: the dragged one, but never more than 28vw, so the page keeps its room on a narrower
     window than the one the width was picked on. `relative` for the drag handle on the left edge. -->
<aside
	class="panel relative flex h-full shrink-0 flex-col"
	style="width: min({ui.rightPanelWidth}px, 28vw)"
>
	<SidebarResizer edge="left" />
	<div class="flex shrink-0 items-center gap-2 px-4 pt-3 pb-2">
		<h2 class="min-w-0 flex-1 truncate font-heading text-base font-bold">
			{view === 'queue'
				? t('queue.title')
				: (playback.queue.sourceName ?? t('player.now_playing'))}
		</h2>
		<!-- Theater mode: where Spotify keeps its "expand" — the playing track, grown to the screen. -->
		{#if view === 'playing'}
			<button
				class="flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
				onclick={() => (ui.theaterOpen = true)}
				aria-label={t('player.theater_mode')}
				title={t('player.theater_mode')}
			>
				<HugeiconsIcon icon={FullScreenIcon} class="h-4 w-4" />
			</button>
		{/if}
		<button
			class="flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
			onclick={() => toggleRightPanel(view ?? 'playing')}
			aria-label={t('player.close_panel')}
			title={t('player.close_panel')}
		>
			<HugeiconsIcon icon={Cancel01Icon} class="h-4 w-4" />
		</button>
	</div>

	{#if view === 'queue'}
		<div class="flex min-h-0 flex-1 flex-col">
			<QueueList />
		</div>
	{:else if now}
		<div class="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
			<!-- The cover opens the full now-playing view, the same as clicking the player bar. -->
			<button
				class="block w-full cursor-pointer overflow-hidden rounded-lg bg-muted"
				onclick={() => (np.open = true)}
				aria-label={t('player.open_player')}
			>
				{#key now.thumbnail}
					{#if now.thumbnail}
						<img
							src={thumb(now.thumbnail, 720)}
							alt=""
							class="aspect-square w-full object-cover"
							{@attach imgReveal}
						/>
					{:else}
						<div class="aspect-square w-full"></div>
					{/if}
				{/key}
			</button>

			<div class="mt-4 flex items-start gap-3">
				<div class="min-w-0 flex-1">
					<Marquee text={now.title} class="font-heading text-2xl font-bold" />
					<ArtistLine
						runs={now.artistRuns}
						text={now.artists}
						class="text-base text-muted-foreground"
					/>
				</div>
				<button
					class="mt-1 flex size-9 shrink-0 cursor-pointer items-center justify-center rounded-full transition-colors hover:bg-foreground/10"
					onclick={() => toggleNowPlayingLike()}
					aria-pressed={playback.rating === 'like'}
					aria-label={playback.rating === 'like'
						? t('player.remove_from_liked')
						: t('player.save_to_liked')}
				>
					<HugeiconsIcon
						icon={FavouriteIcon}
						class="h-5 w-5 {playback.rating === 'like'
							? 'fill-current text-primary'
							: 'text-muted-foreground'}"
					/>
				</button>
			</div>

			{#if artist && (artist.thumbnail || artist.description)}
				<!-- About the artist, as Spotify lays it out: the picture with the label over its top
				     edge, then who they are, how many listen, a Follow, and three lines of the bio. -->
				<div class="mt-6 overflow-hidden rounded-lg bg-foreground/[0.06]">
					{#if artist.thumbnail}
						<a href={artistHref} class="relative block aspect-[4/3] w-full overflow-hidden bg-muted">
							<img
								src={thumb(artist.thumbnail, 720)}
								alt=""
								class="h-full w-full object-cover object-top transition-transform duration-500 hover:scale-[1.03]"
								{@attach imgReveal}
							/>
							<div
								class="pointer-events-none absolute inset-x-0 top-0 h-20 bg-gradient-to-b from-black/55 to-transparent"
							></div>
							<p class="pointer-events-none absolute left-4 top-4 text-base font-bold text-white">
								{t('player.about_artist')}
							</p>
						</a>
					{/if}
					<div class="p-4">
						<div class="flex items-center gap-3">
							<div class="min-w-0 flex-1">
								<a
									href={artistHref}
									class="block truncate font-heading text-base font-bold hover:underline"
								>
									{artist.name ?? now.artists}
								</a>
								{#if artist.monthlyListeners || artist.subscribers}
									<p class="truncate text-base text-muted-foreground">
										{artist.monthlyListeners ?? artist.subscribers}
									</p>
								{/if}
							</div>
							{#if auth.account?.signedIn && artist.channelId}
								<button
									class="shrink-0 cursor-pointer rounded-full px-4 py-1.5 text-sm font-bold transition-[background-color,border-color,scale] hover:scale-[1.03] {following
										? 'border border-foreground/30 hover:border-foreground'
										: 'bg-foreground text-background'}"
									onclick={toggleFollow}
									aria-pressed={following}
								>
									{following ? t('player.following') : t('player.follow')}
								</button>
							{/if}
						</div>
						{#if artist.description}
							<p class="mt-3 line-clamp-3 text-sm leading-relaxed text-muted-foreground">
								{artist.description}
							</p>
						{/if}
					</div>
				</div>
			{/if}

			<!-- What comes next, and the way into the whole queue. -->
			<div class="mt-4 rounded-lg bg-foreground/[0.06] p-4">
				<div class="mb-3 flex items-center justify-between gap-2">
					<p class="text-base font-bold">{t('player.next_in_queue')}</p>
					<button
						class="cursor-pointer text-sm font-bold text-muted-foreground transition-colors hover:text-foreground hover:underline"
						onclick={() => toggleRightPanel('queue')}
					>
						{t('player.open_queue')}
					</button>
				</div>
				{#if next}
					<button
						class="group/next -mx-2 flex w-[calc(100%+1rem)] cursor-pointer items-center gap-3 rounded-md p-2 text-left transition-colors hover:bg-foreground/[0.08]"
						onclick={() => api.playIndex(playback.queue.currentIndex + 1)}
						title={next.title}
					>
						<div class="relative size-12 shrink-0 overflow-hidden rounded-md bg-muted">
							{#if next.thumbnail}
								<img src={thumb(next.thumbnail, 96)} alt="" class="h-full w-full object-cover" />
							{/if}
							<div
								class="absolute inset-0 flex items-center justify-center bg-black/50 text-white opacity-0 transition-opacity group-hover/next:opacity-100"
							>
								<HugeiconsIcon icon={PlayIcon} class="h-5 w-5 fill-current" />
							</div>
						</div>
						<div class="min-w-0 flex-1">
							<p class="truncate text-base">{next.title}</p>
							<p class="truncate text-sm text-muted-foreground">{next.artists}</p>
						</div>
					</button>
				{:else}
					<p class="text-sm text-muted-foreground">{t('player.nothing_next')}</p>
				{/if}
			</div>
		</div>
	{/if}
</aside>
