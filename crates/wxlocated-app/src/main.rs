#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! WxLocated desktop app (GPUI). The same engine as the `wxlocated` CLI.

mod diff;
mod export;
mod gallery;
mod library;
mod native_open;
mod quick_open;
mod settings_view;
mod shell;
#[cfg(all(feature = "snapshot", target_os = "macos"))]
mod snapshot;
mod theme;
mod ui;
mod workspace;

use std::path::PathBuf;

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, Search, SelectAll, Undo};
use gpui_kit::*;

use crate::library::Library;
use crate::shell::AppShell;

actions!(
    wxlocated,
    [
        Quit,
        About,
        OpenSettings,
        Refresh,
        QuickOpen,
        OpenLocalPackage,
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

/// Actions the active miniapp workspace handles; greyed out elsewhere.
pub mod workspace_actions {
    gpui_kit::actions!(
        workspace,
        [ExportMaterial, ShowOverview, ToggleRaw, ToggleInspector, BackToGallery, Rebuild, FocusFilter, FocusGlobalSearch]
    );
}

#[derive(Clone)]
struct MainWindow(AnyWindowHandle, Entity<AppShell>);

impl Global for MainWindow {}

pub fn main_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<MainWindow>().map(|w| w.0)
}

pub fn focus_gallery(cx: &mut App) {
    if let Some(handle) = main_window(cx)
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    open_main_window(cx);
}

pub fn show_home_in_shell(cx: &mut App) {
    if let Some(MainWindow(handle, shell)) = cx.try_global::<MainWindow>().cloned()
        && handle
            .update(cx, |_, window, cx| {
                window.activate_window();
                shell.update(cx, |shell, cx| shell.show_home(cx));
            })
            .is_ok()
    {
        return;
    }
    focus_gallery(cx);
}

/// Opens a mini program inside the unified main shell window, or falls back to
/// a standalone workspace window if no shell window is alive.
pub fn open_app(app_id: &str, cx: &mut App) {
    if let Some(MainWindow(handle, shell)) = cx.try_global::<MainWindow>().cloned()
        && let Ok(Some(_)) = handle.update(cx, |_, window, cx| {
            window.activate_window();
            shell.update(cx, |shell, cx| shell.open_app(app_id, window, cx))
        })
    {
        return;
    }
    workspace::open_app_window(app_id, cx);
}

/// Routes an overlay folder import into the unified shell when present.
pub fn open_overlay_in_shell(app_id: &str, work: PathBuf, cx: &mut App) -> bool {
    if let Some(MainWindow(handle, shell)) = cx.try_global::<MainWindow>().cloned()
        && let Ok(true) = handle.update(cx, |_, window, cx| {
            window.activate_window();
            if let Some(ws) = shell.update(cx, |shell, cx| shell.open_app(app_id, window, cx)) {
                ws.update(cx, |ws, cx| ws.enqueue_overlay(work, window, cx));
                true
            } else {
                false
            }
        })
    {
        return true;
    }
    false
}

fn open_main_window(cx: &mut App) {
    let focus = std::env::var_os("WXLOCATED_NO_FOCUS").is_none();
    let options = theme::window_options("WxLocated", main_bounds(cx), Some(size(px(920.), px(600.))), 48.);
    let Ok((window, shell)) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::follow_appearance(window, cx);
        cx.new(|cx| AppShell::new(window, cx))
    }) else {
        return;
    };
    cx.set_global(MainWindow(window, shell.clone()));
    #[cfg(all(feature = "snapshot", target_os = "macos"))]
    snapshot::schedule(window, shell, cx);
    #[cfg(not(all(feature = "snapshot", target_os = "macos")))]
    let _ = shell;
    if focus {
        cx.activate(true);
    }
}

/// The main window's last frame when it still lies on a connected display,
/// otherwise a centered default.
fn main_bounds(cx: &App) -> Bounds<Pixels> {
    let saved = Library::global(cx).read(cx).settings.gallery_frame;
    if let Some([x, y, width, height]) = saved
        && width >= 920.
        && height >= 600.
    {
        let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
        let center = bounds.center();
        if cx.displays().iter().any(|display| display.bounds().contains(&center)) {
            return bounds;
        }
    }
    Bounds::centered(None, size(px(1280.), px(820.)), cx)
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
        // Panel widths and the window frame change often; write them once on quit.
        cx.on_app_quit(|cx| {
            Library::global(cx).update(cx, |library, cx| library.save_settings(cx));
            async {}
        })
        .detach();
        open_main_window(cx);
        #[cfg(not(target_os = "macos"))]
        {
            let paths: Vec<_> = std::env::args_os().skip(1).map(std::path::PathBuf::from).collect();
            if !paths.is_empty() {
                native_open::open_paths(&paths, cx);
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        }
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
    cx.on_action(|_: &OpenLocalPackage, cx| native_open::prompt_open(cx));
    cx.on_action(|_: &ShowGallery, cx| show_home_in_shell(cx));
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

    use ui::key_binding as key;
    cx.bind_keys([
        KeyBinding::new(key("cmd-q", "ctrl-q"), Quit, None),
        KeyBinding::new(key("cmd-r", "ctrl-r"), Refresh, None),
        KeyBinding::new(key("cmd-,", "ctrl-,"), OpenSettings, None),
        KeyBinding::new(key("cmd-o", "ctrl-o"), QuickOpen, None),
        KeyBinding::new(key("cmd-k", "ctrl-k"), QuickOpen, None),
        KeyBinding::new(key("alt-cmd-o", "alt-ctrl-o"), OpenLocalPackage, None),
        KeyBinding::new(key("cmd-shift-1", "ctrl-shift-1"), ShowGallery, None),
        KeyBinding::new(key("cmd-w", "ctrl-w"), CloseWindow, None),
        KeyBinding::new(key("cmd-m", "ctrl-m"), Minimize, None),
        KeyBinding::new(key("cmd-e", "ctrl-e"), ExportMaterial, Some("Workspace")),
        KeyBinding::new(key("cmd-0", "ctrl-0"), ShowOverview, Some("Workspace")),
        KeyBinding::new(key("alt-cmd-r", "alt-ctrl-r"), ToggleRaw, Some("Workspace")),
        KeyBinding::new(key("alt-cmd-0", "alt-ctrl-0"), ToggleInspector, Some("Workspace")),
        KeyBinding::new(key("cmd-[", "ctrl-["), BackToGallery, Some("Workspace")),
        KeyBinding::new(key("cmd-shift-o", "ctrl-shift-o"), FocusFilter, Some("Workspace")),
        KeyBinding::new(key("cmd-shift-f", "ctrl-shift-f"), FocusGlobalSearch, Some("Workspace")),
        KeyBinding::new(key("cmd-f", "ctrl-f"), FocusSearch, Some("Gallery")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new("cmd-h", HideApp, None)]);

    cx.set_menus(vec![
        Menu {
            name: "WxLocated".into(),
            disabled: false,
            items: vec![
                MenuItem::action("关于 WxLocated", About),
                MenuItem::separator(),
                MenuItem::action("设置…", OpenSettings),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action("隐藏 WxLocated", HideApp),
                MenuItem::action("退出 WxLocated", Quit),
            ],
        },
        Menu {
            name: "文件".into(),
            disabled: false,
            items: vec![
                MenuItem::action("打开小程序…", QuickOpen),
                MenuItem::action("打开本地包或重建层…", OpenLocalPackage),
                MenuItem::action("微信实时缓存", ShowGallery),
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
                MenuItem::action("全局搜索源码…", FocusGlobalSearch),
                MenuItem::action("筛选页面 / 文件", FocusFilter),
            ],
        },
        Menu {
            name: "显示".into(),
            disabled: false,
            items: vec![
                MenuItem::action("重新扫描", Refresh),
                MenuItem::separator(),
                MenuItem::action("分包监控", ShowOverview),
                MenuItem::action("切换原始 / 还原源码", ToggleRaw),
                MenuItem::action("Agent 交付与证据", ToggleInspector),
                MenuItem::action("重新分析当前包", Rebuild),
                MenuItem::separator(),
                MenuItem::action("返回微信实时缓存", BackToGallery),
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
