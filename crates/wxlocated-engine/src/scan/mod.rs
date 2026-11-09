//! Finds cached mini programs: which packages belong to which AppID, which
//! cached version is current, older versions, and plugin code cached under
//! provider AppIDs. Read-only; package bodies are not unpacked.
//!
//! The search paths are the macOS WeChat cache layout; Windows locations are
//! added in the Windows phase.

mod metadata;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use metadata::{Metadata, package_metadata};

use crate::archive::natural_compare;

/// Fixed candidates stay in the report even when absent. Wildcard candidates
/// are expanded only when every directory component exists.
pub const DEFAULT_SEARCH_PATHS: &[&str] = &[
    // 3.x sandbox cache
    "~/Library/Containers/com.tencent.xinWeChat/Data/.wxapplet/packages",
    "~/Library/Containers/com.tencent.xinWeChat/Data/.wxapplet/icon",
    // 4.0 sandbox cache
    "~/Library/Containers/com.tencent.xinWeChat/Data/Documents/app_data/radium/Applet/packages",
    "~/Library/Containers/com.tencent.xinWeChat/Data/Documents/app_data/radium/Applet/icon",
    // Older non-sandbox distribution
    "~/Library/Application Support/WeChat/Applet/packages",
    "~/Library/Application Support/WeChat/Applet/icon",
    // Alternate bundle identifiers and 4.1.7+ account-isolated caches
    "~/Library/Containers/*/Data/.wxapplet/packages",
    "~/Library/Containers/*/Data/.wxapplet/icon",
    "~/Library/Containers/*/Data/Documents/app_data/radium/Applet/packages",
    "~/Library/Containers/*/Data/Documents/app_data/radium/Applet/icon",
    "~/Library/Containers/*/Data/Documents/app_data/radium/users/*/applet/packages",
    "~/Library/Containers/*/Data/Documents/app_data/radium/users/*/applet/icon",
    // Sandboxed legacy Application Support / Caches variants
    "~/Library/Containers/*/Data/Library/Application Support/WeChat/Applet/packages",
    "~/Library/Containers/*/Data/Library/Application Support/WeChat/Applet/icon",
    "~/Library/Containers/*/Data/Library/Caches/applet/packages",
    "~/Library/Containers/*/Data/Library/Caches/applet/icon",
];

/// Each candidate only opens the package index and a few bounded JSON entries,
/// and only when the selected main package gave no display name.
const MAXIMUM_METADATA_FALLBACK_PACKAGES: usize = 4;

#[derive(Debug, Clone)]
pub struct Miniapp {
    pub app_id: String,
    pub name: Option<String>,
    pub main_package: PathBuf,
    pub sub_packages: Vec<PathBuf>,
    pub size: i64,
    pub modified: SystemTime,
    pub version: Option<String>,
    /// Icon bytes: WeChat's cached icon, else one from the package.
    pub icon: Option<Vec<u8>>,
    pub metadata_source: Option<String>,
    pub declared_subpackage_roots: Vec<String>,
}

impl Miniapp {
    pub fn all_packages(&self) -> Vec<PathBuf> {
        std::iter::once(self.main_package.clone()).chain(self.sub_packages.iter().cloned()).collect()
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().filter(|name| !name.is_empty()).unwrap_or(&self.app_id)
    }

    /// The version folder the packages were read from.
    pub fn cache_directory(&self) -> PathBuf {
        self.main_package.parent().map(Path::to_path_buf).unwrap_or_default()
    }
}

/// A cached version directory other than the one selected.
#[derive(Debug, Clone)]
pub struct Generation {
    pub version: String,
    pub directory: PathBuf,
    pub packages: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    /// Newest first.
    pub miniapps: Vec<Miniapp>,
    /// Plugin code cached under its provider AppID (newest version first).
    pub plugin_generations: HashMap<String, Vec<Generation>>,
    /// Older cached versions per app, newest first. Never mixed into `miniapps`.
    pub older_generations: HashMap<String, Vec<Generation>>,
    pub readable_roots: Vec<PathBuf>,
    pub inaccessible_roots: Vec<PathBuf>,
    pub missing_roots: Vec<PathBuf>,
    pub enumeration_errors: usize,
    pub diagnostics: Diagnostics,
}

/// What the scan saw and kept, for the coverage report.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub total_package_files: usize,
    pub total_package_bytes: i64,
    /// Packages whose path names an AppID.
    pub identified_package_files: usize,
    /// Packages of the versions shown (main plus subpackages).
    pub selected_package_files: usize,
    pub plugin_package_files: usize,
    /// Plugin code cached under a provider AppID with no app of its own.
    pub plugin_only_app_count: usize,
    pub old_version_package_files: usize,
    pub resolved_name_count: usize,
    pub resolved_icon_count: usize,
    pub duration: std::time::Duration,
}

#[derive(Debug, Clone)]
struct PackageInfo {
    path: PathBuf,
    size: i64,
    modified: SystemTime,
    is_main: bool,
    is_plugin: bool,
    generation_key: PathBuf,
}

pub fn scan(custom_paths: &[String]) -> Snapshot {
    let paths: Vec<String> = DEFAULT_SEARCH_PATHS.iter().map(|path| path.to_string()).chain(custom_paths.iter().cloned()).collect();
    let mut snapshot = Snapshot::default();
    let mut packages_by_app: Vec<(String, Vec<PackageInfo>)> = Vec::new();
    let mut icons: HashMap<String, (SystemTime, Vec<u8>)> = HashMap::new();
    let mut seen_packages = HashSet::new();
    let started = std::time::Instant::now();

    for root in normalized_roots(&paths) {
        match std::fs::metadata(&root) {
            Ok(metadata) if metadata.is_dir() => {}
            _ => {
                snapshot.missing_roots.push(root);
                continue;
            }
        }
        if !is_readable(&root) {
            snapshot.inaccessible_roots.push(root);
            continue;
        }
        snapshot.readable_roots.push(root.clone());

        let mut errors = 0;
        let files = walk(&root, &mut errors);
        snapshot.enumeration_errors += errors;
        for (path, metadata) in files {
            if let Some(icon_app) = app_id_from_icon(&path)
                && metadata.is_file()
                && metadata.len() <= 1_000_000
                && let Ok(data) = std::fs::read(&path)
            {
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if icons.get(&icon_app).is_none_or(|(current, _)| *current < modified) {
                    icons.insert(icon_app, (modified, data));
                }
                continue;
            }
            if !extension_is(&path, "wxapkg") || !seen_packages.insert(identity_key(&path, Some(&metadata))) || !metadata.is_file() {
                continue;
            }
            snapshot.diagnostics.total_package_files += 1;
            snapshot.diagnostics.total_package_bytes += metadata.len() as i64;
            let Some(app_id) = app_id_from_path(&path) else { continue };
            snapshot.diagnostics.identified_package_files += 1;
            let package = PackageInfo {
                size: metadata.len() as i64,
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                is_main: is_main_package(&path),
                is_plugin: is_plugin_package(&path),
                generation_key: generation_key(&path, &app_id),
                path,
            };
            match packages_by_app.iter_mut().find(|(id, _)| *id == app_id) {
                Some((_, list)) => list.push(package),
                None => packages_by_app.push((app_id, vec![package])),
            }
        }
    }

    for (app_id, packages) in &packages_by_app {
        if packages.iter().all(|package| package.is_plugin) {
            snapshot.plugin_generations.insert(app_id.clone(), generations(packages));
            continue;
        }
        let ranked = ranked_generations(packages);
        let Some(selected) = ranked.first() else { continue };
        let Some(main) = select_main_package(selected) else { continue };

        let selected_paths: HashSet<&PathBuf> = selected.iter().map(|package| &package.path).collect();
        let older: Vec<PackageInfo> =
            packages.iter().filter(|package| !selected_paths.contains(&package.path) && !package.is_plugin).cloned().collect();
        let older = generations(&older);
        snapshot.diagnostics.old_version_package_files += older.iter().map(|g| g.packages.len()).sum::<usize>();
        if !older.is_empty() {
            snapshot.older_generations.insert(app_id.clone(), older);
        }

        let included: Vec<&PackageInfo> = selected.iter().filter(|package| !package.is_plugin || package.path == main.path).collect();
        let mut sub_packages: Vec<PathBuf> =
            included.iter().filter(|package| package.path != main.path).map(|package| package.path.clone()).collect();
        sub_packages.sort_by(|a, b| natural_compare(&a.to_string_lossy(), &b.to_string_lossy()));
        snapshot.diagnostics.selected_package_files += included.len();
        let size = included.iter().map(|package| package.size).sum();
        let modified = included.iter().map(|package| package.modified).max().unwrap_or(main.modified);
        let metadata = resolve_metadata(app_id, &main, selected, packages);
        let cached_icon = icons.contains_key(app_id);
        let icon = icons.get(app_id).map(|(_, data)| data.clone()).or(metadata.icon.clone());
        let metadata_source = match (&metadata.source, cached_icon) {
            (Some(source), true) => Some(format!("本地图标与{source}")),
            (None, true) => Some("本地图标缓存".to_string()),
            (source, false) => source.clone(),
        };
        snapshot.miniapps.push(Miniapp {
            app_id: app_id.clone(),
            name: metadata.name,
            main_package: main.path.clone(),
            sub_packages,
            size,
            modified,
            version: metadata.version,
            icon,
            metadata_source,
            declared_subpackage_roots: metadata.declared_subpackage_roots,
        });
    }
    snapshot.miniapps.sort_by_key(|app| std::cmp::Reverse(app.modified));
    let diagnostics = &mut snapshot.diagnostics;
    diagnostics.plugin_package_files = packages_by_app.iter().flat_map(|(_, list)| list).filter(|p| p.is_plugin).count();
    diagnostics.plugin_only_app_count = snapshot.plugin_generations.len();
    diagnostics.resolved_name_count = snapshot.miniapps.iter().filter(|app| app.name.is_some()).count();
    diagnostics.resolved_icon_count = snapshot.miniapps.iter().filter(|app| app.icon.is_some()).count();
    diagnostics.duration = started.elapsed();
    snapshot
}

/// An app built from `.wxapkg` files (or folders of them) outside the WeChat
/// cache, e.g. dropped on the app. The AppID comes from the path when it
/// names one (needed to decrypt `V1MMWX` packages).
pub fn external_app(paths: &[PathBuf]) -> Option<Miniapp> {
    let mut packages: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() {
            let Ok(entries) = std::fs::read_dir(path) else { continue };
            packages.extend(entries.flatten().map(|entry| entry.path()).filter(|p| extension_is(p, "wxapkg") && p.is_file()));
        } else if extension_is(path, "wxapkg") && path.is_file() {
            packages.push(path.clone());
        }
    }
    packages.retain(|p| !file_name(p).to_uppercase().contains("PLUGINCODE"));
    packages.sort_by_key(|p| file_name(p));
    packages.dedup();
    let main = packages
        .iter()
        .find(|p| file_name(p) == "__APP__.wxapkg")
        .or_else(|| packages.iter().find(|p| !file_name(p).starts_with('_')))
        .or(packages.first())?
        .clone();
    let text = main.to_string_lossy();
    let app_id = crate::archive::inferred_app_id(&main)
        .unwrap_or_else(|| format!("external-{}", &crate::sha256_hex(text.as_bytes())[..12]));
    let metadata = package_metadata(&main, &app_id);
    let stats: Vec<std::fs::Metadata> = packages.iter().filter_map(|p| std::fs::metadata(p).ok()).collect();
    Some(Miniapp {
        app_id,
        name: metadata.name,
        sub_packages: packages.iter().filter(|p| **p != main).cloned().collect(),
        main_package: main,
        size: stats.iter().map(|m| m.len() as i64).sum(),
        modified: stats.iter().filter_map(|m| m.modified().ok()).max().unwrap_or(SystemTime::now()),
        version: metadata.version,
        icon: metadata.icon,
        metadata_source: Some("外部包".into()),
        declared_subpackage_roots: metadata.declared_subpackage_roots,
    })
}

/// Groups packages by version directory, newest (numeric) first.
fn generations(packages: &[PackageInfo]) -> Vec<Generation> {
    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for package in packages {
        match groups.iter_mut().find(|(key, _)| *key == package.generation_key) {
            Some((_, members)) => members.push(package.path.clone()),
            None => groups.push((package.generation_key.clone(), vec![package.path.clone()])),
        }
    }
    let mut result: Vec<Generation> = groups
        .into_iter()
        .map(|(key, mut members)| {
            members.sort_by_key(|a| file_name(a));
            Generation { version: file_name(&key), directory: key, packages: members }
        })
        .collect();
    result.sort_by(|left, right| match (left.version.parse::<i64>(), right.version.parse::<i64>()) {
        (Ok(l), Ok(r)) => r.cmp(&l),
        _ => right.version.cmp(&left.version),
    });
    result
}

fn ranked_generations(packages: &[PackageInfo]) -> Vec<Vec<PackageInfo>> {
    let mut groups: Vec<Vec<PackageInfo>> = Vec::new();
    for package in packages {
        match groups.iter_mut().find(|group| group[0].generation_key == package.generation_key) {
            Some(group) => group.push(package.clone()),
            None => groups.push(vec![package.clone()]),
        }
    }
    groups.retain(|group| group.iter().any(|p| p.is_main) || group.iter().any(|p| !p.is_plugin));
    groups.sort_by(|left, right| {
        let left_main = left.iter().any(|p| p.is_main);
        let right_main = right.iter().any(|p| p.is_main);
        if left_main != right_main {
            return right_main.cmp(&left_main);
        }
        let version = |group: &Vec<PackageInfo>| file_name(&group[0].generation_key).parse::<i64>().ok();
        if let (Some(l), Some(r)) = (version(left), version(right))
            && l != r
        {
            return r.cmp(&l);
        }
        let date = |group: &Vec<PackageInfo>| group.iter().map(|p| p.modified).max();
        date(right).cmp(&date(left))
    });
    groups
}

/// `max(by:)` keeps the first of equal maxima.
fn first_max<'a, T>(items: impl Iterator<Item = &'a T>, mut less: impl FnMut(&T, &T) -> bool) -> Option<&'a T> {
    let mut result = None;
    for item in items {
        match result {
            None => result = Some(item),
            Some(current) if less(current, item) => result = Some(item),
            _ => {}
        }
    }
    result
}

fn select_main_package(packages: &[PackageInfo]) -> Option<PackageInfo> {
    if let Some(main) = first_max(packages.iter().filter(|p| p.is_main), |a, b| a.modified < b.modified) {
        return Some(main.clone());
    }
    first_max(packages.iter().filter(|p| !p.is_plugin), |a, b| {
        if a.size == b.size { a.modified < b.modified } else { a.size < b.size }
    })
    .cloned()
}

fn resolve_metadata(app_id: &str, main: &PackageInfo, selected: &[PackageInfo], all: &[PackageInfo]) -> Metadata {
    let primary = package_metadata(&main.path, app_id);
    if primary.name.is_some() {
        return primary;
    }
    let mut remaining = MAXIMUM_METADATA_FALLBACK_PACKAGES;
    let mut current: Vec<&PackageInfo> = selected.iter().filter(|p| p.path != main.path && !p.is_plugin).collect();
    current.sort_by(|a, b| b.size.cmp(&a.size).then(b.modified.cmp(&a.modified)));
    for package in current {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        let candidate = package_metadata(&package.path, app_id);
        if let Some(name) = candidate.name {
            let source = prefixed_source(&format!("当前分包 {}", file_name(&package.path)), candidate.source.as_deref());
            return Metadata { name: Some(name), source: Some(source), ..primary };
        }
    }
    for generation in ranked_generations(all) {
        if remaining == 0 {
            break;
        }
        if generation[0].generation_key == main.generation_key {
            continue;
        }
        let Some(historical) = select_main_package(&generation) else { continue };
        remaining -= 1;
        let candidate = package_metadata(&historical.path, app_id);
        if let Some(name) = candidate.name {
            let version = file_name(&historical.generation_key);
            let source = prefixed_source(&format!("历史缓存版本 {version}"), candidate.source.as_deref());
            return Metadata { name: Some(name), source: Some(source), ..primary };
        }
    }
    primary
}

fn prefixed_source(prefix: &str, source: Option<&str>) -> String {
    match source.filter(|source| !source.is_empty()) {
        Some(source) => format!("{prefix} · {source}"),
        None => prefix.to_string(),
    }
}

fn normalized_roots(paths: &[String]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for path in paths {
        for url in expanded_roots(path) {
            let normalized = std::fs::canonicalize(&url).unwrap_or(url);
            if seen.insert(identity_key(&normalized, None)) {
                roots.push(normalized);
            }
        }
    }
    roots
}

fn expand_tilde(path: &str) -> String {
    match path.strip_prefix("~") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}{rest}")
        }
        _ => path.to_string(),
    }
}

fn expanded_roots(path: &str) -> Vec<PathBuf> {
    let expanded = expand_tilde(path);
    let components: Vec<&str> = expanded.split('/').filter(|c| !c.is_empty()).collect();
    if !components.contains(&"*") {
        return vec![PathBuf::from(&expanded)];
    }
    if !expanded.starts_with('/') {
        return Vec::new();
    }
    let mut candidates = vec![PathBuf::from("/")];
    for component in components {
        if component == "*" {
            candidates = candidates
                .iter()
                .flat_map(|parent| {
                    let mut children: Vec<PathBuf> = std::fs::read_dir(parent)
                        .map(|entries| {
                            entries
                                .filter_map(Result::ok)
                                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                                .filter(|entry| std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()))
                                .map(|entry| entry.path())
                                .collect()
                        })
                        .unwrap_or_default();
                    if parent.file_name().is_some_and(|name| name == "Containers") {
                        children.retain(|child| is_likely_host_container(&file_name(child)));
                    }
                    children
                })
                .collect();
        } else {
            candidates =
                candidates.iter().map(|parent| parent.join(component)).filter(|child| std::fs::metadata(child).is_ok_and(|m| m.is_dir())).collect();
        }
        if candidates.is_empty() {
            break;
        }
    }
    candidates
}

fn is_likely_host_container(name: &str) -> bool {
    let name = name.to_lowercase();
    name.contains("tencent") || name.contains("wechat") || name.contains("weixin")
}

/// `NSDirectoryEnumerator` order: each entry in directory order, descending
/// into a directory as soon as it is reached; links are not followed.
fn walk(root: &Path, errors: &mut usize) -> Vec<(PathBuf, std::fs::Metadata)> {
    fn visit(directory: &Path, errors: &mut usize, result: &mut Vec<(PathBuf, std::fs::Metadata)>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            *errors += 1;
            return;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                *errors += 1;
                continue;
            };
            let path = entry.path();
            let Ok(link_metadata) = std::fs::symlink_metadata(&path) else { continue };
            let metadata = std::fs::metadata(&path).unwrap_or_else(|_| link_metadata.clone());
            result.push((path.clone(), metadata));
            if link_metadata.is_dir() {
                visit(&path, errors, result);
            }
        }
    }
    let mut result = Vec::new();
    visit(root, errors, &mut result);
    result
}

fn identity_key(path: &Path, metadata: Option<&std::fs::Metadata>) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = metadata.cloned().or_else(|| std::fs::metadata(path).ok());
        if let Some(metadata) = metadata {
            return format!("resource:{}:{}", metadata.dev(), metadata.ino());
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    let normalized = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    format!("path:{}", normalized.to_string_lossy())
}

#[cfg(unix)]
fn is_readable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c_path` is a valid NUL-terminated string for the duration of the call.
    unsafe { libc::access(c_path.as_ptr(), libc::R_OK) == 0 }
}

#[cfg(not(unix))]
fn is_readable(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok()
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn extension_is(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case(extension))
}

fn is_app_id_like(name: &str) -> bool {
    name.len() >= 10
        && name.starts_with("wx")
        && name[2..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The nearest path component shaped like an AppID, else the first such run
/// anywhere in the path.
fn app_id_from_path(path: &Path) -> Option<String> {
    if let Some(component) = path.components().rev().filter_map(|c| c.as_os_str().to_str()).find(|name| is_app_id_like(name)) {
        return Some(component.to_string());
    }
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    let mut index = 0;
    while let Some(found) = text[index..].find("wx") {
        let start = index + found;
        let end = start + 2 + bytes[start + 2..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'-').count();
        if end - start >= 10 {
            return Some(text[start..end].to_string());
        }
        index = start + 1;
    }
    None
}

fn is_main_package(path: &Path) -> bool {
    let name = file_name(path);
    name.eq_ignore_ascii_case("__APP__.wxapkg") || name.eq_ignore_ascii_case("app.wxapkg")
}

fn is_plugin_package(path: &Path) -> bool {
    let upper = path.to_string_lossy().to_uppercase();
    upper.contains("PLUGINCODE") || upper.contains("/PLUGINS/") || upper.contains("__PLUGIN__")
}

fn app_id_from_icon(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_string_lossy().to_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        return None;
    }
    if !path.parent().is_some_and(|parent| file_name(parent).eq_ignore_ascii_case("icon")) {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let candidate = stem.split('_').next().unwrap_or(&stem).to_string();
    is_app_id_like(&candidate).then_some(candidate)
}

/// The version folder right under the AppID folder, else the AppID folder.
fn generation_key(path: &Path, app_id: &str) -> PathBuf {
    let components: Vec<_> = path.components().collect();
    let Some(app_index) = components.iter().rposition(|c| c.as_os_str() == app_id) else {
        return path.parent().map(Path::to_path_buf).unwrap_or_default();
    };
    let end = if app_index + 1 < components.len() - 1 { app_index + 1 } else { app_index };
    components[..=end].iter().collect()
}
