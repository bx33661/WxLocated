//! `wxlocated export`: `<parent>/<appid>-v<version>/` with `raw/`, `source/`,
//! `plugins/`, `semantic/`, `material.json` and `AGENT.md`, built in a staging
//! folder and moved into place at the end.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use super::Context;
use super::report::{self, Inspection, ReportInput, inspect_package};
use crate::archive::{Archive, PackageSet};
use crate::decompile::decompile;
use crate::export::{export_source, uuid_string};
use crate::overlay;
use crate::scan::Miniapp;
use crate::swift_json::encode;

#[derive(Debug, Clone)]
pub struct Options {
    pub include_source: bool,
    pub include_raw: bool,
    pub replace_existing: bool,
    /// Return an existing export untouched when its package hashes still match.
    pub reuse_if_unchanged: bool,
    /// Replace the home directory with `~` and the account directory with `<account>`.
    pub redact: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { include_source: true, include_raw: true, replace_existing: false, reuse_if_unchanged: false, redact: false }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MaterialExportError {
    #[error("目标目录已存在：{0}（使用 --force 覆盖）")]
    DestinationExists(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for MaterialExportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

pub fn folder_name(app: &Miniapp) -> String {
    match &app.version {
        Some(version) => format!("{}-v{version}", app.app_id),
        None => app.app_id.clone(),
    }
}

/// Returns the `ExportResult` document.
pub fn export(
    app: &Miniapp,
    parent: &Path,
    options: &Options,
    context: &Context,
    log: &dyn Fn(&str),
) -> Result<Value, MaterialExportError> {
    let destination = parent.join(folder_name(app));
    if options.reuse_if_unchanged
        && let Some(reused) = reusable_export(&destination, app, context, options)
    {
        log(&format!("包未变化，复用已有材料：{}", destination.to_string_lossy()));
        return Ok(reused);
    }
    if destination.exists() && !options.replace_existing {
        return Err(MaterialExportError::DestinationExists(destination.to_string_lossy().into_owned()));
    }
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".wxlocated-{}", uuid_string()));
    std::fs::create_dir_all(&staging)?;
    let result = build_into(app, &staging, &destination, options, context, log);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn build_into(
    app: &Miniapp,
    staging: &Path,
    destination: &Path,
    options: &Options,
    context: &Context,
    log: &dyn Fn(&str),
) -> Result<Value, MaterialExportError> {
    log(&format!("读取包：{} 个文件", app.all_packages().len()));
    let (set, archive_error) = match report::open_app(app) {
        Ok(set) => (Some(set), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let analysis = set.as_ref().map(report::analyze);

    log("校验包结构与 SHA-256");
    let inspections = report::inspect_app(app);

    let mut reconstruction = None;
    let mut source_directory: Option<PathBuf> = None;
    if options.include_source
        && let Some(set) = &set
    {
        log("重建源码");
        let started = Instant::now();
        let result = decompile(set);
        let duration = started.elapsed().as_secs_f64();
        let exported = export_source(&result, duration, set, staging, "source").map_err(|e| MaterialExportError::Io(e.to_string()))?;
        source_directory = Some(exported);
        reconstruction = Some((result, duration));
    }

    let mut raw_paths: HashMap<String, String> = HashMap::new();
    if options.include_raw {
        log("复制原始包");
        let raw = staging.join("raw");
        std::fs::create_dir_all(&raw)?;
        for path in app.all_packages() {
            let name = file_name(&path);
            std::fs::copy(&path, raw.join(&name))?;
            raw_paths.insert(name.clone(), format!("raw/{name}"));
        }
    }

    let mut report = report::build(&ReportInput {
        app,
        analysis: analysis.as_ref(),
        inspections: &inspections,
        reconstruction: reconstruction.as_ref().map(|(result, duration)| (result, *duration)),
        archive_error,
        archive_notes: set.as_ref().map(|s| s.notes.as_slice()).unwrap_or(&[]),
        context,
    });

    // Plugin code found in the cache: plugins/<alias>/raw and plugins/<alias>/source.
    let mut plugins_root = false;
    let plugin_count = report["wxlocated"]["plugins"].as_array().map_or(0, Vec::len);
    for index in 0..plugin_count {
        let plugin = report["wxlocated"]["plugins"][index].clone();
        if plugin["found"] != json!(true) {
            continue;
        }
        let alias = plugin["alias"].as_str().unwrap_or_default().to_string();
        let provider = plugin["provider"].as_str().unwrap_or_default().to_string();
        let folder = safe_folder_name(&alias);
        let plugin_root = staging.join("plugins").join(&folder);
        std::fs::create_dir_all(&plugin_root)?;
        plugins_root = true;
        log(&format!("插件 {alias}（{provider}）"));
        let packages = plugin["packages"].as_array().cloned().unwrap_or_default();
        if options.include_raw {
            let raw = plugin_root.join("raw");
            std::fs::create_dir_all(&raw)?;
            for (package_index, package) in packages.iter().enumerate() {
                let file = package["file"].as_str().unwrap_or_default();
                std::fs::copy(package["source_path"].as_str().unwrap_or_default(), raw.join(file))?;
                report["wxlocated"]["plugins"][index]["packages"][package_index]["raw_path"] = json!(format!("plugins/{folder}/raw/{file}"));
            }
        }
        if options.include_source
            && let Some(code) = packages.first().and_then(|p| p["source_path"].as_str())
            && let Ok(archive) = Archive::open_with_app_id(Path::new(code), Some(&provider))
        {
            let set = PackageSet::standalone(archive);
            let started = Instant::now();
            let result = decompile(&set);
            if let Ok(exported) = export_source(&result, started.elapsed().as_secs_f64(), &set, &plugin_root, "source") {
                report["wxlocated"]["plugins"][index]["source_directory"] = json!(format!("plugins/{folder}/{}/", file_name(&exported)));
            }
        }
    }
    if let Some(packages) = report["wxlocated"]["packages"].as_array_mut() {
        for package in packages {
            if let Some(raw) = package["file"].as_str().and_then(|file| raw_paths.get(file)) {
                package["raw_path"] = json!(raw);
            }
        }
    }
    if let Some(directory) = &source_directory {
        let name = file_name(directory);
        report["wxlocated"]["reconstruction"]["source_directory"] = json!(format!("{name}/"));
        report["wxlocated"]["reconstruction"]["manifest"] = json!(format!("{name}/WxLocated-decompilation.json"));
    }
    let mut layout = serde_json::Map::new();
    layout.insert("material".into(), json!("material.json"));
    layout.insert("readme".into(), json!("AGENT.md"));
    if options.include_raw {
        layout.insert("raw".into(), json!("raw/"));
    }
    if let Some(directory) = &source_directory {
        layout.insert("source".into(), json!(format!("{}/", file_name(directory))));
    }
    if plugins_root {
        layout.insert("plugins".into(), json!("plugins/"));
    }

    // Agent semantic rebuild layers bound to exactly these package hashes.
    let mut hashes: HashMap<String, String> = HashMap::new();
    for inspection in &inspections {
        if let Some(sha) = &inspection.sha256 {
            hashes.entry(file_name(&inspection.path)).or_insert_with(|| sha.clone());
        }
    }
    let mut semantic_layers = Vec::new();
    for layer in overlay::matching_layers(&app.app_id, app.version.as_deref(), &hashes) {
        let folder = overlay::safe_name(&layer.manifest.name);
        let target = staging.join("semantic").join(&folder);
        copy_directory(&layer.directory, &target)?;
        let mut value = json!({
            "name": layer.manifest.name,
            "created_at": layer.manifest.created_at,
            "files": layer.manifest.files.len(),
            "path": format!("semantic/{folder}/"),
        });
        if let Some(agent) = &layer.manifest.author.agent {
            value["agent"] = json!(agent);
        }
        if let Some(model) = &layer.manifest.author.model {
            value["model"] = json!(model);
        }
        log(&format!("Agent 重建层 {}：{} 个文件", layer.manifest.name, layer.manifest.files.len()));
        semantic_layers.push(value);
    }
    if !semantic_layers.is_empty() {
        layout.insert("semantic".into(), json!("semantic/"));
    }
    report["wxlocated"]["semantic_layers"] = json!(semantic_layers);
    report["wxlocated"]["layout"] = Value::Object(layout);
    report["wxlocated"]["redacted"] = json!(options.redact);

    std::fs::write(staging.join("material.json"), encode(&report, true))?;
    if options.redact {
        redact_paths(staging, report["wxlocated"]["app"]["account_directory"].as_str())?;
    }
    std::fs::write(staging.join("AGENT.md"), agent_readme(&report, options.include_raw, source_directory.is_some()))?;

    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    std::fs::rename(staging, destination)?;
    log(&format!("完成：{}", destination.to_string_lossy()));

    let mut result = json!({
        "schema_version": report::SCHEMA_VERSION,
        "root": destination.to_string_lossy(),
        "material": destination.join("material.json").to_string_lossy(),
        "readme": destination.join("AGENT.md").to_string_lossy(),
        "completeness": report["completeness"],
        "next_actions": report["next_actions"],
        "reused": false,
        "redacted": options.redact,
    });
    if let Some(directory) = &source_directory {
        result["source"] = json!(destination.join(file_name(directory)).to_string_lossy());
    }
    if options.include_raw {
        result["raw"] = json!(destination.join("raw").to_string_lossy());
    }
    Ok(result)
}

/// An existing export is reusable when every package (app and plugins) still
/// has the same SHA-256 and it was produced by the same schema and restorer
/// revision with the same raw/source/redact choices.
fn reusable_export(destination: &Path, app: &Miniapp, context: &Context, options: &Options) -> Option<Value> {
    let material_path = destination.join("material.json");
    let existing: Value = serde_json::from_str(&std::fs::read_to_string(&material_path).ok()?).ok()?;
    let extension = &existing["wxlocated"];
    if existing["schema_version"] != json!(report::SCHEMA_VERSION)
        || extension["reconstruction_revision"] != json!(report::RECONSTRUCTION_REVISION)
        || extension["redacted"].as_bool().unwrap_or(false) != options.redact
    {
        return None;
    }
    let completeness = existing["completeness"].as_str()?.to_string();
    let next_actions = existing["next_actions"].as_array()?.clone();
    let has_raw = destination.join("raw").exists();
    let source_directory = extension["reconstruction"]["source_directory"].as_str().map(str::to_string);
    if has_raw != options.include_raw || source_directory.is_some() != options.include_source {
        return None;
    }

    let inspections: Vec<Inspection> = report::inspect_app(app);
    let mut current: std::collections::BTreeSet<String> =
        inspections.iter().filter_map(|i| i.sha256.as_ref().map(|sha| format!("{}:{sha}", file_name(&i.path)))).collect();
    for plugin in extension["plugins"].as_array()? {
        let provider = plugin["provider"].as_str()?;
        if let Some(generation) = context.plugins.get(provider) {
            for path in &generation.packages {
                let sha = inspect_package(path, Some(provider)).sha256?;
                current.insert(format!("plugin:{provider}:{sha}"));
            }
        }
    }
    let mut recorded: std::collections::BTreeSet<String> = extension["packages"]
        .as_array()?
        .iter()
        .filter_map(|p| Some(format!("{}:{}", p["file"].as_str()?, p["sha256"].as_str()?)))
        .collect();
    for plugin in extension["plugins"].as_array()? {
        let provider = plugin["provider"].as_str()?;
        for package in plugin["packages"].as_array()? {
            recorded.insert(format!("plugin:{provider}:{}", package["sha256"].as_str()?));
        }
    }
    if current != recorded {
        return None;
    }

    let mut result = json!({
        "schema_version": report::SCHEMA_VERSION,
        "root": destination.to_string_lossy(),
        "material": material_path.to_string_lossy(),
        "readme": destination.join("AGENT.md").to_string_lossy(),
        "completeness": completeness,
        "next_actions": next_actions,
        "reused": true,
        "redacted": options.redact,
    });
    if let Some(directory) = source_directory {
        // `URL.appendingPathComponent("source/").path` has no trailing slash.
        result["source"] = json!(destination.join(directory.trim_end_matches('/')).to_string_lossy());
    }
    if has_raw {
        result["raw"] = json!(destination.join("raw").to_string_lossy());
    }
    Some(result)
}

/// Rewrites the JSON manifests so they carry no home directory or account id.
fn redact_paths(root: &Path, account_directory: Option<&str>) -> std::io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut replacements = vec![(home.clone(), "~".to_string()), (home.replace('/', "\\/"), "~".to_string())];
    if let Some(account) = account_directory.filter(|a| !a.is_empty()) {
        replacements.push((account.to_string(), "<account>".to_string()));
    }
    for path in walk_files(root) {
        let name = file_name(&path);
        if name != "material.json" && name != "WxLocated-decompilation.json" {
            continue;
        }
        let Ok(mut text) = std::fs::read_to_string(&path) else { continue };
        for (needle, replacement) in &replacements {
            if !needle.is_empty() {
                text = text.replace(needle, replacement);
            }
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                result.extend(walk_files(&path));
            } else {
                result.push(path);
            }
        }
    }
    result
}

fn copy_directory(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn safe_folder_name(value: &str) -> String {
    let cleaned: String = value.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if cleaned.is_empty() || cleaned.starts_with('.') { "plugin".into() } else { cleaned }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A short orientation file for the agent that picks up the material.
pub fn agent_readme(report: &Value, has_raw: bool, has_source: bool) -> String {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    let metadata = &report["metadata"];
    let extension = &report["wxlocated"];
    let title = metadata["app_name"].as_str().map(str::to_string).unwrap_or_else(|| text(&metadata["app_id"]));
    let count = |value: &Value| value.as_array().map_or(0, Vec::len);
    let mut lines = vec![
        format!("# {title} · 审计材料"),
        String::new(),
        format!("由 WxLocated {} 于 {} 从本机微信缓存导出。", text(&extension["tool_version"]), text(&extension["generated_at"])),
        String::new(),
        format!(
            "- AppID：`{}`，版本：{}",
            text(&metadata["app_id"]),
            extension["app"]["version"].as_str().unwrap_or("未知")
        ),
        format!(
            "- 完整性：**{}**（主包 {}，声明分包 {}，已缓存 {}，缺失 {}）",
            text(&report["completeness"]),
            text(&report["main_package"]["status"]),
            count(&report["subpackages"]["declared"]),
            count(&report["subpackages"]["found"]),
            count(&report["subpackages"]["missing"])
        ),
        String::new(),
        "## 目录".into(),
        String::new(),
        "- `material.json`：材料清单，顶层字段符合 miniapp-recon 的 material-check-output schema；`wxlocated` 字段含包哈希、缓存路径与重建置信度。".into(),
    ];
    if has_raw {
        lines.push("- `raw/`：原始 wxapkg，字节级副本，作为证据来源。".into());
    }
    if has_source {
        lines.push("- `source/`：静态重建的工程，仅用于阅读；`source/WxLocated-decompilation.json` 记录每个文件的来源区间与置信度。".into());
    }
    let found: Vec<String> = extension["plugins"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|p| p["found"] == json!(true))
        .map(|p| format!("{} · {}", text(&p["alias"]), text(&p["provider"])))
        .collect();
    if !found.is_empty() {
        lines.push(format!(
            "- `plugins/<别名>/`：本机缓存中的插件代码（{}），同样分为 raw/ 与 source/。插件是第三方代码，需单独盘点。",
            found.join("、")
        ));
    }
    for layer in extension["semantic_layers"].as_array().cloned().unwrap_or_default() {
        let who: Vec<String> = [&layer["agent"], &layer["model"]].iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        lines.push(format!(
            "- `{}`：Agent 语义化重建层「{}」（{}，{} 个文件）。这是推断结果，不是证据；`overlay.json` 记录了每个文件对应的静态重建文件。",
            text(&layer["path"]),
            text(&layer["name"]),
            who.join(" · "),
            layer["files"].as_i64().unwrap_or(0)
        ));
    }
    lines.push("- 用 `wxlocated trace <本目录> <source 内路径>:<行号>` 把重建代码映射回 raw/ 中的原始字节，作为证据引用；`semantic/<名称>/` 下的文件同样可以 trace。".into());
    lines.push(String::new());
    lines.push("## 使用前须知".into());
    lines.push(String::new());
    for note in report["coverage_notes"].as_array().cloned().unwrap_or_default() {
        lines.push(format!("- {}", text(&note)));
    }
    let degraded: Vec<String> =
        extension["reconstruction"]["degraded_files"].as_array().cloned().unwrap_or_default().iter().map(text).collect();
    if !degraded.is_empty() {
        let shown: Vec<String> = degraded.iter().take(20).map(|path| format!("`{path}`")).collect();
        lines.push(format!("- 以下重建文件为部分推导：{}{}", shown.join("、"), if degraded.len() > 20 { " 等" } else { "" }));
    }
    lines.push(String::new());
    lines.push("## 下一步".into());
    lines.push(String::new());
    for action in report["next_actions"].as_array().cloned().unwrap_or_default() {
        lines.push(format!("- {}", text(&action)));
    }
    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Generation;

    #[test]
    fn reuse_tracks_declared_plugins_and_rejects_obsolete_status_revision() {
        let root = std::env::temp_dir().join(format!("wxlocated-reuse-{}", uuid_string()));
        std::fs::create_dir_all(&root).unwrap();
        let main = root.join("__APP__.wxapkg");
        let plugin = root.join("plugin.wxapkg");
        std::fs::write(&main, b"main-package-fixture").unwrap();
        std::fs::write(&plugin, b"plugin-package-fixture").unwrap();
        let app = Miniapp {
            app_id: "wx-test".into(), name: None, main_package: main.clone(),
            sub_packages: Vec::new(), size: 0, modified: std::time::SystemTime::UNIX_EPOCH,
            version: None, icon: None, metadata_source: None, declared_subpackage_roots: Vec::new(),
        };
        let options = Options { include_source: false, include_raw: false, ..Options::default() };
        let mut manifest = json!({
            "schema_version": report::SCHEMA_VERSION,
            "completeness": "PARTIAL", "next_actions": [],
            "wxlocated": {
                "reconstruction_revision": report::RECONSTRUCTION_REVISION,
                "packages": [{ "file": "__APP__.wxapkg", "sha256": crate::sha256_hex(b"main-package-fixture") }],
                "plugins": [{ "alias": "payment", "provider": "wx-provider", "packages": [] }]
            }
        });
        let save = |manifest: &Value| std::fs::write(root.join("material.json"), serde_json::to_vec(manifest).unwrap()).unwrap();
        save(&manifest);
        let mut context = Context::default();
        assert!(reusable_export(&root, &app, &context, &options).is_some());
        let generation = Generation { version: "1".into(), directory: root.clone(), packages: vec![plugin] };
        context.plugins.insert("wx-unrelated".into(), generation.clone());
        assert!(reusable_export(&root, &app, &context, &options).is_some());
        context.plugins.insert("wx-provider".into(), generation);
        assert!(reusable_export(&root, &app, &context, &options).is_none());
        manifest["completeness"] = json!("COMPLETE");
        manifest["wxlocated"]["plugins"][0]["packages"] = json!([{ "sha256": crate::sha256_hex(b"plugin-package-fixture") }]);
        save(&manifest);
        assert!(reusable_export(&root, &app, &context, &options).is_some());
        context.plugins.remove("wx-provider");
        assert!(reusable_export(&root, &app, &context, &options).is_none());
        manifest["wxlocated"]["reconstruction_revision"] = json!(10);
        manifest["wxlocated"]["plugins"][0]["packages"] = json!([]);
        save(&manifest);
        assert!(reusable_export(&root, &app, &context, &options).is_none());
        let destination = root.join(folder_name(&app));
        std::fs::create_dir_all(&destination).unwrap();
        let marker = destination.join("keep.txt");
        std::fs::write(&marker, b"existing evidence").unwrap();
        let reuse_options = Options { reuse_if_unchanged: true, ..options };
        assert!(matches!(
            export(&app, &root, &reuse_options, &context, &|_| {}),
            Err(MaterialExportError::DestinationExists(_))
        ));
        assert_eq!(std::fs::read(&marker).unwrap(), b"existing evidence");
        std::fs::remove_dir_all(root).unwrap();
    }
}
