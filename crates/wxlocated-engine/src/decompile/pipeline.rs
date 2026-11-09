//! The decompilation pipeline over one package set. Stages run in a fixed
//! order because later ones read what earlier ones produced (markup needs the
//! WXS modules; glass-easel defers to `$gwx` results already in the store).

use std::collections::HashMap;

use super::{
    Artifact, ArtifactStore, Confidence, Content, Diagnostic, MAXIMUM_TEXT_FILE_SIZE, MAXIMUM_TOTAL_TEXT_BYTES,
    ArtifactKind, Severity, SourceRef, Stage, configuration, file_name, path_extension, scripts, wxml, wxs, wxss,
};
use crate::json::Value;
use crate::archive::{PackageSet, SetEntry, natural_compare};

pub struct Decompilation {
    /// In natural path order.
    pub artifacts: Vec<Artifact>,
    pub diagnostics: Vec<Diagnostic>,
}

struct TextEntry {
    entry: SetEntry,
    text: String,
    source: SourceRef,
}

/// Modules by page path, then module name.
pub type WxsModules = HashMap<String, HashMap<String, String>>;

pub fn decompile(set: &PackageSet) -> Decompilation {
    decompile_reporting(set, &|_| {})
}

/// [`decompile`], calling `progress` as each stage starts (for a progress UI).
pub fn decompile_reporting(set: &PackageSet, progress: &dyn Fn(Stage)) -> Decompilation {
    progress(Stage::Indexing);
    let mut store = ArtifactStore::default();

    for (index, entry) in set.entries.iter().enumerate() {
        let source = source_reference(set, entry);
        let raw_path = raw_artifact_path(set, entry);
        let note = if raw_path == entry.path { "包内文件原样保留" } else { "编译容器原样归档" };
        store.add(
            &raw_path,
            None,
            Confidence::Exact,
            Content::Entry(index),
            vec![source.clone()],
            vec![note.into()],
            Stage::Indexing,
        );
        if set.is_composite() {
            let folder = &set.sources[entry.source].export_folder;
            store.add(
                &format!("__wxlocated_packages__/{folder}/{}", entry.path),
                None,
                Confidence::Exact,
                Content::Entry(index),
                vec![source],
                vec!["跨包来源副本".into()],
                Stage::Indexing,
            );
        }
    }

    let text_entries = read_text_entries(set, &mut store);

    let mut embedded_page_configurations: HashMap<String, Value> = HashMap::new();
    for item in text_entries.iter().filter(|item| is_javascript(&item.entry.path)) {
        for (path, value) in scripts::embedded_json(&item.text) {
            embedded_page_configurations.entry(path).or_insert(value);
        }
    }
    let resource_hashes = resource_hashes(set);
    let worker_root = set.entries.iter().any(|entry| file_name(&entry.path).to_lowercase() == "workers.js").then_some("workers");
    let configurations: Vec<&TextEntry> =
        text_entries.iter().filter(|item| item.entry.path.to_lowercase().ends_with("app-config.json")).collect();
    progress(Stage::Configuration);
    for item in &configurations {
        let restoration =
            configuration::restore(&item.text, &item.source, &embedded_page_configurations, &resource_hashes, worker_root);
        store.add_candidates(restoration.files, &item.source, Stage::Configuration);
        store.diagnostics.extend(restoration.diagnostics);
    }
    if configurations.is_empty() {
        store.diagnostics.push(Diagnostic::new(
            Severity::Information,
            Stage::Configuration,
            "包内未出现 app-config.json，原始 JSON 配置仍已保留",
            None,
        ));
    }

    progress(Stage::Scripts);
    for item in &text_entries {
        let lower = item.entry.path.to_lowercase();
        if lower.ends_with(".map") {
            let restoration = scripts::restore_source_map(&item.text, &item.entry.path, &item.source);
            store.add_candidates(restoration.files, &item.source, Stage::Scripts);
            store.diagnostics.extend(restoration.diagnostics);
        }
        if is_javascript(&lower) && is_script_container(&lower, &item.text) {
            let modules = scripts::restore_modules(&item.text);
            let empty = modules.is_empty();
            for module in modules {
                store.add(
                    &module.path,
                    Some(ArtifactKind::Script),
                    module.confidence,
                    Content::Generated(module.content),
                    vec![item.source.applying(&module.source_range)],
                    vec![format!("由 {} 模块工厂静态拆分", item.entry.path)],
                    Stage::Scripts,
                );
            }
            if empty && is_compiled_container(&lower) {
                store.diagnostics.push(Diagnostic::new(
                    Severity::Warning,
                    Stage::Scripts,
                    format!("脚本容器未识别到可静态拆分的模块：{}", item.entry.path),
                    Some(item.source.clone()),
                ));
            }
        }
    }

    let mut inline_wxs: WxsModules = HashMap::new();
    let mut external_wxs: WxsModules = HashMap::new();
    progress(Stage::Wxs);
    for item in text_entries.iter().filter(|item| is_frame_source(&item.entry.path, &item.text)) {
        let restoration = wxs::restore(&item.text, &item.source);
        store.add_candidates(restoration.files, &item.source, Stage::Wxs);
        merge_wxs(restoration.inline_modules, &mut inline_wxs);
        merge_wxs(restoration.external_modules, &mut external_wxs);
        store.diagnostics.extend(restoration.diagnostics);
    }

    let markup_inputs = text_entries.iter().filter(|item| {
        is_frame_source(&item.entry.path, &item.text)
            || matches!(path_extension(&item.entry.path).to_ascii_lowercase().as_str(), "html" | "htm")
    });
    progress(Stage::Markup);
    for item in markup_inputs {
        let restoration = wxml::restore(&item.text, &item.entry.path, &item.source, &inline_wxs, &external_wxs);
        store.add_candidates(restoration.files, &item.source, Stage::Markup);
        store.diagnostics.extend(restoration.diagnostics);
        // glass-easel templates live in the same frame files as the `$gwx`
        // compat shim but cover their own paths; a few components are
        // registered both ways. The deterministic `$gwx` result runs first and
        // wins those; a glass-easel template is added only where nothing holds
        // its path at equal-or-higher confidence, which still lets a
        // render-layer template replace a lower-confidence logic-layer copy.
        if item.text.contains("batchAddCompiledTemplate") {
            let glass_easel = wxml::glass_easel::restore(&item.text, &item.entry.path, &item.source);
            let fresh: Vec<_> = glass_easel
                .files
                .into_iter()
                .filter(|file| store.artifacts.get(&file.path).is_none_or(|existing| existing.confidence < file.confidence))
                .collect();
            store.add_candidates(fresh, &item.source, Stage::Markup);
            store.diagnostics.extend(glass_easel.diagnostics);
        }
    }

    progress(Stage::Styles);
    for item in text_entries.iter().filter(|item| is_style_container(&item.entry.path, &item.text)) {
        let restoration = wxss::restore(&item.text, &item.entry.path, &item.source);
        store.add_candidates(restoration.files, &item.source, Stage::Styles);
        store.diagnostics.extend(restoration.diagnostics);
    }

    progress(Stage::Finalizing);
    let mut artifacts: Vec<Artifact> = store.artifacts.into_values().collect();
    artifacts.sort_by(|a, b| natural_compare(&a.path, &b.path));
    Decompilation { artifacts, diagnostics: store.diagnostics }
}

fn read_text_entries(set: &PackageSet, store: &mut ArtifactStore) -> Vec<TextEntry> {
    let mut result = Vec::new();
    let mut total_bytes = 0u64;
    for entry in set.entries.iter().filter(|entry| should_read_text(&entry.path)) {
        if entry.size > MAXIMUM_TEXT_FILE_SIZE {
            store.diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Indexing,
                format!("文本文件超过单文件分析上限：{}", entry.path),
                Some(source_reference(set, entry)),
            ));
            continue;
        }
        if total_bytes + entry.size > MAXIMUM_TOTAL_TEXT_BYTES {
            store.diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Indexing,
                "文本读取达到总量上限，后续文件仅原样保留",
                None,
            ));
            break;
        }
        let Ok(data) = set.read_limited(entry, MAXIMUM_TEXT_FILE_SIZE) else { continue };
        if data.contains(&0) {
            continue;
        }
        let Some(text) = decode_utf8(data) else { continue };
        total_bytes += entry.size;
        result.push(TextEntry { entry: entry.clone(), text, source: source_reference(set, entry) });
    }
    result
}

/// Foundation's `String(data:encoding: .utf8)`: strict UTF-8, a leading BOM
/// dropped.
fn decode_utf8(mut data: Vec<u8>) -> Option<String> {
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        data.drain(..3);
    }
    String::from_utf8(data).ok()
}

fn source_reference(set: &PackageSet, entry: &SetEntry) -> SourceRef {
    SourceRef { package: set.source_title(entry).to_string(), entry_path: entry.path.clone(), start: None, end: None }
}

fn raw_artifact_path(set: &PackageSet, entry: &SetEntry) -> String {
    if !is_compiled_container(&entry.path) {
        return entry.path.clone();
    }
    format!("__wxlocated_raw__/{}/{}", set.sources[entry.source].export_folder, entry.path)
}

fn merge_wxs(source: WxsModules, target: &mut WxsModules) {
    for (page, modules) in source {
        let existing = target.entry(page).or_default();
        for (module, value) in modules {
            existing.entry(module).or_insert(value);
        }
    }
}

/// `PackageArchiveEntry.isTextPreviewCandidate`, `.wxs`/`.map`, or a compiled
/// container.
fn should_read_text(path: &str) -> bool {
    let extension = path_extension(path).to_ascii_lowercase();
    let text_kind = matches!(
        extension.as_str(),
        "json" | "map" | "js" | "mjs" | "ts" | "wxml" | "html" | "htm" | "xml" | "svg" | "wxss" | "css" | "scss" | "less"
            | "txt" | "md" | "yaml" | "yml" | "csv"
    );
    text_kind || extension == "wxs" || is_compiled_container(path)
}

pub(crate) fn is_compiled_container(path: &str) -> bool {
    let name = file_name(path).to_lowercase();
    matches!(
        name.as_str(),
        "app-service.js"
            | "appservice.js"
            | "workers.js"
            | "app-wxss.js"
            | "page-frame.js"
            | "page-frame.html"
            | "page-frame.css"
            | "common.app.js"
            | "game.js"
    ) || name.starts_with("page-frame.")
}

/// SHA-256 → path of every image entry up to 4 MiB (a later entry wins).
fn resource_hashes(set: &PackageSet) -> HashMap<String, String> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut result = HashMap::new();
    for entry in &set.entries {
        let is_image = matches!(
            path_extension(&entry.path).to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico"
        );
        if !is_image || entry.size > LIMIT {
            continue;
        }
        if let Ok(data) = set.read_limited(entry, LIMIT) {
            result.insert(crate::sha256_hex(&data), entry.path.clone());
        }
    }
    result
}

fn is_javascript(path: &str) -> bool {
    matches!(path_extension(path).to_ascii_lowercase().as_str(), "js" | "mjs" | "cjs")
}

fn is_script_container(path: &str, source: &str) -> bool {
    is_compiled_container(path)
        || source.contains("__wxAppCode__")
        || source.contains("__defineModule__")
        || source.contains("definePlugin(")
}

fn is_style_container(path: &str, source: &str) -> bool {
    let name = file_name(path).to_lowercase();
    name == "app-wxss.js" || name.starts_with("page-frame") || source.contains("setCssToHead")
}

pub(crate) fn is_frame_source(path: &str, source: &str) -> bool {
    file_name(path).to_lowercase().starts_with("page-frame") || source.contains("__WXML_GLOBAL__") || source.contains("$gwx")
}
