use std::io;
use std::path::{Path, PathBuf};

use wxlocated_engine::platform;

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
        .filter(|candidate| candidate.is_file())
        .find_map(|candidate| {
            let target = std::fs::canonicalize(&candidate).ok();
            (target != own).then(|| target.unwrap_or(candidate))
        })
}

pub(super) fn shim_path() -> io::Result<PathBuf> {
    platform::cli_install_path().ok_or_else(|| io::Error::other("无法确定当前用户的命令行安装目录"))
}

pub(super) fn install() -> io::Result<()> {
    update(platform::install_cli)
}

pub(super) fn uninstall() -> io::Result<()> {
    update(platform::uninstall_cli)
}

fn update(operation: fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let binary = cli_binary().ok_or_else(|| io::Error::other("找不到随 App 附带的 wxlocated"))?;
    operation(&shim_path()?, &binary)
}
