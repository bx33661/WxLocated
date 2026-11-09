//! Display name, version, icon and declared subpackages of a cached package,
//! from its own JSON entries or from sidecar files next to it.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::archive::Archive;
use crate::json::{self, Value};
use crate::text::{character_count, trim_whitespace_and_newlines};

const MAXIMUM_JSON_SIZE: u64 = 2 * 1024 * 1024;
const MAXIMUM_ICON_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub name: Option<String>,
    pub version: Option<String>,
    /// Icon bytes found in the package or next to it.
    pub icon: Option<Vec<u8>>,
    pub source: Option<String>,
    pub declared_subpackage_roots: Vec<String>,
}

struct ArchiveMetadata {
    title: Option<String>,
    title_source: Option<String>,
    icon: Option<Vec<u8>>,
    declared_subpackage_roots: Vec<String>,
}

pub fn package_metadata(package: &Path, app_id: &str) -> Metadata {
    let archive = read_archive_metadata(package, app_id);
    let sidecar = name_from_sidecar(package, app_id);
    let adjacent_icon = adjacent_icon(package);

    let inferred = archive.as_ref().and_then(|a| a.title_source.as_deref()).is_some_and(|source| source.contains("标题"));
    let (name, name_source) = match (&archive, &sidecar) {
        (Some(archive), _) if archive.title.is_some() && !inferred => {
            (archive.title.clone(), Some(archive.title_source.clone().unwrap_or_else(|| "包内应用信息".into())))
        }
        (_, Some((value, source))) => (Some(value.clone()), Some(source.clone())),
        _ => (archive.as_ref().and_then(|a| a.title.clone()), archive.as_ref().and_then(|a| a.title_source.clone())),
    };
    let archive_icon = archive.as_ref().and_then(|a| a.icon.clone());
    let mut sources = BTreeSet::new();
    if let Some(source) = name_source {
        sources.insert(source);
    }
    if archive_icon.is_some() {
        sources.insert("包内图标".to_string());
    } else if adjacent_icon.is_some() {
        sources.insert("相邻图标".to_string());
    }
    let declared = archive.as_ref().map(|a| a.declared_subpackage_roots.clone()).unwrap_or_default();
    if !declared.is_empty() {
        sources.insert("包内分包声明".to_string());
    }
    Metadata {
        name,
        version: version_from_path(package, app_id),
        icon: archive_icon.or(adjacent_icon),
        source: (!sources.is_empty()).then(|| sources.into_iter().collect::<Vec<_>>().join("、")),
        declared_subpackage_roots: declared,
    }
}

fn read_archive_metadata(package: &Path, app_id: &str) -> Option<ArchiveMetadata> {
    let archive = Archive::open_with_app_id(package, Some(app_id)).ok()?;
    let mut json_entries: Vec<_> = archive
        .entries
        .iter()
        .filter(|entry| {
            let name = entry.path.to_lowercase();
            ["app-config.json", "app.json", "project.config.json", "project.private.config.json", "app-info.json", "appinfo.json", "ext.json", "game.json", "manifest.json"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
        .collect();
    json_entries.sort_by_key(|entry| metadata_priority(&entry.path));

    let mut title: Option<(String, String)> = None;
    let mut icon_paths: Vec<String> = Vec::new();
    let mut roots: BTreeSet<String> = BTreeSet::new();
    for entry in json_entries {
        let Ok(data) = archive.read_limited(entry, MAXIMUM_JSON_SIZE) else { continue };
        let Ok(text) = String::from_utf8(data) else { continue };
        let Some(object) = json::parse(&text) else { continue };
        if title.is_none() {
            title = extract_title(&object, app_id);
        }
        extract_icon_paths(&object, &mut icon_paths);
        roots.extend(extract_subpackage_roots(&object));
    }

    // The original checks icons with `CGImageSourceCreateWithData`, which
    // accepts any bytes; any readable entry counts.
    let readable = |entry: &crate::archive::Entry| archive.read_limited(entry, MAXIMUM_ICON_SIZE).ok();
    let icon = icon_paths.iter().find_map(|path| {
        let normalized = crate::archive::normalized_archive_path(path);
        archive
            .entries
            .iter()
            .find(|entry| entry.path == normalized || entry.path.ends_with(&format!("/{normalized}")))
            .and_then(readable)
    }).or_else(|| {
        let mut candidates: Vec<(u8, &crate::archive::Entry)> = archive
            .entries
            .iter()
            .filter(|entry| is_image_path(&entry.path) && entry.size <= MAXIMUM_ICON_SIZE)
            .filter_map(|entry| {
                let lower = entry.path.to_lowercase();
                let score = if lower.contains("appicon") || lower.contains("app_icon") {
                    0
                } else if lower.contains("logo") {
                    1
                } else if lower.ends_with("/icon.png") || lower.ends_with("/icon.jpg") || lower.ends_with("/icon.jpeg") {
                    2
                } else {
                    return None;
                };
                Some((score, entry))
            })
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.size.cmp(&b.1.size)));
        candidates.iter().take(8).find_map(|(_, entry)| readable(entry))
    });

    if title.is_none() && icon.is_none() && roots.is_empty() {
        return None;
    }
    let (title, title_source) = title.map_or((None, None), |(title, source)| (Some(title), Some(source)));
    Some(ArchiveMetadata { title, title_source, icon, declared_subpackage_roots: roots.into_iter().collect() })
}

const TITLE_PATHS: &[&[&str]] = &[
    &["accountInfo", "miniProgram", "appName"],
    &["accountInfo", "miniProgram", "nickName"],
    &["accountInfo", "miniProgram", "nickname"],
    &["appInfo", "appName"],
    &["appInfo", "nickName"],
    &["appInfo", "nickname"],
    &["appInfo", "name"],
    &["miniProgram", "appName"],
    &["miniProgram", "nickName"],
    &["miniProgram", "nickname"],
    &["app", "name"],
    &["projectname"],
    &["projectName"],
    &["appName"],
    &["nickName"],
    &["nickname"],
    &["displayName"],
];

fn extract_title(object: &Value, app_id: &str) -> Option<(String, String)> {
    for path in TITLE_PATHS {
        if let Some(title) = value_case_insensitive(path, object).and_then(Value::as_str)
            && is_useful_title(title, app_id)
        {
            let project = *path == ["projectname"] || *path == ["projectName"];
            return Some((normalized_title(title), if project { "包内工程名称" } else { "包内应用信息" }.into()));
        }
    }
    if let (Some(entry_page), Some(Value::Object(pages))) = (object.get("entryPagePath").and_then(Value::as_str), object.get("page")) {
        let key = if entry_page.ends_with(".html") { entry_page.to_string() } else { format!("{entry_page}.html") };
        if let Some((_, page)) = pages.iter().find(|(k, _)| *k == key)
            && let Some(title) = page.get("window").and_then(|w| w.get("navigationBarTitleText")).and_then(Value::as_str)
            && is_useful_title(title, app_id)
        {
            return Some((normalized_title(title), "包内入口页标题".into()));
        }
    }
    for path in [&["global", "window", "navigationBarTitleText"][..], &["window", "navigationBarTitleText"][..]] {
        if let Some(title) = value_case_insensitive(path, object).and_then(Value::as_str)
            && is_useful_title(title, app_id)
        {
            return Some((normalized_title(title), "包内全局标题".into()));
        }
    }
    if let Some(Value::Object(pages)) = object.get("page") {
        let candidates: Vec<String> = pages
            .iter()
            .filter_map(|(_, page)| page.get("window")?.get("navigationBarTitleText")?.as_str().map(str::to_string))
            .filter(|title| is_useful_title(title, app_id))
            .map(|title| normalized_title(&title))
            .collect();
        if let Some(title) = stable_page_title(&candidates) {
            return Some((title, "包内页面标题推断".into()));
        }
    }
    None
}

fn extract_subpackage_roots(object: &Value) -> Vec<String> {
    let Some(packages) = object.get("subPackages").or_else(|| object.get("subpackages")).and_then(Value::as_object_array) else {
        return Vec::new();
    };
    packages
        .into_iter()
        .filter_map(|package| {
            let root = Value::Object(package).get("root")?.as_str()?.trim_matches(|c| c == '/' || c == ' ').to_string();
            (!root.is_empty()).then_some(root)
        })
        .collect()
}

fn extract_icon_paths(value: &Value, result: &mut Vec<String>) {
    match value {
        Value::Object(entries) => {
            for (key, child) in entries {
                if matches!(key.to_lowercase().as_str(), "icon" | "iconpath" | "selectediconpath" | "logo" | "logopath")
                    && let Some(path) = child.as_str()
                    && !path.contains("://")
                    && is_image_path(path)
                {
                    result.push(path.to_string());
                }
                extract_icon_paths(child, result);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| extract_icon_paths(item, result)),
        _ => {}
    }
}

/// Follows `path` matching keys case-insensitively (an exact match first).
fn value_case_insensitive<'a>(path: &[&str], object: &'a Value) -> Option<&'a Value> {
    let mut current = object;
    for component in path {
        let Value::Object(entries) = current else { return None };
        current = entries
            .iter()
            .find(|(key, _)| key == component)
            .or_else(|| entries.iter().find(|(key, _)| key.eq_ignore_ascii_case(component)))
            .map(|(_, value)| value)?;
    }
    Some(current)
}

const GENERIC_TITLES: &[&str] = &[
    "首页", "主页", "我的", "个人中心", "登录", "授权", "加载中", "小程序", "微信", "发现", "消息", "服务", "测试", "示例", "index", "home",
    "wechat", "weixin", "uni-app", "uniapp", "miniprogram", "demo", "test",
];

fn is_useful_title(value: &str, app_id: &str) -> bool {
    let trimmed = normalized_title(value);
    let count = character_count(&trimmed);
    count > 1
        && count < 80
        && !trimmed.eq_ignore_ascii_case(app_id)
        && !trimmed.eq_ignore_ascii_case("Weixin")
        && !GENERIC_TITLES.contains(&trimmed.to_lowercase().as_str())
}

fn stable_page_title(candidates: &[String]) -> Option<String> {
    if candidates.len() < 2 {
        return None;
    }
    let mut counts: Vec<(&String, usize)> = Vec::new();
    for candidate in candidates {
        match counts.iter_mut().find(|(key, _)| *key == candidate) {
            Some((_, count)) => *count += 1,
            None => counts.push((candidate, 1)),
        }
    }
    let best = counts.iter().copied().reduce(|best, item| {
        let better = if best.1 == item.1 { character_count(best.0) < character_count(item.0) } else { best.1 < item.1 };
        if better { item } else { best }
    })?;
    (best.1 >= 2 && best.1 as f64 / candidates.len() as f64 >= 0.5).then(|| best.0.clone())
}

static WHITESPACE_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("regex"));

fn normalized_title(value: &str) -> String {
    let decoded = crate::text::removing_percent_encoding(value).unwrap_or_else(|| value.to_string());
    WHITESPACE_RUN.replace_all(trim_whitespace_and_newlines(&decoded), " ").into_owned()
}

fn metadata_priority(path: &str) -> u8 {
    let lower = path.to_lowercase();
    if lower.ends_with("app-config.json") {
        0
    } else if lower.ends_with("app-info.json") || lower.ends_with("appinfo.json") || lower.ends_with("manifest.json") {
        1
    } else if lower.ends_with("app.json") {
        2
    } else {
        3
    }
}

fn is_image_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    [".png", ".jpg", ".jpeg", ".webp"].iter().any(|suffix| lower.ends_with(suffix))
}

// MARK: Sidecar files

const NAME_PATHS: &[&[&str]] = &[
    &["accountInfo", "miniProgram", "appName"],
    &["accountInfo", "miniProgram", "nickName"],
    &["accountInfo", "miniProgram", "nickname"],
    &["appInfo", "appName"],
    &["appInfo", "nickName"],
    &["appInfo", "nickname"],
    &["appInfo", "name"],
    &["miniProgram", "appName"],
    &["miniProgram", "nickName"],
    &["projectname"],
    &["projectName"],
    &["appName"],
    &["nickName"],
    &["nickname"],
    &["displayName"],
    &["CFBundleDisplayName"],
    &["name"],
];

const GENERIC_NAMES: &[&str] = &[
    "首页", "主页", "我的", "个人中心", "登录", "授权", "加载中", "小程序", "微信", "测试", "示例", "miniprogram", "weixin", "wechat",
    "uni-app", "uniapp", "demo", "test", "index", "home",
];

fn is_useful_name(name: &str, app_id: &str) -> bool {
    let trimmed = normalized_title(name);
    let count = character_count(&trimmed);
    count > 1 && count < 80 && !trimmed.eq_ignore_ascii_case(app_id) && !GENERIC_NAMES.contains(&trimmed.to_lowercase().as_str())
}

fn name_from_sidecar(package: &Path, app_id: &str) -> Option<(String, String)> {
    let parent = package.parent()?;
    let directories: Vec<&Path> = [Some(parent), parent.parent(), parent.parent().and_then(Path::parent)].into_iter().flatten().collect();
    let names = [
        "project.config.json".to_string(),
        "project.private.config.json".into(),
        "app.json".into(),
        "app-config.json".into(),
        "app-info.json".into(),
        "appinfo.json".into(),
        "info.json".into(),
        "manifest.json".into(),
        format!("{app_id}.json"),
        "Info.plist".into(),
        "app-info.plist".into(),
        format!("{app_id}.plist"),
    ];
    for directory in directories {
        for name in &names {
            let path = directory.join(name);
            let Ok(metadata) = std::fs::metadata(&path) else { continue };
            if !metadata.is_file() || metadata.len() >= 1_000_000 {
                continue;
            }
            let Ok(data) = std::fs::read(&path) else { continue };
            let object = if name.to_lowercase().ends_with(".plist") { plist_value(&data) } else { String::from_utf8(data).ok().and_then(|t| json::parse(&t)) };
            let Some(object) = object else { continue };
            for key_path in NAME_PATHS {
                if let Some(value) = value_case_insensitive(key_path, &object).and_then(Value::as_str)
                    && is_useful_name(value, app_id)
                {
                    return Some((normalized_title(value), format!("本地配置 {name}")));
                }
            }
            if let Some(title) = object.get("window").and_then(|w| w.get("navigationBarTitleText")).and_then(Value::as_str)
                && is_useful_name(title, app_id)
            {
                return Some((normalized_title(title), format!("本地配置 {name}")));
            }
        }
    }
    None
}

/// A property list as JSON-like values (strings and dictionaries are what
/// the name lookup needs).
fn plist_value(data: &[u8]) -> Option<Value> {
    fn convert(value: &plist::Value) -> Value {
        match value {
            plist::Value::Dictionary(dictionary) => Value::Object(dictionary.iter().map(|(k, v)| (k.clone(), convert(v))).collect()),
            plist::Value::Array(items) => Value::Array(items.iter().map(convert).collect()),
            plist::Value::String(text) => Value::String(text.clone()),
            plist::Value::Boolean(flag) => Value::Bool(*flag),
            _ => Value::Null,
        }
    }
    plist::Value::from_reader(std::io::Cursor::new(data)).ok().map(|value| convert(&value))
}

fn adjacent_icon(package: &Path) -> Option<Vec<u8>> {
    let directory = package.parent()?;
    let entries = std::fs::read_dir(directory).ok()?;
    entries.filter_map(Result::ok).find_map(|entry| {
        let path = entry.path();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name.starts_with('.') {
            return None;
        }
        let stem = path.file_stem().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let extension = path.extension().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        ((stem.contains("logo") || stem.contains("icon"))
            && matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp")
            && std::fs::metadata(&path).is_ok_and(|m| m.len() <= 1_000_000))
        .then(|| std::fs::read(&path).ok())
        .flatten()
    })
}

/// The path component right after the AppID folder, when it is not the file.
fn version_from_path(package: &Path, app_id: &str) -> Option<String> {
    let components: Vec<String> = package.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let app_index = components.iter().rposition(|c| c == app_id)?;
    if app_index + 1 >= components.len() - 1 {
        return None;
    }
    let candidate = &components[app_index + 1];
    (!candidate.is_empty()).then(|| candidate.clone())
}

