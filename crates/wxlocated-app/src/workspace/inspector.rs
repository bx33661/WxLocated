//! Agent handoff, semantic overlay layers, package integrity fingerprints,
//! and forensic evidence builders.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use wxlocated_engine::decompile::{Artifact, Content};
use wxlocated_engine::material::report::{self, Integrity};

use crate::theme::{Palette, palette};
use crate::ui::{self, badge, display_path, format_size, format_time, section_label};

use super::{Loaded, Phase, View, Workspace};

impl Workspace {
    pub(super) fn copy_evidence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((loaded, index)) = self.selected.as_ref().and_then(|path| self.artifact(path)) else { return };
        let (reading, line) = match &self.preview {
            super::Preview::Code(editor) => {
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
            })
            .ok();
        })
        .detach();
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

    pub(super) fn render_agent_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let app = self.app.clone();
        let context = self.context(cx);
        let folder = app.cache_directory();
        let folder_display = display_path(&folder.to_string_lossy());
        let folder_copy = folder.to_string_lossy().into_owned();
        let cli_command = format!("wxlocated export {} --out ./audit-{}", app.app_id, &app.app_id[app.app_id.len().saturating_sub(6)..]);
        let cli_copied = self.copied == Some("cli-cmd");

        // Card 1: Agent Audit Handoff
        let handoff_card = ui::card(&palette)
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("一键导出并交付给 AI 审计 Agent"))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(palette.secondary)
                                    .child("导出目录会包含 AGENT.md（结构说明）、material.json（完整度清单）、source/（还原源码）与 raw/（原始 wxapkg 证据包）。"),
                            ),
                    )
                    .child(
                        Button::new("agent-export-prompt")
                            .flex_none()
                            .icon(IconName::Sparkles)
                            .label("导出并复制 Agent 提示词")
                            .small()
                            .primary()
                            .disabled(matches!(self.phase, Phase::Loading))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let id = app.app_id.clone();
                                let ctx = context.clone();
                                crate::library::Library::global(cx).update(cx, |library, cx| library.record_use(&id, cx));
                                crate::export::export_and_copy_prompt(this.app.clone(), ctx, window, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .rounded(px(8.))
                    .bg(palette.code)
                    .border_1()
                    .border_color(palette.hairline)
                    .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).font_family(ui::mono_font()).text_color(palette.text).child(cli_command.clone()))
                    .child(
                        Button::new("copy-cli-cmd")
                            .icon(if cli_copied { IconName::Check } else { IconName::Copy })
                            .label(if cli_copied { "已复制" } else { "复制 CLI 命令" })
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                ui::copy(cli_command.clone(), cx);
                                this.mark_copied("cli-cmd", cx);
                            })),
                    ),
            );

        // Card 2: Agent Semantic Layers (Overlays)
        let layers_card = ui::card(&palette)
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).child("Agent 语义重命名层（Overlay）"))
                            .child(div().text_size(px(11.5)).text_color(palette.secondary).child("AI 对源码补充变量命名和注释后，可通过 CLI overlay import 或直接选择含 overlay.json 的文件夹导入，在代码区与静态还原结果彩色对照。")),
                    )
                    .child(
                        Button::new("import-overlay-dir")
                            .flex_none()
                            .icon(IconName::FolderPlus)
                            .label("导入重建层目录…")
                            .small()
                            .on_click(|_, _, cx| crate::native_open::prompt_open(cx)),
                    ),
            )
            .when(self.layers.is_empty(), |this| {
                this.child(
                    div()
                        .p_3()
                        .rounded(px(8.))
                        .bg(palette.fill)
                        .text_size(px(11.5))
                        .font_family(ui::mono_font())
                        .text_color(palette.secondary)
                        .child("提示：wxlocated overlay init <导出目录> <工作目录> → Agent 改写 → wxlocated overlay import <工作目录>"),
                )
            })
            .when(!self.layers.is_empty(), |this| {
                this.child(
                    v_flex().gap_1p5().children(self.layers.iter().enumerate().map(|(index, layer)| {
                        let manifest = &layer.manifest;
                        let meta = [manifest.author.agent.clone(), manifest.author.model.clone(), Some(format!("{} 个文件", manifest.files.len()))]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ");
                        h_flex()
                            .items_center()
                            .gap_3()
                            .p_2p5()
                            .rounded(px(8.))
                            .bg(palette.fill)
                            .child(div().text_size(px(13.)).text_color(palette.accent).child(IconName::Sparkles))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(manifest.name.clone()))
                                    .child(div().text_size(px(11.)).text_color(palette.secondary).child(format!("{meta} · {}", manifest.created_at))),
                            )
                            .child(
                                Button::new(SharedString::from(format!("remove-layer-{index}")))
                                    .icon(IconName::Trash)
                                    .label("移除")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, window, cx| this.remove_layer(index, window, cx))),
                            )
                    })),
                )
            });

        // Card 3: Package Integrity & SHA-256 Fingerprints
        let integrity_card = match self.loaded() {
            None => ui::card(&palette).p_4().gap_2().child(Skeleton::new().h(px(12.)).w(px(180.))).child(Skeleton::new().h(px(10.)).w(px(260.))),
            Some(loaded) => {
                let worst = loaded.inspections.iter().map(|i| i.status).max_by_key(|s| integrity_rank(*s)).unwrap_or(Integrity::Valid);
                let (title, color, icon) = integrity_style(worst, &palette);
                let copied = self.copied == Some("hashes");
                let all_hashes = loaded
                    .inspections
                    .iter()
                    .filter_map(|i| Some(format!("{}  {}", i.sha256.clone()?, i.path.file_name()?.to_string_lossy())))
                    .collect::<Vec<_>>()
                    .join("\n");
                let format_name = loaded.set.sources.first().map(|s| s.archive.format.as_str().to_string()).unwrap_or_else(|| "未知".into());

                ui::card(&palette)
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(color)
                                    .child(icon)
                                    .child(format!("{title} · 格式 {format_name}")),
                            )
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .child(
                                        Button::new("agent-reveal-dir")
                                            .icon(IconName::FolderOpen)
                                            .label(wxlocated_engine::platform::file_manager_name())
                                            .xsmall()
                                            .ghost()
                                            .on_click(move |_, _, cx| cx.reveal_path(&folder)),
                                    )
                                    .child(
                                        Button::new("agent-copy-dir")
                                            .icon(IconName::Copy)
                                            .label("复制缓存目录")
                                            .xsmall()
                                            .ghost()
                                            .on_click(move |_, _, cx| ui::copy(folder_copy.clone(), cx)),
                                    )
                                    .child(
                                        Button::new("copy-hashes")
                                            .icon(if copied { IconName::Check } else { IconName::Hash })
                                            .label(if copied { "已复制全部 SHA-256" } else { "复制全部 SHA-256" })
                                            .xsmall()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                ui::copy(all_hashes.clone(), cx);
                                                this.mark_copied("hashes", cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(div().text_size(px(11.5)).font_family(ui::mono_font()).text_color(palette.secondary).child(format!(
                        "目录：{folder_display} · 修改于 {}",
                        format_time(self.app.modified)
                    )))
                    .child(
                        v_flex().gap_1p5().children(loaded.inspections.iter().enumerate().map(|(index, inspection)| {
                            let (_, color, icon) = integrity_style(inspection.status, &palette);
                            let name = inspection.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let entries = inspection.entry_count.map(|n| format!(" · {n} 项")).unwrap_or_default();
                            v_flex()
                                .p_2p5()
                                .gap_1()
                                .rounded(px(8.))
                                .bg(palette.fill)
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(div().text_size(px(11.)).text_color(color).child(icon))
                                        .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).font_weight(FontWeight::MEDIUM).font_family(ui::mono_font()).child(name))
                                        .child(badge(if index == 0 { "主包" } else { "分包" }, palette.secondary))
                                        .child(div().text_size(px(11.)).text_color(palette.secondary).child(format!("{}{entries}", format_size(inspection.file_size)))),
                                )
                                .when_some(inspection.sha256.clone(), |this, sha| {
                                    this.child(div().truncate().text_size(px(10.5)).font_family(ui::mono_font()).text_color(palette.tertiary).child(format!("SHA-256: {sha}")))
                                })
                        })),
                    )
            }
        };

        div()
            .id("agent-tab")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(920.))
                    .mx_auto()
                    .px_8()
                    .py_6()
                    .gap_5()
                    .child(v_flex().gap_2().child(section_label("Agent 审计交付", None, &palette)).child(handoff_card))
                    .child(v_flex().gap_2().child(section_label("语义重建层与对照", None, &palette)).child(layers_card))
                    .child(v_flex().gap_2().child(section_label("原始包完整性与 SHA-256 证据指纹", None, &palette)).child(integrity_card)),
            )
            .into_any_element()
    }
}

pub(super) fn evidence_method(value: &Value) -> &'static str {
    match value["match"]["method"].as_str() {
        Some("identical") => "原样对应",
        Some("anchored") => "文本定位",
        _ => "仅范围",
    }
}

pub(super) fn build_evidence(loaded: &Loaded, index: usize, view: View, reading: &str, line: Option<i64>, verify: bool) -> Result<Value, String> {
    let artifact = &loaded.result.artifacts[index];
    let source = artifact.sources.first().ok_or("这个文件没有可追溯的原始来源")?;
    let entry = loaded
        .set
        .entries
        .iter()
        .find(|entry| entry.path == source.entry_path && loaded.set.source_title(entry) == source.package)
        .ok_or("找不到对应的原始条目，请重新重建")?;
    let archive = &loaded.set.sources[entry.source].archive;
    let inspection = loaded.inspections.iter().find(|inspection| inspection.path == archive.path).ok_or("没有原始包指纹，请重新重建")?;
    let sha256 = inspection.sha256.as_deref().ok_or("原始包 SHA-256 不可用")?;
    if verify && wxlocated_engine::sha256_file(&archive.path).ok().map(|(hash, _)| hash).as_deref() != Some(sha256) {
        return Err("原始缓存包已变化，无法引用旧指纹。请重新重建后再复制证据。".into());
    }
    let identical = view == View::Raw || (view == View::Rebuilt && matches!(artifact.content, Content::Entry(_)));
    let bytes = if identical && !reading.is_empty() {
        std::borrow::Cow::Borrowed(reading.as_bytes())
    } else {
        std::borrow::Cow::Owned(loaded.set.read_limited(entry, wxlocated_engine::decompile::MAXIMUM_TEXT_FILE_SIZE).map_err(|error| error.to_string())?)
    };
    let segment = if identical {
        0..bytes.len()
    } else {
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
    use super::{Artifact, Content, source_span};
    use wxlocated_engine::decompile::{ArtifactKind, Confidence, SourceRef};

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
