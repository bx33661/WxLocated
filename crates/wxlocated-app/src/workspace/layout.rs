//! Window chrome, action routing and the source tree.

use std::collections::BTreeMap;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tree::{TreeItem, tree};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wxlocated_engine::decompile::Confidence;

use crate::library::{Library, RebuildSummary};
use crate::theme::{Palette, palette};
use crate::ui::{self, app_icon, file_icon, file_tint};
use crate::workspace_actions::*;

use super::{Phase, STEPS, View, Workspace};

impl Workspace {
    pub(super) fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded() else {
            self.visible_files = 0;
            self.tree.update(cx, |tree, cx| tree.set_items(Vec::new(), cx));
            return;
        };
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let paths: Vec<&str> = loaded.result.artifacts.iter().map(|a| a.path.as_str())
            .filter(|p| self.show_containers || !p.starts_with("__wxlocated_"))
            .filter(|p| query.is_empty() || p.to_lowercase().contains(&query)).collect();
        self.visible_files = paths.len();
        let items = build_tree(&paths, !query.is_empty() || paths.len() <= 40);
        let selected = self.selected.as_deref().filter(|path| paths.contains(path)).map(|path| TreeItem::new(path.to_string(), path.to_string()));
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

    fn show_overview(&mut self, _: &ShowOverview, window: &mut Window, cx: &mut Context<Self>) {
        self.select(None, window, cx);
    }

    fn toggle_raw(&mut self, _: &ToggleRaw, window: &mut Window, cx: &mut Context<Self>) {
        let has_raw = self.selected.as_ref().and_then(|p| self.artifact(p)).is_some_and(|(l, i)| Self::raw_entry(&l, &l.result.artifacts[i]).is_some());
        if !has_raw {
            return;
        }
        let next = if self.view == View::Raw { View::Rebuilt } else { View::Raw };
        self.set_view(next, window, cx);
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.show_inspector = !self.show_inspector;
        cx.notify();
    }

    fn back_to_gallery(&mut self, _: &BackToGallery, window: &mut Window, cx: &mut Context<Self>) {
        crate::focus_gallery(cx);
        window.remove_window();
    }

    fn rebuild(&mut self, _: &Rebuild, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_reading(cx);
        self.load(window, cx);
    }

    fn focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |input, cx| input.focus(window, cx));
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
            move |parent| {
                wxlocated_engine::export::export_source(&loaded.result, loaded.seconds, &loaded.set, parent, &folder).map_err(|e| e.to_string())
            },
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
                "enter" | "down" => {
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
        } else if self.tree_focus.contains_focused(window, cx)
            && matches!(event.keystroke.key.as_str(), "enter" | "space")
        {
            let path = self.tree.read(cx).selected_entry().filter(|entry| !entry.is_folder()).map(|entry| entry.item().id.to_string());
            if let Some(path) = path {
                self.select(Some(path), window, cx);
                cx.stop_propagation();
            }
        }
    }

    fn set_panel_width(&mut self, sidebar: bool, width: f32, cx: &mut Context<Self>) {
        Library::global(cx).update(cx, |library, cx| {
            if sidebar {
                library.settings.sidebar_width = width;
            } else {
                library.settings.inspector_width = width;
            }
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
            Some(choice) if choice.path.is_none() => "全部包".to_string(),
            Some(choice) => choice.title.split(" · ").nth(1).unwrap_or(&choice.title).trim_end_matches(".wxapkg").to_string(),
            None => "包".into(),
        };
        let weak = cx.entity().downgrade();
        let export_weak = weak.clone();
        ui::toolbar(52., &palette).child(
            h_flex()
            .w_full()
            .h_full()
            .gap_2()
            .child(
                Button::new("back")
                    .icon(IconName::LayoutGrid)
                    .accessibility_label("返回画廊")
                    .ghost()
                    .small()
                    .tooltip(ui::shortcut("返回画廊（⌘[）"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(BackToGallery), cx)),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(palette.hairline))
            .child(app_icon(icon, ambient, self.app.display_name(), 26., &palette, false))
            .child(
                v_flex()
                    .min_w(px(120.))
                    .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(self.app.display_name().to_string()))
                    .child(div().text_size(px(11.)).text_color(palette.secondary).child(match &self.app.version {
                        Some(version) => format!("版本 {version}"),
                        None => "版本未知".into(),
                    })),
            )
            .child(div().flex_1())
            .child(self.render_status(&palette))
            .child(div().flex_1())
            .when(self.packages.len() > 1, |this| {
                this.child(
                    Button::new("package")
                        .icon(IconName::Package)
                        .label(package_label)
                        .small()
                        .ghost()
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
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
            .child(
                Button::new("export")
                    .icon(IconName::Download)
                    .label("导出")
                    .small()
                    .tooltip(ui::shortcut("导出审计材料、当前文件或源码工程（审计材料 ⌘E）"))
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
            )
            .child(
                Button::new("inspector")
                    .icon(IconName::PanelRight)
                    .accessibility_label("显示或隐藏检查器")
                    .ghost()
                    .small()
                    .selected(self.show_inspector)
                    .tooltip(ui::shortcut("显示或隐藏检查器（⌥⌘0）"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleInspector), cx)),
            ),
        )
    }

    /// Centre readout: rebuild progress, then what was rebuilt.
    fn render_status(&self, palette: &Palette) -> AnyElement {
        let pill = h_flex().gap_2().px_3().py(px(5.)).rounded_full().bg(palette.fill).text_size(px(12.)).text_color(palette.secondary);
        match &self.phase {
            Phase::Loading => pill
                .child(Spinner::new().color(palette.accent))
                .child(format!("{}…", STEPS[self.step.min(STEPS.len() - 1)]))
                .child(div().text_color(palette.tertiary).child(format!("{}/{}", self.step + 1, STEPS.len())))
                .into_any_element(),
            Phase::Failed(_) => pill.child(div().text_color(palette.warning).child(IconName::Package)).child("原始包 · 源码不可重建").into_any_element(),
            Phase::Ready(loaded) => {
                let RebuildSummary { generated, degraded } = loaded.summary;
                pill.child(ui::dot(if degraded == 0 { palette.success } else { palette.warning }, 7.))
                    .child(if degraded == 0 { format!("已重建 {generated} 个文件") } else { format!("已重建 {generated} 个文件 · {degraded} 个降级") })
                    .child(div().text_color(palette.tertiary).child(format!("{:.1} 秒", loaded.seconds)))
                    .into_any_element()
            }
        }
    }

    // MARK: Sidebar

    fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let weak = cx.entity().downgrade();
        let selected = self.selected.clone();
        let overview_selected = selected.is_none();
        let footer = self.loaded().map(|loaded| {
            let RebuildSummary { generated, degraded } = loaded.summary;
            let passthrough = loaded.result.artifacts.len() - generated;
            format!("{generated} 个重建 · {passthrough} 个原样 · {degraded} 个降级")
        });
        let agent_bases: Vec<String> = self.layers.iter().flat_map(|l| l.manifest.files.iter().filter_map(|f| f.base.clone())).collect();
        v_flex()
            .w(px(width))
            .h_full()
            .flex_none()
            .bg(palette.sidebar)
            .border_r_1()
            .border_color(palette.hairline)
            .child(
                v_flex()
                    .p_3()
                    .pb_2()
                    .gap_2()
                    .child(Input::new(&self.filter).aria_label("筛选当前包的文件，回车进入文件树，Escape 清空筛选").small().prefix(IconName::ListFilter).cleanable(true))
                    .child(
                        h_flex()
                            .id("overview-row")
                            .role(Role::Button)
                            .aria_label("打开材料概要")
                            .on_a11y_action(AccessibleAction::Click, {
                                let weak = cx.entity().downgrade();
                                move |_, window, cx| { weak.update(cx, |this, cx| this.select(None, window, cx)).ok(); }
                            })
                            .gap_2()
                            .px_2()
                            .py(px(6.))
                            .rounded(px(7.))
                            .text_size(px(13.))
                            .when(overview_selected, |this| this.bg(palette.accent_soft).text_color(palette.accent))
                            .when(!overview_selected, |this| this.hover(|this| this.bg(palette.fill)))
                            .child(IconName::LayoutDashboard)
                            .child("概要")
                            .child(div().flex_1())
                            .child(div().text_size(px(10.5)).text_color(palette.tertiary).child(ui::shortcut("⌘0")))
                            .on_click(cx.listener(|this, _, window, cx| this.select(None, window, cx))),
                    )
                    .child(
                        h_flex()
                            .pt_1()
                            .text_size(px(11.))
                            .text_color(palette.secondary)
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("源码"))
                            .child(div().flex_1())
                            .child(
                                Button::new("containers")
                                    .label(if self.show_containers { "隐藏原始容器" } else { "显示原始容器" })
                                    .ghost()
                                    .xsmall()
                                    .tooltip("__wxlocated_ 开头的是重建时保留的原始编译产物")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_containers = !this.show_containers;
                                        this.rebuild_tree(cx);
                                    })),
                            ),
                    ),
            )
            .child(match &self.phase {
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
                Phase::Ready(_) if self.visible_files == 0 => {
                    let filtering = !self.filter.read(cx).value().trim().is_empty();
                    let message = if filtering { "当前包没有匹配文件。清空筛选后可继续浏览。" } else { "当前包没有可显示的源码，可以查看原始容器。" };
                    div().id("file-tree-empty").role(Role::Status).aria_label(message).flex_1().p_4()
                        .child(ui::empty_state(Some(IconName::Search), "没有匹配文件", message, &palette)
                            .child(Button::new("clear-file-filter").label(if filtering { "清空筛选" } else { "显示原始容器" }).mt_2()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if filtering { this.filter.update(cx, |input, cx| input.set_value("", window, cx)); }
                                    else { this.show_containers = true; this.rebuild_tree(cx); }
                                    this.filter.update(cx, |input, cx| input.focus(window, cx));
                                })))).into_any_element()
                }
                Phase::Ready(loaded) => {
                    let loaded = loaded.clone();
                    div()
                        .track_focus(&self.tree_focus)
                        .flex_1()
                        .min_h_0()
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
                                    .on_click(move |_, window, cx| {
                                        if !is_folder {
                                            let path = path.clone();
                                            weak.update(cx, |this, cx| this.select(Some(path), window, cx)).ok();
                                        }
                                    })
                            })
                            .size_full(),
                        )
                        .into_any_element()
                }
            })
            .child(
                h_flex()
                    .px_4()
                    .py_2p5()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(palette.hairline)
                    .text_size(px(11.))
                    .text_color(palette.secondary)
                    .child(div().text_size(px(11.)).child(IconName::Code))
                    .child(footer.unwrap_or_else(|| if matches!(self.phase, Phase::Loading) { "正在重建…".into() } else { " ".into() })),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        // Fade new content in when the file, its view or the package changes.
        let key = format!("{}|{:?}|{}|{}", self.selected.as_deref().unwrap_or("overview"), view_key(self.view), self.package, self.loaded().is_some());
        let (sidebar_width, inspector_width) = {
            let settings = &Library::global(cx).read(cx).settings;
            (settings.sidebar_width.clamp(200., 520.), settings.inspector_width.clamp(220., 480.))
        };
        let content = match self.selected.clone() {
            Some(path) => self.render_file(&path, cx),
            None => self.render_overview(cx),
        };
        let content = div().flex_1().min_w_0().h_full().flex().child(content).with_animation(
            SharedString::from(key),
            Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
            |this, t| this.opacity(0.35 + 0.65 * t),
        );
        v_flex()
            .id("workspace")
            .role(Role::Group)
            .aria_label(format!("{} 源码工作区", self.app.display_name()))
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .text_color(palette.text)
            .capture_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(Self::export_material))
            .on_action(cx.listener(Self::show_overview))
            .on_action(cx.listener(Self::toggle_raw))
            .on_action(cx.listener(Self::toggle_inspector))
            .on_action(cx.listener(Self::back_to_gallery))
            .on_action(cx.listener(Self::rebuild))
            .on_action(cx.listener(Self::focus_filter))
            .child(self.render_toolbar(cx))
            .children(ui::settings_notice(cx))
            .child(
                h_flex()
                    .id("panels")
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    // Panel edges drag to resize; widths persist across windows.
                    .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SidebarEdge>, _, cx| {
                        let width = f32::from(event.event.position.x - event.bounds.left()).clamp(200., 520.);
                        this.set_panel_width(true, width, cx);
                    }))
                    .on_drag_move(cx.listener(|this, event: &DragMoveEvent<InspectorEdge>, _, cx| {
                        let width = f32::from(event.bounds.right() - event.event.position.x).clamp(220., 480.);
                        this.set_panel_width(false, width, cx);
                    }))
                    .on_drop(cx.listener(|_, _: &SidebarEdge, _, cx| save_panel_widths(cx)))
                    .on_drop(cx.listener(|_, _: &InspectorEdge, _, cx| save_panel_widths(cx)))
                    .child(self.render_sidebar(sidebar_width, cx))
                    .child(edge_handle("sidebar-edge", SidebarEdge))
                    .child(div().flex_1().min_w_0().h_full().flex().bg(palette.window).child(content))
                    .when(self.show_inspector, |this| {
                        this.child(edge_handle("inspector-edge", InspectorEdge)).child(self.render_inspector(inspector_width, cx))
                    }),
            )
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(palette.accent.opacity(0.05)).border_2().border_color(palette.accent))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.on_drop(paths, window, cx)))
    }
}

/// Drag payloads for the two panel edges.
#[derive(Clone)]
struct SidebarEdge;
#[derive(Clone)]
struct InspectorEdge;

impl Render for SidebarEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Render for InspectorEdge {
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

fn view_key(view: View) -> String {
    match view {
        View::Rebuilt => "rebuilt".into(),
        View::Raw => "raw".into(),
        View::Agent(index) => format!("agent{index}"),
    }
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
