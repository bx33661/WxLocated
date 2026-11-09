//! WxLocated desktop app (GPUI). The same engine as the `wxlocated` CLI.

mod diff;
mod export;
mod gallery;
mod library;
mod native_open;
mod quick_open;
mod settings_view;
#[cfg(feature = "snapshot")]
mod snapshot;
mod theme;
mod ui;
mod workspace;

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, Search, SelectAll, Undo};
use gpui_kit::*;

use crate::gallery::Gallery;
use crate::library::Library;

actions!(
    wxlocated,
    [
        Quit,
        About,
        OpenSettings,
        Refresh,
        QuickOpen,
        ShowGallery,
        HideApp,
        CloseWindow,
        Minimize,
        Zoom,
        OpenContract,
        OpenRepository,
        FocusSearch,
    ]
);

/// Actions the frontmost app window handles; greyed out elsewhere.
pub mod workspace_actions {
    gpui_kit::actions!(workspace, [ExportMaterial, ShowOverview, ToggleRaw, ToggleInspector, BackToGallery, Rebuild, FocusFilter]);
}

/// The gallery window, to bring back from an app window.
struct GalleryWindow(AnyWindowHandle);

impl Global for GalleryWindow {}

pub fn focus_gallery(cx: &mut App) {
    if let Some(handle) = cx.try_global::<GalleryWindow>().map(|g| g.0)
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    open_gallery(cx);
}

fn open_gallery(cx: &mut App) {
    let focus = std::env::var_os("WXLOCATED_NO_FOCUS").is_none();
    let options = theme::window_options("WxLocated", gallery_bounds(cx), Some(size(px(820.), px(560.))), 60.);
    let Ok((window, gallery)) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::follow_appearance(window, cx);
        cx.new(|cx| Gallery::new(window, cx))
    }) else {
        return;
    };
    cx.set_global(GalleryWindow(window));
    #[cfg(feature = "snapshot")]
    snapshot::schedule(window, gallery, cx);
    #[cfg(not(feature = "snapshot"))]
    let _ = gallery;
    if focus {
        cx.activate(true);
    }
}

/// The gallery's last frame when it still lies on a connected display,
/// otherwise a centered default.
fn gallery_bounds(cx: &App) -> Bounds<Pixels> {
    let saved = Library::global(cx).read(cx).settings.gallery_frame;
    if let Some([x, y, width, height]) = saved
        && width >= 820.
        && height >= 560.
    {
        let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
        let center = bounds.center();
        if cx.displays().iter().any(|display| display.bounds().contains(&center)) {
            return bounds;
        }
    }
    Bounds::centered(None, size(px(1180.), px(780.)), cx)
}

fn main() {
    let application = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    // Finder and Dock deliver both package files and overlay folders here.
    let (opened, received) = async_channel::unbounded::<Vec<String>>();
    application.on_open_urls(move |urls| {
        let _ = opened.try_send(urls);
    });
    application.on_reopen(focus_gallery);
    application.run(move |cx| {
        gpui_kit::init(cx);
        Library::init(cx);
        theme::init(cx);
        bind(cx);
        // Panel widths and the gallery frame change often; write them once, on quit.
        cx.on_app_quit(|cx| {
            Library::global(cx).update(cx, |library, cx| library.save_settings(cx));
            async {}
        })
        .detach();
        open_gallery(cx);
        cx.spawn(async move |cx| {
            while let Ok(paths) = received.recv().await {
                cx.update(|cx| native_open::open_urls(paths, cx));
            }
        })
        .detach();
    });
}


fn bind(cx: &mut App) {
    use workspace_actions::*;
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &Refresh, cx| Library::global(cx).update(cx, |library, cx| library.refresh(cx)));
    cx.on_action(|_: &OpenSettings, cx| settings_view::open_settings(None, cx));
    cx.on_action(|_: &About, cx| settings_view::open_settings(Some(settings_view::Tab::About), cx));
    cx.on_action(|_: &QuickOpen, cx| quick_open::open(cx));
    cx.on_action(|_: &ShowGallery, cx| focus_gallery(cx));
    cx.on_action(|_: &OpenContract, cx| cx.open_url("https://github.com/bx33661/WxLocated#cli"));
    cx.on_action(|_: &OpenRepository, cx| cx.open_url("https://github.com/bx33661/WxLocated"));
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        }
    });
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.minimize_window()).ok();
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.zoom_window()).ok();
        }
    });

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", HideApp, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-o", QuickOpen, None),
        KeyBinding::new("cmd-shift-1", ShowGallery, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-e", ExportMaterial, Some("Workspace")),
        KeyBinding::new("cmd-0", ShowOverview, Some("Workspace")),
        KeyBinding::new("alt-cmd-r", ToggleRaw, Some("Workspace")),
        KeyBinding::new("alt-cmd-0", ToggleInspector, Some("Workspace")),
        KeyBinding::new("cmd-[", BackToGallery, Some("Workspace")),
        KeyBinding::new("cmd-shift-o", FocusFilter, Some("Workspace")),
        KeyBinding::new("cmd-f", FocusSearch, Some("Gallery")),
    ]);

    cx.set_menus(vec![
        Menu {
            name: "WxLocated".into(),
            disabled: false,
            items: vec![
                MenuItem::action("关于 WxLocated", About),
                MenuItem::separator(),
                MenuItem::action("设置…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("隐藏 WxLocated", HideApp),
                MenuItem::action("退出 WxLocated", Quit),
            ],
        },
        Menu {
            name: "文件".into(),
            disabled: false,
            items: vec![
                MenuItem::action("打开小程序…", QuickOpen),
                MenuItem::action("显示画廊", ShowGallery),
                MenuItem::separator(),
                MenuItem::action("导出审计材料…", ExportMaterial),
                MenuItem::separator(),
                MenuItem::action("关闭窗口", CloseWindow),
            ],
        },
        Menu {
            name: "编辑".into(),
            disabled: false,
            items: vec![
                MenuItem::action("撤销", Undo),
                MenuItem::action("重做", Redo),
                MenuItem::separator(),
                MenuItem::action("剪切", Cut),
                MenuItem::action("拷贝", Copy),
                MenuItem::action("粘贴", Paste),
                MenuItem::action("全选", SelectAll),
                MenuItem::separator(),
                MenuItem::action("查找…", Search),
                MenuItem::action("筛选文件", FocusFilter),
            ],
        },
        Menu {
            name: "显示".into(),
            disabled: false,
            items: vec![
                MenuItem::action("重新扫描", Refresh),
                MenuItem::separator(),
                MenuItem::action("概要", ShowOverview),
                MenuItem::action("切换原始 / 重建", ToggleRaw),
                MenuItem::action("显示或隐藏检查器", ToggleInspector),
                MenuItem::action("重新重建源码", Rebuild),
                MenuItem::separator(),
                MenuItem::action("返回画廊", BackToGallery),
            ],
        },
        Menu {
            name: "窗口".into(),
            disabled: false,
            items: vec![MenuItem::action("最小化", Minimize), MenuItem::action("缩放", Zoom)],
        },
        Menu {
            name: "帮助".into(),
            disabled: false,
            items: vec![MenuItem::action("Agent 接口契约", OpenContract), MenuItem::action("WxLocated 项目主页", OpenRepository)],
        },
    ]);
}
