<script lang="ts">
	// A page that couldn't load. Two shapes: offline, which says so in words and offers the music that
	// plays without a network, and anything else, which says something went wrong and keeps the raw
	// message small underneath for a bug report. The raw message used to be the whole state, in red.
	import { goto } from '$app/navigation';
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { Alert02Icon, Download04Icon, RefreshIcon, WifiOff01Icon } from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import { t } from '$lib/i18n.svelte';

	let { message, onRetry }: { message: string; onRetry: () => void } = $props();

	let online = $state(typeof navigator === 'undefined' || navigator.onLine);

	/** What a request that never reached the server reads like, from reqwest and the OS resolver.
	 *  `navigator.onLine` alone misses the common case: a network that is up but can't reach
	 *  anything (captive portal, VPN down), where the browser still reports online. */
	const TRANSPORT = /error sending request|dns error|failed to lookup address|network is unreachable|connection refused|no route to host|connection reset/i;
	const offline = $derived(!online || TRANSPORT.test(message));
</script>

<svelte:window ononline={() => (online = true)} onoffline={() => (online = false)} />

<div class="flex max-w-md flex-col items-start gap-3">
	<div class="flex items-center gap-2.5">
		<span class="flex size-9 items-center justify-center rounded-full bg-foreground/8 text-muted-foreground">
			<HugeiconsIcon icon={offline ? WifiOff01Icon : Alert02Icon} class="size-4.5" />
		</span>
		<div>
			<p class="text-sm font-medium">
				{offline ? t('errors.offline_title') : t('errors.generic_title')}
			</p>
			<p class="text-xs text-muted-foreground">
				{offline ? t('errors.offline_desc') : message}
			</p>
		</div>
	</div>
	<div class="flex flex-wrap gap-2">
		<!-- shadcn's Button spreads onclick straight onto the real <button>, so passing `onRetry` directly
		     would hand the callback a MouseEvent — any retry handler with an optional parameter (like
		     home's `load`) would silently receive it as an argument instead of its default. -->
		<Button variant="outline" size="sm" class="gap-2" onclick={() => onRetry()}>
			<HugeiconsIcon icon={RefreshIcon} class="h-4 w-4" />
			{t('common.try_again')}
		</Button>
		{#if offline}
			<Button variant="ghost" size="sm" class="gap-2" onclick={() => goto('/library?tab=downloaded')}>
				<HugeiconsIcon icon={Download04Icon} class="h-4 w-4" />
				{t('errors.offline_downloads')}
			</Button>
		{/if}
	</div>
</div>
