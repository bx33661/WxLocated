//! One window per mini program: the rebuilt source as a file tree; a code
//! view that switches between the rebuild, the raw package bytes and any
//! Agent semantic layer; an overview of the material; and an inspector.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tree::TreeState;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use serde_json::Value;
use wxlocated_engine::archive::{Archive, PackageSet};
#[cfg(all(feature = "snapshot", target_os = "macos"))]
use wxlocated_engine::decompile::Artifact;
use wxlocated_engine::decompile::{Confidence, Decompilation, decompile_reporting};
use wxlocated_engine::material::report::{self, Analysis, Inspection, ReportInput};
use wxlocated_engine::material::Context as MaterialContext;
use wxlocated_engine::overlay::{self, Layer as OverlayLayer};
use wxlocated_engine::scan::Miniapp;

use crate::library::{Library, RebuildSummary};
use crate::ui;

mod inspector;
mod layout;
mod overview;
mod preview;

use preview::{Preview, ReadingKey, ReadingState, View};

/// Everything the window shows, built once per package choice in the background.
struct Loaded {
    set: PackageSet,
    result: Decompilation,
    analysis: Analysis,
    inspections: Vec<Inspection>,
    report: Value,
    seconds: f64,
    summary: RebuildSummary,
    /// Artifact index by path.
    index: HashMap<String, usize>,
}

enum Phase {
    Loading,
    Failed(String),
    Ready(Arc<Loaded>),
}

/// What the window reports while it loads: opening, checking, then each
/// decompile stage in order (`Stage` as usize), then the report.
const STEPS: &[&str] = &["读取包", "校验结构", "建立索引", "还原配置", "拆分脚本", "还原 WXS", "还原页面结构", "还原样式", "整理产物", "生成清单"];

/// A package choice: everything together, or one `.wxapkg` on its own.
struct PackageChoice {
    title: String,
    path: Option<PathBuf>,
}

pub struct Workspace {
    app: Miniapp,
    packages: Vec<PackageChoice>,
    package: usize,
    phase: Phase,
    started: Instant,
    /// Index into `STEPS` while loading.
    step: usize,
    layers: Vec<OverlayLayer>,
    tree: Entity<TreeState>,
    filter: Entity<InputState>,
    focus: FocusHandle,
    tree_focus: FocusHandle,
    /// Path of the selected artifact; `None` shows the overview.
    selected: Option<String>,
    view: View,
    compare: bool,
    preview: Preview,
    /// Bumped per preview request; late results for older requests are dropped.
    preview_generation: u64,
    /// The user asked to open a file above `LARGE_TEXT` anyway.
    open_large: bool,
    readings: HashMap<ReadingKey, ReadingState>,
    last_views: HashMap<(usize, usize), (View, bool)>,
    visible_files: usize,
    evidence: Option<Value>,
    copying_evidence: bool,
    agent_diff: Option<(usize, usize)>,
    pending_imports: VecDeque<PathBuf>,
    importing: bool,
    show_inspector: bool,
    show_containers: bool,
    show_hashes: bool,
    /// Inspector sections folded by the user.
    collapsed: std::collections::HashSet<&'static str>,
    copied: Option<&'static str>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the app's window, or focuses it when it is already open.
pub fn open_app_window(app_id: &str, cx: &mut App) {
    let library = Library::global(cx);
    if let Some(handle) = library.read(cx).app_windows.get(app_id).copied()
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let Some(app) = library.read(cx).app(app_id).cloned() else { return };
    let title = format!("{} · {}", app.display_name(), app.app_id);
    let options = crate::theme::window_options(&title, Bounds::centered(None, size(px(1320.), px(840.)), cx), Some(size(px(900.), px(560.))), 52.);
    let id = app.app_id.clone();
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        crate::theme::follow_appearance(window, cx);
        cx.new(|cx| Workspace::new(app, window, cx))
    }) {
        library.update(cx, |library, _| {
            library.app_windows.insert(id, handle);
        });
    }
}

/// Native file-open events can arrive before the first cache scan or rebuild.
pub fn open_overlay_folder(work: PathBuf, cx: &mut App) {
    let manifest = match overlay::read_manifest_strict(&work) {
        Ok(manifest) => manifest,
        Err(error) => { crate::native_open::report_error(error.to_string(), cx); return; }
    };
    let id = manifest.base.app_id;
    let library = Library::global(cx);
    if library.read(cx).app(&id).is_none() && library.read(cx).snapshot.is_none() {
        library.update(cx, |library, cx| library.refresh(cx));
        cx.spawn(async move |cx| {
            for _ in 0..300 {
                let ready = library.read_with(cx, |library, _| !library.scanning);
                if ready {
                    cx.update(|cx| open_overlay_folder(work, cx));
                    return;
                }
                cx.background_executor().timer(Duration::from_millis(100)).await;
            }
            cx.update(|cx| crate::native_open::report_error("缓存扫描尚未完成，请稍后重新打开重建层。", cx));
        }).detach();
        return;
    }
    if library.read(cx).app(&id).is_none() {
        crate::native_open::report_error(format!("本机没有 {id} 的基础包；请先打开对应小程序或原始包，再导入重建层。"), cx);
        return;
    }
    open_app_window(&id, cx);
    let handle = library.read(cx).app_windows.get(&id).copied();
    if let Some(root) = handle.and_then(|handle| handle.downcast::<gpui_kit::base::Root>()) {
        root.update(cx, |root, window, cx| {
            if let Ok(workspace) = root.view().clone().downcast::<Workspace>() {
                workspace.update(cx, |this, cx| {
                    if !this.pending_imports.contains(&work) {
                        this.pending_imports.push_back(work);
                    }
                    this.start_next_import(window, cx);
                });
            }
        }).ok();
    }
}

impl Workspace {
    fn new(app: Miniapp, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(ui::shortcut("筛选文件  ⇧⌘O")));
        let subscriptions = vec![
            cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.rebuild_tree(cx);
                }
            }),
            cx.observe_in(&tree, window, |this, tree, window, cx| {
                let path = tree.read(cx).selected_entry().filter(|entry| !entry.is_folder()).map(|entry| entry.item().id.to_string());
                if let Some(path) = path
                    && this.selected.as_deref() != Some(&path)
                {
                    this.select(Some(path), window, cx);
                }
            }),
        ];
        let mut packages = Vec::new();
        let all = app.all_packages();
        if all.len() > 1 {
            packages.push(PackageChoice { title: format!("全部包 · 主包与 {} 个分包", all.len() - 1), path: None });
        }
        for (index, path) in all.iter().enumerate() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            packages.push(PackageChoice { title: format!("{} · {name}", if index == 0 { "主包" } else { "分包" }), path: (all.len() > 1).then(|| path.clone()) });
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut workspace = Self {
            app,
            packages,
            package: 0,
            phase: Phase::Loading,
            started: Instant::now(),
            step: 0,
            layers: Vec::new(),
            tree,
            filter,
            focus,
            tree_focus: cx.focus_handle(),
            selected: None,
            view: View::Rebuilt,
            compare: false,
            preview: Preview::None,
            preview_generation: 0,
            open_large: false,
            readings: HashMap::new(),
            last_views: HashMap::new(),
            visible_files: 0,
            evidence: None,
            copying_evidence: false,
            agent_diff: None,
            pending_imports: VecDeque::new(),
            importing: false,
            show_inspector: true,
            show_containers: false,
            show_hashes: false,
            collapsed: std::collections::HashSet::new(),
            copied: None,
            _subscriptions: subscriptions,
        };
        workspace.load(window, cx);
        workspace
    }

    fn context(&self, cx: &App) -> MaterialContext {
        Library::global(cx).read(cx).snapshot.as_ref().map(|s| MaterialContext::for_app(s, &self.app)).unwrap_or_default()
    }

    /// Opens, checks and rebuilds the chosen package(s) in the background.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.phase = Phase::Loading;
        self.started = Instant::now();
        self.step = 0;
        self.selected = None;
        self.view = View::Rebuilt;
        self.preview = Preview::None;
        self.preview_generation += 1;
        self.evidence = None;
        self.agent_diff = None;
        cx.notify();
        let app = self.app.clone();
        let context = self.context(cx);
        let path = self.packages.get(self.package).and_then(|p| p.path.clone());
        let is_full_app = path.is_none();
        let (progress, steps) = async_channel::unbounded::<usize>();
        cx.spawn(async move |this, cx| {
            while let Ok(step) = steps.recv().await {
                if this.update(cx, |this, cx| {
                    this.step = step;
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let task = cx.background_executor().spawn(async move {
            let _ = progress.try_send(0);
            let set = match path {
                None => report::open_app(&app),
                Some(path) => Archive::open_with_app_id(&path, Some(&app.app_id)).map(PackageSet::standalone),
            }
            .map_err(|error| error.to_string())?;
            let _ = progress.try_send(1);
            let analysis = report::analyze(&set);
            let inspections = report::inspect_app(&app);
            let started = Instant::now();
            let result = decompile_reporting(&set, &|stage| {
                let _ = progress.try_send(2 + stage as usize);
            });
            let _ = progress.try_send(STEPS.len() - 1);
            let seconds = started.elapsed().as_secs_f64();
            let report = report::build(&ReportInput {
                app: &app,
                analysis: Some(&analysis),
                inspections: &inspections,
                reconstruction: Some((&result, seconds)),
                archive_error: None,
                archive_notes: &set.notes,
                context: &context,
            });
            let mut summary = RebuildSummary { generated: 0, degraded: 0 };
            let mut index = HashMap::with_capacity(result.artifacts.len());
            for (position, artifact) in result.artifacts.iter().enumerate() {
                summary.generated += usize::from(artifact.content.generated_text().is_some());
                summary.degraded += usize::from(artifact.confidence <= Confidence::Medium);
                index.insert(artifact.path.clone(), position);
            }
            Ok::<_, String>(Loaded { set, result, analysis, inspections, report, seconds, summary, index })
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                this.phase = match outcome {
                    Ok(loaded) => {
                        if is_full_app {
                            let id = this.app.app_id.clone();
                            Library::global(cx).update(cx, |library, cx| library.record_rebuild(&id, loaded.summary, cx));
                        }
                        Phase::Ready(Arc::new(loaded))
                    }
                    Err(message) => Phase::Failed(message),
                };
                this.reload_layers();
                this.rebuild_tree(cx);
                cx.notify();
            })
            .ok();
            // Reopen the file that was showing when the window last closed.
            this.update_in(cx, |this, window, cx| {
                let last = Library::global(cx).read(cx).settings.last_files.get(&this.app.app_id).cloned();
                if this.selected.is_none()
                    && let Some(path) = last
                    && this.artifact(&path).is_some()
                {
                    this.select(Some(path), window, cx);
                }
                this.start_next_import(window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn loaded(&self) -> Option<Arc<Loaded>> {
        match &self.phase {
            Phase::Ready(loaded) => Some(loaded.clone()),
            _ => None,
        }
    }

    fn artifact(&self, path: &str) -> Option<(Arc<Loaded>, usize)> {
        let loaded = self.loaded()?;
        let index = *loaded.index.get(path)?;
        Some((loaded, index))
    }

    /// Package file name → SHA-256, which overlays are checked against.
    fn package_hashes(&self) -> HashMap<String, String> {
        self.loaded()
            .map(|loaded| {
                loaded
                    .inspections
                    .iter()
                    .filter_map(|i| Some((i.path.file_name()?.to_string_lossy().into_owned(), i.sha256.clone()?)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn reload_layers(&mut self) {
        self.layers = overlay::matching_layers(&self.app.app_id, self.app.version.as_deref(), &self.package_hashes());
        if let View::Agent(index) = self.view
            && index >= self.layers.len()
        {
            self.view = View::Rebuilt;
        }
    }

    fn remove_layer(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layer) = self.layers.get(index) else { return };
        let name = layer.manifest.name.clone();
        if let Err(error) = std::fs::remove_dir_all(&layer.directory) {
            window.push_notification(Notification::error(error.to_string()).title("无法删除重建层"), cx);
            return;
        }
        self.readings.retain(|key, _| !matches!(key.2, View::Agent(_)));
        self.last_views.retain(|_, value| !matches!(value.0, View::Agent(_)));
        self.reload_layers();
        if matches!(self.view, View::Agent(_)) {
            self.view = View::Rebuilt;
            self.refresh_preview(window, cx);
        }
        window.push_notification(Notification::info(format!("已删除 Agent 重建层「{name}」")), cx);
        cx.notify();
    }

    fn start_next_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing || self.pending_imports.is_empty() { return; }
        if matches!(self.phase, Phase::Loading) { return; }
        if self.loaded().is_none() {
            self.pending_imports.clear();
            window.push_notification(Notification::error("基础包无法读取，请重新打开有效的原始包后导入。"), cx);
            return;
        }
        let Some(work) = self.pending_imports.pop_front() else { return };
        self.importing = true;
        let (id, version, hashes) = (self.app.app_id.clone(), self.app.version.clone(), self.package_hashes());
        let task = cx.background_executor().spawn(async move { overlay::import(&work, &id, version.as_deref(), &hashes, None) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.importing = false;
                match result {
                    Ok(result) => {
                        this.readings.retain(|key, _| !matches!(key.2, View::Agent(_)));
                        this.last_views.retain(|_, value| !matches!(value.0, View::Agent(_)));
                        this.reload_layers();
                        if matches!(this.view, View::Agent(_)) {
                            this.view = View::Rebuilt;
                            this.compare = false;
                            this.refresh_preview(window, cx);
                        }
                        window.push_notification(Notification::success(format!("已导入 Agent 重建层「{}」· {} 个文件", result.name, result.changed_files.len() + result.added_files.len())), cx);
                    }
                    Err(error) => window.push_notification(Notification::error(error.to_string()).title("无法导入重建层"), cx),
                }
                this.start_next_import(window, cx);
                cx.notify();
            }).ok();
        }).detach();
    }
}

fn kind_title(kind: wxlocated_engine::decompile::ArtifactKind) -> &'static str {
    use wxlocated_engine::decompile::ArtifactKind::*;
    match kind {
        Configuration => "配置",
        Script => "脚本",
        Wxs => "WXS",
        Markup => "页面结构",
        Style => "样式",
        Image => "图片",
        Font => "字体",
        Media => "媒体",
        Text => "文本",
        Binary => "二进制",
    }
}

#[cfg(all(feature = "snapshot", target_os = "macos"))]
pub const SNAPSHOT_STEPS: &[&str] = &["file", "raw", "image", "overview"];

#[cfg(all(feature = "snapshot", target_os = "macos"))]
pub fn snapshot_ready(handle: AnyWindowHandle, cx: &mut AsyncApp) -> bool {
    cx.update(|cx| {
        handle
            .downcast::<gpui_kit::base::Root>()
            .and_then(|root| root.update(cx, |root, _, cx| root.view().clone().downcast::<Workspace>().ok().map(|w| w.read(cx).loaded().is_some())).ok())
            .flatten()
            .unwrap_or(true)
    })
}

#[cfg(all(feature = "snapshot", target_os = "macos"))]
pub fn snapshot_step(handle: AnyWindowHandle, step: &str, cx: &mut AsyncApp) {
    let step = step.to_string();
    cx.update(|cx| {
        let Some(root) = handle.downcast::<gpui_kit::base::Root>() else { return };
        root.update(cx, |root, window, cx| {
            let Ok(view) = root.view().clone().downcast::<Workspace>() else { return };
            view.update(cx, |this, cx| {
                let Some(loaded) = this.loaded() else { return };
                let pick = |predicate: &dyn Fn(&Artifact) -> bool| loaded.result.artifacts.iter().find(|a| predicate(a)).map(|a| a.path.clone());
                match step.as_str() {
                    "file" => {
                        let path = pick(&|a| a.path.ends_with(".wxml") && a.confidence == Confidence::High && a.content.generated_text().is_some_and(|t| t.len() > 400));
                        this.select(path, window, cx);
                    }
                    "raw" => this.set_view(View::Raw, window, cx),
                    "overview" => this.select(None, window, cx),
                    "image" => {
                        let path = pick(&|a| a.path.ends_with(".png") && !a.path.starts_with("__"));
                        this.select(path, window, cx);
                    }
                    _ => {}
                }
            });
        })
        .ok();
    });
}
