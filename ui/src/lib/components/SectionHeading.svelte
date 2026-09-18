<script lang="ts">
	// One header for every section on home (and every shelf elsewhere), so the page reads as one
	// document instead of a stack of unrelated widgets.
	//
	// The rule that runs from the title out to the trailing action is the whole idea: it gives a
	// section a measurable width and an end, which a bare <h2> floating over a row of cards never
	// had. It fades out rather than reaching the edge — a hard line all the way across would read as
	// a divider between sections, and these sit above their content, not between them.
	//
	// Spotify's proportions: a bold title a size up from the cards under it, and "See all" as a quiet
	// link at the far end. No icon beside the title any more — at that size the title says what the
	// section is, and a glyph in front of every heading was the busiest thing on home.
	import type { IconSvgElement } from '@hugeicons/svelte';
	import type { Snippet } from 'svelte';
	import { t } from '$lib/i18n.svelte';

	let {
		title,
		icon,
		onMore,
		moreLabel = t('common.see_all'),
		headingClass = 'font-heading text-2xl font-bold tracking-tight',
		lead,
		children
	}: {
		title: string;
		/** Kept for callers; no longer drawn (see above). */
		icon?: IconSvgElement;
		/** Renders the trailing "See all"; the title becomes a second way to click it. */
		onMore?: () => void;
		moreLabel?: string;
		/** Artist and album pages set their shelves a size larger. */
		headingClass?: string;
		/** Controls that belong to the title (Shortcuts' "Edit Home"), before the rule. */
		lead?: Snippet;
		/** Controls at the far end, before "See all". */
		children?: Snippet;
	} = $props();
</script>

<div class="mb-3 flex items-end gap-3" data-icon={icon ? '' : undefined}>
	{#if onMore}
		<button class="min-w-0 cursor-pointer text-left" onclick={onMore} title="{moreLabel} {title}">
			<h2 class="{headingClass} truncate hover:underline">{title}</h2>
		</button>
	{:else}
		<h2 class="{headingClass} min-w-0 truncate">{title}</h2>
	{/if}
	{@render lead?.()}
	<!-- Spacer, not a rule: the heading's line used to be drawn here, and the design is no lines. -->
	<div class="min-w-6 flex-1"></div>
	{@render children?.()}
	{#if onMore}
		<button
			class="shrink-0 cursor-pointer pb-1 text-sm font-bold text-muted-foreground transition-colors hover:text-foreground hover:underline"
			onclick={onMore}
		>
			{moreLabel}
		</button>
	{/if}
</div>
