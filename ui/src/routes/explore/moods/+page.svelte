<script lang="ts">
	// Every mood and genre, in YouTube's own two groups. Explore carries the same buttons as a
	// single scrolling row; this is where you go to see the whole list at once, so it is laid out
	// to be read down rather than paged sideways.
	//
	// Each one is a card in its own colour with a cover turned into the corner, which is Spotify's
	// "Browse all" grid: with thirty-seven near-identical entries the colour is what an eye
	// navigates by, and the artwork is what tells you what the mood actually sounds like. YouTube
	// publishes the colour with the button; the cover costs a request per mood, so the cards ask
	// for theirs only once they are on screen (and Rust keeps them for a month).
	import { goto } from '$app/navigation';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { MusicNote01Icon } from '@hugeicons/core-free-icons';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import { Skeleton } from '$lib/components/ui/skeleton';
	import * as api from '$lib/api';
	import type { MoodGroup } from '$lib/api';
	import { getCached, putCached } from '$lib/pagecache';
	import { hexToHsv, hsvToHex, isLight } from '$lib/color';
	import { imgReveal } from '$lib/imgreveal';
	import { thumb } from '$lib/thumb';
	import { t } from '$lib/i18n.svelte';

	let groups = $state<MoodGroup[] | null>(null);
	let error = $state<string | null>(null);
	/** mood params → its cover URL, as they arrive. */
	let covers = $state<Record<string, string>>({});

	const KEY = 'moods';

	async function load() {
		const hit = getCached<MoodGroup[]>(KEY);
		if (hit) groups = hit;
		error = null;
		try {
			const fresh = await api.getMoods();
			groups = fresh;
			putCached(KEY, fresh);
		} catch (e) {
			if (!groups) error = String(e);
		}
	}

	// --- covers ---------------------------------------------------------------------------------
	//
	// One browse per mood, so they go through a queue: thirty-seven of them fired at once is a
	// burst YouTube has no reason to enjoy, and the cards at the top are the ones worth filling
	// first. Three at a time keeps the visible row arriving quickly without the stampede.
	const MAX_IN_FLIGHT = 3;
	const asked = new Set<string>();
	const queue: string[] = [];
	let inFlight = 0;

	function pump() {
		while (inFlight < MAX_IN_FLIGHT && queue.length) {
			const params = queue.shift()!;
			inFlight++;
			api.getMoodCover(params)
				// A mood with no cover (or a failed fetch) keeps the plain tile; there is nothing to
				// tell the reader and nothing to retry on their behalf.
				.then((url) => {
					if (url) covers[params] = url;
				})
				.catch(() => {})
				.finally(() => {
					inFlight--;
					pump();
				});
		}
	}

	/** Attachment for a card: ask for its cover once it is near the viewport, once ever. */
	const lazyCover = (params: string) => (node: HTMLElement) => {
		if (asked.has(params)) return;
		const io = new IntersectionObserver(
			([e]) => {
				if (!e.isIntersecting || asked.has(params)) return;
				asked.add(params);
				queue.push(params);
				pump();
				io.disconnect();
			},
			{ rootMargin: '300px 0px' }
		);
		io.observe(node);
		return () => io.disconnect();
	};

	const openMood = (title: string, params: string) =>
		goto(`/explore/mood?params=${encodeURIComponent(params)}&title=${encodeURIComponent(title)}`);

	/** YouTube ships the colour as 0xRRGGBB; anything without one falls back to the app's accent. */
	const hex = (color?: number) =>
		color === undefined ? null : `#${color.toString(16).padStart(6, '0')}`;

	/** What sits under the cover until it lands: the card's own colour moved away from it, so the
	 *  corner reads as a tile on every hue — darker on the pale ones (#ffe780), lighter on the
	 *  dark ones (#606060). */
	function tileColor(bg: string): string {
		const hsv = hexToHsv(bg);
		if (!hsv) return bg;
		const v = isLight(bg) ? Math.max(0, hsv.v - 0.28) : Math.min(1, hsv.v + 0.3);
		return hsvToHex({ ...hsv, s: hsv.s * 0.9, v });
	}

	function cardStyle(color?: number): string {
		const bg = hex(color);
		if (!bg) return '';
		return `background:${bg}; color:${isLight(bg) ? '#111' : '#fff'}; --tile:${tileColor(bg)}`;
	}

	$effect(() => {
		load();
	});
</script>

<div class="p-6">
	<h1 class="mb-6 font-heading text-2xl font-bold">{t('explore.moods')}</h1>
	{#if error}
		<ErrorState message={error} onRetry={load} />
	{:else if !groups}
		{#each Array(2) as _, g (g)}
			<section class="mb-8" aria-hidden="true">
				<Skeleton class="mb-3 h-5 w-40 rounded" />
				<div class="grid grid-cols-[repeat(auto-fill,minmax(11rem,1fr))] gap-3">
					{#each Array(6) as _, i (i)}
						<Skeleton class="aspect-[5/3] rounded-lg" />
					{/each}
				</div>
			</section>
		{/each}
	{:else}
		<div class="content-in flex flex-col gap-8">
			{#each groups as group (group.title)}
				<section>
					<h2 class="mb-3 font-heading text-lg font-semibold">{group.title}</h2>
					<div class="grid grid-cols-[repeat(auto-fill,minmax(11rem,1fr))] gap-3">
						{#each group.chips as chip (chip.params)}
							<button
								type="button"
								onclick={() => openMood(chip.title, chip.params)}
								style={cardStyle(chip.color)}
								class="group relative flex aspect-[5/3] cursor-pointer flex-col items-start justify-start overflow-hidden rounded-lg bg-primary p-3.5 text-left text-primary-foreground transition-transform duration-[var(--duration-fast)] ease-[var(--ease-smooth-out)] hover:-translate-y-0.5"
								{@attach lazyCover(chip.params)}
							>
								<span
									class="relative z-10 line-clamp-2 pr-12 font-heading text-base font-bold leading-tight"
								>
									{chip.title}
								</span>
								<!-- The cover, clipped into the corner at Spotify's angle. Until it arrives (or if
								     the mood has none) the same square stands in, in a shade of the card. -->
								<span
									class="pointer-events-none absolute -bottom-5 -right-6 flex size-20 rotate-[25deg] items-center justify-center overflow-hidden rounded-[3px] bg-[var(--tile,rgba(0,0,0,0.25))] shadow-[0_8px_20px_rgba(0,0,0,0.28)] transition-transform duration-[var(--duration-fast)] ease-[var(--ease-smooth-out)] group-hover:rotate-[18deg]"
								>
									{#if covers[chip.params]}
										<img
											src={thumb(covers[chip.params], 160)}
											alt=""
											loading="lazy"
											class="size-full object-cover transition-opacity duration-[var(--duration-quick)]"
											{@attach imgReveal}
										/>
									{:else}
										<HugeiconsIcon icon={MusicNote01Icon} class="size-6 -rotate-[25deg] opacity-30" />
									{/if}
								</span>
							</button>
						{/each}
					</div>
				</section>
			{/each}
		</div>
	{/if}
</div>
