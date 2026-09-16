<script lang="ts">
	import { Tooltip as TooltipPrimitive } from "bits-ui";

	// 80ms of intent (--duration-micro), not 0: a cursor crossing a toolbar on its way somewhere
	// else shouldn't light up every button it passes. Short enough that a deliberate hover still
	// feels immediate.
	let { delayDuration = 80, ...restProps }: TooltipPrimitive.ProviderProps = $props();
</script>

<TooltipPrimitive.Provider {delayDuration} {...restProps} />
