//! The app-wide model: the latest cache scan, decoded icons, settings, and
//! what each open window found. Scans run on the background executor; every
//! window observes this entity.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui_kit::*;
use wxlocated_engine::scan::{self, Miniapp, Snapshot};
use wxlocated_engine::settings::Settings;

/// Per-app facts derived once per scan, off the main thread.
#[derive(Clone, Copy, Default)]
pub struct Derived {
    /// Declared subpackages not in the cache.
    pub missing: usize,
    /// Whether the app declares subpackages at all.
    pub declares: bool,
    pub plugins: usize,
    pub missing_plugins: usize,
    /// Glow colour sampled from the icon.
    pub ambient: Option<Hsla>,
}

impl Derived {
    pub fn is_incomplete(&self) -> bool {
        self.missing > 0 || self.missing_plugins > 0
    }
}

/// Snapshot mode renders the UI for checks; it must not touch the user's
/// settings (recent apps, last files, names).
pub fn persists() -> bool {
    std::env::var_os("WXLOCATED_SNAPSHOT").is_none()
}

/// What a rebuild produced, shown on gallery tiles.
#[derive(Clone, Copy)]
pub struct RebuildSummary {
    pub generated: usize,
    pub degraded: usize,
}

pub struct Library {
    pub snapshot: Option<Arc<Snapshot>>,
    pub scanning: bool,
    pub last_scan: Option<SystemTime>,
    pub settings: Settings,
    /// The latest failed save; edits remain in memory until retry succeeds.
    pub settings_error: Option<String>,
    /// Apps opened from `.wxapkg` files outside the WeChat cache.
    pub external: Vec<Miniapp>,
    pub rebuilt: HashMap<String, RebuildSummary>,
    icons: HashMap<String, Arc<Image>>,
    derived: HashMap<String, Derived>,
    /// Open app windows by AppID, so opening an app again focuses its window.
    pub app_windows: HashMap<String, AnyWindowHandle>,
    watcher: Option<notify::RecommendedWatcher>,
    watched: Vec<PathBuf>,
}

impl Global for GlobalLibrary {}

/// The single `Library` entity, reachable from any window.
pub struct GlobalLibrary(pub Entity<Library>);

impl Library {
    pub fn init(cx: &mut App) -> Entity<Library> {
        let settings = Settings::load();
        let scan = settings.auto_scan_on_launch;
        let library = cx.new(|_| Library {
            snapshot: None,
            scanning: false,
            last_scan: None,
            settings,
            settings_error: None,
            external: Vec::new(),
            rebuilt: HashMap::new(),
            icons: HashMap::new(),
            derived: HashMap::new(),
            app_windows: HashMap::new(),
            watcher: None,
            watched: Vec::new(),
        });
        cx.set_global(GlobalLibrary(library.clone()));
        if scan {
            library.update(cx, |library, cx| library.refresh(cx));
        }
        library
    }

    pub fn global(cx: &App) -> Entity<Library> {
        cx.global::<GlobalLibrary>().0.clone()
    }

    /// Rescans the cache in the background.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        cx.notify();
        let paths = self.settings.custom_scan_paths.clone();
        let names = self.settings.custom_app_names.clone();
        let external = self.external.clone();
        let task = cx.background_executor().spawn(async move {
            let mut snapshot = scan::scan(&paths);
            for app in &mut snapshot.miniapps {
                if let Some(name) = names.get(&app.app_id) {
                    app.name = Some(name.clone());
                    app.metadata_source = Some("自定义名称".into());
                }
            }
            let derived: HashMap<String, Derived> = snapshot.miniapps.iter().chain(external.iter()).map(|app| {
                let context = wxlocated_engine::material::Context::for_app(&snapshot, app);
                (app.app_id.clone(), derive(app, &context))
            }).collect();
            (snapshot, derived)
        });
        cx.spawn(async move |this, cx| {
            let (snapshot, derived) = task.await;
            this.update(cx, |library, cx| {
                library.icons.clear();
                library.derived = derived;
                library.snapshot = Some(Arc::new(snapshot));
                library.scanning = false;
                library.last_scan = Some(SystemTime::now());
                library.update_monitoring(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Saves settings; callers that change scan inputs refresh afterwards.
    pub fn save_settings(&mut self, cx: &mut Context<Self>) {
        if persists() {
            self.settings_error = self.settings.save().err().map(|error| error.to_string());
        }
        cx.notify();
    }

    /// Read-only view of `derived`, for render code that only has `&self`.
    pub fn derived_ref(&self, app_id: &str) -> Derived {
        self.derived.get(app_id).copied().unwrap_or_default()
    }

    /// Facts are populated by background work; rendering never opens packages.
    pub fn derived(&mut self, app: &Miniapp) -> Derived {
        self.derived_ref(&app.app_id)
    }

    /// Watches the readable cache folders and rescans a moment after WeChat
    /// stops writing (it downloads a package as several files).
    pub fn update_monitoring(&mut self, cx: &mut Context<Self>) {
        use notify::Watcher as _;
        let roots: Vec<PathBuf> = match (&self.snapshot, self.settings.monitor_cache_changes) {
            (Some(snapshot), true) => snapshot.readable_roots.clone(),
            _ => Vec::new(),
        };
        if roots == self.watched && (self.watcher.is_some() || roots.is_empty()) {
            return;
        }
        self.watcher = None;
        self.watched = roots.clone();
        if roots.is_empty() {
            return;
        }
        let (sender, receiver) = smol_channel();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && event.paths.iter().any(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wxapkg")))
            {
                let _ = sender.try_send(());
            }
        });
        let Ok(mut watcher) = watcher else { return };
        for root in &roots {
            let _ = watcher.watch(root, notify::RecursiveMode::Recursive);
        }
        self.watcher = Some(watcher);
        cx.spawn(async move |this, cx| {
            while receiver.recv().await.is_ok() {
                // Let the burst of writes settle, then drain what queued up.
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                while receiver.try_recv().is_ok() {}
                if this.update(cx, |library, cx| library.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn apps(&self) -> &[Miniapp] {
        self.snapshot.as_ref().map(|s| s.miniapps.as_slice()).unwrap_or(&[])
    }

    pub fn app(&self, app_id: &str) -> Option<&Miniapp> {
        self.apps().iter().find(|app| app.app_id == app_id).or_else(|| self.external.iter().find(|app| app.app_id == app_id))
    }

    /// Recently opened apps that are still cached, most recent first.
    pub fn recent_apps(&self) -> Vec<Miniapp> {
        self.settings.recent_app_ids.iter().filter_map(|id| self.apps().iter().find(|app| app.app_id == *id)).cloned().collect()
    }

    pub fn record_use(&mut self, app_id: &str, cx: &mut Context<Self>) {
        if self.settings.recent_app_ids.first().map(String::as_str) == Some(app_id) {
            return;
        }
        self.settings.record_recent(app_id);
        self.save_settings(cx);
    }

    /// Builds an app from dropped `.wxapkg` files or folders.
    pub fn open_external(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) -> Option<String> {
        let app = scan::external_app(paths)?;
        let id = app.app_id.clone();
        let context = self.snapshot.as_ref().map(|snapshot| wxlocated_engine::material::Context::for_app(snapshot, &app)).unwrap_or_default();
        let material = app.clone();
        let facts = cx.background_executor().spawn(async move { derive(&material, &context) });
        let app_id = id.clone();
        cx.spawn(async move |this, cx| {
            let facts = facts.await;
            this.update(cx, |library, cx| {
                library.derived.insert(app_id, facts);
                cx.notify();
            }).ok();
        }).detach();
        self.external.retain(|existing| existing.app_id != id);
        self.external.push(app);
        cx.notify();
        Some(id)
    }

    /// The app's icon as a GPUI image (decoded lazily by GPUI) and its glow
    /// colour, sampled during the scan.
    pub fn icon(&mut self, app: &Miniapp) -> (Option<Arc<Image>>, Option<Hsla>) {
        let ambient = self.derived(app).ambient;
        if let Some(icon) = self.icons.get(&app.app_id) {
            return (Some(icon.clone()), ambient);
        }
        let Some(bytes) = app.icon.as_ref() else { return (None, None) };
        let Some(format) = image_format(bytes) else { return (None, None) };
        let image = Arc::new(Image::from_bytes(format, bytes.clone()));
        self.icons.insert(app.app_id.clone(), image.clone());
        (Some(image), ambient)
    }

    /// Saves a display name (or clears it with `None`) and rescans.
    pub fn set_custom_name(&mut self, app_id: &str, name: Option<String>, cx: &mut Context<Self>) {
        match name.map(|n| n.trim().chars().take(80).collect::<String>()).filter(|n| !n.is_empty()) {
            Some(name) => {
                self.settings.custom_app_names.insert(app_id.to_string(), name);
            }
            None => {
                self.settings.custom_app_names.remove(app_id);
            }
        }
        self.save_settings(cx);
        self.refresh(cx);
    }

    pub fn record_rebuild(&mut self, app_id: &str, summary: RebuildSummary, cx: &mut Context<Self>) {
        self.rebuilt.insert(app_id.to_string(), summary);
        cx.notify();
    }

    pub fn has_coverage_gap(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.readable_roots.is_empty() || !s.inaccessible_roots.is_empty() || s.enumeration_errors > 0)
    }
}

fn derive(app: &Miniapp, context: &wxlocated_engine::material::Context) -> Derived {
    use wxlocated_engine::material::{Completeness, report};
    let analysis = report::open_app(app).ok().map(|set| report::analyze(&set));
    let completeness = Completeness::new(app, analysis.as_ref().map(|analysis| &analysis.plugins), context);
    Derived {
        missing: completeness.missing_subpackages.len(),
        declares: !completeness.declared_subpackages.is_empty(),
        plugins: completeness.plugins.len(),
        missing_plugins: completeness.missing_plugins.len(),
        ambient: app.icon.as_deref().and_then(crate::ui::ambient_color),
    }
}

fn smol_channel() -> (async_channel::Sender<()>, async_channel::Receiver<()>) {
    async_channel::bounded(16)
}

/// Picks the decoder from the file's magic bytes.
pub fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::Webp)
    } else if bytes.starts_with(b"GIF8") {
        Some(ImageFormat::Gif)
    } else if bytes.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
        (head.contains("<svg") || head.trim_start().starts_with("<?xml") && head.contains("svg")).then_some(ImageFormat::Svg)
    }
}

#[cfg(test)]
mod tests {
    use super::image_format;
    use gpui_kit::ImageFormat;

    #[test]
    fn image_formats_by_magic_bytes() {
        assert!(matches!(image_format(b"\x89PNG\r\n\x1a\n...."), Some(ImageFormat::Png)));
        assert!(matches!(image_format(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageFormat::Jpeg)));
        assert!(matches!(image_format(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageFormat::Webp)));
        assert!(matches!(image_format(b"GIF89a"), Some(ImageFormat::Gif)));
        assert!(matches!(image_format(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), Some(ImageFormat::Svg)));
        assert!(image_format(b"var x = 1;").is_none());
        assert!(image_format(b"").is_none());
    }
}
