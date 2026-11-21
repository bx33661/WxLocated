//! JSON commands and their shared cache lookup.

use std::path::PathBuf;
use std::time::SystemTime;

use serde_json::{Value, json};
use wxlocated_engine::material::{Completeness, Context, report};
use wxlocated_engine::scan::{self, Miniapp, Snapshot};
use wxlocated_engine::settings::Settings;
use wxlocated_engine::swift_json::iso8601;

pub const SCHEMA_VERSION: i64 = 2;
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const ACCESS_HINT: &str = if cfg!(target_os = "macos") {
    "部分缓存目录无权读取：在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许当前终端或 Agent 宿主应用"
} else {
    "部分缓存目录无权读取：请确认当前用户具有缓存目录的读取权限，或在设置中添加可读取的 Applet 缓存目录"
};

pub fn scan_cache(extra_paths: &[String]) -> Snapshot {
    let mut settings = Settings::load();
    settings.custom_scan_paths.extend_from_slice(extra_paths);
    let mut snapshot = scan::scan(&settings.custom_scan_paths);
    snapshot.apply_custom_names(&settings.custom_app_names);
    snapshot
}

fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn list(scan_paths: &[String]) -> Value {
    let snapshot = scan_cache(scan_paths);
    let miniapps: Vec<Value> = snapshot.miniapps.iter()
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
#[derive(Debug)]
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

pub fn option_value<'a>(args: &'a [String], flags: &[&str]) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| flags.contains(&pair[0].as_str()))
        .map(|pair| pair[1].as_str())
}

pub fn positionals<'a>(args: &'a [String], valued_flags: &[&str]) -> impl Iterator<Item = &'a str> {
    let mut args = args.iter().map(String::as_str);
    std::iter::from_fn(move || {
        while let Some(arg) = args.next() {
            if valued_flags.contains(&arg) {
                args.next();
            } else if !arg.starts_with('-') {
                return Some(arg);
            }
        }
        None
    })
}

pub fn app_query<'a>(args: &'a [String], command: &str, valued_flags: &[&str]) -> Result<&'a str, Failure> {
    positionals(args, valued_flags).next().ok_or_else(|| Failure {
        exit: EXIT_USAGE, code: "missing_app", message: format!("{command} 需要一个 AppID 或小程序名称"), hint: None,
    })
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

pub fn info(args: &[String], scan_paths: &[String]) -> Result<Value, Failure> {
    let query = app_query(args, "info", &[])?;
    let snapshot = scan_cache(scan_paths);
    let app = resolve_app(&snapshot, query)?;
    Ok(report::inspect(app, &Context::for_app(&snapshot, app)))
}

pub fn absolute_path(path: &str) -> PathBuf {
    let expanded = wxlocated_engine::platform::expand_path(path);
    let joined = std::env::current_dir().unwrap_or_default().join(expanded);
    joined.components().collect()
}

pub fn export(args: &[String], scan_paths: &[String]) -> Result<Value, Failure> {
    use wxlocated_engine::material::export::{MaterialExportError, Options, export};
    const OUTPUT_FLAGS: &[&str] = &["--out", "-o"];
    let query = app_query(args, "export", OUTPUT_FLAGS)?;
    let out = option_value(args, OUTPUT_FLAGS)
        .filter(|value| !value.starts_with('-'))
        .ok_or(Failure { exit: EXIT_USAGE, code: "missing_output", message: "export 需要 --out <目录>".into(), hint: None })?;
    let parent = absolute_path(out);
    let snapshot = scan_cache(scan_paths);
    let app = resolve_app(&snapshot, query)?;
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
    wxlocated_engine::trace::trace(args, absolute_path)
        .map_err(|failure| Failure { exit: failure.exit, code: failure.code, message: failure.message, hint: None })
}

pub fn version() -> Value {
    json!({ "schema_version": SCHEMA_VERSION, "name": "wxlocated", "version": TOOL_VERSION })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use super::*;

    fn miniapp(app_id: &str, name: Option<&str>) -> Miniapp {
        Miniapp {
            app_id: app_id.into(),
            name: name.map(str::to_string),
            main_package: PathBuf::from(format!("/cache/{app_id}/1/__APP__.wxapkg")),
            sub_packages: Vec::new(),
            size: 128,
            modified: SystemTime::UNIX_EPOCH,
            version: Some("1".into()),
            icon: None,
            metadata_source: None,
            declared_subpackage_roots: Vec::new(),
        }
    }

    #[test]
    fn option_and_positional_helpers_skip_valued_flags() {
        let args: Vec<String> = ["--out", "/tmp/out", "wx0123456789abcdef", "--reuse"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(option_value(&args, &["--out", "-o"]), Some("/tmp/out"));
        let pos: Vec<_> = positionals(&args, &["--out", "-o"]).collect();
        assert_eq!(pos, ["wx0123456789abcdef"]);
    }

    #[test]
    fn resolve_app_handles_id_unique_name_ambiguity_and_access_hint() {
        let mut snapshot = Snapshot {
            miniapps: vec![
                miniapp("wx1111111111111111", Some("重复名称")),
                miniapp("wx2222222222222222", Some("重复名称")),
                miniapp("wx3333333333333333", Some("唯一名称")),
            ],
            ..Snapshot::default()
        };

        assert_eq!(resolve_app(&snapshot, "wx1111111111111111").unwrap().app_id, "wx1111111111111111");
        assert_eq!(resolve_app(&snapshot, "唯一名称").unwrap().app_id, "wx3333333333333333");

        let amb = resolve_app(&snapshot, "重复名称").unwrap_err();
        assert_eq!(amb.exit, EXIT_USAGE);
        assert_eq!(amb.code, "ambiguous");

        let missing = resolve_app(&snapshot, "不存在").unwrap_err();
        assert_eq!(missing.exit, EXIT_NOT_FOUND);
        assert_eq!(missing.code, "not_found");
        assert!(missing.hint.as_deref().is_some_and(|h| h.contains("wxlocated list")));

        snapshot.inaccessible_roots.push(PathBuf::from("/blocked"));
        let blocked = resolve_app(&snapshot, "不存在").unwrap_err();
        assert_eq!(blocked.exit, EXIT_NOT_FOUND);
        assert_eq!(blocked.hint.as_deref(), Some(ACCESS_HINT));
    }
}
