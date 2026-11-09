//! Audit material: the declared-versus-cached view of an app and the
//! `material.json` report. Structural facts only; no rule matching.

pub mod export;
pub mod report;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::scan::{Generation, Miniapp, Snapshot};

/// Cache facts beyond the app's own selected version.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Provider AppID → newest cached plugin generation.
    pub plugins: HashMap<String, Generation>,
    pub older_generations: Vec<Generation>,
}

impl Context {
    pub fn for_app(snapshot: &Snapshot, app: &Miniapp) -> Self {
        Self {
            plugins: snapshot
                .plugin_generations
                .iter()
                .filter_map(|(id, generations)| generations.first().map(|g| (id.clone(), g.clone())))
                .collect(),
            older_generations: snapshot.older_generations.get(&app.app_id).cloned().unwrap_or_default(),
        }
    }
}

/// Plugins declared in app.json / subpackages: alias → provider AppID.
pub type DeclaredPlugins = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct Completeness {
    pub declared_subpackages: Vec<String>,
    pub found_subpackages: Vec<String>,
    pub missing_subpackages: Vec<String>,
    /// Cached subpackage files whose root is not declared.
    pub undeclared_subpackage_files: Vec<String>,
    pub plugins: DeclaredPlugins,
    /// Declared plugin alias → cached plugin code (by provider AppID).
    pub found_plugins: BTreeMap<String, Generation>,
    pub missing_plugins: Vec<String>,
    /// Missing subpackage root → older cached version that still has it.
    pub missing_in_older_versions: BTreeMap<String, String>,
}

impl Completeness {
    pub fn new(app: &Miniapp, plugins: Option<&DeclaredPlugins>, context: &Context) -> Self {
        let declared: Vec<String> = app
            .declared_subpackage_roots
            .iter()
            .map(|root| normalized_root(root))
            .filter(|root| !root.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        // First file per cache key wins.
        let mut cached: Vec<(String, String)> = Vec::new();
        for path in &app.sub_packages {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let key = cache_key_for_file(&name);
            if !cached.iter().any(|(existing, _)| *existing == key) {
                cached.push((key, name));
            }
        }
        let (mut found, mut missing) = (Vec::new(), Vec::new());
        for root in &declared {
            if cached.iter().any(|(key, _)| *key == cache_key_for_root(root)) {
                found.push(root.clone());
            } else {
                missing.push(root.clone());
            }
        }
        let matched: BTreeSet<String> = found.iter().map(|root| cache_key_for_root(root)).collect();
        let mut undeclared: Vec<String> = cached.iter().filter(|(key, _)| !matched.contains(key)).map(|(_, name)| name.clone()).collect();
        undeclared.sort();

        let plugins = plugins.cloned().unwrap_or_default();
        let found_plugins: BTreeMap<String, Generation> = plugins
            .iter()
            .filter_map(|(alias, provider)| context.plugins.get(provider).map(|generation| (alias.clone(), generation.clone())))
            .collect();
        let missing_plugins = plugins.keys().filter(|alias| !found_plugins.contains_key(*alias)).cloned().collect();

        let mut older = BTreeMap::new();
        for root in &missing {
            let key = cache_key_for_root(root);
            if let Some(generation) = context.older_generations.iter().find(|generation| {
                generation.packages.iter().any(|path| {
                    cache_key_for_file(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) == key
                })
            }) {
                older.insert(root.clone(), generation.version.clone());
            }
        }

        Self {
            declared_subpackages: declared,
            found_subpackages: found,
            missing_subpackages: missing,
            undeclared_subpackage_files: undeclared,
            plugins,
            found_plugins,
            missing_plugins,
            missing_in_older_versions: older,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.missing_subpackages.is_empty() && self.missing_plugins.is_empty()
    }
}

pub fn normalized_root(root: &str) -> String {
    root.replace('\\', "/").trim_matches(|c| c == '/' || c == ' ').to_string()
}

/// `pages/news` → `_pages_news_`, matching WeChat's cached `_pages_news_.wxapkg`.
pub fn cache_key_for_root(root: &str) -> String {
    format!("_{}_", normalized_root(root).replace('/', "_"))
}

pub fn cache_key_for_file(file_name: &str) -> String {
    let mut name = file_name.to_string();
    if name.to_lowercase().ends_with(".wxapkg") {
        name.truncate(name.len() - 7);
    }
    if !name.starts_with('_') {
        name.insert(0, '_');
    }
    if !name.ends_with('_') {
        name.push('_');
    }
    name
}
