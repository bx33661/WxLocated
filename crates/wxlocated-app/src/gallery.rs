//! Live cache home & miniapp discovery view inside the main window.

use std::time::Duration;

use gpui_kit::Anchor;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wxlocated_engine::archive::natural_compare;
use wxlocated_engine::material::{Completeness, Context as MaterialContext};
use wxlocated_engine::scan::Miniapp;

use crate::library::{Derived, Library, RebuildSummary};
use crate::theme::palette;
use crate::ui::{self, app_icon, badge, format_size, format_time, mascot};

const CARD_WIDTH: f32 = 256.0;
const CARD_GAP: f32 = 12.0;
const PADDING: f32 = 24.0;

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Rebuilt,
    /// Apps whose declared subpackages or plugins are not all cached yet.
    Incomplete,
}

pub struct Gallery {
    library: Entity<Library>,
    search: Entity<InputState>,
    name_input: Entity<InputState>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    selection_anchor: ScrollAnchor,
    filter: Filter,
    selected: Option<String>,
    copied: Option<String>,
    columns: usize,
    _subscriptions: Vec<Subscription>,
}

impl Gallery {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let library = Library::global(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(ui::shortcut("搜索小程序名称、AppID 或缓存路径  ⌘F")));
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("小程序名称"));
        let subscriptions = vec![
            cx.observe(&library, |this, library, cx| {
                if let Some(id) = &this.selected
                    && library.read(cx).app(id).is_none()
                {
                    this.selected = None;
                }
                cx.notify();
            }),
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.selected = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&name_input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.save_name(window, cx);
                }
            }),
        ];
        let focus = cx.focus_handle().tab_stop(true);
        let scroll = ScrollHandle::new();
        let selection_anchor = ScrollAnchor::for_handle(scroll.clone());
        Self {
            library,
            search,
            name_input,
            focus,
            scroll,
            selection_anchor,
            filter: Filter::All,
            selected: None,
            copied: None,
            columns: 3,
            _subscriptions: subscriptions,
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    fn visible_apps<'a>(&self, apps: &'a [Miniapp], cx: &App) -> Vec<&'a Miniapp> {
        let query = self.query(cx);
        let library = self.library.read(cx);
        let mut apps: Vec<_> = apps
            .iter()
            .filter(|app| match self.filter {
                Filter::All => true,
                Filter::Rebuilt => library.rebuilt.contains_key(&app.app_id),
                Filter::Incomplete => library.derived_ref(&app.app_id).is_incomplete(),
            })
            .filter(|app| {
                query.is_empty()
                    || app.display_name().to_lowercase().contains(&query)
                    || app.app_id.to_lowercase().contains(&query)
                    || app.main_package.to_string_lossy().to_lowercase().contains(&query)
            })
            .collect();
        match library.settings.sort_order.as_str() {
            "name" => apps.sort_by(|a, b| natural_compare(a.display_name(), b.display_name())),
            "size" => apps.sort_by_key(|app| std::cmp::Reverse(app.size)),
            _ => apps.sort_by_key(|app| std::cmp::Reverse(app.modified)),
        }
        apps
    }

    fn selected_app(&self, cx: &App) -> Option<Miniapp> {
        self.library.read(cx).app(self.selected.as_ref()?).cloned()
    }

    fn select(&mut self, app_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(app_id.to_string());
        self.focus.focus(window, cx);
        self.selection_anchor.scroll_to(window, cx);
        cx.notify();
    }

    #[cfg(all(feature = "snapshot", target_os = "macos"))]
    pub fn select_for_snapshot(&mut self, app_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.select(app_id, window, cx);
    }

    fn open(&mut self, app_id: &str, cx: &mut Context<Self>) {
        let id = app_id.to_string();
        self.library.update(cx, |library, cx| library.record_use(&id, cx));
        crate::open_app(app_id, cx);
    }

    fn context_for(&self, app: &Miniapp, cx: &App) -> MaterialContext {
        self.library.read(cx).snapshot.as_ref().map(|s| MaterialContext::for_app(s, app)).unwrap_or_default()
    }

    fn move_selection(&mut self, key: &str, is_grid: bool, window: &mut Window, cx: &mut Context<Self>) {
        let all_apps = self.library.read(cx).all_apps();
        let apps = self.visible_apps(&all_apps, cx);
        if apps.is_empty() {
            return;
        }
        let current = self.selected.as_ref().and_then(|id| apps.iter().position(|a| a.app_id == *id));
        let Some(current) = current else {
            let id = apps[0].app_id.clone();
            return self.select(&id, window, cx);
        };
        let columns = if is_grid { self.columns as isize } else { 1 };
        let delta = match key {
            "left" => -1,
            "right" => 1,
            "up" => -columns,
            _ => columns,
        };
        let next = current as isize + delta;
        if (0..apps.len() as isize).contains(&next) {
            let id = apps[next as usize].app_id.clone();
            self.select(&id, window, cx);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            if event.keystroke.key == "escape" {
                self.focus.focus(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let is_grid = self.library.read(cx).settings.gallery_view == "grid";
        match keystroke.key.as_str() {
            key @ ("left" | "right" | "up" | "down") => {
                self.move_selection(key, is_grid, window, cx);
                cx.stop_propagation();
            }
            "enter" => {
                if let Some(id) = self.selected.clone() {
                    self.open(&id, cx);
                    cx.stop_propagation();
                }
            }
            "space" => {
                if let Some(app) = self.selected_app(cx) {
                    self.quick_look(app, window, cx);
                    cx.stop_propagation();
                }
            }
            "escape" if self.selected.is_some() => {
                self.selected = None;
                cx.notify();
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    // MARK: Actions

    fn start_rename(&mut self, app: Miniapp, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(app.app_id.clone());
        let custom = app.metadata_source.as_deref() == Some("自定义名称");
        self.name_input.update(cx, |input, cx| input.set_value(app.name.clone().unwrap_or_default(), window, cx));
        let input = self.name_input.clone();
        let gallery = cx.entity().downgrade();
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.update(cx, |input, cx| input.focus(window, cx));
            }
            let palette = palette(cx);
            let (save, reset) = (gallery.clone(), gallery.clone());
            dialog
                .w(px(420.))
                .title("设置显示名称")
                .child(
                    v_flex()
                        .gap_2()
                        .child(Input::new(&input).aria_label("小程序显示名称"))
                        .child(div().text_size(px(12.)).text_color(palette.secondary).child("名称只保存在本机，并与该 AppID 关联。")),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .when(custom, |this| {
                            this.child(Button::new("reset-name").label("恢复自动识别").ghost().on_click(move |_, window, cx| {
                                reset.update(cx, |this, cx| this.reset_name(window, cx)).ok();
                            }))
                        })
                        .child(div().flex_1())
                        .child(Button::new("cancel-name").label("取消").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("save-name").label("保存").primary().on_click(move |_, window, cx| {
                            save.update(cx, |this, cx| this.save_name(window, cx)).ok();
                        })),
                )
        });
    }

    fn save_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else { return };
        let name = self.name_input.read(cx).value().to_string();
        window.close_dialog(cx);
        self.library.update(cx, |library, cx| library.set_custom_name(&id, Some(name), cx));
    }

    fn reset_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else { return };
        window.close_dialog(cx);
        self.library.update(cx, |library, cx| library.set_custom_name(&id, None, cx));
    }

    fn export_material(&mut self, app: Miniapp, window: &mut Window, cx: &mut Context<Self>) {
        let context = self.context_for(&app, cx);
        let id = app.app_id.clone();
        self.library.update(cx, |library, cx| library.record_use(&id, cx));
        crate::export::export_material(app, context, window, cx);
    }

    fn copy_text_badge(&mut self, app_id: &str, value: String, cx: &mut Context<Self>) {
        ui::copy(value, cx);
        self.copied = Some(app_id.to_string());
        cx.notify();
        let id = app_id.to_string();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            this.update(cx, |this, cx| {
                if this.copied.as_deref() == Some(id.as_str()) {
                    this.copied = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Space bar / context menu: Finder-style quick inspection dialog.
    fn quick_look(&mut self, app: Miniapp, window: &mut Window, cx: &mut Context<Self>) {
        let (icon, ambient) = self.library.update(cx, |library, _| library.icon(&app));
        let summary = self.library.read(cx).rebuilt.get(&app.app_id).copied();
        let derived = self.library.read(cx).derived_ref(&app.app_id);
        let gallery = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let palette = palette(cx);
            let id = app.app_id.clone();
            let gallery = gallery.clone();
            let packages = if app.sub_packages.is_empty() { "仅主包".to_string() } else { format!("主包 + {} 个分包", app.sub_packages.len()) };
            dialog
                .w(px(380.))
                .child(
                    v_flex()
                        .gap_4()
                        .child(
                            h_flex()
                                .gap_3()
                                .child(app_icon(icon.clone(), ambient, app.display_name(), 52., &palette, true))
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .gap_0p5()
                                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(app.display_name().to_string()))
                                        .child(div().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.secondary).child(app.app_id.clone())),
                                ),
                        )
                        .child(
                            v_flex()
                                .gap_1p5()
                                .child(ui::info_row("版本", app.version.clone().unwrap_or_else(|| "未知".into()), &palette, false))
                                .child(ui::info_row("大小", format_size(app.size), &palette, false))
                                .child(ui::info_row("包", packages, &palette, false))
                                .when(derived.missing > 0, |this| {
                                    this.child(ui::info_row("分包", format!("缺 {} 个，在微信进入对应页面会触发下载", derived.missing), &palette, false))
                                })
                                .when(derived.plugins > 0, |this| {
                                    this.child(ui::info_row(
                                        "插件",
                                        format!(
                                            "已缓存 {} / {} 个{}",
                                            derived.plugins.saturating_sub(derived.missing_plugins),
                                            derived.plugins,
                                            if derived.missing_plugins > 0 { "，在微信使用对应插件后自动更新" } else { "" }
                                        ),
                                        &palette,
                                        false,
                                    ))
                                })
                                .child(ui::info_row("修改", format_time(app.modified), &palette, false))
                                .when_some(summary, |this, s| {
                                    this.child(ui::info_row(
                                        "源码",
                                        if s.degraded == 0 {
                                            format!("已重建 {} 个文件", s.generated)
                                        } else {
                                            format!("已重建 {} 个文件 · {} 个降级", s.generated, s.degraded)
                                        },
                                        &palette,
                                        false,
                                    ))
                                })
                                .child(ui::info_row("目录", ui::display_path(&app.cache_directory().to_string_lossy()), &palette, true)),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(div().flex_1().text_size(px(11.)).text_color(palette.tertiary).child("空格或 Esc 关闭"))
                        .child(Button::new("ql-open").label("打开源码工作区").primary().on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            gallery.update(cx, |this, cx| this.open(&id, cx)).ok();
                        })),
                )
        });
    }

    // MARK: Rendering

    fn render_header(&self, total: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let library = self.library.read(cx);
        let palette = palette(cx);
        let scanning = library.scanning;
        let monitoring = library.settings.monitor_cache_changes;
        let sort = library.settings.sort_order.clone();
        let status_text = if scanning {
            "正在扫描微信缓存…".to_string()
        } else if let Some(time) = library.last_scan {
            let when = format_time(time);
            if monitoring {
                format!("{total} 个 · 正在监听变化 · 更新于 {}", when.trim_start_matches("今天 "))
            } else {
                format!("{total} 个 · 更新于 {}", when.trim_start_matches("今天 "))
            }
        } else {
            format!("{total} 个")
        };

        ui::panel_header(48., true, &palette).child(
            h_flex()
                .w_full()
                .h_full()
                .when(!cfg!(target_os = "macos"), |this| this.pr(px(10.)))
                .gap_3()
                .child(
                    h_flex()
                        .gap_2p5()
                        .items_center()
                        .child(div().text_size(px(14.5)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child("微信实时缓存"))
                        .child(
                            h_flex()
                                .id("scan-state")
                                .role(Role::Status)
                                .aria_label(status_text.clone())
                                .gap_1p5()
                                .px_2()
                                .py(px(2.))
                                .rounded_full()
                                .bg(palette.fill)
                                .text_size(px(11.))
                                .text_color(palette.secondary)
                                .when(monitoring && !scanning, |this| this.child(ui::dot(palette.success, 6.)))
                                .child(status_text),
                        ),
                )
                .child(div().flex_1())
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(
                            Button::new("open-local-pkg")
                                .icon(IconName::FolderInput)
                                .label("打开本地包")
                                .small()
                                .on_click(|_, _, cx| crate::native_open::prompt_open(cx)),
                        )
                        .child(
                            Button::new("sort")
                                .icon(IconName::ArrowUpDown)
                                .accessibility_label("排序方式")
                                .ghost()
                                .small()
                                .tooltip("排序方式")
                                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                                    let library = Library::global(cx);
                                    let item = |label: &'static str, key: &'static str| {
                                        let library = library.clone();
                                        PopupMenuItem::new(label).checked(sort == key).on_click(move |_, _, cx| {
                                            library.update(cx, |library, cx| {
                                                library.settings.sort_order = key.to_string();
                                                library.save_settings(cx);
                                            });
                                        })
                                    };
                                    menu.label("排序方式").item(item("最近活跃", "modified")).item(item("名称", "name")).item(item("包体积", "size"))
                                }),
                        )
                        .child(
                            Button::new("refresh")
                                .icon(IconName::RefreshCw)
                                .label("刷新")
                                .ghost()
                                .small()
                                .loading(scanning)
                                .tooltip(ui::shortcut("重新扫描微信缓存（⌘R）"))
                                .on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |library, cx| library.refresh(cx)))),
                        ),
                ),
        )
    }

    fn render_spotlight(&self, app: &Miniapp, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let (icon, ambient) = self.library.update(cx, |library, _| library.icon(app));
        let derived = self.library.read(cx).derived_ref(&app.app_id);
        let context = self.context_for(app, cx);
        let completeness = Completeness::new(app, None, &context);
        let found_sub = completeness.found_subpackages.len();
        let total_sub = completeness.declared_subpackages.len();
        let found_plug = derived.plugins.saturating_sub(derived.missing_plugins);
        let declared_pkgs = 1 + total_sub + derived.plugins;
        let found_pkgs = 1 + found_sub + found_plug;
        let is_full = !derived.is_incomplete();

        let id_open = app.app_id.clone();
        let app_export = app.clone();
        let app_export_ctx = context.clone();

        v_flex()
            .p_4()
            .gap_3()
            .rounded(px(10.))
            .bg(palette.raised)
            .border_1()
            .border_color(if is_full { palette.hairline } else { palette.accent })
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(app_icon(icon, ambient, app.display_name(), 44., &palette, true))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(badge("最新活跃", palette.accent))
                                    .child(div().truncate().text_size(px(14.5)).font_weight(FontWeight::BOLD).text_color(palette.text).child(app.display_name().to_string()))
                                    .when_some(app.version.clone(), |this, v| this.child(badge(format!("v{v}"), palette.secondary))),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.5))
                                    .font_family(ui::mono_font())
                                    .text_color(palette.secondary)
                                    .child(format!("{} · {} · {}", app.app_id, format_size(app.size), format_time(app.modified))),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .child(
                                Button::new("spotlight-open")
                                    .icon(IconName::Code)
                                    .label("审阅源码")
                                    .small()
                                    .on_click(cx.listener(move |this, _, _, cx| this.open(&id_open, cx))),
                            )
                            .child(
                                Button::new("spotlight-agent")
                                    .icon(IconName::Sparkles)
                                    .label("导出并复制 Agent 指令")
                                    .small()
                                    .primary()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let id = app_export.app_id.clone();
                                        this.library.update(cx, |library, cx| library.record_use(&id, cx));
                                        crate::export::export_and_copy_prompt(app_export.clone(), app_export_ctx.clone(), window, cx);
                                    })),
                            ),
                    ),
            )
            .child(div().h(px(1.)).w_full().bg(palette.hairline))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_size(px(12.))
                            .child(div().text_color(palette.secondary).child("分包捕获进度："))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if is_full { palette.success } else { palette.warning })
                                    .child(format!("已落盘 {found_pkgs} / {declared_pkgs} 个包")),
                            )
                            .child(div().flex_1().truncate().text_size(px(11.5)).text_color(palette.tertiary).child(if is_full {
                                "（所需分包与插件已齐全，可直接导出给 Agent 审计）"
                            } else {
                                "（在电脑微信里点开对应功能页后，缺失分包会自动捕获点亮）"
                            })),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .child(pkg_chip("✓ 主包 (__APP__)", true, &palette))
                            .children(completeness.found_subpackages.iter().take(10).map(|root| pkg_chip(format!("✓ {root}"), true, &palette)))
                            .children(completeness.missing_subpackages.iter().take(10).map(|root| pkg_chip(format!("○ {root} · 待触发"), false, &palette)))
                            .when(derived.missing_plugins > 0, |this| {
                                this.child(pkg_chip(format!("○ 缺 {} 个插件", derived.missing_plugins), false, &palette))
                            }),
                    ),
            )
    }

    fn render_list_row(&self, ix: usize, app: &Miniapp, summary: Option<RebuildSummary>, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let library = self.library.read(cx);
        let is_ext = library.is_external(&app.app_id);
        let derived = library.derived_ref(&app.app_id);
        let selected = self.selected.as_deref() == Some(app.app_id.as_str());
        let copied = self.copied.as_deref() == Some(app.app_id.as_str());
        let (icon, ambient) = self.library.update(cx, |library, _| library.icon(app));
        let status_label = short_status(derived, app.sub_packages.len());
        let status_color = if derived.is_incomplete() { palette.warning } else { palette.success };

        let id_click = app.app_id.clone();
        let id_copy = app.app_id.clone();
        let id_menu = app.app_id.clone();
        let export_app = app.clone();
        let gallery = cx.entity().downgrade();

        h_flex()
            .id(SharedString::from(format!("row-{}", app.app_id)))
            .role(Role::ListBoxOption)
            .aria_label(format!("{}，{}，{}", app.display_name(), app.app_id, status_label))
            .aria_selected(selected)
            .when(selected, |this| this.aria_active_descendant().anchor_scroll(Some(self.selection_anchor.clone())))
            .gap_3()
            .px_4()
            .py(px(10.))
            .items_center()
            .cursor_pointer()
            .when(ix > 0, |this| this.border_t_1().border_color(palette.hairline))
            .when(selected, |this| this.bg(palette.accent_soft))
            .when(!selected, |this| this.hover(|s| s.bg(palette.fill)))
            .child(app_icon(icon, ambient, app.display_name(), 34., &palette, false))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(app.display_name().to_string()))
                            .when_some(app.version.clone(), |this, v| {
                                this.child(div().px_1p5().py(px(1.)).rounded(px(4.)).bg(palette.fill).text_size(px(10.5)).font_family(ui::mono_font()).text_color(palette.secondary).child(format!("v{v}")))
                            })
                            .when(is_ext, |this| this.child(badge("外部包", palette.accent))),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .font_family(ui::mono_font())
                            .text_color(palette.tertiary)
                            .child(format!("{} · {} · {}", app.app_id, format_size(app.size), format_time(app.modified))),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2p5()
                    .items_center()
                    .when_some(summary, |this, s| {
                        this.child(div().text_size(px(11.)).text_color(palette.secondary).child(format!("已重建 {} 文件", s.generated)))
                    })
                    .child(badge(status_label, status_color))
                    .child(
                        Button::new(SharedString::from(format!("row-copy-{ix}")))
                            .icon(if copied { IconName::Check } else { IconName::Copy })
                            .label(if copied { "已复制 ID" } else { "复制 ID" })
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.copy_text_badge(&id_copy, id_copy.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("row-exp-{ix}")))
                            .icon(IconName::Download)
                            .label("导出材料")
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.export_material(export_app.clone(), window, cx);
                            })),
                    )
                    .child(div().text_size(px(13.)).text_color(palette.tertiary).child(IconName::ChevronRight)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.open(&id_click, cx);
            }))
            .context_menu(move |menu, _, _| app_context_menu(menu, &id_menu, gallery.clone()))
            .into_any_element()
    }

    fn render_card(&self, app: &Miniapp, summary: Option<RebuildSummary>, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let (icon, ambient) = self.library.update(cx, |library, _| library.icon(app));
        let derived = self.library.read(cx).derived_ref(&app.app_id);
        let selected = self.selected.as_deref() == Some(app.app_id.as_str());
        let status_label = short_status(derived, app.sub_packages.len());
        let status_color = if derived.is_incomplete() { palette.warning } else { palette.success };
        let id_click = app.app_id.clone();
        let id_menu = app.app_id.clone();
        let gallery = cx.entity().downgrade();

        v_flex()
            .id(SharedString::from(format!("card-{}", app.app_id)))
            .role(Role::ListBoxOption)
            .aria_label(format!("{}，{}，{}", app.display_name(), app.app_id, status_label))
            .aria_selected(selected)
            .when(selected, |this| this.aria_active_descendant().anchor_scroll(Some(self.selection_anchor.clone())))
            .w(px(CARD_WIDTH))
            .p_3p5()
            .gap_2p5()
            .rounded(px(10.))
            .bg(palette.raised)
            .border_1()
            .border_color(if selected { palette.accent } else { palette.hairline })
            .cursor_pointer()
            .hover(|s| s.border_color(palette.accent))
            .child(
                h_flex()
                    .gap_2p5()
                    .items_center()
                    .child(app_icon(icon, ambient, app.display_name(), 38., &palette, false))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(app.display_name().to_string()))
                            .child(div().truncate().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.tertiary).child(app.app_id.clone())),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .pt_2()
                    .border_t_1()
                    .border_color(palette.hairline)
                    .text_size(px(11.))
                    .child(div().text_color(palette.secondary).child(format!(
                        "{} · {}",
                        app.version.as_ref().map(|v| format!("v{v}")).unwrap_or_else(|| "版本未知".into()),
                        format_size(app.size)
                    )))
                    .child(badge(
                        if let Some(s) = summary && !derived.is_incomplete() {
                            format!("已重建 {}", s.generated)
                        } else {
                            status_label
                        },
                        status_color,
                    )),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.open(&id_click, cx);
            }))
            .context_menu(move |menu, _, _| app_context_menu(menu, &id_menu, gallery.clone()))
            .into_any_element()
    }

    fn render_coverage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let library = self.library.read(cx);
        let snapshot = library.snapshot.clone();
        let (readable, inaccessible, errors) =
            snapshot.as_ref().map(|s| (s.readable_roots.len(), s.inaccessible_roots.len(), s.enumeration_errors)).unwrap_or_default();
        let description = if readable == 0 {
            "尚未获得可读取的缓存目录访问权限。".to_string()
        } else if inaccessible > 0 {
            format!("{inaccessible} 个缓存目录暂未授权访问，其中的小程序不会显示。")
        } else {
            format!("遍历目录时发生 {errors} 次错误。")
        };
        h_flex()
            .gap_3()
            .px(px(14.))
            .py(px(10.))
            .rounded(px(10.))
            .bg(palette.warning.opacity(0.09))
            .border_1()
            .border_color(palette.warning.opacity(0.28))
            .child(div().text_size(px(16.)).text_color(palette.warning).child(IconName::TriangleAlert))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child("需要访问微信缓存目录权限"))
                    .child(div().text_size(px(11.5)).text_color(palette.secondary).child(description)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1p5()
                    .when(cfg!(target_os = "macos") && inaccessible > 0, |this| {
                        this.child(Button::new("cov-privacy").label("打开隐私设置").small().on_click(|_, _, cx| {
                            cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                        }))
                    })
                    .child(Button::new("cov-add").label("添加目录…").small().on_click(|_, _, cx| crate::settings_view::add_folder(cx)))
                    .child(Button::new("cov-diag").label("查看诊断").small().ghost().on_click(|_, _, cx| {
                        crate::settings_view::open_settings(Some(crate::settings_view::Tab::Scan), cx)
                    })),
            )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let library = self.library.read(cx);
        let scanning = library.scanning || library.snapshot.is_none() && library.settings.auto_scan_on_launch;
        let inaccessible = library.snapshot.as_ref().is_some_and(|s| !s.inaccessible_roots.is_empty());
        let readable = library.snapshot.as_ref().map(|s| s.readable_roots.len()).unwrap_or(0);
        let message = if scanning {
            "正在查找微信缓存中的小程序包…"
        } else if inaccessible {
            if cfg!(target_os = "macos") {
                "部分缓存目录没有访问权限。在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许 WxLocated，然后重启并重新扫描（如升级覆盖后失效，请先用「-」移除旧条目再重新添加）。"
            } else {
                "部分缓存目录没有访问权限。请确认当前用户具有读取权限，或手动添加可读取的 Applet 缓存目录。"
            }
        } else if readable == 0 {
            "没有找到微信小程序缓存目录。请确认已在电脑版微信中打开过小程序，或直接打开/拖入本地 .wxapkg 文件。"
        } else {
            "在微信中点开目标小程序后会自动捕获出现在这里，你也可以直接打开或拖入本地 .wxapkg 文件。"
        };
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div().relative().child(mascot(96.)).with_animation(
                    "mascot-float",
                    Animation::new(Duration::from_millis(2400)).repeat(),
                    |this, delta| this.top(px(-(delta * std::f32::consts::PI).sin() * 5.)),
                ),
            )
            .child(div().mt_2().text_size(px(18.)).font_weight(FontWeight::BOLD).text_color(palette.text).child(if scanning { "正在扫描微信缓存" } else { "还没有发现小程序" }))
            .child(div().max_w(px(460.)).text_size(px(12.5)).text_center().text_color(palette.secondary).child(message))
            .when(!scanning, |this| {
                this.child(
                    h_flex()
                        .mt_2()
                        .gap_2()
                        .child(Button::new("scan").icon(IconName::RefreshCw).label("重新扫描").primary().on_click(cx.listener(|this, _, _, cx| {
                            this.library.update(cx, |library, cx| library.refresh(cx))
                        })))
                        .child(Button::new("empty-open-local").icon(IconName::FolderInput).label("打开本地包").on_click(|_, _, cx| crate::native_open::prompt_open(cx)))
                        .child(Button::new("add-folder").icon(IconName::FolderPlus).label("添加缓存目录").on_click(|_, _, cx| crate::settings_view::add_folder(cx)))
                        .when(cfg!(target_os = "macos") && inaccessible, |this| {
                            this.child(Button::new("privacy").icon(IconName::Shield).label("打开隐私设置").on_click(|_, _, cx| {
                                cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                            }))
                        }),
                )
            })
    }
}

fn pkg_chip(label: impl Into<SharedString>, cached: bool, palette: &crate::theme::Palette) -> Div {
    div()
        .px_2p5()
        .py(px(3.))
        .rounded(px(6.))
        .font_family(ui::mono_font())
        .text_size(px(11.))
        .bg(if cached { palette.success.opacity(0.12) } else { palette.fill })
        .text_color(if cached { palette.success } else { palette.tertiary })
        .child(label.into())
}

fn short_status(derived: Derived, subpackages: usize) -> String {
    match (derived.missing, derived.missing_plugins) {
        (0, 0) if derived.declares || derived.plugins > 0 => format!("分包齐全 ({})", subpackages + 1),
        (0, 0) => "仅主包".into(),
        (packages, 0) => format!("缺 {packages} 个分包"),
        (0, plugins) => format!("缺 {plugins} 个插件"),
        (packages, plugins) => format!("缺 {packages} 分包 · {plugins} 插件"),
    }
}

/// Right-click actions resolve the current app when invoked, even after a rescan.
fn app_context_menu(menu: PopupMenu, app_id: &str, gallery: WeakEntity<Gallery>) -> PopupMenu {
    let item = |label: &'static str, icon: IconName, action: fn(&mut Gallery, Miniapp, &mut Window, &mut Context<Gallery>)| {
        let (gallery, id) = (gallery.clone(), app_id.to_owned());
        PopupMenuItem::new(label).icon(icon).on_click(move |_, window, cx| {
            gallery
                .update(cx, |this, cx| {
                    if let Some(app) = this.library.read(cx).app(&id).cloned() {
                        action(this, app, window, cx);
                    }
                })
                .ok();
        })
    };
    let win_id = app_id.to_owned();
    menu.item(item("打开源码工作区", IconName::ArrowUpRight, |this, app, _, cx| this.open(&app.app_id, cx)))
        .item(PopupMenuItem::new("在新窗口中打开").icon(IconName::ExternalLink).on_click(move |_, _, cx| {
            crate::workspace::open_app_window(&win_id, cx);
        }))
        .item(item("快速查看", IconName::Eye, Gallery::quick_look))
        .separator()
        .item(item("显示缓存目录", IconName::FolderOpen, |_, app, _, cx| cx.reveal_path(&app.cache_directory())))
        .item(item("复制目录路径", IconName::Copy, |this, app, _, cx| {
            this.copy_text_badge(&app.app_id, app.cache_directory().to_string_lossy().into_owned(), cx);
        }))
        .item(item("复制 AppID", IconName::Hash, |this, app, _, cx| {
            this.copy_text_badge(&app.app_id, app.app_id.clone(), cx);
        }))
        .item(item("设置显示名称…", IconName::Pencil, Gallery::start_rename))
        .separator()
        .item(item("导出审计材料…", IconName::Download, Gallery::export_material))
        .item(item("导出主包…", IconName::Package, |_, app, window, cx| crate::export::export_main_package(&app, window, cx)))
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let stage_width = (f32::from(window.viewport_size().width) - 224. - PADDING * 2.).max(400.);
        self.columns = ((stage_width + CARD_GAP) / (CARD_WIDTH + CARD_GAP)).floor().max(1.) as usize;

        let library = self.library.read(cx);
        let all_apps = library.all_apps();
        let apps = self.visible_apps(&all_apps, cx);
        let total = all_apps.len();
        let (rebuilt_count, incomplete_count) = all_apps.iter().fold((0, 0), |(rebuilt, incomplete), app| {
            (rebuilt + usize::from(library.rebuilt.contains_key(&app.app_id)), incomplete + usize::from(library.derived_ref(&app.app_id).is_incomplete()))
        });
        let gap = library.has_coverage_gap();
        let query_empty = self.query(cx).is_empty();
        let is_grid = library.settings.gallery_view == "grid";
        let spotlight = (query_empty && self.filter == Filter::All)
            .then(|| all_apps.iter().max_by_key(|a| a.modified).cloned())
            .flatten();
        let summaries: Vec<Option<RebuildSummary>> = apps.iter().map(|a| library.rebuilt.get(&a.app_id).copied()).collect();
        let filter_index = match self.filter {
            Filter::All => 0,
            Filter::Incomplete => 1,
            Filter::Rebuilt => 2,
        };
        let view_index = usize::from(is_grid);

        let first_scan = total == 0 && (library.scanning || library.snapshot.is_none() && library.settings.auto_scan_on_launch);
        let body = if first_scan {
            v_flex()
                .flex_1()
                .p(px(PADDING))
                .gap_3()
                .child(Skeleton::new().h(px(120.)).w_full().rounded(px(10.)))
                .children((0..5).map(|_| Skeleton::new().h(px(54.)).w_full().rounded(px(8.))))
                .into_any_element()
        } else if total == 0 {
            div().flex_1().flex().child(self.render_empty(cx)).into_any_element()
        } else {
            let list_content: AnyElement = if apps.is_empty() {
                let (title, message) = if !query_empty {
                    ("没有匹配结果", "试试其他名称、AppID 或缓存路径关键词。")
                } else if self.filter == Filter::Incomplete {
                    ("所有小程序分包均已齐全", "目前没有缺少分包或插件的小程序。")
                } else {
                    ("本次还没有打开过小程序源码", "单击任意小程序即可打开并自动还原源码。")
                };
                div().h(px(260.)).child(ui::empty_state(None, title, message, &palette)).into_any_element()
            } else if is_grid {
                h_flex()
                    .id("app-list")
                    .role(Role::ListBox)
                    .aria_label("小程序卡片列表，单击打开")
                    .flex_wrap()
                    .gap(px(CARD_GAP))
                    .children(apps.iter().zip(summaries).map(|(app, summary)| self.render_card(app, summary, cx)))
                    .into_any_element()
            } else {
                v_flex()
                    .id("app-list")
                    .role(Role::ListBox)
                    .aria_label("小程序列表，单击打开")
                    .rounded(px(10.))
                    .bg(palette.raised)
                    .border_1()
                    .border_color(palette.hairline)
                    .overflow_hidden()
                    .children(apps.iter().zip(summaries).enumerate().map(|(ix, (app, summary))| self.render_list_row(ix, app, summary, cx)))
                    .into_any_element()
            };

            div()
                .id("gallery-scroll")
                .track_scroll(&self.scroll)
                .flex_1()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(1080.))
                        .mx_auto()
                        .p(px(PADDING))
                        .gap_5()
                        .when(gap, |this| this.child(self.render_coverage(cx)))
                        .when_some(spotlight.as_ref(), |this, app| this.child(self.render_spotlight(app, cx)))
                        .child(
                            h_flex()
                                .gap_3()
                                .flex_wrap()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(200.))
                                        .max_w(px(360.))
                                        .child(Input::new(&self.search).aria_label("筛选小程序").small().prefix(IconName::Search).cleanable(true)),
                                )
                                .child(
                                    TabBar::new("filter")
                                        .segmented()
                                        .small()
                                        .selected_index(filter_index)
                                        .child(Tab::new().label(format!("全部 {total}")))
                                        .child(Tab::new().label(if incomplete_count > 0 { format!("分包未全 {incomplete_count}") } else { "分包未全".into() }))
                                        .child(Tab::new().label(if rebuilt_count > 0 { format!("已重建 {rebuilt_count}") } else { "已重建".into() }))
                                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                                            this.filter = match index {
                                                0 => Filter::All,
                                                1 => Filter::Incomplete,
                                                _ => Filter::Rebuilt,
                                            };
                                            this.selected = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    TabBar::new("view-mode")
                                        .segmented()
                                        .small()
                                        .selected_index(view_index)
                                        .child(Tab::new().label("列表"))
                                        .child(Tab::new().label("卡片"))
                                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                                            let view = if *index == 1 { "grid" } else { "list" };
                                            this.library.update(cx, |library, cx| {
                                                if library.settings.gallery_view != view {
                                                    library.settings.gallery_view = view.into();
                                                    library.save_settings(cx);
                                                }
                                            });
                                        })),
                                ),
                        )
                        .child(list_content)
                        .child(
                            div()
                                .pt_1()
                                .text_center()
                                .text_size(px(11.5))
                                .text_color(palette.tertiary)
                                .child("单击任意小程序即可浏览源码，也支持直接将 .wxapkg 文件或重建层文件夹拖入窗口"),
                        ),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.selected = None;
                    this.focus.focus(window, cx);
                    cx.notify();
                }))
                .into_any_element()
        };

        v_flex()
            .id("gallery")
            .role(Role::Group)
            .aria_label("微信实时缓存")
            .key_context("Gallery")
            .track_focus(&self.focus)
            .size_full()
            .bg(palette.window)
            .text_color(palette.text)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &crate::FocusSearch, window, cx| this.search.update(cx, |input, cx| input.focus(window, cx))))
            .child(self.render_header(total, cx))
            .children(ui::settings_notice(cx))
            .child(body)
    }
}
