//! Writes a decompilation to disk: every artifact under one folder plus
//! `WxLocated-decompilation.json`, the per-file manifest (sources, confidence,
//! SHA-256). Written to a staging folder and moved into place at the end.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::archive::PackageSet;
use crate::decompile::{Confidence, Content, Decompilation, MAXIMUM_GENERATED_FILE_COUNT, SourceRef, normalized_path};
use crate::swift_json::{encode, iso8601};

const MAXIMUM_TOTAL_SIZE: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("导出目录不可用")]
    DestinationUnavailable,
    #[error("重建文件数量 {0} 超过上限")]
    TooManyFiles(usize),
    #[error("导出内容大小 {0} 字节超过上限")]
    SizeLimitExceeded(u64),
    #[error("导出路径冲突：{0}")]
    PathConflict(String),
    #[error("写入失败：{0}")]
    WriteFailed(String),
    #[error("包文件在导出过程中发生变化")]
    SourceChanged,
}

pub fn confidence_title(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Exact => "原样",
        Confidence::High => "高置信",
        Confidence::Medium => "中置信",
        Confidence::Low => "低置信",
    }
}

fn source_json(source: &SourceRef) -> Value {
    let mut value = json!({ "package": source.package, "entryPath": source.entry_path });
    if let Some(start) = source.start {
        value["startOffset"] = json!(start);
    }
    if let Some(end) = source.end {
        value["endOffset"] = json!(end);
    }
    value
}

/// A random RFC 4122 version 4 UUID in Foundation's uppercase form.
pub fn uuid_string() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase()
}

/// Exports `result` to `<parent>/<folder_name>` (or `<folder_name> 2`, … when
/// taken) and returns that folder.
pub fn export_source(
    result: &Decompilation,
    duration_seconds: f64,
    set: &PackageSet,
    parent: &Path,
    folder_name: &str,
) -> Result<PathBuf, ExportError> {
    if !parent.is_dir() {
        return Err(ExportError::DestinationUnavailable);
    }
    if result.artifacts.len() > MAXIMUM_GENERATED_FILE_COUNT {
        return Err(ExportError::TooManyFiles(result.artifacts.len()));
    }
    let estimated: u64 = result
        .artifacts
        .iter()
        .map(|artifact| match &artifact.content {
            Content::Generated(text) => text.len() as u64,
            Content::Entry(index) => set.entries[*index].size,
        })
        .fold(0u64, u64::saturating_add);
    if estimated > MAXIMUM_TOTAL_SIZE {
        return Err(ExportError::SizeLimitExceeded(estimated));
    }
    validate_paths(result.artifacts.iter().map(|artifact| artifact.path.as_str()))?;

    let final_path = unique_destination(parent, &sanitized_folder_name(folder_name));
    let staging = parent.join(format!(".WxLocated-Decompile-{}.partial", uuid_string()));
    std::fs::create_dir(&staging).map_err(|_| ExportError::DestinationUnavailable)?;
    let outcome = write_contents(result, duration_seconds, set, &staging);
    if let Err(error) = outcome {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    std::fs::rename(&staging, &final_path).map_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
        ExportError::WriteFailed(final_path.to_string_lossy().into_owned())
    })?;
    Ok(final_path)
}

fn write_contents(result: &Decompilation, duration_seconds: f64, set: &PackageSet, staging: &Path) -> Result<(), ExportError> {
    let mut artifacts = Vec::with_capacity(result.artifacts.len());
    for artifact in &result.artifacts {
        let output = artifact.path.split('/').fold(staging.to_path_buf(), |path, component| path.join(component));
        if let Some(directory) = output.parent() {
            std::fs::create_dir_all(directory).map_err(|_| ExportError::WriteFailed(artifact.path.clone()))?;
        }
        let (bytes, size) = match &artifact.content {
            Content::Generated(text) => (text.as_bytes().to_vec(), text.len() as u64),
            Content::Entry(index) => {
                let entry = &set.entries[*index];
                (set.read(entry).map_err(|_| ExportError::SourceChanged)?, entry.size)
            }
        };
        std::fs::write(&output, &bytes).map_err(|_| ExportError::WriteFailed(artifact.path.clone()))?;
        artifacts.push(json!({
            "path": artifact.path,
            "kind": artifact.kind.as_str(),
            "confidence": confidence_title(artifact.confidence),
            "size": size,
            "generated": artifact.content.generated_text().is_some(),
            "sources": artifact.sources.iter().map(source_json).collect::<Vec<_>>(),
            "notes": artifact.notes,
            "sha256": crate::sha256_hex(&bytes),
        }));
    }

    let mut input_packages = Vec::new();
    for source in &set.sources {
        let path = &source.archive.path;
        let data = std::fs::read(path).map_err(|_| ExportError::SourceChanged)?;
        input_packages.push(json!({
            "path": path.to_string_lossy(),
            "format": source.archive.format.as_str(),
            "size": data.len(),
            "sha256": crate::sha256_hex(&data),
        }));
    }
    let count = |confidence: Confidence| result.artifacts.iter().filter(|a| a.confidence == confidence).count();
    let generated = result.artifacts.iter().filter(|a| a.content.generated_text().is_some()).count();
    let diagnostics: Vec<Value> = result
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let mut value = json!({
                "id": uuid_string(),
                "severity": diagnostic.severity as u8,
                "stage": diagnostic.stage.as_str(),
                "message": diagnostic.message,
            });
            if let Some(source) = &diagnostic.source {
                value["source"] = source_json(source);
            }
            value
        })
        .collect();
    let manifest = json!({
        // 3: no heuristic quality score; confidence per file is the signal.
        "schemaVersion": 3,
        "generatedAt": iso8601(SystemTime::now()),
        "durationMilliseconds": (duration_seconds * 1000.0) as i64,
        "inputPackages": input_packages,
        "coverage": {
            "inputFiles": set.entries.len(),
            "generatedFiles": generated,
            "passthroughFiles": result.artifacts.len() - generated,
            "exactFiles": count(Confidence::Exact),
            "highConfidenceFiles": count(Confidence::High),
            "degradedFiles": result.artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).count(),
        },
        "artifacts": artifacts,
        "diagnostics": diagnostics,
    });
    std::fs::write(staging.join("WxLocated-decompilation.json"), encode(&manifest, true))
        .map_err(|_| ExportError::WriteFailed("WxLocated-decompilation.json".into()))
}

/// Paths must be normalized, unique once case and accents are folded, and no
/// path may be another's folder.
fn validate_paths<'a>(paths: impl Iterator<Item = &'a str> + Clone) -> Result<(), ExportError> {
    let mut known: HashSet<String> = HashSet::from([folded("WxLocated-decompilation.json")]);
    for path in paths.clone() {
        if normalized_path(path).as_deref() != Some(path) || !known.insert(folded(path)) {
            return Err(ExportError::PathConflict(path.to_string()));
        }
    }
    for path in paths {
        let components: Vec<String> = folded(path).split('/').map(str::to_string).collect();
        for end in 1..components.len() {
            if known.contains(&components[..end].join("/")) {
                return Err(ExportError::PathConflict(path.to_string()));
            }
        }
    }
    Ok(())
}

/// Case-insensitive, accent-insensitive key (the default macOS volume).
fn folded(path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    path.nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)).collect::<String>().to_lowercase()
}

fn sanitized_folder_name(value: &str) -> String {
    let pieces: Vec<&str> =
        value.split(|c: char| c == '/' || c == ':' || c == '\0' || c.is_control() || matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}')).filter(|p| !p.is_empty()).collect();
    let joined = pieces.join("-");
    let name = crate::text::trim_whitespace_and_newlines(&joined);
    let name = if name.is_empty() { "wxapkg-source" } else { name };
    name.chars().take(100).collect()
}

fn unique_destination(parent: &Path, preferred: &str) -> PathBuf {
    let mut destination = parent.join(preferred);
    let mut suffix = 2;
    while destination.exists() {
        destination = parent.join(format!("{preferred} {suffix}"));
        suffix += 1;
    }
    destination
}
