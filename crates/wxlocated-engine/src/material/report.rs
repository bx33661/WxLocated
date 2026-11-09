//! `material.json`: top-level fields follow MiniApp Audit Skills'
//! `material-check-output.schema.json`; WxLocated provenance lives under
//! `wxlocated`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};

use super::{Completeness, Context, DeclaredPlugins};
use crate::archive::{Archive, ArchiveError, PackageSet};
use crate::decompile::{Confidence, Decompilation};
use crate::json;
use crate::scan::Miniapp;
use crate::swift_json::iso8601;

/// Bump on breaking changes to material.json or CLI JSON output.
pub const SCHEMA_VERSION: i64 = 2;
/// Bump whenever restoration output or material status semantics change, so
/// `--reuse` does not return an obsolete reconstruction or completeness report.
pub const RECONSTRUCTION_REVISION: i64 = 11;
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

// MARK: Content analysis

/// Structure declared in the package's configuration files.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub configuration_files: Vec<String>,
    pub page_paths: Vec<String>,
    pub plugins: DeclaredPlugins,
}

const CONFIGURATION_NAMES: &[&str] = &[
    "app-config.json",
    "app.json",
    "app-info.json",
    "appinfo.json",
    "project.config.json",
    "project.private.config.json",
    "ext.json",
    "sitemap.json",
    "manifest.json",
];

/// Only configuration files are read: structure comes from declarations, not
/// from pattern matching over code.
pub fn analyze(set: &PackageSet) -> Analysis {
    const MAXIMUM_FILE_SIZE: u64 = 2 * 1024 * 1024;
    const MAXIMUM_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
    let mut configurations = Vec::new();
    let mut pages = BTreeSet::new();
    let mut plugins = DeclaredPlugins::new();
    let mut inspected = 0u64;
    for entry in &set.entries {
        let name = entry.path.rsplit('/').next().unwrap_or(&entry.path).to_lowercase();
        if !CONFIGURATION_NAMES.contains(&name.as_str()) || entry.size == 0 || entry.size > MAXIMUM_FILE_SIZE {
            continue;
        }
        if inspected + entry.size > MAXIMUM_TOTAL_BYTES {
            break;
        }
        let Ok(data) = set.read_limited(entry, MAXIMUM_FILE_SIZE) else { continue };
        let Ok(text) = String::from_utf8(data) else { continue };
        inspected += entry.size;
        if text.is_empty() {
            continue;
        }
        let Some(object) = json::parse(&text) else { continue };
        configurations.push(entry.path.clone());
        extract_structure(&object, &mut pages, &mut plugins, 0);
    }
    configurations.sort();
    Analysis { configuration_files: configurations, page_paths: pages.into_iter().collect(), plugins }
}

fn extract_structure(value: &json::Value, pages: &mut BTreeSet<String>, plugins: &mut DeclaredPlugins, depth: usize) {
    if depth >= 32 {
        return;
    }
    match value {
        json::Value::Object(entries) => {
            if let Some(declared) = value.get("pages").and_then(json::Value::as_string_array) {
                pages.extend(declared);
            }
            if let Some(json::Value::Object(page_entries)) = value.get("page") {
                pages.extend(page_entries.iter().map(|(key, _)| key.strip_suffix(".html").unwrap_or(key).to_string()));
            }
            if let Some(json::Value::Object(declared)) = value.get("plugins") {
                for (alias, value) in declared {
                    let provider = match value {
                        json::Value::Object(_) => value.get("provider").and_then(json::Value::as_str).unwrap_or(""),
                        _ => "",
                    };
                    plugins.insert(alias.clone(), provider.to_string());
                }
            }
            let subpackages = value.get("subPackages").or_else(|| value.get("subpackages"));
            if let Some(packages) = subpackages.and_then(json::Value::as_object_array) {
                for package in packages {
                    let package = json::Value::Object(package);
                    let root = package.get("root").and_then(json::Value::as_str).unwrap_or("");
                    if let Some(sub_pages) = package.get("pages").and_then(json::Value::as_string_array) {
                        pages.extend(sub_pages.iter().map(|page| if root.is_empty() { page.clone() } else { format!("{root}/{page}") }));
                    }
                }
            }
            for (_, child) in entries {
                extract_structure(child, pages, plugins, depth + 1);
            }
        }
        json::Value::Array(items) => items.iter().for_each(|item| extract_structure(item, pages, plugins, depth + 1)),
        _ => {}
    }
}

// MARK: Package integrity

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    Valid,
    Attention,
    Unrecognized,
    Damaged,
    Unreadable,
}

impl Integrity {
    pub fn machine_name(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Attention => "attention",
            Self::Unrecognized => "unrecognized",
            Self::Damaged => "damaged",
            Self::Unreadable => "unreadable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Inspection {
    pub path: PathBuf,
    pub file_size: i64,
    pub sha256: Option<String>,
    pub status: Integrity,
    pub entry_count: Option<i64>,
    pub notes: Vec<String>,
}

/// Read-only check: header, index and a streaming hash.
pub fn inspect_package(path: &Path, app_id: Option<&str>) -> Inspection {
    let unreadable = |file_size, note: &str| Inspection {
        path: path.to_path_buf(),
        file_size,
        sha256: None,
        status: Integrity::Unreadable,
        entry_count: None,
        notes: vec![note.to_string()],
    };
    let Ok(metadata) = std::fs::metadata(path) else { return unreadable(0, "文件已经不在缓存目录中") };
    let file_size = metadata.len() as i64;
    let Ok(bytes) = std::fs::read(path) else { return unreadable(file_size, "文件读取失败，请检查目录权限") };
    let sha256 = crate::sha256_hex(&bytes);
    let (status, entry_count, notes) = inspect_structure(&bytes, path, app_id);
    Inspection { path: path.to_path_buf(), file_size, sha256: Some(sha256), status, entry_count, notes }
}

fn inspect_structure(bytes: &[u8], path: &Path, app_id: Option<&str>) -> (Integrity, Option<i64>, Vec<String>) {
    let damaged = |count: Option<i64>, note: &str| (Integrity::Damaged, count, vec![note.to_string()]);
    let file_size = bytes.len() as u64;
    if file_size < 14 {
        return damaged(None, "文件长度小于 wxapkg 基础头部");
    }
    let header = &bytes[..14];
    if !(header[0] == 0xBE && header[13] == 0xED) {
        if header.starts_with(b"V1MMWX")
            && let Some(app_id) = app_id
            && let Ok(archive) = Archive::open_with_app_id(path, Some(app_id))
        {
            return (Integrity::Valid, Some(archive.entries.len() as i64), vec!["微信缓存加密层与解密后索引检查通过".into()]);
        }
        return (Integrity::Unrecognized, None, vec!["未识别标准包头，可能是加密包或格式变体".into()]);
    }
    let be32 = |data: &[u8], at: usize| data.get(at..at + 4).map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()));
    let index_size = be32(header, 5) as usize;
    if !(4..=8 * 1024 * 1024).contains(&index_size) || (14 + index_size) as u64 > file_size {
        return damaged(None, "索引长度超出文件边界");
    }
    let index = &bytes[14..14 + index_size];
    let file_count = be32(index, 0) as i64;
    if file_count <= 0 || file_count >= 100_000 {
        return damaged(Some(file_count), "索引文件数量异常");
    }
    let body_start = (14 + index_size) as u64;
    let mut cursor = 4usize;
    let mut names = BTreeSet::new();
    let (mut duplicates, mut out_of_bounds) = (0, 0);
    for _ in 0..file_count {
        if cursor + 4 > index.len() {
            return damaged(Some(file_count), "索引条目长度不完整");
        }
        let name_length = be32(index, cursor) as usize;
        cursor += 4;
        if name_length == 0 || name_length >= 16_384 || cursor + name_length + 8 > index.len() {
            return damaged(Some(file_count), "索引路径长度异常");
        }
        let Ok(name) = std::str::from_utf8(&index[cursor..cursor + name_length]) else {
            return damaged(Some(file_count), "索引中存在非 UTF-8 路径");
        };
        cursor += name_length;
        if !names.insert(name.replace('\\', "/").trim_matches('/').to_string()) {
            duplicates += 1;
        }
        let offset = u64::from(be32(index, cursor));
        cursor += 4;
        let size = u64::from(be32(index, cursor));
        cursor += 4;
        if offset < body_start || offset > file_size || size > file_size - offset.min(file_size) {
            out_of_bounds += 1;
        }
    }
    let mut notes = Vec::new();
    if duplicates > 0 {
        notes.push(format!("{duplicates} 个重复索引路径（读取时以最后一条为准）"));
    }
    if out_of_bounds > 0 {
        notes.push(format!("{out_of_bounds} 个条目超出文件边界"));
    }
    let declared_body = u64::from(be32(header, 9));
    let mismatch = declared_body != file_size - body_start;
    if mismatch {
        notes.push("头部声明大小与实际文件大小不一致".into());
    }
    if out_of_bounds > 0 {
        return (Integrity::Damaged, Some(file_count), notes);
    }
    // Repeated names still resolve (to their last entry); worth reporting,
    // not a reason to refuse the package.
    if duplicates > 0 || mismatch {
        return (Integrity::Attention, Some(file_count), notes);
    }
    (Integrity::Valid, Some(file_count), vec!["包头、索引与文件边界检查通过".into()])
}

// MARK: Report

pub struct ReportInput<'a> {
    pub app: &'a Miniapp,
    pub analysis: Option<&'a Analysis>,
    pub inspections: &'a [Inspection],
    pub reconstruction: Option<(&'a Decompilation, f64)>,
    pub archive_error: Option<String>,
    pub archive_notes: &'a [String],
    pub context: &'a Context,
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn issue(kind: &str, fields: &[(&str, Value)], impact: String) -> Value {
    let mut value = json!({ "type": kind, "impact": impact });
    for (key, field) in fields {
        value[*key] = field.clone();
    }
    value
}

pub fn package_entry(inspection: &Inspection, role: &str) -> Value {
    let mut value = json!({
        "file": file_name(&inspection.path),
        "role": role,
        "size_bytes": inspection.file_size,
        "integrity": inspection.status.machine_name(),
        "source_path": inspection.path.to_string_lossy(),
    });
    if let Some(sha) = &inspection.sha256 {
        value["sha256"] = json!(sha);
    }
    if let Some(count) = inspection.entry_count {
        value["entry_count"] = json!(count);
    }
    value
}

pub fn build(input: &ReportInput) -> Value {
    let app = input.app;
    let completeness = Completeness::new(app, input.analysis.map(|a| &a.plugins), input.context);
    let main = input.inspections.first();

    let mut main_issues = Vec::new();
    if let Some(error) = &input.archive_error {
        main_issues.push(issue("corrupted_file", &[("file", json!(file_name(&app.main_package)))], format!("主包无法解析：{error}")));
    } else if let Some(main) = main
        && main.status != Integrity::Valid
    {
        let impact = main.notes.first().cloned().unwrap_or_default();
        main_issues.push(issue("corrupted_file", &[("file", json!(file_name(&main.path)))], impact));
    }
    if let Some(analysis) = input.analysis {
        let names: BTreeSet<String> =
            analysis.configuration_files.iter().map(|p| p.rsplit('/').next().unwrap_or(p).to_lowercase()).collect();
        if !names.contains("app.json") && !names.contains("app-config.json") {
            main_issues.push(issue("missing_entry", &[("file", json!("app.json"))], "未找到应用配置，页面与分包声明可能不完整".into()));
        }
    }
    let subpackage_issues: Vec<Value> = completeness
        .missing_subpackages
        .iter()
        .map(|root| issue("missing_subpackage", &[("subpackage", json!(root))], "该分包未下载到本机缓存，其中的页面与逻辑不在本次材料中".into()))
        .collect();
    let plugin_issues: Vec<Value> = completeness
        .missing_plugins
        .iter()
        .map(|alias| {
            let provider = completeness.plugins.get(alias).cloned().unwrap_or_default();
            let mut fields = vec![("plugin", json!(alias))];
            if !provider.is_empty() {
                fields.push(("provider", json!(provider)));
            }
            issue("missing_plugin", &fields, "本机缓存中没有该插件的代码，本次材料不包含插件实现".into())
        })
        .collect();

    let blocked = input.archive_error.is_some()
        || main.is_some_and(|m| matches!(m.status, Integrity::Damaged | Integrity::Unreadable | Integrity::Unrecognized));
    let state = if blocked { "BLOCKED" } else if completeness.is_complete() { "COMPLETE" } else { "PARTIAL" };

    let mut quality_issues = Vec::new();
    let damaged: Vec<&Inspection> = input.inspections.iter().skip(1).filter(|i| i.status != Integrity::Valid).collect();
    if !damaged.is_empty() {
        let names: Vec<String> = damaged.iter().map(|i| file_name(&i.path)).collect();
        quality_issues.push(issue("corrupted_files", &[("count", json!(damaged.len()))], format!("部分分包结构异常：{}", names.join(", "))));
    }
    let duplicates: Vec<&String> = input.archive_notes.iter().filter(|note| note.contains("包内路径重复")).collect();
    if !duplicates.is_empty() {
        let mut impact = duplicates.iter().take(3).map(|s| s.as_str()).collect::<Vec<_>>().join("；");
        if duplicates.len() > 3 {
            impact.push_str(&format!("；另有 {} 处", duplicates.len() - 3));
        }
        quality_issues.push(issue("duplicate_entries", &[("count", json!(duplicates.len()))], impact));
    }
    for (root, version) in &completeness.missing_in_older_versions {
        quality_issues.push(issue(
            "potential_version_mix",
            &[("count", json!(1))],
            format!("缺失分包 {root} 只存在于旧版本目录 {version}，为保持版本一致未混入本次材料"),
        ));
    }
    if let Some((result, _)) = input.reconstruction
        && !result.diagnostics.is_empty()
    {
        quality_issues.push(issue(
            "unknown_sections",
            &[("count", json!(result.diagnostics.len()))],
            "重建过程产生诊断，详见 source/WxLocated-decompilation.json".into(),
        ));
    }
    let degraded: Vec<String> = input
        .reconstruction
        .map(|(result, _)| {
            let mut paths: Vec<String> =
                result.artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).map(|a| a.path.clone()).collect();
            paths.sort();
            paths
        })
        .unwrap_or_default();

    let mut next_actions = Vec::new();
    if blocked {
        next_actions.push("主包无法解析，需在微信中重新打开该小程序以刷新缓存，或提供设备侧导出的包".to_string());
    }
    if !completeness.missing_subpackages.is_empty() {
        next_actions.push(format!(
            "在微信中进入以下分包对应的页面以触发下载，然后重新运行 wxlocated export：{}",
            completeness.missing_subpackages.join(", ")
        ));
    }
    if !completeness.missing_plugins.is_empty() {
        let list: Vec<String> = completeness
            .missing_plugins
            .iter()
            .map(|alias| match completeness.plugins.get(alias) {
                Some(provider) if !provider.is_empty() => format!("{alias} ({provider})"),
                _ => alias.clone(),
            })
            .collect();
        next_actions.push(format!("在微信中触发以下插件加载后重新导出：{}", list.join(", ")));
    }
    if !degraded.is_empty() {
        next_actions.push(format!(
            "{} 个重建文件为部分推导（见 wxlocated.reconstruction.degraded_files），引用前请回到 raw/ 原始包核对",
            degraded.len()
        ));
    }
    if next_actions.is_empty() {
        next_actions.push("材料完整，可进入威胁建模与静态审计".into());
    }

    let mut notes = vec![
        "所有内容来自本机微信缓存，WxLocated 未执行包内代码，也未联网".to_string(),
        "source/ 是静态重建结果，仅用于提升可读性；作为证据引用时以 raw/ 原始包为准".to_string(),
    ];
    if completeness.declared_subpackages.is_empty() && !app.sub_packages.is_empty() {
        notes.push("未读到分包声明，无法判断是否缺少分包".into());
    }
    notes.push(format!("主包与分包均取自同一版本目录 {}，未混入其他版本", file_name(&app.cache_directory())));
    if !completeness.found_plugins.is_empty() {
        notes.push("插件代码按提供方 AppID 从本机缓存取得，版本为缓存中最新的一份，无法与 app.json 声明的插件版本号直接对应".into());
    }
    if !completeness.undeclared_subpackage_files.is_empty() {
        notes.push(format!("缓存中有未在 app.json 声明的分包文件：{}", completeness.undeclared_subpackage_files.join(", ")));
    }

    let packages: Vec<Value> = input
        .inspections
        .iter()
        .enumerate()
        .map(|(index, inspection)| package_entry(inspection, if index == 0 { "main" } else { "subpackage" }))
        .collect();

    let plugins: Vec<Value> = completeness
        .plugins
        .iter()
        .map(|(alias, provider)| {
            let generation = completeness.found_plugins.get(alias);
            let packages: Vec<Value> = generation
                .map(|g| {
                    g.packages.iter().map(|path| package_entry(&inspect_package(path, Some(provider)), "plugin")).collect()
                })
                .unwrap_or_default();
            let mut value = json!({ "alias": alias, "provider": provider, "found": generation.is_some(), "packages": packages });
            if let Some(generation) = generation {
                value["cache_version"] = json!(generation.version);
            }
            value
        })
        .collect();

    let older_versions: Vec<Value> = input
        .context
        .older_generations
        .iter()
        .map(|generation| {
            let provides: Vec<&String> =
                completeness.missing_in_older_versions.iter().filter(|(_, v)| **v == generation.version).map(|(k, _)| k).collect();
            json!({
                "version": generation.version,
                "directory": generation.directory.to_string_lossy(),
                "packages": generation.packages.iter().map(|p| file_name(p)).collect::<Vec<_>>(),
                "provides_missing_subpackages": provides,
            })
        })
        .collect();

    let mut app_info = json!({
        "modified_at": iso8601(app.modified),
        "cache_directory": app.cache_directory().to_string_lossy(),
    });
    if let Some(version) = &app.version {
        app_info["version"] = json!(version);
    }
    if let Some(account) = account_directory(&app.main_package) {
        app_info["account_directory"] = json!(account);
    }
    if let Some(source) = &app.metadata_source {
        app_info["name_source"] = json!(source);
    }

    let mut wxlocated = json!({
        "tool_version": TOOL_VERSION,
        "reconstruction_revision": RECONSTRUCTION_REVISION,
        "generated_at": iso8601(SystemTime::now()),
        "app": app_info,
        "packages": packages,
        "undeclared_subpackage_files": completeness.undeclared_subpackage_files,
        "redacted": false,
        "semantic_layers": [],
        "plugins": plugins,
        "older_versions": older_versions,
    });
    if let Some(version) = wechat_client_version() {
        wxlocated["wechat_client_version"] = json!(version);
    }
    if let Some((result, duration)) = input.reconstruction {
        let mut confidence: BTreeMap<&str, usize> = BTreeMap::new();
        for artifact in &result.artifacts {
            *confidence.entry(confidence_machine_name(artifact.confidence)).or_default() += 1;
        }
        let generated = result.artifacts.iter().filter(|a| a.content.generated_text().is_some()).count();
        wxlocated["reconstruction"] = json!({
            "generated_files": generated,
            "passthrough_files": result.artifacts.len() - generated,
            "confidence": confidence,
            "degraded_files": degraded,
            "diagnostics": result.diagnostics.len(),
            "duration_seconds": duration,
        });
    }

    let mut metadata = json!({
        "app_id": app.app_id,
        "pages_count": input.analysis.map_or(0, |a| a.page_paths.len()),
        "subpackages_count": completeness.declared_subpackages.len().max(app.sub_packages.len()),
        "plugins_count": completeness.plugins.len(),
    });
    if let Some(name) = &app.name {
        metadata["app_name"] = json!(name);
    }
    let decompile_quality = match input.reconstruction {
        Some(_) if !degraded.is_empty() => "ISSUES",
        _ => "OK",
    };

    json!({
        "skill": "miniapp-recon",
        "schema_version": SCHEMA_VERSION,
        "scanned_at": iso8601(SystemTime::now()),
        "input_path": app.cache_directory().to_string_lossy(),
        "material_type": "wechat_cache",
        "completeness": state,
        "metadata": metadata,
        "main_package": { "status": if main_issues.is_empty() { "OK" } else { "ISSUES" }, "issues": main_issues },
        "subpackages": {
            "declared": completeness.declared_subpackages,
            "found": completeness.found_subpackages,
            "missing": completeness.missing_subpackages,
            "issues": subpackage_issues,
        },
        "plugins": {
            "declared": completeness.plugins.keys().collect::<Vec<_>>(),
            "found": completeness.found_plugins.keys().collect::<Vec<_>>(),
            "issues": plugin_issues,
        },
        "quality": { "version_consistency": "OK", "decompile_quality": decompile_quality, "issues": quality_issues },
        "next_actions": next_actions,
        "coverage_notes": notes,
        "wxlocated": wxlocated,
    })
}

pub fn confidence_machine_name(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Exact => "exact",
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

/// The per-account directory segment of a cache path (`…/users/<hash>/…`).
pub fn account_directory(package: &Path) -> Option<String> {
    let components: Vec<String> = package.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let index = components.iter().position(|c| c == "users")?;
    components.get(index + 1).cloned()
}

/// The installed WeChat's version (macOS).
pub fn wechat_client_version() -> Option<String> {
    for path in ["/Applications/WeChat.app", "/Applications/微信.app"] {
        let plist = Path::new(path).join("Contents/Info.plist");
        if let Ok(plist::Value::Dictionary(info)) = plist::Value::from_file(&plist)
            && let Some(version) = info.get("CFBundleShortVersionString").and_then(plist::Value::as_string)
        {
            return Some(version.to_string());
        }
    }
    None
}

/// `info`: the report without rebuilding source.
pub fn inspect(app: &Miniapp, context: &Context) -> Value {
    let set = open_app(app);
    let analysis = set.as_ref().ok().map(analyze);
    let inspections = inspect_app(app);
    build(&ReportInput {
        app,
        analysis: analysis.as_ref(),
        inspections: &inspections,
        reconstruction: None,
        archive_error: set.is_err().then(|| "无法打开包".to_string()),
        archive_notes: set.as_ref().map(|s| s.notes.as_slice()).unwrap_or(&[]),
        context,
    })
}

pub fn open_app(app: &Miniapp) -> Result<PackageSet, ArchiveError> {
    PackageSet::open_app(&app.app_id, &app.all_packages(), app.declared_subpackage_roots.len())
}

pub fn inspect_app(app: &Miniapp) -> Vec<Inspection> {
    app.all_packages().iter().map(|path| inspect_package(path, Some(&app.app_id))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Generation;

    #[test]
    fn plugin_absence_restoration_and_blocked_precedence() {
        let app = Miniapp {
            app_id: "wx-test".into(), name: None, main_package: "cache/__APP__.wxapkg".into(),
            sub_packages: Vec::new(), size: 0, modified: SystemTime::UNIX_EPOCH,
            version: None, icon: None, metadata_source: None, declared_subpackage_roots: Vec::new(),
        };
        let analysis = Analysis {
            configuration_files: vec!["app.json".into()],
            plugins: [("payment".into(), "wx-provider".into())].into(),
            ..Analysis::default()
        };
        let mut inspections = vec![Inspection {
            path: app.main_package.clone(), file_size: 0, sha256: None,
            status: Integrity::Valid, entry_count: Some(1), notes: Vec::new(),
        }];
        let mut context = Context::default();
        let report = |context: &Context, inspections: &[Inspection], archive_error| build(&ReportInput {
            app: &app, analysis: Some(&analysis), inspections, reconstruction: None,
            archive_error, archive_notes: &[], context,
        });
        let missing = report(&context, &inspections, None);
        assert_eq!(missing["completeness"], "PARTIAL");
        assert_eq!(missing["plugins"]["issues"][0]["type"], "missing_plugin");
        context.plugins.insert("wx-provider".into(), Generation {
            version: "1".into(), directory: "plugin-cache".into(), packages: Vec::new(),
        });
        assert_eq!(report(&context, &inspections, None)["completeness"], "COMPLETE");
        context.plugins.clear();
        for status in [Integrity::Damaged, Integrity::Unreadable, Integrity::Unrecognized] {
            inspections[0].status = status;
            assert_eq!(report(&context, &inspections, None)["completeness"], "BLOCKED");
        }
        inspections[0].status = Integrity::Valid;
        assert_eq!(report(&context, &inspections, Some("broken index".into()))["completeness"], "BLOCKED");
    }
}
