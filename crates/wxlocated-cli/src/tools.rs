//! `overlay`, `watch` and `doctor`.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use wxlocated_engine::material::report::{self, wechat_client_version};
use wxlocated_engine::material::{Completeness, Context, DeclaredPlugins};
use wxlocated_engine::overlay::{self, Author, OverlayError};
use wxlocated_engine::scan::Miniapp;
use wxlocated_engine::swift_json::{encode, iso8601};

use crate::commands::{EXIT_FAILURE, EXIT_NOT_FOUND, EXIT_USAGE, Failure, SCHEMA_VERSION, TOOL_VERSION, expand_tilde, scan_cache};

/// `URL(fileURLWithPath:isDirectory:).path`: absolute, no trailing slash.
fn absolute(path: &str) -> std::path::PathBuf {
    let joined = std::env::current_dir().unwrap_or_default().join(expand_tilde(path));
    let text = joined.to_string_lossy().into_owned();
    let trimmed = text.trim_end_matches('/');
    std::path::PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed })
}

fn overlay_failure(error: OverlayError) -> Failure {
    Failure { exit: error.exit(), code: error.code(), message: error.to_string(), hint: None }
}

fn package_hashes(app: &Miniapp) -> HashMap<String, String> {
    let mut hashes = HashMap::new();
    for inspection in report::inspect_app(app) {
        if let Some(sha) = inspection.sha256 {
            let name = inspection.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            hashes.entry(name).or_insert(sha);
        }
    }
    hashes
}

pub fn overlay(args: &[String]) -> Result<Value, Failure> {
    let usage = || Failure {
        exit: EXIT_USAGE,
        code: "missing_arguments",
        message: "用法：overlay init <材料目录> <工作目录> | overlay import <工作目录> | overlay list [appid] | overlay remove <appid> <名称>".into(),
        hint: None,
    };
    let Some(sub) = args.first() else { return Err(usage()) };
    let rest = &args[1..];
    let value = |flag: &str| rest.iter().position(|a| a == flag).and_then(|i| rest.get(i + 1)).cloned();
    let mut positionals = Vec::new();
    let mut skip = false;
    for arg in rest {
        if skip {
            skip = false;
            continue;
        }
        if matches!(arg.as_str(), "--name" | "--agent" | "--model") {
            skip = true;
            continue;
        }
        if !arg.starts_with('-') {
            positionals.push(arg.clone());
        }
    }
    match sub.as_str() {
        "init" => {
            if positionals.len() < 2 {
                return Err(usage());
            }
            let (material, work) = (absolute(&positionals[0]), absolute(&positionals[1]));
            let author = Author { agent: value("--agent"), model: value("--model") };
            let manifest = overlay::initialize(&material, &work, &value("--name").unwrap_or_else(|| "agent".into()), author)
                .map_err(overlay_failure)?;
            let work_text = work.to_string_lossy().into_owned();
            let mut output = json!({
                "schema_version": overlay::SCHEMA_VERSION,
                "work_directory": work_text,
                "manifest": work.join(overlay::MANIFEST_NAME).to_string_lossy(),
                "app_id": manifest.base.app_id,
                "files": manifest.files.len(),
                "next_steps": [
                    "直接修改工作目录中的文件；新文件请在 overlay.json 的 files 中登记，base 留空",
                    format!("完成后运行 wxlocated overlay import {work_text}"),
                    "未修改的文件会在导入时自动丢弃",
                ],
            });
            if let Some(version) = manifest.base.version {
                output["version"] = json!(version);
            }
            Ok(output)
        }
        "import" => {
            let Some(path) = positionals.first() else { return Err(usage()) };
            let work = absolute(path);
            let manifest = overlay::read_manifest_strict(&work).map_err(overlay_failure)?;
            let snapshot = scan_cache();
            let app = snapshot
                .miniapps
                .iter()
                .find(|app| app.app_id == manifest.base.app_id)
                .ok_or_else(|| overlay_failure(OverlayError::UnknownApp(manifest.base.app_id.clone())))?;
            let result = overlay::import(&work, &app.app_id, app.version.as_deref(), &package_hashes(app), value("--name").as_deref())
                .map_err(overlay_failure)?;
            let mut output = json!({
                "schema_version": overlay::SCHEMA_VERSION,
                "app_id": result.app_id,
                "name": result.name,
                "stored_at": result.stored_at.to_string_lossy(),
                "changed_files": result.changed_files,
                "added_files": result.added_files,
                "unchanged_files_dropped": result.unchanged_files_dropped,
            });
            if let Some(version) = result.version {
                output["version"] = json!(version);
            }
            Ok(output)
        }
        "list" => {
            let filter = positionals.first();
            let root = overlay::root();
            let mut apps: Vec<String> = std::fs::read_dir(&root)
                .map(|entries| entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            apps.sort();
            let mut layers = Vec::new();
            for app_id in apps.iter().filter(|id| filter.is_none_or(|f| f == *id)) {
                let mut versions: Vec<String> = std::fs::read_dir(root.join(app_id))
                    .map(|entries| entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                    .unwrap_or_default();
                versions.sort();
                for version in versions.iter().filter(|v| !v.starts_with('.')) {
                    for layer in overlay::layers(app_id, Some(version)) {
                        let mut entry = json!({
                            "app_id": app_id,
                            "version": version,
                            "name": layer.manifest.name,
                            "created_at": layer.manifest.created_at,
                            "files": layer.manifest.files.len(),
                            "directory": layer.directory.to_string_lossy(),
                        });
                        if let Some(agent) = &layer.manifest.author.agent {
                            entry["agent"] = json!(agent);
                        }
                        if let Some(model) = &layer.manifest.author.model {
                            entry["model"] = json!(model);
                        }
                        layers.push(entry);
                    }
                }
            }
            Ok(json!({ "schema_version": overlay::SCHEMA_VERSION, "layers": layers }))
        }
        "remove" => {
            if positionals.len() < 2 {
                return Err(usage());
            }
            let (app_id, name) = (&positionals[0], &positionals[1]);
            let versions: Vec<String> = std::fs::read_dir(overlay::root().join(app_id))
                .map(|entries| entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            let layer = versions.iter().flat_map(|v| overlay::layers(app_id, Some(v))).find(|layer| layer.manifest.name == *name);
            let Some(layer) = layer else {
                return Err(Failure { exit: EXIT_NOT_FOUND, code: "not_found", message: format!("{app_id} 没有名为 {name} 的重建层"), hint: None });
            };
            std::fs::remove_dir_all(&layer.directory)
                .map_err(|e| Failure { exit: EXIT_FAILURE, code: "failed", message: e.to_string(), hint: None })?;
            Ok(json!({ "schema_version": overlay::SCHEMA_VERSION, "removed": layer.directory.to_string_lossy() }))
        }
        _ => Err(usage()),
    }
}

// MARK: watch

fn locate(query: &str) -> Result<(Miniapp, Context), Failure> {
    let snapshot = scan_cache();
    let app = snapshot
        .miniapps
        .iter()
        .find(|app| app.app_id == query)
        .or_else(|| snapshot.miniapps.iter().find(|app| app.name.as_deref() == Some(query)))
        .cloned()
        .ok_or_else(|| Failure {
            exit: EXIT_NOT_FOUND,
            code: "not_found",
            message: format!("本机缓存中没有找到 {query}"),
            hint: Some("先在微信中打开这个小程序，再运行 wxlocated list 确认 AppID".into()),
        })?;
    let context = Context::for_app(&snapshot, &app);
    Ok((app, context))
}

/// Plugin declarations come from app.json, so they are read once per version.
fn declared_plugins(app: &Miniapp) -> DeclaredPlugins {
    report::open_app(app).map(|set| report::analyze(&set).plugins).unwrap_or_default()
}

fn emit(fields: Value) {
    let mut event = json!({ "schema_version": SCHEMA_VERSION, "at": iso8601(SystemTime::now()) });
    if let (Value::Object(target), Value::Object(source)) = (&mut event, fields) {
        target.extend(source);
    }
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", encode(&event, false));
    let _ = stdout.flush();
}

fn watch_hint(missing_subpackages: &[String], missing_plugins: &[String]) -> String {
    let mut parts = Vec::new();
    if !missing_subpackages.is_empty() {
        parts.push(format!("在微信中进入以下分包对应的页面：{}", missing_subpackages.join(", ")));
    }
    if !missing_plugins.is_empty() {
        parts.push(format!("使用会加载以下插件的功能：{}", missing_plugins.join(", ")));
    }
    parts.join("；")
}

fn with_version(mut value: Value, version: &Option<String>) -> Value {
    if let Some(version) = version {
        value["version"] = json!(version);
    }
    value
}

/// Waits for missing subpackages and plugins to land in the cache. Emits one
/// JSON object per line: `started`, `subpackage_found`, `plugin_found`,
/// `version_changed`, then `complete` (exit 0) or `timeout` (exit 75).
pub fn watch(args: &[String]) -> Result<u8, Failure> {
    let query = args
        .iter()
        .find(|arg| !arg.starts_with('-') && arg.parse::<f64>().is_err())
        .ok_or(Failure { exit: EXIT_USAGE, code: "missing_app", message: "watch 需要一个 AppID 或小程序名称".into(), hint: None })?;
    let number = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<f64>().ok());
    let timeout = number("--timeout").unwrap_or(600.0);
    let interval = number("--interval").unwrap_or(2.0).max(0.5);

    let (mut app, context) = locate(query)?;
    let mut plugins = declared_plugins(&app);
    let mut state = Completeness::new(&app, Some(&plugins), &context);
    let complete = |state: &Completeness| state.missing_subpackages.is_empty() && state.missing_plugins.is_empty();

    let mut started = with_version(
        json!({
            "event": "started",
            "app_id": app.app_id,
            "missing_subpackages": state.missing_subpackages,
            "missing_plugins": state.missing_plugins,
        }),
        &app.version,
    );
    if !complete(&state) {
        started["hint"] = json!(watch_hint(&state.missing_subpackages, &state.missing_plugins));
    }
    emit(started);
    if complete(&state) {
        emit(with_version(json!({ "event": "complete", "app_id": app.app_id }), &app.version));
        return Ok(0);
    }

    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs_f64(interval));
        let Ok((latest, latest_context)) = locate(&app.app_id) else { continue };
        if latest.version != app.version {
            plugins = declared_plugins(&latest);
            let next = Completeness::new(&latest, Some(&plugins), &latest_context);
            let mut event = with_version(
                json!({
                    "event": "version_changed",
                    "app_id": latest.app_id,
                    "missing_subpackages": next.missing_subpackages,
                    "missing_plugins": next.missing_plugins,
                }),
                &latest.version,
            );
            if let Some(previous) = &app.version {
                event["previous_version"] = json!(previous);
            }
            emit(event);
            state = next;
        } else {
            let next = Completeness::new(&latest, Some(&plugins), &latest_context);
            for root in state.missing_subpackages.iter().filter(|root| !next.missing_subpackages.contains(root)) {
                let key = wxlocated_engine::material::cache_key_for_root(root);
                let file = latest.sub_packages.iter().find(|path| {
                    wxlocated_engine::material::cache_key_for_file(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                        == key
                });
                let mut event = with_version(
                    json!({ "event": "subpackage_found", "app_id": latest.app_id, "root": root, "missing_subpackages": next.missing_subpackages }),
                    &latest.version,
                );
                if let Some(file) = file {
                    event["file"] = json!(file.to_string_lossy());
                }
                emit(event);
            }
            for alias in state.missing_plugins.iter().filter(|alias| !next.missing_plugins.contains(alias)) {
                let mut event = with_version(
                    json!({ "event": "plugin_found", "app_id": latest.app_id, "plugin": alias, "missing_plugins": next.missing_plugins }),
                    &latest.version,
                );
                if let Some(provider) = plugins.get(alias) {
                    event["provider"] = json!(provider);
                }
                emit(event);
            }
            state = next;
        }
        app = latest;
        if complete(&state) {
            emit(with_version(
                json!({ "event": "complete", "app_id": app.app_id, "hint": format!("运行 wxlocated export {} --out <目录> 导出完整材料", app.app_id) }),
                &app.version,
            ));
            return Ok(0);
        }
    }
    emit(with_version(
        json!({
            "event": "timeout",
            "app_id": app.app_id,
            "missing_subpackages": state.missing_subpackages,
            "missing_plugins": state.missing_plugins,
            "hint": watch_hint(&state.missing_subpackages, &state.missing_plugins),
        }),
        &app.version,
    ));
    Ok(75)
}

// MARK: doctor

/// `~/.local/bin/wxlocated`, a shim forwarding to the installed binary.
pub fn shim_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/wxlocated")
}

fn os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        let sysctl = |name: &str| {
            std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", name])
                .output()
                .ok()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .unwrap_or_default()
        };
        format!("Version {} (Build {})", sysctl("kern.osproductversion"), sysctl("kern.osversion"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::consts::OS.to_string()
    }
}

/// Checks what agents most often trip over: disk access, cache locations,
/// WeChat, and whether the CLI is reachable on PATH. Exit 0 when nothing
/// fails (warnings allowed), 70 otherwise.
pub fn doctor() -> (Value, u8) {
    let snapshot = scan_cache();
    let mut checks: Vec<Value> = Vec::new();
    let check = |id: &str, status: &str, message: String, fix: Option<String>| {
        let mut value = json!({ "id": id, "status": status, "message": message });
        if let Some(fix) = fix {
            value["fix"] = json!(fix);
        }
        value
    };
    let roots = |list: &[std::path::PathBuf]| list.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(", ");

    if snapshot.inaccessible_roots.is_empty() {
        checks.push(check("disk_access", "ok", "缓存目录均可读取".into(), None));
    } else {
        checks.push(check(
            "disk_access",
            "fail",
            format!("{} 个缓存目录无权读取：{}", snapshot.inaccessible_roots.len(), roots(&snapshot.inaccessible_roots)),
            Some("在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许运行 wxlocated 的终端或 Agent 宿主应用，然后重启它".into()),
        ));
    }
    if snapshot.readable_roots.is_empty() {
        checks.push(check(
            "cache_roots",
            "fail",
            "没有找到可读取的小程序缓存目录".into(),
            Some("确认已在 Mac 版微信中打开过小程序；缓存位置特殊时，在 WxLocated 设置中添加 Applet 缓存目录".into()),
        ));
    } else {
        checks.push(check(
            "cache_roots",
            "ok",
            format!("{} 个缓存目录：{}", snapshot.readable_roots.len(), roots(&snapshot.readable_roots)),
            None,
        ));
    }
    let incomplete =
        snapshot.miniapps.iter().filter(|app| !Completeness::new(app, None, &Context::default()).missing_subpackages.is_empty()).count();
    if snapshot.miniapps.is_empty() {
        checks.push(check("miniapps", "warn", "缓存中没有小程序".into(), Some("在微信中打开目标小程序后重试".into())));
    } else {
        let suffix = if incomplete == 0 { String::new() } else { format!("，其中 {incomplete} 个缺少分包") };
        checks.push(check(
            "miniapps",
            if incomplete == 0 { "ok" } else { "warn" },
            format!("{} 个小程序{suffix}", snapshot.miniapps.len()),
            (incomplete > 0).then(|| "对缺分包的小程序运行 wxlocated watch <appid>，并在微信中进入对应页面".to_string()),
        ));
    }
    match wechat_client_version() {
        Some(version) => checks.push(check("wechat", "ok", format!("微信 {version}"), None)),
        None => checks.push(check("wechat", "warn", "未在 /Applications 找到微信，material.json 将不记录客户端版本".into(), None)),
    }

    let executable = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let shim = shim_path();
    let shim_directory = shim.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let on_path = std::env::var("PATH").unwrap_or_default().split(':').any(|entry| entry == shim_directory);
    match std::fs::read_to_string(&shim) {
        Ok(contents) if contents.contains(&executable) && on_path => {
            checks.push(check("cli", "ok", format!("wxlocated 已安装在 {}", shim.to_string_lossy()), None));
        }
        Ok(contents) if contents.contains(&executable) => checks.push(check(
            "cli",
            "warn",
            format!("wxlocated 已安装，但 {shim_directory} 不在 PATH 中"),
            Some("在 shell 配置中加入 export PATH=\"$HOME/.local/bin:$PATH\"".into()),
        )),
        Ok(_) => checks.push(check("cli", "warn", "wxlocated 指向另一份 WxLocated".into(), Some("在 WxLocated 设置 › Agent 中重新安装命令行工具".into()))),
        Err(_) => checks.push(check(
            "cli",
            "warn",
            format!("wxlocated 未安装，当前通过 {executable} 调用"),
            Some("在 WxLocated 设置 › Agent 中安装命令行工具".into()),
        )),
    }

    let failed = checks.iter().any(|c| c["status"] == "fail");
    let report = json!({
        "schema_version": SCHEMA_VERSION,
        "ok": !failed,
        "tool_version": TOOL_VERSION,
        "macos_version": os_version(),
        "executable": executable,
        "checks": checks,
    });
    (report, if failed { EXIT_FAILURE } else { 0 })
}
