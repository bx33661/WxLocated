//! Exports from the app: audit material (same as `wxlocated export`), the
//! main package, a single file, or the rebuilt source project. Work runs in
//! the background; the outcome arrives as a toast with a "show" action.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::*;
use wxlocated_engine::material::Context as MaterialContext;
use wxlocated_engine::material::export::{Options, export};
use wxlocated_engine::scan::Miniapp;
use crate::library::Library;

/// One toast slot per window for export progress and results.
struct ExportNote;

pub fn running(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::info(message).id::<ExportNote>().autohide(false), cx);
}

/// Success toast; with `reveal`, a button that shows the result in Finder.
pub fn done(message: impl Into<SharedString>, reveal: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let mut note = Notification::success(message).id::<ExportNote>();
    if let Some(path) = reveal {
        note = note.action(move |_, _, _| {
            let path = path.clone();
            Button::new("reveal").label("在 Finder 中显示").ghost().on_click(move |_, _, cx| cx.reveal_path(&path))
        });
    }
    window.push_notification(note, cx);
}

pub fn failed(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::error(message).title("导出失败").id::<ExportNote>(), cx);
}

/// Exports audit material to the default folder, or asks for one.
pub fn export_material<V: 'static>(app: Miniapp, context: MaterialContext, window: &mut Window, cx: &mut Context<V>) {
    start_material_export(app, context, false, window, cx);
}

fn start_material_export(app: Miniapp, context: MaterialContext, choose_directory: bool, window: &mut Window, cx: &mut App) {
    let settings = Library::global(cx).read(cx).settings.clone();
    let fixed = (!choose_directory).then(|| settings.export_directory.clone().map(PathBuf::from)).flatten().filter(|p| p.is_dir());
    let paths = fixed.is_none().then(|| {
        cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("导出到这里".into()) })
    });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let parent = match (fixed, paths) {
            (Some(parent), _) => parent,
            (None, Some(paths)) => match paths.await {
                Ok(Ok(Some(paths))) if !paths.is_empty() => paths[0].clone(),
                _ => return,
            },
            _ => return,
        };
        cx.update(|cx| handle.update(cx, |_, window, cx| running(format!("正在导出 {} 的审计材料…", app.display_name()), window, cx))).ok();
        let options = Options {
            replace_existing: false,
            reuse_if_unchanged: true,
            redact: settings.redact_exports,
            include_raw: settings.export_include_raw,
            include_source: settings.export_include_source,
            ..Options::default()
        };
        let retry_app = app.clone();
        let retry_context = context.clone();
        let task = cx.background_executor().spawn(async move {
            export(&app, &parent, &options, &context, &|_| {})
                .and_then(|result| material_summary(&result).map_err(wxlocated_engine::material::export::MaterialExportError::Io))
                .map_err(|error| error.to_string())
        });
        let outcome = task.await;
        cx.update(|cx| handle.update(cx, |_, window, cx| match outcome {
            Ok((root, summary)) => material_done(summary, root, window, cx),
            Err(message) => {
                window.push_notification(Notification::error(message).title("导出失败，已有材料未被覆盖")
                    .id::<ExportNote>().autohide(false)
                    .action(move |_, _, _| {
                        let app = retry_app.clone();
                        let context = retry_context.clone();
                        Button::new("retry-material-folder").label("选择其他目录…").ghost()
                            .on_click(move |_, window, cx| start_material_export(app.clone(), context.clone(), true, window, cx))
                    }), cx);
            }
        }))
        .ok();
    })
    .detach();
}

fn material_done(summary: String, root: PathBuf, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::new().with_type(NotificationType::Success).title("审计材料").id::<ExportNote>().autohide(false)
        .content(move |_, _, _| {
            let open = root.clone();
            let copy = root.clone();
            gpui_kit::base::v_flex().gap_2()
                .child(gpui_kit::div().id("material-summary").role(Role::Status).aria_label(summary.clone()).child(summary.clone()))
                .child(gpui_kit::base::h_flex().gap_1()
                    .child(Button::new("open-material").label("打开目录").ghost().on_click(move |_, _, cx| cx.open_with_system(&open)))
                    .child(Button::new("copy-material-path").label("复制材料路径").ghost().on_click(move |_, _, cx| crate::ui::copy(copy.to_string_lossy(), cx))))
                .into_any_element()
        }), cx);
}

/// Reads only the completed export, including reused material, off the UI thread.
fn material_summary(result: &serde_json::Value) -> Result<(PathBuf, String), String> {
    let root = PathBuf::from(result["root"].as_str().ok_or("导出结果缺少材料路径")?);
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("material.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let extension = &report["wxlocated"];
    let mut packages = extension["packages"].as_array().cloned().unwrap_or_default();
    let mut source_directories = Vec::new();
    if let Some(source) = extension["reconstruction"]["source_directory"].as_str() {
        source_directories.push(root.join(source));
    }
    for plugin in extension["plugins"].as_array().into_iter().flatten() {
        packages.extend(plugin["packages"].as_array().into_iter().flatten().cloned());
        if let Some(source) = plugin["source_directory"].as_str() {
            source_directories.push(root.join(source));
        }
    }
    let raw = packages.iter().filter_map(|p| p["raw_path"].as_str()).filter(|path| root.join(path).is_file()).count();
    let source = source_directories.iter().try_fold(0, |count, directory| count_files(directory).map(|files| count + files))?;
    let layers = extension["semantic_layers"].as_array().map_or(0, Vec::len);
    let missing_subpackages = report["subpackages"]["missing"].as_array().map_or(0, Vec::len);
    let missing_plugins = report["plugins"]["issues"].as_array().map_or(0, Vec::len);
    let missing = if missing_subpackages > 0 || missing_plugins > 0 {
        format!("缺 {missing_subpackages} 个分包、{missing_plugins} 个插件")
    } else if report["completeness"].as_str() == Some("COMPLETE") {
        "材料完整".into()
    } else {
        "主包或材料存在问题，详见 material.json".into()
    };
    let state = if result["reused"].as_bool() == Some(true) { "已复用已有材料" } else { "已新生成材料" };
    Ok((root, format!("{state} · raw 原始包 {raw} 个 · source 文件 {source} 个（含插件与清单） · Agent 层 {layers} 个 · {missing}")))
}

fn count_files(directory: &Path) -> Result<usize, String> {
    let mut count = 0;
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() {
            count += count_files(&entry.path())?;
        } else if kind.is_file() {
            count += 1;
        }
    }
    Ok(count)
}

/// Asks where to save, then writes `bytes` there in the background.
pub fn save_bytes<V: 'static>(suggested: String, bytes: impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static, window: &mut Window, cx: &mut Context<V>) {
    let directory = downloads();
    let path = cx.prompt_for_new_path(&directory, Some(&suggested));
    cx.spawn_in(window, async move |this, cx| {
        let Ok(Ok(Some(path))) = path.await else { return };
        let target = path.clone();
        let outcome = cx.background_executor().spawn(async move { bytes().and_then(|data| std::fs::write(&target, data).map_err(|e| e.to_string())) }).await;
        this.update_in(cx, |_, window, cx| match outcome {
            Ok(()) => done(format!("已导出 {}", file_name(&path)), Some(path), window, cx),
            Err(message) => failed(message, window, cx),
        })
        .ok();
    })
    .detach();
}

/// Asks for a parent folder, then runs `work` with it in the background;
/// `work` returns the folder it created.
pub fn into_folder<V: 'static>(
    progress: &'static str,
    work: impl FnOnce(&Path) -> Result<PathBuf, String> + Send + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("导出到这里".into()) });
    cx.spawn_in(window, async move |this, cx| {
        let Ok(Ok(Some(paths))) = paths.await else { return };
        let Some(parent) = paths.into_iter().next() else { return };
        this.update_in(cx, |_, window, cx| running(progress, window, cx)).ok();
        let outcome = cx.background_executor().spawn(async move { work(&parent) }).await;
        this.update_in(cx, |_, window, cx| match outcome {
            Ok(folder) => done(format!("已导出到 {}", file_name(&folder)), Some(folder), window, cx),
            Err(message) => failed(message, window, cx),
        })
        .ok();
    })
    .detach();
}

/// The main `.wxapkg`, copied byte for byte.
pub fn export_main_package<V: 'static>(app: &Miniapp, window: &mut Window, cx: &mut Context<V>) {
    let source = app.main_package.clone();
    let name: String = app.display_name().chars().map(|c| if "/\\:".contains(c) { '_' } else { c }).collect();
    save_bytes(format!("{name}.wxapkg"), move || std::fs::read(&source).map_err(|e| e.to_string()), window, cx);
}

fn downloads() -> PathBuf {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Downloads")).filter(|p| p.is_dir()).unwrap_or_else(|| PathBuf::from("/"))
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
