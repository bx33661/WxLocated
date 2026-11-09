//! Settings: launch and appearance, scan coverage and extra cache folders,
//! the `wxlocated` command and export defaults for agents, what is written
//! where, and about.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::Anchor;
use serde_json::Value;

use crate::library::Library;
use crate::theme::{Palette, palette};
use crate::ui::{self, badge, display_path, mascot};

pub const AGENT_SNIPPET: &str = "## 小程序审计材料（WxLocated）
- `wxlocated list` 列出本机缓存的小程序（JSON）
- `wxlocated info <appid>` 查看材料完整度（material-check schema）
- `wxlocated export <appid> --out <目录>` 导出 raw/、source/、material.json、AGENT.md
- 导出后先读 AGENT.md 与 material.json；引用证据时以 raw/ 原始包为准，source/ 只是重建的阅读视图
- completeness 为 PARTIAL 时，按 next_actions 补齐分包后重新导出";

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    General,
    Scan,
    Agent,
    Privacy,
    About,
}

const TABS: &[(Tab, &str, IconName)] = &[
    (Tab::General, "通用", IconName::Settings),
    (Tab::Scan, "扫描", IconName::Search),
    (Tab::Agent, "Agent", IconName::SquareTerminal),
    (Tab::Privacy, "隐私", IconName::Shield),
    (Tab::About, "关于", IconName::Info),
];

pub struct SettingsView {
    library: Entity<Library>,
    tab: Tab,
    doctor: Option<Result<Value, String>>,
    checking: bool,
    copied: Option<&'static str>,
    cli_error: Option<String>,
    _subscription: Subscription,
}

struct SettingsWindow(AnyWindowHandle);

impl Global for SettingsWindow {}

pub fn open_settings(tab: Option<Tab>, cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsWindow>().map(|w| w.0)
        && handle
            .update(cx, |root, window, cx| {
                window.activate_window();
                if let (Some(tab), Ok(root)) = (tab, root.downcast::<gpui_kit::base::Root>())
                    && let Ok(view) = root.read(cx).view().clone().downcast::<SettingsView>()
                {
                    view.update(cx, |view, cx| {
                        view.tab = tab;
                        cx.notify();
                    });
                }
            })
            .is_ok()
    {
        return;
    }
    let options = crate::theme::window_options("设置", Bounds::centered(None, size(px(860.), px(640.)), cx), Some(size(px(640.), px(460.))), 44.);
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        crate::theme::follow_appearance(window, cx);
        cx.new(|cx| SettingsView::new(tab.unwrap_or(Tab::General), cx))
    }) {
        cx.set_global(SettingsWindow(handle));
    }
}

#[cfg(feature = "snapshot")]
pub fn snapshot_tab(handle: AnyWindowHandle, tab: Tab, cx: &mut App) {
    if let Some(root) = handle.downcast::<gpui_kit::base::Root>() {
        root.update(cx, |root, _, cx| {
            if let Ok(view) = root.view().clone().downcast::<SettingsView>() {
                view.update(cx, |view, cx| {
                    view.tab = tab;
                    cx.notify();
                });
            }
        })
        .ok();
    }
}

#[cfg(feature = "snapshot")]
pub fn window(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<SettingsWindow>().map(|w| w.0)
}

/// Asks for an extra cache folder, saves it and rescans.
pub fn add_folder(cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("添加".into()) });
    let library = Library::global(cx);
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = paths.await else { return };
        library.update(cx, |library, cx| {
            for path in paths {
                let path = path.to_string_lossy().into_owned();
                if !library.settings.custom_scan_paths.contains(&path) {
                    library.settings.custom_scan_paths.push(path);
                }
            }
            library.save_settings(cx);
            library.refresh(cx);
        });
    })
    .detach();
}

/// The `wxlocated` CLI shipped with the app: `Contents/Helpers/wxlocated` in
/// the macOS bundle (`Contents/MacOS` already holds `WxLocated`, the same name
/// on a case-insensitive volume), next to the executable elsewhere and in dev.
pub fn cli_binary() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let name = if cfg!(windows) { "wxlocated.exe" } else { "wxlocated" };
    let own = std::fs::canonicalize(&executable).ok();
    [directory.join("../Helpers").join(name), directory.join(name)]
        .into_iter()
        .find(|candidate| candidate.is_file() && std::fs::canonicalize(candidate).ok() != own)
        .map(|candidate| std::fs::canonicalize(&candidate).unwrap_or(candidate))
}

fn shim_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/wxlocated")
}

#[derive(PartialEq)]
enum CliStatus {
    NotInstalled,
    Installed,
    Elsewhere,
}

fn cli_status() -> CliStatus {
    match (std::fs::read_to_string(shim_path()), cli_binary()) {
        (Err(_), _) => CliStatus::NotInstalled,
        (Ok(contents), Some(binary)) if contents.contains(&*binary.to_string_lossy()) => CliStatus::Installed,
        _ => CliStatus::Elsewhere,
    }
}

/// A shim in ~/.local/bin forwarding to the bundled CLI; no admin rights.
fn install_cli() -> std::io::Result<()> {
    let binary = cli_binary().ok_or_else(|| std::io::Error::other("找不到随 App 附带的 wxlocated"))?;
    let shim = shim_path();
    std::fs::create_dir_all(shim.parent().unwrap_or(&shim))?;
    std::fs::write(&shim, format!("#!/bin/sh\n# Installed by WxLocated. Forwards to the bundled CLI.\nexec \"{}\" \"$@\"\n", binary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

impl SettingsView {
    fn new(tab: Tab, cx: &mut Context<Self>) -> Self {
        let library = Library::global(cx);
        let subscription = cx.observe(&library, |_, _, cx| cx.notify());
        Self { library, tab, doctor: None, checking: false, copied: None, cli_error: None, _subscription: subscription }
    }

    fn update_settings(&self, cx: &mut App, edit: impl FnOnce(&mut wxlocated_engine::settings::Settings)) {
        self.library.update(cx, |library, cx| {
            edit(&mut library.settings);
            library.save_settings(cx);
            library.update_monitoring(cx);
        });
    }

    fn mark_copied(&mut self, key: &'static str, cx: &mut Context<Self>) {
        self.copied = Some(key);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
            this.update(cx, |this, cx| {
                if this.copied == Some(key) {
                    this.copied = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn run_doctor(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = cli_binary() else {
            self.doctor = Some(Err("没有找到随 App 附带的 wxlocated".into()));
            cx.notify();
            return;
        };
        self.checking = true;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let output = std::process::Command::new(binary).arg("doctor").output().map_err(|e| e.to_string())?;
            serde_json::from_slice::<Value>(&output.stdout).map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.doctor = Some(result);
                this.checking = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_export_directory(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("选择".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| this.update_settings(cx, |s| s.export_directory = Some(path.to_string_lossy().into_owned()))).ok();
        })
        .detach();
    }

    // MARK: Pages

    fn render_general(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let palette = palette(cx);
        let settings = self.library.read(cx).settings.clone();
        let appearance = settings.appearance.clone();
        let sort = settings.sort_order.clone();
        let library = self.library.clone();
        let appearance_label = match appearance.as_str() {
            "light" => "浅色",
            "dark" => "深色",
            _ => "跟随系统",
        };
        let sort_label = match sort.as_str() {
            "name" => "名称",
            "size" => "文件大小",
            _ => "最近修改",
        };
        vec![
            group(
                "启动",
                vec![
                    row(
                        "启动时自动扫描",
                        None,
                        self.switch("auto-scan", settings.auto_scan_on_launch, |s, v| s.auto_scan_on_launch = v, cx),
                        &palette,
                    ),
                    row(
                        "自动监测缓存变化",
                        Some("使用系统文件事件，在微信下载新分包或更新版本后自动重扫。"),
                        self.switch("monitor", settings.monitor_cache_changes, |s, v| s.monitor_cache_changes = v, cx),
                        &palette,
                    ),
                ],
                &palette,
            ),
            group(
                "外观",
                vec![
                    row(
                        "主题",
                        None,
                        Button::new("appearance")
                            .label(appearance_label)
                            .small()
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                let item = |label: &'static str, key: &'static str| {
                                    let library = library.clone();
                                    PopupMenuItem::new(label).checked(appearance == key).on_click(move |_, _, cx| {
                                        library.update(cx, |library, cx| {
                                            library.settings.appearance = key.into();
                                            library.save_settings(cx);
                                        });
                                        crate::theme::apply(cx);
                                    })
                                };
                                menu.item(item("跟随系统", "system")).item(item("浅色", "light")).item(item("深色", "dark"))
                            })
                            .into_any_element(),
                        &palette,
                    ),
                    row(
                        "画廊排序",
                        None,
                        Button::new("sort-order")
                            .label(sort_label)
                            .small()
                            .dropdown_menu_with_anchor(Anchor::TopRight, {
                                let library = self.library.clone();
                                move |menu, _, _| {
                                    let item = |label: &'static str, key: &'static str| {
                                        let library = library.clone();
                                        PopupMenuItem::new(label).checked(sort == key).on_click(move |_, _, cx| {
                                            library.update(cx, |library, cx| {
                                                library.settings.sort_order = key.into();
                                                library.save_settings(cx);
                                            })
                                        })
                                    };
                                    menu.item(item("最近修改", "modified")).item(item("名称", "name")).item(item("文件大小", "size"))
                                }
                            })
                            .into_any_element(),
                        &palette,
                    ),
                ],
                &palette,
            ),
        ]
    }

    fn render_scan(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let palette = palette(cx);
        let library = self.library.read(cx);
        let scanning = library.scanning;
        let snapshot = library.snapshot.clone();
        let custom = library.settings.custom_scan_paths.clone();
        let last_scan = library.last_scan;
        let gap = library.has_coverage_gap();
        let apps = snapshot.as_ref().map(|s| s.miniapps.len()).unwrap_or(0);
        let (title, description, color, icon) = match &snapshot {
            _ if scanning => ("正在重新核验目录", "正在遍历缓存并重新计算包、版本、插件与元数据指标。".to_string(), palette.accent, IconName::RefreshCw),
            None => ("等待首次扫描", "开始扫描后会记录目录状态和每类文件的处理结果。".to_string(), palette.secondary, IconName::Clock),
            Some(s) if s.readable_roots.is_empty() => {
                ("尚未发现可读取的缓存目录", "请选择包含 packages 与 icon 的 Applet 缓存目录后重新扫描。".to_string(), palette.warning, IconName::TriangleAlert)
            }
            Some(s) if gap => (
                "当前扫描范围存在缺口",
                format!("已读取 {} 个目录；{} 个目录权限受限，遍历错误 {} 次。", s.readable_roots.len(), s.inaccessible_roots.len(), s.enumeration_errors),
                palette.warning,
                IconName::TriangleAlert,
            ),
            Some(s) => (
                "可访问目录中的落盘文件已完整枚举",
                format!("已读取 {} 个目录，遍历错误为 0；插件包和旧版本包均已单独统计。", s.readable_roots.len()),
                palette.success,
                IconName::CircleCheck,
            ),
        };
        let mut pages = vec![
            h_flex()
                .gap_3()
                .p(px(14.))
                .rounded(px(12.))
                .bg(color.opacity(0.08))
                .border_1()
                .border_color(color.opacity(0.22))
                .child(div().text_size(px(22.)).text_color(color).child(icon))
                .child(
                    v_flex()
                        .flex_1()
                        .gap_0p5()
                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(11.5)).text_color(palette.secondary).child(description))
                        .when_some(last_scan, |this, time| {
                            this.child(div().text_size(px(11.)).text_color(palette.tertiary).child(format!("生成于 {}", ui::format_time(time))))
                        }),
                )
                .child(
                    Button::new("rescan")
                        .icon(IconName::RefreshCw)
                        .label("重新扫描")
                        .small()
                        .loading(scanning)
                        .on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |library, cx| library.refresh(cx)))),
                )
                .into_any_element(),
        ];
        if let Some(s) = &snapshot {
            let d = &s.diagnostics;
            let percent = |value: usize| if apps == 0 { "暂无应用".to_string() } else { format!("{:.0}%", value as f64 * 100. / apps as f64) };
            let duration = if d.duration.as_secs_f64() < 1. { format!("{} ms", d.duration.as_millis()) } else { format!("{:.1} s", d.duration.as_secs_f64()) };
            let unidentified = d.total_package_files - d.identified_package_files;
            let metrics = [
                ("原始包", d.total_package_files.to_string(), ui::format_size(d.total_package_bytes), IconName::Package),
                (
                    "已识别包",
                    d.identified_package_files.to_string(),
                    if unidentified == 0 { "AppID 全部识别".into() } else { format!("{unidentified} 个待识别") },
                    IconName::BadgeCheck,
                ),
                ("展示应用", apps.to_string(), format!("{} 个当前版本包", d.selected_package_files), IconName::LayoutGrid),
                ("插件包", d.plugin_package_files.to_string(), format!("{} 组未混入应用", d.plugin_only_app_count), IconName::Puzzle),
                ("旧版本包", d.old_version_package_files.to_string(), "保留在磁盘，列表去重".into(), IconName::Archive),
                ("自动名称", format!("{}/{apps}", d.resolved_name_count), percent(d.resolved_name_count), IconName::Type),
                ("Logo 命中", format!("{}/{apps}", d.resolved_icon_count), percent(d.resolved_icon_count), IconName::Image),
                ("扫描耗时", duration, "原生索引，无运行时依赖".into(), IconName::Timer),
            ];
            pages.push(
                v_flex()
                    .gap_2()
                    .child(ui::section_label("本次结果", None, &palette))
                    .child(h_flex().flex_wrap().gap_2().children(metrics.into_iter().map(|(label, value, detail, icon)| {
                        v_flex()
                            .w(px(140.))
                            .gap_1()
                            .p(px(11.))
                            .rounded(px(10.))
                            .bg(palette.fill)
                            .child(h_flex().gap_1p5().text_size(px(11.)).text_color(palette.secondary).child(div().text_size(px(11.)).child(icon)).child(label))
                            .child(div().text_size(px(21.)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(value))
                            .child(div().truncate().text_size(px(10.5)).text_color(palette.tertiary).child(detail))
                    })))
                    .into_any_element(),
            );
            let roots = s
                .readable_roots
                .iter()
                .map(|r| (r, "已扫描", palette.success, IconName::CircleCheck))
                .chain(s.inaccessible_roots.iter().map(|r| (r, "权限受限", palette.warning, IconName::CircleAlert)))
                .chain(s.missing_roots.iter().map(|r| (r, "未检测到", palette.tertiary, IconName::CircleMinus)));
            pages.push(
                v_flex()
                    .gap_2()
                    .child(ui::section_label("目录明细", Some(format!("可读 {} · 受限 {}", s.readable_roots.len(), s.inaccessible_roots.len())), &palette))
                    .child(ui::card(&palette).px_3().py_1().children(roots.map(|(root, label, color, icon)| {
                        h_flex()
                            .gap_2()
                            .py(px(7.))
                            .child(div().text_size(px(13.)).text_color(color).child(icon))
                            .child(div().w(px(54.)).flex_none().text_size(px(11.)).text_color(color).child(label))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.secondary).child(display_path(&root.to_string_lossy())))
                    })))
                    .into_any_element(),
            );
        }
        pages.push(
            group(
                "附加扫描目录",
                custom
                    .iter()
                    .map(|path| {
                        let path_for_remove = path.clone();
                        h_flex()
                            .gap_2()
                            .px(px(14.))
                            .py(px(9.))
                            .child(div().text_color(palette.secondary).child(IconName::Folder))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).font_family(ui::mono_font()).child(display_path(path)))
                            .child(Button::new(SharedString::from(format!("remove-{path}"))).icon(IconName::Minus).ghost().xsmall().tooltip("移除此目录").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    let path = path_for_remove.clone();
                                    this.library.update(cx, |library, cx| {
                                        library.settings.custom_scan_paths.retain(|p| *p != path);
                                        library.save_settings(cx);
                                        library.refresh(cx);
                                    })
                                }),
                            ))
                            .into_any_element()
                    })
                    .chain(std::iter::once(
                        h_flex()
                            .gap_2()
                            .px(px(14.))
                            .py(px(9.))
                            .when(custom.is_empty(), |this| {
                                this.child(div().flex_1().text_size(px(12.)).text_color(palette.secondary).child("缓存位置特殊时在这里添加 Applet 目录，添加后立即重扫。"))
                            })
                            .when(!custom.is_empty(), |this| this.child(div().flex_1()))
                            .child(Button::new("add-folder").icon(IconName::Plus).label("添加目录…").small().on_click(|_, _, cx| add_folder(cx)))
                            .into_any_element(),
                    ))
                    .collect(),
                &palette,
            ),
        );
        pages.push(
            h_flex()
                .items_start()
                .gap_2()
                .text_size(px(11.))
                .text_color(palette.tertiary)
                .child(div().child(IconName::Info))
                .child(div().flex_1().child("“已完整枚举”指当前可访问目录中已经落盘的文件。尚未触发下载的懒加载分包、已被系统清理的缓存，以及未授予访问权限的目录不会出现在包总数中。"))
                .into_any_element(),
        );
        pages
    }

    fn render_agent(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let palette = palette(cx);
        let status = cli_status();
        let binary = cli_binary();
        let settings = self.library.read(cx).settings.clone();
        let snippet_copied = self.copied == Some("snippet");
        let status_badge = match status {
            CliStatus::Installed => badge("已安装", palette.success),
            CliStatus::Elsewhere => badge("指向其他位置", palette.warning),
            CliStatus::NotInstalled => badge("未安装", palette.secondary),
        };
        let mut cli_rows = vec![
            row(
                "wxlocated",
                Some("安装到 ~/.local/bin，无需管理员权限。业务命令输出 JSON，帮助使用 wxlocated --help；字段约定见项目 README。"),
                h_flex()
                    .gap_2()
                    .child(status_badge)
                    .child(
                        Button::new("install-cli")
                            .label(if status == CliStatus::Installed { "重新安装" } else { "安装" })
                            .small()
                            .when(status != CliStatus::Installed, |b| b.primary())
                            .disabled(binary.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cli_error = install_cli().err().map(|e| format!("安装失败：{e}"));
                                cx.notify();
                            })),
                    )
                    .when(status != CliStatus::NotInstalled, |this| {
                        this.child(Button::new("uninstall-cli").label("卸载").small().ghost().on_click(cx.listener(|_, _, _, cx| {
                            let _ = std::fs::remove_file(shim_path());
                            cx.notify();
                        })))
                    })
                    .into_any_element(),
                &palette,
            ),
            row("位置", None, div().text_size(px(11.5)).font_family(ui::mono_font()).text_color(palette.secondary).child("~/.local/bin/wxlocated").into_any_element(), &palette),
        ];
        if let Some(error) = &self.cli_error {
            cli_rows.push(div().px(px(14.)).py_2().text_size(px(12.)).text_color(palette.error).child(error.clone()).into_any_element());
        }
        if binary.is_none() {
            cli_rows.push(
                div().px(px(14.)).py_2().text_size(px(12.)).text_color(palette.warning).child("没有找到随 App 附带的 wxlocated（开发构建请先 cargo build -p wxlocated-cli）。").into_any_element(),
            );
        }

        let export_dir = settings.export_directory.clone();
        let doctor = self.render_doctor(&palette);
        vec![
            group("命令行工具", cli_rows, &palette),
            v_flex()
                .gap_2()
                .child(ui::section_label("给 Agent 的说明", Some("放进 CLAUDE.md、AGENTS.md 或 Skill".into()), &palette))
                .child(
                    v_flex()
                        .rounded(px(12.))
                        .bg(palette.code)
                        .border_1()
                        .border_color(palette.hairline)
                        .child(div().p(px(14.)).text_size(px(11.5)).line_height(px(19.)).font_family(ui::mono_font()).text_color(palette.text).child(AGENT_SNIPPET))
                        .child(
                            h_flex().px(px(10.)).py(px(8.)).border_t_1().border_color(palette.hairline).child(div().flex_1()).child(
                                Button::new("copy-snippet")
                                    .icon(if snippet_copied { IconName::Check } else { IconName::Copy })
                                    .label(if snippet_copied { "已复制" } else { "复制" })
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        ui::copy(AGENT_SNIPPET, cx);
                                        this.mark_copied("snippet", cx);
                                    })),
                            ),
                        ),
                )
                .into_any_element(),
            group(
                "导出审计材料（App 内）",
                vec![
                    row(
                        "默认位置",
                        None,
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .max_w(px(220.))
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(if export_dir.is_some() { palette.text } else { palette.secondary })
                                    .child(export_dir.as_deref().map(display_path).unwrap_or_else(|| "每次询问".into())),
                            )
                            .child(Button::new("choose-export").label("选择…").small().on_click(cx.listener(|this, _, _, cx| this.choose_export_directory(cx))))
                            .when(export_dir.is_some(), |this| {
                                this.child(Button::new("clear-export").label("清除").small().ghost().on_click(cx.listener(|this, _, _, cx| {
                                    this.update_settings(cx, |s| s.export_directory = None)
                                })))
                            })
                            .into_any_element(),
                        &palette,
                    ),
                    row("包含原始包（raw/）", None, self.switch("include-raw", settings.export_include_raw, |s, v| s.export_include_raw = v, cx), &palette),
                    row("包含重建源码（source/）", None, self.switch("include-source", settings.export_include_source, |s, v| s.export_include_source = v, cx), &palette),
                    row(
                        "脱敏",
                        Some("清单中的家目录替换为 ~，账号目录替换为 <account>，便于把材料交给他人或云端 Agent。命令行对应 --redact。"),
                        self.switch("redact", settings.redact_exports, |s, v| s.redact_exports = v, cx),
                        &palette,
                    ),
                ],
                &palette,
            ),
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .child(ui::section_label("环境检查", Some("与 wxlocated doctor 相同".into()), &palette).flex_1())
                        .child(div().w_2())
                        .child(Button::new("doctor").icon(IconName::Stethoscope).label(if self.checking { "检查中…" } else { "运行检查" }).small().loading(self.checking).on_click(
                            cx.listener(|this, _, _, cx| this.run_doctor(cx)),
                        )),
                )
                .child(doctor)
                .into_any_element(),
        ]
    }

    fn render_doctor(&self, palette: &Palette) -> AnyElement {
        match &self.doctor {
            None => ui::card(palette)
                .p(px(14.))
                .text_size(px(12.))
                .text_color(palette.secondary)
                .child("检查磁盘访问权限、缓存目录、微信版本与命令行安装。")
                .into_any_element(),
            Some(Err(message)) => ui::card(palette).p(px(14.)).text_size(px(12.)).text_color(palette.error).child(message.clone()).into_any_element(),
            Some(Ok(report)) => {
                let checks = report["checks"].as_array().cloned().unwrap_or_default();
                ui::card(palette)
                    .px(px(14.))
                    .py_1()
                    .children(checks.into_iter().map(|check| {
                        let status = check["status"].as_str().unwrap_or_default();
                        let (color, icon) = match status {
                            "ok" => (palette.success, IconName::CircleCheck),
                            "warn" => (palette.warning, IconName::CircleAlert),
                            _ => (palette.error, IconName::CircleX),
                        };
                        h_flex()
                            .items_start()
                            .gap_2()
                            .py(px(8.))
                            .child(div().text_size(px(14.)).text_color(color).child(icon))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(div().text_size(px(12.)).text_color(palette.text).child(check["message"].as_str().unwrap_or_default().to_string()))
                                    .when_some(check["fix"].as_str().map(str::to_string), |this, fix| {
                                        this.child(div().text_size(px(11.)).text_color(palette.secondary).child(fix))
                                    }),
                            )
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_privacy(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let palette = palette(cx);
        let overlays = wxlocated_engine::overlay::root();
        let text = |value: &'static str| div().text_size(px(12.)).text_color(palette.secondary).child(value).into_any_element();
        let mono = |value: String| div().max_w(px(320.)).truncate().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.secondary).child(value).into_any_element();
        vec![
            group(
                "本机处理",
                vec![
                    row("网络", None, text("不主动发送网络请求，没有遥测、统计或崩溃上传"), &palette),
                    row("代码", None, text("包内 JavaScript 只作为文本处理，从不执行"), &palette),
                    row("缓存", None, text("微信缓存始终只读"), &palette),
                ],
                &palette,
            ),
            group(
                "会写入的位置",
                vec![
                    row("导出", None, text("只写入你选择或 --out 指定的目录"), &palette),
                    row("Agent 重建层", None, mono(display_path(&overlays.to_string_lossy())), &palette),
                    row("命令行工具", None, mono("~/.local/bin/wxlocated（仅在你点击安装时）".into()), &palette),
                    row(
                        "偏好设置",
                        None,
                        mono(wxlocated_engine::settings::settings_path().map(|p| display_path(&p.to_string_lossy())).unwrap_or_default()),
                        &palette,
                    ),
                ],
                &palette,
            ),
            h_flex()
                .gap_2()
                .child(Button::new("reveal-overlays").icon(IconName::FolderOpen).label("在 Finder 中显示重建层目录").small().on_click(move |_, _, cx| {
                    let _ = std::fs::create_dir_all(&overlays);
                    cx.reveal_path(&overlays)
                }))
                .into_any_element(),
            div()
                .text_size(px(11.))
                .text_color(palette.tertiary)
                .child("导出材料默认不脱敏；交给他人前可在 Agent 页开启脱敏，或在命令行加 --redact。")
                .into_any_element(),
        ]
    }

    fn render_about(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let palette = palette(cx);
        let link = |id: &'static str, label: &'static str, url: &'static str| {
            Button::new(id).label(label).small().ghost().on_click(move |_, _, cx| cx.open_url(url))
        };
        let components = [
            ("GPUI", "界面框架", "Apache-2.0", "https://github.com/zed-industries/zed"),
            ("gpui-component", "组件库", "Apache-2.0", "https://github.com/longbridge/gpui-component"),
            ("tree-sitter", "语法高亮", "MIT", "https://github.com/tree-sitter/tree-sitter"),
            ("Lucide", "图标", "ISC", "https://lucide.dev"),
            ("similar", "差异对比", "Apache-2.0", "https://github.com/mitsuhiko/similar"),
            ("notify", "缓存监测", "CC0 / Artistic-2.0", "https://github.com/notify-rs/notify"),
            ("aes · pbkdf2 · sha2", "解密与校验", "MIT / Apache-2.0", "https://github.com/RustCrypto"),
        ];
        vec![
            h_flex()
                .gap_5()
                .p(px(20.))
                .rounded(px(14.))
                .bg(linear_gradient(
                    135.,
                    linear_color_stop(palette.success.opacity(0.12), 0.),
                    linear_color_stop(palette.accent.opacity(0.08), 1.),
                ))
                .border_1()
                .border_color(palette.hairline)
                .child(mascot(84.))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_size(px(24.)).font_weight(FontWeight::BOLD).text_color(palette.text).child("WxLocated"))
                        .child(div().text_size(px(13.)).text_color(palette.secondary).child("面向 Agent 的微信小程序审计材料基座"))
                        .child(div().text_size(px(11.)).font_family(ui::mono_font()).text_color(palette.tertiary).child(format!(
                            "版本 {} · {} {}",
                            env!("CARGO_PKG_VERSION"),
                            std::env::consts::OS,
                            std::env::consts::ARCH
                        )))
                        .child(
                            h_flex()
                                .mt_1()
                                .gap_1()
                                .ml(px(-8.))
                                .child(link("gh", "GitHub", "https://github.com/bx33661/WxLocated"))
                                .child(link("rel", "发布与下载", "https://github.com/bx33661/WxLocated/releases"))
                                .child(link("contract", "Agent 接口契约", "https://github.com/bx33661/WxLocated#cli")),
                        ),
                )
                .into_any_element(),
            group(
                "推荐搭配",
                vec![row(
                    "MiniApp Audit Skills",
                    Some("基于 Agent 的小程序安全审计技能集。WxLocated 负责材料，Skills 负责威胁建模、静态审计、动态验证与报告。"),
                    link("skills", "打开", "https://github.com/bx33661/miniapp-audit-skills").into_any_element(),
                    &palette,
                )],
                &palette,
            ),
            group(
                "开源组件",
                components
                    .into_iter()
                    .map(|(name, usage, license, url)| {
                        h_flex()
                            .id(name)
                            .gap_2()
                            .px(px(14.))
                            .py(px(8.))
                            .cursor_pointer()
                            .hover(|this| this.bg(palette.fill))
                            .on_click(move |_, _, cx| cx.open_url(url))
                            .child(div().text_size(px(12.5)).text_color(palette.accent).child(name))
                            .child(div().text_size(px(11.5)).text_color(palette.secondary).child(usage))
                            .child(div().flex_1())
                            .child(div().text_size(px(11.)).text_color(palette.tertiary).child(license))
                            .into_any_element()
                    })
                    .collect(),
                &palette,
            ),
        ]
    }

    fn switch(
        &self,
        id: &'static str,
        checked: bool,
        set: impl Fn(&mut wxlocated_engine::settings::Settings, bool) + 'static,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let library = self.library.clone();
        Switch::new(id)
            .checked(checked)
            .on_click(move |checked: &bool, _, cx| {
                let checked = *checked;
                library.update(cx, |library, cx| {
                    set(&mut library.settings, checked);
                    library.save_settings(cx);
                    library.update_monitoring(cx);
                });
            })
            .into_any_element()
    }
}

/// A titled card of rows separated by hairlines, like a grouped form.
fn group(title: &'static str, rows: Vec<AnyElement>, palette: &Palette) -> AnyElement {
    let count = rows.len();
    v_flex()
        .gap_2()
        .child(ui::section_label(title, None, palette))
        .child(
            v_flex().rounded(px(12.)).bg(palette.raised).border_1().border_color(palette.hairline).overflow_hidden().children(
                rows.into_iter().enumerate().map(|(index, row)| div().when(index + 1 < count, |this| this.border_b_1().border_color(palette.hairline)).child(row)),
            ),
        )
        .into_any_element()
}

/// Label (with an optional explanation) on the left, a control on the right.
fn row(title: &'static str, description: Option<&'static str>, control: AnyElement, palette: &Palette) -> AnyElement {
    h_flex()
        .gap_4()
        .px(px(14.))
        .py(px(10.))
        .min_h(px(44.))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_size(px(13.)).text_color(palette.text).child(title))
                .when_some(description, |this, text| this.child(div().text_size(px(11.)).text_color(palette.secondary).child(text))),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let current = self.tab;
        let title = TABS.iter().find(|(tab, _, _)| *tab == current).map(|(_, label, _)| *label).unwrap_or_default();
        let content = match self.tab {
            Tab::General => self.render_general(cx),
            Tab::Scan => self.render_scan(cx),
            Tab::Agent => self.render_agent(cx),
            Tab::Privacy => self.render_privacy(cx),
            Tab::About => self.render_about(cx),
        };
        let body = h_flex()
            .flex_1()
            .min_h_0()
            .text_color(palette.text)
            .child(
                v_flex()
                    .w(px(190.))
                    .h_full()
                    .flex_none()
                    .p_2()
                    .pt_3()
                    .gap_0p5()
                    .bg(palette.sidebar)
                    .border_r_1()
                    .border_color(palette.hairline)
                    .children(TABS.iter().map(|&(tab, label, icon)| {
                        let selected = tab == current;
                        h_flex()
                            .id(label)
                            .gap_2p5()
                            .px_2p5()
                            .py(px(6.))
                            .rounded(px(7.))
                            .text_size(px(13.))
                            .when(selected, |this| this.bg(palette.accent).text_color(palette.on_accent))
                            .when(!selected, |this| this.text_color(palette.text).hover(|this| this.bg(palette.fill)))
                            .child(
                                div()
                                    .size(px(22.))
                                    .rounded(px(6.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(13.))
                                    .when(selected, |this| this.bg(white().opacity(0.2)))
                                    .when(!selected, |this| this.bg(palette.fill_strong).text_color(palette.secondary))
                                    .child(icon),
                            )
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.tab = tab;
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div().id("settings-scroll").flex_1().min_w_0().h_full().bg(palette.window).overflow_y_scroll().child(
                    v_flex()
                        .w_full()
                        .max_w(px(660.))
                        .px(px(28.))
                        .pt(px(22.))
                        .pb(px(32.))
                        .gap(px(22.))
                        .child(div().text_size(px(22.)).font_weight(FontWeight::BOLD).text_color(palette.ink).child(title))
                        .children(content),
                ),
            );
        v_flex()
            .size_full()
            .child(ui::toolbar(44., &palette).child(
                h_flex().w_full().justify_center().pr(px(crate::theme::TRAFFIC_LIGHTS - 12.)).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(palette.secondary).child(format!("设置 · {title}")),
            ))
            .children(ui::settings_notice(cx))
            .child(body)
    }
}
