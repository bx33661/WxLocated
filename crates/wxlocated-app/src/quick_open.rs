//! ⌘O: type to find a mini program, Return to open it.

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wxlocated_engine::scan::Miniapp;

use crate::library::Library;
use crate::theme::palette;
use crate::ui::{self, app_icon, badge};

struct QuickOpenWindow(AnyWindowHandle, Entity<InputState>);

impl Global for QuickOpenWindow {}

pub fn open(cx: &mut App) {
    if let Some((handle, query)) = cx.try_global::<QuickOpenWindow>().map(|w| (w.0, w.1.clone()))
        && handle.update(cx, |_, window, cx| {
            window.activate_window();
            query.update(cx, |input, cx| input.focus(window, cx));
        }).is_ok()
    {
        return;
    }
    let options = WindowOptions {
        is_resizable: false,
        focus: true,
        ..crate::theme::window_options("打开小程序", Bounds::centered(None, size(px(560.), px(440.)), cx), None, 56.)
    };
    if let Ok((handle, view)) = gpui_kit::open_window(options, cx, |window, cx| {
        crate::theme::follow_appearance(window, cx);
        cx.new(|cx| QuickOpen::new(window, cx))
    }) {
        cx.set_global(QuickOpenWindow(handle, view.read(cx).query.clone()));
        cx.activate(true);
    }
}

pub struct QuickOpen {
    query: Entity<InputState>,
    selected: usize,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl QuickOpen {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("名称或 AppID"));
        query.update(cx, |input, cx| input.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.selected = 0;
                    this.scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.open_selected(window, cx),
                _ => {}
            }),
            cx.observe(&Library::global(cx), |_, _, cx| cx.notify()),
        ];
        Self { query, selected: 0, focus: cx.focus_handle().tab_stop(true), scroll: ScrollHandle::new(), _subscriptions: subscriptions }
    }

    /// Recent apps first, then the rest by modification time.
    fn results(&self, cx: &App) -> Vec<Miniapp> {
        let library = Library::global(cx).read(cx);
        let text = self.query.read(cx).value().trim().to_lowercase();
        let recent = &library.settings.recent_app_ids;
        let mut apps: Vec<Miniapp> = library
            .apps()
            .iter()
            .filter(|a| text.is_empty() || a.display_name().to_lowercase().contains(&text) || a.app_id.to_lowercase().contains(&text))
            .cloned()
            .collect();
        apps.sort_by_key(|a| (recent.iter().position(|id| *id == a.app_id).unwrap_or(usize::MAX), std::cmp::Reverse(a.modified)));
        apps
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let results = self.results(cx);
        if let Some(app) = results.get(self.selected).or(results.first()) {
            let id = app.app_id.clone();
            Library::global(cx).update(cx, |library, cx| library.record_use(&id, cx));
            crate::workspace::open_app_window(&id, cx);
            window.remove_window();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.results(cx).len().min(60);
        if event.keystroke.modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" if count > 0 => self.selected = (self.selected + 1).min(count - 1),
            "up" => self.selected = self.selected.saturating_sub(1),
            "home" if self.focus.is_focused(window) => self.selected = 0,
            "end" if self.focus.is_focused(window) && count > 0 => self.selected = count - 1,
            "enter" if self.focus.is_focused(window) => {
                self.open_selected(window, cx);
                cx.stop_propagation();
                return;
            }
            "escape" => {
                window.remove_window();
                cx.stop_propagation();
                return;
            }
            _ => return,
        }
        cx.stop_propagation();
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }
}

impl Render for QuickOpen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let results = self.results(cx);
        self.selected = self.selected.min(results.len().min(60).saturating_sub(1));
        let result_count = if results.len() > 60 { format!("显示前 60 / {} 个，请输入更多关键词缩小范围", results.len()) } else { format!("{} 个", results.len()) };
        let rows: Vec<AnyElement> = results
            .iter()
            .take(60)
            .enumerate()
            .map(|(index, app)| {
                let (icon, ambient) = Library::global(cx).update(cx, |library, _| library.icon(app));
                let derived = Library::global(cx).update(cx, |library, _| library.derived(app));
                let material = match (derived.missing, derived.missing_plugins) {
                    (0, 0) => None,
                    (packages, 0) => Some(format!("缺 {packages} 个分包")),
                    (0, plugins) => Some(format!("缺 {plugins} 个插件")),
                    (packages, plugins) => Some(format!("缺 {packages} 个分包 · {plugins} 个插件")),
                };
                let id = app.app_id.clone();
                let selected = index == self.selected;
                h_flex()
                    .id(index)
                    .role(Role::ListBoxOption)
                    .aria_label(format!("{}，{}{}", app.display_name(), app.app_id, material.as_ref().map(|text| format!("，{text}")).unwrap_or_default()))
                    .aria_selected(selected)
                    .when(selected, |this| this.aria_active_descendant())
                    .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                        Library::global(cx).update(cx, |library, cx| library.record_use(&id, cx));
                        crate::workspace::open_app_window(&id, cx);
                        window.remove_window();
                    })
                    .gap(px(10.))
                    .flex_shrink_0()
                    .mx_2()
                    .px_2()
                    .py(px(6.))
                    .rounded(px(8.))
                    .when(selected, |this| this.bg(palette.accent_soft))
                    .hover(|this| this.bg(palette.fill))
                    .child(app_icon(icon, ambient, app.display_name(), 28., &palette, false))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(app.display_name().to_string()))
                            .child(div().text_size(px(10.5)).font_family(ui::mono_font()).text_color(palette.secondary).child(app.app_id.clone())),
                    )
                    .when_some(material, |this, text| this.child(badge(text, palette.warning)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.selected = index;
                        this.focus.focus(window, cx);
                        cx.notify();
                        if event.click_count() >= 2 {
                            this.open_selected(window, cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .id("quick-open")
            .role(Role::Group)
            .aria_label("快速打开小程序")
            .size_full()
            .bg(palette.window)
            .text_color(palette.text)
            .capture_key_down(cx.listener(Self::on_key))
            .child(
                ui::toolbar(56., &palette).child(
                    h_flex()
                        .w_full()
                        .h_full()
                        .gap_2()
                        .child(div().text_size(px(16.)).text_color(palette.accent).child(IconName::Search))
                        .child(div().flex_1().child(Input::new(&self.query).aria_label("按名称或 AppID 搜索小程序").appearance(false).text_size(px(17.)))),
                ),
            )
            .child(if rows.is_empty() {
                ui::empty_state(None, "没有匹配的小程序", "试试名称或 AppID 的一部分。", &palette).into_any_element()
            } else {
                v_flex().id("results").role(Role::ListBox).aria_label("匹配的小程序，方向键选择，回车打开").track_focus(&self.focus).track_scroll(&self.scroll).flex_1().overflow_y_scroll().py_2().gap_0p5().children(rows).into_any_element()
            })
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .border_t_1()
                    .border_color(palette.hairline)
                    .text_size(px(11.))
                    .text_color(palette.tertiary)
                    .child("↑↓ 选择")
                    .child("↩ 打开")
                    .child("esc 关闭")
                    .child(div().flex_1())
                    .child(div().id("result-count").role(Role::Status).aria_label(result_count.clone()).child(result_count)),
            )
    }
}
