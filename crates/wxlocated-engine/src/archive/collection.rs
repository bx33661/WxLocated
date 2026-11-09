//! Several packages read as one logical archive (a main package and its
//! subpackages) while every entry keeps the package it came from. A broken
//! optional subpackage becomes a note and does not hide a valid main package.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::{Archive, ArchiveError, Entry, natural_compare};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Standalone,
    Main,
    Subpackage,
}

pub struct PackageSource {
    pub archive: Archive,
    pub kind: SourceKind,
    /// `主包 · __APP__.wxapkg`, `分包 · x.wxapkg`, or the file name.
    pub title: String,
    /// Folder for this package's own copy of its files in an export.
    pub export_folder: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetEntry {
    /// Index into `PackageSet::sources`.
    pub source: usize,
    pub path: String,
    pub offset: u64,
    pub size: u64,
}

pub struct PackageSet {
    pub sources: Vec<PackageSource>,
    pub entries: Vec<SetEntry>,
    pub notes: Vec<String>,
    composite_format: bool,
}

impl PackageSet {
    /// One package on its own (as plugins are decompiled).
    pub fn standalone(archive: Archive) -> Self {
        let title = file_name(&archive.path);
        let export_folder = file_stem(&archive.path);
        let entries = archive
            .entries
            .iter()
            .map(|entry| SetEntry { source: 0, path: entry.path.clone(), offset: entry.offset, size: entry.size })
            .collect();
        let notes = archive.notes.clone();
        Self {
            sources: vec![PackageSource { archive, kind: SourceKind::Standalone, title, export_folder }],
            entries,
            notes,
            composite_format: false,
        }
    }

    /// An app's main package followed by its subpackages. With a single
    /// package this is the same as `standalone`.
    pub fn open_app(app_id: &str, packages: &[PathBuf], declared_subpackages: usize) -> Result<Self, ArchiveError> {
        let main_path = packages.first().ok_or(ArchiveError::Unreadable)?;
        let main = Archive::open_with_app_id(main_path, Some(app_id))?;
        if packages.len() == 1 {
            return Ok(Self::standalone(main));
        }

        let mut opened = vec![(main, SourceKind::Main, 0usize)];
        let mut notes = Vec::new();
        for (index, path) in packages.iter().enumerate().skip(1) {
            match Archive::open_with_app_id(path, Some(app_id)) {
                Ok(archive) => opened.push((archive, SourceKind::Subpackage, index)),
                Err(error) => notes.push(format!("分包 {} 读取失败：{error}", file_name(path))),
            }
        }

        let mut used_folders = HashSet::new();
        let mut sources = Vec::new();
        let mut entries = Vec::new();
        let opened_count = opened.len();
        for (archive, kind, ordinal) in opened {
            let (title, base_folder) = match kind {
                SourceKind::Main => (format!("主包 · {}", file_name(&archive.path)), "主包".to_string()),
                SourceKind::Subpackage => (
                    format!("分包 · {}", file_name(&archive.path)),
                    format!("分包-{}", sanitized_component(&file_stem(&archive.path))),
                ),
                SourceKind::Standalone => (file_name(&archive.path), format!("包-{}", ordinal + 1)),
            };
            let export_folder = unique_folder(&base_folder, &mut used_folders);
            let source = sources.len();
            entries.extend(archive.entries.iter().map(|entry: &Entry| SetEntry {
                source,
                path: entry.path.clone(),
                offset: entry.offset,
                size: entry.size,
            }));
            notes.extend(archive.notes.iter().map(|note| format!("{title}：{note}")));
            sources.push(PackageSource { archive, kind, title, export_folder });
        }

        let mut path_counts: HashMap<&str, usize> = HashMap::new();
        for entry in &entries {
            *path_counts.entry(entry.path.as_str()).or_default() += 1;
        }
        let collisions = path_counts.values().filter(|&&count| count > 1).count();
        notes.insert(0, format!("已统一读取 {}/{} 个包", opened_count, packages.len()));
        if declared_subpackages > 0 {
            notes.insert(1, format!("分包声明 {} 项，当前缓存 {} 个分包文件", declared_subpackages, packages.len() - 1));
        }
        if collisions > 0 {
            notes.push(format!("检测到 {collisions} 个跨包同路径文件，已按来源分别保留"));
        }

        entries.sort_by(|left, right| natural_compare(&left.path, &right.path).then(left.source.cmp(&right.source)));
        Ok(Self { sources, entries, notes, composite_format: true })
    }

    pub fn is_composite(&self) -> bool {
        self.sources.len() > 1 || self.composite_format
    }

    pub fn read(&self, entry: &SetEntry) -> Result<Vec<u8>, ArchiveError> {
        self.read_limited(entry, u64::MAX)
    }

    pub fn read_limited(&self, entry: &SetEntry, maximum_size: u64) -> Result<Vec<u8>, ArchiveError> {
        let archive = &self.sources[entry.source].archive;
        archive.read_limited(&Entry { path: entry.path.clone(), offset: entry.offset, size: entry.size }, maximum_size)
    }

    pub fn source_title(&self, entry: &SetEntry) -> &str {
        &self.sources[entry.source].title
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `URL.deletingPathExtension().lastPathComponent`.
fn file_stem(path: &Path) -> String {
    path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn sanitized_component(value: &str) -> String {
    let pieces: Vec<&str> = value.split(['/', ':', '\0']).filter(|piece| !piece.is_empty()).collect();
    let joined = pieces.join("-");
    let trimmed = crate::text::trim_whitespace_and_newlines(&joined);
    let result = if trimmed.is_empty() { "内容" } else { trimmed };
    result.chars().take(80).collect()
}

/// Folder names compare case-insensitively (as on the default macOS volume).
fn unique_folder(base: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = base.to_string();
    let mut suffix = 2;
    while !used.insert(candidate.to_lowercase()) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}
