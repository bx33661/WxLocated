<img src="design/brand/cute/app-icon-1024.png" alt="WxLocated 图标" width="88" align="right" />

# WxLocated

基于 Rust + GPUI 的微信小程序本地工作台：定位缓存、解包解密、静态重建源码，导出带来源与 SHA-256 的审计材料，支持桌面查看和 Agent JSON CLI。

[![Windows x64 / ARM64](https://img.shields.io/badge/Windows-x64%20%2F%20ARM64-0078D4)](#windows)
[![macOS 14+](https://img.shields.io/badge/macOS-14%2B-111111?logo=apple)](#quick-start)
[![Rust](https://img.shields.io/badge/Rust-2024-B7410E?logo=rust)](Cargo.toml)
[![GPUI](https://img.shields.io/badge/UI-GPUI-38A89D)](crates/wxlocated-app)

[快速开始](#quick-start) · [命令行](#cli) · [导出材料](#materials) · [构建](#build) · [隐私](#privacy)

![WxLocated Rust / GPUI 源码工作区，使用示例小程序展示](design/wxlocated-rust-workspace.png)

## 主要能力

- 扫描微信多账户缓存，读取标准 `wxapkg` 与 `V1MMWX` 本地加密包。
- 合并主包与分包，检查缺失分包和插件。
- 恢复 JavaScript、WXML、WXS、WXSS 与配置，提取 Source Map 内嵌源码。
- 对照重建、原始与 Agent 视图，追溯来源并导出审计材料。

<a id="quick-start"></a>

## 快速开始

从 [GitHub Releases](https://github.com/bx33661/WxLocated/releases/latest) 下载。Windows 10/11 选 x64 ZIP，Windows 11 ARM 选 ARM64 ZIP。macOS 14+ 使用 Universal 包，支持 Apple Silicon 和 Intel。

macOS 从源码运行需要 Xcode Command Line Tools 和 [rustup](https://rustup.rs/)。Windows 构建要求见[下文](#windows)。

```bash
git clone https://github.com/bx33661/WxLocated.git
cd WxLocated
cargo run -p wxlocated-app
```

启动后扫描缓存，打开小程序核对材料完整度、浏览源码，再选择“导出 → 导出审计材料”。也可直接拖入 `.wxapkg` 文件或包目录；缺失分包需在微信中访问对应页面触发下载。

<a id="cli"></a>

## 命令行

在“设置 → Agent → 命令行工具”中安装，或直接调用发布目录中的 `wxlocated.exe`（macOS 为应用内的 `Contents/Helpers/wxlocated`）。Windows 安装到当前用户的 `%LOCALAPPDATA%\WxLocated\bin` 并加入用户 PATH，安装后需重新打开终端。以下 AppID 为示例，使用时替换为扫描结果。

```bash
wxlocated list
wxlocated info wx0123456789abcdef
wxlocated export wx0123456789abcdef --out ../materials --reuse --redact
wxlocated trace ../materials/wx0123456789abcdef-v1 app.js:42
wxlocated --help
```

业务命令输出 JSON，`watch` 输出 JSON Lines；日志写入 stderr。可用 `--scan-path <目录>` 为本次命令添加缓存目录，重复指定可扫描多个位置；支持 `~` 和 `%环境变量%`。例如：

```powershell
.\wxlocated.exe list --scan-path 'D:\WeChat Files\Applet'
.\wxlocated.exe doctor --scan-path 'D:\自定义缓存'
```

Agent 可通过 `overlay init` / `overlay import` 创建、修改并导入语义化重建层。

<a id="materials"></a>

## 导出材料

```text
<appid>-v<版本>/
├── material.json       完整度、包指纹与重建诊断
├── AGENT.md            给 Agent 的材料说明
├── raw/                原始包副本
├── source/             静态重建源码与来源清单
├── plugins/<别名>/     已缓存的插件材料
└── semantic/<层名>/    已导入的 Agent 重建层
```

**证据以 `raw/` 为准。** 静态重建不能保证还原原始工程，Agent 重建层属于推断。可将材料交给 [MiniApp Audit Skills](https://github.com/bx33661/miniapp-audit-skills) 继续审计。

<a id="build"></a>

## 构建

```bash
cargo build --release -p wxlocated-cli  # target/release/wxlocated
./scripts/bundle-macos.sh              # target/bundle/WxLocated.app
cargo test --workspace
```

应用包默认包含 Apple Silicon 与 Intel 架构；可用 `ARCHS=aarch64` 或 `ARCHS=x86_64` 只构建单一架构。生成 DMG、PKG、ZIP 与校验文件使用 `./scripts/build-release.sh`。

<a id="windows"></a>

### Windows

源码构建需要：

- [rustup](https://rustup.rs/) 和 MSVC 工具链。x64 使用 `x86_64-pc-windows-msvc`，ARM64 使用 `aarch64-pc-windows-msvc`。Rust 版本由 `rust-toolchain.toml` 固定。
- Visual Studio 2022 Build Tools 的“使用 C++ 的桌面开发”，包括 MSVC 和 Windows 10/11 SDK。
- Release 构建需要 SDK 中的 `fxc.exe` 编译 GPUI 的 DirectX 着色器。通常自动发现，也可通过 `GPUI_FXC_PATH` 指定完整路径。

在 PowerShell 中执行：

```powershell
cargo build -p wxlocated-cli
cargo run -p wxlocated-app
cargo test --workspace
./scripts/bundle-windows.ps1
./scripts/bundle-windows.ps1 -Target aarch64-pc-windows-msvc
```

ZIP 和 SHA-256 校验文件位于 `target/bundle/windows/`，文件名以 `windows-x64` 或 `windows-arm64` 结尾。解压后运行 `wxlocated-app.exe`，保留同目录下的 `wxlocated.exe`。运行需要相应架构的 [VC++ x64](https://aka.ms/vs/17/release/vc_redist.x64.exe) 或 [VC++ ARM64](https://aka.ms/vs/17/release/vc_redist.arm64.exe) 运行库。

包中的 `install-windows.ps1` 可选安装开始菜单快捷方式，并把 WxLocated 加入 `.wxapkg` 的“打开方式”，无需管理员权限，也不修改默认打开应用。移动或删除目录前运行 `./install-windows.ps1 -Uninstall` 清理这些注册。

默认扫描会参考系统 Documents 位置（含重定向）、微信注册表中的存储位置，以及 AppData 下的缓存候选目录，兼容 `WeChat Files` 和 `xwechat_files` 下的 Applet 布局。微信版本、账户和自定义存储位置可能影响目录；未发现时，在设置中添加实际包含 `.wxapkg` 的缓存目录，或使用 CLI 的 `--scan-path`。

Windows 快捷键使用 Ctrl，例如 Ctrl+R 重新扫描、Ctrl+O 打开小程序、Ctrl+E 导出材料。支持拖入包文件或目录，以及通过程序参数打开本地文件。首次适配的实机验证进度见 [Windows 清单](TODO-windows.md)。

<a id="privacy"></a>

## 数据与隐私

解析与重建均在本机完成，原始缓存只读；应用不执行包内代码，也不主动发送网络请求。设置和重建层在 macOS 存放于 `~/Library/Application Support/WxLocated/`，Windows 存放于 `%APPDATA%\WxLocated\`；导出写入用户指定目录。

材料可能含本机信息，建议保存在仓库外。`--redact` 脱敏清单中的本机路径信息，不会清除源码内容。
