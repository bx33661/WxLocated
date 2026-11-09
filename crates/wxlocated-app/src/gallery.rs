//! Main window: an icon gallery of every cached mini program.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::Anchor;
use wxlocated_engine::archive::natural_compare;
use wxlocated_engine::material::Context as MaterialContext;
use wxlocated_engine::scan::Miniapp;

use crate::library::{Derived, Library, RebuildSummary};
use crate::theme::{Palette, palette, soft_shadow};
use crate::ui::{self, app_icon, format_size, format_time, mascot};

const TILE_WIDTH: f32 = 120.0;
const TILE_GAP: f32 = 6.0;
const PADDING: f32 = 28.0;

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Rebuilt,
    /// Apps whose declared subpackages or plugins are not all cached.
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
    hovered: Option<String>,
    /// The tile the pointer just left, which animates back down.
    unhovered: Option<String>,
    copied: Option<String>,
    columns: usize,
    _subscriptions: Vec<Subscription>,
}

impl Gallery {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let library = Library::global(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("名称、AppID 或路径  ⌘F"));
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
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            if let WindowBounds::Windowed(bounds) = window.window_bounds() {
                let frame = [f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height)];
                this.library.update(cx, |library, _| library.settings.gallery_frame = Some(frame));
            }
        }));
        let focus = cx.focus_handle().tab_stop(true);
        focus.focus(window, cx);
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
            hovered: None,
            unhovered: None,
            copied: None,
            columns: 6,
            _subscriptions: subscriptions,
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    fn visible_apps(&self, cx: &App) -> Vec<Miniapp> {
        let query = self.query(cx);
        let library = self.library.read(cx);
        let mut apps: Vec<Miniapp> = library
            .apps()
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
            .cloned()
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

    #[cfg(feature = "snapshot")]
    pub fn select_for_snapshot(&mut self, app_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.select(app_id, window, cx);
    }

    fn open(&mut self, app_id: &str, cx: &mut Context<Self>) {
        let id = app_id.to_string();
        self.library.update(cx, |library, cx| library.record_use(&id, cx));
        crate::workspace::open_app_window(app_id, cx);
    }

    fn context_for(&self, app: &Miniapp, cx: &App) -> MaterialContext {
        self.library.read(cx).snapshot.as_ref().map(|s| MaterialContext::for_app(s, app)).unwrap_or_default()
    }

    fn move_selection(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let apps = self.visible_apps(cx);
        if apps.is_empty() {
            return;
        }
        let current = self.selected.as_ref().and_then(|id| apps.iter().position(|a| a.app_id == *id));
        let Some(current) = current else {
            let id = apps[0].app_id.clone();
            return self.select(&id, window, cx);
        };
        let columns = self.columns as isize;
        let next = current as isize
            + match key {
                "left" => -1,
                "right" => 1,
                "up" => -columns,
                _ => columns,
            };
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
        match keystroke.key.as_str() {
            key @ ("left" | "right" | "up" | "down") => {
                self.move_selection(key, window, cx);
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

    fn copy_path(&mut self, app: &Miniapp, cx: &mut Context<Self>) {
        ui::copy(app.cache_directory().to_string_lossy(), cx);
        self.copied = Some(app.app_id.clone());
        cx.notify();
        let id = app.app_id.clone();
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

    /// Space bar: a Finder-style preview card.
    fn quick_look(&mut self, app: Miniapp, window: &mut Window, cx: &mut Context<Self>) {
        let (icon, ambient) = self.library.update(cx, |library, _| library.icon(&app));
        let summary = self.library.read(cx).rebuilt.get(&app.app_id).copied();
        let derived = self.library.update(cx, |library, _| library.derived(&app));
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
                                .when(derived.missing > 0, |this| this.child(ui::info_row("分包", format!("缺 {} 个，在微信中进入对应页面会触发下载", derived.missing), &palette, false)))
                                .when(derived.plugins > 0, |this| this.child(ui::info_row("插件", format!("已缓存 {} / {} 个{}", derived.plugins.saturating_sub(derived.missing_plugins), derived.plugins, if derived.missing_plugins > 0 { "，在微信中使用对应插件后重新扫描" } else { "" }), &palette, false)))
                                .child(ui::info_row("修改", format_time(app.modified), &palette, false))
                                .when_some(summary, |this, s| this.child(ui::info_row(
                                    "源码",
                                    if s.degraded == 0 { format!("已重建 {} 个文件", s.generated) } else { format!("已重建 {} 个文件 · {} 个降级", s.generated, s.degraded) },
                                    &palette,
                                    false,
                                )))
                                .child(ui::info_row("目录", ui::display_path(&app.cache_directory().to_string_lossy()), &palette, true)),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(div().flex_1().text_size(px(11.)).text_color(palette.tertiary).child("空格关闭"))
                        .child(Button::new("ql-open").label("打开").primary().on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            gallery.update(cx, |this, cx| this.open(&id, cx)).ok();
                        })),
                )
        });
    }

    fn open_dropped(&mut self, paths: &ExternalPaths, _window: &mut Window, cx: &mut Context<Self>) {
        crate::native_open::open_paths(paths.paths(), cx);
    }

    // MARK: Rendering

    fn render_header(&self, count: usize, rebuilt: usize, incomplete: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let library = self.library.read(cx);
        let palette = palette(cx);
        let scanning = library.scanning;
        let mut subtitle = vec![format!("{count} 个")];
        if let Some(time) = library.last_scan {
            subtitle.push(format!("更新于 {}", format_time(time).trim_start_matches("今天 ")));
        }
        if library.settings.monitor_cache_changes {
            subtitle.push("实时监测".into());
        }
        let scan_status = if scanning { "正在扫描微信缓存…".to_string() } else { subtitle.join(" · ") };
        let monitoring = library.settings.monitor_cache_changes;
        let sort = library.settings.sort_order.clone();
        let filter_index = match self.filter {
            Filter::All => 0,
            Filter::Rebuilt => 1,
            Filter::Incomplete => 2,
        };
        ui::toolbar(60., &palette).child(
            h_flex()
            .w_full()
            .min_w_0()
            .h_full()
            .gap_3()
            .child(
                v_flex()
                    .min_w(px(150.))
                    .child(div().text_size(px(18.)).font_weight(FontWeight::BOLD).text_color(palette.text).child("小程序"))
                    .child(
                        h_flex()
                            .id("scan-state")
                            .role(Role::Status)
                            .aria_label(scan_status.clone())
                            .gap_1p5()
                            .text_size(px(11.5))
                            .text_color(palette.secondary)
                            .when(monitoring && !scanning, |this| this.child(div().id("monitoring-state").role(Role::Status).aria_label("正在实时监测缓存变化").child(ui::dot(palette.success, 6.))))
                            .child(scan_status),
                    ),
            )
            .child(div().flex_1())
            .child(
                TabBar::new("filter")
                    .segmented()
                    .small()
                    .selected_index(filter_index)
                    .child(Tab::new().label("全部"))
                    .child(Tab::new().label(if rebuilt > 0 { format!("已重建 {rebuilt}") } else { "已重建".into() }))
                    .child(Tab::new().label(if incomplete > 0 { format!("待补材料 {incomplete}") } else { "待补材料".into() }))
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.filter = match index {
                            0 => Filter::All,
                            1 => Filter::Rebuilt,
                            _ => Filter::Incomplete,
                        };
                        this.selected = None;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(div().flex_1().min_w(px(140.)).max_w(px(240.)).child(Input::new(&self.search).aria_label("筛选小程序：名称、AppID 或缓存路径").small().prefix(IconName::Search).cleanable(true)))
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
                        menu.label("排序方式").item(item("最近修改", "modified")).item(item("名称", "name")).item(item("文件大小", "size"))
                    }),
            )
            .child(
                Button::new("refresh")
                    .icon(IconName::RefreshCw)
                    .accessibility_label("重新扫描微信缓存")
                    .ghost()
                    .small()
                    .loading(scanning)
                    .tooltip("重新扫描（⌘R）")
                    .on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |library, cx| library.refresh(cx)))),
            )
            .child(
                Button::new("more")
                    .icon(IconName::Ellipsis)
                    .accessibility_label("更多操作")
                    .ghost()
                    .small()
                    .dropdown_menu_with_anchor(Anchor::TopRight, |menu, _, _| {
                        menu.item(PopupMenuItem::new("打开小程序…  ⌘O").icon(IconName::Search).on_click(|_, _, cx| crate::quick_open::open(cx)))
                            .item(PopupMenuItem::new("添加缓存目录…").icon(IconName::FolderPlus).on_click(|_, _, cx| crate::settings_view::add_folder(cx)))
                            .item(PopupMenuItem::new("扫描诊断…").icon(IconName::Activity).on_click(|_, _, cx| {
                                crate::settings_view::open_settings(Some(crate::settings_view::Tab::Scan), cx)
                            }))
                            .separator()
                            .item(PopupMenuItem::new("设置…  ⌘,").icon(IconName::Settings).on_click(|_, _, cx| crate::settings_view::open_settings(None, cx)))
                    }),
            ),
        )
    }

    fn render_tile(&self, app: &Miniapp, summary: Option<RebuildSummary>, cx: &mut Context<Self>) -> impl IntoElement {
        let (icon, ambient) = self.library.update(cx, |library, _| library.icon(app));
        let derived = self.library.update(cx, |library, _| library.derived(app));
        let declared = derived.declares || derived.plugins > 0;
        let selected = self.selected.as_deref() == Some(app.app_id.as_str());
        let hovered = self.hovered.as_deref() == Some(app.app_id.as_str());
        let palette = palette(cx);
        let id = app.app_id.clone();
        let (hover_id, menu_app) = (id.clone(), app.clone());
        let accessible_id = id.clone();
        let title = match &app.name {
            Some(name) if !name.is_empty() => name.clone(),
            _ => format!("未识别 · {}", &app.app_id[app.app_id.len().saturating_sub(6)..]),
        };
        let material = material_status(derived);
        let caption = if derived.is_incomplete() {
            div().text_color(palette.warning).child(material.clone())
        } else if summary.is_some() {
            div().text_color(palette.secondary).child(format!("已重建 · {}", format_size(app.size)))
        } else {
            div().text_color(palette.tertiary).child(format_size(app.size))
        };
        // The tile's accessible name and this marker both expose material state.
        let mark = declared.then(|| {
            let color = if derived.is_incomplete() { palette.warning } else { palette.success };
            div()
                .id("material-state")
                .role(Role::Status)
                .aria_label(material.clone())
                .absolute()
                .top(px(3.))
                .right(px(3.))
                .size(px(12.))
                .rounded_full()
                .border_2()
                .border_color(palette.window)
                .bg(color)
        });
        let gallery = cx.entity().downgrade();
        let accessible_gallery = gallery.clone();
        v_flex()
            .id(SharedString::from(format!("tile-{id}")))
            .role(Role::ListBoxOption)
            .aria_label(format!("{}，{}，{}", title, app.app_id, material))
            .aria_description("方向键选择，回车打开，空格快速查看")
            .aria_selected(selected)
            .when(selected, |this| this.aria_active_descendant().anchor_scroll(Some(self.selection_anchor.clone())))
            .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                accessible_gallery.update(cx, |this, cx| {
                    this.select(&accessible_id, window, cx);
                    this.open(&accessible_id, cx);
                }).ok();
            })
            .w(px(TILE_WIDTH))
            .pt(px(12.))
            .pb(px(10.))
            .gap(px(8.))
            .items_center()
            .rounded(px(16.))
            .when(app.name.is_none() && !hovered && !selected, |this| this.opacity(0.72))
            .when(hovered && !selected, |this| this.bg(palette.fill))
            .child(
                div()
                    .relative()
                    .p(px(7.))
                    .rounded(px(20.))
                    .when(selected, |this| this.bg(palette.fill_strong))
                    .child({
                        let icon = div().relative().child(app_icon(icon, ambient, &title, 60., &palette, hovered));
                        let settling = !hovered && self.unhovered.as_deref() == Some(app.app_id.as_str());
                        if hovered || settling {
                            // Lift on hover, settle back when the pointer leaves.
                            icon.with_animation(
                                SharedString::from(format!("lift-{}-{hovered}", app.app_id)),
                                Animation::new(Duration::from_millis(if hovered { 180 } else { 260 })).with_easing(ease_out_quint()),
                                move |this, t| this.top(px(-3. * if hovered { t } else { 1. - t })),
                            )
                            .into_any_element()
                        } else {
                            icon.into_any_element()
                        }
                    })
                    .children(mark),
            )
            .child(
                v_flex()
                    .w_full()
                    .px_1()
                    .gap(px(3.))
                    .items_center()
                    .child(
                        div()
                            .max_w(px(TILE_WIDTH - 8.))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded_full()
                            .truncate()
                            .text_size(px(12.))
                            .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                            .when(selected, |this| this.bg(palette.accent).text_color(palette.on_accent))
                            .when(!selected, |this| this.text_color(if app.name.is_some() { palette.text } else { palette.secondary }))
                            .child(title.clone()),
                    )
                    .child(caption.text_size(px(10.5))),
            )
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                let next = hovering.then(|| hover_id.clone());
                if this.hovered != next && (*hovering || this.hovered.as_deref() == Some(hover_id.as_str())) {
                    if !*hovering {
                        this.unhovered = Some(hover_id.clone());
                    }
                    this.hovered = next;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.select(&id, window, cx);
                if event.click_count() >= 2 {
                    this.open(&id, cx);
                }
            }))
            .context_menu(move |menu, _, _| app_context_menu(menu, &menu_app, gallery.clone()))
    }

    fn render_recent(&self, apps: &[Miniapp], cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let cards: Vec<AnyElement> = apps
            .iter()
            .map(|app| {
                let (icon, ambient) = self.library.update(cx, |library, _| library.icon(app));
                let summary = self.library.read(cx).rebuilt.get(&app.app_id).copied();
                let derived = self.library.read(cx).derived_ref(&app.app_id);
                let hovered = self.hovered.as_deref() == Some(&*format!("recent-{}", app.app_id));
                let id = app.app_id.clone();
                let accessible_id = id.clone();
                let keyboard_id = id.clone();
                let accessible_gallery = cx.entity().downgrade();
                let hover_key = format!("recent-{id}");
                let detail = if derived.is_incomplete() {
                    material_status(derived)
                } else {
                    match summary {
                        Some(_) => format!("已重建 · {}", format_size(app.size)),
                        None => format!("{} · {}", app.version.as_ref().map(|v| format!("版本 {v}")).unwrap_or_else(|| "版本未知".into()), format_size(app.size)),
                    }
                };
                h_flex()
                    .id(SharedString::from(hover_key.clone()))
                    .role(Role::Button)
                    .aria_label(format!("打开最近使用的小程序：{}，{}，{}", app.display_name(), app.app_id, detail))
                    .tab_index(0)
                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                        accessible_gallery.update(cx, |this, cx| this.open(&accessible_id, cx)).ok();
                    })
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified() && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.open(&keyboard_id, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .flex_1()
                    .min_w_0()
                    .max_w(px(300.))
                    .gap(px(11.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(14.))
                    .bg(palette.raised)
                    .border_1()
                    .border_color(palette.hairline)
                    .focus(|style| style.border_color(palette.accent))
                    .shadow(soft_shadow(&palette, hovered))
                    .child(app_icon(icon, ambient, app.display_name(), 38., &palette, hovered))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().truncate().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(app.display_name().to_string()))
                            .child(div().truncate().text_size(px(10.5)).text_color(palette.secondary).child(detail)),
                    )
                    .child(div().text_size(px(13.)).text_color(if hovered { palette.secondary } else { transparent_black() }).child(IconName::ArrowUpRight))
                    .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                        let next = hovering.then(|| hover_key.clone());
                        if this.hovered != next {
                            this.hovered = next;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open(&id, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_2p5()
            .child(ui::section_label("最近打开", None, &palette).text_size(px(14.)))
            .child(h_flex().gap_3().children(cards))
    }

    fn render_coverage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let library = self.library.read(cx);
        let snapshot = library.snapshot.clone();
        let (readable, inaccessible, errors) =
            snapshot.as_ref().map(|s| (s.readable_roots.len(), s.inaccessible_roots.len(), s.enumeration_errors)).unwrap_or_default();
        let description = if readable == 0 {
            "尚未发现可读取的缓存目录。".to_string()
        } else if inaccessible > 0 {
            format!("{inaccessible} 个缓存目录没有访问权限，其中的小程序不会显示。")
        } else {
            format!("遍历目录时发生 {errors} 次错误。")
        };
        h_flex()
            .gap_3()
            .px(px(14.))
            .py(px(10.))
            .rounded(px(12.))
            .bg(palette.warning.opacity(0.09))
            .border_1()
            .border_color(palette.warning.opacity(0.25))
            .child(div().text_size(px(16.)).text_color(palette.warning).child(IconName::TriangleAlert))
            .child(
                v_flex()
                    .flex_1()
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child("扫描范围有缺口"))
                    .child(div().text_size(px(11.5)).text_color(palette.secondary).child(description)),
            )
            .child(Button::new("cov-add").label("添加目录…").small().on_click(|_, _, cx| crate::settings_view::add_folder(cx)))
            .child(Button::new("cov-diag").label("查看诊断").small().ghost().on_click(|_, _, cx| {
                crate::settings_view::open_settings(Some(crate::settings_view::Tab::Scan), cx)
            }))
    }

    fn render_action_bar(&self, app: &Miniapp, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let (icon, ambient) = self.library.update(cx, |library, _| library.icon(app));
        let copied = self.copied.as_deref() == Some(app.app_id.as_str());
        let detail = format!(
            "{} · {} · {}",
            app.app_id,
            app.version.as_ref().map(|v| format!("版本 {v}")).unwrap_or_else(|| "版本未知".into()),
            format_size(app.size)
        );
        let folder = app.cache_directory();
        let (copy_app, export_app, package_app, open_id) = (app.clone(), app.clone(), app.clone(), app.app_id.clone());
        let capsule = h_flex()
            .id("action-bar")
            .gap_3()
            .pl(px(12.))
            .pr(px(8.))
            .py(px(8.))
            .rounded_full()
            .bg(palette.raised)
            .border_1()
            .border_color(palette.hairline)
            .shadow(soft_shadow(&palette, true))
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(app_icon(icon, ambient, app.display_name(), 28., &palette, false))
            .child(
                v_flex()
                    .max_w(px(300.))
                    .child(div().truncate().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(app.display_name().to_string()))
                    .child(div().truncate().text_size(px(10.5)).font_family(ui::mono_font()).text_color(palette.secondary).child(detail)),
            )
            .child(div().w(px(1.)).h(px(22.)).bg(palette.hairline))
            .child(
                h_flex()
                    .gap_1()
                    .child(Button::new("bar-reveal").icon(IconName::FolderOpen).accessibility_label("在 Finder 中显示缓存目录").ghost().small().tooltip("在 Finder 中显示").on_click(move |_, _, cx| cx.reveal_path(&folder)))
                    .child(
                        Button::new("bar-copy")
                            .icon(if copied { IconName::Check } else { IconName::Copy })
                            .accessibility_label(if copied { "路径已复制" } else { "复制缓存目录路径" })
                            .ghost()
                            .small()
                            .tooltip(if copied { "已复制" } else { "复制路径" })
                            .on_click(cx.listener(move |this, _, _, cx| this.copy_path(&copy_app, cx))),
                    )
                    .child(
                        Button::new("bar-package")
                            .icon(IconName::Package)
                            .accessibility_label("导出主包")
                            .ghost()
                            .small()
                            .tooltip("导出主包…")
                            .on_click(cx.listener(move |_, _, window, cx| crate::export::export_main_package(&package_app, window, cx))),
                    ),
            )
            .child(
                Button::new("bar-export")
                    .icon(IconName::Download)
                    .label("导出审计材料")
                    .small()
                    .rounded_full()
                    .on_click(cx.listener(move |this, _, window, cx| this.export_material(export_app.clone(), window, cx))),
            )
            .child(
                Button::new("bar-open")
                    .icon(IconName::ArrowUpRight)
                    .label("打开")
                    .primary()
                    .small()
                    .rounded_full()
                    .tooltip("打开（↩）")
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&open_id, cx))),
            );
        div()
            .absolute()
            .bottom(px(18.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(capsule.with_animation(
                SharedString::from(format!("bar-{}", app.app_id)),
                Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                |this, delta| this.opacity(delta).relative().top(px(14. * (1. - delta))),
            ))
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
            "部分缓存目录没有访问权限。在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许 WxLocated，然后重新扫描。"
        } else if readable == 0 {
            "没有找到微信小程序缓存目录。确认已在 Mac 版微信中打开过小程序；缓存位置特殊时可以添加目录。"
        } else {
            "在微信中打开过的小程序会出现在这里。打开目标小程序后重新扫描，或者把 .wxapkg 文件拖进这个窗口。"
        };
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div().relative().child(mascot(108.)).with_animation(
                    "mascot-float",
                    Animation::new(Duration::from_millis(2400)).repeat(),
                    |this, delta| this.top(px(-(delta * std::f32::consts::PI).sin() * 6.)),
                ),
            )
            .child(div().mt_2().text_size(px(20.)).font_weight(FontWeight::BOLD).text_color(palette.text).child(if scanning { "正在扫描微信缓存" } else { "还没有找到小程序" }))
            .child(div().max_w(px(440.)).text_size(px(13.)).text_center().text_color(palette.secondary).child(message))
            .when(!scanning, |this| {
                this.child(
                    h_flex()
                        .mt_2()
                        .gap_2()
                        .child(Button::new("scan").icon(IconName::RefreshCw).label("重新扫描").primary().on_click(cx.listener(|this, _, _, cx| {
                            this.library.update(cx, |library, cx| library.refresh(cx))
                        })))
                        .child(Button::new("add-folder").icon(IconName::FolderPlus).label("添加目录…").on_click(|_, _, cx| crate::settings_view::add_folder(cx)))
                        .when(inaccessible, |this| {
                            this.child(Button::new("privacy").label("打开隐私设置").ghost().on_click(|_, _, cx| {
                                cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                            }))
                        }),
                )
            })
    }
}

fn material_status(derived: Derived) -> String {
    match (derived.missing, derived.missing_plugins) {
        (0, 0) if derived.declares || derived.plugins > 0 => "声明的分包和插件已缓存".into(),
        (0, 0) => "未声明分包或插件".into(),
        (packages, 0) => format!("缺 {packages} 个分包"),
        (0, plugins) => format!("缺 {plugins} 个插件"),
        (packages, plugins) => format!("缺 {packages} 个分包 · {plugins} 个插件"),
    }
}

/// Right-click menu on a tile.
type TileAction = Box<dyn Fn(&mut Gallery, Miniapp, &mut Window, &mut Context<Gallery>)>;

fn app_context_menu(menu: PopupMenu, app: &Miniapp, gallery: WeakEntity<Gallery>) -> PopupMenu {
    let item = |label: &'static str, icon: IconName, action: TileAction| {
        let (gallery, app) = (gallery.clone(), app.clone());
        PopupMenuItem::new(label).icon(icon).on_click(move |_, window, cx| {
            gallery.update(cx, |this, cx| action(this, app.clone(), window, cx)).ok();
        })
    };
    menu.item(item("打开", IconName::ArrowUpRight, Box::new(|this, app, _, cx| this.open(&app.app_id, cx))))
        .item(item("快速查看", IconName::Eye, Box::new(|this, app, window, cx| this.quick_look(app, window, cx))))
        .separator()
        .item(item("在 Finder 中显示", IconName::FolderOpen, Box::new(|_, app, _, cx| cx.reveal_path(&app.cache_directory()))))
        .item(item("复制路径", IconName::Copy, Box::new(|this, app, _, cx| this.copy_path(&app, cx))))
        .item(item("复制 AppID", IconName::Hash, Box::new(|_, app, _, cx| ui::copy(app.app_id.clone(), cx))))
        .item(item("设置显示名称…", IconName::Pencil, Box::new(|this, app, window, cx| this.start_rename(app, window, cx))))
        .separator()
        .item(item("导出审计材料…", IconName::Download, Box::new(|this, app, window, cx| this.export_material(app, window, cx))))
        .item(item("导出主包…", IconName::Package, Box::new(|_, app, window, cx| crate::export::export_main_package(&app, window, cx))))
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette: Palette = palette(cx);
        let width = f32::from(window.viewport_size().width) - PADDING * 2. + TILE_GAP;
        self.columns = ((width / (TILE_WIDTH + TILE_GAP)).floor() as usize).max(2);
        let apps = self.visible_apps(cx);
        let library = self.library.read(cx);
        let total = library.apps().len();
        let rebuilt_count = library.apps().iter().filter(|a| library.rebuilt.contains_key(&a.app_id)).count();
        let incomplete_count = library.apps().iter().filter(|a| library.derived_ref(&a.app_id).is_incomplete()).count();
        let gap = library.has_coverage_gap();
        let query_empty = self.query(cx).is_empty();
        let recent: Vec<Miniapp> = if self.filter == Filter::All && query_empty {
            let mut recent = library.recent_apps();
            recent.truncate((self.columns / 2).clamp(2, 4));
            recent
        } else {
            Vec::new()
        };
        let summaries: Vec<Option<RebuildSummary>> = apps.iter().map(|a| library.rebuilt.get(&a.app_id).copied()).collect();
        let selected = self.selected_app(cx);
        let sort_label = match self.library.read(cx).settings.sort_order.as_str() {
            "name" => "按名称",
            "size" => "按文件大小",
            _ => "按最近修改",
        };

        let first_scan = total == 0 && (library.scanning || library.snapshot.is_none() && library.settings.auto_scan_on_launch);
        let body = if first_scan {
            // Placeholder tiles that shimmer until the first scan lands.
            div()
                .flex_1()
                .bg(palette.window)
                .px(px(PADDING))
                .pt(px(20.))
                .child(
                    v_flex()
                        .gap_4()
                        .child(Skeleton::new().h(px(14.)).w(px(90.)))
                        .child(h_flex().flex_wrap().gap(px(TILE_GAP)).children((0..self.columns * 3).map(|_| {
                            v_flex()
                                .w(px(TILE_WIDTH))
                                .pt(px(19.))
                                .pb(px(10.))
                                .gap(px(10.))
                                .items_center()
                                .child(Skeleton::new().size(px(60.)).rounded(px(13.5)))
                                .child(Skeleton::new().h(px(11.)).w(px(72.)))
                                .child(Skeleton::new().h(px(9.)).w(px(44.)))
                        }))),
                )
                .into_any_element()
        } else if total == 0 {
            div().flex_1().flex().bg(palette.window).child(self.render_empty(cx)).into_any_element()
        } else {
            let tiles: Vec<AnyElement> = apps.iter().zip(summaries).map(|(app, summary)| self.render_tile(app, summary, cx).into_any_element()).collect();
            let empty_filter = apps.is_empty().then(|| {
                let (title, message) = if !query_empty {
                    ("没有匹配结果", "试试名称、AppID 或缓存路径的一部分。")
                } else if self.filter == Filter::Incomplete {
                    ("声明的材料都已缓存", "声明的分包与插件都已在本机缓存；进入概要检查源码重建和材料质量。")
                } else {
                    ("本次还没有重建源码", "打开一个小程序窗口，源码会在后台自动重建，完成后显示在这里。")
                };
                div().h(px(300.)).child(ui::empty_state(None, title, message, &palette))
            });
            div()
                .id("gallery-scroll")
                .track_scroll(&self.scroll)
                .flex_1()
                .bg(palette.window)
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .px(px(PADDING))
                        .pt(px(16.))
                        .pb(px(if selected.is_some() { 100. } else { 32. }))
                        .gap(px(26.))
                        .when(gap, |this| this.child(self.render_coverage(cx)))
                        .when(!recent.is_empty(), |this| this.child(self.render_recent(&recent, cx)))
                        .child(
                            v_flex()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .items_baseline()
                                        .gap_2()
                                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(match self.filter {
                                            Filter::All => "全部",
                                            Filter::Rebuilt => "已重建源码",
                                            Filter::Incomplete => "待补材料",
                                        }))
                                        .child(div().text_size(px(12.)).text_color(palette.secondary).child(format!("{} 个", apps.len())))
                                        .child(div().flex_1())
                                        .child(div().text_size(px(11.5)).text_color(palette.tertiary).child(sort_label)),
                                )
                                .children(empty_filter)
                                .child(h_flex().id("app-list").role(Role::ListBox).aria_label("小程序列表").flex_wrap().gap(px(TILE_GAP)).mx(px(-6.)).children(tiles)),
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
            .aria_label("小程序画廊")
            .key_context("Gallery")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .text_color(palette.text)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &crate::FocusSearch, window, cx| this.search.update(cx, |input, cx| input.focus(window, cx))))
            .child(self.render_header(total, rebuilt_count, incomplete_count, cx))
            .children(ui::settings_notice(cx))
            .child(body)
            .when_some(selected, |this, app| this.child(self.render_action_bar(&app, cx)))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(palette.accent.opacity(0.06)).border_2().border_color(palette.accent))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.open_dropped(paths, window, cx)))
    }
}
