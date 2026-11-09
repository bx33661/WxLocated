//! One window per mini program: the rebuilt source as a file tree; a code
//! view that switches between the rebuild, the raw package bytes and any
//! Agent semantic layer; an overview of the material; and an inspector.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, GutterDecoration, Input, InputEvent, InputState, OverviewRulerDecoration, OverviewRulerLane, Position, RangeDecoration,
    RangeDecorationStyle,
};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tree::{TreeItem, TreeState, tree};
use gpui_kit::component::{Selectable as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::Anchor;
use serde_json::Value;
use wxlocated_engine::archive::{Archive, PackageSet};
use wxlocated_engine::decompile::{Artifact, Confidence, Content, Decompilation, Severity, decompile_reporting};
use wxlocated_engine::export::confidence_title;
use wxlocated_engine::material::report::{self, Analysis, Inspection, Integrity, ReportInput};
use wxlocated_engine::material::{Completeness, Context as MaterialContext};
use wxlocated_engine::overlay::{self, Layer as OverlayLayer, OverlayFile};
use wxlocated_engine::scan::Miniapp;

use crate::library::{Library, RebuildSummary, image_format};
use crate::theme::{Palette, palette, soft_shadow};
use crate::ui::{self, app_icon, badge, chip, file_icon, file_tint, format_size, format_time, section_label};
use crate::workspace_actions::*;

/// Everything the window shows, built once per package choice in the background.
struct Loaded {
    set: PackageSet,
    result: Decompilation,
    analysis: Analysis,
    inspections: Vec<Inspection>,
    report: Value,
    seconds: f64,
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

/// Which layer of the selected file is shown.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum View {
    Rebuilt,
    Raw,
    /// Index into `Workspace::layers`.
    Agent(usize),
}

enum Preview {
    None,
    /// Reading in the background.
    Loading,
    Code(Entity<EditorState>),
    Image(Arc<Image>, Option<(u32, u32)>),
    Binary(u64),
    /// Text too large to open without asking (size in bytes).
    TooLarge(u64),
    Error(String),
}

/// Text above this opens only on request: the editor lays it out on the
/// main thread, and multi-megabyte compiled containers would stall the UI.
const LARGE_TEXT: usize = 3 * 1024 * 1024;

/// A preview worked out in the background, ready to show.
enum Prepared {
    Image(Vec<u8>, ImageFormat, Option<(u32, u32)>),
    Text {
        text: String,
        language: &'static str,
        highlight: Option<std::ops::Range<usize>>,
        evidence: Option<Value>,
        agent_diff: Option<(usize, usize)>,
    },
    Binary(u64),
    TooLarge(u64),
    Error(String),
}

type ReadingKey = (usize, usize, View, bool);

/// Small per-view bookmarks; only the visible editor retains source text.
#[derive(Clone)]
struct ReadingState {
    selection: std::ops::Range<usize>,
    reversed: bool,
    scroll: Point<Pixels>,
    query: String,
    search_open: bool,
    search_active: bool,
    case_insensitive: bool,
    search_match: usize,
    open_large: bool,
}

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
            library.app_windows.insert(id.clone(), handle);
        });
        cx.on_window_closed(move |cx, closed| {
            if closed == handle.window_id() {
                Library::global(cx).update(cx, |library, _| {
                    library.app_windows.remove(&id);
                });
            }
        })
        .detach();
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
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("筛选文件  ⇧⌘O"));
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
            let index = result.artifacts.iter().enumerate().map(|(i, a)| (a.path.clone(), i)).collect();
            Ok::<_, String>(Loaded { set, result, analysis, inspections, report, seconds, index })
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                this.phase = match outcome {
                    Ok(loaded) => {
                        let summary = RebuildSummary {
                            generated: loaded.result.artifacts.iter().filter(|a| a.content.generated_text().is_some()).count(),
                            degraded: loaded.result.artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).count(),
                        };
                        let id = this.app.app_id.clone();
                        Library::global(cx).update(cx, |library, cx| library.record_rebuild(&id, summary, cx));
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

    fn reading_key(&self) -> Option<ReadingKey> {
        let path = self.selected.as_deref()?;
        let loaded = self.loaded()?;
        Some((self.package, *loaded.index.get(path)?, self.view, self.compare))
    }

    fn remember_reading(&mut self, cx: &App) {
        let Some(key) = self.reading_key() else { return };
        self.last_views.insert((key.0, key.1), (self.view, self.compare));
        if let Preview::Code(editor) = &self.preview {
            let state = editor.read(cx);
            let search = state.search_session();
            self.readings.insert(key, ReadingState {
                selection: state.selected_range(),
                reversed: state.is_selection_reversed(),
                scroll: state.scroll_offset(),
                query: search.query.clone(),
                search_open: search.open,
                search_active: search.is_active(),
                case_insensitive: search.case_insensitive,
                search_match: search.matcher.current_match_index(),
                open_large: self.open_large,
            });
        }
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded() else {
            self.visible_files = 0;
            self.tree.update(cx, |tree, cx| tree.set_items(Vec::new(), cx));
            return;
        };
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let paths: Vec<&str> = loaded.result.artifacts.iter().map(|a| a.path.as_str())
            .filter(|p| self.show_containers || !p.starts_with("__wxlocated_"))
            .filter(|p| query.is_empty() || p.to_lowercase().contains(&query)).collect();
        self.visible_files = paths.len();
        let items = build_tree(&paths, !query.is_empty() || paths.len() <= 40);
        let selected = self.selected.as_deref().filter(|path| paths.contains(path)).map(|path| TreeItem::new(path.to_string(), path.to_string()));
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            if let Some(item) = selected.as_ref() {
                tree.set_selected_item(Some(item), cx);
            }
        });
        cx.notify();
    }

    fn select(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_reading(cx);
        if self.selected != path {
            let saved = path.as_deref().and_then(|p| self.artifact(p)).and_then(|(_, index)| self.last_views.get(&(self.package, index)).copied());
            (self.view, self.compare) = saved.unwrap_or((View::Rebuilt, false));
            if let View::Agent(index) = self.view
                && path.as_deref().and_then(|path| self.layer_file(index, path)).is_none()
            {
                self.view = View::Rebuilt;
                self.compare = false;
            }
            self.open_large = false;
        }
        self.selected = path;
        let (id, path) = (self.app.app_id.clone(), self.selected.clone());
        Library::global(cx).update(cx, |library, cx| {
            let changed = match path {
                Some(path) => library.settings.last_files.insert(id, path.clone()).as_deref() != Some(path.as_str()),
                None => library.settings.last_files.remove(&id).is_some(),
            };
            if changed {
                library.save_settings(cx);
            }
        });
        self.refresh_preview(window, cx);
    }

    fn set_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_reading(cx);
        self.view = view;
        self.compare = false;
        self.refresh_preview(window, cx);
    }

    /// The raw entry behind a rebuilt file, when it differs from it.
    fn raw_entry<'a>(loaded: &'a Loaded, artifact: &Artifact) -> Option<&'a wxlocated_engine::archive::SetEntry> {
        artifact.content.generated_text()?;
        let source = artifact.sources.first()?;
        loaded.set.entries.iter().find(|e| e.path == source.entry_path && loaded.set.source_title(e) == source.package)
    }

    fn layer_file<'a>(&'a self, index: usize, base: &str) -> Option<(&'a OverlayLayer, &'a OverlayFile, PathBuf)> {
        let layer = self.layers.get(index)?;
        let file = layer.manifest.files.iter().find(|f| f.base.as_deref() == Some(base))?;
        Some((layer, file, layer.directory.join(&file.path)))
    }

    /// The selected file's bytes in the current view.
    fn current_bytes(&self, loaded: &Loaded, artifact: &Artifact) -> Result<Vec<u8>, String> {
        let layer = self.layer_path(&artifact.path);
        read_view(loaded, artifact, self.view, layer.as_deref())
    }

    /// The Agent layer's file replacing `path`, in the Agent view.
    fn layer_path(&self, path: &str) -> Option<PathBuf> {
        match self.view {
            View::Agent(index) => self.layer_file(index, path).map(|(_, _, path)| path),
            _ => None,
        }
    }

    fn refresh_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_generation += 1;
        self.evidence = None;
        self.agent_diff = None;
        let generation = self.preview_generation;
        let Some(path) = self.selected.clone() else {
            self.preview = Preview::None;
            cx.notify();
            return;
        };
        let Some((loaded, index)) = self.artifact(&path) else { return };
        if let Some(saved) = self.reading_key().and_then(|key| self.readings.get(&key)) {
            self.open_large |= saved.open_large;
        }
        self.preview = Preview::Loading;
        cx.notify();
        let (view, compare, force) = (self.view, self.compare, self.open_large);
        let layer = self.layer_path(&path);
        // Reading (and decrypting) the entry, the diff and the source range
        // all happen off the main thread; only the editor is built here.
        let task = cx.background_executor().spawn(async move {
            prepare(&loaded, index, &path, view, compare, force, layer.as_deref())
        });
        cx.spawn_in(window, async move |this, cx| {
            let prepared = task.await;
            this.update_in(cx, |this, window, cx| {
                if this.preview_generation != generation {
                    return;
                }
                this.preview = this.show(prepared, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show(&mut self, prepared: Prepared, window: &mut Window, cx: &mut Context<Self>) -> Preview {
        match prepared {
            Prepared::Image(bytes, format, dimensions) => Preview::Image(Arc::new(Image::from_bytes(format, bytes)), dimensions),
            Prepared::Binary(size) => Preview::Binary(size),
            Prepared::TooLarge(size) => Preview::TooLarge(size),
            Prepared::Error(message) => Preview::Error(message),
            Prepared::Text { text, language, highlight, evidence, agent_diff } => {
                self.evidence = evidence;
                self.agent_diff = agent_diff;
                let saved = self.reading_key().and_then(|key| self.readings.get(&key)).cloned();
                let highlight_lines = highlight.as_ref().map(|range| (
                    text[..range.start].bytes().filter(|byte| *byte == b'\n').count() as u32,
                    text.bytes().filter(|byte| *byte == b'\n').count() as u32,
                ));
                let editor = cx.new(|cx| EditorState::new(window, cx).language(language).searchable(true).soft_wrap(false).default_value(text));
                if let Some(range) = highlight {
                    let (line, total) = highlight_lines.unwrap();
                    let accent = crate::theme::palette(cx).accent;
                    editor.update(cx, |state, cx| {
                        if saved.is_none() {
                            state.set_cursor_position(Position::new((line + 28).min(total), 0), window, cx);
                            state.set_cursor_position(Position::new(line, 0), window, cx);
                        }
                        let _ = state.create_range_decorations_collection(vec![RangeDecoration::new(range.clone()).with_style(RangeDecorationStyle::Fill).with_color(accent.opacity(0.13))], cx);
                        let _ = state.create_gutter_decorations_collection(vec![GutterDecoration::new(range.clone()).with_color(accent)], cx);
                        let _ = state.create_overview_ruler_decorations_collection(vec![OverviewRulerDecoration::new(range).with_lane(OverviewRulerLane::Left).with_color(accent)], cx);
                    });
                }
                if let Some(saved) = saved {
                    editor.update(cx, |state, cx| {
                        if saved.search_open {
                            state.open_search(false, cx);
                        }
                        state.set_search_query(saved.query, saved.case_insensitive, cx);
                        let count = state.search_session().matcher.len();
                        let target = saved.search_match.min(count.saturating_sub(1));
                        // The dependency exposes navigation but no session setter.
                        // Replay the shorter direction, before layout, then restore the viewport.
                        if target <= count / 2 {
                            for _ in 0..target { state.next_search_match(cx); }
                        } else {
                            for _ in 0..count - target { state.previous_search_match(cx); }
                        }
                        if !saved.search_active {
                            state.close_search(cx);
                        }
                        state.set_selected_range_with_direction(saved.selection, saved.reversed, cx);
                        state.set_scroll_offset(saved.scroll, cx);
                    });
                }
                Preview::Code(editor)
            }
        }
    }

    // MARK: Actions

    fn export_material(&mut self, _: &ExportMaterial, window: &mut Window, cx: &mut Context<Self>) {
        let context = self.context(cx);
        crate::export::export_material(self.app.clone(), context, window, cx);
    }

    fn show_overview(&mut self, _: &ShowOverview, window: &mut Window, cx: &mut Context<Self>) {
        self.select(None, window, cx);
    }

    fn toggle_raw(&mut self, _: &ToggleRaw, window: &mut Window, cx: &mut Context<Self>) {
        let has_raw = self.selected.as_ref().and_then(|p| self.artifact(p)).is_some_and(|(l, i)| Self::raw_entry(&l, &l.result.artifacts[i]).is_some());
        if !has_raw {
            return;
        }
        let next = if self.view == View::Raw { View::Rebuilt } else { View::Raw };
        self.set_view(next, window, cx);
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.show_inspector = !self.show_inspector;
        cx.notify();
    }

    fn back_to_gallery(&mut self, _: &BackToGallery, window: &mut Window, cx: &mut Context<Self>) {
        crate::focus_gallery(cx);
        window.remove_window();
    }

    fn rebuild(&mut self, _: &Rebuild, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_reading(cx);
        self.load(window, cx);
    }

    fn focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |input, cx| input.focus(window, cx));
    }

    fn choose_package(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index != self.package {
            self.remember_reading(cx);
            self.package = index;
            self.load(window, cx);
        }
    }

    fn export_current_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((loaded, index)) = self.selected.as_ref().and_then(|p| self.artifact(p)) else { return };
        let artifact = &loaded.result.artifacts[index];
        let name = artifact.path.rsplit('/').next().unwrap_or(&artifact.path).to_string();
        let bytes = self.current_bytes(&loaded, artifact);
        crate::export::save_bytes(name, move || bytes, window, cx);
    }

    fn export_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded() else { return };
        let folder = [Some(self.app.display_name().to_string()), self.app.version.clone(), Some("重建源码".into())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("-");
        crate::export::into_folder(
            "正在导出重建源码…",
            move |parent| {
                wxlocated_engine::export::export_source(&loaded.result, loaded.seconds, &loaded.set, parent, &folder).map_err(|e| e.to_string())
            },
            window,
            cx,
        );
    }

    fn copy_evidence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((loaded, index)) = self.selected.as_ref().and_then(|path| self.artifact(path)) else { return };
        let (reading, line) = match &self.preview {
            Preview::Code(editor) => {
                let state = editor.read(cx);
                (Some(state.text().clone()), (!self.compare).then_some(i64::from(state.cursor_position().line) + 1))
            }
            _ => (None, None),
        };
        let (view, generation) = (self.view, self.preview_generation);
        self.copying_evidence = true;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let reading = reading.map(|text| text.to_string()).unwrap_or_default();
            build_evidence(&loaded, index, view, &reading, line, true)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.copying_evidence = false;
                if this.preview_generation != generation {
                    window.push_notification(Notification::info("文件已切换，请在当前文件重新复制证据引用。"), cx);
                    cx.notify();
                    return;
                }
                match result {
                    Ok(evidence) => {
                        let method = evidence_method(&evidence);
                        match serde_json::to_string_pretty(&evidence) {
                            Ok(text) => {
                                ui::copy(text, cx);
                                window.push_notification(Notification::success(format!("已复制证据引用 · {method}")), cx);
                                this.evidence = Some(evidence);
                            }
                            Err(error) => window.push_notification(Notification::error(error.to_string()).title("无法复制证据"), cx),
                        }
                    }
                    Err(error) => window.push_notification(Notification::error(error).title("无法复制证据"), cx),
                }
                cx.notify();
            }).ok();
        }).detach();
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

    fn on_drop(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        crate::native_open::open_paths(paths.paths(), cx);
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let filter_focused = self.filter.focus_handle(cx).is_focused(window);
        if filter_focused {
            match event.keystroke.key.as_str() {
                "escape" => {
                    self.filter.update(cx, |input, cx| input.set_value("", window, cx));
                    self.tree.update(cx, |tree, cx| tree.focus(window, cx));
                    cx.stop_propagation();
                }
                "enter" | "down" => {
                    self.tree.update(cx, |tree, cx| {
                        if tree.selected_index().is_none() && tree.entry(0).is_some() {
                            tree.set_selected_index(Some(0), cx);
                        }
                        tree.focus(window, cx);
                    });
                    cx.stop_propagation();
                }
                _ => {}
            }
        } else if self.tree_focus.contains_focused(window, cx)
            && matches!(event.keystroke.key.as_str(), "enter" | "space")
        {
            let path = self.tree.read(cx).selected_entry().filter(|entry| !entry.is_folder()).map(|entry| entry.item().id.to_string());
            if let Some(path) = path {
                self.select(Some(path), window, cx);
                cx.stop_propagation();
            }
        }
    }

    fn set_panel_width(&mut self, sidebar: bool, width: f32, cx: &mut Context<Self>) {
        Library::global(cx).update(cx, |library, cx| {
            if sidebar {
                library.settings.sidebar_width = width;
            } else {
                library.settings.inspector_width = width;
            }
            cx.notify();
        });
        cx.notify();
    }

    fn mark_copied(&mut self, key: &'static str, cx: &mut Context<Self>) {
        self.copied = Some(key);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            this.update(cx, |this, cx| {
                if this.copied == Some(key) {
                    this.copied = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    // MARK: Toolbar

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let (icon, ambient) = Library::global(cx).update(cx, |library, _| library.icon(&self.app));
        let has_file = self.selected.is_some();
        let ready = self.loaded().is_some();
        let package_titles: Vec<String> = self.packages.iter().map(|p| p.title.clone()).collect();
        let current_package = self.package;
        let package_label = match self.packages.get(self.package) {
            Some(choice) if choice.path.is_none() => "全部包".to_string(),
            Some(choice) => choice.title.split(" · ").nth(1).unwrap_or(&choice.title).trim_end_matches(".wxapkg").to_string(),
            None => "包".into(),
        };
        let weak = cx.entity().downgrade();
        let export_weak = weak.clone();
        ui::toolbar(52., &palette).child(
            h_flex()
            .w_full()
            .h_full()
            .gap_2()
            .child(
                Button::new("back")
                    .icon(IconName::LayoutGrid)
                    .accessibility_label("返回画廊")
                    .ghost()
                    .small()
                    .tooltip("返回画廊（⌘[）")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(BackToGallery), cx)),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(palette.hairline))
            .child(app_icon(icon, ambient, self.app.display_name(), 26., &palette, false))
            .child(
                v_flex()
                    .min_w(px(120.))
                    .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(self.app.display_name().to_string()))
                    .child(div().text_size(px(11.)).text_color(palette.secondary).child(match &self.app.version {
                        Some(version) => format!("版本 {version}"),
                        None => "版本未知".into(),
                    })),
            )
            .child(div().flex_1())
            .child(self.render_status(&palette))
            .child(div().flex_1())
            .when(self.packages.len() > 1, |this| {
                this.child(
                    Button::new("package")
                        .icon(IconName::Package)
                        .label(package_label)
                        .small()
                        .ghost()
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                            let mut menu = menu.label("切换主包、分包或全部包");
                            for (index, title) in package_titles.iter().enumerate() {
                                let weak = weak.clone();
                                menu = menu.item(PopupMenuItem::new(title.clone()).checked(index == current_package).on_click(move |_, window, cx| {
                                    weak.update(cx, |this, cx| this.choose_package(index, window, cx)).ok();
                                }));
                            }
                            menu
                        }),
                )
            })
            .child(
                Button::new("export")
                    .icon(IconName::Download)
                    .label("导出")
                    .small()
                    .tooltip("导出审计材料、当前文件或源码工程（审计材料 ⌘E）")
                    .disabled(!ready)
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        let (a, b) = (export_weak.clone(), export_weak.clone());
                        menu.item(PopupMenuItem::new("导出审计材料…  ⌘E").icon(IconName::PackageCheck).on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ExportMaterial), cx)
                        }))
                        .separator()
                        .item(PopupMenuItem::new("导出当前文件…").icon(IconName::FileDown).disabled(!has_file).on_click(move |_, window, cx| {
                            a.update(cx, |this, cx| this.export_current_file(window, cx)).ok();
                        }))
                        .item(PopupMenuItem::new("导出源码工程…").icon(IconName::FolderDown).on_click(move |_, window, cx| {
                            b.update(cx, |this, cx| this.export_project(window, cx)).ok();
                        }))
                    }),
            )
            .child(
                Button::new("inspector")
                    .icon(IconName::PanelRight)
                    .accessibility_label("显示或隐藏检查器")
                    .ghost()
                    .small()
                    .selected(self.show_inspector)
                    .tooltip("显示或隐藏检查器（⌥⌘0）")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleInspector), cx)),
            ),
        )
    }

    /// Centre readout: rebuild progress, then what was rebuilt.
    fn render_status(&self, palette: &Palette) -> AnyElement {
        let pill = h_flex().gap_2().px_3().py(px(5.)).rounded_full().bg(palette.fill).text_size(px(12.)).text_color(palette.secondary);
        match &self.phase {
            Phase::Loading => pill
                .child(Spinner::new().color(palette.accent))
                .child(format!("{}…", STEPS[self.step.min(STEPS.len() - 1)]))
                .child(div().text_color(palette.tertiary).child(format!("{}/{}", self.step + 1, STEPS.len())))
                .into_any_element(),
            Phase::Failed(_) => pill.child(div().text_color(palette.warning).child(IconName::Package)).child("原始包 · 源码不可重建").into_any_element(),
            Phase::Ready(loaded) => {
                let generated = loaded.result.artifacts.iter().filter(|a| a.content.generated_text().is_some()).count();
                let degraded = loaded.result.artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).count();
                pill.child(ui::dot(if degraded == 0 { palette.success } else { palette.warning }, 7.))
                    .child(if degraded == 0 { format!("已重建 {generated} 个文件") } else { format!("已重建 {generated} 个文件 · {degraded} 个降级") })
                    .child(div().text_color(palette.tertiary).child(format!("{:.1} 秒", loaded.seconds)))
                    .into_any_element()
            }
        }
    }

    // MARK: Sidebar

    fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let weak = cx.entity().downgrade();
        let selected = self.selected.clone();
        let overview_selected = selected.is_none();
        let footer = self.loaded().map(|loaded| {
            let generated = loaded.result.artifacts.iter().filter(|a| a.content.generated_text().is_some()).count();
            let degraded = loaded.result.artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).count();
            let passthrough = loaded.result.artifacts.len() - generated;
            format!("{generated} 个重建 · {passthrough} 个原样 · {degraded} 个降级")
        });
        let agent_bases: Vec<String> = self.layers.iter().flat_map(|l| l.manifest.files.iter().filter_map(|f| f.base.clone())).collect();
        v_flex()
            .w(px(width))
            .h_full()
            .flex_none()
            .bg(palette.sidebar)
            .border_r_1()
            .border_color(palette.hairline)
            .child(
                v_flex()
                    .p_3()
                    .pb_2()
                    .gap_2()
                    .child(Input::new(&self.filter).aria_label("筛选当前包的文件，回车进入文件树，Escape 清空筛选").small().prefix(IconName::ListFilter).cleanable(true))
                    .child(
                        h_flex()
                            .id("overview-row")
                            .role(Role::Button)
                            .aria_label("打开材料概要")
                            .on_a11y_action(AccessibleAction::Click, {
                                let weak = cx.entity().downgrade();
                                move |_, window, cx| { weak.update(cx, |this, cx| this.select(None, window, cx)).ok(); }
                            })
                            .gap_2()
                            .px_2()
                            .py(px(6.))
                            .rounded(px(7.))
                            .text_size(px(13.))
                            .when(overview_selected, |this| this.bg(palette.accent_soft).text_color(palette.accent))
                            .when(!overview_selected, |this| this.hover(|this| this.bg(palette.fill)))
                            .child(IconName::LayoutDashboard)
                            .child("概要")
                            .child(div().flex_1())
                            .child(div().text_size(px(10.5)).text_color(palette.tertiary).child("⌘0"))
                            .on_click(cx.listener(|this, _, window, cx| this.select(None, window, cx))),
                    )
                    .child(
                        h_flex()
                            .pt_1()
                            .text_size(px(11.))
                            .text_color(palette.secondary)
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("源码"))
                            .child(div().flex_1())
                            .child(
                                Button::new("containers")
                                    .label(if self.show_containers { "隐藏原始容器" } else { "显示原始容器" })
                                    .ghost()
                                    .xsmall()
                                    .tooltip("__wxlocated_ 开头的是重建时保留的原始编译产物")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_containers = !this.show_containers;
                                        this.rebuild_tree(cx);
                                    })),
                            ),
                    ),
            )
            .child(match &self.phase {
                Phase::Loading => v_flex()
                    .flex_1()
                    .px_4()
                    .pt_2()
                    .gap(px(11.))
                    .children([150., 110., 180., 90., 160., 130., 100., 170., 120.].into_iter().enumerate().map(|(i, w)| {
                        h_flex().gap_2().pl(px(if i % 3 == 0 { 0. } else { 16. })).child(Skeleton::new().size(px(14.))).child(Skeleton::new().h(px(10.)).w(px(w)))
                    }))
                    .into_any_element(),
                Phase::Failed(message) => {
                    div().flex_1().p_4().child(ui::empty_state(Some(IconName::FileX), "无法读取这个包", message.clone(), &palette)).into_any_element()
                }
                Phase::Ready(_) if self.visible_files == 0 => {
                    let filtering = !self.filter.read(cx).value().trim().is_empty();
                    let message = if filtering { "当前包没有匹配文件。清空筛选后可继续浏览。" } else { "当前包没有可显示的源码，可以查看原始容器。" };
                    div().id("file-tree-empty").role(Role::Status).aria_label(message).flex_1().p_4()
                        .child(ui::empty_state(Some(IconName::Search), "没有匹配文件", message, &palette)
                            .child(Button::new("clear-file-filter").label(if filtering { "清空筛选" } else { "显示原始容器" }).mt_2()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if filtering { this.filter.update(cx, |input, cx| input.set_value("", window, cx)); }
                                    else { this.show_containers = true; this.rebuild_tree(cx); }
                                    this.filter.update(cx, |input, cx| input.focus(window, cx));
                                })))).into_any_element()
                }
                Phase::Ready(loaded) => {
                    let loaded = loaded.clone();
                    div()
                        .track_focus(&self.tree_focus)
                        .flex_1()
                        .min_h_0()
                        .child(
                            tree(&self.tree, move |ix, entry, _selected, _window, cx| {
                                let item = entry.item();
                                let path = item.id.to_string();
                                let palette = crate::theme::palette(cx);
                                let is_folder = entry.is_folder();
                                let confidence = (!is_folder).then(|| loaded.index.get(&path).map(|&i| loaded.result.artifacts[i].confidence)).flatten();
                                let has_agent = !is_folder && agent_bases.contains(&path);
                                let is_selected = selected.as_deref() == Some(path.as_str());
                                let weak = weak.clone();
                                ListItem::new(ix)
                                    .selected(is_selected)
                                    .pl(px(10. + entry.depth() as f32 * 14.))
                                    .child(
                                        h_flex()
                                            .gap(px(7.))
                                            .text_size(px(12.5))
                                            .child(if is_folder {
                                                div()
                                                    .text_color(palette.secondary)
                                                    .child(if entry.is_expanded() { IconName::FolderOpen } else { IconName::Folder })
                                                    .into_any_element()
                                            } else {
                                                div().text_color(file_tint(&path, &palette)).child(file_icon(&path)).into_any_element()
                                            })
                                            .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
                                            .when(has_agent, |this| this.child(div().text_size(px(11.)).text_color(palette.accent).child(IconName::Sparkles)))
                                            .when_some(confidence.filter(|c| *c <= Confidence::Medium), |this, _| this.child(ui::dot(palette.warning, 6.))),
                                    )
                                    .on_click(move |_, window, cx| {
                                        if !is_folder {
                                            let path = path.clone();
                                            weak.update(cx, |this, cx| this.select(Some(path), window, cx)).ok();
                                        }
                                    })
                            })
                            .size_full(),
                        )
                        .into_any_element()
                }
            })
            .child(
                h_flex()
                    .px_4()
                    .py_2p5()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(palette.hairline)
                    .text_size(px(11.))
                    .text_color(palette.secondary)
                    .child(div().text_size(px(11.)).child(IconName::Code))
                    .child(footer.unwrap_or_else(|| if matches!(self.phase, Phase::Loading) { "正在重建…".into() } else { " ".into() })),
            )
    }

    // MARK: File view

    fn render_file(&self, path: &str, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let Some((loaded, index)) = self.artifact(path) else { return div().into_any_element() };
        let artifact = &loaded.result.artifacts[index];
        let has_raw = Self::raw_entry(&loaded, artifact).is_some();
        let mut options: Vec<(View, String)> = vec![(View::Rebuilt, "重建".into())];
        if has_raw {
            options.push((View::Raw, "原始".into()));
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if self.layer_file(index, path).is_some() {
                options.push((View::Agent(index), if self.layers.len() > 1 { format!("Agent · {}", layer.manifest.name) } else { "Agent".into() }));
            }
        }
        let selected_index = options.iter().position(|(v, _)| *v == self.view).unwrap_or(0);
        let views: Vec<View> = options.iter().map(|(v, _)| *v).collect();
        let detail = match self.view {
            View::Raw => Self::raw_entry(&loaded, artifact)
                .map(|e| format!("原始文件 · {} · {}", loaded.set.source_title(e), format_size(e.size as i64)))
                .unwrap_or_default(),
            _ => format!("{} · {}", kind_title(artifact.kind), confidence_title(artifact.confidence)),
        };
        let parts: Vec<&str> = path.split('/').collect();
        let last = parts.len() - 1;
        let crumbs = h_flex()
            .h(px(38.))
            .flex_none()
            .px_3()
            .gap_1p5()
            .bg(palette.code_chrome)
            .border_b_1()
            .border_color(palette.hairline)
            .text_size(px(12.))
            .child(
                Button::new("crumb-overview")
                    .icon(IconName::ChevronLeft)
                    .label("概要")
                    .ghost()
                    .xsmall()
                    .tooltip("回到概要（⌘0）")
                    .on_click(cx.listener(|this, _, window, cx| this.select(None, window, cx))),
            )
            .child(div().w(px(1.)).h(px(14.)).bg(palette.hairline))
            .child(div().text_color(file_tint(path, &palette)).child(file_icon(path)))
            .child(
                h_flex().min_w_0().flex_shrink(1.).gap_1().overflow_hidden().children(parts.iter().enumerate().map(|(i, part)| {
                    h_flex()
                        .gap_1()
                        .when(i > 0, |this| this.child(div().text_size(px(10.)).text_color(palette.tertiary).child(IconName::ChevronRight)))
                        .child(
                            div()
                                .truncate()
                                .when(i == last, |this| this.font_weight(FontWeight::SEMIBOLD).text_color(palette.text))
                                .when(i != last, |this| this.text_color(palette.secondary))
                                .child(part.to_string()),
                        )
                })),
            )
            .child(div().flex_1())
            .child(div().flex_none().text_size(px(11.)).text_color(palette.secondary).child(detail))
            .when(options.len() > 1, |this| {
                this.child(
                    TabBar::new("views")
                        .segmented()
                        .xsmall()
                        .selected_index(selected_index)
                        .children(options.iter().map(|(_, label)| Tab::new().label(label.clone())))
                        .on_click(cx.listener(move |this, index: &usize, window, cx| {
                            if let Some(view) = views.get(*index) {
                                this.set_view(*view, window, cx);
                            }
                        })),
                )
            });

        let banner = match self.view {
            View::Agent(index) => self.layer_file(index, path).map(|(layer, file, _)| {
                let manifest = &layer.manifest;
                let meta = [Some(manifest.name.clone()), manifest.author.agent.clone(), manifest.author.model.clone(), Some(manifest.created_at.clone()), file.note.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                let stats = self.agent_diff;
                h_flex()
                    .flex_none()
                    .gap_2()
                    .px_3()
                    .py(px(7.))
                    .bg(palette.warning.opacity(0.10))
                    .border_b_1()
                    .border_color(palette.warning.opacity(0.25))
                    .child(div().text_color(palette.warning).child(IconName::Sparkles))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child("Agent 推断，不是证据"))
                            .child(div().truncate().text_size(px(11.)).text_color(palette.secondary).child(meta)),
                    )
                    .when_some(stats, |this, (removed, added)| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .text_size(px(11.))
                                .font_family(ui::mono_font())
                                .child(div().text_color(palette.success).child(format!("+{added}")))
                                .child(div().text_color(palette.error).child(format!("−{removed}"))),
                        )
                    })
                    .when(artifact.content.generated_text().is_some(), |this| {
                        this.child(
                            Button::new("compare")
                                .icon(IconName::ArrowLeftRight)
                                .label("对比静态重建")
                                .xsmall()
                                .selected(self.compare)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.remember_reading(cx);
                                    this.compare = !this.compare;
                                    this.refresh_preview(window, cx);
                                })),
                        )
                    })
                    .child(div().text_size(px(11.)).text_color(palette.tertiary).child("引用请切到“原始”"))
            }),
            View::Raw => Some(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .px_3()
                    .py(px(6.))
                    .bg(palette.accent.opacity(0.07))
                    .border_b_1()
                    .border_color(palette.hairline)
                    .text_size(px(11.5))
                    .text_color(palette.secondary)
                    .child(div().text_color(palette.accent).child(IconName::ScanSearch))
                    .child(div().flex_1().child("包内原始字节；高亮处是这个文件的来源区间。引用证据时以这里为准。"))
                    .child(div().text_color(palette.tertiary).child("⌥⌘R 切回重建")),
            ),
            View::Rebuilt => None,
        };

        let body = match &self.preview {
            Preview::Code(editor) => div().flex_1().min_h_0().bg(palette.code).child(Editor::new(editor).aria_label(format!("源码预览 {path}")).readonly(true).bordered(false).h_full()).into_any_element(),
            Preview::Image(image, dimensions) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .bg(palette.code)
                .child(
                    div()
                        .p_3()
                        .rounded(px(10.))
                        .bg(palette.raised)
                        .border_1()
                        .border_color(palette.hairline)
                        .shadow(soft_shadow(&palette, false))
                        .child(img(image.clone()).max_w(px(720.)).max_h(px(520.)).when_some(*dimensions, |this, (w, h)| {
                            let scale = (720. / w as f32).min(520. / h as f32).min(1.).max(64. / (w.max(h) as f32).max(1.));
                            this.w(px(w as f32 * scale)).h(px(h as f32 * scale))
                        })),
                )
                .when_some(*dimensions, |this, (w, h)| this.child(div().text_size(px(11.)).text_color(palette.secondary).child(format!("{w} × {h} px"))))
                .into_any_element(),
            Preview::Binary(size) => div()
                .flex_1()
                .bg(palette.code)
                .child(ui::empty_state(Some(IconName::FileArchive), "二进制文件", format!("{}，无法以文本显示，可以从工具栏导出。", format_size(*size as i64)), &palette))
                .into_any_element(),
            Preview::Error(message) => {
                div().flex_1().bg(palette.code).child(ui::empty_state(Some(IconName::CircleAlert), "读取失败", message.clone(), &palette)).into_any_element()
            }
            Preview::None | Preview::Loading => div().flex_1().bg(palette.code).into_any_element(),
            Preview::TooLarge(size) => div()
                .flex_1()
                .bg(palette.code)
                .child(
                    ui::empty_state(
                        Some(IconName::FileText),
                        "文件较大",
                        format!("{}，打开后语法高亮与排版需要一点时间。导出后也可以用其他编辑器查看。", format_size(*size as i64)),
                        &palette,
                    )
                    .child(Button::new("open-large").label("仍然打开").primary().mt_2().on_click(cx.listener(|this, _, window, cx| {
                        this.open_large = true;
                        this.refresh_preview(window, cx);
                    }))),
                )
                .into_any_element(),
        };
        v_flex().flex_1().min_w_0().h_full().child(crumbs).children(banner).child(body).into_any_element()
    }

    // MARK: Overview

    fn render_overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let Some(loaded) = self.loaded() else {
            return match &self.phase {
                Phase::Failed(message) => ui::empty_state(Some(IconName::FileX), "无法读取这个包", message.clone(), &palette).into_any_element(),
                _ => {
                    let elapsed = self.started.elapsed().as_secs();
                    let current = self.step.min(STEPS.len() - 1);
                    // A vertical stepper: done steps ticked, the current one spinning.
                    let steps = v_flex().gap(px(7.)).children(STEPS.iter().enumerate().map(|(index, label)| {
                        let (marker, color) = if index < current {
                            (div().text_size(px(12.)).text_color(palette.accent).child(IconName::CircleCheck).into_any_element(), palette.secondary)
                        } else if index == current {
                            (Spinner::new().color(palette.accent).into_any_element(), palette.text)
                        } else {
                            (div().size(px(12.)).flex().items_center().justify_center().child(ui::dot(palette.hairline, 6.)).into_any_element(), palette.tertiary)
                        };
                        h_flex()
                            .gap_2()
                            .text_size(px(12.5))
                            .child(div().w(px(16.)).flex().justify_center().child(marker))
                            .child(div().text_color(color).when(index == current, |this| this.font_weight(FontWeight::MEDIUM)).child(*label))
                    }));
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_4()
                        .child(ui::mascot(84.))
                        .child(
                            v_flex()
                                .items_center()
                                .gap_1()
                                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("正在读取包、校验结构并重建源码"))
                                .child(div().text_size(px(12.)).text_color(palette.secondary).child(if elapsed > 3 { "大包需要多一点时间，完成后会自动显示。" } else { "通常几秒内完成。" })),
                        )
                        .child(
                            div()
                                .px_5()
                                .py_4()
                                .rounded(px(12.))
                                .bg(palette.raised)
                                .border_1()
                                .border_color(palette.hairline)
                                .shadow(soft_shadow(&palette, false))
                                .child(steps),
                        )
                        .into_any_element()
                }
            };
        };
        let (icon, ambient) = Library::global(cx).update(cx, |library, _| library.icon(&self.app));
        let context = self.context(cx);
        let completeness = Completeness::new(&self.app, Some(&loaded.analysis.plugins), &context);
        let artifacts = &loaded.result.artifacts;
        let generated = artifacts.iter().filter(|a| a.content.generated_text().is_some()).count();
        let passthrough = artifacts.len() - generated;
        let mut degraded: Vec<&Artifact> = artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).collect();
        degraded.sort_by_key(|a| a.confidence);

        let header = h_flex()
            .gap(px(18.))
            .child(app_icon(icon, ambient, self.app.display_name(), 68., &palette, true))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_size(px(26.)).font_weight(FontWeight::BOLD).line_height(px(32.)).child(self.app.display_name().to_string()))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_size(px(12.)).font_family(ui::mono_font()).text_color(palette.secondary).child(self.app.app_id.clone()))
                            .when_some(self.app.version.clone(), |this, v| this.child(badge(format!("v{v}"), palette.secondary)))
                            .child(badge(format_size(self.app.size), palette.secondary)),
                    ),
            );

        // Metric row: what was rebuilt, what needs care, how complete.
        let subpackages = completeness.declared_subpackages.len();
        let found_subpackages = completeness.found_subpackages.len();
        let plugins = completeness.plugins.len();
        let found_plugins = completeness.found_plugins.len();
        let declared = subpackages + plugins;
        let found = found_subpackages + found_plugins;
        let ratio = if declared == 0 { 100. } else { found as f32 * 100. / declared as f32 };
        let blocked = loaded.report["completeness"] == "BLOCKED";
        let ring_color = if blocked { palette.error } else if completeness.is_complete() { palette.success } else { palette.warning };
        let metric = |label: &'static str, value: String, detail: String, tint: Hsla, palette: &Palette| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(3.))
                .p(px(14.))
                .rounded(px(12.))
                .bg(palette.raised)
                .border_1()
                .border_color(palette.hairline)
                .shadow(soft_shadow(palette, false))
                .child(div().text_size(px(11.5)).text_color(palette.secondary).child(label))
                .child(div().text_size(px(26.)).line_height(px(30.)).font_weight(FontWeight::SEMIBOLD).text_color(tint).child(value))
                .child(div().truncate().text_size(px(11.)).text_color(palette.tertiary).child(detail))
        };
        let metrics = h_flex()
            .gap_3()
            .items_stretch()
            .child(metric("已重建", generated.to_string(), format!("{passthrough} 个原样保留"), palette.ink, &palette))
            .child(metric(
                "降级",
                degraded.len().to_string(),
                if degraded.is_empty() { "全部按结构确定性还原".into() } else { "部分结构为推导".into() },
                if degraded.is_empty() { palette.ink } else { palette.warning },
                &palette,
            ))
            .child(metric("耗时", format!("{:.1}s", loaded.seconds), format!("{} 个产物", artifacts.len()), palette.ink, &palette))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .p(px(14.))
                    .rounded(px(12.))
                    .bg(palette.raised)
                    .border_1()
                    .border_color(palette.hairline)
                    .shadow(soft_shadow(&palette, false))
                    .child(
                        div()
                            .relative()
                            .size(px(54.))
                            .flex_none()
                            .child(ProgressCircle::new("completeness").value(ratio).color(ring_color).size(px(54.)))
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if declared == 0 { "—".to_string() } else { format!("{found}/{declared}") }),
                            ),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap(px(3.))
                            .child(div().text_size(px(11.5)).text_color(palette.secondary).child("声明材料"))
                            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(ring_color)
                                .child(if blocked { "受阻" } else if completeness.is_complete() { "齐备" } else { "待补齐" }))
                            .child(div().text_size(px(10.5)).text_color(palette.secondary).child(format!("分包 {found_subpackages}/{subpackages}")))
                            .child(div().text_size(px(10.5)).text_color(palette.secondary).child(format!("插件 {found_plugins}/{plugins}"))),
                    ),
            );

        // Reconstruction quality and file types, each as one stacked bar.
        let count = |c: Confidence| artifacts.iter().filter(|a| a.confidence == c && !a.path.starts_with("__wxlocated_")).count();
        let quality = [
            ("原样", count(Confidence::Exact), palette.success),
            ("高置信", count(Confidence::High), palette.accent),
            ("部分推导", count(Confidence::Medium), palette.warning),
            ("低置信", count(Confidence::Low), palette.error),
        ];
        let mut kind_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for artifact in artifacts.iter().filter(|a| !a.path.starts_with("__wxlocated_")) {
            *kind_counts.entry(kind_title(artifact.kind)).or_default() += 1;
        }
        let mut kind_counts: Vec<(&'static str, usize)> = kind_counts.into_iter().collect();
        kind_counts.sort_by_key(|kind| std::cmp::Reverse(kind.1));
        let kind_colors = [
            palette.accent,
            hsla(0.60, 0.65, 0.58, 1.),
            hsla(0.08, 0.75, 0.58, 1.),
            hsla(0.76, 0.55, 0.62, 1.),
            hsla(0.13, 0.80, 0.52, 1.),
            hsla(0.95, 0.60, 0.62, 1.),
            palette.tertiary,
        ];
        let bar = |segments: Vec<(String, usize, Hsla)>, palette: &Palette| {
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .h(px(10.))
                        .w_full()
                        .rounded_full()
                        .overflow_hidden()
                        .bg(palette.fill)
                        .gap(px(2.))
                        .children(segments.iter().filter(|s| s.1 > 0).map(|(_, n, color)| {
                            div().h_full().flex_grow(*n as f32).flex_basis(px(0.)).bg(*color)
                        })),
                )
                .child(h_flex().flex_wrap().gap_x_4().gap_y_1().children(segments.into_iter().map(|(label, n, color)| {
                    h_flex()
                        .gap_1p5()
                        .text_size(px(11.5))
                        .child(div().size(px(8.)).rounded(px(2.)).bg(color))
                        .child(div().text_color(palette.secondary).child(label))
                        .child(div().font_weight(FontWeight::MEDIUM).child(n.to_string()))
                })))
        };
        let quality_card = ui::card(&palette)
            .p(px(14.))
            .gap_4()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child("重建置信度"))
                    .child(bar(quality.iter().map(|(l, n, c)| (l.to_string(), *n, *c)).collect(), &palette)),
            )
            .child(
                v_flex().gap_1p5().child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child("文件类型")).child(bar(
                    kind_counts.iter().enumerate().map(|(i, (l, n))| (l.to_string(), *n, kind_colors[i.min(kind_colors.len() - 1)])).collect(),
                    &palette,
                )),
            );

        // Completeness: what an agent will and will not receive.
        let counts = format!("分包 {found_subpackages}/{subpackages} · 插件 {found_plugins}/{plugins}");
        let (state_title, state_color, state_detail) = if blocked {
            ("材料受阻", palette.error, "主包无法可靠读取，请刷新缓存后重新重建。".to_string())
        } else if completeness.is_complete() {
            ("声明材料齐备", palette.success, format!("{counts}。缓存齐备不代表业务完整或源码完全恢复。"))
        } else {
            ("材料待补齐", palette.warning, format!("{counts}。在微信中进入相关页面或加载插件后会自动重扫。"))
        };
        let chip_row = |title: &str, values: Vec<String>, limit: usize, palette: &Palette| {
            let more = values.len().saturating_sub(limit);
            h_flex()
                .items_start()
                .gap_2p5()
                .child(div().w(px(40.)).flex_none().pt(px(3.)).text_size(px(11.5)).text_color(palette.secondary).child(title.to_string()))
                .child(
                    h_flex()
                        .flex_1()
                        .flex_wrap()
                        .gap(px(5.))
                        .children(values.into_iter().take(limit).map(|v| chip(v, palette, true)))
                        .when(more > 0, |this| this.child(div().pt(px(3.)).text_size(px(11.)).text_color(palette.tertiary).child(format!("+{more}")))),
                )
        };
        let mut completeness_card = ui::card(&palette).p(px(14.)).gap_2p5().child(
            h_flex()
                .gap_2()
                .child(ui::dot(state_color, 8.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(state_title))
                .child(div().flex_1().text_size(px(11.5)).text_color(palette.secondary).child(state_detail)),
        );
        if !completeness.declared_subpackages.is_empty() {
            completeness_card = completeness_card.child(chip_row("已缓存", completeness.found_subpackages.clone(), 20, &palette));
            if !completeness.missing_subpackages.is_empty() {
                completeness_card = completeness_card.child(chip_row("缺失", completeness.missing_subpackages.clone(), 20, &palette));
            }
            if !completeness.missing_in_older_versions.is_empty() {
                let names: Vec<String> = completeness.missing_in_older_versions.keys().cloned().collect();
                completeness_card = completeness_card.child(
                    div().text_size(px(11.)).text_color(palette.tertiary).child(format!("其中 {} 在旧版本缓存里还有，为保持版本一致不会混入。", names.join("、"))),
                );
            }
        }
        if !completeness.found_plugins.is_empty() {
            completeness_card = completeness_card.child(chip_row("插件", completeness.found_plugins.keys().map(|k| format!("{k} · 已缓存")).collect(), 10, &palette));
        }
        if !completeness.missing_plugins.is_empty() {
            completeness_card = completeness_card.child(chip_row("缺插件", completeness.missing_plugins.clone(), 10, &palette));
        }

        // Structure: pages and configuration.
        let structure = ui::card(&palette)
            .p(px(14.))
            .gap_2p5()
            .child(chip_row("页面", loaded.analysis.page_paths.clone(), 18, &palette))
            .child(chip_row("配置", loaded.analysis.configuration_files.clone(), 6, &palette));

        let shown: Vec<(String, Confidence, Option<String>)> = degraded.iter().take(10).map(|a| (a.path.clone(), a.confidence, a.notes.first().cloned())).collect();
        let degraded_count = degraded.len();
        let degraded_rows: Vec<AnyElement> = shown
            .into_iter()
            .enumerate()
            .map(|(i, (path, confidence, note))| {
                let open = path.clone();
                h_flex()
                    .id(SharedString::from(format!("degraded-{i}")))
                    .gap_3()
                    .px_3()
                    .py(px(9.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|this| this.bg(palette.fill))
                    .child(badge(confidence_title(confidence), palette.warning))
                    .child(div().text_color(file_tint(&path, &palette)).child(file_icon(&path)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_size(px(12.)).font_family(ui::mono_font()).child(path))
                            .when_some(note, |this, note| this.child(div().truncate().text_size(px(11.)).text_color(palette.secondary).child(note))),
                    )
                    .child(div().text_size(px(11.)).text_color(palette.tertiary).child(IconName::ChevronRight))
                    .on_click(cx.listener(move |this, _, window, cx| this.select(Some(open.clone()), window, cx)))
                    .into_any_element()
            })
            .collect();

        let diagnostics: Vec<(Severity, String)> = loaded.result.diagnostics.iter().take(10).map(|d| (d.severity, d.message.clone())).collect();
        let strings = |value: &Value| -> Vec<String> { value.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default() };
        let next_actions = strings(&loaded.report["next_actions"]);
        let notes = strings(&loaded.report["coverage_notes"]);

        div()
            .id("overview")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(900.))
                    .mx_auto()
                    .px(px(36.))
                    .py(px(30.))
                    .gap(px(28.))
                    .child(header)
                    .child(metrics)
                    .child(v_flex().gap_2p5().child(section_label("重建质量", Some("不含原始容器".into()), &palette)).child(quality_card))
                    .child(v_flex().gap_2p5().child(section_label("材料完整度", Some("导出时写入 material.json".into()), &palette)).child(completeness_card))
                    .child(
                        v_flex()
                            .gap_2p5()
                            .child(section_label(
                                "页面结构",
                                Some(format!("{} 个页面 · {} 个配置", loaded.analysis.page_paths.len(), loaded.analysis.configuration_files.len())),
                                &palette,
                            ))
                            .child(structure),
                    )
                    .when(!degraded_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_2p5()
                                .child(section_label("降级文件", Some(format!("{degraded_count} 个 · 部分结构为推导，引用前请对照原始文件")), &palette))
                                .child(ui::card(&palette).p_1().children(degraded_rows)),
                        )
                    })
                    .when(!diagnostics.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .child(section_label("兼容性提示", Some(format!("{} 条", loaded.result.diagnostics.len())), &palette))
                                .children(diagnostics.into_iter().map(|(severity, message)| {
                                    let (icon, color) = match severity {
                                        Severity::Error => (IconName::CircleX, palette.error),
                                        Severity::Warning => (IconName::TriangleAlert, palette.warning),
                                        Severity::Information => (IconName::Info, palette.secondary),
                                    };
                                    h_flex()
                                        .items_start()
                                        .gap_2()
                                        .text_size(px(12.))
                                        .child(div().pt(px(1.)).text_color(color).child(icon))
                                        .child(div().flex_1().text_color(palette.secondary).child(message))
                                })),
                        )
                    })
                    .when(!next_actions.is_empty(), |this| {
                        this.child(
                            v_flex().gap_2().child(section_label("下一步", None, &palette)).children(next_actions.into_iter().map(|action| {
                                h_flex()
                                    .items_start()
                                    .gap_2()
                                    .text_size(px(12.5))
                                    .child(div().pt(px(1.)).text_color(palette.accent).child(IconName::ArrowRight))
                                    .child(div().flex_1().child(action))
                            })),
                        )
                    })
                    .when(!notes.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1p5()
                                .p(px(14.))
                                .rounded(px(12.))
                                .border_1()
                                .border_color(palette.hairline)
                                .child(section_label("使用前须知", None, &palette))
                                .children(notes.into_iter().map(|note| div().text_size(px(11.5)).text_color(palette.secondary).child(format!("· {note}")))),
                        )
                    }),
            )
            .into_any_element()
    }

    // MARK: Inspector

    fn render_inspector(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let mut sections: Vec<AnyElement> = Vec::new();
        // Click a heading to fold its section; folded sections show only the heading.
        let weak = cx.entity().downgrade();
        let collapsed = self.collapsed.clone();
        let section = |title: &'static str, palette: &Palette| {
            let folded = collapsed.contains(title);
            let weak = weak.clone();
            let accessible = weak.clone();
            v_flex()
                .w_full()
                .gap(px(9.))
                .when(folded, |this| this.h(px(18.)).overflow_hidden())
                .child(
                    h_flex()
                        .id(title)
                        .role(Role::Button)
                        .aria_label(format!("{}{}", if folded { "展开" } else { "折叠" }, title))
                        .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                            accessible.update(cx, |this, cx| {
                                if !this.collapsed.remove(title) { this.collapsed.insert(title); }
                                cx.notify();
                            }).ok();
                        })
                        .gap_1()
                        .cursor_pointer()
                        .child(div().text_size(px(10.)).text_color(palette.tertiary).child(if folded { IconName::ChevronRight } else { IconName::ChevronDown }))
                        .child(section_label(title, None, palette))
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |this, cx| {
                                if !this.collapsed.remove(title) {
                                    this.collapsed.insert(title);
                                }
                                cx.notify();
                            })
                            .ok();
                        }),
                )
        };

        if let Some((loaded, index)) = self.selected.as_ref().and_then(|p| self.artifact(p)) {
            let artifact = &loaded.result.artifacts[index];
            let size = match &artifact.content {
                Content::Generated(text) => text.len() as i64,
                Content::Entry(entry) => loaded.set.entries[*entry].size as i64,
            };
            let (color, explanation) = match artifact.confidence {
                Confidence::Exact => (palette.success, "与包内字节或 Source Map 原文一致。"),
                Confidence::High => (palette.accent, "按编译结构确定性还原，语义应与原文一致。"),
                Confidence::Medium => (palette.warning, "部分结构为推导结果，引用前请对照原始文件（⌥⌘R）。"),
                Confidence::Low => (palette.warning, "只能部分还原，保留了原始片段与诊断，请以原始文件为准（⌥⌘R）。"),
            };
            let path = artifact.path.clone();
            sections.push(
                section("当前文件", &palette)
                    .child(h_flex().gap_2().child(badge(confidence_title(artifact.confidence), color)).child(div().text_size(px(11.5)).text_color(palette.secondary).child(format_size(size))))
                    .child(div().text_size(px(11.5)).text_color(palette.secondary).child(explanation))
                    .child(
                        v_flex().id("source-precision").role(Role::Status).aria_label("来源匹配精度").gap_1()
                            .child(div().text_size(px(11.5)).font_weight(FontWeight::MEDIUM).child("来源匹配"))
                            .child(div().text_size(px(11.5)).text_color(palette.secondary)
                                .child(self.evidence.as_ref().map(evidence_method).unwrap_or("尚未定位")))
                            .child(div().text_size(px(11.)).text_color(palette.secondary).child("复制时按当前光标重新定位。"))
                            .when_some(self.evidence.as_ref().and_then(|value| value["match"]["note"].as_str()), |this, note| {
                                this.child(div().text_size(px(11.)).text_color(palette.secondary).child(note.to_string()))
                            })
                            .when_some(self.evidence.as_ref().and_then(|value| value["match"]["candidates"].as_u64()).filter(|n| *n > 1), |this, count| {
                                this.child(div().text_size(px(11.)).text_color(palette.warning).child(format!("有 {count} 处相同文本，当前采用第一处，请核对。")))
                            })
                            .child(Button::new("copy-evidence").icon(IconName::Copy).label("复制证据引用").accessibility_label("复制当前光标对应的原始包证据引用")
                                .xsmall().loading(self.copying_evidence).disabled(self.copying_evidence || matches!(self.preview, Preview::Loading | Preview::TooLarge(_) | Preview::Error(_)))
                                .tooltip(if self.compare { "差异视图提供文件级原始范围；退出对比后可定位当前行" } else { "按当前光标定位，包含包哈希、条目、字节偏移与匹配精度" })
                                .on_click(cx.listener(|this, _, window, cx| this.copy_evidence(window, cx)))),
                    )
                    .when(!artifact.notes.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(palette.tertiary).child("恢复步骤"))
                                .children(artifact.notes.iter().take(6).map(|note| div().text_size(px(11.5)).text_color(palette.secondary).child(note.clone()))),
                        )
                    })
                    .when(!artifact.sources.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(palette.tertiary).child("来源"))
                                .children(artifact.sources.iter().take(4).map(|source| {
                                    let range = match (source.start, source.end) {
                                        (Some(start), Some(end)) => format!(" · {start}..<{end}"),
                                        _ => String::new(),
                                    };
                                    div().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.secondary).child(format!("{}{range}", source.entry_path))
                                })),
                        )
                    })
                    .child(h_flex().child(Button::new("copy-file-path").icon(IconName::Copy).label("复制路径").xsmall().on_click(move |_, _, cx| ui::copy(path.clone(), cx))))
                    .into_any_element(),
            );
        }

        let app = &self.app;
        let folder = app.cache_directory();
        let folder_copy = folder.to_string_lossy().into_owned();
        let format = self.loaded().and_then(|l| l.set.sources.first().map(|s| s.archive.format.as_str().to_string()));
        sections.push(
            section("应用", &palette)
                .child(ui::info_row("AppID", app.app_id.clone(), &palette, true))
                .child(ui::info_row("版本", app.version.clone().unwrap_or_else(|| "未知".into()), &palette, false))
                .child(ui::info_row("大小", format_size(app.size), &palette, false))
                .child(ui::info_row("包", if app.sub_packages.is_empty() { "仅主包".into() } else { format!("主包 + {} 个分包", app.sub_packages.len()) }, &palette, false))
                .when_some(format, |this, format| this.child(ui::info_row("格式", format, &palette, false)))
                .child(ui::info_row("修改", format_time(app.modified), &palette, false))
                .child(ui::info_row("名称来源", app.metadata_source.clone().unwrap_or_else(|| "文件路径与属性".into()), &palette, false))
                .child(ui::info_row("目录", ui::display_path(&folder_copy), &palette, true))
                .child(
                    h_flex()
                        .pt_1()
                        .gap_1p5()
                        .child(Button::new("app-reveal").icon(IconName::FolderOpen).label("Finder").xsmall().on_click(move |_, _, cx| cx.reveal_path(&folder)))
                        .child(Button::new("app-copy").icon(IconName::Copy).label("复制路径").xsmall().on_click(move |_, _, cx| ui::copy(folder_copy.clone(), cx))),
                )
                .into_any_element(),
        );

        let rebuild = section("源码重建", &palette);
        sections.push(
            match &self.phase {
                Phase::Ready(loaded) => {
                    let generated = loaded.result.artifacts.iter().filter(|a| a.content.generated_text().is_some()).count();
                    let degraded = loaded.result.artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).count();
                    rebuild
                        .child(ui::info_row("文件", format!("{generated} 个重建 · {} 个原样", loaded.result.artifacts.len() - generated), &palette, false))
                        .child(ui::info_row("耗时", format!("{:.1} 秒", loaded.seconds), &palette, false))
                        .when(degraded > 0, |this| this.child(ui::info_row("降级", format!("{degraded} 个文件"), &palette, false).text_color(palette.warning)))
                }
                Phase::Loading => rebuild.child(h_flex().gap_2().text_size(px(12.)).text_color(palette.secondary).child(Spinner::new().color(palette.accent)).child("正在重建…")),
                Phase::Failed(_) => rebuild.child(div().text_size(px(12.)).text_color(palette.secondary).child("这个包无法重建源码")),
            }
            .child(
                h_flex().child(
                    Button::new("rebuild")
                        .icon(IconName::RefreshCw)
                        .label("重新重建")
                        .xsmall()
                        .disabled(matches!(self.phase, Phase::Loading))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(Rebuild), cx)),
                ),
            )
            .into_any_element(),
        );

        let layers = section("Agent 重建层", &palette);
        sections.push(
            if self.layers.is_empty() {
                layers.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(palette.secondary)
                        .child("还没有导入。Agent 可以用 wxlocated overlay init 生成工作目录，改完后 overlay import，或者把含 overlay.json 的文件夹拖进这个窗口。"),
                )
            } else {
                layers
                    .children(self.layers.iter().enumerate().map(|(index, layer)| {
                        let manifest = &layer.manifest;
                        let meta = [manifest.author.agent.clone(), manifest.author.model.clone(), Some(format!("{} 个文件", manifest.files.len()))]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ");
                        h_flex()
                            .items_start()
                            .gap_2()
                            .child(div().pt(px(2.)).text_size(px(12.)).text_color(palette.warning).child(IconName::Sparkles))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(manifest.name.clone()))
                                    .child(div().text_size(px(11.)).text_color(palette.secondary).child(meta))
                                    .child(div().text_size(px(11.)).text_color(palette.tertiary).child(manifest.created_at.clone())),
                            )
                            .child(
                                Button::new(SharedString::from(format!("remove-layer-{index}")))
                                    .icon(IconName::Trash)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("删除这个重建层")
                                    .on_click(cx.listener(move |this, _, window, cx| this.remove_layer(index, window, cx))),
                            )
                    }))
                    .child(div().text_size(px(11.)).text_color(palette.tertiary).child("在代码区切换到“Agent”查看；它是推断结果，不是证据。"))
            }
            .into_any_element(),
        );

        let integrity = section("包与完整性", &palette);
        sections.push(
            match self.loaded() {
                None => integrity.child(v_flex().gap_2().child(Skeleton::new().h(px(12.)).w(px(140.))).child(Skeleton::new().h(px(10.)).w(px(200.)))),
                Some(loaded) => {
                    let worst = loaded.inspections.iter().map(|i| i.status).max_by_key(|s| integrity_rank(*s)).unwrap_or(Integrity::Valid);
                    let (title, color, icon) = integrity_style(worst, &palette);
                    let show_hashes = self.show_hashes;
                    let copied = self.copied == Some("hashes");
                    let all_hashes = loaded
                        .inspections
                        .iter()
                        .filter_map(|i| Some(format!("{}  {}", i.sha256.clone()?, i.path.file_name()?.to_string_lossy())))
                        .collect::<Vec<_>>()
                        .join("\n");
                    integrity
                        .child(h_flex().gap_1p5().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(icon).child(title))
                        .children(loaded.inspections.iter().enumerate().map(|(index, inspection)| {
                            let (_, color, icon) = integrity_style(inspection.status, &palette);
                            let name = inspection.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let entries = inspection.entry_count.map(|n| format!(" · {n} 项")).unwrap_or_default();
                            v_flex()
                                .gap(px(2.))
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .child(div().text_size(px(10.)).text_color(color).child(icon))
                                        .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).font_weight(FontWeight::MEDIUM).font_family(ui::mono_font()).child(name))
                                        .child(div().text_size(px(10.5)).text_color(palette.tertiary).child(if index == 0 { "主包" } else { "分包" })),
                                )
                                .child(div().text_size(px(11.)).text_color(palette.secondary).child(format!("{}{entries}", format_size(inspection.file_size))))
                                .when(show_hashes, |this| {
                                    this.when_some(inspection.sha256.clone(), |this, sha| {
                                        this.child(div().truncate().text_size(px(10.)).font_family(ui::mono_font()).text_color(palette.tertiary).child(sha))
                                    })
                                })
                        }))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .child(
                                    Button::new("toggle-hashes")
                                        .icon(if show_hashes { IconName::ChevronDown } else { IconName::ChevronRight })
                                        .label("SHA-256 指纹")
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_hashes = !this.show_hashes;
                                            cx.notify();
                                        })),
                                )
                                .when(show_hashes, |this| {
                                    this.child(
                                        Button::new("copy-hashes")
                                            .icon(if copied { IconName::Check } else { IconName::Hash })
                                            .label(if copied { "已复制" } else { "复制全部" })
                                            .xsmall()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                ui::copy(all_hashes.clone(), cx);
                                                this.mark_copied("hashes", cx);
                                            })),
                                    )
                                }),
                        )
                }
            }
            .into_any_element(),
        );

        div()
            .id("inspector")
            .w(px(width))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .bg(palette.sidebar)
            .border_l_1()
            .border_color(palette.hairline)
            .child(v_flex().p_4().gap(px(22.)).children(sections))
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        // Fade new content in when the file, its view or the package changes.
        let key = format!("{}|{:?}|{}|{}", self.selected.as_deref().unwrap_or("overview"), view_key(self.view), self.package, self.loaded().is_some());
        let (sidebar_width, inspector_width) = {
            let settings = &Library::global(cx).read(cx).settings;
            (settings.sidebar_width.clamp(200., 520.), settings.inspector_width.clamp(220., 480.))
        };
        let content = match self.selected.clone() {
            Some(path) => self.render_file(&path, cx),
            None => self.render_overview(cx),
        };
        let content = div().flex_1().min_w_0().h_full().flex().child(content).with_animation(
            SharedString::from(key),
            Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
            |this, t| this.opacity(0.35 + 0.65 * t),
        );
        v_flex()
            .id("workspace")
            .role(Role::Group)
            .aria_label(format!("{} 源码工作区", self.app.display_name()))
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .text_color(palette.text)
            .capture_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(Self::export_material))
            .on_action(cx.listener(Self::show_overview))
            .on_action(cx.listener(Self::toggle_raw))
            .on_action(cx.listener(Self::toggle_inspector))
            .on_action(cx.listener(Self::back_to_gallery))
            .on_action(cx.listener(Self::rebuild))
            .on_action(cx.listener(Self::focus_filter))
            .child(self.render_toolbar(cx))
            .children(ui::settings_notice(cx))
            .child(
                h_flex()
                    .id("panels")
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    // Panel edges drag to resize; widths persist across windows.
                    .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SidebarEdge>, _, cx| {
                        let width = f32::from(event.event.position.x - event.bounds.left()).clamp(200., 520.);
                        this.set_panel_width(true, width, cx);
                    }))
                    .on_drag_move(cx.listener(|this, event: &DragMoveEvent<InspectorEdge>, _, cx| {
                        let width = f32::from(event.bounds.right() - event.event.position.x).clamp(220., 480.);
                        this.set_panel_width(false, width, cx);
                    }))
                    .on_drop(cx.listener(|_, _: &SidebarEdge, _, cx| save_panel_widths(cx)))
                    .on_drop(cx.listener(|_, _: &InspectorEdge, _, cx| save_panel_widths(cx)))
                    .child(self.render_sidebar(sidebar_width, cx))
                    .child(edge_handle("sidebar-edge", SidebarEdge))
                    .child(div().flex_1().min_w_0().h_full().flex().bg(palette.window).child(content))
                    .when(self.show_inspector, |this| {
                        this.child(edge_handle("inspector-edge", InspectorEdge)).child(self.render_inspector(inspector_width, cx))
                    }),
            )
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(palette.accent.opacity(0.05)).border_2().border_color(palette.accent))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.on_drop(paths, window, cx)))
    }
}

/// Bytes of `artifact` in `view`: the rebuild, the raw entry it came from,
/// or an Agent layer's file (`layer`).
fn read_view(loaded: &Loaded, artifact: &Artifact, view: View, layer: Option<&std::path::Path>) -> Result<Vec<u8>, String> {
    match view {
        View::Rebuilt => match &artifact.content {
            Content::Generated(text) => Ok(text.as_bytes().to_vec()),
            Content::Entry(index) => loaded.set.read(&loaded.set.entries[*index]).map_err(|e| e.to_string()),
        },
        View::Raw => {
            let entry = Workspace::raw_entry(loaded, artifact).ok_or("这个文件没有单独的原始字节")?;
            loaded.set.read(entry).map_err(|e| e.to_string())
        }
        View::Agent(_) => {
            let path = layer.ok_or("这个重建层没有替换该文件")?;
            std::fs::read(path).map_err(|e| format!("无法读取 {}：{e}", path.display()))
        }
    }
}

/// Everything about a preview that can be worked out off the main thread.
fn prepare(loaded: &Loaded, index: usize, path: &str, view: View, compare: bool, force: bool, layer: Option<&std::path::Path>) -> Prepared {
    let artifact = &loaded.result.artifacts[index];
    let bytes = match read_view(loaded, artifact, view, layer) {
        Ok(bytes) => bytes,
        Err(message) => return Prepared::Error(message),
    };
    let is_image = view != View::Raw || artifact.kind == wxlocated_engine::decompile::ArtifactKind::Image;
    if let Some(format) = image_format(&bytes).filter(|_| is_image) {
        let dimensions = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
        return Prepared::Image(bytes, format, dimensions);
    }
    let size = bytes.len() as u64;
    let Ok(mut text) = String::from_utf8(bytes) else { return Prepared::Binary(size) };
    if text.len() > LARGE_TEXT && !force {
        return Prepared::TooLarge(size);
    }
    let evidence = build_evidence(loaded, index, view, &text, (!compare).then_some(1), false).ok();
    let agent_diff = match (view, artifact.content.generated_text()) {
        (View::Agent(_), Some(base)) => Some(crate::diff::counts(base, &text)),
        _ => None,
    };
    let mut language = if view == View::Raw { "javascript" } else { ui::language_for(path) };
    if let (View::Agent(_), true, Some(base)) = (view, compare, artifact.content.generated_text()) {
        text = crate::diff::unified(base, &text, "静态重建", "Agent");
        language = "text";
    }
    let highlight = (view == View::Raw).then(|| source_span(artifact, &text)).flatten();
    Prepared::Text { text, language, highlight, evidence, agent_diff }
}

fn evidence_method(value: &Value) -> &'static str {
    match value["match"]["method"].as_str() {
        Some("identical") => "原样对应",
        Some("anchored") => "文本定位",
        _ => "仅范围",
    }
}

fn build_evidence(loaded: &Loaded, index: usize, view: View, reading: &str, line: Option<i64>, verify: bool) -> Result<Value, String> {
    let artifact = &loaded.result.artifacts[index];
    let source = artifact.sources.first().ok_or("这个文件没有可追溯的原始来源")?;
    let entry = loaded.set.entries.iter().find(|entry| entry.path == source.entry_path && loaded.set.source_title(entry) == source.package)
        .ok_or("找不到对应的原始条目，请重新重建")?;
    let archive = &loaded.set.sources[entry.source].archive;
    let inspection = loaded.inspections.iter().find(|inspection| inspection.path == archive.path).ok_or("没有原始包指纹，请重新重建")?;
    let sha256 = inspection.sha256.as_deref().ok_or("原始包 SHA-256 不可用")?;
    if verify && report::inspect_package(&archive.path, wxlocated_engine::archive::inferred_app_id(&archive.path).as_deref()).sha256.as_deref() != Some(sha256) {
        return Err("原始缓存包已变化，无法引用旧指纹。请重新重建后再复制证据。".into());
    }
    let identical = view == View::Raw || (view == View::Rebuilt && matches!(artifact.content, Content::Entry(_)));
    let bytes = if identical && !reading.is_empty() {
        std::borrow::Cow::Borrowed(reading.as_bytes())
    } else {
        std::borrow::Cow::Owned(loaded.set.read_limited(entry, wxlocated_engine::decompile::MAXIMUM_TEXT_FILE_SIZE).map_err(|error| error.to_string())?)
    };
    let segment = if identical { 0..bytes.len() } else {
        std::str::from_utf8(&bytes).ok().and_then(|raw| source_span(artifact, raw)).unwrap_or(0..bytes.len())
    };
    let located = wxlocated_engine::trace::locate_entry(&bytes, reading, line, segment.clone(), identical, 120);
    let mut result = serde_json::json!({
        "schema_version": report::SCHEMA_VERSION,
        "source": { "path": artifact.path, "view": match view { View::Rebuilt => "source", View::Raw => "raw", View::Agent(_) => "semantic" }, "line": line },
        "raw": { "package_path": archive.path, "package_sha256": sha256, "entry_path": entry.path,
            "segment_start": segment.start, "segment_end": segment.end, "offset_unit": "entry_utf8_byte" },
        "match": located,
    });
    if matches!(view, View::Agent(_)) {
        result["note"] = Value::String("Agent 视图是推断；证据引用仅指向上述原始包与条目，无法匹配的修改只定位到范围。".into());
    }
    Ok(result)
}

/// Drag payloads for the two panel edges.
#[derive(Clone)]
struct SidebarEdge;
#[derive(Clone)]
struct InspectorEdge;

impl Render for SidebarEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Render for InspectorEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn edge_handle<T: Clone + Render>(id: &'static str, payload: T) -> impl IntoElement {
    gpui_kit::component::resize_handle::<T, T>(id, Axis::Horizontal).on_drag(payload, |payload, _, _, cx| cx.new(|_| payload.clone()))
}

fn save_panel_widths(cx: &mut App) {
    Library::global(cx).update(cx, |library, cx| library.save_settings(cx));
}

fn view_key(view: View) -> String {
    match view {
        View::Rebuilt => "rebuilt".into(),
        View::Raw => "raw".into(),
        View::Agent(index) => format!("agent{index}"),
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

fn integrity_rank(status: Integrity) -> u8 {
    match status {
        Integrity::Valid => 0,
        Integrity::Unrecognized => 1,
        Integrity::Attention => 2,
        Integrity::Unreadable => 3,
        Integrity::Damaged => 4,
    }
}

fn integrity_style(status: Integrity, palette: &Palette) -> (&'static str, Hsla, IconName) {
    match status {
        Integrity::Valid => ("结构正常", palette.success, IconName::BadgeCheck),
        Integrity::Attention => ("需要留意", palette.warning, IconName::CircleAlert),
        Integrity::Unrecognized => ("加密或未知格式", palette.secondary, IconName::CircleQuestionMark),
        Integrity::Damaged => ("结构异常", palette.error, IconName::OctagonX),
        Integrity::Unreadable => ("读取失败", palette.warning, IconName::TriangleAlert),
    }
}

/// Where in the raw entry text the artifact's first source range lies. Ranges
/// index the restorer's input: the entry text, or for an HTML container the
/// joined `<script>` blocks, so those are located in the raw text by content.
fn source_span(artifact: &Artifact, raw: &str) -> Option<std::ops::Range<usize>> {
    let source = artifact.sources.first()?;
    let (start, end) = (source.start?, source.end?);
    // The restorers saw the text without a BOM; the editor shows it with one.
    let text = raw.strip_prefix('\u{FEFF}').unwrap_or(raw);
    let bom = raw.len() - text.len();
    let span = if source.entry_path.to_lowercase().ends_with(".html") {
        let script = wxlocated_engine::decompile::wxml::extract_scripts(text);
        let slice = script.get(start..end)?;
        let position = text.find(slice)?;
        position..position + slice.len()
    } else {
        text.get(start..end)?;
        start..end
    };
    Some(span.start + bom..span.end + bom)
}

/// Nested tree items from sorted paths; folders first at each level.
fn build_tree(paths: &[&str], expanded: bool) -> Vec<TreeItem> {
    #[derive(Default)]
    struct Node {
        folders: BTreeMap<String, Node>,
        files: Vec<(String, String)>,
    }
    let mut root = Node::default();
    for path in paths {
        let mut node = &mut root;
        let parts: Vec<&str> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            node = node.folders.entry(part.to_string()).or_default();
        }
        node.files.push((path.to_string(), parts[parts.len() - 1].to_string()));
    }
    fn items(node: &Node, prefix: &str, expanded: bool) -> Vec<TreeItem> {
        let mut result: Vec<TreeItem> = node
            .folders
            .iter()
            .map(|(name, child)| {
                let id = format!("{prefix}{name}/");
                TreeItem::new(format!("dir:{id}"), name.clone()).expanded(expanded).children(items(child, &id, expanded))
            })
            .collect();
        result.extend(node.files.iter().map(|(path, name)| TreeItem::new(path.clone(), name.clone())));
        result
    }
    items(&root, "", expanded)
}

#[cfg(feature = "snapshot")]
pub const SNAPSHOT_STEPS: &[&str] = &["file", "raw", "image", "overview"];

#[cfg(feature = "snapshot")]
pub fn snapshot_ready(handle: AnyWindowHandle, cx: &mut AsyncApp) -> bool {
    cx.update(|cx| {
        handle
            .downcast::<gpui_kit::base::Root>()
            .and_then(|root| root.update(cx, |root, _, cx| root.view().clone().downcast::<Workspace>().ok().map(|w| w.read(cx).loaded().is_some())).ok())
            .flatten()
            .unwrap_or(true)
    })
}

#[cfg(feature = "snapshot")]
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

#[cfg(test)]
mod tests {
    use super::{Artifact, Confidence, Content, build_tree, source_span};
    use wxlocated_engine::decompile::{ArtifactKind, SourceRef};

    fn artifact(entry: &str, start: usize, end: usize) -> Artifact {
        Artifact {
            path: "pages/a.wxml".into(),
            kind: ArtifactKind::Markup,
            confidence: Confidence::High,
            content: Content::Generated(String::new()),
            sources: vec![SourceRef { package: "主包".into(), entry_path: entry.into(), start: Some(start), end: Some(end) }],
            notes: Vec::new(),
        }
    }

    #[test]
    fn tree_puts_folders_first_and_nests() {
        let items = build_tree(&["b.js", "pages/x/index.wxml", "pages/x/index.js", "a.json"], true);
        let labels: Vec<_> = items.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(labels, ["pages", "b.js", "a.json"]);
        assert_eq!(items[0].children[0].label.as_ref(), "x");
        assert_eq!(items[0].children[0].children.len(), 2);
    }

    #[test]
    fn source_span_in_a_script_entry_accounts_for_a_bom() {
        let raw = "\u{FEFF}var a=1;var b=2;";
        let span = source_span(&artifact("app-service.js", 8, 16), raw).expect("span");
        assert_eq!(&raw[span], "var b=2;");
    }

    #[test]
    fn source_span_in_an_html_container_finds_the_script_text() {
        let raw = "<html><script>var a=1;</script><p>x</p><script>var b=2;</script></html>";
        let joined = wxlocated_engine::decompile::wxml::extract_scripts(raw);
        let start = joined.find("var b=2;").expect("script text");
        let span = source_span(&artifact("page-frame.html", start, start + 8), raw).expect("span");
        assert_eq!(&raw[span], "var b=2;");
    }

    #[test]
    fn out_of_range_sources_have_no_span() {
        assert!(source_span(&artifact("x.js", 5, 500), "short").is_none());
    }
}
