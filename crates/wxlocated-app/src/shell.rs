//! Unified single-window application shell: master navigation rail on the
//! left and the active workspace (live cache home, exported materials hub,
//! or an opened mini program) on the right.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wxlocated_engine::scan::Miniapp;

use crate::gallery::Gallery;
use crate::library::Library;
use crate::theme::palette;
use crate::ui::{self, app_icon, badge, display_path, empty_state};
use crate::workspace::Workspace;

#[derive(Clone, PartialEq, Eq)]
pub enum Stage {
    Home,
    Exports,
    App(String),
}

pub struct AppShell {
    library: Entity<Library>,
    gallery: Entity<Gallery>,
    stage: Stage,
    workspaces: HashMap<String, Entity<Workspace>>,
    _subscriptions: Vec<Subscription>,
}

impl AppShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let library = Library::global(cx);
        let gallery = cx.new(|cx| Gallery::new(window, cx));
        let subscriptions = vec![
            cx.observe(&library, |_, _, cx| cx.notify()),
            cx.observe_window_bounds(window, |this, window, cx| {
                if let WindowBounds::Windowed(bounds) = window.window_bounds() {
                    let frame = [
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    ];
                    this.library.update(cx, |library, _| library.settings.gallery_frame = Some(frame));
                }
            }),
        ];
        Self {
            library,
            gallery,
            stage: Stage::Home,
            workspaces: HashMap::new(),
            _subscriptions: subscriptions,
        }
    }

    #[cfg(all(feature = "snapshot", target_os = "macos"))]
    pub fn gallery_entity(&self) -> Entity<Gallery> {
        self.gallery.clone()
    }

    pub fn show_home(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Home;
        cx.notify();
    }

    pub fn show_exports(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Exports;
        cx.notify();
    }

    pub fn open_app(&mut self, app_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Workspace>> {
        let app = self.library.read(cx).app(app_id)?.clone();
        self.library.update(cx, |library, cx| library.record_use(app_id, cx));
        let needs_new = self
            .workspaces
            .get(app_id)
            .is_none_or(|ws| !ws.read(cx).matches_app(&app));
        if needs_new {
            let ws = cx.new(|cx| Workspace::new(app, true, window, cx));
            self.workspaces.insert(app_id.to_string(), ws);
        }
        self.stage = Stage::App(app_id.to_string());
        cx.notify();
        self.workspaces.get(app_id).cloned()
    }

    fn sidebar_apps(&self, cx: &App) -> Vec<Miniapp> {
        let library = self.library.read(cx);
        let all = library.all_apps();
        let mut chosen = Vec::new();
        for recent_id in &library.settings.recent_app_ids {
            if let Some(app) = all.iter().find(|a| &a.app_id == recent_id) {
                chosen.push(app.clone());
            }
            if chosen.len() >= 6 {
                return chosen;
            }
        }
        for app in all {
            if !chosen.iter().any(|c| c.app_id == app.app_id) {
                chosen.push(app);
                if chosen.len() >= 6 {
                    break;
                }
            }
        }
        chosen
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let library = self.library.read(cx);
        let total_apps = library.all_apps().len();
        let scanning = library.scanning;
        let monitoring = library.settings.monitor_cache_changes;
        let export_count = library.settings.exported_materials.len();
        let recent_apps = self.sidebar_apps(cx);
        let cli_ready = crate::settings_view::cli_installed();

        let home_active = self.stage == Stage::Home;
        let exports_active = self.stage == Stage::Exports;

        let nav_button = |id: &'static str, icon: IconName, label: &'static str, active: bool, trailing: AnyElement| {
            h_flex()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .gap_2()
                .px_2p5()
                .py(px(6.))
                .rounded(px(7.))
                .cursor_pointer()
                .text_size(px(12.5))
                .when(active, |this| this.bg(palette.accent_soft).text_color(palette.accent).font_weight(FontWeight::SEMIBOLD))
                .when(!active, |this| this.text_color(palette.secondary).hover(|s| s.bg(palette.fill).text_color(palette.text)))
                .child(div().text_size(px(13.5)).child(icon))
                .child(div().flex_1().truncate().child(label))
                .child(trailing)
        };

        v_flex()
            .w(px(224.))
            .h_full()
            .flex_none()
            .bg(palette.sidebar)
            .border_r_1()
            .border_color(palette.hairline)
            .child(ui::sidebar_header(
                48.,
                &palette,
                h_flex()
                    .w_full()
                    .h_full()
                    .items_center()
                    .justify_between()
                    .when(!cfg!(target_os = "macos"), |this| {
                        this.child(
                            h_flex()
                                .gap_1p5()
                                .text_size(px(13.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(palette.text)
                                .child("WxLocated"),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_family(ui::mono_font())
                            .text_color(palette.tertiary)
                            .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                    ),
            ))
            .child(
                v_flex()
                    .id("sidebar-nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2p5()
                    .gap_4()
                    .child(
                        h_flex()
                            .id("sidebar-quick-open")
                            .role(Role::Button)
                            .aria_label("快速跳转或搜索小程序")
                            .px_2p5()
                            .py(px(6.))
                            .gap_2()
                            .rounded(px(7.))
                            .bg(palette.raised)
                            .border_1()
                            .border_color(palette.hairline)
                            .cursor_pointer()
                            .hover(|s| s.border_color(palette.accent))
                            .child(div().text_size(px(12.)).text_color(palette.tertiary).child(IconName::Search))
                            .child(div().flex_1().text_size(px(12.)).text_color(palette.secondary).child("快速跳转…"))
                            .child(div().text_size(px(10.5)).font_family(ui::mono_font()).text_color(palette.tertiary).child(ui::shortcut("⌘O")))
                            .on_click(|_, _, cx| crate::quick_open::open(cx)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().px_2().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(palette.tertiary).child("采集与发现"))
                            .child(
                                nav_button(
                                    "nav-home",
                                    IconName::Radar,
                                    "微信实时缓存",
                                    home_active,
                                    h_flex()
                                        .gap_1()
                                        .when(monitoring && !scanning, |this| this.child(ui::dot(palette.success, 6.)))
                                        .child(div().text_size(px(11.)).font_family(ui::mono_font()).child(total_apps.to_string()))
                                        .into_any_element(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.show_home(cx))),
                            )
                            .child(
                                nav_button(
                                    "nav-exports",
                                    IconName::PackageCheck,
                                    "已导出审计材料",
                                    exports_active,
                                    div().text_size(px(11.)).font_family(ui::mono_font()).child(export_count.to_string()).into_any_element(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.show_exports(cx))),
                            ),
                    )
                    .when(!recent_apps.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(div().px_2().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(palette.tertiary).child("最近小程序"))
                                .children(recent_apps.into_iter().map(|app| {
                                    let active = matches!(&self.stage, Stage::App(id) if id == &app.app_id);
                                    let (icon, ambient) = self.library.update(cx, |library, _| library.icon(&app));
                                    let incomplete = self.library.read(cx).derived_ref(&app.app_id).is_incomplete();
                                    let open_id = app.app_id.clone();
                                    let menu_id = app.app_id.clone();
                                    let name = app.display_name().to_string();
                                    h_flex()
                                        .id(SharedString::from(format!("sb-app-{}", app.app_id)))
                                        .gap_2()
                                        .px_2()
                                        .py(px(5.5))
                                        .rounded(px(7.))
                                        .cursor_pointer()
                                        .when(active, |this| this.bg(palette.accent_soft).text_color(palette.text).font_weight(FontWeight::SEMIBOLD))
                                        .when(!active, |this| this.text_color(palette.secondary).hover(|s| s.bg(palette.fill).text_color(palette.text)))
                                        .child(app_icon(icon, ambient, &name, 20., &palette, false))
                                        .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).child(name))
                                        .child(ui::dot(if incomplete { palette.warning } else { palette.success }, 6.))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_app(&open_id, window, cx);
                                        }))
                                        .context_menu(move |menu, _, _| {
                                            let id_shell = menu_id.clone();
                                            let id_win = menu_id.clone();
                                            menu.item(PopupMenuItem::new("切换查看").icon(IconName::ArrowUpRight).on_click(move |_, _, cx| {
                                                crate::open_app(&id_shell, cx);
                                            }))
                                            .item(PopupMenuItem::new("在新窗口中打开").icon(IconName::ExternalLink).on_click(move |_, _, cx| {
                                                crate::workspace::open_app_window(&id_win, cx);
                                            }))
                                        })
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .p_2p5()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(palette.hairline)
                    .child(
                        Button::new("sb-open-local")
                            .icon(IconName::FolderInput)
                            .label("打开本地 .wxapkg…")
                            .small()
                            .ghost()
                            .w_full()
                            .on_click(|_, _, cx| crate::native_open::prompt_open(cx)),
                    )
                    .child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .justify_between()
                            .text_size(px(11.))
                            .text_color(palette.tertiary)
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .child(ui::dot(if cli_ready { palette.success } else { palette.tertiary }, 6.))
                                    .child(if cli_ready { "CLI 已就绪" } else { "CLI 未安装" }),
                            )
                            .child(
                                Button::new("sb-settings")
                                    .icon(IconName::Settings)
                                    .label("设置")
                                    .xsmall()
                                    .ghost()
                                    .on_click(|_, _, cx| crate::settings_view::open_settings(None, cx)),
                            ),
                    ),
            )
    }

    fn render_exports(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let records = self.library.read(cx).settings.exported_materials.clone();

        let body = if records.is_empty() {
            div()
                .flex_1()
                .flex()
                .child(
                    empty_state(
                        Some(IconName::PackageCheck),
                        "还没有导出记录",
                        "在「微信实时缓存」中点开任意小程序并点击「导出审计材料」，导出目录会保留在这里，方便随时打开目录或复制给 Agent 的指令。",
                        &palette,
                    )
                    .child(
                        Button::new("exports-go-home")
                            .label("前往微信实时缓存")
                            .primary()
                            .mt_2()
                            .on_click(cx.listener(|this, _, _, cx| this.show_home(cx))),
                    ),
                )
                .into_any_element()
        } else {
            div()
                .id("exports-scroll")
                .flex_1()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(920.))
                        .mx_auto()
                        .p_6()
                        .gap_3()
                        .children(records.into_iter().enumerate().map(|(ix, record)| {
                            let path = PathBuf::from(&record.root);
                            let exists = path.is_dir();
                            let reveal_path = path.clone();
                            let prompt = crate::export::agent_prompt(&record.name, &record.app_id, &path);
                            let remove_root = record.root.clone();
                            let app_id = record.app_id.clone();
                            let app_exists_in_lib = self.library.read(cx).app(&app_id).is_some();
                            let open_app_id = app_id.clone();

                            h_flex()
                                .id(ix)
                                .gap_4()
                                .p_4()
                                .rounded(px(10.))
                                .bg(palette.raised)
                                .border_1()
                                .border_color(palette.hairline)
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(record.name.clone()))
                                                .when_some(record.version.clone(), |this, v| this.child(badge(format!("v{v}"), palette.secondary)))
                                                .when(!exists, |this| this.child(badge("目录已移动或删除", palette.warning))),
                                        )
                                        .child(div().text_size(px(11.5)).font_family(ui::mono_font()).text_color(palette.secondary).child(record.app_id.clone()))
                                        .child(div().truncate().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.tertiary).child(display_path(&record.root))),
                                )
                                .child(
                                    h_flex()
                                        .flex_none()
                                        .gap_1p5()
                                        .when(app_exists_in_lib, |this| {
                                            this.child(Button::new(SharedString::from(format!("exp-app-{ix}"))).label("查看源码").small().ghost().on_click(
                                                cx.listener(move |this, _, window, cx| {
                                                    this.open_app(&open_app_id, window, cx);
                                                }),
                                            ))
                                        })
                                        .child(
                                            Button::new(SharedString::from(format!("exp-open-{ix}")))
                                                .icon(IconName::FolderOpen)
                                                .label("打开目录")
                                                .small()
                                                .disabled(!exists)
                                                .on_click(move |_, _, cx| cx.reveal_path(&reveal_path)),
                                        )
                                        .child(
                                            Button::new(SharedString::from(format!("exp-prompt-{ix}")))
                                                .icon(IconName::Copy)
                                                .label("复制 Agent 指令")
                                                .small()
                                                .primary()
                                                .on_click(move |_, window, cx| {
                                                    ui::copy(prompt.clone(), cx);
                                                    crate::export::done("已复制给 AI Agent 的审计指令", None, window, cx);
                                                }),
                                        )
                                        .child(
                                            Button::new(SharedString::from(format!("exp-del-{ix}")))
                                                .icon(IconName::Trash)
                                                .ghost()
                                                .xsmall()
                                                .tooltip("从记录中移除（不删除磁盘文件）")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    let root = remove_root.clone();
                                                    this.library.update(cx, |library, cx| {
                                                        library.settings.exported_materials.retain(|r| r.root != root);
                                                        library.save_settings(cx);
                                                    });
                                                })),
                                        ),
                                )
                        })),
                )
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .h_full()
            .bg(palette.window)
            .child(
                ui::panel_header(48., true, &palette).child(
                    h_flex()
                        .w_full()
                        .h_full()
                        .when(!cfg!(target_os = "macos"), |this| this.pr(px(10.)))
                        .justify_between()
                        .child(div().text_size(px(14.5)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child("已导出审计材料"))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(palette.secondary)
                                .child("每个导出目录均含 AGENT.md、material.json、source/ 与 raw/"),
                        ),
                ),
            )
            .children(ui::settings_notice(cx))
            .child(body)
            .into_any_element()
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let stage: AnyElement = match &self.stage {
            Stage::Home => self.gallery.clone().into_any_element(),
            Stage::Exports => self.render_exports(cx),
            Stage::App(id) => match self.workspaces.get(id) {
                Some(ws) => ws.clone().into_any_element(),
                None => self.gallery.clone().into_any_element(),
            },
        };

        h_flex()
            .id("app-shell")
            .size_full()
            .bg(palette.window)
            .text_color(palette.text)
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().flex().child(stage))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(palette.accent.opacity(0.05)).border_2().border_color(palette.accent))
            .on_drop(|paths: &ExternalPaths, _, cx| crate::native_open::open_paths(paths.paths(), cx))
    }
}
