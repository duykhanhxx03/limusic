// Equalizer presets.
//
// One gain per band of `equalizerBands()`, in dB, in the same order: 31, 62, 125, 250, 500, 1k, 2k,
// 4k, 8k, 16k Hz. Kept deliberately gentle — most "bass boost" presets people meet are +10 or more
// at the bottom, which on a track already mastered loud just clips. Nothing here exceeds ±6, and
// anything that lifts a region trims the preamp to make room for it.

export interface Preset {
	/** i18n key under `eq.presets`. */
	id: string;
	preamp: number;
	gains: number[];
}

export const PRESETS: Preset[] = [
	{ id: 'flat', preamp: 0, gains: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
	{ id: 'bass', preamp: -3, gains: [6, 5, 4, 2, 0, 0, 0, 0, 0, 0] },
	{ id: 'treble', preamp: -2, gains: [0, 0, 0, 0, 0, 1, 2, 4, 5, 5] },
	{ id: 'vocal', preamp: -2, gains: [-2, -2, -1, 1, 3, 4, 3, 1, 0, 0] },
	{ id: 'rock', preamp: -3, gains: [5, 4, 2, -1, -2, 0, 2, 4, 5, 5] },
	{ id: 'pop', preamp: -2, gains: [-1, 0, 2, 4, 4, 2, 0, -1, -1, -1] },
	{ id: 'jazz', preamp: -2, gains: [3, 2, 1, 2, -1, -1, 0, 1, 3, 4] },
	{ id: 'classical', preamp: -1, gains: [4, 3, 2, 1, -1, -1, 0, 2, 3, 4] },
	{ id: 'electronic', preamp: -3, gains: [5, 4, 1, 0, -2, 2, 1, 2, 5, 5] }
];

/** Which preset these gains are, if any — so the chip can show a tick after a restart. */
export function matchPreset(preamp: number, gains: number[]): string | null {
	const same = (a: number[], b: number[]) => a.length === b.length && a.every((v, i) => v === b[i]);
	return PRESETS.find((p) => p.preamp === preamp && same(p.gains, gains))?.id ?? null;
}

/** A band's label: "31" under a kilohertz, "16k" above it. */
export function bandLabel(hz: number): string {
	return hz >= 1000 ? `${hz / 1000}k` : String(hz);
}
