<img src="design/brand/cute/app-icon-1024.png" alt="WxLocated 图标" width="88" align="right" />

# WxLocated

基于 Rust + GPUI 的微信小程序本地工作台。定位缓存、解包解密、静态重建源码，并导出可追溯的审计材料。提供桌面端和面向 Agent 的 JSON CLI。

<a id="quick-start"></a>

## 下载与使用

| 系统 | 选择 |
| --- | --- |
| Windows 10/11 · Intel / AMD | x64 便携 ZIP |
| Windows 11 · ARM | ARM64 便携 ZIP |
| macOS 14+ · Apple Silicon / Intel | Universal DMG / PKG / ZIP |

[下载最新版](https://github.com/bx33661/WxLocated/releases/latest)

Windows 解压后运行 `wxlocated-app.exe`，需要对应架构的 [VC++ 运行库](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)。macOS 打开 DMG 将应用拖到“应用程序”（或使用 PKG / ZIP），应用采用 ad-hoc 签名、尚未公证；Windows EXE 未做 Authenticode 签名。所有发布包均附带 `SHA256SUMS.txt` 校验文件。

macOS 首次启动如提示无法验证或已损坏，可在“系统设置 › 隐私与安全性”点击“仍要打开”，或执行 `xattr -dr com.apple.quarantine /Applications/WxLocated.app`。读取微信缓存需在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许 WxLocated；因使用 ad-hoc 签名，覆盖升级后若无法读取，需先从列表移除旧条目再重新添加并重启应用。

## 核心功能

- **一体式单窗口工作台（Obsidian Studio）**：左侧导航栏快速切换「微信实时缓存」、「已导出审计材料」与最近打开的小程序，支持 `⌘O` / `⌘K`（Windows 为 `Ctrl+O` / `Ctrl+K`）快速跳转，内置浅色与深色主题。
- **微信实时缓存捕获与焦点监控**：支持 macOS 微信 3.x / 4.0 / 4.2 多账号路径与 Windows 3.x / 4.x 缓存路径；首页焦点卡实时追踪当前活跃小程序的主包与分包捕获进度，缺包时提示在微信触发对应页面并支持一键导出复制 Agent 指令；也支持单击列表/卡片切换，或直接拖入/选择本地 `.wxapkg` 文件与重建层目录。
- **三页签源码工作区**：顶部划分 `源码与搜索`、`分包监控` 与 `Agent 交付与证据`；浏览页面时支持 `JS 逻辑 | WXML 结构 | WXSS 样式 | JSON 配置` 四件套一键切换，提供按页面/目录树/`⇧⌘F` 跨文件全文搜索、多文件标签页、前进后退历史、自动换行与底部字节证据栏。
- **Agent 交付与彩色对照**：导出自包含审计材料并记录到「已导出审计材料」中心，一键复制可直接粘贴给 Claude Code / Cursor / Codex 的审计提示词；支持导入外部 Agent 重建层并通过原生语法高亮红绿 Diff 对照差异。未找到缓存时，可在设置中添加自定义扫描目录。

![WxLocated 源码工作区，使用示例小程序展示](design/wxlocated-rust-workspace.png)

<a id="cli"></a>

## 命令行

在“设置 → Agent → 命令行工具”中安装，Windows 安装后重新打开终端。将示例 AppID 和材料目录替换为实际值。

```sh
wxlocated list
wxlocated info wx0123456789abcdef
wxlocated export wx0123456789abcdef --out ../materials --reuse --redact
wxlocated trace ../materials/wx0123456789abcdef-v1 app.js:42
```

业务命令输出 JSON，`watch` 输出 JSON Lines。可用 `--scan-path <目录>` 补充扫描路径，完整命令见 `wxlocated --help`。

<a id="materials"></a>

## 导出材料

- `raw/` 保存原始包，引用证据时以此为准。
- `source/` 保存静态重建源码与来源清单。
- `material.json` 记录完整度、包指纹与重建诊断，`AGENT.md` 说明如何使用材料。
- 已找到的插件和已导入的 Agent 重建层分别放在 `plugins/` 和 `semantic/`。

<a id="build"></a>
<a id="windows"></a>

## 源码构建

安装 [rustup](https://rustup.rs/)。macOS 需要 Xcode Command Line Tools，Windows 需要 MSVC C++ 构建工具和 Windows SDK，Release 构建还需要 SDK 的 `fxc.exe`。

```sh
cargo run -p wxlocated-app
cargo build --release -p wxlocated-cli
cargo test --workspace --locked
```

macOS 打包用 `./scripts/build-release.sh`。Windows x64 用 `./scripts/bundle-windows.ps1`，ARM64 加 `-Target aarch64-pc-windows-msvc`。 [Windows 验收清单](TODO-windows.md)

<a id="privacy"></a>

## 隐私

缓存只读，解析与重建均在本机完成，应用不执行包内代码。`--redact` 仅脱敏清单中的本机路径，导出源码仍可能含敏感信息。
