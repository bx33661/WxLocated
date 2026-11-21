//! File layers, background preview preparation and editor reading bookmarks.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, GutterDecoration, OverviewRulerDecoration, OverviewRulerLane, Position, RangeDecoration,
    RangeDecorationStyle,
};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use wxlocated_engine::decompile::{Artifact, Content};
use wxlocated_engine::export::confidence_title;
use wxlocated_engine::overlay::{Layer as OverlayLayer, OverlayFile};

use crate::library::{Library, image_format};
use crate::theme::{palette, soft_shadow};
use crate::ui::{self, file_icon, file_tint, format_size};

use super::inspector::{build_evidence, source_span};
use super::{Loaded, Workspace, kind_title};

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

    pub(super) fn select(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn set_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_reading(cx);
        self.view = view;
        self.compare = false;
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

    pub(super) fn export_current_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((loaded, index)) = self.selected.as_ref().and_then(|p| self.artifact(p)) else { return };
        let artifact = &loaded.result.artifacts[index];
        let name = artifact.path.rsplit('/').next().unwrap_or(&artifact.path).to_string();
        let bytes = self.current_bytes(&loaded, artifact);
        crate::export::save_bytes(name, move || bytes, window, cx);
    }

    pub(super) fn render_file(&self, path: &str, cx: &mut Context<Self>) -> AnyElement {
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
                    .tooltip(ui::shortcut("回到概要（⌘0）"))
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
                    .child(div().text_color(palette.tertiary).child(ui::shortcut("⌥⌘R 切回重建"))),
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
    let identical = view == View::Raw || (view == View::Rebuilt && matches!(artifact.content, Content::Entry(_)));
    let raw_size = artifact
        .sources
        .first()
        .and_then(|s| loaded.set.entries.iter().find(|e| e.path == s.entry_path && loaded.set.source_title(e) == s.package))
        .map_or(0, |e| e.size);
    let evidence = if identical || raw_size <= 512 * 1024 {
        build_evidence(loaded, index, view, &text, (!compare).then_some(1), false).ok()
    } else {
        None
    };
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
