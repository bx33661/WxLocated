//! Agent-made semantic rebuilds ("overlays") layered on top of the static
//! rebuild. An overlay is bound to one exact cache state (AppID, version,
//! package SHA-256s); only files that differ from the static rebuild are
//! stored, under `<config>/WxLocated/overlays/<appid>/<version>/<name>/`.
//! Overlays are reading aids, never evidence.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: i64 = 1;
pub const MANIFEST_NAME: &str = "overlay.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasePackage {
    pub file: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Base {
    pub app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub packages: Vec<BasePackage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Author {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayFile {
    /// Path inside the overlay directory.
    pub path: String,
    /// The static rebuild file (relative to source/) this file replaces;
    /// `None` for new files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// SHA-256 of that static rebuild file when the overlay was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "default_schema")]
    pub schema_version: i64,
    pub name: String,
    pub base: Base,
    pub author: Author,
    /// ISO 8601, as written.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub files: Vec<OverlayFile>,
}

fn default_schema() -> i64 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub manifest: Manifest,
    pub directory: PathBuf,
}

/// `~/Library/Application Support/WxLocated/overlays` on macOS (where the
/// Swift app keeps them), the config folder's `overlays` elsewhere.
pub fn root() -> PathBuf {
    if let Some(root) = std::env::var_os("WXLOCATED_OVERLAY_ROOT") {
        return PathBuf::from(root);
    }
    crate::settings::settings_path()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("overlays")
}

pub fn directory(app_id: &str, version: Option<&str>, name: &str) -> PathBuf {
    root().join(app_id).join(version.unwrap_or("unversioned")).join(safe_name(name))
}

pub fn read_manifest(directory: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(directory.join(MANIFEST_NAME)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Layers stored for this app version, by folder name.
pub fn layers(app_id: &str, version: Option<&str>) -> Vec<Layer> {
    let version_directory = root().join(app_id).join(version.unwrap_or("unversioned"));
    let Ok(entries) = std::fs::read_dir(&version_directory) else { return Vec::new() };
    let mut names: Vec<String> = entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let directory = version_directory.join(&name);
            read_manifest(&directory).map(|manifest| Layer { manifest, directory })
        })
        .collect()
}

/// Layers whose base packages still match the given hashes (file → SHA-256).
pub fn matching_layers(app_id: &str, version: Option<&str>, hashes: &std::collections::HashMap<String, String>) -> Vec<Layer> {
    layers(app_id, version)
        .into_iter()
        .filter(|layer| layer.manifest.base.packages.iter().all(|package| hashes.get(&package.file) == Some(&package.sha256)))
        .collect()
}

/// Letters, digits, `-`, `_` and `.`; anything else becomes `_`.
pub fn safe_name(value: &str) -> String {
    let cleaned: String = value.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if cleaned.is_empty() || cleaned.starts_with('.') { "agent".into() } else { cleaned }
}

// MARK: Writing

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("{0} 中没有 overlay.json")]
    MissingManifest(String),
    #[error("重建层中的路径不合法：{0}")]
    InvalidPath(String),
    #[error("overlay.json 登记了 {0}，但目录中没有这个文件")]
    MissingFile(String),
    #[error("本机缓存中没有 {0}")]
    UnknownApp(String),
    #[error("重建层的基线与当前缓存不一致：{0}")]
    BaseMismatch(String),
    #[error("{0} 不是 wxlocated export 导出的材料目录")]
    NotMaterial(String),
    #[error("{0}")]
    Io(String),
}

impl OverlayError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingManifest(_) => "missing_overlay_manifest",
            Self::InvalidPath(_) => "invalid_overlay_path",
            Self::MissingFile(_) => "missing_overlay_file",
            Self::UnknownApp(_) => "not_found",
            Self::BaseMismatch(_) => "base_mismatch",
            Self::NotMaterial(_) => "not_material",
            Self::Io(_) => "failed",
        }
    }

    /// 66 not found, 64 usage, 70 failure.
    pub fn exit(&self) -> u8 {
        match self {
            Self::UnknownApp(_) | Self::MissingManifest(_) | Self::NotMaterial(_) | Self::MissingFile(_) => 66,
            Self::InvalidPath(_) => 64,
            Self::BaseMismatch(_) | Self::Io(_) => 70,
        }
    }
}

impl From<std::io::Error> for OverlayError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

pub fn write_manifest(directory: &Path, manifest: &Manifest) -> Result<(), OverlayError> {
    let value = serde_json::to_value(manifest).map_err(|e| OverlayError::Io(e.to_string()))?;
    std::fs::write(directory.join(MANIFEST_NAME), crate::swift_json::encode(&value, true))?;
    Ok(())
}

pub fn read_manifest_strict(directory: &Path) -> Result<Manifest, OverlayError> {
    let text = std::fs::read_to_string(directory.join(MANIFEST_NAME))
        .map_err(|_| OverlayError::MissingManifest(directory.to_string_lossy().into_owned()))?;
    serde_json::from_str(&text).map_err(|e| OverlayError::Io(e.to_string()))
}

fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

/// A working directory from an exported material: a copy of source/ plus an
/// overlay.json whose base and per-file hashes are already filled in.
pub fn initialize(material_root: &Path, work: &Path, name: &str, author: Author) -> Result<Manifest, OverlayError> {
    let not_material = || OverlayError::NotMaterial(material_root.to_string_lossy().into_owned());
    let material: serde_json::Value = std::fs::read_to_string(material_root.join("material.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(|m: &serde_json::Value| m["metadata"]["app_id"].is_string() && m["wxlocated"]["packages"].is_array())
        .ok_or_else(not_material)?;
    let source_root = material_root.join("source");
    let source_manifest: serde_json::Value = std::fs::read_to_string(source_root.join("WxLocated-decompilation.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(|m: &serde_json::Value| m["artifacts"].is_array())
        .ok_or_else(not_material)?;
    if work.exists() {
        return Err(OverlayError::InvalidPath(format!("{} 已存在", work.to_string_lossy())));
    }
    if let Some(parent) = work.parent() {
        std::fs::create_dir_all(parent)?;
    }
    copy_tree(&source_root, work)?;
    let _ = std::fs::remove_file(work.join("WxLocated-decompilation.json"));

    let files = source_manifest["artifacts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|artifact| {
            let path = artifact["path"].as_str()?;
            (!path.starts_with("__wxlocated_")).then(|| OverlayFile {
                path: path.to_string(),
                base: Some(path.to_string()),
                base_sha256: artifact["sha256"].as_str().map(str::to_string),
                note: None,
            })
        })
        .collect();
    let packages = material["wxlocated"]["packages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|p| p["role"].as_str() != Some("plugin"))
        .filter_map(|p| Some(BasePackage { file: p["file"].as_str()?.to_string(), sha256: p["sha256"].as_str()?.to_string() }))
        .collect();
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        name: name.to_string(),
        base: Base {
            app_id: material["metadata"]["app_id"].as_str().unwrap_or_default().to_string(),
            version: material["wxlocated"]["app"]["version"].as_str().map(str::to_string),
            packages,
        },
        author,
        created_at: crate::swift_json::iso8601(std::time::SystemTime::now()),
        description: None,
        files,
    };
    write_manifest(work, &manifest)?;
    Ok(manifest)
}

pub struct ImportResult {
    pub app_id: String,
    pub version: Option<String>,
    pub name: String,
    pub stored_at: PathBuf,
    pub changed_files: Vec<String>,
    pub added_files: Vec<String>,
    pub unchanged_files_dropped: usize,
}

/// Validates an overlay against the current cache (`hashes`: package file →
/// SHA-256) and stores the files that differ from the static rebuild. An
/// existing layer with the same name is replaced.
pub fn import(
    work: &Path,
    app_id: &str,
    version: Option<&str>,
    hashes: &std::collections::HashMap<String, String>,
    name_override: Option<&str>,
) -> Result<ImportResult, OverlayError> {
    let mut manifest = read_manifest_strict(work)?;
    if let Some(name) = name_override {
        manifest.name = name.to_string();
    }
    if manifest.base.app_id != app_id {
        return Err(OverlayError::BaseMismatch(format!("重建层属于 {}，不是 {app_id}", manifest.base.app_id)));
    }
    if let Some(base_version) = &manifest.base.version
        && Some(base_version.as_str()) != version
    {
        return Err(OverlayError::BaseMismatch(format!(
            "重建层基于版本 {base_version}，缓存中现在是 {}；请重新 export 并 overlay init",
            version.unwrap_or("未知")
        )));
    }
    for package in &manifest.base.packages {
        if hashes.get(&package.file) != Some(&package.sha256) {
            return Err(OverlayError::BaseMismatch(format!("{} 的 SHA-256 已变化", package.file)));
        }
    }

    let (mut kept, mut changed, mut added, mut dropped) = (Vec::new(), Vec::new(), Vec::new(), 0);
    for file in &manifest.files {
        if crate::decompile::normalized_path(&file.path).as_deref() != Some(file.path.as_str()) || file.path == MANIFEST_NAME {
            return Err(OverlayError::InvalidPath(file.path.clone()));
        }
        let data = std::fs::read(work.join(&file.path)).map_err(|_| OverlayError::MissingFile(file.path.clone()))?;
        if file.base_sha256.as_deref() == Some(crate::sha256_hex(&data).as_str()) {
            dropped += 1;
            continue;
        }
        if file.base.is_none() {
            added.push(file.path.clone());
        } else {
            changed.push(file.path.clone());
        }
        kept.push(file.clone());
    }

    let destination = directory(app_id, version, &manifest.name);
    let staging = destination.parent().unwrap_or(Path::new(".")).join(format!(".import-{}", crate::export::uuid_string()));
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> Result<(), OverlayError> {
        for file in &kept {
            let target = staging.join(&file.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(work.join(&file.path), target)?;
        }
        manifest.files = kept;
        write_manifest(&staging, &manifest)?;
        if destination.exists() {
            std::fs::remove_dir_all(&destination)?;
        }
        std::fs::rename(&staging, &destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result?;
    changed.sort();
    added.sort();
    Ok(ImportResult {
        app_id: app_id.to_string(),
        version: version.map(str::to_string),
        name: manifest.name,
        stored_at: destination,
        changed_files: changed,
        added_files: added,
        unchanged_files_dropped: dropped,
    })
}
