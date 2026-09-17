//! AutoEq headphone corrections for the equalizer.
//!
//! [AutoEq](https://github.com/jaakkopasanen/AutoEq) (MIT) publishes an equalization for about
//! 8,800 headphone measurements. One of its outputs, `FixedBandEQ.txt`, is exactly our equalizer:
//! a preamp plus ten peaking filters at 31 Hz … 16 kHz, Q 1.41, ±12 dB. So a correction is not a
//! new kind of filter, it is a preset for one pair of headphones, and choosing one fills the same
//! ten bands the sliders move. SimpMusic imports it the same way.
//!
//! Two things are fetched, both from the repo's raw files:
//! - `results/INDEX.md`, one markdown line per measurement. Parsed into `autoeq_entry` so a search
//!   is a local query, and refreshed at most weekly, conditionally on its ETag (the file is 850 KB
//!   and changes a few times a month).
//! - `results/<path>/<name> FixedBandEQ.txt` for the one headphone chosen. Kept in `autoeq_curve`
//!   for good: a curve never changes under its path, and once fetched the choice works offline.

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;

use crate::state::AppState;

const RESULTS: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results";
/// How long a fetched index is trusted before it is checked again.
const INDEX_TTL: i64 = 7 * 24 * 60 * 60;
/// Results per search. A query of one or two letters matches thousands; nobody scrolls past this.
pub const SEARCH_LIMIT: usize = 60;
const UA: &str =
    concat!("Limusic v", env!("CARGO_PKG_VERSION"), " (https://github.com/duykhanhxx03/limusic)");

/// One line of the index.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexEntry {
    /// Relative to `results/`, still percent-encoded, e.g. `oratory1990/over-ear/Sennheiser%20HD%20600`.
    pub path: String,
    pub name: String,
    /// Who measured it: `oratory1990`, `crinacle`, `Rtings` …
    pub source: String,
    /// The measurement rig, when the source used more than one.
    pub rig: Option<String>,
}

/// A search hit, as the picker lists it.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: String,
    pub name: String,
    pub source: String,
    pub rig: Option<String>,
    /// The curve is already on disk.
    pub cached: bool,
}

/// A correction: the preamp and one gain per band of `player::EQ_BANDS`, in dB.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Curve {
    pub preamp: f64,
    pub gains: Vec<f64>,
}

/// `- [Sennheiser HD 600](./crinacle/GRAS%2043AG-7%20over-ear/Sennheiser%20HD%20600) by crinacle on GRAS 43AG-7`
///
/// Lines that aren't entries (the heading, the blurb) are skipped. The path is `\S+` rather than
/// "up to the first `)`" because names carry parentheses of their own: `1MORE Aero (ANC Off)`.
pub fn parse_index(md: &str) -> Vec<IndexEntry> {
    static LINE: OnceLock<Regex> = OnceLock::new();
    let line = LINE.get_or_init(|| {
        Regex::new(r"^-\s+\[(.+)\]\((\S+)\)\s+by\s+(.+?)(?:\s+on\s+(.+))?$").unwrap()
    });
    md.lines()
        .filter_map(|l| {
            let c = line.captures(l.trim_end())?;
            Some(IndexEntry {
                path: c[2].trim_start_matches("./").to_string(),
                name: c[1].to_string(),
                source: c[3].to_string(),
                rig: c.get(4).map(|m| m.as_str().to_string()),
            })
        })
        .collect()
}

/// A `FixedBandEQ.txt`:
///
/// ```text
/// Preamp: -7.5 dB
/// Filter 1: ON PK Fc 31 Hz Gain 6.9 dB Q 1.41
/// …
/// Filter 10: ON PK Fc 16000 Hz Gain -6.5 dB Q 1.41
/// ```
///
/// Each filter lands on the band its frequency is within 5% of (AutoEq writes 31 where the octave
/// is 31.25). `None` unless all ten bands are filled: a partial curve applied as if it were whole
/// would be a correction for headphones nobody owns.
pub fn parse_fixed_band(txt: &str) -> Option<Curve> {
    static PREAMP: OnceLock<Regex> = OnceLock::new();
    static FILTER: OnceLock<Regex> = OnceLock::new();
    let preamp_rx =
        PREAMP.get_or_init(|| Regex::new(r"^Preamp:\s*(-?\d+(?:\.\d+)?)\s*dB").unwrap());
    let filter_rx = FILTER.get_or_init(|| {
        Regex::new(
            r"^Filter\s+\d+:\s+ON\s+PK\s+Fc\s+(\d+(?:\.\d+)?)\s+Hz\s+Gain\s+(-?\d+(?:\.\d+)?)\s+dB",
        )
        .unwrap()
    });
    let mut preamp = 0.0;
    let mut gains: Vec<Option<f64>> = vec![None; player::EQ_BANDS.len()];
    for line in txt.lines().map(str::trim) {
        if let Some(c) = preamp_rx.captures(line) {
            preamp = c[1].parse().ok()?;
        } else if let Some(c) = filter_rx.captures(line) {
            let fc: f64 = c[1].parse().ok()?;
            let gain: f64 = c[2].parse().ok()?;
            let band = player::EQ_BANDS
                .iter()
                .position(|&hz| (fc - hz as f64).abs() <= hz as f64 * 0.05)?;
            gains[band] = Some(gain);
        }
    }
    Some(Curve { preamp, gains: gains.into_iter().collect::<Option<Vec<_>>>()? })
}

fn get(url: String) -> reqwest::RequestBuilder {
    crate::http::client().get(url).header("User-Agent", UA).timeout(Duration::from_secs(20))
}

/// Make sure the index is there and no older than a week. Returns how many headphones it holds.
///
/// Stale data is still data: a failed check leaves the old index (and its timestamp, so the next
/// open tries again) and reports success as long as there is something to search. Only an empty
/// index with no way to fill it is an error.
pub async fn refresh(state: &AppState) -> Result<i64, String> {
    // One fetch at a time. Opening the dialog twice while the first check is still out would
    // otherwise download the file twice and race the two replacements.
    static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = RUNNING.lock().await;

    let now = crate::db::now_secs();
    let (count, etag, fetched_at) = state.db.autoeq_index_state();
    if count > 0 && fetched_at.is_some_and(|t| now - t < INDEX_TTL) {
        return Ok(count);
    }
    match fetch_index(etag.filter(|_| count > 0).as_deref()).await {
        Ok(None) => {
            state.db.touch_autoeq_index(now);
            Ok(count)
        }
        Ok(Some((entries, new_etag))) => {
            state
                .db
                .replace_autoeq_index(&entries, new_etag.as_deref(), now)
                .map_err(|e| e.to_string())?;
            Ok(entries.len() as i64)
        }
        Err(e) if count > 0 => {
            tracing::warn!(error = %e, "autoeq: index refresh failed, keeping the old one");
            Ok(count)
        }
        Err(e) => Err(e),
    }
}

/// `Ok(None)` is "unchanged since `etag`".
async fn fetch_index(
    etag: Option<&str>,
) -> Result<Option<(Vec<IndexEntry>, Option<String>)>, String> {
    let mut req = get(format!("{RESULTS}/INDEX.md"));
    if let Some(etag) = etag {
        req = req.header("If-None-Match", etag);
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    if res.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let res = res.error_for_status().map_err(|e| e.to_string())?;
    let etag =
        res.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
    let body = res.text().await.map_err(|e| e.to_string())?;
    let entries = parse_index(&body);
    // A 200 that parses to nothing is GitHub serving something else (an error page, a moved
    // file). Replacing a working index with it would empty the picker.
    if entries.is_empty() {
        return Err("AutoEq index had no entries".into());
    }
    Ok(Some((entries, etag)))
}

/// The correction for one headphone from the index, from disk if it was fetched before.
pub async fn curve(state: &AppState, path: &str) -> Result<Curve, String> {
    if let Some(hit) = state.db.autoeq_curve(path) {
        if hit.gains.len() == player::EQ_BANDS.len() {
            return Ok(hit);
        }
    }
    // Only paths the index listed: this one is spliced into a URL.
    if !state.db.autoeq_entry_exists(path) {
        return Err("Unknown headphone".into());
    }
    // The file is named after the last path segment, which is already percent-encoded.
    let name = path.rsplit('/').next().unwrap_or(path);
    let url = format!("{RESULTS}/{path}/{name}%20FixedBandEQ.txt");
    let body = get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let curve = parse_fixed_band(&body).ok_or("This correction couldn't be read")?;
    state.db.put_autoeq_curve(path, &curve);
    Ok(curve)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_lines_parse_with_and_without_a_rig() {
        let md = "# Index\nThis is a list of all equalization profiles.\n\n\
            - [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser%20HD%20600) by oratory1990\n\
            - [1MORE Aero (ANC Off)](./HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)) by HypetheSonics on GRAS RA0045\n";
        let e = parse_index(md);
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0],
            IndexEntry {
                path: "oratory1990/over-ear/Sennheiser%20HD%20600".into(),
                name: "Sennheiser HD 600".into(),
                source: "oratory1990".into(),
                rig: None,
            }
        );
        // The name's own parentheses stay out of the path, and the rig splits off the source.
        assert_eq!(e[1].name, "1MORE Aero (ANC Off)");
        assert_eq!(e[1].path, "HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)");
        assert_eq!(e[1].source, "HypetheSonics");
        assert_eq!(e[1].rig.as_deref(), Some("GRAS RA0045"));
    }

    const HD600: &str = "Preamp: -7.5 dB\n\
        Filter 1: ON PK Fc 31 Hz Gain 6.9 dB Q 1.41\n\
        Filter 2: ON PK Fc 62 Hz Gain 3.3 dB Q 1.41\n\
        Filter 3: ON PK Fc 125 Hz Gain -1.1 dB Q 1.41\n\
        Filter 4: ON PK Fc 250 Hz Gain -1.6 dB Q 1.41\n\
        Filter 5: ON PK Fc 500 Hz Gain 0.6 dB Q 1.41\n\
        Filter 6: ON PK Fc 1000 Hz Gain -0.8 dB Q 1.41\n\
        Filter 7: ON PK Fc 2000 Hz Gain 0.1 dB Q 1.41\n\
        Filter 8: ON PK Fc 4000 Hz Gain -1.0 dB Q 1.41\n\
        Filter 9: ON PK Fc 8000 Hz Gain 3.9 dB Q 1.41\n\
        Filter 10: ON PK Fc 16000 Hz Gain -6.5 dB Q 1.41\n";

    #[test]
    fn a_fixed_band_file_fills_the_ten_bands() {
        // The Sennheiser HD 600 file as AutoEq serves it.
        let c = parse_fixed_band(HD600).unwrap();
        assert_eq!(c.preamp, -7.5);
        assert_eq!(c.gains, vec![6.9, 3.3, -1.1, -1.6, 0.6, -0.8, 0.1, -1.0, 3.9, -6.5]);
    }

    #[test]
    fn a_curve_missing_a_band_is_rejected() {
        let partial: String =
            HD600.lines().filter(|l| !l.starts_with("Filter 7:")).collect::<Vec<_>>().join("\n");
        assert_eq!(parse_fixed_band(&partial), None);
        // A parametric file is not a fixed-band one, even though its filters parse.
        assert_eq!(
            parse_fixed_band("Preamp: -6.7 dB\nFilter 1: ON LSC Fc 105 Hz Gain 1.9 dB Q 0.70\n"),
            None
        );
    }

    #[test]
    fn search_matches_every_word_and_ranks_prefixes_first() {
        let db = crate::db::Db::open(std::path::Path::new(":memory:")).unwrap();
        let entry = |path: &str, name: &str, source: &str| IndexEntry {
            path: path.into(),
            name: name.into(),
            source: source.into(),
            rig: None,
        };
        db.replace_autoeq_index(
            &[
                entry("a/Sennheiser%20HD%20600", "Sennheiser HD 600", "oratory1990"),
                entry("b/Sennheiser%20HD%20650", "Sennheiser HD 650", "crinacle"),
                entry("c/Drop%20x%20HD%20600", "Drop x Sennheiser HD 600", "Rtings"),
                entry("d/50%25%20Off", "Brand_50% Off", "Rtings"),
            ],
            Some("W/\"x\""),
            1,
        )
        .unwrap();
        let names =
            |q: &str| db.search_autoeq(q, 10).into_iter().map(|e| e.name).collect::<Vec<_>>();
        assert_eq!(names("hd 600"), vec!["Drop x Sennheiser HD 600", "Sennheiser HD 600"]);
        // Same headphone, several measurements: the reference source first, not the alphabet.
        db.replace_autoeq_index(
            &[
                entry("x/1", "Sennheiser HD 600", "Auriculares Argentina"),
                entry("x/2", "Sennheiser HD 600", "crinacle"),
                entry("x/3", "Sennheiser HD 600", "oratory1990"),
            ],
            None,
            1,
        )
        .unwrap();
        let sources =
            db.search_autoeq("hd 600", 10).into_iter().map(|e| e.source).collect::<Vec<_>>();
        assert_eq!(sources, vec!["oratory1990", "crinacle", "Auriculares Argentina"]);
        db.replace_autoeq_index(
            &[
                entry("a/Sennheiser%20HD%20600", "Sennheiser HD 600", "oratory1990"),
                entry("b/Sennheiser%20HD%20650", "Sennheiser HD 650", "crinacle"),
                entry("c/Drop%20x%20HD%20600", "Drop x Sennheiser HD 600", "Rtings"),
                entry("d/50%25%20Off", "Brand_50% Off", "Rtings"),
            ],
            Some("W/\"x\""),
            1,
        )
        .unwrap();
        assert_eq!(names("sennheiser 600"), vec!["Sennheiser HD 600", "Drop x Sennheiser HD 600"]);
        // LIKE wildcards typed by the user are literal.
        assert_eq!(names("_50%"), vec!["Brand_50% Off"]);
        assert!(names("5_0").is_empty());
        assert_eq!(db.search_autoeq("", 2).len(), 2);

        assert!(!db.search_autoeq("HD 650", 1)[0].cached);
        db.put_autoeq_curve(
            "b/Sennheiser%20HD%20650",
            &Curve { preamp: -3.0, gains: vec![1.5; 10] },
        );
        assert!(db.search_autoeq("HD 650", 1)[0].cached);
        assert_eq!(db.autoeq_curve("b/Sennheiser%20HD%20650").unwrap().gains, vec![1.5; 10]);
        assert!(db.autoeq_entry_exists("a/Sennheiser%20HD%20600"));
        assert!(!db.autoeq_entry_exists("../../elsewhere"));
        assert_eq!(db.autoeq_index_state(), (4, Some("W/\"x\"".into()), Some(1)));
    }
}
