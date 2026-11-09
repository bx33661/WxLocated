//! Finder, Dock and window drops share one import route.

use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use crate::library::Library;

/// URL parsing also handles localhost, Unicode, spaces and encoded path bytes.
pub fn open_urls(urls: Vec<String>, cx: &mut App) {
    let mut paths = Vec::with_capacity(urls.len());
    for value in urls {
        match file_path(&value) {
            Ok(path) => paths.push(path),
            Err(message) => report_error(message, cx),
        }
    }
    open_paths(&paths, cx);
}

fn file_path(value: &str) -> Result<PathBuf, String> {
    url::Url::parse(value)
        .ok()
        .filter(|url| url.scheme() == "file")
        .and_then(|url| url.to_file_path().ok())
        .ok_or_else(|| format!("无法读取文件地址：{value}。请打开本机 .wxapkg 文件或含 overlay.json 的文件夹。"))
}

/// Overlay folders never fall through to package scanning. Multiple overlay
/// folders and package files in the same drop are each forwarded to their owner.
pub fn open_paths(paths: &[PathBuf], cx: &mut App) {
    let mut packages = Vec::new();
    let mut overlays = Vec::new();
    for path in paths {
        if path.is_dir() && path.join("overlay.json").exists() {
            overlays.push(path.clone());
        } else if path.is_file() && path.file_name().is_some_and(|name| name == "overlay.json") {
            if let Some(parent) = path.parent() {
                overlays.push(parent.to_path_buf());
            }
        } else if path.is_dir() || path.is_file() && path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("wxapkg")) {
            packages.push(path.clone());
        } else {
            report_error(format!("无法打开 {}：请选择可读取的 .wxapkg 文件、包目录或含 overlay.json 的重建层目录。", path.display()), cx);
        }
    }
    // Register the base app before routing layers from the same drop to it.
    if !packages.is_empty() {
        let id = Library::global(cx).update(cx, |library, cx| library.open_external(&packages, cx));
        if let Some(id) = id {
            crate::workspace::open_app_window(&id, cx);
        } else {
            let names = packages.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("、");
            report_error(format!("{names} 中没有可读取的 .wxapkg 包。包目录应直接包含 .wxapkg 文件；重建层目录应包含 overlay.json。"), cx);
        }
    }
    for work in overlays {
        crate::workspace::open_overlay_folder(work, cx);
    }
}

pub fn report_error(message: impl Into<SharedString>, cx: &mut App) {
    crate::focus_gallery(cx);
    let message = message.into();
    if let Some(handle) = cx.try_global::<crate::GalleryWindow>().map(|window| window.0) {
        handle.update(cx, |_, window, cx| {
            window.push_notification(Notification::error(message).title("无法打开"), cx);
        }).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::file_path;

    #[test]
    fn native_file_urls_preserve_paths() {
        assert_eq!(file_path("file:///tmp/a%20b/x.wxapkg").unwrap().to_str(), Some("/tmp/a b/x.wxapkg"));
        assert_eq!(file_path("file://localhost/%E5%8C%85/overlay.json").unwrap().to_str(), Some("/包/overlay.json"));
        assert_eq!(file_path("file:///tmp/a%23b").unwrap().to_str(), Some("/tmp/a#b"));
        assert!(file_path("https://example.com/x").is_err());
        assert!(file_path("file://remote-host/tmp/x.wxapkg").is_err());
    }
}
