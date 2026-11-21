//! Package loading progress and live Subpackage / Completeness monitoring tab.

use std::collections::BTreeMap;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wxlocated_engine::decompile::{Artifact, Confidence, Severity};
use wxlocated_engine::export::confidence_title;
use wxlocated_engine::material::Completeness;

use crate::library::Library;
use crate::theme::{Palette, palette, soft_shadow};
use crate::ui::{self, app_icon, badge, chip, file_icon, file_tint, format_size, section_label};
use crate::workspace_actions::Rebuild;

use super::{Phase, STEPS, Workspace, kind_title};

impl Workspace {
    pub(super) fn render_packages_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let Some(loaded) = self.loaded() else {
            return match &self.phase {
                Phase::Failed(message) => ui::empty_state(Some(IconName::FileX), "无法读取这个包", message.clone(), &palette).into_any_element(),
                _ => {
                    let elapsed = self.started.elapsed().as_secs();
                    let current = self.step.min(STEPS.len() - 1);
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
                        .child(ui::mascot(80.))
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
                                .rounded(px(10.))
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

        let latest_app = Library::global(cx).read(cx).app(&self.app.app_id).cloned();
        let disk_changed = latest_app.as_ref().is_some_and(|latest| !self.matches_app(latest));
        let active_app = latest_app.as_ref().unwrap_or(&self.app);

        let (icon, ambient) = Library::global(cx).update(cx, |library, _| library.icon(active_app));
        let context = self.context(cx);
        let completeness = Completeness::new(active_app, Some(&loaded.analysis.plugins), &context);
        let artifacts = &loaded.result.artifacts;
        let generated = loaded.summary.generated;
        let passthrough = artifacts.len().saturating_sub(generated);
        let mut degraded: Vec<&Artifact> = artifacts.iter().filter(|a| a.confidence <= Confidence::Medium).collect();
        degraded.sort_by_key(|a| a.confidence);

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
                .rounded(px(10.))
                .bg(palette.raised)
                .border_1()
                .border_color(palette.hairline)
                .child(div().text_size(px(11.5)).text_color(palette.secondary).child(label))
                .child(div().text_size(px(24.)).line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).text_color(tint).child(value))
                .child(div().truncate().text_size(px(11.)).text_color(palette.tertiary).child(detail))
        };

        let metrics = h_flex()
            .gap_3()
            .items_stretch()
            .child(metric("已还原文件", generated.to_string(), format!("{passthrough} 个原样保留"), palette.ink, &palette))
            .child(metric(
                "部分推导 / 降级",
                degraded.len().to_string(),
                if degraded.is_empty() { "全部按结构确定性还原".into() } else { "点击下方列表核对原始字节".into() },
                if degraded.is_empty() { palette.ink } else { palette.warning },
                &palette,
            ))
            .child(metric("重建耗时", format!("{:.1}s", loaded.seconds), format!("{} 个产物", artifacts.len()), palette.ink, &palette))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .p(px(14.))
                    .rounded(px(10.))
                    .bg(palette.raised)
                    .border_1()
                    .border_color(palette.hairline)
                    .child(
                        div()
                            .relative()
                            .size(px(52.))
                            .flex_none()
                            .child(ProgressCircle::new("completeness").value(ratio).color(ring_color).size(px(52.)))
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if declared == 0 { "100%".to_string() } else { format!("{found}/{declared}") }),
                            ),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().text_size(px(11.5)).text_color(palette.secondary).child("分包与插件"))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(ring_color)
                                    .child(if blocked { "受阻" } else if completeness.is_complete() { "已齐备" } else { "待补齐" }),
                            )
                            .child(div().text_size(px(10.5)).text_color(palette.secondary).child(format!("分包 {found_subpackages}/{subpackages} · 插件 {found_plugins}/{plugins}"))),
                    ),
            );

        // Live Subpackage Capture Checklist
        let mut capture_rows: Vec<AnyElement> = vec![subpackage_row("__APP__.wxapkg (主包)", true, "主包始终包含全局配置、公共脚本与入口页", &palette)];
        for root in &completeness.found_subpackages {
            capture_rows.push(subpackage_row(&format!("{root} (声明分包)"), true, "已落盘并纳入源码重建", &palette));
        }
        for root in &completeness.missing_subpackages {
            let older_hint = completeness
                .missing_in_older_versions
                .get(root)
                .map(|v| format!("未落盘（旧版本 v{v} 缓存中存在，不混用）；请在微信中进入此分包页面触发下载"))
                .unwrap_or_else(|| "尚未落盘；在电脑微信中点开对应功能页面即可自动捕获".into());
            capture_rows.push(subpackage_row(&format!("{root} (缺失分包)"), false, &older_hint, &palette));
        }
        for file in &completeness.undeclared_subpackage_files {
            capture_rows.push(subpackage_row(&format!("{file} (附加分包)"), true, "已落盘缓存文件", &palette));
        }
        for alias in completeness.found_plugins.keys() {
            capture_rows.push(subpackage_row(&format!("plugin://{alias} (已缓存插件)"), true, "已关联最新缓存插件版本", &palette));
        }
        for alias in &completeness.missing_plugins {
            capture_rows.push(subpackage_row(&format!("plugin://{alias} (缺失插件)"), false, "在微信中打开使用该插件的功能页可触发下载", &palette));
        }

        // Confidence and file kinds stacked bars
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
                        .children(segments.iter().filter(|s| s.1 > 0).map(|(_, n, color)| div().h_full().flex_grow(*n as f32).flex_basis(px(0.)).bg(*color))),
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
            .child(v_flex().gap_1p5().child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child("重建置信度")).child(bar(
                quality.iter().map(|(l, n, c)| (l.to_string(), *n, *c)).collect(),
                &palette,
            )))
            .child(v_flex().gap_1p5().child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child("文件类型")).child(bar(
                kind_counts.iter().enumerate().map(|(i, (l, n))| (l.to_string(), *n, kind_colors[i.min(kind_colors.len() - 1)])).collect(),
                &palette,
            )));

        // Clickable pages & configuration files (clicking jumps straight to Code tab!)
        let page_buttons: Vec<AnyElement> = loaded
            .analysis
            .page_paths
            .iter()
            .take(30)
            .enumerate()
            .map(|(ix, page)| {
                let target = ["js", "wxml", "json", "wxss"]
                    .iter()
                    .map(|ext| format!("{page}.{ext}"))
                    .find(|candidate| loaded.index.contains_key(candidate));
                let title = loaded.page_titles.get(page).cloned();
                let label = match title {
                    Some(t) => format!("{t} · {page}"),
                    None => page.clone(),
                };
                if let Some(open_path) = target {
                    Button::new(SharedString::from(format!("pkg-pg-{ix}")))
                        .label(label)
                        .xsmall()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_at_line(open_path.clone(), None, window, cx);
                        }))
                        .into_any_element()
                } else {
                    chip(label, &palette, true).into_any_element()
                }
            })
            .collect();

        let degraded_rows: Vec<AnyElement> = degraded
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, artifact)| {
                let open = artifact.path.clone();
                let note = artifact.notes.first().cloned();
                h_flex()
                    .id(SharedString::from(format!("degraded-{i}")))
                    .gap_3()
                    .px_3()
                    .py(px(9.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|this| this.bg(palette.fill))
                    .child(badge(confidence_title(artifact.confidence), palette.warning))
                    .child(div().text_color(file_tint(&artifact.path, &palette)).child(file_icon(&artifact.path)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_size(px(12.)).font_family(ui::mono_font()).child(artifact.path.clone()))
                            .when_some(note, |this, note| this.child(div().truncate().text_size(px(11.)).text_color(palette.secondary).child(note))),
                    )
                    .child(div().text_size(px(11.)).text_color(palette.tertiary).child(IconName::ChevronRight))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_at_line(open.clone(), None, window, cx);
                    }))
                    .into_any_element()
            })
            .collect();

        let diagnostics: Vec<(Severity, String)> = loaded.result.diagnostics.iter().take(10).map(|d| (d.severity, d.message.clone())).collect();
        let next_actions: Vec<String> = loaded.report["next_actions"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        div()
            .id("packages-tab")
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
                    .gap_6()
                    .when(disk_changed, |this| {
                        this.child(
                            h_flex()
                                .gap_3()
                                .p_3p5()
                                .rounded(px(10.))
                                .bg(palette.accent_soft)
                                .border_1()
                                .border_color(palette.accent)
                                .child(div().text_size(px(16.)).text_color(palette.accent).child(IconName::Zap))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child("检测到微信缓存已更新（新分包已落盘）"))
                                        .child(div().text_size(px(11.5)).text_color(palette.secondary).child("点击右侧按钮重新重建源码即可将新增分包加入文件树与全局搜索。")),
                                )
                                .child(
                                    Button::new("reload-new-pkgs")
                                        .flex_none()
                                        .icon(IconName::RefreshCw)
                                        .label("立即重新载入")
                                        .small()
                                        .primary()
                                        .on_click(|_, window, cx| window.dispatch_action(Box::new(Rebuild), cx)),
                                ),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_4()
                            .child(app_icon(icon, ambient, active_app.display_name(), 56., &palette, false))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child(active_app.display_name().to_string()))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(div().text_size(px(12.)).font_family(ui::mono_font()).text_color(palette.secondary).child(active_app.app_id.clone()))
                                            .when_some(active_app.version.clone(), |this, v| this.child(badge(format!("v{v}"), palette.secondary)))
                                            .child(badge(format_size(active_app.size), palette.secondary)),
                                    ),
                            ),
                    )
                    .child(metrics)
                    .child(
                        v_flex()
                            .gap_2()
                            .child(section_label("实时分包与插件捕获清单", Some("由微信缓存变化自动更新".into()), &palette))
                            .child(ui::card(&palette).overflow_hidden().children(capture_rows)),
                    )
                    .when(!page_buttons.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .child(section_label("页面路由快速直达", Some(format!("共 {} 个页面 · 点击直达源码", loaded.analysis.page_paths.len())), &palette))
                                .child(ui::card(&palette).p_3p5().child(h_flex().flex_wrap().gap_1p5().children(page_buttons))),
                        )
                    })
                    .child(v_flex().gap_2().child(section_label("重建质量与文件分布", None, &palette)).child(quality_card))
                    .when(!degraded_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .child(section_label("降级文件", Some(format!("{} 个 · 点击核对原始字节", degraded.len())), &palette))
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
                            v_flex().gap_2().child(section_label("建议下一步", None, &palette)).children(next_actions.into_iter().map(|action| {
                                h_flex()
                                    .items_start()
                                    .gap_2()
                                    .text_size(px(12.))
                                    .child(div().pt(px(1.)).text_color(palette.accent).child(IconName::ArrowRight))
                                    .child(div().flex_1().child(action))
                            })),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn subpackage_row(name: &str, cached: bool, detail: &str, palette: &Palette) -> AnyElement {
    h_flex()
        .px_4()
        .py_3()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(palette.hairline)
        .child(badge(if cached { "已落盘" } else { "待触发" }, if cached { palette.success } else { palette.warning }))
        .child(div().text_size(px(12.5)).font_family(ui::mono_font()).font_weight(FontWeight::MEDIUM).text_color(palette.text).child(name.to_string()))
        .child(div().flex_1().truncate().text_size(px(11.5)).text_color(palette.secondary).child(detail.to_string()))
        .into_any_element()
}
