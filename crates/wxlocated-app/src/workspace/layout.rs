//! Workspace header, 3-tab navigation, left navigator (Pages / Tree / Search),
//! and action routing.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use gpui_kit::Anchor;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tree::{TreeItem, tree};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wxlocated_engine::decompile::{ArtifactKind, Confidence};
use wxlocated_engine::material::Completeness;

use crate::library::Library;
use crate::theme::palette;
use crate::ui::{self, app_icon, file_icon, file_tint};
use crate::workspace_actions::*;

use super::{BrowseMode, Loaded, Phase, STEPS, TreeFilter, Workspace, WorkspaceTab};

impl Workspace {
    pub(super) fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded() else {
            self.visible_files = 0;
            self.tree.update(cx, |tree, cx| tree.set_items(Vec::new(), cx));
            return;
        };
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let paths: Vec<&str> = loaded
            .result
            .artifacts
            .iter()
            .filter(|a| self.show_containers || !a.path.starts_with("__wxlocated_"))
            .filter(|a| match self.tree_filter {
                TreeFilter::All => true,
                TreeFilter::Script => matches!(a.kind, ArtifactKind::Script | ArtifactKind::Wxs),
                TreeFilter::Markup => a.kind == ArtifactKind::Markup,
                TreeFilter::Config => a.kind == ArtifactKind::Configuration,
                TreeFilter::Degraded => a.confidence <= Confidence::Medium,
            })
            .map(|a| a.path.as_str())
            .filter(|p| query.is_empty() || p.to_lowercase().contains(&query))
            .collect();
        self.visible_files = paths.len();
        let items = build_tree(&paths, !query.is_empty() || paths.len() <= 40);
        let selected = self
            .selected
            .as_deref()
            .filter(|path| paths.contains(path))
            .map(|path| TreeItem::new(path.to_string(), path.to_string()));
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            if let Some(item) = selected.as_ref() {
                tree.set_selected_item(Some(item), cx);
            }
        });
        cx.notify();
    }

    fn export_material(&mut self, _: &ExportMaterial, window: &mut Window, cx: &mut Context<Self>) {
        let context = self.context(cx);
        crate::export::export_material(self.app.clone(), context, window, cx);
    }

    fn show_overview(&mut self, _: &ShowOverview, _: &mut Window, cx: &mut Context<Self>) {
        self.tab = WorkspaceTab::Packages;
        cx.notify();
    }

    fn toggle_raw(&mut self, _: &ToggleRaw, window: &mut Window, cx: &mut Context<Self>) {
        let has_raw = self
            .selected
            .as_ref()
            .and_then(|p| self.artifact(p))
            .is_some_and(|(l, i)| Self::raw_entry(&l, &l.result.artifacts[i]).is_some());
        if !has_raw {
            return;
        }
        self.tab = WorkspaceTab::Code;
        let next = if self.view == super::View::Raw { super::View::Rebuilt } else { super::View::Raw };
        self.set_view(next, false, window, cx);
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.tab = if self.tab == WorkspaceTab::Agent { WorkspaceTab::Code } else { WorkspaceTab::Agent };
        cx.notify();
    }

    fn back_to_gallery(&mut self, _: &BackToGallery, window: &mut Window, cx: &mut Context<Self>) {
        if self.embedded {
            crate::show_home_in_shell(cx);
        } else {
            crate::focus_gallery(cx);
            window.remove_window();
        }
    }

    fn rebuild(&mut self, _: &Rebuild, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_reading(cx);
        if let Some(latest) = Library::global(cx).read(cx).app(&self.app.app_id).cloned() {
            self.app = latest;
        }
        self.load(window, cx);
    }

    fn focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = WorkspaceTab::Code;
        if self.browse_mode == BrowseMode::Search {
            self.browse_mode = BrowseMode::Tree;
        }
        self.filter.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn focus_global_search(&mut self, _: &FocusGlobalSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = WorkspaceTab::Code;
        self.browse_mode = BrowseMode::Search;
        self.global_search.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn choose_package(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index != self.package {
            self.remember_reading(cx);
            self.package = index;
            self.load(window, cx);
        }
    }

    fn export_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded() else { return };
        let folder = [Some(self.app.display_name().to_string()), self.app.version.clone(), Some("重建源码".into())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("-");
        crate::export::into_folder(
            "正在导出重建源码…",
            move |parent| wxlocated_engine::export::export_source(&loaded.result, loaded.seconds, &loaded.set, parent, &folder).map_err(|e| e.to_string()),
            window,
            cx,
        );
    }

    fn on_drop(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        crate::native_open::open_paths(paths.paths(), cx);
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let filter_focused = self.filter.focus_handle(cx).is_focused(window);
        if filter_focused {
            match event.keystroke.key.as_str() {
                "escape" => {
                    self.filter.update(cx, |input, cx| input.set_value("", window, cx));
                    self.tree.update(cx, |tree, cx| tree.focus(window, cx));
                    cx.stop_propagation();
                }
                "enter" | "down" if self.browse_mode == BrowseMode::Tree => {
                    self.tree.update(cx, |tree, cx| {
                        if tree.selected_index().is_none() && tree.entry(0).is_some() {
                            tree.set_selected_index(Some(0), cx);
                        }
                        tree.focus(window, cx);
                    });
                    cx.stop_propagation();
                }
                _ => {}
            }
        } else if self.tree_focus.contains_focused(window, cx) && matches!(event.keystroke.key.as_str(), "enter" | "space") {
            let path = self.tree.read(cx).selected_entry().filter(|entry| !entry.is_folder()).map(|entry| entry.item().id.to_string());
            if let Some(path) = path {
                self.select(Some(path), window, cx);
                cx.stop_propagation();
            }
        }
    }

    fn set_panel_width(&mut self, width: f32, cx: &mut Context<Self>) {
        Library::global(cx).update(cx, |library, cx| {
            library.settings.sidebar_width = width;
            cx.notify();
        });
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let (icon, ambient) = Library::global(cx).update(cx, |library, _| library.icon(&self.app));
        let has_file = self.selected.is_some();
        let ready = self.loaded().is_some();
        let package_titles: Vec<String> = self.packages.iter().map(|p| p.title.clone()).collect();
        let current_package = self.package;
        let package_label = match self.packages.get(self.package) {
            Some(choice) if choice.path.is_none() => format!("全部包 ({})", self.app.all_packages().len()),
            Some(choice) => choice.title.split(" · ").nth(1).unwrap_or(&choice.title).trim_end_matches(".wxapkg").to_string(),
            None => "包".into(),
        };
        let pkg_tab_label = match self.loaded() {
            Some(loaded) => {
                let completeness = Completeness::new(&self.app, Some(&loaded.analysis.plugins), &self.context(cx));
                let found = 1 + completeness.found_subpackages.len() + completeness.found_plugins.len();
                let total = 1 + completeness.declared_subpackages.len() + completeness.plugins.len();
                format!("分包监控 {found}/{total}")
            }
            None => "分包监控".into(),
        };
        let selected_tab = match self.tab {
            WorkspaceTab::Code => 0,
            WorkspaceTab::Packages => 1,
            WorkspaceTab::Agent => 2,
        };
        let weak = cx.entity().downgrade();
        let export_weak = weak.clone();

        ui::panel_header(48., self.embedded, &palette).child(
            h_flex()
                .w_full()
                .h_full()
                .when(!cfg!(target_os = "macos"), |this| this.pr(px(10.)))
                .gap_2p5()
                .child(
                    Button::new("back")
                        .icon(IconName::ArrowLeft)
                        .label("缓存列表")
                        .ghost()
                        .small()
                        .tooltip(ui::shortcut("返回微信实时缓存（⌘[）"))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(BackToGallery), cx)),
                )
                .child(div().w(px(1.)).h(px(18.)).bg(palette.hairline))
                .child(app_icon(icon, ambient, self.app.display_name(), 24., &palette, false))
                .child(
                    h_flex()
                        .min_w(px(48.))
                        .flex_shrink_1()
                        .max_w(px(220.))
                        .gap_1p5()
                        .items_baseline()
                        .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(self.app.display_name().to_string()))
                        .when_some(self.app.version.clone(), |this, v| {
                            this.child(div().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.tertiary).child(format!("v{v}")))
                        }),
                )
                .when(self.packages.len() > 1, |this| {
                    this.child(
                        Button::new("package")
                            .icon(IconName::Package)
                            .label(package_label)
                            .xsmall()
                            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                                let mut menu = menu.label("切换主包、分包或全部包");
                                for (index, title) in package_titles.iter().enumerate() {
                                    let weak = weak.clone();
                                    menu = menu.item(PopupMenuItem::new(title.clone()).checked(index == current_package).on_click(move |_, window, cx| {
                                        weak.update(cx, |this, cx| this.choose_package(index, window, cx)).ok();
                                    }));
                                }
                                menu
                            }),
                    )
                })
                .child(div().flex_1())
                .child(
                    TabBar::new("ws-main-tabs")
                        .segmented()
                        .small()
                        .selected_index(selected_tab)
                        .child(Tab::new().label("源码与搜索"))
                        .child(Tab::new().label(pkg_tab_label))
                        .child(Tab::new().label(if self.layers.is_empty() { "Agent 交付与证据".into() } else { format!("Agent 交付 · {} 层", self.layers.len()) }))
                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                            this.tab = match index {
                                0 => WorkspaceTab::Code,
                                1 => WorkspaceTab::Packages,
                                _ => WorkspaceTab::Agent,
                            };
                            cx.notify();
                        })),
                )
                .child(div().flex_1())
                .when(matches!(self.phase, Phase::Loading), |this| {
                    this.child(
                        h_flex()
                            .gap_1p5()
                            .px_2()
                            .py(px(3.))
                            .rounded_full()
                            .bg(palette.fill)
                            .text_size(px(11.))
                            .text_color(palette.secondary)
                            .child(Spinner::new().color(palette.accent))
                            .child(STEPS[self.step.min(STEPS.len() - 1)]),
                    )
                })
                .child(
                    Button::new("rebuild-btn")
                        .icon(IconName::RefreshCw)
                        .accessibility_label("重新分析当前小程序包")
                        .ghost()
                        .small()
                        .disabled(matches!(self.phase, Phase::Loading))
                        .tooltip("重新扫描分包并还原源码")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(Rebuild), cx)),
                )
                .child(
                    Button::new("export")
                        .icon(IconName::Download)
                        .label("导出审计材料")
                        .primary()
                        .small()
                        .tooltip(ui::shortcut("导出审计材料、当前文件或源码工程（⌘E）"))
                        .disabled(!ready)
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                            let (a, b) = (export_weak.clone(), export_weak.clone());
                            menu.item(PopupMenuItem::new(ui::shortcut("导出审计材料…  ⌘E")).icon(IconName::PackageCheck).on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ExportMaterial), cx)
                            }))
                            .separator()
                            .item(PopupMenuItem::new("导出当前文件…").icon(IconName::FileDown).disabled(!has_file).on_click(move |_, window, cx| {
                                a.update(cx, |this, cx| this.export_current_file(window, cx)).ok();
                            }))
                            .item(PopupMenuItem::new("导出源码工程…").icon(IconName::FolderDown).on_click(move |_, window, cx| {
                                b.update(cx, |this, cx| this.export_project(window, cx)).ok();
                            }))
                        }),
                ),
        )
    }

    // MARK: Left Navigator

    fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let browse_index = match self.browse_mode {
            BrowseMode::Pages => 0,
            BrowseMode::Tree => 1,
            BrowseMode::Search => 2,
        };
        let footer = self.loaded().map(|loaded| {
            let generated = loaded.summary.generated;
            let passthrough = loaded.result.artifacts.len().saturating_sub(generated);
            format!("{generated} 重建 · {passthrough} 原样 · {} 降级", loaded.summary.degraded)
        });

        let body: AnyElement = match &self.phase {
            Phase::Loading => v_flex()
                .flex_1()
                .px_4()
                .pt_2()
                .gap(px(11.))
                .children([150., 110., 180., 90., 160., 130., 100., 170., 120.].into_iter().enumerate().map(|(i, w)| {
                    h_flex().gap_2().pl(px(if i % 3 == 0 { 0. } else { 16. })).child(Skeleton::new().size(px(14.))).child(Skeleton::new().h(px(10.)).w(px(w)))
                }))
                .into_any_element(),
            Phase::Failed(message) => {
                div().flex_1().p_4().child(ui::empty_state(Some(IconName::FileX), "无法读取这个包", message.clone(), &palette)).into_any_element()
            }
            Phase::Ready(loaded) => match self.browse_mode {
                BrowseMode::Pages => self.render_pages_navigator(loaded, cx),
                BrowseMode::Tree => self.render_tree_navigator(loaded, cx),
                BrowseMode::Search => self.render_global_search(cx),
            },
        };

        v_flex()
            .w(px(width))
            .h_full()
            .flex_none()
            .bg(palette.sidebar)
            .border_r_1()
            .border_color(palette.hairline)
            .child(
                div().p_2p5().pb_2().child(
                    TabBar::new("ws-browse-mode")
                        .segmented()
                        .small()
                        .selected_index(browse_index)
                        .child(Tab::new().label("按页面"))
                        .child(Tab::new().label("目录树"))
                        .child(Tab::new().label("全文搜"))
                        .on_click(cx.listener(|this, index: &usize, window, cx| {
                            this.browse_mode = match index {
                                0 => BrowseMode::Pages,
                                1 => BrowseMode::Tree,
                                _ => BrowseMode::Search,
                            };
                            if this.browse_mode == BrowseMode::Search {
                                this.global_search.update(cx, |input, cx| input.focus(window, cx));
                            }
                            cx.notify();
                        })),
                ),
            )
            .child(body)
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(palette.hairline)
                    .text_size(px(11.))
                    .text_color(palette.tertiary)
                    .child(div().text_size(px(11.)).child(IconName::Code))
                    .child(footer.unwrap_or_else(|| if matches!(self.phase, Phase::Loading) { "正在还原…".into() } else { " ".into() })),
            )
    }

    fn render_pages_navigator(&self, loaded: &Loaded, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let active_stem = self.selected.as_deref().and_then(|path| {
            ["js", "wxml", "wxss", "json"]
                .iter()
                .find_map(|ext| path.strip_suffix(&format!(".{ext}")))
        });

        let mut page_files_set: HashSet<String> = HashSet::new();
        for page in &loaded.analysis.page_paths {
            for ext in ["js", "wxml", "wxss", "json"] {
                page_files_set.insert(format!("{page}.{ext}"));
            }
        }

        let pages: Vec<(String, String, Option<String>)> = loaded
            .analysis
            .page_paths
            .iter()
            .filter_map(|page| {
                let first_file = ["js", "wxml", "json", "wxss"]
                    .iter()
                    .map(|ext| format!("{page}.{ext}"))
                    .find(|candidate| loaded.index.contains_key(candidate))?;
                let title = loaded.page_titles.get(page).cloned();
                if !query.is_empty()
                    && !page.to_lowercase().contains(&query)
                    && !title.as_deref().is_some_and(|t| t.to_lowercase().contains(&query))
                {
                    return None;
                }
                Some((page.clone(), first_file, title))
            })
            .collect();

        let shared_files: Vec<String> = loaded
            .result
            .artifacts
            .iter()
            .filter(|a| !a.path.starts_with("__wxlocated_"))
            .filter(|a| !matches!(a.kind, ArtifactKind::Image | ArtifactKind::Font | ArtifactKind::Media | ArtifactKind::Binary))
            .filter(|a| !page_files_set.contains(&a.path))
            .filter(|a| query.is_empty() || a.path.to_lowercase().contains(&query))
            .take(150)
            .map(|a| a.path.clone())
            .collect();

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .child(Input::new(&self.filter).aria_label("筛选页面或全局脚本").small().prefix(IconName::ListFilter).cleanable(true)),
            )
            .child(if pages.is_empty() && shared_files.is_empty() {
                div()
                    .flex_1()
                    .p_4()
                    .child(ui::empty_state(Some(IconName::Search), "没有匹配的页面", "清空筛选或切换到“目录树”浏览全部文件。", &palette))
                    .into_any_element()
            } else {
                div()
                    .id("pages-nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .px_2()
                            .pb_3()
                            .gap_3()
                            .when(!pages.is_empty(), |this| {
                                this.child(
                                    v_flex()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .px_2()
                                                .py_1()
                                                .text_size(px(11.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(palette.tertiary)
                                                .child(format!("页面路由 ({})", pages.len())),
                                        )
                                        .children(pages.into_iter().enumerate().map(|(ix, (page, open_file, title))| {
                                            let is_active = active_stem == Some(page.as_str());
                                            let fallback_name = page.rsplit('/').find(|seg| *seg != "index").unwrap_or(&page).to_string();
                                            let primary = title.unwrap_or(fallback_name);
                                            v_flex()
                                                .id(SharedString::from(format!("pg-row-{ix}")))
                                                .px_2p5()
                                                .py(px(6.))
                                                .gap(px(1.5))
                                                .rounded(px(7.))
                                                .cursor_pointer()
                                                .when(is_active, |this| this.bg(palette.accent_soft))
                                                .when(!is_active, |this| this.hover(|s| s.bg(palette.fill)))
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_size(px(12.5))
                                                        .font_weight(if is_active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                                                        .text_color(if is_active { palette.accent } else { palette.text })
                                                        .child(primary),
                                                )
                                                .child(div().truncate().text_size(px(10.5)).font_family(ui::mono_font()).text_color(palette.tertiary).child(page))
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    this.select(Some(open_file.clone()), window, cx);
                                                }))
                                        })),
                                )
                            })
                            .when(!shared_files.is_empty(), |this| {
                                this.child(
                                    v_flex()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .px_2()
                                                .py_1()
                                                .text_size(px(11.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(palette.tertiary)
                                                .child("公共配置、工具与组件"),
                                        )
                                        .children(shared_files.into_iter().enumerate().map(|(ix, file_path)| {
                                            let is_active = self.selected.as_deref() == Some(file_path.as_str());
                                            let open = file_path.clone();
                                            h_flex()
                                                .id(SharedString::from(format!("sh-row-{ix}")))
                                                .gap_2()
                                                .px_2p5()
                                                .py(px(5.))
                                                .rounded(px(6.))
                                                .cursor_pointer()
                                                .when(is_active, |this| this.bg(palette.accent_soft).text_color(palette.accent).font_weight(FontWeight::MEDIUM))
                                                .when(!is_active, |this| this.text_color(palette.text).hover(|s| s.bg(palette.fill)))
                                                .child(div().text_size(px(12.)).text_color(file_tint(&file_path, &palette)).child(file_icon(&file_path)))
                                                .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).font_family(ui::mono_font()).child(file_path))
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    this.select(Some(open.clone()), window, cx);
                                                }))
                                        })),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_tree_navigator(&self, loaded: &Arc<Loaded>, cx: &mut Context<Self>) -> AnyElement {
        let palette = palette(cx);
        let weak = cx.entity().downgrade();
        let selected = self.selected.clone();
        let agent_bases: Vec<String> = self.layers.iter().flat_map(|l| l.manifest.files.iter().filter_map(|f| f.base.clone())).collect();
        let loaded = loaded.clone();

        let type_chips = [
            (TreeFilter::All, "全部"),
            (TreeFilter::Script, "JS"),
            (TreeFilter::Markup, "WXML"),
            (TreeFilter::Config, "JSON"),
            (TreeFilter::Degraded, "降级"),
        ];

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .px_3()
                    .pb_2()
                    .gap_1p5()
                    .border_b_1()
                    .border_color(palette.hairline)
                    .child(Input::new(&self.filter).aria_label("筛选当前包的文件").small().prefix(IconName::ListFilter).cleanable(true))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .children(type_chips.into_iter().map(|(tf, label)| {
                                Button::new(SharedString::from(format!("tf-{label}")))
                                    .label(label)
                                    .xsmall()
                                    .ghost()
                                    .selected(self.tree_filter == tf)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.tree_filter != tf {
                                            this.tree_filter = tf;
                                            this.rebuild_tree(cx);
                                        }
                                    }))
                            }))
                            .child(
                                Button::new("containers")
                                    .label(if self.show_containers { "隐藏编译产物" } else { "编译产物" })
                                    .xsmall()
                                    .ghost()
                                    .selected(self.show_containers)
                                    .tooltip("__wxlocated_ 开头的是还原时保留的原始编译容器")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_containers = !this.show_containers;
                                        this.rebuild_tree(cx);
                                    })),
                            ),
                    ),
            )
            .child(if self.visible_files == 0 {
                let filtering = !self.filter.read(cx).value().trim().is_empty() || self.tree_filter != TreeFilter::All;
                div()
                    .id("file-tree-empty")
                    .role(Role::Status)
                    .flex_1()
                    .p_4()
                    .child(
                        ui::empty_state(
                            Some(IconName::Search),
                            "没有匹配文件",
                            if filtering { "重置筛选条件后可继续浏览全部源码。" } else { "当前包没有可显示的源码，可以查看编译产物容器。" },
                            &palette,
                        )
                        .child(Button::new("clear-file-filter").label(if filtering { "重置筛选" } else { "显示编译产物" }).small().mt_2().on_click(
                            cx.listener(move |this, _, window, cx| {
                                if filtering {
                                    this.tree_filter = TreeFilter::All;
                                    this.filter.update(cx, |input, cx| input.set_value("", window, cx));
                                } else {
                                    this.show_containers = true;
                                }
                                this.rebuild_tree(cx);
                            }),
                        )),
                    )
                    .into_any_element()
            } else {
                div()
                    .track_focus(&self.tree_focus)
                    .flex_1()
                    .min_h_0()
                    .pt_1()
                    .child(
                        tree(&self.tree, move |ix, entry, _selected, _window, cx| {
                            let item = entry.item();
                            let path = item.id.to_string();
                            let palette = crate::theme::palette(cx);
                            let is_folder = entry.is_folder();
                            let confidence = (!is_folder).then(|| loaded.index.get(&path).map(|&i| loaded.result.artifacts[i].confidence)).flatten();
                            let has_agent = !is_folder && agent_bases.contains(&path);
                            let is_selected = selected.as_deref() == Some(path.as_str());
                            let weak = weak.clone();
                            ListItem::new(ix)
                                .selected(is_selected)
                                .pl(px(10. + entry.depth() as f32 * 14.))
                                .child(
                                    h_flex()
                                        .gap(px(7.))
                                        .text_size(px(12.5))
                                        .child(if is_folder {
                                            div()
                                                .text_color(palette.secondary)
                                                .child(if entry.is_expanded() { IconName::FolderOpen } else { IconName::Folder })
                                                .into_any_element()
                                        } else {
                                            div().text_color(file_tint(&path, &palette)).child(file_icon(&path)).into_any_element()
                                        })
                                        .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
                                        .when(has_agent, |this| this.child(div().text_size(px(11.)).text_color(palette.accent).child(IconName::Sparkles)))
                                        .when_some(confidence.filter(|c| *c <= Confidence::Medium), |this, _| this.child(ui::dot(palette.warning, 6.))),
                                )
                                // IMPORTANT: omit `.on_click` on folders so `gpui_base::Tree` handles folder expand/collapse cleanly.
                                .when(!is_folder, |this| {
                                    this.on_click(move |_, window, cx| {
                                        weak.update(cx, |this, cx| this.select(Some(path.clone()), window, cx)).ok();
                                    })
                                })
                        })
                        .size_full(),
                    )
                    .into_any_element()
            })
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let sidebar_width = Library::global(cx).read(cx).settings.sidebar_width.clamp(210., 460.);

        let main_stage: AnyElement = match self.tab {
            WorkspaceTab::Code => h_flex()
                .id("ws-code-panels")
                .flex_1()
                .min_h_0()
                .items_start()
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SidebarEdge>, _, cx| {
                    let width = f32::from(event.event.position.x - event.bounds.left()).clamp(210., 460.);
                    this.set_panel_width(width, cx);
                }))
                .on_drop(cx.listener(|_, _: &SidebarEdge, _, cx| save_panel_widths(cx)))
                .child(self.render_sidebar(sidebar_width, cx))
                .child(edge_handle("sidebar-edge", SidebarEdge))
                .child(div().flex_1().min_w_0().h_full().flex().bg(palette.code).child(self.render_code_stage(cx)))
                .into_any_element(),
            WorkspaceTab::Packages => self.render_packages_tab(cx),
            WorkspaceTab::Agent => self.render_agent_tab(cx),
        };

        v_flex()
            .id("workspace")
            .role(Role::Group)
            .aria_label(format!("{} 源码工作区", self.app.display_name()))
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .bg(palette.window)
            .text_color(palette.text)
            .capture_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(Self::export_material))
            .on_action(cx.listener(Self::show_overview))
            .on_action(cx.listener(Self::toggle_raw))
            .on_action(cx.listener(Self::toggle_inspector))
            .on_action(cx.listener(Self::back_to_gallery))
            .on_action(cx.listener(Self::rebuild))
            .on_action(cx.listener(Self::focus_filter))
            .on_action(cx.listener(Self::focus_global_search))
            .child(self.render_toolbar(cx))
            .when(!self.embedded, |this| this.children(ui::settings_notice(cx)))
            .child(main_stage)
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(palette.accent.opacity(0.05)).border_2().border_color(palette.accent))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.on_drop(paths, window, cx)))
    }
}

#[derive(Clone)]
struct SidebarEdge;

impl Render for SidebarEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn edge_handle<T: Clone + Render>(id: &'static str, payload: T) -> impl IntoElement {
    gpui_kit::component::resize_handle::<T, T>(id, Axis::Horizontal).on_drag(payload, |payload, _, _, cx| cx.new(|_| payload.clone()))
}

fn save_panel_widths(cx: &mut App) {
    Library::global(cx).update(cx, |library, cx| library.save_settings(cx));
}

/// Nested tree items from sorted paths; folders first at each level.
fn build_tree(paths: &[&str], expanded: bool) -> Vec<TreeItem> {
    #[derive(Default)]
    struct Node {
        folders: BTreeMap<String, Node>,
        files: Vec<(String, String)>,
    }
    let mut root = Node::default();
    for path in paths {
        let mut node = &mut root;
        let parts: Vec<&str> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            node = node.folders.entry(part.to_string()).or_default();
        }
        node.files.push((path.to_string(), parts[parts.len() - 1].to_string()));
    }
    fn items(node: &Node, prefix: &str, expanded: bool) -> Vec<TreeItem> {
        let mut result: Vec<TreeItem> = node
            .folders
            .iter()
            .map(|(name, child)| {
                let id = format!("{prefix}{name}/");
                TreeItem::new(format!("dir:{id}"), name.clone()).expanded(expanded).children(items(child, &id, expanded))
            })
            .collect();
        result.extend(node.files.iter().map(|(path, name)| TreeItem::new(path.clone(), name.clone())));
        result
    }
    items(&root, "", expanded)
}

#[cfg(test)]
mod tests {
    use super::build_tree;

    #[test]
    fn tree_puts_folders_first_and_nests() {
        let items = build_tree(&["b.js", "pages/x/index.wxml", "pages/x/index.js", "a.json"], true);
        let labels: Vec<_> = items.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(labels, ["pages", "b.js", "a.json"]);
        assert_eq!(items[0].children[0].label.as_ref(), "x");
        assert_eq!(items[0].children[0].children.len(), 2);
    }
}
