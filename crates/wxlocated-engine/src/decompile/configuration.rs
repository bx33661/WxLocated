//! `app.json`, page/component `.json` and `ext.json` from `app-config.json`
//! and the JSON registered in compiled scripts.

use std::collections::{BTreeSet, HashMap};

use super::{ArtifactKind, Candidate, Confidence, Diagnostic, Severity, SourceRef, Stage, normalized_path};
use crate::json::{self, Value, pretty};

pub struct ConfigurationRestoration {
    pub files: Vec<Candidate>,
    pub diagnostics: Vec<Diagnostic>,
}

const COPIED_KEYS: &[&str] = &[
    "tabBar",
    "networkTimeout",
    "navigateToMiniProgramAppIdList",
    "functionalPages",
    "plugins",
    "preloadRule",
    "resizable",
    "useExtendedLib",
    "lazyCodeLoading",
    "renderer",
    "componentFramework",
    "darkmode",
    "themeLocation",
    "permission",
    "requiredPrivateInfos",
    "sitemapLocation",
    "debug",
];

pub fn restore(
    source: &str,
    source_reference: &SourceRef,
    embedded_page_configurations: &HashMap<String, Value>,
    resource_hashes: &HashMap<String, String>,
    worker_root: Option<&str>,
) -> ConfigurationRestoration {
    let Some(Value::Object(root_entries)) = json::parse(source) else {
        return ConfigurationRestoration {
            files: Vec::new(),
            diagnostics: vec![Diagnostic::new(
                Severity::Error,
                Stage::Configuration,
                "app-config.json 不是有效 JSON",
                Some(source_reference.clone()),
            )],
        };
    };
    let root = Value::Object(root_entries.clone());
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    let mut app: Vec<(String, Value)> = Vec::new();

    let mut pages = normalize_page_list(&root.get("pages").and_then(Value::as_string_array).unwrap_or_default());
    if let Some(entry_page) = root.get("entryPagePath").and_then(Value::as_str).and_then(normalize_page_path) {
        pages.retain(|page| *page != entry_page);
        pages.insert(0, entry_page);
    }

    let subpackages = normalize_subpackages(root.get("subPackages").or_else(|| root.get("subpackages")));
    let subpackage_pages: std::collections::HashSet<String> = subpackages
        .iter()
        .flat_map(|package| {
            let package = Value::Object(package.clone());
            match (package.get("root").and_then(Value::as_str), package.get("pages").and_then(Value::as_string_array)) {
                (Some(root), Some(pages)) => pages.iter().map(|page| format!("{root}{page}")).collect(),
                _ => Vec::new(),
            }
        })
        .collect();
    pages.retain(|page| !subpackage_pages.contains(page));

    json::set(&mut app, "pages", Value::Array(pages.into_iter().map(Value::String).collect()));
    if let Some(window) = root.get("global").filter(|global| matches!(global, Value::Object(_))).and_then(|global| global.get("window")) {
        json::set(&mut app, "window", window.clone());
    } else if let Some(window) = root.get("window") {
        json::set(&mut app, "window", window.clone());
    }
    if !subpackages.is_empty() {
        json::set(&mut app, "subPackages", Value::Array(subpackages.into_iter().map(Value::Object).collect()));
    }
    for key in COPIED_KEYS {
        if let Some(value) = root.get(key) {
            json::set(&mut app, key, value.clone());
        }
    }
    if let Some(worker_root) = worker_root.filter(|root| !root.is_empty()) {
        json::set(&mut app, "workers", Value::String(worker_root.to_string()));
    }
    let app = replacing_icon_data(&Value::Object(app), resource_hashes);

    if let Some(text) = pretty(&app) {
        files.push(
            Candidate::new("app.json", Some(ArtifactKind::Configuration), text, Confidence::High)
                .notes(vec!["由 app-config.json 重建".into()]),
        );
    }

    // Two sources describe a page's JSON: the compiled registration in the
    // scripts (`__wxAppCode__["x.json"] = {...}`, the whole file) and
    // app-config.json's `page["x.html"]` (window settings only). Merge them
    // per output path; the compiled file is the base.
    let mut compiled: HashMap<String, Value> = HashMap::new();
    let mut windows: HashMap<String, Value> = HashMap::new();
    let mut invalid: Vec<String> = Vec::new();
    for (raw_path, value) in embedded_page_configurations {
        let Some(path) = configuration_path(raw_path) else {
            invalid.push(raw_path.clone());
            continue;
        };
        if path == "app.json" {
            continue;
        }
        compiled.entry(path).or_insert_with(|| value.clone());
    }
    if let Some(Value::Object(page_entries)) = root.get("page") {
        for (raw_path, value) in page_entries {
            let Some(path) = configuration_path(raw_path) else {
                invalid.push(raw_path.clone());
                continue;
            };
            let window = match value {
                Value::Object(_) => value.get("window").cloned().unwrap_or_else(|| value.clone()),
                _ => value.clone(),
            };
            windows.insert(path, window);
        }
    }
    invalid.sort();
    for raw_path in invalid {
        diagnostics.push(Diagnostic::new(
            Severity::Warning,
            Stage::Configuration,
            format!("页面配置路径异常：{raw_path}"),
            Some(source_reference.clone()),
        ));
    }

    let paths: BTreeSet<&String> = compiled.keys().chain(windows.keys()).collect();
    for path in paths {
        let (configuration, notes) = if let Some(base) = compiled.get(path) {
            let mut notes = vec!["由脚本中登记的页面 JSON 恢复".to_string()];
            let mut configuration = base.clone();
            if let (Value::Object(entries), Some(Value::Object(window))) = (&mut configuration, windows.get(path)) {
                let missing: Vec<(String, Value)> =
                    window.iter().filter(|(key, _)| !entries.iter().any(|(existing, _)| existing == key)).cloned().collect();
                if !missing.is_empty() {
                    let mut names: Vec<&str> = missing.iter().map(|(key, _)| key.as_str()).collect();
                    names.sort();
                    notes.push(format!("补入 app-config.json 页面窗口配置 {} 项：{}", missing.len(), names.join("、")));
                }
                entries.extend(missing);
            }
            (configuration, notes)
        } else if let Some(window) = windows.get(path) {
            (window.clone(), vec!["由 app-config.json 页面配置重建（只含窗口配置）".to_string()])
        } else {
            continue;
        };
        let Some(text) = pretty(&configuration) else {
            diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Configuration,
                format!("页面配置内容异常：{path}"),
                Some(source_reference.clone()),
            ));
            continue;
        };
        files.push(Candidate::new(path.clone(), Some(ArtifactKind::Configuration), text, Confidence::High).notes(notes));
    }

    if let Some(ext_app_id) = root.get("extAppid").and_then(Value::as_str) {
        let mut ext = vec![("extEnable".to_string(), Value::Bool(true)), ("extAppid".to_string(), Value::String(ext_app_id.to_string()))];
        if let Some(value) = root.get("ext") {
            ext.push(("ext".to_string(), value.clone()));
        }
        if let Some(text) = pretty(&Value::Object(ext)) {
            files.push(
                Candidate::new("ext.json", Some(ArtifactKind::Configuration), text, Confidence::High)
                    .notes(vec!["由 app-config.json ext 字段重建".into()]),
            );
        }
    }

    let known: Vec<&str> = COPIED_KEYS
        .iter()
        .copied()
        .chain(["pages", "entryPagePath", "global", "window", "subPackages", "subpackages", "page", "extAppid", "ext"])
        .collect();
    let mut unknown: Vec<&str> = root_entries.iter().map(|(key, _)| key.as_str()).filter(|key| !known.contains(key)).collect();
    unknown.sort();
    if !unknown.is_empty() {
        diagnostics.push(Diagnostic::new(
            Severity::Information,
            Stage::Configuration,
            format!("保留编译配置中的扩展字段：{}", unknown.join(", ")),
            Some(source_reference.clone()),
        ));
    }
    ConfigurationRestoration { files, diagnostics }
}

fn normalize_page_list(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values.iter().filter_map(|value| normalize_page_path(value)).filter(|page| seen.insert(page.clone())).collect()
}

fn normalize_page_path(value: &str) -> Option<String> {
    let value = value.strip_suffix(".html").unwrap_or(value);
    let value = value.strip_suffix(".wxml").unwrap_or(value);
    normalized_path(value)
}

fn normalize_subpackages(value: Option<&Value>) -> Vec<Vec<(String, Value)>> {
    let Some(packages) = value.and_then(Value::as_object_array) else { return Vec::new() };
    packages
        .into_iter()
        .filter_map(|package| {
            let mut root = Value::Object(package.clone()).get("root")?.as_str()?.replace('\\', "/");
            root = root.trim_start_matches('/').to_string();
            if !root.ends_with('/') {
                root.push('/');
            }
            normalized_path(&root)?;
            let mut output = package.clone();
            json::set(&mut output, "root", Value::String(root.clone()));
            if let Some(pages) = Value::Object(package).get("pages").and_then(Value::as_string_array) {
                let pages = pages
                    .iter()
                    .filter_map(|page| {
                        let value = page.replace('\\', "/");
                        let value = value.strip_prefix(root.as_str()).unwrap_or(&value);
                        normalize_page_path(value)
                    })
                    .map(Value::String)
                    .collect();
                json::set(&mut output, "pages", Value::Array(pages));
            }
            Some(output)
        })
        .collect()
}

fn configuration_path(raw_path: &str) -> Option<String> {
    let mut path = raw_path;
    for suffix in [".html", ".wxml", ".js", ".json"] {
        if let Some(stripped) = path.strip_suffix(suffix) {
            path = stripped;
            break;
        }
    }
    normalized_path(path).map(|path| path + ".json")
}

/// Tab bar icons inlined as base64 point back at the package image with the
/// same bytes.
fn replacing_icon_data(value: &Value, resource_hashes: &HashMap<String, String>) -> Value {
    match value {
        Value::Object(entries) => {
            let mut entries = entries.clone();
            for (data_key, path_key) in [("iconData", "iconPath"), ("selectedIconData", "selectedIconPath")] {
                let found = entries
                    .iter()
                    .find(|(key, _)| key == data_key)
                    .and_then(|(_, value)| value.as_str())
                    .and_then(decode_base64)
                    .and_then(|data| resource_hashes.get(&crate::sha256_hex(&data)).cloned());
                if let Some(path) = found {
                    json::remove(&mut entries, data_key);
                    json::set(&mut entries, path_key, Value::String(path));
                }
            }
            Value::Object(entries.into_iter().map(|(key, child)| (key, replacing_icon_data(&child, resource_hashes))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| replacing_icon_data(item, resource_hashes)).collect()),
        _ => value.clone(),
    }
}

/// `Data(base64Encoded:)`: standard alphabet, no whitespace, at least the
/// required `=` padding (extra `=` is accepted).
pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let data = text.trim_end_matches('=');
    let padding = text.len() - data.len();
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let remainder = data.len() % 4;
    if remainder == 1 {
        return None;
    }
    let required = (4 - remainder) % 4;
    if padding < required {
        return None;
    }
    let mut output = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.as_bytes().chunks(4) {
        let mut accumulator = 0u32;
        for &byte in chunk {
            accumulator = (accumulator << 6) | value(byte)?;
        }
        match chunk.len() {
            4 => output.extend_from_slice(&[(accumulator >> 16) as u8, (accumulator >> 8) as u8, accumulator as u8]),
            3 => output.extend_from_slice(&[(accumulator >> 10) as u8, (accumulator >> 2) as u8]),
            2 => output.push((accumulator >> 4) as u8),
            _ => return None,
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_foundation() {
        let cases: &[(&str, Option<&[u8]>)] = &[
            ("aGk=", Some(b"hi")),
            ("aGk", None),
            ("aG k=", None),
            ("aGk=\n", None),
            ("aGk==", Some(b"hi")),
            ("", Some(b"")),
            ("aGk===", Some(b"hi")),
            ("aGk=a", None),
            ("a", None),
            ("aG==", Some(b"h")),
            ("aG=", None),
            ("aGk=aGk=", None),
            ("YQ", None),
            ("YQ===", Some(b"a")),
        ];
        for (input, expected) in cases {
            assert_eq!(decode_base64(input).as_deref(), *expected, "{input:?}");
        }
    }

    #[test]
    fn rebuilds_app_json() {
        let source = r#"{"pages":["pages/a.html","pages/b"],"entryPagePath":"pages/b.html","global":{"window":{"t":1}},"subPackages":[{"root":"sub","pages":["sub/x"]}],"extra":1}"#;
        let reference = SourceRef { package: "p".into(), entry_path: "app-config.json".into(), start: None, end: None };
        let restoration = restore(source, &reference, &HashMap::new(), &HashMap::new(), None);
        assert_eq!(
            restoration.files[0].content,
            "{\n  \"pages\" : [\n    \"pages/b\",\n    \"pages/a\"\n  ],\n  \"subPackages\" : [\n    {\n      \"pages\" : [\n        \"x\"\n      ],\n      \"root\" : \"sub/\"\n    }\n  ],\n  \"window\" : {\n    \"t\" : 1\n  }\n}\n"
        );
        assert_eq!(restoration.diagnostics[0].message, "保留编译配置中的扩展字段：extra");
    }
}
