//! Hidden parity commands: the same JSON as the Swift oracle's
//! `__dump-decompilation`, for `scripts/parity-decompilation.py`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use wxlocated_engine::archive::{Archive, PackageSet};
use wxlocated_engine::decompile::{self, Content, SourceRef};

/// What the Swift oracle reports as its `input`.
#[derive(Deserialize, Serialize)]
pub struct Input {
    app_id: String,
    packages: Vec<String>,
    declared_subpackages: usize,
    standalone: bool,
}

#[derive(Serialize)]
struct Output {
    input: Input,
    notes: Vec<String>,
    artifacts: Vec<Artifact>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Serialize)]
struct Artifact {
    path: String,
    kind: &'static str,
    confidence: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_entry: Option<String>,
    sources: Vec<Source>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct Source {
    package: String,
    entry_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<usize>,
}

impl From<&SourceRef> for Source {
    fn from(source: &SourceRef) -> Self {
        Self { package: source.package.clone(), entry_path: source.entry_path.clone(), start: source.start, end: source.end }
    }
}

#[derive(Serialize)]
struct Diagnostic {
    severity: u8,
    stage: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<Source>,
}

/// `__dump-decompilation --spec <input.json>`
pub fn dump_decompilation(args: &[String]) -> Result<serde_json::Value, String> {
    let spec_index = args.iter().position(|arg| arg == "--spec").ok_or("__dump-decompilation 需要 --spec <input.json>")?;
    let spec_path = args.get(spec_index + 1).ok_or("--spec 缺少文件")?;
    let text = std::fs::read_to_string(spec_path).map_err(|error| error.to_string())?;
    let input: Input = serde_json::from_str(&text).map_err(|error| error.to_string())?;

    let packages: Vec<PathBuf> = input.packages.iter().map(PathBuf::from).collect();
    let set = if input.standalone {
        PackageSet::standalone(Archive::open_with_app_id(&packages[0], Some(&input.app_id)).map_err(|error| error.code())?)
    } else {
        PackageSet::open_app(&input.app_id, &packages, input.declared_subpackages).map_err(|error| error.code())?
    };
    let result = decompile::decompile(&set);

    let artifacts = result
        .artifacts
        .iter()
        .map(|artifact| {
            let (content, archive_entry) = match &artifact.content {
                Content::Generated(text) => (Some(text.clone()), None),
                Content::Entry(index) => {
                    let entry = &set.entries[*index];
                    (None, Some(format!("{}@{}+{}", set.source_title(entry), entry.offset, entry.size)))
                }
            };
            Artifact {
                path: artifact.path.clone(),
                kind: artifact.kind.as_str(),
                confidence: artifact.confidence as u8,
                content,
                archive_entry,
                sources: artifact.sources.iter().map(Source::from).collect(),
                notes: artifact.notes.clone(),
            }
        })
        .collect();
    let diagnostics = result
        .diagnostics
        .iter()
        .map(|diagnostic| Diagnostic {
            severity: diagnostic.severity as u8,
            stage: diagnostic.stage.as_str(),
            message: diagnostic.message.clone(),
            source: diagnostic.source.as_ref().map(Source::from),
        })
        .collect();
    let output = Output { input, notes: set.notes.clone(), artifacts, diagnostics };
    serde_json::to_value(output).map_err(|error| error.to_string())
}
