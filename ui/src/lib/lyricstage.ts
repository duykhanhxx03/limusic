/**
 * Word-synced lyrics on a WebGL stage (PixiJS), for the views with room for them.
 *
 * Why not the DOM, which drew these until 2026-09: WebKitGTK rasterises text on the pixel grid
 * every time it paints it. A line gliding by fractions of a pixel, a word lifting by two, a line
 * scaling by 4% — each frame lands the glyphs on a different pixel row, so they step instead of
 * moving, and the lyrics read as trembling. Apple Music has no such problem because it never
 * re-draws text while it moves: a line is a layer, drawn once, moved by the compositor.
 *
 * This does the same. Every word is drawn once, into a per-line texture atlas. After that, motion
 * is geometry (a spring per line, a lift per word), the karaoke sweep is a fragment shader with a
 * soft edge, depth is a blur filter, and nothing is rasterised again until the layout changes. At
 * rest every position is snapped to the device pixel grid, so still text is as sharp as the DOM's.
 *
 * Every line but the sung one goes one step further: it is drawn once more, blur and all, into a
 * texture of its own, redrawn only when something inside it changes. At rest it costs one textured
 * quad a frame instead of a draw call per word and a blur per frame.
 *
 * Time comes from the same `MediaClock` the DOM view uses; which line is sung, and whether an
 * interlude is on, is still decided by `LyricsView` and handed in. This file only draws.
 */
import {
	Application,
	BlurFilter,
	CanvasSource,
	Container,
	GlProgram,
	Graphics,
	Mesh,
	MeshGeometry,
	Rectangle,
	RendererType,
	Shader,
	Sprite,
	Texture,
	Ticker,
	UniformGroup
} from 'pixi.js';
import type { LyricLine } from '$lib/api';
import type { MediaClock } from '$lib/mediaclock';

export type Interlude = { at: number; from: number; to: number };

export interface StageOptions {
	/** Positioned element the canvas fills. Its `color` is the text colour. */
	host: HTMLElement;
	clock: MediaClock;
	expanded: boolean;
	/** The lyrics page that covers the main panel: a left-aligned column of readable width, in
	 *  the side panel's type rather than theater's. */
	page?: boolean;
	/** A line was clicked. */
	onSeek: (index: number) => void;
	/** The GPU went away (context lost). The owner should fall back to the DOM view. */
	onLost: () => void;
}

// --- look -----------------------------------------------------------------------------------

/** Unsung words on the sung line, as a fraction of full colour. */
const DIM = 0.5;
/** Soft edge of the sweep, each side of the edge, in em. */
const BAND = 0.35;
/** How far a word rises as it is sung, in em. */
const LIFT = 0.05;
/** A word held this long glows while it is held. */
const HELD_MS = 900;
/** Lines below the sung one start moving this much later each, capped at `STAGGER_CAP` lines. */
const STAGGER_MS = 45;
const STAGGER_CAP = 8;
/** Inactive lines in the big view sit back a little. */
const INACTIVE_SCALE = 0.97;

function lineAlpha(distance: number): number {
	return Math.max(0.18, 0.42 - (distance - 1) * 0.07);
}

function lineBlur(distance: number): number {
	return Math.min(distance * 0.7, 2.8);
}

// --- shader ---------------------------------------------------------------------------------
//
// Pixi's own mesh shader plus three uniforms. `vX` is the fragment's distance from the start of
// the word in CSS px, so the edge can be placed in the same units the layout was measured in.

const VERTEX = `
in vec2 aPosition;
in vec2 aUV;
out vec2 vUV;
out float vX;
out vec4 vColor;
uniform mat3 uProjectionMatrix;
uniform mat3 uWorldTransformMatrix;
uniform vec4 uWorldColorAlpha;
uniform mat3 uTransformMatrix;
uniform vec4 uColor;
void main() {
	mat3 mvp = uProjectionMatrix * uWorldTransformMatrix * uTransformMatrix;
	gl_Position = vec4((mvp * vec3(aPosition, 1.0)).xy, 0.0, 1.0);
	vUV = aUV;
	vX = aPosition.x;
	vColor = uWorldColorAlpha * uColor;
}`;

const FRAGMENT = `
in vec2 vUV;
in float vX;
in vec4 vColor;
out vec4 finalColor;
uniform sampler2D uTexture;
uniform float uEdge;
uniform float uBand;
uniform float uDim;
void main() {
	float lit = 1.0 - smoothstep(uEdge - uBand, uEdge + uBand, vX);
	finalColor = texture(uTexture, vUV) * vColor * mix(uDim, 1.0, lit);
}`;

let program: GlProgram | null = null;
const sweepProgram = () => (program ??= GlProgram.from({ vertex: VERTEX, fragment: FRAGMENT, name: 'lyric-sweep' }));

// --- motion helpers -------------------------------------------------------------------------

/** A damped spring toward a target that can be told to wait before following a new one. */
class Spring {
	value: number;
	velocity = 0;
	target: number;
	private pending: { target: number; at: number } | null = null;

	constructor(value: number) {
		this.value = this.target = value;
	}

	/** Follow `target` from wall time `at` (ms). */
	aim(target: number, at = 0) {
		if (at <= 0) {
			this.pending = null;
			this.target = target;
		} else {
			this.pending = { target, at };
		}
	}

	snap(value: number) {
		this.value = this.target = value;
		this.velocity = 0;
		this.pending = null;
	}

	get resting(): boolean {
		return !this.pending && this.value === this.target && this.velocity === 0;
	}

	/** Advance `dt` seconds. Stiffness 90, damping 15: settles in about half a second with an
	 *  overshoot of a percent or two, which is the give Apple's lines have. */
	step(now: number, dt: number) {
		if (this.pending && now >= this.pending.at) {
			this.target = this.pending.target;
			this.pending = null;
		}
		if (this.value === this.target && this.velocity === 0) return;
		let left = dt;
		while (left > 0) {
			const h = Math.min(left, 1 / 240);
			this.velocity += (-90 * (this.value - this.target) - 15 * this.velocity) * h;
			this.value += this.velocity * h;
			left -= h;
		}
		if (Math.abs(this.value - this.target) < 0.01 && Math.abs(this.velocity) < 0.05) {
			this.value = this.target;
			this.velocity = 0;
		}
	}
}

/** Exponential approach with time constant `tau` ms; lands exactly once close enough. */
function approach(value: number, target: number, dtMs: number, tau: number, eps = 0.002): number {
	if (value === target) return value;
	const next = value + (target - value) * (1 - Math.exp(-dtMs / tau));
	return Math.abs(next - target) < eps ? target : next;
}

/** `letterSpacing` on a 2D context, where the engine has it (the CSS view uses `tracking`). */
function setSpacing(ctx: CanvasRenderingContext2D, px: number) {
	if ('letterSpacing' in ctx) (ctx as CanvasRenderingContext2D & { letterSpacing: string }).letterSpacing = `${px}px`;
}

/** The smallest whole number of CSS px that is also a whole number of device px: 1 at a ratio of 1
 *  or 2, 4 at 1.25, 5 at the app's zoom steps of 0.2. 0 when no step small enough exists. */
function pixelGrid(dpr: number): number {
	for (let k = 1; k <= 16; k++) if (Math.abs(k * dpr - Math.round(k * dpr)) < 1e-3) return k;
	return 0;
}

const clamp01 = (x: number) => (x < 0 ? 0 : x > 1 ? 1 : x);
const easeOutCubic = (t: number) => 1 - Math.pow(1 - t, 3);
/** CSS `ease-in-out`, near enough: the app's token for a content swap. */
const easeInOut = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);

/** Changing lyrics (a new track): the old set goes quickly and quietly, the new one is revealed —
 *  the transition tokens for a close (`--duration-quick`) and a content reveal (`--duration-slow`,
 *  8px of travel). A close faster than the open is the rule, not a preference. */
const SWAP_OUT_MS = 150;
const SWAP_IN_MS = 400;
const SWAP_RISE = 8;

/** Rows rasterised per frame while new lyrics are still all but invisible: laying out a whole
 *  set at once cost one 15–50 ms frame, and a row arriving a frame late is unseen at that opacity. */
const BUILD_BUDGET = 5;
/** A resize that rewraps the lines does so at most this often; in between, the old layout stays.
 *  Rewrapping rebuilds every row on screen, which a window or panel drag asked for every frame. */
const REWRAP_MS = 200;
/** The interlude dots, when nothing else moves, redraw this often rather than every display frame:
 *  their fill and breath move them a quarter of a pixel at most in that time, too little to see
 *  the steps, and a sung word or a scroll still gets every frame. */
const DOTS_MS = 50;

const reducedMotion =
	typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

// --- layout types ---------------------------------------------------------------------------

type Token = {
	text: string;
	translation: boolean;
	/** Media ms; `timed` false for line-synced lyrics and translations. */
	start: number;
	end: number;
	timed: boolean;
	/** Position of the word's box inside the row's text area, CSS px, placed on device pixels in
	 *  the row (`tokenize`). */
	x: number;
	y: number;
	w: number;
	mesh?: Mesh<MeshGeometry, Shader>;
	sweep?: UniformGroup;
	glow?: Sprite;
	lift: number;
};

type Row = {
	/** Line index; for the dots, the line they precede. */
	index: number;
	dots: boolean;
	tokens: Token[];
	height: number;
	/** Layout top in content coordinates, CSS px. */
	top: number;
	/** Placed, scaled and faded as a whole; the unit a row is cached as (`settle`). */
	node: Container;
	/** What the row draws, one level inside `node`. The blur sits here rather than on `node` so
	 *  a cached row keeps it baked into its texture instead of blurring that texture every frame. */
	body: Container;
	/** `node` is drawn from a texture of itself. */
	cached: boolean;
	/** Something drawn inside the row changed this frame, so a cached row redraws its texture. */
	dirty: boolean;
	built: boolean;
	sources: CanvasSource[];
	y: Spring;
	scale: Spring;
	alpha: number;
	blur: number;
	dim: number;
	filter: BlurFilter | null;
	hover: boolean;
	/** Dots only. */
	dotShapes?: Graphics[];
	dotGroup?: Container;
	born: number;
};

type Metrics = {
	dpr: number;
	size: number;
	family: string;
	transFamily: string;
	lineH: number;
	transSize: number;
	transLineH: number;
	padY: number;
	/** Letter spacing of the lyric type, CSS px (`tracking-[-0.02em]` once it is display-sized). */
	tracking: number;
	colX: number;
	colW: number;
	top: number;
	bottom: number;
	bias: number;
	ascent: number;
	transAscent: number;
	cellPad: number;
	dot: number;
	dotGap: number;
	/** `pixelGrid(dpr)`: where a row's cache texture may start so that it lines up with the
	 *  screen's pixels. 0 turns caching off. */
	grid: number;
};

/** What the next frame is for: nothing, only the interlude dots (a slower one will do), or motion. */
type Pace = 'rest' | 'dots' | 'full';

export class LyricStage {
	static async create(opts: StageOptions): Promise<LyricStage> {
		// Pixi's global system ticker is not the app's ticker stopped below. The renderer's
		// scheduler (which drives its GPU garbage collector) and the event system's hover poller
		// both add listeners to it during `init`, and it starts itself on the first one: a
		// requestAnimationFrame every frame for as long as a stage lives, paused or not, which
		// keeps the page awake at 60 Hz and defeats the sleep in `frame`. Kept stopped, it is
		// advanced by `frame` instead, only while something is drawn. The poller loses nothing:
		// it only re-tests `dynamic` targets under a still pointer, and the rows are `static`.
		// Hover and click-to-seek come from the DOM pointer events the event system listens to.
		Ticker.system.autoStart = false;
		Ticker.system.stop();
		const app = new Application();
		const { clientWidth: w, clientHeight: h } = opts.host;
		await app.init({
			preference: 'webgl',
			backgroundAlpha: 0,
			antialias: false,
			autoDensity: true,
			resolution: window.devicePixelRatio || 1,
			width: Math.max(1, w),
			height: Math.max(1, h),
			autoStart: false,
			sharedTicker: false
		});
		app.ticker?.stop();
		return new LyricStage(app, opts);
	}

	private readonly app: Application;
	private readonly host: HTMLElement;
	private readonly clock: MediaClock;
	private readonly onSeek: (index: number) => void;
	private readonly onLost: () => void;
	private expanded: boolean;
	private page: boolean;

	private lines: LyricLine[] = [];
	private rows: Row[] = [];
	private lineRows: Row[] = [];
	private leaving: Row[] = [];
	private active = -1;
	private interlude: Interlude | null = null;
	private m!: Metrics;
	private color = 0xffffff;

	private scroll = 0;
	private browsing = false;
	private browseTimer: ReturnType<typeof setTimeout> | undefined;

	/** Lines waiting for the ones on screen to fade out. */
	private pending: LyricLine[] | null = null;
	private swap: { phase: 'out' | 'in'; at: number } | null = null;

	private frameId = 0;
	private lastTs = 0;
	/** The slower frame the interlude dots asked for (`DOTS_MS`); `kick` replaces it. */
	private dotsTimer: ReturnType<typeof setTimeout> | undefined;
	/** A rewrap a resize put off (`REWRAP_MS`), and when the last one ran. */
	private rewrapTimer: ReturnType<typeof setTimeout> | undefined;
	private rewrapAt = 0;
	private readonly measurer = document.createElement('canvas').getContext('2d')!;
	private readonly resizeObserver: ResizeObserver;
	private readonly themeObserver: MutationObserver;
	private dead = false;

	private constructor(app: Application, opts: StageOptions) {
		this.app = app;
		this.host = opts.host;
		this.clock = opts.clock;
		this.onSeek = opts.onSeek;
		this.onLost = opts.onLost;
		this.expanded = opts.expanded;
		this.page = opts.page ?? false;

		const canvas = app.canvas;
		canvas.style.position = 'absolute';
		canvas.style.inset = '0';
		canvas.style.width = '100%';
		canvas.style.height = '100%';
		this.host.appendChild(canvas);
		canvas.addEventListener('webglcontextlost', this.lost);
		this.host.addEventListener('wheel', this.onWheel, { passive: true });

		this.readTheme();
		this.m = this.metrics();
		this.resizeObserver = new ResizeObserver(() => this.resize());
		this.resizeObserver.observe(this.host);
		// Theme switches restyle <html>; the text colour and the heading font follow them.
		this.themeObserver = new MutationObserver(() => {
			const before = `${this.color}|${this.m.family}`;
			this.readTheme();
			const m = this.metrics();
			if (m.family !== this.m.family) {
				this.m = m;
				this.relayout(false);
			} else if (`${this.color}|${this.m.family}` !== before) {
				for (const row of this.rows) this.tint(row);
				this.kick();
			}
		});
		this.themeObserver.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['class', 'style', 'data-theme']
		});
	}

	destroy() {
		if (this.dead) return;
		this.dead = true;
		cancelAnimationFrame(this.frameId);
		clearTimeout(this.browseTimer);
		clearTimeout(this.dotsTimer);
		clearTimeout(this.rewrapTimer);
		this.resizeObserver.disconnect();
		this.themeObserver.disconnect();
		this.host.removeEventListener('wheel', this.onWheel);
		this.app.canvas.removeEventListener('webglcontextlost', this.lost);
		for (const row of [...this.rows, ...this.leaving]) this.dropRow(row);
		this.app.destroy({ removeView: true }, { children: true });
	}

	// --- inputs from the view -----------------------------------------------------------------

	setLines(lines: LyricLine[]) {
		if (lines === (this.pending ?? this.lines)) return;
		this.active = -1;
		this.interlude = null;
		if (this.rows.length && !reducedMotion) {
			// Something is on screen: it leaves first, and the new lines are laid out once it has.
			this.pending = lines;
			if (this.swap?.phase !== 'out') this.swap = { phase: 'out', at: performance.now() };
			this.kick();
			return;
		}
		this.showLines(lines);
	}

	private showLines(lines: LyricLine[]) {
		this.pending = null;
		this.lines = lines;
		this.relayout(false);
		this.swap = lines.length && !reducedMotion ? { phase: 'in', at: performance.now() } : null;
		this.kick();
	}

	setActive(active: number, interlude: Interlude | null) {
		if (this.pending) {
			// These indices are into the lines not laid out yet. Applied to the rows still fading
			// out they would light the wrong line, so keep them for the layout the swap does.
			this.active = active;
			this.interlude = interlude;
			return;
		}
		const gapChanged = (interlude?.at ?? -1) !== (this.interlude?.at ?? -1);
		if (active === this.active && !gapChanged) return;
		this.active = active;
		this.interlude = interlude;
		if (gapChanged) this.placeDots();
		if (this.browsing) for (const row of this.rows) row.y.aim(row.top - this.scroll);
		else this.focus(true);
		this.kick();
	}

	setExpanded(expanded: boolean) {
		if (expanded === this.expanded) return;
		this.expanded = expanded;
		this.m = this.metrics();
		this.relayout(false);
	}

	/** The clock moved (a report, a seek, pause or resume). */
	wake() {
		this.kick();
	}

	// --- theme and metrics --------------------------------------------------------------------

	private readTheme() {
		const probe = document.createElement('canvas').getContext('2d')!;
		probe.fillStyle = getComputedStyle(this.host).color;
		probe.fillRect(0, 0, 1, 1);
		const [r, g, b] = probe.getImageData(0, 0, 1, 1).data;
		this.color = (r << 16) | (g << 8) | b;
	}

	private metrics(): Metrics {
		const dpr = window.devicePixelRatio || 1;
		const expanded = this.expanded;
		const page = this.page && !expanded;
		const width = this.host.clientWidth;
		const sidePad = expanded ? 40 : page ? 56 : 20;
		const colW = Math.max(
			120,
			Math.min(width - sidePad * 2, expanded ? 768 : page ? 640 : Infinity)
		);
		// Expanded (theater, and the player view's enlarged lyrics) keeps the DOM view's type:
		// `text-[clamp(1.75rem,3.2vw,2.75rem)]`. Everywhere else the type follows the column it is set
		// in: 20px in the player view's lyrics column at a 900px window, 24px at 1280, 32px (the cap)
		// at 1920 — well under theater's proportion (0.068 of its column), which wrapped most lines
		// three deep at the player view's width. 20px is also the floor a 320px side panel sits on.
		// The page keeps close to the side panel's 20px however wide the column is: lyrics to read
		// along with, not a poster.
		const size = expanded
			? Math.min(44, Math.max(28, window.innerWidth * 0.032))
			: page
				? 22
				: Math.round(Math.min(32, Math.max(20, colW * 0.052)));
		const big = expanded || size >= 26;
		const lineH = size * (big ? 1.18 : 1.375);

		const probe = document.createElement('span');
		probe.className = 'font-heading';
		this.host.appendChild(probe);
		const family = getComputedStyle(probe).fontFamily;
		probe.remove();
		const transFamily = getComputedStyle(this.host).fontFamily;

		const ctx = this.measurer;
		ctx.font = `700 ${size}px ${family}`;
		const fm = ctx.measureText('Hg');
		const asc = fm.fontBoundingBoxAscent ?? size * 0.8;
		const desc = fm.fontBoundingBoxDescent ?? size * 0.2;
		ctx.font = `italic 400 14px ${transFamily}`;
		const tm = ctx.measureText('Hg');
		const tAsc = tm.fontBoundingBoxAscent ?? 11;
		const tDesc = tm.fontBoundingBoxDescent ?? 3;

		return {
			dpr,
			size,
			family,
			transFamily,
			lineH,
			transSize: 14,
			transLineH: 20,
			padY: big ? 12 : 8,
			tracking: big ? -0.02 * size : 0,
			// The page sets its column from the left edge, as Spotify's does; the rest centre it.
			colX: page ? sidePad : Math.round(((width - colW) / 2) * dpr) / dpr,
			colW,
			top: 24,
			bottom: window.innerHeight * 0.55,
			bias: expanded || page ? 0.38 : 0.5,
			// Half-leading, as CSS places a glyph run in its line box.
			ascent: (lineH - (asc + desc)) / 2 + asc,
			transAscent: (20 - (tAsc + tDesc)) / 2 + tAsc,
			// Rounded up to whole device pixels: a word's cell starts this far before the word, and at
			// a ratio like 1.25 a padding of 21 px (26.25 device px) put every glyph between pixels,
			// drawn soft even at rest and softer again through a cached row's texture.
			cellPad: Math.ceil(Math.ceil(size * 0.5) * dpr) / dpr,
			dot: expanded ? 12 : 8,
			dotGap: expanded ? 10 : 8,
			grid: pixelGrid(dpr)
		};
	}

	private resize() {
		const { clientWidth: w, clientHeight: h } = this.host;
		if (!w || !h) return;
		const m = this.metrics();
		this.app.renderer.resize(w, h, m.dpr);
		const old = this.m;
		const same =
			m.size === old.size && m.colW === old.colW && m.dpr === old.dpr && m.family === old.family;
		if (same) {
			clearTimeout(this.rewrapTimer);
			this.rewrapTimer = undefined;
			this.m = m;
			this.focus(false);
			this.kick();
			return;
		}
		const wait = this.rewrapAt + REWRAP_MS - performance.now();
		if (wait <= 0) {
			this.m = m;
			this.relayout(false);
			return;
		}
		// A drag resizes every frame, and a rewrap rebuilds every row on screen. Until the next one
		// is due the rows keep the layout they were built for, only moved to stay centred in the
		// new width (the page's column hangs from its left edge, so it stays put).
		const centred = !this.page || this.expanded;
		this.m = {
			...old,
			colX: centred ? Math.round(((w - old.colW) / 2) * old.dpr) / old.dpr : old.colX,
			bottom: m.bottom
		};
		this.focus(false);
		this.kick();
		this.rewrapTimer ??= setTimeout(() => {
			this.rewrapTimer = undefined;
			if (!this.dead) this.resize();
		}, wait);
	}

	// --- layout ---------------------------------------------------------------------------------

	private relayout(animate: boolean) {
		// A rewrap a resize put off happens here instead, with the metrics it was waiting for; the
		// ones in `this.m` meanwhile are the old layout's.
		if (this.rewrapTimer !== undefined) {
			clearTimeout(this.rewrapTimer);
			this.rewrapTimer = undefined;
			this.m = this.metrics();
		}
		this.rewrapAt = performance.now();
		for (const row of [...this.rows, ...this.leaving]) this.dropRow(row);
		this.rows = [];
		this.leaving = [];
		this.lineRows = this.lines.map((line, i) => {
			const { tokens, height } = this.tokenize(line);
			return this.makeRow(i, tokens, height);
		});
		this.rows = [...this.lineRows];
		if (this.interlude) this.placeDots();
		this.stack();
		this.focus(animate);
		this.kick();
	}

	private tokenize(line: LyricLine): { tokens: Token[]; height: number } {
		const m = this.m;
		const ctx = this.measurer;
		const tokens: Token[] = [];
		// Wrap units: a word, or syllables with no space between them, which must stay together.
		const units: Token[][] = [];
		let unit: Token[] = [];
		const push = (t: Token, breakAfter: boolean) => {
			unit.push(t);
			if (breakAfter) {
				units.push(unit);
				unit = [];
			}
		};

		ctx.font = `700 ${m.size}px ${m.family}`;
		setSpacing(ctx, m.tracking);
		const space = m.size * 0.26;
		const words = line.words?.length
			? line.words.map((w) => ({ text: w.text, start: w.start_ms, end: w.end_ms, timed: true }))
			: (line.text || '♪')
					.split(/(?<= )/)
					.map((text) => ({ text, start: 0, end: 0, timed: false }));
		for (const w of words) {
			const text = w.text.trimEnd();
			if (!text) continue;
			push(
				{
					text,
					translation: false,
					start: w.start,
					end: w.end,
					timed: w.timed,
					x: 0,
					y: 0,
					w: ctx.measureText(text).width,
					lift: 0
				},
				w.text.endsWith(' ')
			);
		}
		if (unit.length) units.push(unit);

		let x = 0;
		let y = 0;
		// Every word on the device pixel grid, down as well as across: a line height like 51.92 px
		// put a wrapped line and a translation between pixel rows, so a cached row resampled them
		// once into its texture and again drawing it scaled or moving, softer than the line above.
		// Down, it is the word's place in the row that lands on the grid: the text area starts `padY`
		// below the row's top, which at a ratio like 1.2 is between pixels itself. Only the words
		// move; the rows keep their measured height, and the layout its place.
		const snap = (v: number) => Math.round(v * m.dpr) / m.dpr;
		const snapY = (v: number) => snap(m.padY + v) - m.padY;
		for (const u of units) {
			const width = u.reduce((sum, t) => sum + t.w, 0);
			if (x > 0 && x + width > m.colW) {
				x = 0;
				y += m.lineH;
			}
			for (const t of u) {
				t.x = snap(x);
				t.y = snapY(y);
				x += t.w;
				tokens.push(t);
			}
			x += space;
		}
		let height = y + m.lineH;

		if (line.translation) {
			ctx.font = `italic 400 ${m.transSize}px ${m.transFamily}`;
			setSpacing(ctx, 0);
			x = 0;
			y = height + 4;
			const tSpace = ctx.measureText(' ').width;
			for (const part of line.translation.split(' ')) {
				if (!part) continue;
				const w = ctx.measureText(part).width;
				if (x > 0 && x + w > m.colW) {
					x = 0;
					y += m.transLineH;
				}
				tokens.push({
					text: part,
					translation: true,
					start: 0,
					end: 0,
					timed: false,
					x: snap(x),
					y: snapY(y),
					w,
					lift: 0
				});
				x += w + tSpace;
			}
			height = y + m.transLineH;
		}
		return { tokens, height };
	}

	private makeRow(index: number, tokens: Token[], textH: number, dots = false): Row {
		const m = this.m;
		const node = new Container();
		const body = new Container();
		node.addChild(body);
		this.app.stage.addChild(node);
		const row: Row = {
			index,
			dots,
			tokens,
			height: dots ? m.dot + (this.expanded ? 32 : 16) : textH + m.padY * 2,
			top: 0,
			node,
			body,
			cached: false,
			dirty: false,
			built: false,
			sources: [],
			y: new Spring(0),
			scale: new Spring(1),
			alpha: 1,
			blur: 0,
			dim: 1,
			filter: null,
			hover: false,
			born: performance.now()
		};
		if (!dots) {
			node.eventMode = 'static';
			node.cursor = 'pointer';
			node.on('pointertap', () => this.onSeek(index));
			node.on('pointerover', () => {
				row.hover = true;
				this.kick();
			});
			node.on('pointerout', () => {
				row.hover = false;
				this.kick();
			});
		}
		node.visible = false;
		return row;
	}

	/** Content-coordinate tops, in order. */
	private stack() {
		let top = this.m.top;
		for (const row of this.rows) {
			row.top = top;
			top += row.height;
			const hit = row.node.hitArea as Rectangle | null;
			if (!row.dots && (!hit || hit.height !== row.height)) {
				row.node.hitArea = new Rectangle(0, 0, this.m.colW, row.height);
			}
		}
	}

	private contentHeight(): number {
		const last = this.rows[this.rows.length - 1];
		return (last ? last.top + last.height : this.m.top) + this.m.bottom;
	}

	private placeDots() {
		const current = this.rows.find((r) => r.dots);
		const gap = this.interlude;
		if (current && current.index === gap?.at) return;
		const scrollBefore = this.scroll;
		if (current) {
			this.rows = this.rows.filter((r) => r !== current);
			current.born = performance.now();
			this.leaving.push(current);
		}
		if (gap) {
			const dots = this.makeRow(gap.at, [], 0, true);
			const at = this.rows.findIndex((r) => !r.dots && r.index === gap.at);
			this.rows.splice(at < 0 ? this.rows.length : at, 0, dots);
		}
		this.stack();
		// A new row appears where it would have been before the list moves, so it rides the wave
		// in with the rest instead of materialising at its destination.
		for (const row of this.rows) if (row.dots && row !== current) row.y.snap(row.top - scrollBefore);
	}

	// --- focus and scroll -----------------------------------------------------------------------

	private anchorRow(): Row | undefined {
		const dots = this.interlude ? this.rows.find((r) => r.dots) : undefined;
		if (dots) return dots;
		return this.active >= 0 ? this.lineRows[this.active] : this.lineRows[0];
	}

	private focus(animate: boolean) {
		const target = this.anchorRow();
		const viewH = this.host.clientHeight;
		const max = Math.max(0, this.contentHeight() - viewH);
		if (target) {
			this.scroll = Math.min(max, Math.max(0, target.top - (viewH - target.height) * this.m.bias));
		}
		this.aimRows(animate ? target : undefined);
	}

	/** Send every row toward its place for the current scroll; with `anchor`, the rows below it
	 *  start a beat later each — Apple's wave. */
	private aimRows(anchor: Row | undefined) {
		const now = performance.now();
		const anchorAt = anchor ? this.rows.indexOf(anchor) : -1;
		for (let k = 0; k < this.rows.length; k++) {
			const row = this.rows[k];
			const place = row.top - this.scroll;
			if (!anchor) {
				row.y.snap(place);
				continue;
			}
			const below = k - anchorAt;
			row.y.aim(place, below > 0 ? now + Math.min(below, STAGGER_CAP) * STAGGER_MS : 0);
		}
	}

	private readonly onWheel = (e: WheelEvent) => {
		const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? this.host.clientHeight : 1;
		const max = Math.max(0, this.contentHeight() - this.host.clientHeight);
		this.scroll = Math.min(max, Math.max(0, this.scroll + e.deltaY * unit));
		this.browsing = true;
		clearTimeout(this.browseTimer);
		this.browseTimer = setTimeout(() => {
			this.browsing = false;
			this.focus(true);
			this.kick();
		}, 3000);
		for (const row of this.rows) row.y.aim(row.top - this.scroll);
		this.kick();
	};

	// --- building a row's textures ----------------------------------------------------------------

	private build(row: Row) {
		if (row.built) return;
		row.built = true;
		if (row.dots) {
			this.buildDots(row);
			return;
		}
		const m = this.m;
		const dpr = m.dpr;
		const pad = m.cellPad;
		const tokens = row.tokens;
		if (!tokens.length) return;

		// Pack every token into its own cell: words that touch in the layout (syllables) must not
		// sample each other's glyphs through their padding.
		const cells: { x: number; y: number; w: number; h: number }[] = [];
		const limit = Math.ceil((m.colW + pad * 2) * dpr);
		let cx = 0;
		let cy = 0;
		let rowH = 0;
		for (const t of tokens) {
			const lh = t.translation ? m.transLineH : m.lineH;
			const w = Math.ceil((t.w + pad * 2) * dpr);
			const h = Math.ceil((lh + pad * 2) * dpr);
			if (cx > 0 && cx + w > limit) {
				cx = 0;
				cy += rowH;
				rowH = 0;
			}
			cells.push({ x: cx, y: cy, w, h });
			cx += w;
			rowH = Math.max(rowH, h);
		}
		const width = Math.max(1, Math.min(limit, cells.reduce((a, c) => Math.max(a, c.x + c.w), 0)));
		const height = Math.max(1, cy + rowH);

		const held = tokens.some((t) => t.timed && t.end - t.start >= HELD_MS);
		const canvas = document.createElement('canvas');
		canvas.width = width;
		canvas.height = height;
		const glowCanvas = held ? document.createElement('canvas') : null;
		if (glowCanvas) {
			glowCanvas.width = width;
			glowCanvas.height = height;
		}
		const ctx = canvas.getContext('2d')!;
		const gctx = glowCanvas?.getContext('2d') ?? null;
		const px = Math.round(pad * dpr);
		for (let k = 0; k < tokens.length; k++) {
			const t = tokens[k];
			const c = cells[k];
			const font = t.translation
				? `italic 400 ${m.transSize * dpr}px ${m.transFamily}`
				: `700 ${m.size * dpr}px ${m.family}`;
			const baseline = Math.round((t.translation ? m.transAscent : m.ascent) * dpr);
			ctx.font = font;
			ctx.fillStyle = '#fff';
			setSpacing(ctx, t.translation ? 0 : m.tracking * dpr);
			ctx.fillText(t.text, c.x + px, c.y + px + baseline);
			if (gctx && t.timed && t.end - t.start >= HELD_MS) {
				gctx.font = font;
				setSpacing(gctx, t.translation ? 0 : m.tracking * dpr);
				gctx.fillStyle = '#fff';
				gctx.shadowColor = '#fff';
				gctx.shadowBlur = BAND * m.size * dpr;
				gctx.fillText(t.text, c.x + px, c.y + px + baseline);
			}
		}

		const source = new CanvasSource({ resource: canvas, resolution: 1 });
		const texture = new Texture({ source });
		row.sources.push(source);
		const glowSource = glowCanvas ? new CanvasSource({ resource: glowCanvas, resolution: 1 }) : null;
		if (glowSource) row.sources.push(glowSource);
		// Uploaded now, the canvases can let go of their pixels: kept, each is a second copy of the
		// row's atlas in memory for as long as the row is built. Nothing reads them again. These
		// sources are never updated or resized, texture sources are not garbage-collected unless
		// asked (`autoGarbageCollect`), and a lost context ends the stage (`onLost`) rather than
		// restoring it. WebGL only, where the upload copies the pixels.
		if (this.app.renderer.type === RendererType.WEBGL) {
			for (const s of row.sources) this.app.renderer.texture.initSource(s);
			for (const c of [canvas, glowCanvas]) if (c) c.width = c.height = 0;
		}

		// The row's drawn extent: every cell, padding and all, which is where a blur or a glow can
		// reach, and a word's lift above that. As `boundsArea` it is also the texture the row is
		// cached into (`settle`), so it lies on the pixel grid: starting on it, the words land in the
		// texture pixel for pixel as they would on screen, and a cached row at rest is the same image
		// as a live one; a whole number of device pixels across, a blur drawn into it is not
		// stretched (Pixi sizes the blur's output by the texture's size in CSS px, but its viewport
		// in whole pixels).
		if (m.grid) {
			const g = m.grid;
			let right = 0;
			let bottom = 0;
			for (let k = 0; k < tokens.length; k++) {
				right = Math.max(right, tokens[k].x - pad + cells[k].w / dpr);
				bottom = Math.max(bottom, m.padY + tokens[k].y - pad + cells[k].h / dpr);
			}
			const left = Math.floor(-pad / g) * g;
			const top = Math.floor((m.padY - pad - LIFT * m.size - 1) / g) * g;
			row.node.boundsArea = new Rectangle(
				left,
				top,
				Math.ceil((right - left) / g) * g,
				Math.ceil((bottom - top) / g) * g
			);
		}

		for (let k = 0; k < tokens.length; k++) {
			const t = tokens[k];
			const c = cells[k];
			const w = c.w / dpr;
			const h = c.h / dpr;
			const u0 = c.x / width;
			const v0 = c.y / height;
			const u1 = (c.x + c.w) / width;
			const v1 = (c.y + c.h) / height;
			const geometry = new MeshGeometry({
				positions: new Float32Array([-pad, -pad, w - pad, -pad, w - pad, h - pad, -pad, h - pad]),
				uvs: new Float32Array([u0, v0, u1, v0, u1, v1, u0, v1]),
				indices: new Uint32Array([0, 1, 2, 0, 2, 3])
			});
			const sweep = new UniformGroup({
				uEdge: { value: t.w + BAND * m.size * 2, type: 'f32' },
				uBand: { value: BAND * m.size, type: 'f32' },
				uDim: { value: 1, type: 'f32' }
			});
			const shader = new Shader({ glProgram: sweepProgram(), resources: { uTexture: source, sweep } });
			const mesh = new Mesh({ geometry, shader, texture });
			mesh.position.set(t.x, m.padY + t.y);
			if (t.translation) mesh.alpha = 0.8;
			t.mesh = mesh;
			t.sweep = sweep;

			if (glowSource && t.timed && t.end - t.start >= HELD_MS) {
				const glow = new Sprite(
					new Texture({ source: glowSource, frame: new Rectangle(c.x, c.y, c.w, c.h) })
				);
				glow.scale.set(1 / dpr);
				glow.position.set(t.x - pad, m.padY + t.y - pad);
				glow.alpha = 0;
				glow.visible = false;
				row.body.addChild(glow);
				t.glow = glow;
			}
			row.body.addChild(mesh);
		}
		this.tint(row);
	}

	private buildDots(row: Row) {
		const m = this.m;
		const group = new Container();
		const shapes: Graphics[] = [];
		const r = m.dot / 2;
		for (let k = 0; k < 3; k++) {
			const g = new Graphics().circle(0, 0, r).fill(0xffffff);
			g.position.set(r + k * (m.dot + m.dotGap), 0);
			group.addChild(g);
			shapes.push(g);
		}
		group.position.set(0, row.height / 2);
		row.body.addChild(group);
		row.dotShapes = shapes;
		row.dotGroup = group;
		this.tint(row);
	}

	private tint(row: Row) {
		for (const t of row.tokens) {
			if (t.mesh) t.mesh.tint = this.color;
			if (t.glow) t.glow.tint = this.color;
		}
		for (const g of row.dotShapes ?? []) g.tint = this.color;
		row.dirty = true;
	}

	private unbuild(row: Row) {
		if (!row.built) return;
		row.built = false;
		this.settle(row, false);
		for (const t of row.tokens) {
			if (t.mesh) {
				// A mesh does not own its geometry or shader, and their GPU buffers outlive it.
				const { geometry, shader } = t.mesh;
				t.mesh.destroy();
				geometry.destroy(true);
				shader?.destroy(false);
			}
			t.glow?.destroy({ texture: true, textureSource: false });
			t.mesh = undefined;
			t.sweep = undefined;
			t.glow = undefined;
		}
		for (const child of row.body.removeChildren()) child.destroy({ children: true });
		for (const source of row.sources) source.destroy();
		row.sources = [];
		row.dotShapes = undefined;
		row.dotGroup = undefined;
		row.body.filters = null;
		row.filter?.destroy();
		row.filter = null;
	}

	private dropRow(row: Row) {
		this.unbuild(row);
		row.node.destroy({ children: true });
	}

	/** Draw the row from a texture of itself while `cache` holds: the row is shown and is not the
	 *  sung line, whose words change every frame. Its place, scale and opacity stay free, because
	 *  they apply to the texture as a whole. Anything that changes inside it (a sweep flipping on a
	 *  seek, a lift or dimming or blur easing, the theme's colour) marks it `dirty`, and the texture
	 *  is redrawn that frame, which costs about what drawing the row directly would.
	 *
	 *  Redrawn rather than drawn directly while it eases, because at a fractional pixel ratio Pixi
	 *  lands a blur drawn to the screen over half a pixel away from the same blur drawn into a
	 *  texture: a row handed from one to the other when its blur settled would twitch. */
	private settle(row: Row, cache: boolean) {
		if (cache && row.built && row.node.boundsArea) {
			if (!row.cached) {
				row.node.cacheAsTexture({ resolution: this.m.dpr });
				row.cached = true;
			} else if (row.dirty) {
				row.node.updateCacheTexture();
			}
		} else if (row.cached) {
			row.node.cacheAsTexture(false);
			row.cached = false;
			// Pixi (8.20) leaves the children's inherited opacity as it was inside the cached group,
			// which the row's own alpha did not reach, and recomputes it only when that alpha next
			// changes: a dimmed row would come back at full strength. Re-adding the body is what
			// makes it recompute them now.
			row.node.removeChild(row.body);
			row.node.addChild(row.body);
		}
		row.dirty = false;
	}

	// --- the frame loop -------------------------------------------------------------------------

	private readonly lost = (e: Event) => {
		e.preventDefault();
		this.onLost();
	};

	private kick() {
		// A display frame does whatever the slower one the dots asked for would have.
		clearTimeout(this.dotsTimer);
		this.dotsTimer = undefined;
		if (!this.frameId && !this.dead) this.frameId = requestAnimationFrame(this.frame);
	}

	private readonly frame = (ts: number) => {
		this.frameId = 0;
		if (this.dead) return;
		const dt = this.lastTs ? Math.min(100, ts - this.lastTs) : 16;
		this.lastTs = ts;
		const pace = this.update(ts, dt);
		// The system ticker stopped in `create`, advanced by hand. The GPU garbage collector it
		// clocks falls due here and collects after the render below, the only place it ever did,
		// so it loses nothing by waiting while the stage sleeps and nothing is drawn.
		Ticker.system.update(ts);
		this.app.render();
		if (pace === 'full') this.kick();
		else {
			// Until the next kick nothing moves but the dots, which are painted from the clock, not
			// from `dt`. That kick can land anywhere in the dots' wait, up to `DOTS_MS` after this
			// frame; timed from here, the first step of whatever it sets moving would jump.
			this.lastTs = 0;
			if (pace === 'dots') this.dotsTimer = setTimeout(() => this.kick(), DOTS_MS);
		}
	};

	/** Advance everything to wall time `now`, and say what the next frame is for. */
	private update(now: number, dtMs: number): Pace {
		const ms = this.clock.valueAt(now) * 1000;
		const playing = !this.clock.isPaused;
		const viewH = this.host.clientHeight;
		const dt = dtMs / 1000;
		let moving = false;
		let dots = false;

		let fade = 1;
		let rise = 0;
		if (this.swap) {
			moving = true;
			const t = now - this.swap.at;
			if (this.swap.phase === 'out') {
				const k = clamp01(t / SWAP_OUT_MS);
				fade = 1 - k;
				if (k >= 1) {
					this.showLines(this.pending ?? this.lines);
					fade = this.swap ? 0 : 1;
				}
			} else {
				const e = easeInOut(clamp01(t / SWAP_IN_MS));
				fade = e;
				rise = (1 - e) * SWAP_RISE;
				if (e >= 1) this.swap = null;
			}
		}
		if (this.app.stage.alpha !== fade) this.app.stage.alpha = fade;
		if (this.app.stage.y !== rise) this.app.stage.y = rise;

		// Read after the swap, which may have laid out new lines (and, with a rewrap pending, for
		// new metrics).
		const m = this.m;
		const snap = (v: number) => Math.round(v * m.dpr) / m.dpr;
		// How far past its box a row can draw: its cells' padding, which holds a glow, a blur's
		// spread and a lifted word.
		const reach = m.cellPad * 2;
		// New lyrics are still all but invisible for their first few frames (`easeInOut` is under 5%
		// for the first ~90 ms); their rows are rasterised a few a frame while that lasts.
		let budget = this.swap?.phase === 'in' && fade < 0.05 ? BUILD_BUDGET : Infinity;

		for (const row of this.rows) {
			const focusIndex = this.interlude ? -1 : this.active;
			const isAnchor = row.dots ? true : row.index === focusIndex;
			// Rows before the first line measure their distance from a line -1 that is not there.
			const distance = row.dots
				? 0
				: this.interlude
					? Math.abs(row.index - this.interlude.at) + (row.index >= this.interlude.at ? 1 : 0)
					: Math.abs(row.index - this.active);
			const near = isAnchor || row.hover;
			const alphaTarget = near ? 1 : this.browsing ? 0.85 : lineAlpha(distance);
			const blurTarget = near || this.browsing || !this.expanded ? 0 : lineBlur(distance);
			const scaleTarget = near || !this.expanded ? 1 : INACTIVE_SCALE;
			const hasTimedWords = row.tokens.some((t) => t.timed);
			const dimTarget = isAnchor && !row.dots && hasTimedWords ? DIM : 1;

			row.y.step(now, dt);
			if (row.scale.target !== scaleTarget) row.scale.aim(scaleTarget);
			row.scale.step(now, dt);
			row.alpha = approach(row.alpha, alphaTarget, dtMs, 110);
			row.blur = approach(row.blur, blurTarget, dtMs, 110, 0.02);
			row.dim = approach(row.dim, dimTarget, dtMs, 110);
			if (!row.y.resting || !row.scale.resting) moving = true;
			if (row.alpha !== alphaTarget || row.blur !== blurTarget || row.dim !== dimTarget) moving = true;

			const y = row.y.value;
			const onScreen = y + row.height > -viewH * 0.25 && y < viewH * 1.25;
			const keep = y + row.height > -viewH * 1.5 && y < viewH * 2.5;
			if (!keep) {
				this.unbuild(row);
				row.node.visible = false;
				continue;
			}
			if (!onScreen) {
				this.settle(row, false);
				row.node.visible = false;
				continue;
			}
			if (!row.built) {
				if (budget <= 0) {
					row.node.visible = false;
					moving = true;
					continue;
				}
				budget--;
				this.build(row);
			}
			// Built ahead in the band around the canvas, drawn only where it meets it. A row that is
			// not drawn is not painted either: what it shows is worked out afresh from the clock
			// when it comes back, and it no longer keeps frames coming for nobody to see.
			const shown = y + rise + row.height + reach > 0 && y + rise - reach < viewH;
			row.node.visible = shown;
			if (!shown) {
				this.settle(row, false);
				continue;
			}

			const node = row.node;
			const settled = row.y.resting && row.scale.resting;
			node.pivot.set(0, row.height / 2);
			node.position.set(m.colX, (settled ? snap(y) : y) + row.height / 2);
			node.scale.set(row.scale.value);
			node.alpha = row.alpha;
			const body = row.body;
			if (row.blur > 0.05) {
				if (!row.filter) {
					row.filter = new BlurFilter({ strength: 0, quality: 3, resolution: m.dpr });
					// Pixi clips a filter to the canvas, measured from where it draws to. Into a
					// cached row's texture, that is the texture's corner rather than the row's place
					// on screen, and a row taller than the canvas would lose its lower part.
					row.filter.clipToViewport = false;
				}
				if (row.filter.strength !== row.blur) {
					row.filter.strength = row.blur;
					// Room for the whole kernel; with Pixi's default the spread was cut off in a hard seam.
					// In whole device pixels as well: Pixi lines the blur's area up with them before
					// padding it, and at a ratio like 1.25 a padding that is not moved the blurred line
					// a pixel or more off where it sits unblurred.
					const g = m.grid || 1;
					row.filter.padding = Math.ceil((Math.ceil(row.blur * 4) + 2) / g) * g;
					row.dirty = true;
				}
				if (body.filters?.[0] !== row.filter) {
					body.filters = [row.filter];
					row.dirty = true;
				}
			} else if (body.filters) {
				body.filters = null;
				row.dirty = true;
			}

			if (row.dots) {
				if (this.paintDots(row, ms, now, playing)) dots = true;
				continue;
			}
			if (this.paintWords(row, isAnchor, ms, playing, dtMs)) moving = true;
			this.settle(row, !isAnchor);
		}

		this.leaving = this.leaving.filter((row) => {
			const t = (now - row.born) / 300;
			if (t >= 1) {
				this.dropRow(row);
				return false;
			}
			row.node.alpha = row.alpha * (1 - t);
			row.dotGroup?.scale.set(1 - 0.4 * easeOutCubic(t));
			moving = true;
			return true;
		});
		return moving ? 'full' : dots ? 'dots' : 'rest';
	}

	/** Sweep, lift and glow for one row's words. True while they are still changing. Every value it
	 *  changes marks the row `dirty`, which is what redraws a cached row's texture. */
	private paintWords(row: Row, sung: boolean, ms: number, playing: boolean, dtMs: number): boolean {
		const m = this.m;
		const band = BAND * m.size;
		const lift = LIFT * m.size;
		let moving = false;
		for (const t of row.tokens) {
			if (!t.mesh || !t.sweep) continue;
			const u = t.sweep.uniforms;
			let p = 1;
			let rise = 0;
			let glow = 0;
			if (sung && t.timed) {
				p = t.end > t.start ? clamp01((ms - t.start) / (t.end - t.start)) : ms >= t.start ? 1 : 0;
				// The rise takes at least 400 ms, so a quick word still floats up rather than jumping.
				const q = clamp01((ms - t.start) / Math.max(t.end - t.start, 400));
				rise = -lift * easeOutCubic(q);
				if (q >= 1) rise = -Math.round(lift * m.dpr) / m.dpr;
				if (playing && q < 1) moving = true;
				if (t.end - t.start >= HELD_MS) glow = Math.sin(Math.PI * p) * 0.55;
			} else if (!sung && t.timed) {
				// Off the sung line: the words settle back down and the line reads as plain text.
				p = row.index < this.active || (this.interlude && row.index < this.interlude.at) ? 1 : 0;
			}
			const edge = p * (t.w + band * 2) - band;
			if (u.uEdge !== edge) {
				u.uEdge = edge;
				row.dirty = true;
			}
			if (u.uDim !== row.dim) {
				u.uDim = row.dim;
				row.dirty = true;
			}
			t.lift = sung ? rise : approach(t.lift, 0, dtMs, 90, 0.01);
			if (t.lift !== 0 && !sung) moving = true;
			const y = m.padY + t.y + t.lift;
			if (t.mesh.position.y !== y) {
				t.mesh.position.y = y;
				row.dirty = true;
			}
			if (t.glow) {
				// Under 1/255 the glow's alpha packs to zero and it draws nothing, which is almost
				// all the time: then it is not drawn at all.
				const lit = glow >= 1 / 255;
				const gy = y - m.cellPad;
				if (t.glow.visible !== lit || t.glow.alpha !== glow || t.glow.position.y !== gy) {
					t.glow.visible = lit;
					t.glow.alpha = glow;
					t.glow.position.y = gy;
					row.dirty = true;
				}
			}
		}
		return moving;
	}

	private paintDots(row: Row, ms: number, now: number, playing: boolean): boolean {
		const gap = this.interlude;
		if (!gap || !row.dotShapes || !row.dotGroup) return false;
		const third = (gap.to - gap.from) / 3;
		row.dotShapes.forEach((g, k) => {
			const p = clamp01((ms - (gap.from + k * third)) / third);
			g.alpha = 0.25 + p * 0.75;
			g.scale.set(0.8 + p * 0.35);
		});
		// Breathing, 1 → 1.08 and back over 3.2 s, while the song plays.
		const breath = playing ? 0.5 - 0.5 * Math.cos((2 * Math.PI * ((now - row.born) % 3200)) / 3200) : 0;
		row.dotGroup.scale.set(1 + 0.08 * breath);
		return playing;
	}
}
