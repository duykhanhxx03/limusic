<script lang="ts">
	import type { Snippet } from 'svelte';
	import { mode } from 'mode-watcher';
	import { auth, playback } from '$lib/player.svelte';
	import { thumb } from '$lib/thumb';
	import { brightness, hexToHsv, hsvToHex } from '$lib/color';
	import { t, type TranslationKey } from '$lib/i18n.svelte';

	// Home's own controls (edit the page, add a shortcut) sit at the end of the greeting row.
	// `tint` is the raw colour of a cover the header should take on for now: the shortcut under the
	// pointer, Spotify's quick-access grid (HomeFeed). `null` is the header's own wash.
	let { actions, tint = null }: { actions?: Snippet; tint?: string | null } = $props();

	// How deep the header goes when it takes on a cover in the dark theme, as perceived brightness
	// (`color.ts`), the measure `toAccent` aims with too. Not an HSV value: value is hue-blind, and
	// at one value a yellow or a cyan cover came out twice as bright as a blue one, a khaki band the
	// grey controls in the greeting row could barely be read on. This is about where that value put
	// red and purple, which already looked right, so every other hue is brought to meet them.
	const DEPTH = 0.26;

	/** The cover's colour, brought to what the header can carry. Dark: the lyrics page's recipe (its
	 *  hue, a saturation that reads as a colour without going loud), at whatever value puts it at
	 *  DEPTH, so every cover gives the header the same depth. Light: a pale wash of the same hue, so
	 *  the dark greeting over it still reads. */
	const shade = $derived.by(() => {
		const hsv = tint ? hexToHsv(tint) : null;
		if (!hsv) return null;
		if (mode.current === 'light')
			return hsvToHex({ h: hsv.h, s: Math.min(0.35, Math.max(hsv.s * 0.5, 0.2)), v: 0.96 });
		const { h } = hsv;
		const s = Math.min(0.7, Math.max(hsv.s, 0.4));
		// Brightness is linear in value at a fixed hue and saturation, so one division finds it.
		return hsvToHex({ h, s, v: DEPTH / brightness(hsvToHex({ h, s, v: 1 })) });
	});
	// The layer fades out in the colour it had. Clearing the colour on the frame the fade starts
	// swaps it for transparent under a layer still mostly opaque, which reads as a flash to grey,
	// so the last colour is held and opacity alone says whether it shows. `.pre`, so a new colour
	// and the opacity that reveals it reach the DOM together.
	let held = $state<string | null>(null);
	// Whether the fill crossfades into a new colour. Only while some of the layer is on screen: from
	// opacity 0 a new colour has to land at once, or a fade in sweeps over from the last tile's hue
	// (or up from transparent, a second fade multiplied into the opacity one), and a fill left
	// transitioning under opacity 0 still has WebKit repaint the whole box every frame for nothing.
	// Asked of the layer itself rather than tracked from its transition events, because the layout
	// hides home with `display: none` (opening a tile, then coming back), and a layer hidden that way
	// is at 0 without a `transitionend` ever saying so.
	let lit = $state(false);
	let layer = $state<HTMLDivElement>();
	$effect.pre(() => {
		if (!shade) return;
		lit = !!layer && Number(getComputedStyle(layer).opacity) > 0;
		held = shade;
	});

	// Fixed at mount — a greeting that flips mid-session is uncanny.
	const hour = new Date().getHours();
	const daypartKey: TranslationKey =
		hour < 5
			? 'home.good_night'
			: hour < 12
				? 'home.good_morning'
				: hour < 18
					? 'home.good_afternoon'
					: 'home.good_evening';
	const daypart = $derived(t(daypartKey));

	// Google's CDN doesn't serve every rewritten size, so a 404'd backdrop must degrade to nothing
	// rendered, never a broken-image glyph. Re-arm whenever the track changes, mirroring MediaCard.
	let artFailed = $state(false);
	$effect(() => {
		playback.now?.thumbnail; // re-arm when the track changes
		artFailed = false;
	});
</script>

<!-- The wash is taller than the hero: it runs on behind the mood chips and the shortcuts grid and
     fades into the page under them, the colour at the top of Spotify's home. -z-10 against the feed's
     `isolate`, so it paints under everything that follows without covering it. overflow-hidden lives
     on the wash, not the hero, so the scaled blur is clipped without clipping the hero's content. -->
<div class="relative">
	<div class="pointer-events-none absolute inset-x-0 top-0 -z-10 h-[26rem] overflow-hidden">
		{#if playback.now?.thumbnail && !artFailed}
			<!-- 96px, not display size: blur-2xl is a 40px blur, so every detail above a handful of
			     pixels is thrown away anyway. The old 1200px source decoded to 5.7 MiB for this, and
			     re-decoded on every track change. -->
			<img
				src={thumb(playback.now.thumbnail, 96)}
				alt=""
				class="pointer-events-none absolute inset-0 h-full w-full art-wash scale-110 object-cover opacity-60 blur-2xl"
				onerror={() => (artFailed = true)}
			/>
		{:else}
			<!-- Nothing playing: without this the header is a bare strip with a greeting in it. An accent
			     wash keeps it a header. Inline style so it can't be lost to a stale dev stylesheet, and it
			     rides --primary so every preset theme gets its own. -->
			<div
				class="pointer-events-none absolute inset-0 opacity-[0.18]"
				style="background:radial-gradient(120% 130% at 12% 0%, var(--primary) 0%, transparent 58%)"
			></div>
		{/if}
		<!-- The hovered shortcut's colour: solid, over the playing cover (which it hides entirely
		     while it shows, as Spotify's header changes colour outright) and under the fade that
		     carries it into the page. The colour lives on this one element and only its fill and
		     opacity move: a root custom property changing on hover would restyle the whole document
		     (#217, layout.css). Tile to tile, the fill itself crossfades. Reduced motion needs nothing
		     here: layout.css cuts every transition short for it.
		     On a compositing layer of its own, for the reason .art-wash has one (layout.css): the fades
		     then run on the compositor, and a crossfade dirties this one flat layer rather than the
		     gradient, greeting and tiles it would otherwise share a backing with whenever nothing is
		     playing (the fallback under it is not promoted, so nothing lifts it for free). -->
		<div
			bind:this={layer}
			class="absolute inset-0 duration-[var(--duration-very-slow)] ease-[var(--ease-smooth-out)] will-change-[opacity] {lit
				? 'transition-[background-color,opacity]'
				: 'transition-opacity'} {shade ? 'opacity-100' : 'opacity-0'}"
			style:background-color={held}
			ontransitionend={(e) => {
				// All the way out: let go of the colour, and of the crossfade with it, together, so
				// the drop happens at once instead of as a transition nobody can see.
				if (e.propertyName === 'opacity' && !shade) {
					lit = false;
					held = null;
				}
			}}
		></div>
		<div
			class="absolute inset-0 bg-gradient-to-b from-background/30 via-background/75 to-background"
		></div>
	</div>
	<div class="relative px-6 pt-6 pb-3">
		<div class="flex min-w-0 items-center gap-3">
			{#if auth.account?.signedIn && auth.account.thumbnail}
				<!-- max-width:none defeats Tailwind Preflight's `img{max-width:100%}`, which in a tight box
				     clamps width to the content-box while height stays fixed → a vertical oval. Inline so
				     it's immune to Preflight and to stale dev CSS. -->
				<img
					src={thumb(auth.account.thumbnail, 128)}
					alt=""
					style="width:2.25rem;height:2.25rem;max-width:none"
					class="shrink-0 rounded-full object-cover"
				/>
			{/if}
			<!-- `leading-[1.35]`, not the `text-4xl` default of 1.11. `truncate` brings
			     `overflow:hidden`, so anything the line box does not cover is cut — and at 1.11 a
			     descender plus a stacked diacritic does not fit. Latin greetings never showed it
			     ("Good evening" has no descender below a tone mark); Vietnamese does, which is where
			     "Chúc ngủ ngon" lost the tails of its g's. -->
			<h1 class="min-w-0 truncate font-heading text-[2rem] font-bold leading-[1.35] tracking-tight">
				{daypart}{auth.account?.name ? `, ${auth.account.name.split(' ')[0]}` : ''}
			</h1>
			<div class="min-w-4 flex-1"></div>
			{@render actions?.()}
		</div>
	</div>
</div>
