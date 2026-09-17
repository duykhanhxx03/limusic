<script lang="ts">
	// The app mark: a ring with a play triangle in it, the YouTube Music shape.
	//
	// Drawn here rather than shipped as an asset file, and in `currentColor` rather than YouTube's
	// red, which is what makes it follow the theme: it inherits the surrounding text colour, so it is
	// black on a light theme and white on a dark one with nothing to switch. An asset would need two
	// files and a `prefers-color-scheme` guard that the app's own theme toggle would then disagree
	// with, since the theme here is a class on <html>, not the OS setting.
	//
	// This is the in-app mark only. The bundled icon (src-tauri/icons) and the browser favicon are
	// the filled red version, because neither of those sits on a surface whose colour we control.
	let { class: className = 'h-6 w-6' }: { class?: string } = $props();
</script>

<svg viewBox="0 0 24 24" fill="none" class={className} role="img" aria-label="YouTube Music ++">
	<!-- The ring. Stroked, not two filled circles: a stroke keeps its weight when the mark is
	     scaled, where a donut built from fills has to be re-tuned at every size. -->
	<circle cx="12" cy="12" r="10.25" stroke="currentColor" stroke-width="1.5" />
	<!-- The triangle, filled, optically centred: a triangle centred on its bounding box reads as
	     sitting left of centre, because its mass is toward the flat edge. The half-pixel nudge right
	     is the usual correction. -->
	<path d="M9.9 7.9 L17 12 L9.9 16.1 Z" fill="currentColor" />
</svg>
