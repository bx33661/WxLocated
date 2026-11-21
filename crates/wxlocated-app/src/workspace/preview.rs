//! Code preview, editor tabs, navigation history, miniapp 4-part companion
//! switcher (`JS` / `WXML` / `WXSS` / `JSON`), syntax-highlighted Agent diff,
//! and bottom forensic evidence status bar.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::input::{
    Editor, EditorState, GutterDecoration, OverviewRulerDecoration, OverviewRulerLane, Position, RangeDecoration,
    RangeDecorationStyle,
};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use wxlocated_engine::decompile::{Artifact, ArtifactKind, Content};
use wxlocated_engine::export::confidence_title;
use wxlocated_engine::overlay::{Layer as OverlayLayer, OverlayFile};

use crate::library::{Library, image_format};
use crate::theme::{palette, soft_shadow};
use crate::ui::{self, badge, file_icon, file_tint, format_size};

use super::inspector::{build_evidence, evidence_method, source_span};
use super::{BrowseMode, Loaded, Workspace, WorkspaceTab};

/// Which layer of the selected file is shown.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum View {
    Rebuilt,
    Raw,
    /// Index into `Workspace::layers`.
    Agent(usize),
}

pub(super) enum Preview {
    None,
    /// Reading in the background.
    Loading,
    Code(Entity<EditorState>),
    Diff(Entity<DiffState>),
    IdenticalDiff,
    Image(Arc<Image>, Option<(u32, u32)>),
    Binary(u64),
    /// Text too large to open without asking (size in bytes).
    TooLarge(u64),
    Error(String),
}

/// Text above this opens only on request: multi-megabyte containers would stall layout.
const LARGE_TEXT: usize = 3 * 1024 * 1024;
const MAX_OPEN_TABS: usize = 12;
const MAX_HISTORY: usize = 50;

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
    Diff {
        patch: Option<String>,
        language: &'static str,
        agent_diff: (usize, usize),
    },
    Binary(u64),
    TooLarge(u64),
    Error(String),
}

pub(super) type ReadingKey = (usize, usize, View, bool);

/// Small per-view bookmarks; only the visible editor retains source text.
#[derive(Clone)]
pub(super) struct ReadingState {
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

impl Workspace {
    fn reading_key(&self) -> Option<ReadingKey> {
        let path = self.selected.as_deref()?;
        let loaded = self.loaded()?;
        Some((self.package, *loaded.index.get(path)?, self.view, self.compare))
    }

    pub(super) fn remember_reading(&mut self, cx: &App) {
        let Some(key) = self.reading_key() else { return };
        self.last_views.insert((key.0, key.1), (self.view, self.compare));
        if let Preview::Code(editor) = &self.preview {
            let state = editor.read(cx);
            let search = state.search_session();
            self.readings.insert(
                key,
                ReadingState {
                    selection: state.selected_range(),
                    reversed: state.is_selection_reversed(),
                    scroll: state.scroll_offset(),
                    query: search.query.clone(),
                    search_open: search.open,
                    search_active: search.is_active(),
                    case_insensitive: search.case_insensitive,
                    search_match: search.matcher.current_match_index(),
                    open_large: self.open_large,
                },
            );
        }
    }

    pub(super) fn select(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_file(path, None, true, window, cx);
    }

    pub(super) fn open_at_line(&mut self, path: String, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = WorkspaceTab::Code;
        self.navigate_file(Some(path), line, true, window, cx);
    }

    fn navigate_file(&mut self, path: Option<String>, jump_line: Option<u32>, push_history: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = &path
            && self.selected.as_deref() == Some(target.as_str())
        {
            if let Some(line) = jump_line
                && let Preview::Code(editor) = &self.preview
            {
                let zero_based = line.saturating_sub(1);
                editor.update(cx, |state, cx| {
                    state.set_cursor_position(Position::new(zero_based, 0), window, cx);
                });
            }
            cx.notify();
            return;
        }

        self.remember_reading(cx);
        if let Some(target) = &path {
            if push_history && let Some(prev) = self.selected.clone() {
                if self.history_back.last().map(String::as_str) != Some(prev.as_str()) {
                    self.history_back.push(prev);
                    if self.history_back.len() > MAX_HISTORY {
                        self.history_back.remove(0);
                    }
                }
                self.history_forward.clear();
            }
            if !self.open_tabs.iter().any(|open| open == target) {
                if self.open_tabs.len() >= MAX_OPEN_TABS {
                    if let Some(evict) = self.open_tabs.iter().position(|open| self.selected.as_deref() != Some(open.as_str())) {
                        self.open_tabs.remove(evict);
                    } else {
                        self.open_tabs.remove(0);
                    }
                }
                self.open_tabs.push(target.clone());
            }
            let saved = self.artifact(target).and_then(|(_, index)| self.last_views.get(&(self.package, index)).copied());
            (self.view, self.compare) = saved.unwrap_or((View::Rebuilt, false));
            if let View::Agent(index) = self.view
                && self.layer_file(index, target).is_none()
            {
                self.view = View::Rebuilt;
                self.compare = false;
            }
            self.open_large = false;
        }
        self.pending_jump_line = jump_line;
        self.selected = path;

        let (id, persisted_path) = (self.app.app_id.clone(), self.selected.clone());
        Library::global(cx).update(cx, |library, cx| {
            let changed = match persisted_path {
                Some(path) => library.settings.last_files.insert(id, path.clone()).as_deref() != Some(path.as_str()),
                None => library.settings.last_files.remove(&id).is_some(),
            };
            if changed {
                library.save_settings(cx);
            }
        });
        self.refresh_preview(window, cx);
    }

    pub(super) fn close_tab(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pos) = self.open_tabs.iter().position(|open| open == path) else { return };
        self.open_tabs.remove(pos);
        if self.selected.as_deref() == Some(path) {
            let next = self
                .open_tabs
                .get(pos)
                .cloned()
                .or_else(|| self.open_tabs.last().cloned());
            self.navigate_file(next, None, false, window, cx);
        } else {
            cx.notify();
        }
    }

    pub(super) fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prev) = self.history_back.pop() else { return };
        if let Some(current) = self.selected.clone() {
            self.history_forward.push(current);
        }
        self.navigate_file(Some(prev), None, false, window, cx);
    }

    pub(super) fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.history_forward.pop() else { return };
        if let Some(current) = self.selected.clone() {
            self.history_back.push(current);
        }
        self.navigate_file(Some(next), None, false, window, cx);
    }

    pub(super) fn toggle_word_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = !Library::global(cx).read(cx).settings.editor_word_wrap;
        Library::global(cx).update(cx, |library, cx| {
            library.settings.editor_word_wrap = next;
            library.save_settings(cx);
        });
        if let Preview::Code(editor) = &self.preview {
            editor.update(cx, |state, cx| state.set_soft_wrap(next, window, cx));
        }
        cx.notify();
    }

    pub(super) fn set_view(&mut self, view: View, compare: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_reading(cx);
        self.view = view;
        self.compare = compare;
        self.refresh_preview(window, cx);
    }

    /// The raw entry behind a rebuilt file, when it differs from it.
    pub(super) fn raw_entry<'a>(loaded: &'a Loaded, artifact: &Artifact) -> Option<&'a wxlocated_engine::archive::SetEntry> {
        artifact.content.generated_text()?;
        let source = artifact.sources.first()?;
        loaded.set.entries.iter().find(|e| e.path == source.entry_path && loaded.set.source_title(e) == source.package)
    }

    fn layer_file<'a>(&'a self, index: usize, base: &str) -> Option<(&'a OverlayLayer, &'a OverlayFile, PathBuf)> {
        let layer = self.layers.get(index)?;
        let file = layer.manifest.files.iter().find(|f| f.base.as_deref() == Some(base))?;
        Some((layer, file, layer.directory.join(&file.path)))
    }

    fn current_bytes(&self, loaded: &Loaded, artifact: &Artifact) -> Result<Vec<u8>, String> {
        let layer = self.layer_path(&artifact.path);
        read_view(loaded, artifact, self.view, layer.as_deref())
    }

    fn layer_path(&self, path: &str) -> Option<PathBuf> {
        match self.view {
            View::Agent(index) => self.layer_file(index, path).map(|(_, _, path)| path),
            _ => None,
        }
    }

    pub(super) fn refresh_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_generation += 1;
        self.evidence = None;
        self.agent_diff = None;
        let generation = self.preview_generation;
        let Some(path) = self.selected.clone() else {
            self.preview = Preview::None;
            cx.notify();
            return;
        };
        let Some((loaded, index)) = self.artifact(&path) else {
            self.preview = Preview::None;
            cx.notify();
            return;
        };
        if let Some(saved) = self.reading_key().and_then(|key| self.readings.get(&key)) {
            self.open_large |= saved.open_large;
        }
        self.preview = Preview::Loading;
        cx.notify();
        let (view, compare, force) = (self.view, self.compare, self.open_large);
        let layer = self.layer_path(&path);
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
        let word_wrap = Library::global(cx).read(cx).settings.editor_word_wrap;
        let jump_line = self.pending_jump_line.take();
        match prepared {
            Prepared::Image(bytes, format, dimensions) => Preview::Image(Arc::new(Image::from_bytes(format, bytes)), dimensions),
            Prepared::Binary(size) => Preview::Binary(size),
            Prepared::TooLarge(size) => Preview::TooLarge(size),
            Prepared::Error(message) => Preview::Error(message),
            Prepared::Diff { patch, language, agent_diff } => {
                self.agent_diff = Some(agent_diff);
                match patch.and_then(|p| DiffFile::parse(&p).ok()).filter(|files| !files.is_empty()) {
                    Some(files) => {
                        let files: Vec<DiffFile> = files.into_iter().map(|f| f.with_language(language)).collect();
                        let mode = self.diff_mode;
                        let diff_state = cx.new(|cx| DiffState::new(files, cx).with_mode(mode));
                        Preview::Diff(diff_state)
                    }
                    None => Preview::IdenticalDiff,
                }
            }
            Prepared::Text { text, language, highlight, evidence, agent_diff } => {
                self.evidence = evidence;
                self.agent_diff = agent_diff;
                let saved = self.reading_key().and_then(|key| self.readings.get(&key)).cloned();
                let highlight_lines = highlight.as_ref().map(|range| {
                    (
                        text[..range.start].bytes().filter(|byte| *byte == b'\n').count() as u32,
                        text.bytes().filter(|byte| *byte == b'\n').count() as u32,
                    )
                });
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .searchable(true)
                        .soft_wrap(word_wrap)
                        .default_value(text)
                });
                if let Some(range) = highlight {
                    let (line, total) = highlight_lines.unwrap();
                    let accent = palette(cx).accent;
                    editor.update(cx, |state, cx| {
                        if saved.is_none() && jump_line.is_none() {
                            state.set_cursor_position(Position::new((line + 28).min(total), 0), window, cx);
                            state.set_cursor_position(Position::new(line, 0), window, cx);
                        }
                        let _ = state.create_range_decorations_collection(vec![RangeDecoration::new(range.clone()).with_style(RangeDecorationStyle::Fill).with_color(accent.opacity(0.13))], cx);
                        let _ = state.create_gutter_decorations_collection(vec![GutterDecoration::new(range.clone()).with_color(accent)], cx);
                        let _ = state.create_overview_ruler_decorations_collection(vec![OverviewRulerDecoration::new(range).with_lane(OverviewRulerLane::Left).with_color(accent)], cx);
                    });
                }
                if let Some(line) = jump_line {
                    let zero_based = line.saturating_sub(1);
                    editor.update(cx, |state, cx| {
                        state.set_cursor_position(Position::new(zero_based.saturating_add(16), 0), window, cx);
                        state.set_cursor_position(Position::new(zero_based, 0), window, cx);
                    });
                } else if let Some(saved) = saved {
                    editor.update(cx, |state, cx| {
                        if saved.search_open {
                            state.open_search(false, cx);
                        }
                        state.set_search_query(saved.query, saved.case_insensitive, cx);
                        let count = state.search_session().matcher.len();
                        let target = saved.search_match.min(count.saturating_sub(1));
                        if target <= count / 2 {
                            for _ in 0..target {
                                state.next_search_match(cx);
                            }
                        } else {
                            for _ in 0..count - target {
                                state.previous_search_match(cx);
                            }
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

    pub(super) fn export_current_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((loaded, index)) = self.selected.as_ref().and_then(|p| self.artifact(p)) else { return };
        let artifact = &loaded.result.artifacts[index];
        let name = artifact.path.rsplit('/').next().unwrap_or(&artifact.path).to_string();
        let bytes = self.current_bytes(&loaded, artifact);
        crate::export::save_bytes(name, move || bytes, window, cx);
    }

    pub(super) fn render_code_stage(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let Some(path) = self.selected.as_deref() else {
            return ui::empty_state(
                Some(IconName::Code),
                "选择文件查看还原源码",
                "点击左侧页面或目录树中的任意文件，也可以使用 ⇧⌘F 全局检索接口与敏感调用。",
                &palette,
            )
            .child(
                h_flex()
                    .mt_2()
                    .gap_2()
                    .child(Button::new("empty-pick-search").icon(IconName::FileSearch).label("全文搜索 ⇧⌘F").small().on_click(cx.listener(|this, _, window, cx| {
                        this.browse_mode = BrowseMode::Search;
                        this.global_search.update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    })))
                    .child(Button::new("empty-pick-pkgs").icon(IconName::Layers).label("查看分包监控").small().ghost().on_click(cx.listener(|this, _, _, cx| {
                        this.tab = WorkspaceTab::Packages;
                        cx.notify();
                    }))),
            )
            .into_any_element();
        };
        let Some((loaded, index)) = self.artifact(path) else {
            return div().flex_1().bg(palette.code).into_any_element();
        };
        let artifact = &loaded.result.artifacts[index];
        let word_wrap = Library::global(cx).read(cx).settings.editor_word_wrap;
        let has_raw = Self::raw_entry(&loaded, artifact).is_some();

        // Layer / view switcher options.
        let mut layer_options: Vec<(View, bool, String)> = vec![(View::Rebuilt, false, "还原源码".into())];
        if has_raw {
            layer_options.push((View::Raw, false, "原始字节".into()));
        }
        for (layer_idx, layer) in self.layers.iter().enumerate() {
            if self.layer_file(layer_idx, path).is_some() {
                let name = if self.layers.len() > 1 { format!("Agent · {}", layer.manifest.name) } else { "Agent".into() };
                layer_options.push((View::Agent(layer_idx), false, name.clone()));
                if artifact.content.generated_text().is_some() {
                    let cmp_label = if self.layers.len() > 1 { format!("对比 · {}", layer.manifest.name) } else { "Agent 对比".into() };
                    layer_options.push((View::Agent(layer_idx), true, cmp_label));
                }
            }
        }
        let selected_layer_ix = layer_options
            .iter()
            .position(|(v, cmp, _)| *v == self.view && *cmp == self.compare)
            .unwrap_or(0);
        let layer_targets: Vec<(View, bool)> = layer_options.iter().map(|(v, c, _)| (*v, *c)).collect();

        // Row 1: History Back/Forward + Open Tabs
        let tabs_bar = h_flex()
            .h(px(34.))
            .flex_none()
            .px_2()
            .gap_1p5()
            .bg(palette.code_chrome)
            .border_b_1()
            .border_color(palette.hairline)
            .child(
                h_flex()
                    .gap_0p5()
                    .child(
                        Button::new("hist-back")
                            .icon(IconName::ArrowLeft)
                            .xsmall()
                            .ghost()
                            .disabled(self.history_back.is_empty())
                            .tooltip("后退")
                            .on_click(cx.listener(|this, _, window, cx| this.navigate_back(window, cx))),
                    )
                    .child(
                        Button::new("hist-fwd")
                            .icon(IconName::ArrowRight)
                            .xsmall()
                            .ghost()
                            .disabled(self.history_forward.is_empty())
                            .tooltip("前进")
                            .on_click(cx.listener(|this, _, window, cx| this.navigate_forward(window, cx))),
                    ),
            )
            .child(div().w(px(1.)).h(px(16.)).bg(palette.hairline))
            .child(
                h_flex()
                    .id("editor-tabs")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .gap_1()
                    .items_center()
                    .children(self.open_tabs.iter().enumerate().map(|(ix, tab_path)| {
                        let active = self.selected.as_deref() == Some(tab_path.as_str());
                        let name = tab_path.rsplit('/').next().unwrap_or(tab_path).to_string();
                        let select_path = tab_path.clone();
                        let close_path = tab_path.clone();
                        h_flex()
                            .id(SharedString::from(format!("etab-{ix}")))
                            .flex_none()
                            .h(px(26.))
                            .pl_2p5()
                            .pr_1()
                            .gap_1p5()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(active, |this| {
                                this.bg(palette.code).border_1().border_color(palette.hairline).text_color(palette.text).font_weight(FontWeight::MEDIUM)
                            })
                            .when(!active, |this| this.text_color(palette.secondary).hover(|s| s.bg(palette.fill).text_color(palette.text)))
                            .child(div().text_size(px(11.5)).text_color(file_tint(tab_path, &palette)).child(file_icon(tab_path)))
                            .child(div().text_size(px(11.5)).font_family(ui::mono_font()).child(name))
                            .child(
                                Button::new(SharedString::from(format!("etab-close-{ix}")))
                                    .icon(IconName::Close)
                                    .xsmall()
                                    .ghost()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_tab(&close_path, window, cx);
                                    })),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select(Some(select_path.clone()), window, cx);
                            }))
                    })),
            );

        // Row 2: Path + 4-Part Miniapp Page Companion Switcher + Word Wrap + Layer Tabs
        let companions = companion_siblings(path, &loaded.index);
        let companion_bar = h_flex()
            .h(px(36.))
            .flex_none()
            .px_3()
            .gap_2p5()
            .bg(palette.code)
            .border_b_1()
            .border_color(palette.hairline)
            .child(div().text_color(file_tint(path, &palette)).child(file_icon(path)))
            .child(div().min_w_0().flex_shrink(1.).truncate().text_size(px(11.5)).font_family(ui::mono_font()).text_color(palette.secondary).child(path.to_string()))
            .when_some(companions, |this, siblings| {
                this.child(div().w(px(1.)).h(px(14.)).bg(palette.hairline)).child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .children(siblings.into_iter().map(|(label, sib_path)| {
                            let active = sib_path == path;
                            Button::new(SharedString::from(format!("comp-{label}")))
                                .label(label)
                                .xsmall()
                                .when(active, |b| b.primary())
                                .when(!active, |b| b.ghost())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select(Some(sib_path.clone()), window, cx);
                                }))
                        })),
                )
            })
            .child(div().flex_1())
            .when(matches!(self.preview, Preview::Diff(_)), |this| {
                let unified = self.diff_mode == DiffMode::Unified;
                this.child(
                    Button::new("diff-mode-toggle")
                        .icon(IconName::Columns2)
                        .label(if unified { "统一视图" } else { "并排视图" })
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.diff_mode = match this.diff_mode {
                                DiffMode::Unified => DiffMode::Split,
                                _ => DiffMode::Unified,
                            };
                            if let Preview::Diff(state) = &this.preview {
                                let mode = this.diff_mode;
                                state.update(cx, |s, cx| s.set_mode(mode, cx));
                            }
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("word-wrap-toggle")
                    .icon(IconName::TextWrap)
                    .label("自动换行")
                    .xsmall()
                    .ghost()
                    .selected(word_wrap)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_word_wrap(window, cx))),
            )
            .when(layer_options.len() > 1, |this| {
                this.child(
                    TabBar::new("views")
                        .segmented()
                        .xsmall()
                        .selected_index(selected_layer_ix)
                        .children(layer_options.iter().map(|(_, _, label)| Tab::new().label(label.clone())))
                        .on_click(cx.listener(move |this, index: &usize, window, cx| {
                            if let Some((view, compare)) = layer_targets.get(*index) {
                                this.set_view(*view, *compare, window, cx);
                            }
                        })),
                )
            });

        let banner = match self.view {
            View::Agent(layer_idx) => self.layer_file(layer_idx, path).map(|(layer, file, _)| {
                let manifest = &layer.manifest;
                let meta = [
                    Some(manifest.name.clone()),
                    manifest.author.agent.clone(),
                    manifest.author.model.clone(),
                    file.note.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                let stats = self.agent_diff;
                h_flex()
                    .flex_none()
                    .gap_2()
                    .px_3()
                    .py(px(6.))
                    .bg(palette.warning.opacity(0.09))
                    .border_b_1()
                    .border_color(palette.warning.opacity(0.24))
                    .child(div().text_color(palette.warning).child(IconName::Sparkles))
                    .child(div().text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).child("Agent 推断视图"))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(palette.secondary).child(meta))
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
                    .child(div().text_size(px(11.)).text_color(palette.tertiary).child("证据引用请切换至“原始字节”"))
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
                    .child(div().flex_1().child("原始容器字节（绿色高亮区间即为当前文件源码所在位置）"))
                    .child(div().text_color(palette.tertiary).child(ui::shortcut("⌥⌘R 切回还原源码"))),
            ),
            View::Rebuilt => None,
        };

        let body: AnyElement = match &self.preview {
            Preview::Code(editor) => div()
                .flex_1()
                .min_h_0()
                .bg(palette.code)
                .child(Editor::new(editor).aria_label(format!("源码预览 {path}")).readonly(true).bordered(false).h_full())
                .into_any_element(),
            Preview::Diff(diff_state) => div()
                .flex_1()
                .min_h_0()
                .bg(palette.code)
                .child(Diff::new(diff_state).header_visible(false).soft_wrap(word_wrap).size_full())
                .into_any_element(),
            Preview::IdenticalDiff => div()
                .flex_1()
                .bg(palette.code)
                .child(ui::empty_state(Some(IconName::CircleCheck), "内容完全一致", "当前 Agent 重建层与静态还原源码在此文件上没有文本差异。", &palette))
                .into_any_element(),
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
                .child(ui::empty_state(Some(IconName::FileArchive), "二进制文件", format!("{}，无法以文本显示，可在下方导出保存。", format_size(*size as i64)), &palette))
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
                        format!("{}，打开后语法高亮需要一点时间；你也可以直接导出或通过 ⇧⌘F 检索。", format_size(*size as i64)),
                        &palette,
                    )
                    .child(Button::new("open-large").label("仍然打开").primary().mt_2().on_click(cx.listener(|this, _, window, cx| {
                        this.open_large = true;
                        this.refresh_preview(window, cx);
                    }))),
                )
                .into_any_element(),
        };

        // Slim 28px Bottom Forensic Evidence Status Bar
        let size_bytes = match &artifact.content {
            Content::Generated(text) => text.len() as i64,
            Content::Entry(entry) => loaded.set.entries[*entry].size as i64,
        };
        let conf_color = match artifact.confidence {
            wxlocated_engine::decompile::Confidence::Exact => palette.success,
            wxlocated_engine::decompile::Confidence::High => palette.accent,
            _ => palette.warning,
        };
        let source_summary = artifact
            .sources
            .first()
            .map(|s| match (s.start, s.end) {
                (Some(start), Some(end)) => format!("{} · {} · {start}..<{end}", s.package, s.entry_path),
                _ => format!("{} · {}", s.package, s.entry_path),
            })
            .unwrap_or_else(|| "无原始包来源".into());
        let precision = self.evidence.as_ref().map(evidence_method).unwrap_or("按需定位");
        let path_to_copy = artifact.path.clone();

        let status_bar = h_flex()
            .h(px(28.))
            .flex_none()
            .px_3()
            .gap_2p5()
            .bg(palette.code_chrome)
            .border_t_1()
            .border_color(palette.hairline)
            .text_size(px(11.))
            .child(badge(confidence_title(artifact.confidence), conf_color))
            .child(div().text_color(palette.secondary).child(format_size(size_bytes)))
            .child(div().text_color(palette.tertiary).child("·"))
            .child(div().flex_1().min_w_0().truncate().font_family(ui::mono_font()).text_color(palette.secondary).child(format!("{source_summary} · {precision}")))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Button::new("status-copy-path")
                            .icon(IconName::Copy)
                            .label("复制路径")
                            .xsmall()
                            .ghost()
                            .on_click(move |_, _, cx| ui::copy(path_to_copy.clone(), cx)),
                    )
                    .child(
                        Button::new("status-export-file")
                            .icon(IconName::FileDown)
                            .label("导出文件")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| this.export_current_file(window, cx))),
                    )
                    .child(
                        Button::new("status-copy-evidence")
                            .icon(IconName::ScanSearch)
                            .label("复制当前行证据")
                            .xsmall()
                            .loading(self.copying_evidence)
                            .disabled(self.copying_evidence || matches!(self.preview, Preview::Loading | Preview::TooLarge(_) | Preview::Error(_) | Preview::Binary(_)))
                            .tooltip("按当前光标定位，复制原始包 SHA-256、条目名称与字节偏移 JSON")
                            .on_click(cx.listener(|this, _, window, cx| this.copy_evidence(window, cx))),
                    ),
            );

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(tabs_bar)
            .child(companion_bar)
            .children(banner)
            .child(body)
            .child(status_bar)
            .into_any_element()
    }
}

/// Lists existing sibling files (`JS`, `WXML`, `WXSS`, `JSON`) when the current
/// path belongs to a page/component with at least 2 companion files.
fn companion_siblings(path: &str, index: &HashMap<String, usize>) -> Option<Vec<(&'static str, String)>> {
    if path.starts_with("__wxlocated_") {
        return None;
    }
    let stem = ["js", "wxml", "wxss", "json"]
        .iter()
        .find_map(|ext| path.strip_suffix(&format!(".{ext}")))?;
    let candidates = [
        ("JS 逻辑", "js"),
        ("WXML 结构", "wxml"),
        ("WXSS 样式", "wxss"),
        ("JSON 配置", "json"),
    ];
    let siblings: Vec<(&'static str, String)> = candidates
        .into_iter()
        .filter_map(|(label, ext)| {
            let candidate = format!("{stem}.{ext}");
            index.contains_key(&candidate).then_some((label, candidate))
        })
        .collect();
    (siblings.len() >= 2).then_some(siblings)
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
    let is_image = view != View::Raw || artifact.kind == ArtifactKind::Image;
    if let Some(format) = image_format(&bytes).filter(|_| is_image) {
        let dimensions = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
        return Prepared::Image(bytes, format, dimensions);
    }
    let size = bytes.len() as u64;
    let Ok(text) = String::from_utf8(bytes) else { return Prepared::Binary(size) };
    if text.len() > LARGE_TEXT && !force {
        return Prepared::TooLarge(size);
    }
    if let (View::Agent(_), true, Some(base)) = (view, compare, artifact.content.generated_text()) {
        let agent_diff = crate::diff::counts(base, &text);
        let patch = crate::diff::git_patch(path, base, &text);
        return Prepared::Diff { patch, language: ui::language_for(path), agent_diff };
    }
    let identical = view == View::Raw || (view == View::Rebuilt && matches!(artifact.content, Content::Entry(_)));
    let raw_size = artifact
        .sources
        .first()
        .and_then(|s| loaded.set.entries.iter().find(|e| e.path == s.entry_path && loaded.set.source_title(e) == s.package))
        .map_or(0, |e| e.size);
    let evidence = if identical || raw_size <= 512 * 1024 {
        build_evidence(loaded, index, view, &text, Some(1), false).ok()
    } else {
        None
    };
    let agent_diff = match (view, artifact.content.generated_text()) {
        (View::Agent(_), Some(base)) => Some(crate::diff::counts(base, &text)),
        _ => None,
    };
    let language = if view == View::Raw { "javascript" } else { ui::language_for(path) };
    let highlight = (view == View::Raw).then(|| source_span(artifact, &text)).flatten();
    Prepared::Text { text, language, highlight, evidence, agent_diff }
}
