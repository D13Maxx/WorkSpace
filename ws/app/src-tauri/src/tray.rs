//! Closing the window while agents work hides Workspace to a tray icon, so
//! they finish; it quits by itself once the last one does.

use crate::state::AppState;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager as _};

const TRAY: &str = "working";

/// Hide the main window to the tray if any turn is running. Returns whether
/// it did; if not, the window closes as usual.
pub fn hide_if_working(app: &AppHandle) -> bool {
    let working = app.try_state::<AppState>().is_some_and(|s| s.working());
    let Some(main) = app.get_webview_window("main") else {
        return false;
    };
    if !working {
        return false;
    }
    if app.tray_by_id(TRAY).is_none() {
        if let Err(e) = add_tray(app) {
            // No tray to come back through: closing as usual is better than a
            // window that is gone with nothing to show it again.
            tracing::warn!(error = %e, "could not add the tray icon");
            return false;
        }
    }
    let _ = main.hide();
    true
}

fn add_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Workspace", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Stop the agents and quit", true, None::<&str>)?;
    let mut tray = TrayIconBuilder::with_id(TRAY)
        .tooltip("Workspace: agents are working. Click to open.")
        .menu(&Menu::with_items(app, &[&open, &quit])?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => quit_now(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Bring the main window back, as it was, and drop the tray icon.
pub fn show_main(app: &AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
    }
    let _ = app.remove_tray_by_id(TRAY);
}

/// After a turn ends: quit if the window was hidden and nothing else runs.
pub fn quit_if_done(app: &AppHandle) {
    let hidden = app
        .get_webview_window("main")
        .is_some_and(|w| !w.is_visible().unwrap_or(true));
    let working = app.try_state::<AppState>().is_some_and(|s| s.working());
    if hidden && !working {
        quit_now(app);
    }
}

/// Stop helpers' tools and terminals, then exit. Running turns stop; what
/// they wrote so far is already saved.
pub fn quit_now(app: &AppHandle) {
    crate::bridge::stop_all();
    if let Some(state) = app.try_state::<AppState>() {
        state.terminals.close_all();
    }
    app.exit(0);
}
