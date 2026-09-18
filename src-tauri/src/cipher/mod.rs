//! `CipherDeobfuscator` (context/05) — the signature/`n`-transform runtime the orchestrator calls.
//!
//! Ties [`fetcher`] (player.js) + [`extractor`]/[`config`] (function names) + a hidden cipher
//! webview ([`crate::webview`]) that runs YouTube's own code (its `_yt_player` harness global comes
//! with the document — see `webview::HARNESS_HTML`). Every public method degrades
//! gracefully: a webview or extraction failure yields `None` / the original URL, and the
//! orchestrator falls through to the non-cipher fallback clients (context/06 §5).
//!
//! The webview is built on demand and torn down when idle (`teardown_if_idle`), never held for the
//! life of the process: it is a second `WebKitWebProcess`, and STS — the one thing every /player
//! request needs — comes from analysis alone.

mod config;
mod extractor;
mod fetcher;

pub use config::PlayerConfigStore;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::webview::Bridge;
use fetcher::PlayerJsFetcher;

const CIPHER_LABEL: &str = "limusic-cipher";
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on fetching player.js, which happens under the build lock. The shared HTTP client sets
/// no timeout of its own, so a stalled connection would otherwise hold every resolve that needs
/// the cipher behind it, not just the one that hit the stall. Generous, because an uncached
/// player.js is about 3 MB.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Discovery/validation (context/05): prove the injected exports actually WORK before the
/// orchestrator commits to this player, by running each on a sample input.
///
/// The old brute-force ("scan `window` for any 1-arg function that transforms a probe string") is
/// gone. It never had a chance — the sig function and `n` class live inside player.js's IIFE
/// closure and are never on `window` — and calling every enumerable 1-arg global to find out is a
/// side-effecting scan: it invokes `fetch` among others.
const DISCOVERY_JS: &str = r#"(function(){
  var t="grut12Abc_-";
  function ok(s){return typeof s==='string'&&/^[A-Za-z0-9_-]+$/.test(s)&&s!==t;}
  window.__n_ok=false;
  window.__sig_ok=false;
  try{window.__n_ok=(typeof window._nTransformFunc==='function'&&ok(window._nTransformFunc(t)));}catch(e){}
  try{window.__sig_ok=(typeof window._cipherSigFunc==='function'&&typeof window._cipherSigFunc(t)==='string');}catch(e){}
  window.__cipher_loaded=true;
})();"#;

#[derive(Default)]
struct Inner {
    bridge: Option<Bridge>,
    sts: Option<i32>,
    built_epoch: u64,
    n_available: bool,
    /// Whether an `_cipherSigFunc` export exists (i.e. a sig function name was found). When false,
    /// deciphering is impossible on this player regardless of freshness — so we skip refetch/retry.
    sig_available: bool,
    /// player.js has been fetched and its config/STS resolved for `built_epoch`. This alone
    /// answers `signature_timestamp`, which is why it is separate from `discovered`: the /player
    /// request needs an STS on every resolve, and paying a whole web process for that would keep
    /// the cipher webview resident on a machine that never plays anything. Invalidation
    /// (self-heal) clears this to force a re-fetch + re-analysis.
    analyzed: bool,
    /// Discovery has run for `built_epoch`, so `sig_available`/`n_available` mean something.
    /// False after an analysis-only pass; the first decipher call builds the webview and probes.
    discovered: bool,
    /// When the webview last did work, for the idle teardown. `Some` whenever `bridge` is.
    last_used: Option<Instant>,
    /// Moves on every invalidation and every completed build. A decipher attempt that fails
    /// carries the value it ran under into its self-heal, which stands down if it has moved: some
    /// other resolve has already healed or rebuilt, and healing again would destroy the webview
    /// that resolve may be evaluating on right now.
    generation: u64,
    /// Builds (everything `ensure_analyzed` does under the build lock) that have run to the end,
    /// whether they worked or not. A caller reads it before queueing for the lock, and so can tell
    /// once it holds the lock whether a build finished while it waited.
    builds: u64,
    /// Why the last of those builds failed, if it did (see [`Self::failed_since`]).
    build_error: Option<String>,
}

impl Inner {
    /// Whether [`CipherDeobfuscator::ensure_analyzed`] still owes work for `epoch`. `bridge_ok` is
    /// whether the webview window is actually still up, which only the caller can ask Tauri.
    fn owes_work(&self, epoch: u64, want_bridge: bool, bridge_ok: bool) -> bool {
        if !self.analyzed || self.built_epoch != epoch {
            return true;
        }
        if !want_bridge {
            return false; // STS is already in hand; no web process needed to hand it over
        }
        if !self.discovered {
            return true; // an analysis-only pass got us here — the webview was never built
        }
        // An undecipherable player owes no webview: discovery already proved there is nothing to
        // call, and a rebuild would find the same thing.
        keep_bridge(self.sig_available, self.n_available) && !bridge_ok
    }

    /// Whether the webview has gone unused for `idle` (never-used counts as idle).
    fn idle_for(&self, idle: Duration) -> bool {
        !self.last_used.is_some_and(|t| t.elapsed() < idle)
    }

    /// Destroy the webview, if there is one. `last_used` goes with it, since there is nothing
    /// left for the idle teardown to time.
    fn drop_bridge(&mut self) {
        if let Some(b) = self.bridge.take() {
            let _ = b.destroy();
            self.last_used = None;
        }
    }

    /// Forget the analysis and the webview, so the next `ensure_analyzed` re-fetches player.js
    /// and rebuilds from scratch. Only ever called under the build lock
    /// (see [`CipherDeobfuscator::build`]).
    fn invalidate(&mut self) {
        self.analyzed = false;
        self.discovered = false;
        self.drop_bridge();
        self.generation += 1;
        // The next build starts from a fresh player.js, so an earlier failure says nothing about it.
        self.build_error = None;
    }

    /// The error of a build that finished, and failed, after the caller saw `seen` builds. That
    /// caller queued behind it, and takes its error rather than running the same build again
    /// straight away. Resolves are never cancelled, so otherwise the playing track, the lookahead
    /// and any downloads would each wait out a whole failed attempt in turn (about 12 s for a
    /// webview that never comes up), where before the build lock they failed together. The next
    /// caller to arrive after the failure tries afresh.
    fn failed_since(&self, seen: u64) -> Option<String> {
        if self.builds == seen {
            return None;
        }
        self.build_error.clone()
    }

    /// Count a finished build, and remember its error for the callers queued behind it.
    fn record_build(&mut self, result: &Result<(), String>) {
        self.builds += 1;
        self.build_error = result.as_ref().err().cloned();
    }
}

pub struct CipherDeobfuscator {
    app: AppHandle,
    fetcher: PlayerJsFetcher,
    config: Arc<PlayerConfigStore>,
    inner: Mutex<Inner>,
    /// Serialises everything that analyses, builds or invalidates: `ensure_analyzed` when it owes
    /// work, the self-heal in `deobfuscate_stream_url`, and `on_stream_rejected`.
    ///
    /// The webview lives under one fixed label, and `Bridge::create` starts by reclaiming that
    /// label. Two resolves that both found the webview missing (the playing track and the
    /// lookahead, or a download) each destroyed the other's window mid-load, and both fell through
    /// to the non-web clients. The invalidations need it too: landing in the middle of a build,
    /// they were undone by that build's commit, which marked the stale analysis fresh again.
    ///
    /// Double-checked: `ensure_analyzed` takes it only when work is owed and asks again once it
    /// holds it, so a resolve whose STS (or webview) is already in hand never queues behind a
    /// build. The build it waited behind has usually done its work. If that build failed instead,
    /// the caller returns its error rather than repeating it (see [`Inner::failed_since`]).
    ///
    /// Lock order is `build`, then `inner`. `inner` is only held for short sections with no
    /// `.await` inside them, and never while waiting for `build`. `build` is never held across a
    /// call into `ensure_analyzed` or `try_deobfuscate`, which take it themselves: a tokio
    /// `Mutex` is not re-entrant, so that would deadlock. Nothing awaited under it (the player.js
    /// fetch, the webview's create, load and evals on the main thread) takes either lock.
    /// `teardown_if_idle` takes only `inner`, which is safe because a build empties the bridge
    /// slot before it starts and refills it only when it finishes.
    build: Mutex<()>,
}

impl CipherDeobfuscator {
    pub fn new(app: AppHandle, app_data_dir: &Path, config: Arc<PlayerConfigStore>) -> Self {
        CipherDeobfuscator {
            fetcher: PlayerJsFetcher::new(app_data_dir),
            config,
            inner: Mutex::new(Inner::default()),
            build: Mutex::new(()),
            app,
        }
    }

    /// STS of the player.js we decipher with (preferred over any other source). context/05.
    ///
    /// Once analysis is in hand this never waits for a webview build (see [`Self::build`]).
    pub async fn signature_timestamp(&self) -> Option<i32> {
        if self.ensure_analyzed(false).await.is_err() {
            return None;
        }
        self.inner.lock().await.sts
    }

    /// `signatureCipher` string → a full, signed stream URL. `None` on any failure. context/05.
    pub async fn deobfuscate_stream_url(&self, cipher: &str, video_id: &str) -> Option<String> {
        if self.ensure_analyzed(true).await.is_err() {
            return None;
        }
        // No sig function on this player (obfuscation defeated extraction) → deciphering is
        // impossible and a fresh player.js won't change that. Skip the refetch/rebuild churn and
        // let the orchestrator degrade to the direct clients. context/05 (config table is the fix).
        if !self.inner.lock().await.sig_available {
            return None;
        }
        let failed_on = match self.try_deobfuscate(cipher).await {
            Ok(u) => return Some(u),
            Err(generation) => generation,
        };
        // One self-heal retry: a stale player.js can silently produce a wrong signature. context/05.
        //
        // Under the build lock, so the invalidation lands between builds and never inside one, and
        // only if nothing has healed or rebuilt since this attempt ran (see `Inner::generation`).
        // The lock pins `generation`, which only moves under it, and is dropped again before the
        // retry, whose `ensure_analyzed` takes it to rebuild.
        {
            let _build = self.build.lock().await;
            let heal = self.inner.lock().await.generation == failed_on;
            if heal {
                tracing::warn!(
                    video_id,
                    "decipher failed — refetching player.js and retrying once"
                );
                self.fetcher.invalidate();
                self.inner.lock().await.invalidate();
            } else {
                tracing::debug!(
                    video_id,
                    "decipher failed, but another resolve has healed since — retrying on its webview"
                );
            }
        }
        self.try_deobfuscate(cipher).await.ok()
    }

    /// One decipher attempt. On failure, the error is the [`Inner::generation`] the attempt ran
    /// under, for the caller's self-heal to compare.
    async fn try_deobfuscate(&self, cipher: &str) -> Result<String, u64> {
        let ensured = self.ensure_analyzed(true).await;
        let (bridge, generation) = {
            let mut inner = self.inner.lock().await;
            inner.last_used = Some(Instant::now());
            (inner.bridge.clone(), inner.generation)
        };
        let (Ok(()), Some(bridge), Some((s, sp, base))) = (ensured, bridge, parse_cipher(cipher))
        else {
            return Err(generation);
        };
        let js = format!(
            "(function(){{try{{return String(window._cipherSigFunc({}));}}catch(e){{return null;}}}})()",
            js_string(&s)
        );
        let sig = match bridge.eval_json(js, CALL_TIMEOUT).await {
            Ok(Value::String(sig)) if !sig.is_empty() => sig,
            _ => return Err(generation),
        };
        let sep = if base.contains('?') { '&' } else { '?' };
        Ok(format!("{base}{sep}{sp}={}", urlencoding::encode(&sig)))
    }

    /// Replace `&n=` with its throttling-deobfuscated value. Returns the URL UNCHANGED on any
    /// failure so playback still attempts (context/05). Only meaningful for web clients.
    pub async fn transform_n_param_in_url(&self, url: &str) -> String {
        match self.try_transform_n(url).await {
            Some(u) => u,
            None => url.to_owned(),
        }
    }

    async fn try_transform_n(&self, url: &str) -> Option<String> {
        self.ensure_analyzed(true).await.ok()?;
        let mut inner = self.inner.lock().await;
        if !inner.n_available {
            return None;
        }
        inner.last_used = Some(Instant::now());
        let bridge = inner.bridge.clone()?;
        drop(inner);

        let re = regex::Regex::new(r"[?&]n=([^&]+)").ok()?;
        let enc = re.captures(url)?.get(1)?.as_str().to_owned();
        let decoded = urlencoding::decode(&enc).ok()?.into_owned();
        let js = format!(
            "(function(){{try{{return String(window._nTransformFunc({}));}}catch(e){{return null;}}}})()",
            js_string(&decoded)
        );
        match bridge.eval_json(js, CALL_TIMEOUT).await.ok()? {
            Value::String(newn) if !newn.is_empty() && newn != decoded => Some(url.replacen(
                &format!("n={enc}"),
                &format!("n={}", urlencoding::encode(&newn)),
                1,
            )),
            _ => None,
        }
    }

    /// Self-heal after a 403 on a deciphered URL: refresh the config table + invalidate player.js.
    /// Returns true if something changed (caller may clear WEB_REMIX failure memory). context/05, 06.
    pub async fn on_stream_rejected(&self) -> bool {
        // The registry refresh is network I/O and touches none of our state, so it runs before
        // the build lock rather than holding every decipher up behind it.
        let table_changed = self.config.refresh_after_stream_rejection().await;
        // The invalidation waits out any build in flight (see `build`); the next ensure_analyzed
        // then rebuilds from a fresh player.js.
        let _build = self.build.lock().await;
        self.fetcher.invalidate();
        self.inner.lock().await.invalidate();
        table_changed
    }

    /// Warm the player.js cache + analysis off the first-play path (context/04 §startup).
    ///
    /// Analysis ONLY: it deliberately does not build the webview. That used to happen here, which
    /// meant an app that was merely open (never played a note) carried a second
    /// `WebKitWebProcess` for the whole session — measured at 91 MiB PSS / 234 MiB RSS on Fedora.
    /// STS is what the /player request actually needs at startup, and analysis alone produces it;
    /// the webview is built by the first call that has a signature to decipher.
    pub async fn prewarm(&self) {
        if let Err(e) = self.ensure_analyzed(false).await {
            tracing::warn!(error = %e, "cipher prewarm failed (will retry on demand)");
        }
    }

    /// Drop the webview if sig/n haven't been needed for `idle` — the same mint-and-drop policy
    /// the BotGuard isolate uses (Phase-0 hybrid decision), now that the webview is built on
    /// demand rather than held for the life of the process. The analysis survives, so a rebuild is
    /// a disk-cached player.js plus one injection (~400ms), and STS keeps answering meanwhile.
    ///
    /// The idle window has to outlast a track: sig/n run once per resolve, so a shorter one would
    /// tear down and rebuild once per song, with the rebuild landing on the play path.
    // ponytail: called from the periodic task in lib.rs that already ticks for PoToken.
    pub async fn teardown_if_idle(&self, idle: Duration) {
        let mut inner = self.inner.lock().await;
        if inner.bridge.is_none() || !inner.idle_for(idle) {
            return;
        }
        inner.drop_bridge();
        tracing::debug!("cipher webview torn down (idle)");
    }

    /// Ensure player.js analysis (STS + config lookup) is fresh for the current config epoch.
    ///
    /// With `want_bridge`, also ensure the cipher webview exists and discovery has run — but only
    /// when the player is decipherable at all; otherwise the webview is destroyed/never built and
    /// the analysis alone satisfies `signature_timestamp`. Callers that only need STS pass `false`
    /// and never pay for a web process (see [`Self::prewarm`]).
    async fn ensure_analyzed(&self, want_bridge: bool) -> Result<(), String> {
        // Double-checked (see `build`): the common case owes nothing and leaves without touching
        // the build lock. Once it is held, ask again, with the epoch read afresh, because the
        // build we queued behind has usually done our work.
        let Some(seen) = self.work_owed(self.config.config_epoch(), want_bridge).await else {
            return Ok(());
        };
        let _build = self.build.lock().await;
        let epoch = self.config.config_epoch();
        if self.work_owed(epoch, want_bridge).await.is_none() {
            return Ok(());
        }
        if let Some(e) = self.inner.lock().await.failed_since(seen) {
            return Err(e);
        }
        let result = self.analyze_and_build(epoch, want_bridge).await;
        self.inner.lock().await.record_build(&result);
        result
    }

    /// The work [`Self::ensure_analyzed`] owes, run under the build lock. Split out so that every
    /// way it can end, the early returns included, is recorded in one place for the callers
    /// queued behind it.
    async fn analyze_and_build(&self, epoch: u64, want_bridge: bool) -> Result<(), String> {
        // Fetch player.js and look up its config — the only way in on the 2025+ players.
        let player = tokio::time::timeout(FETCH_TIMEOUT, self.fetcher.fetch())
            .await
            .map_err(|_| format!("player.js fetch timed out after {FETCH_TIMEOUT:?}"))?
            .map_err(|e| e.to_string())?;
        let cfg = self.config.get(&player.hash);
        if cfg.is_none() {
            // Unknown player hash — pull the registries off the hot path; a validated config for it
            // lands on the next rebuild (context/05 §forceRefresh). This run can't decipher.
            let config = self.config.clone();
            tauri::async_runtime::spawn(async move {
                config.force_refresh().await;
            });
        }
        // STS still comes from player.js when the registry hasn't listed this hash yet: it is a
        // plain literal and stays reliably greppable, and a correct STS keeps the /player requests
        // valid for the direct-URL clients even while deciphering is impossible.
        let sts = cfg.as_ref().and_then(|c| c.sts).or_else(|| extractor::extract_sts(&player.js));
        // No config for this player means `build_injection` splices no exports at all, so the
        // webview could only discover what we already know. Skip it rather than spend a whole web
        // process and a 2.9 MB injection proving it. Keep the analysis, which is where STS comes
        // from. Re-probed as soon as the answer could change: a rotated player.js, or a registry
        // entry landing for this hash (then `cfg` is `Some` and we fall through). context/05, KI-1.
        if cfg.is_none() {
            let mut inner = self.inner.lock().await;
            inner.drop_bridge();
            inner.sts = sts;
            inner.built_epoch = epoch;
            inner.n_available = false;
            inner.sig_available = false;
            inner.analyzed = true;
            inner.discovered = true; // nothing to discover: no config means no exports to probe
            inner.last_used = None;
            tracing::info!(
                hash = player.hash,
                ?sts,
                "cipher: no player config for this hash — skipping the webview build (KI-1)"
            );
            return Ok(());
        }
        // Record what we learned before any webview work. Discovery stays unset, so the first
        // decipher call falls through to the build below. Recording it first means a failed build
        // still leaves STS in hand: a resolve that only wants STS and queued behind this one then
        // finds nothing owed, instead of taking the error of a webview it never needed.
        {
            let mut inner = self.inner.lock().await;
            inner.drop_bridge(); // player.js rotated or the config epoch moved — it's stale
            inner.sts = sts;
            inner.built_epoch = epoch;
            inner.analyzed = true;
            inner.discovered = false;
        }
        // Analysis-only caller: stop short of the web process.
        if !want_bridge {
            tracing::info!(hash = player.hash, ?sts, "cipher: analysis complete (no webview)");
            return Ok(());
        }

        tracing::info!(hash = player.hash, ?sts, "cipher: building webview");
        let injected = extractor::build_injection(&player.js, cfg.as_ref());

        // The stale webview went with the old analysis above; create fresh and load the player.
        let bridge = Bridge::create(&self.app, CIPHER_LABEL).await.map_err(|e| e.to_string())?;
        if let Err(e) = Self::load_player(&bridge, &injected).await {
            let _ = bridge.destroy(); // don't orphan the hidden window on a failed load
            return Err(e);
        }
        let n_available = matches!(
            bridge.eval_json("window.__n_ok?true:false".into(), CALL_TIMEOUT).await,
            Ok(Value::Bool(true))
        );
        let sig_available = matches!(
            bridge.eval_json("window.__sig_ok?true:false".into(), CALL_TIMEOUT).await,
            Ok(Value::Bool(true))
        );

        let mut inner = self.inner.lock().await;
        if keep_bridge(sig_available, n_available) {
            inner.bridge = Some(bridge);
            inner.last_used = Some(Instant::now());
        } else {
            tracing::info!(
                "cipher: discovery found no usable sig/n on this player — dropping the webview \
                 (KI-1; rebuilt on config-epoch change or self-heal)"
            );
            let _ = bridge.destroy();
            inner.bridge = None;
            inner.last_used = None;
        }
        // STS, the epoch and `analyzed` were recorded before the build, and nothing moves them
        // without the build lock, which this build still holds.
        inner.n_available = n_available;
        inner.sig_available = sig_available;
        inner.discovered = true;
        inner.generation += 1;
        tracing::info!(sig_available, n_available, "cipher analysis complete");
        Ok(())
    }

    /// [`Inner::owes_work`] against the live state, asking Tauri whether the webview is still up.
    /// When work is owed, returns how many builds had finished by then (see [`Inner::builds`]).
    async fn work_owed(&self, epoch: u64, want_bridge: bool) -> Option<u64> {
        let inner = self.inner.lock().await;
        let bridge_ok = inner.bridge.as_ref().is_some_and(|b| b.exists());
        inner.owes_work(epoch, want_bridge, bridge_ok).then_some(inner.builds)
    }

    /// Inject player.js + discovery into a freshly-built cipher `bridge` and wait for discovery to
    /// finish. Split out so `ensure_analyzed` can destroy the webview on any of these failures.
    async fn load_player(bridge: &Bridge, injected: &str) -> Result<(), String> {
        bridge.eval(injected).map_err(|e| e.to_string())?;
        bridge.eval(DISCOVERY_JS).map_err(|e| e.to_string())?;
        // Wait for discovery to finish, then the caller reads whether n/sig are usable.
        bridge
            .call_async("window.__cipher_loaded?true:new Promise(r=>{var i=setInterval(()=>{if(window.__cipher_loaded){clearInterval(i);r(true);}},50);})", LOAD_TIMEOUT)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Parse a `signatureCipher` query string → `(s, sp, base_url)` with values percent-decoded.
/// `sp` defaults to `"signature"` (context/05). Returns `None` if `s` or `url` is missing.
fn parse_cipher(cipher: &str) -> Option<(String, String, String)> {
    let mut s = None;
    let mut sp = None;
    let mut url = None;
    for pair in cipher.split('&') {
        let (k, v) = pair.split_once('=')?;
        let v = urlencoding::decode(v).ok()?.into_owned();
        match k {
            "s" => s = Some(v),
            "sp" => sp = Some(v),
            "url" => url = Some(v),
            _ => {}
        }
    }
    Some((s?, sp.unwrap_or_else(|| "signature".into()), url?))
}

/// A JS string literal for the given value (properly escaped via JSON).
fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Whether a built webview is worth keeping resident (context/05 + Phase-0 hybrid decision).
fn keep_bridge(sig_available: bool, n_available: bool) -> bool {
    sig_available || n_available
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_signature_cipher() {
        let c = "s=ABC%3D%3D&sp=sig&url=https%3A%2F%2Fx.com%2Fv%3Fitag%3D251";
        let (s, sp, url) = parse_cipher(c).unwrap();
        assert_eq!(s, "ABC==");
        assert_eq!(sp, "sig");
        assert_eq!(url, "https://x.com/v?itag=251");
    }

    #[test]
    fn cipher_defaults_sp_to_signature() {
        let (_, sp, _) = parse_cipher("s=X&url=https%3A%2F%2Fx.com").unwrap();
        assert_eq!(sp, "signature");
    }

    #[test]
    fn cipher_missing_url_is_none() {
        assert!(parse_cipher("s=X&sp=sig").is_none());
    }

    #[test]
    fn js_string_escapes() {
        assert_eq!(js_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    /// The state machine the memory win rests on: an analysis-only pass must satisfy an STS
    /// caller without a webview, and must still leave the first decipher call to build one.
    #[test]
    fn analysis_only_pass_owes_a_bridge_to_the_first_decipher() {
        let analysed = Inner { analyzed: true, built_epoch: 7, ..Inner::default() };
        assert!(!analysed.owes_work(7, false, false), "STS needs no webview");
        assert!(analysed.owes_work(7, true, false), "decipher must build one");
        assert!(analysed.owes_work(8, false, false), "a new config epoch re-analyses");
    }

    #[test]
    fn a_discovered_player_owes_a_bridge_only_when_its_window_is_gone() {
        let discovered = Inner {
            analyzed: true,
            discovered: true,
            sig_available: true,
            built_epoch: 7,
            ..Inner::default()
        };
        assert!(!discovered.owes_work(7, true, true), "webview is up — nothing owed");
        assert!(discovered.owes_work(7, true, false), "torn down while idle — rebuild it");

        // Discovery proved there is nothing callable, so a missing webview is not a debt.
        let undecipherable = Inner { sig_available: false, ..discovered };
        assert!(!undecipherable.owes_work(7, true, false));
    }

    /// Both self-heals go through `invalidate`. It must leave STS and the webview owed again, and
    /// move the generation, so a second heal queued behind it on the build lock stands down.
    #[test]
    fn invalidation_owes_everything_again_and_moves_the_generation() {
        let mut inner = Inner {
            analyzed: true,
            discovered: true,
            sig_available: true,
            built_epoch: 7,
            ..Inner::default()
        };
        let failed_on = inner.generation;
        inner.invalidate();
        assert!(inner.owes_work(7, false, true), "STS must be re-analysed");
        assert!(inner.owes_work(7, true, true), "and the webview rebuilt");
        assert_ne!(inner.generation, failed_on, "a heal from before this one must stand down");
    }

    /// A failed build fails only the callers that were already queued behind it. Anyone arriving
    /// later, or after a success or an invalidation, builds again.
    #[test]
    fn a_failed_build_is_shared_with_its_queue_and_not_beyond() {
        let mut inner = Inner::default();
        let queued = inner.builds;
        inner.record_build(&Err("webview never became ready".into()));
        assert_eq!(inner.failed_since(queued).as_deref(), Some("webview never became ready"));
        assert_eq!(inner.failed_since(inner.builds), None, "a later caller tries afresh");

        let queued = inner.builds;
        inner.record_build(&Ok(()));
        assert_eq!(inner.failed_since(queued), None, "a success leaves no error to share");

        let queued = inner.builds;
        inner.record_build(&Err("player.js fetch timed out".into()));
        inner.invalidate();
        assert_eq!(inner.failed_since(queued), None, "an invalidation changes the next build");
    }

    #[test]
    fn idle_teardown_waits_for_the_window() {
        let fresh = Inner { last_used: Some(Instant::now()), ..Inner::default() };
        assert!(!fresh.idle_for(Duration::from_secs(600)));
        assert!(fresh.idle_for(Duration::ZERO));
        // Built but never used (or already torn down) counts as idle.
        assert!(Inner::default().idle_for(Duration::from_secs(600)));
    }

    #[test]
    fn keep_bridge_only_when_sig_or_n_available() {
        assert!(keep_bridge(true, true));
        assert!(keep_bridge(true, false));
        assert!(keep_bridge(false, true));
        assert!(!keep_bridge(false, false));
    }
}
