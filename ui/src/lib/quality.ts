// The audio-quality chip's text, from what the backend says is actually streaming.
//
// Derived from the format's own `mimeType` and `bitrate`, never from the itag. itag 251 is always
// Opus, but its bitrate is whatever YouTube encoded that particular track at — measured across a
// handful of tracks it ranged 106–167 kbps — so a nominal number would be wrong most of the time.

/** Pull the codec out of a mimeType: `audio/webm; codecs="opus"` → `opus`. */
function codecOf(mime: string): string | null {
	const m = /codecs="?([^";]+)"?/.exec(mime);
	const raw = (m?.[1] ?? mime.split('/')[1] ?? '').trim().toLowerCase();
	if (!raw) return null;
	// The families worth naming. `mp4a.40.2` is AAC-LC and `mp4a.40.5` is HE-AAC, but the chip has
	// room for the family, not the profile — and the bitrate beside it already says which is which.
	if (raw.startsWith('opus')) return 'Opus';
	if (raw.startsWith('mp4a') || raw.startsWith('aac')) return 'AAC';
	if (raw.startsWith('vorbis')) return 'Vorbis';
	if (raw.startsWith('flac')) return 'FLAC';
	if (raw.startsWith('mp3') || raw.startsWith('mpeg')) return 'MP3';
	return raw.toUpperCase().slice(0, 8);
}

export interface Quality {
	/** "Opus", "AAC"… */
	codec: string;
	/** Rounded kbps, or null when the backend did not say. */
	kbps: number | null;
	/** "Opus 160" — what the chip shows. */
	label: string;
	/** Roughly how good it is, for colouring. Thresholds are about *perceptual* parity, not
	 *  numbers: Opus at 160 and AAC at 256 are both transparent, Opus at 64 is not. */
	tier: 'high' | 'medium' | 'low';
}

export function quality(codec?: string | null, bitrate?: number | null): Quality | null {
	if (!codec) return null;
	const name = codecOf(codec);
	if (!name) return null;
	const kbps = typeof bitrate === 'number' && bitrate > 0 ? Math.round(bitrate / 1000) : null;

	// Opus is worth roughly 1.6x AAC at the same bitrate, so the bands differ by codec rather than
	// pretending one number means the same thing everywhere.
	const highAt = name === 'Opus' ? 128 : 192;
	const lowAt = name === 'Opus' ? 80 : 112;
	const tier: Quality['tier'] =
		kbps === null ? 'medium' : kbps >= highAt ? 'high' : kbps < lowAt ? 'low' : 'medium';

	return { codec: name, kbps, label: kbps === null ? name : `${name} ${kbps}`, tier };
}
