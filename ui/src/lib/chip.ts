/**
 * The filter/navigation pill, shared by home's mood filter and Explore's mood row so the two rows
 * of chips cannot drift apart.
 *
 * Tonal at rest, filled with the accent when on. Grey-on-grey pills that go black when selected
 * are YouTube Music's chip row exactly, and they carry no colour of the app at all; this way the
 * one active filter is the only saturated thing above the feed. The resting chip is a fill rather
 * than an outline because the outline it used to carry measured 1.26:1 against the page — so the
 * chip was really being drawn by its label either way.
 */
export const chipClass = (active = false) =>
	`shrink-0 cursor-pointer rounded-full px-3.5 py-1.5 text-sm transition-colors ${
		active
			? 'bg-primary text-primary-foreground'
			: 'bg-foreground/8 text-muted-foreground hover:bg-foreground/15 hover:text-foreground'
	}`;
