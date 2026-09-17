<script lang="ts">
	// The equalizer as a curve you draw on. Ten points on a smooth line, one per band; press
	// anywhere and the band under the pointer goes to that height, and keep the button down to sweep
	// across the others. Replaces ten vertical range inputs, which could only be moved one at a time
	// and, at a whole dB per step, couldn't hold an AutoEq correction (they come in tenths).
	//
	// The line is a Catmull-Rom spline through the band gains, so a handle always sits on it. It is a
	// picture of the settings, not a measured frequency response: neighbouring Q 1.41 bands overlap,
	// and two +6 dB bands side by side sum to a little more than +6 between them. Drawing the real
	// response would put the handles off the line they drag, which reads as broken.
	//
	// Keyboard: every handle is a slider. Up/Down (or Right/Left) move it 0.5 dB, Page Up/Down 3,
	// Home/End to the limits.
	import { untrack } from 'svelte';
	import { Tween } from 'svelte/motion';
	import { cubicOut } from 'svelte/easing';
	import { GAIN_RANGE, bandLabel, formatDb } from '$lib/eq';

	let {
		bands,
		gains = $bindable(),
		onchange
	}: {
		bands: number[];
		gains: number[];
		/** After any change made here. The owner debounces. */
		onchange: () => void;
	} = $props();

	const H = 200;
	/** Left gutter for the dB scale. */
	const GUTTER = 32;
	/** Room above +12 and below −12 so a handle at the limit isn't cut in half. */
	const PAD_Y = 12;
	const GRID = [12, 6, 0, -6, -12];

	let svg = $state<SVGSVGElement | null>(null);
	let width = $state(0);
	let dragging = $state(false);
	let hovered = $state<number | null>(null);
	let focused = $state<number | null>(null);
	/** The band whose value is shown: the one being dragged, else focused, else under the pointer. */
	let pressed = $state<number | null>(null);
	const shownBand = $derived(pressed ?? focused ?? hovered);

	const reducedMotion =
		typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
	// Eased when the whole curve changes at once (a preset, a correction, Flatten), immediate while
	// it is being drawn: a line that lags the pointer is one you can't place.
	const shown = new Tween<number[]>([], { duration: 250, easing: cubicOut });
	$effect(() => {
		const next = gains.slice();
		const instant = dragging || reducedMotion;
		// Untracked: reading the tween here would re-run this every frame of its own animation,
		// and each re-run restarts it, so it would never arrive.
		untrack(() => {
			const first = shown.current.length !== next.length;
			shown.set(next, { duration: instant || first ? 0 : 250 });
		});
	});
	const uid = $props.id();
	const clipId = `eq-plot-${uid}`;

	const plotW = $derived(Math.max(0, width - GUTTER));
	const xOf = (i: number) => GUTTER + ((i + 0.5) / bands.length) * plotW;
	const yOf = (g: number) => PAD_Y + ((GAIN_RANGE - g) / (2 * GAIN_RANGE)) * (H - 2 * PAD_Y);

	/** Catmull-Rom through the points as cubic Béziers, run flat out to both edges of the plot. */
	const line = $derived.by(() => {
		const g = shown.current;
		if (!g.length || !width) return '';
		const p = g.map((v, i) => [xOf(i), yOf(v)] as const);
		let d = `M ${GUTTER} ${p[0][1]} L ${p[0][0]} ${p[0][1]}`;
		for (let i = 0; i < p.length - 1; i++) {
			const p0 = p[i - 1] ?? p[i];
			const p1 = p[i];
			const p2 = p[i + 1];
			const p3 = p[i + 2] ?? p2;
			const c1x = p1[0] + (p2[0] - p0[0]) / 6;
			const c1y = p1[1] + (p2[1] - p0[1]) / 6;
			const c2x = p2[0] - (p3[0] - p1[0]) / 6;
			const c2y = p2[1] - (p3[1] - p1[1]) / 6;
			d += ` C ${c1x} ${c1y} ${c2x} ${c2y} ${p2[0]} ${p2[1]}`;
		}
		return `${d} L ${width} ${p[p.length - 1][1]}`;
	});
	const area = $derived(line && `${line} L ${width} ${yOf(0)} L ${GUTTER} ${yOf(0)} Z`);

	const clampGain = (v: number) => Math.min(GAIN_RANGE, Math.max(-GAIN_RANGE, Math.round(v * 10) / 10));

	function set(i: number, v: number) {
		const g = clampGain(v);
		if (gains[i] === g) return;
		gains[i] = g;
		onchange();
	}

	function bandAt(clientX: number): number {
		const r = svg!.getBoundingClientRect();
		const i = Math.floor(((clientX - r.left - GUTTER) / plotW) * bands.length);
		return Math.min(bands.length - 1, Math.max(0, i));
	}

	function drawAt(e: PointerEvent) {
		const r = svg!.getBoundingClientRect();
		const i = bandAt(e.clientX);
		let g = GAIN_RANGE - ((e.clientY - r.top - PAD_Y) / (H - 2 * PAD_Y)) * 2 * GAIN_RANGE;
		// A detent at 0 dB: landing exactly flat by hand is otherwise a one-pixel target.
		if (Math.abs(g) < 0.4) g = 0;
		pressed = i;
		set(i, g);
	}

	function onpointerdown(e: PointerEvent) {
		if (e.button !== 0 || !svg) return;
		e.preventDefault();
		svg.setPointerCapture(e.pointerId);
		dragging = true;
		drawAt(e);
	}
	function onpointermove(e: PointerEvent) {
		if (!svg) return;
		if (dragging) drawAt(e);
		else hovered = bandAt(e.clientX);
	}
	function endDrag() {
		dragging = false;
		pressed = null;
	}

	function onkeydown(e: KeyboardEvent, i: number) {
		const step: Record<string, number> = {
			ArrowUp: 0.5,
			ArrowRight: 0.5,
			ArrowDown: -0.5,
			ArrowLeft: -0.5,
			PageUp: 3,
			PageDown: -3
		};
		if (e.key in step) set(i, gains[i] + step[e.key]);
		else if (e.key === 'Home') set(i, -GAIN_RANGE);
		else if (e.key === 'End') set(i, GAIN_RANGE);
		else return;
		e.preventDefault();
	}
</script>

<div class="select-none" bind:clientWidth={width}>
	<svg
		bind:this={svg}
		width={width}
		height={H}
		class="block cursor-crosshair touch-none overflow-visible"
		role="group"
		{onpointerdown}
		{onpointermove}
		onpointerup={endDrag}
		onpointercancel={endDrag}
		onpointerleave={() => (hovered = null)}
	>
		<defs>
			<clipPath id={clipId}>
				<rect x={GUTTER} y={0} width={plotW} height={H} />
			</clipPath>
		</defs>

		{#each GRID as db (db)}
			<line
				x1={GUTTER}
				x2={width}
				y1={yOf(db)}
				y2={yOf(db)}
				class={db === 0 ? 'stroke-foreground/25' : 'stroke-foreground/8'}
				stroke-dasharray={db === 0 ? undefined : '2 4'}
			/>
			<text
				x={GUTTER - 8}
				y={yOf(db)}
				text-anchor="end"
				dominant-baseline="middle"
				class="fill-muted-foreground text-[10px] tabular-nums"
			>
				{db > 0 ? `+${db}` : db}
			</text>
		{/each}
		{#each bands as hz, i (hz)}
			<line
				x1={xOf(i)}
				x2={xOf(i)}
				y1={PAD_Y}
				y2={H - PAD_Y}
				class="transition-colors duration-[var(--duration-quick)] {shownBand === i
					? 'stroke-foreground/15'
					: 'stroke-foreground/5'}"
			/>
		{/each}

		<g clip-path="url(#{clipId})" class="pointer-events-none">
			<path d={area} class="fill-primary/12" />
			<path d={line} class="fill-none stroke-primary" stroke-width="2" stroke-linejoin="round" />
		</g>

		{#each bands as hz, i (hz)}
			{@const g = shown.current[i] ?? 0}
			<circle
				cx={xOf(i)}
				cy={yOf(g)}
				r={shownBand === i ? 7 : 5}
				class="fill-background stroke-primary outline-none transition-[r] duration-[var(--duration-quick)]"
				stroke-width="2"
				tabindex="0"
				role="slider"
				aria-label="{bandLabel(hz)} Hz"
				aria-valuemin={-GAIN_RANGE}
				aria-valuemax={GAIN_RANGE}
				aria-valuenow={gains[i]}
				aria-valuetext="{formatDb(gains[i] ?? 0)} dB"
				onfocus={() => (focused = i)}
				onblur={() => (focused = null)}
				onkeydown={(e) => onkeydown(e, i)}
			/>
		{/each}
		{#if shownBand !== null && width}
			{@const g = shown.current[shownBand] ?? 0}
			<!-- Above the handle, or below it when the handle is near the top. -->
			<text
				x={Math.min(width - 16, Math.max(GUTTER + 16, xOf(shownBand)))}
				y={yOf(g) < 34 ? yOf(g) + 22 : yOf(g) - 14}
				text-anchor="middle"
				class="pointer-events-none fill-foreground text-[11px] font-medium tabular-nums"
			>
				{formatDb(gains[shownBand] ?? 0)}
			</text>
		{/if}
	</svg>
	<div class="mt-1 flex" style="padding-left: {GUTTER}px">
		{#each bands as hz, i (hz)}
			<span
				class="flex-1 text-center text-[10px] tabular-nums transition-colors {shownBand === i
					? 'text-foreground'
					: 'text-muted-foreground'}"
			>
				{bandLabel(hz)}
			</span>
		{/each}
	</div>
</div>

<style>
	circle:focus-visible {
		stroke-width: 3;
	}
</style>
