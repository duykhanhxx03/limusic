<script lang="ts">
	// Every mood and genre on one page: where Explore's "See all" and the titlebar's Browse button
	// land. The grid itself is MoodGrid, which the empty search page shows as well.
	import MoodGrid from '$lib/components/MoodGrid.svelte';
	import { t } from '$lib/i18n.svelte';
</script>

<div class="p-6">
	<h1 class="mb-6 font-heading text-2xl font-bold">{t('explore.moods')}</h1>
	<MoodGrid />
</div>
