//! Static source reconstruction. Every restorer is mechanical: it reads the
//! compiler's output and never evaluates code.

pub mod configuration;
mod pipeline;
pub mod scripts;
pub mod wxml;
pub mod wxs;
pub mod wxss;

use std::collections::HashMap;
use std::ops::Range;

pub use pipeline::{Decompilation, decompile, decompile_reporting};

pub const MAXIMUM_GENERATED_FILE_COUNT: usize = 100_000;
pub const MAXIMUM_TEXT_FILE_SIZE: u64 = 32 * 1024 * 1024;
pub const MAXIMUM_TOTAL_TEXT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Configuration,
    Script,
    Wxs,
    Markup,
    Style,
    Image,
    Font,
    Media,
    Text,
    Binary,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Script => "script",
            Self::Wxs => "wxs",
            Self::Markup => "markup",
            Self::Style => "style",
            Self::Image => "image",
            Self::Font => "font",
            Self::Media => "media",
            Self::Text => "text",
            Self::Binary => "binary",
        }
    }

    pub fn infer(path: &str) -> Self {
        match path_extension(path).to_ascii_lowercase().as_str() {
            "json" | "json5" | "map" => Self::Configuration,
            "js" | "mjs" | "cjs" | "ts" => Self::Script,
            "wxs" => Self::Wxs,
            "wxml" | "html" | "htm" | "xml" | "svg" => Self::Markup,
            "wxss" | "css" | "scss" | "less" => Self::Style,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" => Self::Image,
            "ttf" | "otf" | "woff" | "woff2" => Self::Font,
            "mp3" | "aac" | "wav" | "mp4" | "mov" | "webm" => Self::Media,
            "txt" | "md" | "yaml" | "yml" | "csv" => Self::Text,
            _ => Self::Binary,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Low = 1,
    Medium = 2,
    High = 3,
    Exact = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Indexing,
    Configuration,
    Scripts,
    Wxs,
    Markup,
    Styles,
    Finalizing,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Configuration => "configuration",
            Self::Scripts => "scripts",
            Self::Wxs => "wxs",
            Self::Markup => "markup",
            Self::Styles => "styles",
            Self::Finalizing => "finalizing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Information = 0,
    Warning = 1,
    Error = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    /// Title of the package the entry came from.
    pub package: String,
    pub entry_path: String,
    /// Byte range in the entry's text (after any `<script>` extraction).
    pub start: Option<usize>,
    pub end: Option<usize>,
}

impl SourceRef {
    pub fn applying(&self, range: &Range<usize>) -> Self {
        Self { package: self.package.clone(), entry_path: self.entry_path.clone(), start: Some(range.start), end: Some(range.end) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub stage: Stage,
    pub message: String,
    pub source: Option<SourceRef>,
}

impl Diagnostic {
    pub fn new(severity: Severity, stage: Stage, message: impl Into<String>, source: Option<SourceRef>) -> Self {
        Self { severity, stage, message: message.into(), source }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Generated(String),
    /// An entry kept as is: index into `PackageSet::entries`.
    Entry(usize),
}

impl Content {
    pub fn generated_text(&self) -> Option<&str> {
        match self {
            Self::Generated(text) => Some(text),
            Self::Entry(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Artifact {
    pub path: String,
    pub kind: ArtifactKind,
    pub confidence: Confidence,
    pub content: Content,
    pub sources: Vec<SourceRef>,
    pub notes: Vec<String>,
}

/// One file a restorer proposes.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: String,
    pub kind: ArtifactKind,
    pub content: String,
    pub confidence: Confidence,
    /// Byte ranges in the restorer's input it was built from.
    pub source_ranges: Vec<Range<usize>>,
    pub notes: Vec<String>,
}

impl Candidate {
    pub fn new(path: impl Into<String>, kind: Option<ArtifactKind>, content: String, confidence: Confidence) -> Self {
        let path = path.into();
        let kind = kind.unwrap_or_else(|| ArtifactKind::infer(&path));
        Self { path, kind, content, confidence, source_ranges: Vec::new(), notes: Vec::new() }
    }

    pub fn ranges(mut self, ranges: Vec<Range<usize>>) -> Self {
        self.source_ranges = ranges;
        self
    }

    pub fn notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }
}

/// Results by output path. A second result for a path is kept only when it
/// is more trustworthy; every non-trivial collision is reported.
#[derive(Default)]
pub struct ArtifactStore {
    pub artifacts: HashMap<String, Artifact>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ArtifactStore {
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        raw_path: &str,
        kind: Option<ArtifactKind>,
        confidence: Confidence,
        content: Content,
        sources: Vec<SourceRef>,
        notes: Vec<String>,
        stage: Stage,
    ) {
        if self.artifacts.len() >= MAXIMUM_GENERATED_FILE_COUNT {
            self.diagnostics.push(Diagnostic::new(Severity::Error, stage, "重建文件数量达到上限", None));
            return;
        }
        let Some(path) = normalized_path(raw_path) else {
            self.diagnostics.push(Diagnostic::new(
                Severity::Warning,
                stage,
                format!("忽略异常重建路径：{raw_path}"),
                sources.first().cloned(),
            ));
            return;
        };
        let first_source = sources.first().cloned();
        let candidate = Artifact {
            kind: kind.unwrap_or_else(|| ArtifactKind::infer(&path)),
            path: path.clone(),
            confidence,
            content,
            sources,
            notes,
        };
        let Some(current) = self.artifacts.get(&path) else {
            self.artifacts.insert(path, candidate);
            return;
        };

        // Same content from another container (e.g. render and logic layer
        // copies): nothing to report.
        if current.content == candidate.content {
            if confidence > current.confidence {
                self.artifacts.insert(path, candidate);
            }
            return;
        }
        if confidence > current.confidence
            || (confidence == current.confidence
                && current.content.generated_text().is_none()
                && candidate.content.generated_text().is_some())
        {
            self.artifacts.insert(path.clone(), candidate);
            self.diagnostics.push(Diagnostic::new(
                Severity::Information,
                stage,
                format!("同路径有置信度更高的结果，已替换：{path}"),
                first_source,
            ));
        } else if confidence < current.confidence {
            self.diagnostics.push(Diagnostic::new(
                Severity::Information,
                stage,
                format!("同路径另有置信度较低的结果，已保留较高者：{path}"),
                first_source,
            ));
        } else {
            // Two different results at the same confidence: a real ambiguity.
            self.diagnostics.push(Diagnostic::new(
                Severity::Warning,
                stage,
                format!("同路径重建冲突（置信度相同、内容不同），保留先出现的结果：{path}"),
                first_source,
            ));
        }
    }

    /// Adds a restorer's candidates; their byte ranges become source ranges
    /// in the given entry.
    pub fn add_candidates(&mut self, candidates: Vec<Candidate>, source: &SourceRef, stage: Stage) {
        for candidate in candidates {
            let sources = if candidate.source_ranges.is_empty() {
                vec![source.clone()]
            } else {
                candidate.source_ranges.iter().map(|range| source.applying(range)).collect()
            };
            self.add(
                &candidate.path,
                Some(candidate.kind),
                candidate.confidence,
                Content::Generated(candidate.content),
                sources,
                candidate.notes,
                stage,
            );
        }
    }
}

/// A relative output path: `\` → `/`, scheme prefixes become folders, `.`
/// and empty components dropped; `None` for `..`, NUL, over-long names or
/// an empty result.
pub fn normalized_path(raw_path: &str) -> Option<String> {
    let replaced = raw_path.replace('\\', "/");
    let replaced = scheme_as_folder(crate::text::trim_whitespace_and_newlines(&replaced));
    let mut components = Vec::new();
    for component in replaced.split('/').filter(|component| !component.is_empty()) {
        if component == "." {
            continue;
        }
        if component == ".." || component.contains('\0') || component.len() > 255 {
            return None;
        }
        components.push(component);
    }
    if components.is_empty() {
        return None;
    }
    let result = components.join("/");
    (result.len() <= 16_384).then_some(result)
}

/// `plugin://<id>/x` and `plugin-private://<id>/x` are the plugin's own files:
/// `__plugin__/<id>/x`. Any other `scheme://x` becomes `__scheme__/x`, so no
/// directory is named `scheme:`.
pub fn scheme_as_folder(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.first().is_none_or(|byte| !byte.is_ascii_alphabetic()) {
        return path.to_string();
    }
    let scheme_length = bytes.iter().take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')).count();
    if !path[scheme_length..].starts_with("://") {
        return path.to_string();
    }
    let scheme = path[..scheme_length].to_ascii_lowercase();
    let folder = if scheme == "plugin" || scheme == "plugin-private" { "plugin" } else { scheme.as_str() };
    format!("__{folder}__/{}", &path[scheme_length + 3..])
}

/// `path` with its extension (as `path_extension` sees it) replaced; a
/// relative path stays relative. (The Swift version used to resolve it against
/// the process's current directory; fixed on both sides.)
pub fn changing_extension(path: &str, extension: &str) -> String {
    let current = path_extension(path);
    let trimmed = path.trim_end_matches('/');
    let base = if current.is_empty() { trimmed } else { &trimmed[..trimmed.len() - current.len() - 1] };
    let extension = extension.trim_matches('.');
    if extension.is_empty() { base.to_string() } else { format!("{base}.{extension}") }
}

/// Last path component.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `URL(fileURLWithPath:).pathExtension`: the text after the last `.` of the
/// last component; empty for none, a leading-dot name, or an extension with an
/// ASCII space (Foundation rejects those; tabs, NBSP and CJK are kept).
pub fn path_extension(path: &str) -> &str {
    let name = file_name(path.trim_end_matches('/'));
    match name.rfind('.') {
        Some(0) | None => "",
        Some(position) if name[position + 1..].contains(' ') => "",
        Some(position) => &name[position + 1..],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalized_path("./a//b/./c.js").as_deref(), Some("a/b/c.js"));
        assert_eq!(normalized_path("a/../b"), None);
        assert_eq!(normalized_path("plugin-private://wx1/x.wxml").as_deref(), Some("__plugin__/wx1/x.wxml"));
        assert_eq!(normalized_path("Https://h/x").as_deref(), Some("__https__/h/x"));
        assert_eq!(path_extension("a/b.WXML"), "WXML");
        assert_eq!(path_extension("a.b/c"), "");
        assert_eq!(path_extension("a.b c"), "");
        assert_eq!(path_extension("a.b\tc"), "b\tc");
        assert_eq!(path_extension("dir.x/"), "x");
        assert_eq!(path_extension("x/.b"), "");
    }

    #[test]
    fn store_keeps_higher_confidence() {
        let mut store = ArtifactStore::default();
        let add = |store: &mut ArtifactStore, text: &str, confidence| {
            store.add("p.wxml", None, confidence, Content::Generated(text.into()), vec![], vec![], Stage::Markup)
        };
        add(&mut store, "a", Confidence::Low);
        add(&mut store, "b", Confidence::High);
        add(&mut store, "c", Confidence::Medium);
        add(&mut store, "d", Confidence::High);
        assert_eq!(store.artifacts["p.wxml"].content, Content::Generated("b".into()));
        let messages: Vec<_> = store.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "同路径有置信度更高的结果，已替换：p.wxml",
                "同路径另有置信度较低的结果，已保留较高者：p.wxml",
                "同路径重建冲突（置信度相同、内容不同），保留先出现的结果：p.wxml"
            ]
        );
    }
}
