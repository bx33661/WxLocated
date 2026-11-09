//! User settings shared by the CLI and the desktop app: extra cache folders to
//! scan and custom display names.
//!
//! Stored as JSON in the platform config folder. On macOS, until that file
//! exists, the Swift app's preferences (`com.wxlocated.WxLocated`) are read so
//! nothing set there is lost.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub custom_scan_paths: Vec<String>,
    pub custom_app_names: BTreeMap<String, String>,
    /// Exports from the app replace the home folder and account id in manifests.
    pub redact_exports: bool,
    /// Where the app exports audit material; `None` asks every time.
    pub export_directory: Option<String>,
    pub export_include_raw: bool,
    pub export_include_source: bool,
    /// Gallery order: `modified`, `name` or `size`.
    pub sort_order: String,
    /// Most recently opened first.
    pub recent_app_ids: Vec<String>,
    pub auto_scan_on_launch: bool,
    /// Rescan when WeChat writes to its cache.
    pub monitor_cache_changes: bool,
    /// `system`, `light` or `dark`.
    pub appearance: String,
    /// The file last shown in each app's window, restored when it reopens.
    pub last_files: BTreeMap<String, String>,
    /// App window panel widths, in points.
    pub sidebar_width: f32,
    pub inspector_width: f32,
    /// Gallery window frame `[x, y, width, height]`, restored at launch.
    pub gallery_frame: Option<[f32; 4]>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            custom_scan_paths: Vec::new(),
            custom_app_names: BTreeMap::new(),
            redact_exports: false,
            export_directory: None,
            export_include_raw: true,
            export_include_source: true,
            sort_order: "modified".into(),
            recent_app_ids: Vec::new(),
            auto_scan_on_launch: true,
            monitor_cache_changes: true,
            appearance: "system".into(),
            last_files: BTreeMap::new(),
            sidebar_width: 290.,
            inspector_width: 290.,
            gallery_frame: None,
        }
    }
}

impl Settings {
    /// Moves `app_id` to the front of the recent list (at most 8 kept).
    pub fn record_recent(&mut self, app_id: &str) {
        self.recent_app_ids.retain(|id| id != app_id);
        self.recent_app_ids.insert(0, app_id.to_string());
        self.recent_app_ids.truncate(8);
    }
}

impl Settings {
    pub fn load() -> Self {
        if let Some(path) = settings_path()
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(settings) = serde_json::from_str(&text)
        {
            return settings;
        }
        legacy_preferences().unwrap_or_default()
    }

    /// Writes `settings.json`; from then on it is the only source.
    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path().ok_or_else(|| std::io::Error::other("no settings folder"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text + "\n")
    }
}

/// `~/Library/Application Support/WxLocated/settings.json` on macOS,
/// `%APPDATA%\WxLocated\settings.json` on Windows.
pub fn settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support/WxLocated/settings.json"))
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|data| PathBuf::from(data).join("WxLocated").join("settings.json"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/wxlocated/settings.json"))
    }
}

#[cfg(target_os = "macos")]
fn legacy_preferences() -> Option<Settings> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join("Library/Preferences/com.wxlocated.WxLocated.plist");
    let plist::Value::Dictionary(dictionary) = plist::Value::from_file(path).ok()? else { return None };
    let custom_scan_paths = match dictionary.get("customScanPaths") {
        Some(plist::Value::Array(items)) => items.iter().filter_map(|item| item.as_string().map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    let custom_app_names = match dictionary.get("customAppNames") {
        Some(plist::Value::Dictionary(names)) => {
            names.iter().filter_map(|(id, name)| name.as_string().map(|name| (id.clone(), name.to_string()))).collect()
        }
        _ => BTreeMap::new(),
    };
    let recent_app_ids = match dictionary.get("recentAppIDs") {
        Some(plist::Value::Array(items)) => items.iter().filter_map(|item| item.as_string().map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    Some(Settings { custom_scan_paths, custom_app_names, recent_app_ids, ..Settings::default() })
}

#[cfg(not(target_os = "macos"))]
fn legacy_preferences() -> Option<Settings> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings.json written before the app-only fields existed still loads,
    /// keeps what it had, and gets defaults for the rest.
    #[test]
    fn older_files_load_with_defaults() {
        let old = r#"{"custom_scan_paths":["/a"],"custom_app_names":{"wx1":"甲"},"redact_exports":true}"#;
        let settings: Settings = serde_json::from_str(old).expect("parses");
        assert_eq!(settings.custom_scan_paths, ["/a"]);
        assert_eq!(settings.custom_app_names["wx1"], "甲");
        assert!(settings.redact_exports);
        assert!(settings.export_include_raw && settings.export_include_source);
        assert!(settings.auto_scan_on_launch && settings.monitor_cache_changes);
        assert_eq!(settings.sort_order, "modified");
        assert_eq!(settings.appearance, "system");
        assert_eq!(settings.sidebar_width, 290.);
        assert!(settings.gallery_frame.is_none());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let settings: Settings = serde_json::from_str(r#"{"future_field":1,"sort_order":"name"}"#).expect("parses");
        assert_eq!(settings.sort_order, "name");
    }

    #[test]
    fn recent_list_moves_to_front_and_is_bounded() {
        let mut settings = Settings::default();
        for id in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
            settings.record_recent(id);
        }
        settings.record_recent("c");
        assert_eq!(settings.recent_app_ids.first().map(String::as_str), Some("c"));
        assert_eq!(settings.recent_app_ids.len(), 8);
        assert_eq!(settings.recent_app_ids.iter().filter(|id| *id == "c").count(), 1);
    }
}
