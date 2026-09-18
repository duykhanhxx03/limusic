<script lang="ts">
	import { forgetVideoUrl, playback, videoUrlFor } from '$lib/player.svelte';
	import { canVideo, hasVideo, registerVideo, showVideo, video } from '$lib/video.svelte';

	// --- Music videos (plan 031) -------------------------------------------------------------
	// When the track *is* a music video, the player view draws the video instead of the artwork.
	// mpv stays the audio master: the element is muted and its clock is stapled to mpv's position.
	// Bytes come from Rust over a loopback proxy, so nothing here ever sees a googlevideo URL.
	//
	// This component owns the element and is always mounted (see +layout), so closing the player
	// view no longer destroys the picture. The state the view also reads lives in $lib/video.

	// How the picture is kept in step with mpv. Measured against mpv actually playing the same
	// track, because the guesses before it were all wrong in the same direction:
	//
	//   - Drift is *flat*. Once the two agree they stay agreeing over minutes, so nothing needs
	//     continuous correction.
	//   - A seek is expensive. A tiny buffered one loses ~85 ms of motion; a real mid-stream one
	//     costs about a second of re-buffering, and mpv plays on throughout. A seek issued to close
	//     a small gap therefore *opens* a gap of roughly its own cost, and a loop that seeks
	//     whenever it is out seeks forever. That was the stutter, twice, and it is also where the
	//     steady half-second offset came from: the seek landed exactly its own cost behind.
	//   - A `playbackRate` change is nearly free and holds exactly: 1.5x measured a dead-steady
	//     ratio of 1.500 with no stalls, on a picture that is muted anyway.
	//
	// So the rate does the work and seeking is the exception. A seek is only for a gap too big to
	// trim out in reasonable time (a track change, a scrub, coming back from the tray), and it aims
	// past the target by what the re-buffer will cost. The first sync of a fresh element is one of
	// those cases by definition: resolving the video costs a round trip, so mpv is already a second
	// or two in before the element can seek at all.
	/** Trim bands. A rate write is *not* free on WebKitGTK, whatever the steady-state measurement
	 *  above says: WebKit's GStreamer backend implements a `playbackRate` change as a flushing
	 *  seek, so every write drops the sink's buffers and shows as a flicker (#107). So the rate is
	 *  written twice per correction at most, once to start it and once to end it, and never for
	 *  the wobble that ~4 Hz sampling puts on `drift`. TRIM_TICKS is that guard: about two seconds
	 *  of sustained out-of-band drift before a correction starts at all. */
	const TRIM_FROM = 0.35;
	const TRIM_TO = 0.15;
	const TRIM_TICKS = 8;
	/** Above this, trimming would take too long to sit through, so pay for one seek instead. */
	const SEEK_FROM = 2.5;
	/** The lead on an *unbuffered* seek: it aims past the re-buffer that seek is about to cost. A
	 *  seek whose target is already buffered lands within a frame, so leading it would only
	 *  overshoot. `bufferedAt` below is what tells the two apart. */
	const SEEK_COST = 1;
	const SEEK_COOLDOWN = 6000;
	/** Getting a fresh element onto the music. The first seek is unbuffered, so where it lands
	 *  cannot be predicted, only measured: it costs about a second of re-buffering and mpv plays on
	 *  through it. So seek with a lead, wait for the seek to land *and* for the element to have
	 *  enough data to play on, then measure again. A correction at that point is a buffered seek
	 *  (~85 ms), cheap enough to spend and accurate to a frame or two.
	 *
	 *  SYNC_WAIT is how long one attempt gets to become playable before it is written off and
	 *  re-aimed at where the music has reached by then. It is a *retry*, never a success: a cold
	 *  seek into the middle of a WebM has to fetch the container index first and can take seconds,
	 *  and treating that timeout as "synced" is what made reopening the view mid-track resume the
	 *  picture at completely the wrong place and then fight itself on the steady-state cooldown. */
	const SYNC_SEEKS = 3;
	const SYNC_WAIT = 4000;
	/** How long the element keeps its stream once nobody can see the picture: the window is hidden
	 *  (opening the mini player hides the main window, src-tauri/src/mini.rs), or the player view is
	 *  shut. Coming back from either is an expensive mid-track seek, so it is worth holding through
	 *  a short absence rather than stopping on the spot. Past the grace the element goes `dormant`:
	 *  paused *and* released, src off, because a parked pipeline is not free. Measured on
	 *  Fedora/NVIDIA with a restored music-video track nobody had even played: 385 MiB of GPU memory
	 *  and 154 MiB of PSS, held for the session.
	 *  It starts dormant for that reason, and only the view claiming the element wakes it, so a
	 *  restored queue, or a whole session played with the view shut, costs nothing beyond the URL
	 *  warm-up the store already does.
	 *  ponytail: one flat timer for both triggers. Raise it if coming back still resyncs, lower it
	 *  if idle memory matters more than that resync. */
	const IDLE_GRACE = 60000;
	/** The view being shut gets an earlier first stage: past REST_GRACE the picture is `resting`,
	 *  paused but still attached. Playing with the view shut used to keep the picture running so a
	 *  reopen had nothing to re-sync, and what that cost was a full video decode into the 0x0
	 *  parking box for as long as the music played. The attached pipeline only costs memory, which
	 *  IDLE_GRACE already bounds. So a quick close and reopen still comes back with nothing to
	 *  re-sync, a longer one pays one seek on a stream that is already open and indexed, and only
	 *  past IDLE_GRACE does it pay the full converge from byte 0. Either of those runs behind the
	 *  artwork (see `showVideo`), so what they cost is time, not a wrong picture.
	 *  A hidden window with the view open does not rest: that is the mini player round trip, which
	 *  is expected back well inside IDLE_GRACE and should come back to a picture still in step. */
	const REST_GRACE = 15000;
	let idleTimer: ReturnType<typeof setTimeout> | undefined;
	let restTimer: ReturnType<typeof setTimeout> | undefined;
	let hidden = $state(false);
	let dormant = $state(true);
	let resting = $state(false);
	let synced = false;
	let syncSeeks = 0;
	let syncStart = 0;
	let lastSeek = 0;
	/** A correction in progress, the speed it was picked against, and how many ticks running the
	 *  drift has been out of band. Plain lets: nothing renders off them. */
	let trimming = false;
	let trimSpeed = 1;
	let outOfBand = 0;

	/** Which track the URL below was fetched for. A plain let, so the fetch effect can read it
	 *  without depending on itself. */
	let fetchedId: string | null = null;

	let parking = $state<HTMLElement | null>(null);
	let el: HTMLVideoElement | null = null;

	// Two effects, not one: the reset must key on the *track*, or toggling back to artwork would
	// throw the URL away and the way back would be a fresh download (#107). Effects run in
	// creation order, so this one clears before the next one fetches.
	$effect(() => {
		playback.now?.videoId;
		canVideo();
		video.url = null;
		fetchedId = null;
		synced = false;
		video.ready = false;
		syncSeeks = 0;
		syncStart = 0;
		lastSeek = 0; // a new track may sync at once, whatever the last one was doing
		// `dormant` and `resting` are deliberately *not* reset here: they mean "nobody can see the
		// picture and the grace is over", which a track change does not undo. Clearing them would
		// start the next video track decoding into a window or a parking box nobody is looking at.
		// Coming back into view is what clears them, and that path already re-runs the whole
		// converge phase.
	});

	$effect(() => {
		const id = playback.now?.videoId;
		// video.want is a dependency so turning video on mid-track starts the fetch; fetchedId is
		// what stops turning it off and on again from refetching what we already have.
		// `dormant` means the grace is over and the stream released, so resolving a URL here would
		// only feed a pipeline we are deliberately not holding. The view being shut is the same for
		// a track that starts while it is: attaching its stream would build a pipeline for nobody,
		// and the grace exists for the track the view was showing, not for the ones after it. Both
		// are state, so waking or reopening re-runs this and the fetch happens then.
		if (!id || !canVideo() || !video.want || dormant || !video.shown || fetchedId === id) return;
		fetchedId = id;
		let cancelled = false;
		// Usually already resolved (the store warms it when the track starts, and keeps it across
		// this component being unmounted), in which case this settles without touching the network.
		// A `#t=` media fragment was tried here, to open the stream where the music already is
		// rather than at byte 0. WebKitGTK's GStreamer backend did not honour it, and it is not
		// free to leave in: the element still prerolls, so a fragment it half-applies is one more
		// unknown in a phase that has to measure precisely. The converge phase below handles the
		// opening position instead.
		videoUrlFor(id).then((u) => !cancelled && (video.url = u));
		// Cancelled with nothing to show for it (toggled off mid-flight): let it be tried again.
		return () => {
			cancelled = true;
			if (!video.url) fetchedId = null;
		};
	});

	/** Where mpv is *now*, not where it was when the last tick was emitted. Ticks land at ~4 Hz
	 *  (src-tauri/src/lib.rs), so `playback.position` alone is up to 250 ms old, and lining the
	 *  picture up against it parks it that far behind the music every time. */
	function mpvNow() {
		if (playback.paused) return playback.position;
		const since = (performance.now() - playback.positionAt) / 1000;
		// Past a couple of tick intervals the stream has stopped rather than slowed: a stall, a
		// backgrounded window, or the instant after unpausing, where the newest sample is still the
		// one from the pause. Extrapolating there would be a guess that overshoots.
		if (since > 0.4) return playback.position;
		return playback.position + since * playback.speed;
	}

	/** Catch-up rate for a gap, picked once when the correction starts and held until it closes:
	 *  re-picking it as the gap shrinks would cost a flush per band crossed. */
	function trimFor(drift: number) {
		const a = Math.abs(drift);
		const k = a > 1.2 ? 0.5 : a > 0.5 ? 0.25 : 0.1;
		return playback.speed * (1 + Math.sign(drift) * k);
	}

	/** Whether `t` is already downloaded, which is what decides if a seek there is nearly free or
	 *  costs a round trip and a re-buffer. */
	function bufferedAt(v: HTMLVideoElement, t: number) {
		for (let i = 0; i < v.buffered.length; i++) {
			if (t >= v.buffered.start(i) && t <= v.buffered.end(i)) return true;
		}
		return false;
	}

	/** Start the picture, if the music is going and the picture has neither rested nor gone dormant.
	 *  Not before the converge phase has finished: playing during it means showing real motion from
	 *  the wrong moment of the video, which reads as broken in a way a still frame does not. */
	function resumeVideo() {
		if (el && synced && !playback.paused && !dormant && !resting) el.play().catch(() => {});
	}

	function syncVideo() {
		// readyState 0 has no clock to compare against, and mid-seek the comparison is meaningless.
		// Anything above that is fair game: re-buffering sits at 2 for long stretches and the
		// picture keeps moving perfectly well there.
		// A resting picture is paused on purpose while mpv plays on, so the drift only grows, and
		// every seek it provoked would fetch and decode for nobody. Waking re-runs the converge
		// phase from wherever it sits.
		if (!el || resting || !hasVideo() || el.seeking || el.readyState < 1) return;
		const drift = mpvNow() - el.currentTime;
		if (!synced) {
			if (!syncStart) syncStart = performance.now();
			// readyState < 3 means the picture is not playable where it currently sits, so `drift`
			// is still moving by whatever the load or the re-buffer costs, and measuring now would
			// be measuring that. Waiting is only worth so much though: past SYNC_WAIT this attempt
			// is written off and re-aimed below, at where the music has reached by then.
			const stalled = performance.now() - syncStart > SYNC_WAIT;
			if (el.readyState < 3 && !stalled) return;
			// In band, or out of attempts. A stall is neither, so it falls through to another seek
			// rather than resuming a picture that is nowhere near the music.
			if (Math.abs(drift) <= TRIM_TO || syncSeeks >= SYNC_SEEKS) {
				synced = true;
				// Only now does the view swap the artwork for the picture (see `showVideo`), so the
				// first frame anyone sees is this one, not the opening frame or a stale one.
				video.ready = true;
				// Hold the steady-state cooldown only if the picture is somewhere the trim can
				// actually work from. Giving up still seconds out has to be allowed to seek at
				// once, not sit out six more.
				lastSeek = Math.abs(drift) <= SEEK_FROM ? performance.now() : 0;
				resumeVideo();
				return;
			}
			syncSeeks++;
			syncStart = performance.now(); // each attempt gets its own wait
			trimming = false;
			outOfBand = 0;
			el.playbackRate = playback.speed;
			// Lead a seek that has to fetch bytes, not one landing in data we already hold: the
			// first costs about a second of motion, the second lands within a frame. Never while
			// paused, since mpv is not moving and there is nothing to lead.
			const target = mpvNow();
			el.currentTime = target + (playback.paused || bufferedAt(el, target) ? 0 : SEEK_COST);
			return;
		}
		if (Math.abs(drift) > SEEK_FROM) {
			const now = performance.now();
			if (now - lastSeek < SEEK_COOLDOWN) return; // let the last one finish re-buffering
			lastSeek = now;
			trimming = false;
			outOfBand = 0;
			el.playbackRate = playback.speed;
			// Aim past the re-buffer this is about to cost, or it lands behind by exactly that.
			// Not while paused: mpv is not moving, so the lead would only overshoot.
			el.currentTime = mpvNow() + (playback.paused ? 0 : SEEK_COST);
			return;
		}
		if (playback.paused) return; // nothing is moving, so there is nothing to trim
		// A tempo change makes the held rate meaningless: it was picked against the old speed.
		if (trimming && trimSpeed !== playback.speed) trimming = false;
		if (trimming) {
			if (Math.abs(drift) > TRIM_TO) return; // still closing: hold the rate, write nothing
			trimming = false;
		}
		if (el.playbackRate !== playback.speed) {
			el.playbackRate = playback.speed; // ends a correction, or follows a tempo change
			return;
		}
		outOfBand = Math.abs(drift) > TRIM_FROM ? outOfBand + 1 : 0;
		if (outOfBand < TRIM_TICKS) return;
		outOfBand = 0;
		trimming = true;
		trimSpeed = playback.speed;
		el.playbackRate = trimFor(drift);
	}

	$effect(() => {
		playback.position; // the tick this runs on
		playback.paused; // and a pause/resume, which position ticks do not cover
		playback.speed; // and a tempo change, which must reach the picture without waiting for drift
		syncVideo();
	});

	$effect(() => {
		const paused = playback.paused;
		if (!el) return;
		// Losing the video has to stop the picture, and it used to fall out of the guard below
		// before it could: `hasVideo()` goes false on every track change (the reset effect clears
		// `video.url`), so an audio track after a video one left the old picture decoding at full
		// rate inside the parked 0x0 container until it reached its own end.
		if (!hasVideo()) {
			el.pause();
			return;
		}
		// `dormant`, not `document.hidden`: a hidden window keeps the picture for IDLE_GRACE, so a
		// mini-player round trip comes back with nothing to re-sync. Past that it stops, and
		// unpausing from the mini player must not start a dormant window decoding again. `resting`
		// is the same for the view being shut, from REST_GRACE on.
		// `synced` is a plain let, so this effect does not re-run when it flips; `syncVideo` and
		// the idle rule call `resumeVideo` themselves at those moments.
		if (paused || dormant || resting || !synced) el.pause();
		else resumeVideo();
	});

	// A hidden window is one of the two ways the picture stops mattering; the player view not
	// holding the element (`video.shown`) is the other. Kept as state so the rule below is one
	// effect. The tray and the mini player both arrive here: mini.rs hides the main window rather
	// than doing anything the webview could tell apart.
	$effect(() => {
		const onVisibility = () => (hidden = document.hidden);
		document.addEventListener('visibilitychange', onVisibility);
		onVisibility();
		return () => document.removeEventListener('visibilitychange', onVisibility);
	});

	// Nobody watching: the window still decodes video unless we stop it, so eventually we do.
	// Nothing here touches mpv, so the audio carries on either way.
	$effect(() => {
		// Whether the music is moving does not come into it. The view is the only place the
		// picture can be seen, so the view claiming the element is the only thing that wakes it:
		// playing with the view shut used to wake it too, and decoded every music-video track of
		// the session into the parking box, whether or not the view had ever been opened. This also
		// covers the cold start, where `paused` reads false before the backend's restore lands
		// (player.svelte.ts): the view is not mounted at all without `playback.now`.
		const idle = hidden || !video.shown;
		clearTimeout(idleTimer);
		if (idle) {
			// Not on the spot: see IDLE_GRACE.
			if (!dormant) {
				idleTimer = setTimeout(() => {
					dormant = true;
					el?.pause();
				}, IDLE_GRACE);
			}
			return;
		}
		// Inside the grace, so the picture never stopped and never lost step. That is the whole
		// point of the grace: there is nothing to do.
		if (!dormant && !resting) return;
		// It rested, or sat out the whole grace and was released, so it is seconds out by
		// definition and, released, has no stream at all. Run the whole converge phase again
		// rather than sitting out the cooldown and then trimming, and keep the picture still until
		// it lands. From rest that is one seek on the stream it kept.
		// `ready` goes with `synced`, which puts the artwork back up until the picture lands. It is
		// cleared here rather than when the picture rests or is released: the view can stay open
		// with the picture parked (the enlarged lyrics drop the artwork column), and clearing it
		// there would bring the artwork wash back on its own a while later. Nothing puts the
		// element back on screen from rest or dormancy without coming through here, and this runs
		// in the same flush as whatever brought it back (the view's claim, or the window showing),
		// before anything paints.
		dormant = false;
		resting = false;
		synced = false;
		video.ready = false;
		syncSeeks = 0;
		syncStart = 0;
		lastSeek = 0;
		el?.pause();
		// The picture restarts from inside resumeVideo, which is also where the "only if mpv is
		// still going" check now lives.
		syncVideo();
	});

	// The first stage of the rule above, for the view alone: see REST_GRACE. Its own effect so each
	// timer only starts over on its own inputs: this one must not restart when the window hides, nor
	// the release timer when this one fires. Nothing to do once dormant, which has already stopped
	// the picture.
	$effect(() => {
		clearTimeout(restTimer);
		if (video.shown || resting || dormant) return;
		restTimer = setTimeout(() => {
			resting = true;
			el?.pause();
		}, REST_GRACE);
	});

	// The element is built by hand, not by an {#if}, because it has to move between DOM parents and
	// Svelte removes the nodes it created from the parent it created them in. Owning the node
	// outright is what makes claimVideo/parkVideo safe.
	$effect(() => {
		const park = parking;
		if (!park || el) return;
		const v = document.createElement('video');
		v.muted = true;
		v.playsInline = true;
		v.preload = 'auto';
		v.addEventListener('loadedmetadata', syncVideo);
		v.addEventListener('canplay', syncVideo);
		v.addEventListener('seeked', syncVideo);
		v.addEventListener('error', () => {
			if (playback.now?.videoId) forgetVideoUrl(playback.now.videoId);
			video.url = null;
		});
		el = v;
		registerVideo(v, park);
	});

	// src and class are the only two things that change on it, so drive them from here rather than
	// from markup that no longer owns the node.
	$effect(() => {
		if (!el) return;
		// `dormant` releases it: see IDLE_GRACE. `video.url` is kept, so waking re-attaches the
		// same stream without another resolve.
		const u = hasVideo() && !dormant ? video.url : null;
		// Setting src to the same string would still reload the element, so only write a change.
		if (u && el.getAttribute('src') !== u) el.setAttribute('src', u);
		else if (!u && el.hasAttribute('src')) {
			el.removeAttribute('src');
			// Removing the attribute does not run the media load algorithm (HTML 4.8.11 says so
			// outright), so without this the element keeps the finished video's GStreamer pipeline,
			// its buffers and its connection to the proxy for the rest of the session. `load()` is
			// the only thing that runs the release steps. Never on a src *swap*: that path already
			// reloads, and resetting readyState mid-swap is what the converge phase is written
			// around. This is also what makes dormancy worth anything: the pipeline, the decoder
			// and (on NVIDIA) a few hundred MiB of GPU memory only come back on `load()`.
			el.load();
		}
	});

	$effect(() => {
		if (!el) return;
		// Hidden rather than unmounted when the view shows artwork instead: rebuilding it refetched
		// from byte 0 and re-seeked, which is the black frame in #107. That includes the converge
		// phase (see `showVideo`), which loads and seeks here under the artwork just as it used to
		// in the parking box: none of it needs the picture to be visible.
		//
		// No shadow, unlike the artwork: WebKitGTK re-blurs a resting box-shadow over the whole tile
		// on every repaint, and a video repaints 24 times a second at this size. It dragged the whole
		// app, not just the picture. Same cause as the card-hover cost measured on 2026-08-06.
		el.className = `w-full rounded-2xl bg-black object-contain ${
			showVideo() ? 'aspect-video' : 'pointer-events-none absolute inset-0 h-full opacity-0'
		}`;
	});
</script>

<!-- Where the picture waits while nothing is showing it: in the document, so the spec's
     "removed from a Document" steps never pause it, but zero-sized and unpaintable.
     Nothing plays in here for long: once the view has let go of the picture it is paused after
     REST_GRACE and released after IDLE_GRACE, whether or not the music is playing, and a track
     that starts while it waits here is not loaded until the view comes back for it. The view
     shows the artwork until the picture has caught up again. -->
<div
	bind:this={parking}
	aria-hidden="true"
	class="pointer-events-none fixed left-0 top-0 h-0 w-0 overflow-hidden opacity-0"
></div>
