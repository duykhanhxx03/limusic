//! Opening a URL in the user's real browser.
//!
//! Every outbound link leaves the app this way: an `<a href>` inside the webview would navigate
//! the SPA itself off the app, with no way back. Reached from `commands::open_external`.

/// Open a URL in the user's default browser. No opener plugin in the app; three lines cover the
/// three platforms.
pub(crate) fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let cmd = {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(url);
        unappimage(&mut cmd);
        cmd.spawn()
    };
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(url).spawn();
    // cmd.exe re-parses its own command line, and `Command::arg` only quotes args containing
    // spaces — so an unquoted `&` in a URL splits it into a second command and the browser gets
    // the query string chopped off at the first parameter. raw_arg passes the quoted URL through
    // verbatim.
    #[cfg(target_os = "windows")]
    let cmd = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd").raw_arg(format!("/C start \"\" \"{url}\"")).spawn()
    };
    cmd.map(|_| ()).map_err(|e| format!("Couldn't open the browser: {e}"))
}

/// Undo the AppImage's environment for a child process.
///
/// Inside an AppImage the AppDir is first on `LD_LIBRARY_PATH` (plus `GTK_PATH`, `GIO_MODULE_DIR`,
/// `XDG_DATA_DIRS`, …) and every child inherits it. `xdg-open` immediately hands off to a *host*
/// binary (`kde-open`, `gio`), which then links the host's libcurl against our bundled libssl,
/// built on Ubuntu 24.04 (OpenSSL 3.0) and missing every symbol version added since:
///
/// ```text
/// kde-open: /tmp/.mount_limusiXXXXXX/usr/lib/libssl.so.3: version `OPENSSL_3.5.0' not found
///           (required by /lib64/libcurl.so.4)
/// ```
///
/// The opener dies before the browser ever starts, and since `spawn` itself succeeded, the app
/// had no way to tell the link never opened (issue #50, Fedora 44 / KDE).
///
/// Same bug class as the bundled-libwayland breakage: our libs shadowing the host's for code we
/// don't control. Fixed the same way, by scope: strip AppDir paths from the child, keep the user's.
#[cfg(target_os = "linux")]
fn unappimage(cmd: &mut std::process::Command) {
    let Some(appdir) = std::env::var("APPDIR").ok().filter(|d| !d.is_empty()) else { return };
    for (key, value) in std::env::vars_os() {
        let (Some(key), Some(value)) = (key.to_str(), value.to_str()) else { continue };
        if !value.contains(&appdir) {
            continue;
        }
        match strip_appdir(value, &appdir) {
            Some(kept) => cmd.env(key, kept),
            None => cmd.env_remove(key),
        };
    }
}

/// Drop AppDir-rooted entries from a colon-separated env value. `None` means nothing survived, so
/// the variable should be unset rather than set to an empty string (an empty `LD_LIBRARY_PATH`
/// entry means "current directory").
#[cfg(target_os = "linux")]
fn strip_appdir(value: &str, appdir: &str) -> Option<String> {
    let kept: Vec<&str> =
        value.split(':').filter(|p| !p.starts_with(appdir) && !p.is_empty()).collect();
    (!kept.is_empty()).then(|| kept.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #50: what the child of `xdg-open` must not inherit.
    #[cfg(target_os = "linux")]
    #[test]
    fn strip_appdir_keeps_host_paths_and_drops_the_var_when_nothing_is_left() {
        let dir = "/tmp/.mount_limusiNnkaBP";
        // LD_LIBRARY_PATH as AppRun.wrapped writes it: AppDir dirs first, user's value appended.
        assert_eq!(
            strip_appdir(&format!("{dir}/usr/lib/:{dir}/usr/lib64/:/opt/mine/lib"), dir),
            Some("/opt/mine/lib".to_string())
        );
        // Nothing but AppDir dirs (the usual case): unset it, don't leave "" behind.
        assert_eq!(strip_appdir(&format!("{dir}/usr/lib:{dir}/usr/lib64"), dir), None);
        // A lone AppDir value, e.g. GSETTINGS_SCHEMA_DIR.
        assert_eq!(strip_appdir(dir, dir), None);
        // XDG_DATA_DIRS keeps the host's dirs, which is how xdg-open finds the .desktop handlers.
        assert_eq!(
            strip_appdir(&format!("{dir}/usr/share:/usr/share:/usr/local/share"), dir),
            Some("/usr/share:/usr/local/share".to_string())
        );
    }
}
