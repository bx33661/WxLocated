//! The agent-facing commands. Every command writes one JSON document to
//! stdout in the exact format of the Swift CLI it replaces.

use std::time::SystemTime;

use serde_json::{Value, json};
use wxlocated_engine::material::{Completeness, Context, report};
use wxlocated_engine::scan::{self, Miniapp, Snapshot};
use wxlocated_engine::settings::Settings;
use wxlocated_engine::swift_json::iso8601;

pub const SCHEMA_VERSION: i64 = 2;
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const ACCESS_HINT: &str =
    "部分缓存目录无权读取：在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许当前终端或 Agent 宿主应用";

/// A scan with custom names applied, as the Swift `ScannerViewModel` gives it.
pub fn scan_cache() -> Snapshot {
    let settings = Settings::load();
    let mut snapshot = scan::scan(&settings.custom_scan_paths);
    for app in &mut snapshot.miniapps {
        if let Some(name) = settings.custom_app_names.get(&app.app_id) {
            app.name = Some(name.clone());
            app.metadata_source = Some("自定义名称".into());
        }
    }
    snapshot
}

fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn list() -> Value {
    let snapshot = scan_cache();
    let mut apps: Vec<&Miniapp> = snapshot.miniapps.iter().collect();
    apps.sort_by_key(|app| std::cmp::Reverse(app.modified));
    let miniapps: Vec<Value> = apps
        .iter()
        .map(|app| {
            let completeness = Completeness::new(app, None, &Context::default());
            let mut entry = json!({
                "app_id": app.app_id,
                "name": app.display_name(),
                "name_resolved": app.name.is_some(),
                "size_bytes": app.size,
                "modified_at": iso8601(app.modified),
                "cache_directory": path_text(&app.cache_directory()),
                "main_package": file_name(&app.main_package),
                "subpackage_files": app.sub_packages.iter().map(|p| file_name(p)).collect::<Vec<_>>(),
                "declared_subpackages": completeness.declared_subpackages.len(),
                "missing_subpackages": completeness.missing_subpackages,
            });
            if let Some(version) = &app.version {
                entry["version"] = json!(version);
            }
            entry
        })
        .collect();
    let mut output = json!({
        "schema_version": SCHEMA_VERSION,
        "generated_at": iso8601(SystemTime::now()),
        "count": miniapps.len(),
        "readable_roots": snapshot.readable_roots.iter().map(|p| path_text(p)).collect::<Vec<_>>(),
        "inaccessible_roots": snapshot.inaccessible_roots.iter().map(|p| path_text(p)).collect::<Vec<_>>(),
        "miniapps": miniapps,
    });
    if !snapshot.inaccessible_roots.is_empty() {
        output["hint"] = json!(ACCESS_HINT);
    }
    output
}

/// A command failure: printed as `{schema_version, error}` with this exit code.
pub struct Failure {
    pub exit: u8,
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

pub const EXIT_USAGE: u8 = 64;
pub const EXIT_NOT_FOUND: u8 = 66;
pub const EXIT_FAILURE: u8 = 70;

impl Failure {
    pub fn output(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(hint) = &self.hint {
            error["hint"] = json!(hint);
        }
        json!({ "schema_version": SCHEMA_VERSION, "error": error })
    }
}

/// The first argument that is not an option (or an option's value).
pub fn positional(args: &[String], command: &str) -> Result<String, Failure> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--out" || arg == "-o" {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Ok(arg.clone());
    }
    Err(Failure { exit: EXIT_USAGE, code: "missing_app", message: format!("{command} 需要一个 AppID 或小程序名称"), hint: None })
}

/// By AppID, else by a unique name.
pub fn resolve_app<'a>(snapshot: &'a Snapshot, query: &str) -> Result<&'a Miniapp, Failure> {
    if let Some(app) = snapshot.miniapps.iter().find(|app| app.app_id == query) {
        return Ok(app);
    }
    let by_name: Vec<&Miniapp> = snapshot.miniapps.iter().filter(|app| app.name.as_deref() == Some(query)).collect();
    match by_name.len() {
        1 => Ok(by_name[0]),
        0 => Err(Failure {
            exit: EXIT_NOT_FOUND,
            code: "not_found",
            message: format!("本机缓存中没有找到 {query}"),
            hint: Some(if snapshot.inaccessible_roots.is_empty() { "运行 wxlocated list 查看可用的 AppID".into() } else { ACCESS_HINT.into() }),
        }),
        count => Err(Failure {
            exit: EXIT_USAGE,
            code: "ambiguous",
            message: format!("有 {count} 个小程序叫“{query}”，请改用 AppID"),
            hint: Some(by_name.iter().map(|app| app.app_id.as_str()).collect::<Vec<_>>().join(", ")),
        }),
    }
}

pub fn info(args: &[String]) -> Result<Value, Failure> {
    let query = positional(args, "info")?;
    let snapshot = scan_cache();
    let app = resolve_app(&snapshot, &query)?;
    Ok(report::inspect(app, &Context::for_app(&snapshot, app)))
}

/// `~` expanded; relative paths stay relative to the current directory.
pub fn expand_tilde(path: &str) -> std::path::PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            std::path::PathBuf::from(format!("{}{rest}", std::env::var("HOME").unwrap_or_default()))
        }
        _ => std::path::PathBuf::from(path),
    }
}

pub fn export(args: &[String]) -> Result<Value, Failure> {
    use wxlocated_engine::material::export::{MaterialExportError, Options, export};
    let query = positional(args, "export")?;
    let out = args
        .iter()
        .position(|arg| arg == "--out" || arg == "-o")
        .and_then(|index| args.get(index + 1))
        .ok_or(Failure { exit: EXIT_USAGE, code: "missing_output", message: "export 需要 --out <目录>".into(), hint: None })?;
    let parent = std::env::current_dir().unwrap_or_default().join(expand_tilde(out));
    let snapshot = scan_cache();
    let app = resolve_app(&snapshot, &query)?;
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let options = Options {
        replace_existing: has("--force"),
        include_source: !has("--no-source"),
        include_raw: !has("--no-raw"),
        reuse_if_unchanged: has("--reuse"),
        redact: has("--redact"),
    };
    let log = |message: &str| eprintln!("[wxlocated] {message}");
    export(app, &parent, &options, &Context::for_app(&snapshot, app), &log).map_err(|error| match error {
        MaterialExportError::DestinationExists(_) => {
            Failure { exit: EXIT_FAILURE, code: "destination_exists", message: error.to_string(), hint: None }
        }
        MaterialExportError::Io(_) => Failure { exit: EXIT_FAILURE, code: "failed", message: error.to_string(), hint: None },
    })
}

pub fn trace(args: &[String]) -> Result<Value, Failure> {
    let current = std::env::current_dir().unwrap_or_default();
    // `URL(fileURLWithPath:isDirectory:).path`: absolute, no trailing slash.
    let root = |path: &str| {
        let joined = current.join(expand_tilde(path));
        let text = joined.to_string_lossy().into_owned();
        let trimmed = text.trim_end_matches('/');
        std::path::PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed })
    };
    wxlocated_engine::trace::trace(args, root)
        .map_err(|failure| Failure { exit: failure.exit, code: failure.code, message: failure.message, hint: None })
}

pub fn version() -> Value {
    json!({ "schema_version": SCHEMA_VERSION, "name": "wxlocated", "version": TOOL_VERSION })
}
