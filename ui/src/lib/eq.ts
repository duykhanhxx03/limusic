// Equalizer presets and the numbers the dialog shares with the backend.
//
// One gain per band of `equalizerBands()`, in dB, in the same order: 31, 62, 125, 250, 500, 1k, 2k,
// 4k, 8k, 16k Hz. The set and the values are the ones SimpMusic ships (Spotify's list of names),
// tuned for the Q 1.41 bands the player builds. No preset stores a preamp: each one gets exactly
// enough cut to take its loudest band back to 0 dB, so choosing one never makes anything clip that
// didn't before, and a preset that only cuts keeps its level.

/** ±dB per band. Matches `EQ_GAIN_RANGE` in commands.rs. */
export const GAIN_RANGE = 12;
/** Preamp limits, cut only. Matches `EQ_PREAMP_MIN`/`EQ_PREAMP_MAX` in commands.rs: below the
 *  bands' −12 because AutoEq corrections ask for more than that. */
export const PREAMP_MIN = -15;
export const PREAMP_MAX = 0;

export interface Preset {
	/** i18n key under `eq.presets`. */
	id: string;
	gains: number[];
}

export const PRESETS: Preset[] = [
	{ id: 'flat', gains: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
	{ id: 'acoustic', gains: [4, 4, 3, 1, 1.5, 1.5, 3, 3.5, 3, 1.5] },
	{ id: 'bass_booster', gains: [6, 5.5, 4.5, 2.5, 0.5, 0, 0, 0, 0, 0] },
	{ id: 'bass_reducer', gains: [-6, -5.5, -4.5, -2.5, -0.5, 0, 0, 0, 0, 0] },
	{ id: 'classical', gains: [4.5, 3.5, 3, 2.5, -1.5, -1.5, 0, 2, 3, 3.5] },
	{ id: 'dance', gains: [5, 6, 3.5, 0, 2, 3, 4, 4, 3, 0] },
	{ id: 'deep', gains: [6, 5, 3, 1.5, 3, 2, 0.5, -2.5, -4, -5] },
	{ id: 'electronic', gains: [5, 4.5, 1.5, 0, -1.5, 2, 1, 1.5, 4.5, 5] },
	{ id: 'hiphop', gains: [6, 5, 1.5, 3, -1, -1, 1.5, -1, 2, 3] },
	{ id: 'jazz', gains: [4, 3, 1.5, 2, -1.5, -1.5, 0, 1.5, 3, 4] },
	{ id: 'latin', gains: [5, 3, 0, 0, -1.5, -1.5, -1.5, 0, 3, 5] },
	{ id: 'loudness', gains: [6, 5, 0, 0, -2, 0, -1, -5, 5, 1] },
	{ id: 'lounge', gains: [-3, -1.5, -0.5, 1.5, 4, 2.5, 0, -1.5, 2, 1] },
	{ id: 'piano', gains: [3, 2, 0, 2.5, 3, 1, 2, 4, 3, 3] },
	{ id: 'pop', gains: [-1.5, -1, 0, 2, 4, 4, 2, 0, -1, -1.5] },
	{ id: 'rnb', gains: [5.5, 6, 4.5, 1, -2.5, -1, 2.5, 3, 3.5, 4] },
	{ id: 'rock', gains: [5, 4, 3, 1.5, -0.5, -1, 0.5, 3, 4, 4.5] },
	{ id: 'small_speakers', gains: [6, 5, 3.5, 2, 0, -0.5, 0, 1.5, 2.5, 2] },
	{ id: 'spoken_word', gains: [-4, -3.5, -2, 0, 3, 4.5, 5, 4, 2, -1] },
	{ id: 'treble_booster', gains: [0, 0, 0, 0, 0, 0, 2.5, 4.5, 5.5, 6] },
	{ id: 'treble_reducer', gains: [0, 0, 0, 0, 0, 0, -2.5, -4.5, -5.5, -6] },
	{ id: 'vocal_booster', gains: [-2.5, -3, -2.5, 1.5, 4, 4, 3.5, 2, 0, -1.5] }
];

/** The cut that brings a curve's loudest band back to 0 dB. */
export function headroom(gains: number[]): number {
	return -Math.max(0, ...gains);
}

/** Equal to a tenth of a dB, the precision everything is stored at. Stored values are floats that
 *  went through JSON and a slider, so exact comparison would miss a curve that is the same. */
export function sameCurve(a: number[], b: number[]): boolean {
	return a.length === b.length && a.every((v, i) => Math.abs(v - b[i]) < 0.05);
}

/** Which preset this curve is, if any — so the picker can name it after a restart. */
export function matchPreset(preamp: number, gains: number[]): string | null {
	return (
		PRESETS.find((p) => Math.abs(headroom(p.gains) - preamp) < 0.05 && sameCurve(p.gains, gains))
			?.id ?? null
	);
}

/** A band's label: "31" under a kilohertz, "16k" above it. */
export function bandLabel(hz: number): string {
	return hz >= 1000 ? `${hz / 1000}k` : String(hz);
}

/** A gain as the dialog prints it: signed, one decimal only when it has one. */
export function formatDb(v: number): string {
	const r = Math.round(v * 10) / 10;
	const s = Number.isInteger(r) ? String(r) : r.toFixed(1);
	return r > 0 ? `+${s}` : s;
}
