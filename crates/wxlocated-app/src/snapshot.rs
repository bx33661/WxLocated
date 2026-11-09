//! Dev only (`--features snapshot`): renders windows to PNG without screen
//! recording permission, then quits. Used to check the UI from the terminal.
//!
//!     WXLOCATED_SNAPSHOT=<dir> [WXLOCATED_SNAPSHOT_APP=<appid>] cargo run -p wxlocated-app --features snapshot
//!
//! Writes `<dir>/gallery.png`, `<dir>/gallery-selected.png` and, with an app,
//! the app window's views.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::*;

use crate::library::Library;

pub fn schedule(gallery: AnyWindowHandle, gallery_view: Entity<crate::gallery::Gallery>, cx: &mut App) {
    let Some(directory) = std::env::var_os("WXLOCATED_SNAPSHOT").map(PathBuf::from) else { return };
    let app_id = std::env::var("WXLOCATED_SNAPSHOT_APP").ok();
    let _ = std::fs::create_dir_all(&directory);
    cx.spawn(async move |cx| {
        let library = cx.update(|cx| Library::global(cx));
        cx.background_executor().timer(Duration::from_millis(60)).await;
        capture(gallery, &directory.join("gallery-loading.png"), cx);
        for _ in 0..600 {
            let ready = library.read_with(cx, |library, _| library.snapshot.is_some() && !library.scanning);
            if ready {
                break;
            }
            cx.background_executor().timer(Duration::from_millis(200)).await;
        }
        settle(cx).await;
        capture(gallery, &directory.join("gallery.png"), cx);

        let first = app_id.clone().or_else(|| library.read_with(cx, |library, _| library.apps().first().map(|a| a.app_id.clone())));
        if let Some(id) = first.clone() {
            cx.update(|cx| {
                gallery
                    .update(cx, |_, window, cx| gallery_view.update(cx, |gallery, cx| gallery.select_for_snapshot(&id, window, cx)))
                    .ok();
            });
            settle(cx).await;
            capture(gallery, &directory.join("gallery-selected.png"), cx);
            cx.update(|cx| gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx));
            settle(cx).await;
            capture(gallery, &directory.join("gallery-dark.png"), cx);
            cx.update(|cx| crate::theme::apply(cx));
        }
        cx.update(|cx| crate::settings_view::open_settings(None, cx));
        settle(cx).await;
        if let Some(handle) = cx.update(|cx| crate::settings_view::window(cx)) {
            use crate::settings_view::Tab;
            for (tab, name) in [(Tab::General, "general"), (Tab::Scan, "scan"), (Tab::Agent, "agent"), (Tab::Privacy, "privacy"), (Tab::About, "about")] {
                cx.update(|cx| crate::settings_view::snapshot_tab(handle, tab, cx));
                settle(cx).await;
                capture(handle, &directory.join(format!("settings-{name}.png")), cx);
            }
        }
        if let Some(id) = app_id {
            cx.update(|cx| crate::workspace::open_app_window(&id, cx));
            cx.background_executor().timer(Duration::from_millis(250)).await;
            if let Some(handle) = library.read_with(cx, |library, _| library.app_windows.get(&id).copied()) {
                capture(handle, &directory.join("app-loading.png"), cx);
            }
            settle(cx).await;
            let handle = library.read_with(cx, |library, _| library.app_windows.get(&id).copied());
            if let Some(handle) = handle {
                // Wait for the rebuild, then each view.
                for _ in 0..600 {
                    let done = crate::workspace::snapshot_ready(handle, cx);
                    if done {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                }
                crate::workspace::snapshot_step(handle, "overview", cx);
                settle(cx).await;
                capture(handle, &directory.join("app-summary.png"), cx);
                for (index, step) in crate::workspace::SNAPSHOT_STEPS.iter().enumerate() {
                    crate::workspace::snapshot_step(handle, step, cx);
                    settle(cx).await;
                    capture(handle, &directory.join(format!("app-{index}-{step}.png")), cx);
                }
                cx.update(|cx| gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx));
                crate::workspace::snapshot_step(handle, "file", cx);
                settle(cx).await;
                capture(handle, &directory.join("app-dark-file.png"), cx);
                crate::workspace::snapshot_step(handle, "overview", cx);
                settle(cx).await;
                capture(handle, &directory.join("app-dark-summary.png"), cx);
                cx.update(|cx| crate::theme::apply(cx));
            }
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn settle(cx: &mut AsyncApp) {
    cx.background_executor().timer(Duration::from_millis(1500)).await;
}

fn capture(handle: AnyWindowHandle, path: &Path, cx: &mut AsyncApp) {
    let result = cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            // A background window does not repaint on its own: draw a frame now.
            window.refresh();
            let arena = window.draw(cx);
            let image = window.render_to_image();
            arena.clear(cx);
            image
        })
    });
    match result {
        Ok(Ok(image)) => match image.save(path) {
            Ok(()) => eprintln!("snapshot: {}", path.display()),
            Err(error) => eprintln!("snapshot: {} failed: {error}", path.display()),
        },
        Ok(Err(error)) => eprintln!("snapshot: render failed: {error}"),
        Err(error) => eprintln!("snapshot: window gone: {error}"),
    }
}
