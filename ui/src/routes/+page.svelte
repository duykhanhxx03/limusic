<!-- Home is not rendered here. It lives in +layout.svelte as `HomeFeed`, mounted on the first visit
     and kept mounted (hidden) while you are on another page, so coming back finds the same mood
     chip, the same pages loaded and the same scroll position instead of a feed rebuilt from the
     top. This route only has to exist so `/` resolves. -->
