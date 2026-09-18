//! System tray: app icon + menu (Show / Play-Pause / Next / Previous / Restart / Quit). Menu actions
//! route into the same [`AppState`] methods the OS media keys use (see media.rs), so the tray
//! can never behave differently from MPRIS.
//!
//! Two backends, because clicking the icon to restore the window is unreachable on Linux
//! otherwise: Tauri's `tray-icon` talks to **libappindicator**, whose D-Bus item exposes no
//! `Activate` method at all (only `SecondaryActivate`/`Scroll`), so a left-click has nothing to
//! call and `TrayIconEvent` never fires — its GTK backend wires up zero signals. Electron apps
//! get click-to-restore by implementing StatusNotifierItem directly and advertising
//! `ItemIsMenu=false`; [`ksni`] does the same for us on Linux. Windows/macOS keep `tray-icon`.
//!
//! Both backends expose the same two entry points — [`init`] and [`set_playing`] — so lib.rs
//! never learns which one is live.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

pub use imp::{init, set_icon, set_playing};

/// What the tray names the track: "Title — Artist", cut short enough for a menu row.
pub fn now_playing_label(title: &str, artists: &str) -> String {
    const MAX: usize = 60;
    let full = if artists.trim().is_empty() {
        title.trim().to_owned()
    } else {
        format!("{} — {}", title.trim(), artists.trim())
    };
    if full.chars().count() <= MAX {
        return full;
    }
    let cut: String = full.chars().take(MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The track now playing, shown as the tray's tooltip and the first (inert) menu row.
pub fn set_now_playing(app: &AppHandle, title: &str, artists: &str) {
    imp::set_now_playing(app, now_playing_label(title, artists));
}

/// Bring the main window back from close-to-tray, minimize, or the mini player. Every "come back"
/// path — tray menu, tray click, second launch, the widget's restore button — goes through here so
/// they can't drift apart.
pub fn show_main(app: &AppHandle) {
    crate::mini::close(app);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        set_main_visible(app, true);
    }
}

/// Tell the main window's SPA whether anyone can see it.
///
/// WebKitGTK does not pass a GTK hide down to the page: `document.visibilityState` stays
/// `"visible"` for a window that is closed to the tray, so the SPA cannot work this out on its own
/// and keeps restyling for nobody. That is worse than wasted work. While the window is unmapped
/// the web process never gives any of it back: measured over 20 track changes with the window
/// hidden, it grew 137 MB and held it, then dropped the whole lot within two seconds of the window
/// being shown again. A night in the tray is thousands of those. See `theme.svelte.ts`.
pub fn set_main_visible(app: &AppHandle, visible: bool) {
    let _ = app.emit_to("main", "ui-visible", visible);
}

/// Shared by both backends: menu ids are the contract between them.
fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "show" => show_main(app),
        "quit" | "restart" => {
            // Users now quit mid-song from the tray; persist the exact resume position first.
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                state.flush_position();
            }
            // Same for the widget's own position, if that's what they were quitting from.
            crate::mini::save_position(app);
            if id == "restart" {
                // `request_restart`, not `restart`: it goes through RunEvent::Exit, which is
                // where the single-instance plugin releases the D-Bus name. Skip that and the
                // relaunched process hands off to the still-dying one and exits, leaving no app.
                //
                // ponytail: under `cargo tauri dev` this leaves you with no window. The CLI
                // exits when its child does, taking the vite server with it, so the relaunched
                // binary navigates to a dead devUrl and the transparent frameless window paints
                // nothing (taskbar + tray entry, no visible window). Release builds embed the
                // frontend, so restart is only usable there.
                app.request_restart();
            } else {
                app.exit(0);
            }
        }
        other => {
            let Some(state) = app.try_state::<Arc<AppState>>() else { return };
            let state = state.inner().clone();
            let id = other.to_string();
            tauri::async_runtime::spawn(async move {
                match id.as_str() {
                    "play_pause" => state.resume_or_toggle().await,
                    "next" => state.next_in_queue().await,
                    "prev" => state.prev_in_queue().await,
                    _ => {}
                }
            });
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::OnceLock;

    use ksni::menu::{MenuItem, StandardItem};
    use ksni::{Handle, Icon, Tray, TrayMethods};
    use tauri::AppHandle;

    use super::{handle_menu, show_main};

    /// `Handle` isn't `Clone`, so it lives here rather than in Tauri's managed state — that also
    /// keeps [`set_playing`] callable without borrowing across an await.
    static HANDLE: OnceLock<Handle<LimusicTray>> = OnceLock::new();

    struct LimusicTray {
        app: AppHandle,
        playing: bool,
        icon: Vec<Icon>,
        /// "Title — Artist" of the current track, empty before anything has played.
        now_playing: String,
    }

    impl Tray for LimusicTray {
        fn id(&self) -> String {
            "limusic".into()
        }

        fn title(&self) -> String {
            crate::APP_NAME.into()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            self.icon.clone()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: crate::APP_NAME.into(),
                description: self.now_playing.clone(),
                ..Default::default()
            }
        }

        /// The entire reason this backend exists: Plasma dispatches a left-click here.
        fn activate(&mut self, _x: i32, _y: i32) {
            show_main(&self.app);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let item = |label: &str, id: &'static str| {
                MenuItem::from(StandardItem {
                    label: label.into(),
                    activate: Box::new(move |t: &mut Self| handle_menu(&t.app, id)),
                    ..Default::default()
                })
            };
            let mut rows = Vec::new();
            if !self.now_playing.is_empty() {
                rows.push(MenuItem::from(StandardItem {
                    label: self.now_playing.clone(),
                    enabled: false,
                    ..Default::default()
                }));
                rows.push(MenuItem::Separator);
            }
            rows.extend([
                item(&format!("Show {}", crate::APP_NAME), "show"),
                MenuItem::Separator,
                item(if self.playing { "Pause" } else { "Play" }, "play_pause"),
                item("Next", "next"),
                item("Previous", "prev"),
                MenuItem::Separator,
                item("Restart", "restart"),
                item("Quit", "quit"),
            ]);
            rows
        }
    }

    /// Tauri hands us RGBA; StatusNotifierItem wants ARGB32 in network byte order.
    fn icon_pixmap(img: &tauri::image::Image<'_>) -> Vec<Icon> {
        let mut data = img.rgba().to_vec();
        for px in data.chunks_exact_mut(4) {
            px.rotate_right(1); // [R,G,B,A] -> [A,R,G,B]
        }
        vec![Icon { width: img.width() as i32, height: img.height() as i32, data }]
    }

    pub fn init(app: &AppHandle) -> tauri::Result<()> {
        let icon = crate::appicon::current(app).map(|i| icon_pixmap(&i)).unwrap_or_default();
        let tray =
            LimusicTray { app: app.clone(), playing: false, icon, now_playing: String::new() };
        // Registering with the StatusNotifierWatcher is async and can outlive setup(); a failure
        // here costs the tray, not the app, so it's logged rather than propagated.
        tauri::async_runtime::spawn(async move {
            match tray.spawn().await {
                Ok(handle) => {
                    let _ = HANDLE.set(handle);
                }
                Err(e) => tracing::error!("tray: StatusNotifierItem registration failed: {e}"),
            }
        });
        Ok(())
    }

    pub fn set_playing(_app: &AppHandle, playing: bool) {
        let Some(handle) = HANDLE.get() else { return };
        tauri::async_runtime::spawn(async move {
            handle.update(|t| t.playing = playing).await;
        });
    }

    pub fn set_now_playing(_app: &AppHandle, label: String) {
        let Some(handle) = HANDLE.get() else { return };
        tauri::async_runtime::spawn(async move {
            handle.update(|t| t.now_playing = label).await;
        });
    }

    pub fn set_icon(_app: &AppHandle, icon: &tauri::image::Image<'_>) {
        let Some(handle) = HANDLE.get() else { return };
        let pixmap = icon_pixmap(icon);
        tauri::async_runtime::spawn(async move {
            handle.update(|t| t.icon = pixmap).await;
        });
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
    use tauri::{AppHandle, Manager, Wry};

    use super::{handle_menu, show_main};

    /// Managed handle to the live-label item so the mpv event pump can flip "Play"/"Pause".
    struct TrayState {
        play_pause: MenuItem<Wry>,
        now_playing: MenuItem<Wry>,
    }

    pub fn init(app: &AppHandle) -> tauri::Result<()> {
        let show = MenuItem::with_id(
            app,
            "show",
            &format!("Show {}", crate::APP_NAME),
            true,
            None::<&str>,
        )?;
        let play_pause = MenuItem::with_id(app, "play_pause", "Play", true, None::<&str>)?;
        // Inert: a label, not an action. Names the app until a track starts.
        let now_playing =
            MenuItem::with_id(app, "now_playing", crate::APP_NAME, false, None::<&str>)?;
        let next = MenuItem::with_id(app, "next", "Next", true, None::<&str>)?;
        let prev = MenuItem::with_id(app, "prev", "Previous", true, None::<&str>)?;
        let restart = MenuItem::with_id(app, "restart", "Restart", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
        let menu = Menu::with_items(
            app,
            &[
                &now_playing,
                &PredefinedMenuItem::separator(app)?,
                &show,
                &PredefinedMenuItem::separator(app)?,
                &play_pause,
                &next,
                &prev,
                &PredefinedMenuItem::separator(app)?,
                &restart,
                &quit,
            ],
        )?;

        // Menu on right-click only, so a left double-click can't also pop it open behind the
        // window it just restored.
        let mut builder = TrayIconBuilder::with_id("main")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .tooltip(crate::APP_NAME)
            .on_menu_event(|app, event| handle_menu(app, event.id.as_ref()))
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                    show_main(tray.app_handle());
                }
            });
        if let Some(icon) = crate::appicon::current(app) {
            builder = builder.icon(icon);
        }
        builder.build(app)?;

        app.manage(TrayState { play_pause, now_playing });
        Ok(())
    }

    pub fn set_playing(app: &AppHandle, playing: bool) {
        if let Some(t) = app.try_state::<TrayState>() {
            let _ = t.play_pause.set_text(if playing { "Pause" } else { "Play" });
        }
    }

    pub fn set_now_playing(app: &AppHandle, label: String) {
        if let Some(t) = app.try_state::<TrayState>() {
            let _ = t.now_playing.set_text(&label);
        }
        if let Some(tray) = app.tray_by_id("main") {
            let _ = tray.set_tooltip(Some(format!("{}\n{label}", crate::APP_NAME)));
        }
    }

    pub fn set_icon(app: &AppHandle, icon: &tauri::image::Image<'_>) {
        if let Some(tray) = app.tray_by_id("main") {
            let _ = tray.set_icon(Some(icon.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::now_playing_label;

    #[test]
    fn the_tray_label_names_the_track_and_fits_a_menu() {
        assert_eq!(
            now_playing_label("Espresso", "Sabrina Carpenter"),
            "Espresso — Sabrina Carpenter"
        );
        assert_eq!(now_playing_label(" Intro ", ""), "Intro");
        let long = now_playing_label(&"ư".repeat(80), "Artist");
        // Counted in characters, not bytes: a Vietnamese title must not be cut mid-character.
        assert_eq!(long.chars().count(), 60);
        assert!(long.ends_with('…'));
    }
}
