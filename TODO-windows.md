# Windows 适配清单

目标：Windows 10/11 x64 和 Windows 11 ARM64 下支持缓存发现、解包重建、CLI、桌面操作和便携发布，同时保留 macOS 支持。

## 已实现

- [x] 建立 Windows MSVC 编译、测试和 Release 构建基线，确认 GPUI 与 Windows SDK 前置条件。
- [x] 统一用户目录、下载目录、环境变量展开、平台 CLI 安装路径和 PATH 判断。
- [x] 增加 Windows 微信缓存候选目录、重定向 Documents 和注册表存储位置发现。
- [x] 按系统路径组件展开通配目录，支持盘符、反斜杠和 UNC 路径结构。
- [x] 用构造缓存验证多账户扫描、AppID、版本隔离、主分包及插件归组。
- [x] 修复 JSON 中 Windows 路径脱敏，覆盖转义反斜杠、正斜杠和扩展路径前缀。
- [x] 识别 Windows 微信账户目录，避免把 Windows 用户目录误认为微信账户。
- [x] 处理 Windows 保留文件名、非法字符、尾随空格/句点，并在源码导出前检查不可写路径。
- [x] 实现 UTF-8 `.cmd` CLI 入口、用户 PATH 注册及环境变更通知；无需管理员权限。
- [x] 适配 doctor、Windows 版本、微信注册表及可执行文件版本检测。
- [x] 适配 Ctrl 快捷键、提示文字、文件管理器操作和默认导出位置。
- [x] 调整 Windows 标题栏留白，沿用 GPUI 的窗口按钮与 DPI manifest。
- [x] 支持桌面程序参数打开文件，关闭最后一个窗口后退出，Release GUI 不显示控制台。
- [x] 将 macOS 专用 snapshot 功能限制到 macOS，Windows 全特性检查可通过。
- [x] 嵌入 Windows 图标与 EXE 版本资源；CLI 使用带长路径配置的 manifest。
- [x] 增加 Windows ZIP + SHA-256 打包脚本。
- [x] 增加可选的开始菜单 / `.wxapkg`“打开方式”注册与卸载脚本，不修改默认应用。
- [x] 增加 Windows / macOS CI，以及发布 CLI 的完整冒烟测试脚本。
- [x] 更新 README：构建依赖、运行、打包、CLI、缓存位置与存储位置。
- [x] 避免 Windows 工具链初始化时自动安装两个 Apple 编译目标；macOS 打包按需安装目标。

## 已执行验证

- Windows x64 / Rust 1.99.1（仓库指定版本）：`cargo check --workspace --all-targets --all-features --locked` 通过；初期基线也在 Rust 1.100 下验证通过。
- Windows x64：51 个引擎测试、11 个桌面端测试通过，包含中文/空格目录、超过 260 字符的扫描路径、账户识别、版本/插件隔离、JSON 脱敏和文件 URL。
- CLI 的 `.cmd` 入口已实际执行测试，覆盖中文、空格、`%` 字符、Rust 规范化路径前缀及参数转发；写入脚本前转换为 cmd.exe 可访问的普通路径。
- `scripts/smoke-cli.py` 在调试版和 Release CLI 上通过：扫描 → info → 重建导出 → 脱敏 → 反斜杠路径 trace → reuse → overlay init。
- 调试版与发布包内的 Release 桌面程序均已启动，并通过参数打开中文路径测试包；关闭所有窗口后正常退出。验证时禁用了用户设置持久化。
- Windows Release 构建及 ZIP / SHA-256 生成成功；包内 CRC 与 SHA-256 已验证，EXE 版本资源及 GUI / CLI 子系统配置正确。
- Windows SDK 资源编译器已验证中文资源文件和素材路径。
- 两个 PowerShell 脚本通过语法解析检查；打包脚本已实际执行。
- 已恢复本机原本不完整的 Rust 1.99.1 安装，并使用仓库指定工具链通过检查、测试和 Release 打包。

## 待实机验收

- [ ] 用真实 Windows 微信 3.x / 4.x 缓存验证候选路径、加密包、分包和插件；本机默认位置未发现真实缓存。
- [ ] 验证自定义存储盘、重定向 Documents 和实际 UNC 共享上的缓存发现。
- [ ] 验证快捷键、中文输入法、鼠标拖放、文件选择/导出对话框、资源管理器定位、多屏 DPI、窗口按钮和主题切换。
- [ ] 点击设置页验证 CLI 安装/卸载及新终端 PATH 生效；本轮未修改用户的持久 PATH。
- [ ] 在真实发布目录验证可选开始菜单和文件关联的安装/卸载；本轮未注册用户文件关联。
- [ ] 在只有 VC++ 运行库、未安装开发工具的 Windows 10/11 机器上验证便携 ZIP。
- [ ] 确认更新后的 Windows ZIP 验证和 `macos-15` 回归在 GitHub Actions 上通过。本地 macOS 已通过全特性测试，并用合成包检查重建源码、原始视图和概要页；Windows 引擎已通过交叉类型检查。

## 使用入口

```powershell
cargo run -p wxlocated-app
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\bundle-windows.ps1
python scripts/smoke-cli.py --cli target/x86_64-pc-windows-msvc/release/wxlocated.exe
```

发布包位于 `target/bundle/windows/`，按 x64 和 ARM64 区分。ARM64 打包使用 `-Target aarch64-pc-windows-msvc`。
