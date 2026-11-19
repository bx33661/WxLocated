//! Package loading overview and material summary.

use std::collections::BTreeMap;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use wxlocated_engine::decompile::{Artifact, Confidence, Severity};
use wxlocated_engine::export::confidence_title;
use wxlocated_engine::material::Completeness;

use crate::library::Library;
use crate::theme::{Palette, palette, soft_shadow};
use crate::ui::{self, app_icon, badge, chip, file_icon, file_tint, format_size, section_label};

use super::{Phase, STEPS, Workspace, kind_title};

impl Workspace {
    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> AnyElement {
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
        let generated = loaded.summary.generated;
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
}
