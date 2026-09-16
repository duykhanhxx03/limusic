<script lang="ts">
	import { auth, playback } from '$lib/player.svelte';
	import { thumb } from '$lib/thumb';
	import { t, type TranslationKey } from '$lib/i18n.svelte';

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

<!-- overflow-hidden lives on the backdrop wrapper, not the hero, so the scaled blur is clipped
     without clipping the hero's own content. (Search and its preview panel used to hang out of the
     bottom edge here; both live on the titlebar now.) -->
<div class="relative">
	<div class="pointer-events-none absolute inset-0 overflow-hidden">
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
		<div
			class="absolute inset-0 bg-gradient-to-t from-background via-background/70 to-background/40"
		></div>
		<div
			class="absolute inset-0 bg-gradient-to-r from-background/80 via-background/30 to-transparent"
		></div>
	</div>
	<div class="relative p-6 pt-8">
		<div class="flex min-w-0 items-center gap-3">
			{#if auth.account?.signedIn && auth.account.thumbnail}
				<!-- max-width:none defeats Tailwind Preflight's `img{max-width:100%}`, which in a tight box
				     clamps width to the content-box while height stays fixed → a vertical oval. Inline so
				     it's immune to Preflight and to stale dev CSS. -->
				<img
					src={thumb(auth.account.thumbnail, 128)}
					alt=""
					style="width:2.75rem;height:2.75rem;max-width:none"
					class="shrink-0 rounded-full object-cover ring-2 ring-foreground/15"
				/>
			{/if}
			<h1 class="truncate font-heading text-4xl font-bold tracking-tight">
				{daypart}{auth.account?.name ? `, ${auth.account.name.split(' ')[0]}` : ''}
			</h1>
		</div>
	</div>
</div>
