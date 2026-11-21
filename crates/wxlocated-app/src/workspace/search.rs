//! Workspace-wide source code search (`⇧⌘F`), running off the main thread.

use std::borrow::Cow;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wxlocated_engine::decompile::{Artifact, ArtifactKind, Content};

use crate::theme::palette;
use crate::ui;

use super::{Loaded, Workspace};

const MAX_ENTRY_SEARCH_BYTES: u64 = 2 * 1024 * 1024;
const MAX_HITS_PER_FILE: usize = 50;
const MAX_TOTAL_HITS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchHit {
    pub path: String,
    pub line: u32,
    pub preview: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SearchKindFilter {
    All,
    Script,
    Markup,
    Style,
    Config,
}

#[derive(Default)]
pub(super) struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    pub matched_files: usize,
    pub truncated: bool,
}

impl SearchKindFilter {
    fn matches(self, kind: ArtifactKind) -> bool {
        match self {
            Self::All => true,
            Self::Script => matches!(kind, ArtifactKind::Script | ArtifactKind::Wxs),
            Self::Markup => kind == ArtifactKind::Markup,
            Self::Style => kind == ArtifactKind::Style,
            Self::Config => kind == ArtifactKind::Configuration,
        }
    }
}

pub(super) fn search_loaded(
    loaded: &Loaded,
    query: &str,
    case_sensitive: bool,
    kind_filter: SearchKindFilter,
    include_containers: bool,
    limit: usize,
) -> SearchOutcome {
    search_artifacts(
        &loaded.result.artifacts,
        query,
        case_sensitive,
        kind_filter,
        include_containers,
        limit,
        |entry_index| {
            let entry = loaded.set.entries.get(entry_index)?;
            if entry.size > MAX_ENTRY_SEARCH_BYTES {
                return None;
            }
            let bytes = loaded.set.read_limited(entry, MAX_ENTRY_SEARCH_BYTES).ok()?;
            String::from_utf8(bytes).ok().map(Cow::Owned)
        },
    )
}

pub(super) fn search_artifacts<'a>(
    artifacts: &'a [Artifact],
    query: &str,
    case_sensitive: bool,
    kind_filter: SearchKindFilter,
    include_containers: bool,
    limit: usize,
    read_entry: impl Fn(usize) -> Option<Cow<'a, str>>,
) -> SearchOutcome {
    let trimmed = query.trim();
    if trimmed.is_empty() || limit == 0 {
        return SearchOutcome::default();
    }
    let needle_lower = if case_sensitive { String::new() } else { trimmed.to_lowercase() };
    let mut hits = Vec::new();
    let mut matched_files = 0usize;
    let mut truncated = false;

    for artifact in artifacts {
        if !include_containers && artifact.path.starts_with("__wxlocated_") {
            continue;
        }
        if !kind_filter.matches(artifact.kind) {
            continue;
        }
        if matches!(artifact.kind, ArtifactKind::Image | ArtifactKind::Font | ArtifactKind::Media | ArtifactKind::Binary) {
            continue;
        }
        let text: Option<Cow<'a, str>> = match &artifact.content {
            Content::Generated(text) => Some(Cow::Borrowed(text.as_str())),
            Content::Entry(index) => read_entry(*index),
        };
        let Some(text) = text else { continue };
        let mut file_hits = 0usize;
        let mut file_matched = false;
        for (idx, line) in text.lines().enumerate() {
            if !contains_match(line, trimmed, &needle_lower, case_sensitive) {
                continue;
            }
            if !file_matched {
                matched_files += 1;
                file_matched = true;
            }
            hits.push(SearchHit {
                path: artifact.path.clone(),
                line: (idx + 1) as u32,
                preview: summarize_line(line),
            });
            file_hits += 1;
            if hits.len() >= limit {
                truncated = true;
                return SearchOutcome { hits, matched_files, truncated };
            }
            if file_hits >= MAX_HITS_PER_FILE {
                truncated = true;
                break;
            }
        }
    }

    SearchOutcome { hits, matched_files, truncated }
}

fn contains_match(line: &str, needle: &str, needle_lower: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        return line.contains(needle);
    }
    if needle.is_ascii() && line.is_ascii() {
        let hay = line.as_bytes();
        let target = needle.as_bytes();
        return hay.windows(target.len()).any(|window| window.eq_ignore_ascii_case(target));
    }
    line.to_lowercase().contains(needle_lower)
}

fn summarize_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= 140 {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(139).collect();
    format!("{truncated}…")
}

impl Workspace {
    pub(super) fn trigger_global_search(&mut self, cx: &mut Context<Self>) {
        self.search_generation += 1;
        let generation = self.search_generation;
        let query = self.global_search.read(cx).value().to_string();
        if query.trim().is_empty() {
            self.search_running = false;
            self.search_hits.clear();
            self.search_matched_files = 0;
            self.search_truncated = false;
            cx.notify();
            return;
        }
        let Some(loaded) = self.loaded() else {
            self.search_running = false;
            self.search_hits.clear();
            self.search_matched_files = 0;
            self.search_truncated = false;
            cx.notify();
            return;
        };
        let case_sensitive = self.search_case_sensitive;
        let kind_filter = self.search_kind;
        let include_containers = self.search_include_containers;
        self.search_running = true;
        cx.notify();

        let task = cx.background_executor().spawn(async move {
            search_loaded(&loaded, &query, case_sensitive, kind_filter, include_containers, MAX_TOTAL_HITS)
        });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search_running = false;
                this.search_hits = outcome.hits;
                this.search_matched_files = outcome.matched_files;
                this.search_truncated = outcome.truncated;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_global_search(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let query = self.global_search.read(cx).value().trim().to_string();
        let filters = [
            (SearchKindFilter::All, "全部"),
            (SearchKindFilter::Script, "JS"),
            (SearchKindFilter::Markup, "WXML"),
            (SearchKindFilter::Style, "WXSS"),
            (SearchKindFilter::Config, "JSON"),
        ];

        let summary_text = if query.is_empty() {
            "输入接口路径、Token 键名、密钥字面量或关键字搜索".to_string()
        } else if self.search_running {
            "正在检索全部还原源码…".to_string()
        } else if self.search_hits.is_empty() {
            "当前包内没有匹配项".to_string()
        } else if self.search_truncated {
            format!("显示前 {} 处匹配（{} 个文件，结果已截断）", self.search_hits.len(), self.search_matched_files)
        } else {
            format!("在 {} 个文件中找到 {} 处匹配", self.search_matched_files, self.search_hits.len())
        };

        let results: AnyElement = if query.is_empty() {
            div()
                .flex_1()
                .p_4()
                .child(ui::empty_state(Some(IconName::FileSearch), "跨文件源码搜索", "支持接口 URL、wx.request、密钥字面量、字段名等全包检索。", &palette))
                .into_any_element()
        } else if self.search_running && self.search_hits.is_empty() {
            h_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_size(px(12.))
                .text_color(palette.secondary)
                .child(Spinner::new().color(palette.accent))
                .child("正在搜索…")
                .into_any_element()
        } else if self.search_hits.is_empty() {
            div()
                .flex_1()
                .p_4()
                .child(ui::empty_state(Some(IconName::Search), "没有找到匹配内容", "可以尝试取消区分大小写，或切换到“全部”文件类型。", &palette))
                .into_any_element()
        } else {
            div()
                .id("global-search-results")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .p_2()
                        .gap_1()
                        .children(self.search_hits.iter().enumerate().map(|(ix, hit)| {
                            let active = self.selected.as_deref() == Some(hit.path.as_str());
                            let path = hit.path.clone();
                            let line = hit.line;
                            v_flex()
                                .id(SharedString::from(format!("gs-hit-{ix}")))
                                .gap(px(3.))
                                .p_2()
                                .rounded(px(7.))
                                .cursor_pointer()
                                .when(active, |this| this.bg(palette.accent_soft))
                                .when(!active, |this| this.hover(|s| s.bg(palette.fill)))
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .items_center()
                                        .child(div().text_size(px(11.5)).text_color(ui::file_tint(&hit.path, &palette)).child(ui::file_icon(&hit.path)))
                                        .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).font_family(ui::mono_font()).font_weight(FontWeight::MEDIUM).text_color(palette.text).child(hit.path.clone()))
                                        .child(div().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.accent).child(format!(":{line}"))),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(11.))
                                        .font_family(ui::mono_font())
                                        .text_color(palette.secondary)
                                        .child(hit.preview.clone()),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_at_line(path.clone(), Some(line), window, cx);
                                }))
                        })),
                )
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .px_3()
                    .pb_2p5()
                    .gap_2()
                    .border_b_1()
                    .border_color(palette.hairline)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.global_search).aria_label("跨文件源码全文搜索").small().prefix(IconName::Search).cleanable(true)))
                            .child(
                                Button::new("gs-case")
                                    .label("Aa")
                                    .xsmall()
                                    .selected(self.search_case_sensitive)
                                    .tooltip("区分大小写")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.search_case_sensitive = !this.search_case_sensitive;
                                        this.trigger_global_search(cx);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .children(filters.into_iter().map(|(kind, label)| {
                                Button::new(SharedString::from(format!("gs-kind-{label}")))
                                    .label(label)
                                    .xsmall()
                                    .ghost()
                                    .selected(self.search_kind == kind)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.search_kind != kind {
                                            this.search_kind = kind;
                                            this.trigger_global_search(cx);
                                        }
                                    }))
                            }))
                            .child(
                                Button::new("gs-containers")
                                    .label("含编译产物")
                                    .xsmall()
                                    .ghost()
                                    .selected(self.search_include_containers)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.search_include_containers = !this.search_include_containers;
                                        this.trigger_global_search(cx);
                                    })),
                            ),
                    )
                    .child(div().text_size(px(11.)).text_color(palette.tertiary).child(summary_text)),
            )
            .child(results)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use wxlocated_engine::decompile::{Artifact, ArtifactKind, Confidence, Content};

    use super::{SearchKindFilter, search_artifacts};

    fn sample_artifacts() -> Vec<Artifact> {
        vec![
            Artifact {
                path: "utils/request.js".into(),
                kind: ArtifactKind::Script,
                confidence: Confidence::High,
                content: Content::Generated("const API_KEY = \"SecretToken\";\nconst URL = \"https://api.example.com\";\n".into()),
                sources: Vec::new(),
                notes: Vec::new(),
            },
            Artifact {
                path: "pages/home/index.wxml".into(),
                kind: ArtifactKind::Markup,
                confidence: Confidence::High,
                content: Content::Generated("<view>请求token校验</view>\n".into()),
                sources: Vec::new(),
                notes: Vec::new(),
            },
            Artifact {
                path: "__wxlocated_container__/app-service.js".into(),
                kind: ArtifactKind::Script,
                confidence: Confidence::Exact,
                content: Content::Generated("const SecretToken = 1;\n".into()),
                sources: Vec::new(),
                notes: Vec::new(),
            },
            Artifact {
                path: "raw-only.json".into(),
                kind: ArtifactKind::Configuration,
                confidence: Confidence::Exact,
                content: Content::Entry(0),
                sources: Vec::new(),
                notes: Vec::new(),
            },
        ]
    }

    #[test]
    fn search_respects_case_sensitivity_kinds_containers_and_limit() {
        let artifacts = sample_artifacts();
        let entry_loader = |idx: usize| -> Option<Cow<'_, str>> {
            (idx == 0).then_some(Cow::Borrowed("{\"tokenKey\":\"abc\"}\n"))
        };

        let all_ci = search_artifacts(&artifacts, "token", false, SearchKindFilter::All, false, 100, entry_loader);
        assert_eq!(all_ci.hits.len(), 3);
        assert_eq!(all_ci.matched_files, 3);
        assert!(!all_ci.truncated);

        let cs = search_artifacts(&artifacts, "SecretToken", true, SearchKindFilter::Script, false, 100, entry_loader);
        assert_eq!(cs.hits.len(), 1);
        assert_eq!(cs.hits[0].path, "utils/request.js");
        assert_eq!(cs.hits[0].line, 1);

        let with_containers = search_artifacts(&artifacts, "SecretToken", true, SearchKindFilter::All, true, 100, entry_loader);
        assert_eq!(with_containers.hits.len(), 2);

        let capped = search_artifacts(&artifacts, "token", false, SearchKindFilter::All, true, 1, entry_loader);
        assert_eq!(capped.hits.len(), 1);
        assert!(capped.truncated);
    }

    #[test]
    fn summarize_truncates_safely_on_unicode() {
        let long = format!("前缀{}", "文".repeat(200));
        let summary = super::summarize_line(&long);
        assert!(summary.ends_with('…'));
        assert_eq!(summary.chars().count(), 140);
    }
}
