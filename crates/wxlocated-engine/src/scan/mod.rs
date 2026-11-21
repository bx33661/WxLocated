//! Finds cached mini programs: which packages belong to which AppID, which
//! cached version is current, older versions, and plugin code cached under
//! provider AppIDs. Read-only; package bodies are not unpacked.
//!
//! Search roots follow each platform's WeChat layout and user configuration.

mod metadata;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use metadata::{Metadata, package_metadata};

use crate::archive::natural_compare;

/// Fixed candidates stay in the report even when absent. Wildcard candidates
/// are expanded only when every directory component exists.
#[cfg(not(windows))]
pub const DEFAULT_SEARCH_PATHS: &[&str] = &[
    // 3.x sandbox cache
    "~/Library/Containers/com.tencent.xinWeChat/Data/.wxapplet/packages",
    "~/Library/Containers/com.tencent.xinWeChat/Data/.wxapplet/icon",
    // 4.0 sandbox cache
    "~/Library/Containers/com.tencent.xinWeChat/Data/Documents/app_data/radium/Applet/packages",
    "~/Library/Containers/com.tencent.xinWeChat/Data/Documents/app_data/radium/Applet/icon",
    // 4.1.7+ / 4.2.0 account-isolated caches in the standard WeChat container
    "~/Library/Containers/com.tencent.xinWeChat/Data/Documents/app_data/radium/users/*/applet/packages",
    "~/Library/Containers/com.tencent.xinWeChat/Data/Documents/app_data/radium/users/*/applet/icon",
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

#[cfg(windows)]
pub const DEFAULT_SEARCH_PATHS: &[&str] = &[
    "%APPDATA%/Tencent/WeChat/WechatApp/LocalCache",
    "%LOCALAPPDATA%/Tencent/WeChat/WechatApp/LocalCache",
    "%APPDATA%/Tencent/WeChat/XPlugin/Plugins/WMPF/*/Applet",
    "%APPDATA%/Tencent/xwechat/radium/Applet",
    "%LOCALAPPDATA%/Tencent/xwechat/radium/Applet",
    "%APPDATA%/Tencent/xwechat/radium/users/*/applet/packages",
    "%APPDATA%/Tencent/xwechat/radium/users/*/applet/icon",
    "%LOCALAPPDATA%/Tencent/xwechat/radium/users/*/applet/packages",
    "%LOCALAPPDATA%/Tencent/xwechat/radium/users/*/applet/icon",
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

impl Snapshot {
    pub fn apply_custom_names(&mut self, names: &BTreeMap<String, String>) {
        for app in &mut self.miniapps {
            if let Some(name) = names.get(&app.app_id) {
                app.name = Some(name.clone());
                app.metadata_source = Some("自定义名称".into());
            }
        }
    }
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

#[derive(Debug)]
struct PackageInfo {
    path: PathBuf,
    size: i64,
    modified: SystemTime,
    is_main: bool,
    is_plugin: bool,
    generation_key: PathBuf,
}

pub fn scan(custom_paths: &[String]) -> Snapshot {
    let paths: Vec<String> = DEFAULT_SEARCH_PATHS.iter().map(|path| path.to_string())
        .chain(crate::platform::wechat_cache_paths())
        .chain(custom_paths.iter().cloned()).collect();
    scan_paths(&paths)
}

fn scan_paths(paths: &[String]) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let mut packages_by_app: Vec<(String, Vec<PackageInfo>)> = Vec::new();
    let mut app_indices: HashMap<String, usize> = HashMap::new();
    let mut icons: HashMap<String, (SystemTime, Vec<u8>)> = HashMap::new();
    let mut seen_packages = HashSet::new();
    let mut seen_inaccessible = HashSet::new();
    let started = std::time::Instant::now();

    let (roots, blocked_roots) = normalized_roots(paths);
    for blocked in blocked_roots {
        if seen_inaccessible.insert(identity_key(&blocked, None)) {
            snapshot.inaccessible_roots.push(blocked);
        }
    }

    for root in roots {
        match std::fs::metadata(&root) {
            Ok(metadata) if metadata.is_dir() => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                if seen_inaccessible.insert(identity_key(&root, None)) {
                    snapshot.inaccessible_roots.push(root);
                }
                continue;
            }
            _ => {
                snapshot.missing_roots.push(root);
                continue;
            }
        }
        if !is_readable(&root) {
            if seen_inaccessible.insert(identity_key(&root, None)) {
                snapshot.inaccessible_roots.push(root);
            }
            continue;
        }
        snapshot.readable_roots.push(root.clone());

        snapshot.enumeration_errors += walk(&root, |entry| {
            let path = entry.path();
            let icon_app = app_id_from_icon(&path);
            if icon_app.is_none() && !extension_is(&path, "wxapkg") {
                return;
            }
            let Ok(metadata) = std::fs::metadata(&path) else { return };
            if !metadata.is_file() {
                return;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if let Some(app_id) = icon_app {
                if metadata.len() <= 1_000_000
                    && icons.get(&app_id).is_none_or(|(current, _)| *current < modified)
                    && let Ok(data) = std::fs::read(&path)
                {
                    icons.insert(app_id, (modified, data));
                }
                return;
            }
            if !seen_packages.insert(identity_key(&path, Some(&metadata))) {
                return;
            }
            let size = metadata.len() as i64;
            snapshot.diagnostics.total_package_files += 1;
            snapshot.diagnostics.total_package_bytes += size;
            let Some(app_id) = app_id_from_path(&path) else { return };
            snapshot.diagnostics.identified_package_files += 1;
            let package = PackageInfo {
                size,
                modified,
                is_main: is_main_package(&path),
                is_plugin: is_plugin_package(&path),
                generation_key: generation_key(&path, &app_id),
                path,
            };
            snapshot.diagnostics.plugin_package_files += usize::from(package.is_plugin);
            if let Some(&index) = app_indices.get(&app_id) {
                packages_by_app[index].1.push(package);
            } else {
                app_indices.insert(app_id.clone(), packages_by_app.len());
                packages_by_app.push((app_id, vec![package]));
            }
        });
    }

    for (app_id, packages) in packages_by_app {
        let groups = group_generations(&packages);
        if packages.iter().all(|package| package.is_plugin) {
            snapshot.plugin_generations.insert(app_id, reported_generations(&groups, true, None));
            continue;
        }
        let mut ranked: Vec<_> = groups.iter().filter_map(|group| Some((group, group.main()?))).collect();
        // A total order keeps mixed numeric and named generations deterministic.
        ranked.sort_by_cached_key(|(group, _)| std::cmp::Reverse(group.rank()));
        let Some((&(selected, main), historical)) = ranked.split_first() else { continue };

        let older = reported_generations(&groups, false, Some(selected.directory()));
        snapshot.diagnostics.old_version_package_files += older.iter().map(|g| g.packages.len()).sum::<usize>();
        if !older.is_empty() {
            snapshot.older_generations.insert(app_id.clone(), older);
        }

        let mut sub_packages = Vec::new();
        let mut size = main.size;
        let mut modified = main.modified;
        for package in selected.members.iter().filter(|p| !p.is_plugin && p.path != main.path) {
            size += package.size;
            modified = modified.max(package.modified);
            sub_packages.push(package.path.clone());
        }
        sub_packages.sort_by(|a, b| natural_compare(&a.to_string_lossy(), &b.to_string_lossy()));
        snapshot.diagnostics.selected_package_files += 1 + sub_packages.len();
        let metadata = resolve_metadata(&app_id, main, selected, historical);
        let cached_icon = icons.remove(&app_id).map(|(_, data)| data);
        let metadata_source = match (metadata.source, cached_icon.is_some()) {
            (Some(source), true) => Some(format!("本地图标与{source}")),
            (None, true) => Some("本地图标缓存".into()),
            (source, false) => source,
        };
        snapshot.miniapps.push(Miniapp {
            app_id,
            name: metadata.name,
            main_package: main.path.clone(),
            sub_packages,
            size,
            modified,
            version: metadata.version,
            icon: cached_icon.or(metadata.icon),
            metadata_source,
            declared_subpackage_roots: metadata.declared_subpackage_roots,
        });
    }
    snapshot.miniapps.sort_by_key(|app| std::cmp::Reverse(app.modified));
    let diagnostics = &mut snapshot.diagnostics;
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

struct CachedGeneration<'a> {
    version: String,
    members: Vec<&'a PackageInfo>,
    newest: SystemTime,
    explicit_main: Option<&'a PackageInfo>,
    fallback_main: Option<&'a PackageInfo>,
}

impl<'a> CachedGeneration<'a> {
    fn push(&mut self, package: &'a PackageInfo) {
        self.members.push(package);
        self.newest = self.newest.max(package.modified);
        if package.is_plugin {
            return;
        }
        if package.is_main && self.explicit_main.is_none_or(|main| main.modified < package.modified) {
            self.explicit_main = Some(package);
        }
        if self.fallback_main.is_none_or(|main| (main.size, main.modified) < (package.size, package.modified)) {
            self.fallback_main = Some(package);
        }
    }

    fn main(&self) -> Option<&'a PackageInfo> {
        self.explicit_main.or(self.fallback_main)
    }

    fn directory(&self) -> &'a Path {
        &self.members[0].generation_key
    }

    fn rank(&self) -> (bool, Option<i64>, SystemTime) {
        (self.explicit_main.is_some(), self.version.parse().ok(), self.newest)
    }
}

fn group_generations(packages: &[PackageInfo]) -> Vec<CachedGeneration<'_>> {
    let mut indices = HashMap::new();
    let mut groups = Vec::new();
    for package in packages {
        let index = *indices.entry(package.generation_key.as_path()).or_insert_with(|| {
            let index = groups.len();
            groups.push(CachedGeneration {
                version: file_name(&package.generation_key),
                members: Vec::new(),
                newest: package.modified,
                explicit_main: None,
                fallback_main: None,
            });
            index
        });
        groups[index].push(package);
    }
    groups
}

fn reported_generations(groups: &[CachedGeneration<'_>], include_plugins: bool, exclude: Option<&Path>) -> Vec<Generation> {
    let mut result = Vec::new();
    for group in groups.iter().filter(|group| Some(group.directory()) != exclude) {
        let mut members: Vec<_> = group.members.iter()
            .filter(|p| include_plugins || !p.is_plugin)
            .map(|p| p.path.clone())
            .collect();
        if !members.is_empty() {
            members.sort_by_cached_key(|p| file_name(p));
            result.push(Generation { version: group.version.clone(), directory: group.directory().to_path_buf(), packages: members });
        }
    }
    result.sort_by(|left, right| {
        match (left.version.parse::<i64>().ok(), right.version.parse::<i64>().ok()) {
            (None, None) => right.version.cmp(&left.version),
            (left, right) => right.cmp(&left),
        }
    });
    result
}

fn resolve_metadata(app_id: &str, main: &PackageInfo, selected: &CachedGeneration<'_>, historical: &[(&CachedGeneration<'_>, &PackageInfo)]) -> Metadata {
    let primary = package_metadata(&main.path, app_id);
    if primary.name.is_some() {
        return primary;
    }
    let mut remaining = MAXIMUM_METADATA_FALLBACK_PACKAGES;
    let mut current: Vec<_> = selected.members.iter().copied().filter(|p| p.path != main.path && !p.is_plugin).collect();
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
    for &(_, historical) in historical {
        if remaining == 0 {
            break;
        }
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

#[derive(Debug, Default, PartialEq, Eq)]
struct ExpandedRoots {
    candidates: Vec<PathBuf>,
    inaccessible: Vec<PathBuf>,
}

fn normalized_roots(paths: &[String]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut seen_roots = HashSet::new();
    let mut seen_inaccessible = HashSet::new();
    let mut roots = Vec::new();
    let mut inaccessible = Vec::new();
    for path in paths {
        let expanded = expanded_roots(path);
        for url in expanded.candidates {
            let normalized = std::fs::canonicalize(&url).unwrap_or(url);
            if seen_roots.insert(identity_key(&normalized, None)) {
                roots.push(normalized);
            }
        }
        for url in expanded.inaccessible {
            let normalized = std::fs::canonicalize(&url).unwrap_or(url);
            if seen_inaccessible.insert(identity_key(&normalized, None)) {
                inaccessible.push(normalized);
            }
        }
    }
    (roots, inaccessible)
}

fn expanded_roots(path: &str) -> ExpandedRoots {
    use std::io::ErrorKind;
    use std::path::Component;

    let expanded = crate::platform::expand_path(path);
    if !expanded.components().any(|component| component.as_os_str() == "*") {
        return ExpandedRoots { candidates: vec![expanded], inaccessible: Vec::new() };
    }
    let mut candidates = vec![PathBuf::new()];
    let mut inaccessible = Vec::new();
    for component in expanded.components() {
        if matches!(component, Component::Normal(name) if name == "*") {
            let mut next_candidates = Vec::new();
            for parent in &candidates {
                let dir = if parent.as_os_str().is_empty() { Path::new(".") } else { parent.as_path() };
                let entries = match std::fs::read_dir(dir) {
                    Ok(entries) => entries,
                    Err(error) => {
                        if error.kind() == ErrorKind::PermissionDenied {
                            inaccessible.push(dir.to_path_buf());
                        }
                        continue;
                    }
                };
                let mut children: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                    .filter(|entry| std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()))
                    .map(|entry| entry.path())
                    .collect();
                if parent.file_name().is_some_and(|name| name == "Containers") {
                    children.retain(|child| is_likely_host_container(&file_name(child)));
                }
                next_candidates.extend(children);
            }
            candidates = next_candidates;
        } else {
            let mut next_candidates = Vec::new();
            for parent in &candidates {
                let child = parent.join(component.as_os_str());
                if matches!(component, Component::Prefix(_) | Component::RootDir) {
                    next_candidates.push(child);
                    continue;
                }
                match std::fs::metadata(&child) {
                    Ok(metadata) if metadata.is_dir() => next_candidates.push(child),
                    Err(error) if error.kind() == ErrorKind::PermissionDenied => inaccessible.push(child),
                    _ => {}
                }
            }
            candidates = next_candidates;
        }
        if candidates.is_empty() {
            break;
        }
    }
    ExpandedRoots { candidates, inaccessible }
}

fn is_likely_host_container(name: &str) -> bool {
    let name = name.to_lowercase();
    name.contains("tencent") || name.contains("wechat") || name.contains("weixin")
}

/// Depth-first iteration in directory order without retaining the entire tree.
/// Directory symlinks are never traversed.
fn walk(root: &Path, mut file: impl FnMut(std::fs::DirEntry)) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else { return 1 };
    let mut stack = vec![entries];
    let mut errors = 0;
    while let Some(entries) = stack.last_mut() {
        let entry = match entries.next() {
            Some(Ok(entry)) => entry,
            Some(Err(_)) => {
                errors += 1;
                continue;
            }
            None => {
                stack.pop();
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => match std::fs::read_dir(entry.path()) {
                Ok(children) => stack.push(children),
                Err(_) => errors += 1,
            },
            Ok(kind) if kind.is_file() || kind.is_symlink() => file(entry),
            Ok(_) => {}
            Err(_) => errors += 1,
        }
    }
    errors
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

fn is_readable(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok()
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn extension_is(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case(extension))
}

fn is_strict_app_id(name: &str) -> bool {
    name.len() == 18 && name.starts_with("wx") && name[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_excluded_wx_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["wxid_", "wxlocated", "wxapplet", "wxcomponents", "wxwork"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

fn is_app_id_like(name: &str) -> bool {
    if is_strict_app_id(name) {
        return true;
    }
    name.len() >= 10
        && name.starts_with("wx")
        && name[2..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && name[2..].bytes().any(|b| b.is_ascii_digit())
        && !is_excluded_wx_name(name)
}

/// The nearest strict AppID component, else an embedded strict AppID run, else
/// the nearest AppID-like component or run.
fn app_id_from_path(path: &Path) -> Option<String> {
    if let Some(component) = path.components().rev().filter_map(|c| c.as_os_str().to_str()).find(|name| is_strict_app_id(name)) {
        return Some(component.to_string());
    }
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    let mut index = 0;
    while let Some(found) = text[index..].find("wx") {
        let start = index + found;
        let end = start + 18;
        if let Some(candidate) = text.get(start..end)
            && is_strict_app_id(candidate)
            && !bytes.get(end).is_some_and(u8::is_ascii_hexdigit)
            && !(start > 0 && bytes[start - 1].is_ascii_hexdigit())
            && !text[..start].to_ascii_lowercase().ends_with("wxid_")
        {
            return Some(candidate.to_string());
        }
        index = start + 1;
    }
    if let Some(component) = path.components().rev().filter_map(|c| c.as_os_str().to_str()).find(|name| is_app_id_like(name)) {
        return Some(component.to_string());
    }
    index = 0;
    while let Some(found) = text[index..].find("wx") {
        let start = index + found;
        let end = start + 2 + bytes[start + 2..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'-').count();
        let candidate = &text[start..end];
        if is_app_id_like(candidate) {
            return Some(candidate.to_string());
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
    let upper = path.to_string_lossy().replace('\\', "/").to_uppercase();
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("WxLocated 扫描 test {}", crate::export::uuid_string()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn package(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            // One ordinary app.js entry, enough to exercise real archive indexing.
            let name = b"app.js";
            let body = b"module.exports = {};";
            let index_size = 4 + 12 + name.len() as u32;
            let mut bytes = vec![0xbe, 0, 0, 0, 0];
            bytes.extend(index_size.to_be_bytes());
            bytes.extend((body.len() as u32).to_be_bytes());
            bytes.push(0xed);
            bytes.extend(1u32.to_be_bytes());
            bytes.extend((name.len() as u32).to_be_bytes());
            bytes.extend(name);
            bytes.extend((14 + index_size).to_be_bytes());
            bytes.extend((body.len() as u32).to_be_bytes());
            bytes.extend(body);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn scans_account_wildcards_without_mixing_versions_or_plugins() {
        let fixture = Fixture::new();
        fixture.package("account A/Applet/wx0123456789abcdef/1/__APP__.wxapkg");
        let main = fixture.package("account A/Applet/wx0123456789abcdef/2/__APP__.wxapkg");
        let sub = fixture.package("account A/Applet/wx0123456789abcdef/2/sub.wxapkg");
        fixture.package("account B/Applet/wx1111111111111111/1/__APP__.wxapkg");
        fixture.package("account A/Applet/wx2222222222222222/1/PLUGINS/plugin.wxapkg");
        fixture.package("account A/Applet/wx0123456789abcdef/3/PLUGINS/__APP__.wxapkg");
        fixture.package("account A/Applet/wx0123456789abcdef/foo/__APP__.wxapkg");
        let pattern = fixture.0.join("*/Applet").to_string_lossy().into_owned();
        // Duplicate roots and mixed path separators must not duplicate packages.
        let snapshot = scan_paths(&[pattern, fixture.0.join("account A/Applet").to_string_lossy().into_owned()]);
        assert_eq!(snapshot.miniapps.len(), 2);
        assert_eq!(snapshot.diagnostics.total_package_files, 7);
        let app = snapshot.miniapps.iter().find(|app| app.app_id == "wx0123456789abcdef").unwrap();
        assert!(crate::platform::paths_equal(&app.main_package, &main));
        assert_eq!(app.sub_packages.len(), 1);
        assert!(crate::platform::paths_equal(&app.sub_packages[0], &sub));
        let older = &snapshot.older_generations[&app.app_id];
        assert_eq!(older.iter().map(|g| g.version.as_str()).collect::<Vec<_>>(), ["1", "foo"]);
        assert_eq!(snapshot.plugin_generations["wx2222222222222222"].len(), 1);
        assert_eq!(snapshot.enumeration_errors, 0);
    }

    #[cfg(windows)]
    #[test]
    fn scans_paths_longer_than_max_path() {
        use std::os::windows::ffi::OsStrExt;
        let fixture = Fixture::new();
        let relative = format!("{}/{}/wx0123456789abcdef/1/__APP__.wxapkg", "a".repeat(120), "包".repeat(100));
        let package = fixture.package(&relative);
        assert!(package.as_os_str().encode_wide().count() > 260);
        let snapshot = scan_paths(&[fixture.0.to_string_lossy().into_owned()]);
        assert_eq!(snapshot.miniapps.len(), 1);
        assert!(crate::platform::paths_equal(&snapshot.miniapps[0].main_package, &package));
    }

    #[cfg(windows)]
    #[test]
    fn expands_drive_paths_with_both_separators() {
        let fixture = Fixture::new();
        fixture.package("user/Applet/wx0123456789abcdef/1/__APP__.wxapkg");
        let pattern = fixture.0.join("*/Applet");
        let native = expanded_roots(&pattern.to_string_lossy()).candidates;
        let forward = expanded_roots(&pattern.to_string_lossy().replace('\\', "/")).candidates;
        assert_eq!(native.len(), 1);
        assert_eq!(native, forward);
    }

    #[cfg(unix)]
    #[test]
    fn reports_permission_denied_wildcard_parent_as_inaccessible() {
        use std::os::unix::fs::PermissionsExt;

        struct PermissionGuard(PathBuf);
        impl Drop for PermissionGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }

        let fixture = Fixture::new();
        fixture.package("users/acc/applet/packages/wx0123456789abcdef/1/__APP__.wxapkg");
        let users = fixture.0.join("users");
        let _guard = PermissionGuard(users.clone());
        std::fs::set_permissions(&users, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&users).is_ok() {
            // Skip when running as root, which bypasses directory mode bits.
            return;
        }
        let pattern = fixture.0.join("users/*/applet/packages").to_string_lossy().into_owned();
        let snapshot = scan_paths(&[pattern]);
        assert!(snapshot.readable_roots.is_empty());
        assert_eq!(snapshot.inaccessible_roots.len(), 1);
        assert!(crate::platform::paths_equal(&snapshot.inaccessible_roots[0], &users));
    }

    #[test]
    fn ignores_wechat_user_ids_and_tool_folders_as_app_ids() {
        assert_eq!(app_id_from_path(Path::new("/Users/a/WeChat Files/wxid_smoke_account/Applet/test.wxapkg")), None);
        assert_eq!(app_id_from_path(Path::new("/tmp/wxlocated-samples/test.wxapkg")), None);
        assert_eq!(
            app_id_from_path(Path::new("/cache/wx0123456789abcdef/1/wxcomponents/sub.wxapkg")).as_deref(),
            Some("wx0123456789abcdef")
        );
    }
}
