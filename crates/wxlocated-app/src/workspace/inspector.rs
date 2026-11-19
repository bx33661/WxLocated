//! Inspector sections, package integrity and evidence citations.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use wxlocated_engine::decompile::{Artifact, Confidence, Content};
use wxlocated_engine::export::confidence_title;
use wxlocated_engine::material::report::{self, Integrity};

use crate::library::RebuildSummary;
use crate::theme::{Palette, palette};
use crate::ui::{self, badge, format_size, format_time, section_label};
use crate::workspace_actions::Rebuild;

use super::{Loaded, Phase, Preview, View, Workspace};

impl Workspace {
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

    pub(super) fn render_inspector(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
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
                Confidence::Exact => (palette.success, "与包内字节或 Source Map 原文一致。".to_string()),
                Confidence::High => (palette.accent, "按编译结构确定性还原，语义应与原文一致。".to_string()),
                Confidence::Medium => (palette.warning, ui::shortcut("部分结构为推导结果，引用前请对照原始文件（⌥⌘R）。")),
                Confidence::Low => (palette.warning, ui::shortcut("只能部分还原，保留了原始片段与诊断，请以原始文件为准（⌥⌘R）。")),
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
                        .child(Button::new("app-reveal").icon(IconName::FolderOpen).label(wxlocated_engine::platform::file_manager_name()).xsmall().on_click(move |_, _, cx| cx.reveal_path(&folder)))
                        .child(Button::new("app-copy").icon(IconName::Copy).label("复制路径").xsmall().on_click(move |_, _, cx| ui::copy(folder_copy.clone(), cx))),
                )
                .into_any_element(),
        );

        let rebuild = section("源码重建", &palette);
        sections.push(
            match &self.phase {
                Phase::Ready(loaded) => {
                    let RebuildSummary { generated, degraded } = loaded.summary;
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

fn evidence_method(value: &Value) -> &'static str {
    match value["match"]["method"].as_str() {
        Some("identical") => "原样对应",
        Some("anchored") => "文本定位",
        _ => "仅范围",
    }
}

pub(super) fn build_evidence(loaded: &Loaded, index: usize, view: View, reading: &str, line: Option<i64>, verify: bool) -> Result<Value, String> {
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
pub(super) fn source_span(artifact: &Artifact, raw: &str) -> Option<std::ops::Range<usize>> {
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

#[cfg(test)]
mod tests {
    use super::{Artifact, Confidence, Content, source_span};
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
