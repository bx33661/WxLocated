use std::fs::{self, File};
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows_path;

pub fn cli_install_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().join("WxLocated/bin/wxlocated.cmd"))
    }
    #[cfg(not(windows))]
    {
        super::home_dir().map(|home| home.join(".local/bin/wxlocated"))
    }
}

/// The shell wrapper installed by the desktop app. Keep filesystem paths
/// canonical internally, but convert Windows verbatim prefixes for cmd.exe.
pub fn cli_wrapper(binary: &Path) -> io::Result<String> {
    #[cfg(windows)]
    {
        let text = binary.to_str().ok_or_else(|| io::Error::other("命令行程序路径不是有效 Unicode"))?;
        let target = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            text.strip_prefix(r"\\?\").unwrap_or(text).to_string()
        };
        if target.encode_utf16().count() >= 260
            || !Path::new(&target).is_absolute()
            || !Path::new(&target).components().all(|component| match component {
                std::path::Component::Normal(name) => name.to_str().is_some_and(super::windows_safe_component),
                _ => true,
            })
        {
            return Err(io::Error::other(
                "安装目录无法被 Windows 命令行访问，请将 WxLocated 移动到较短的普通路径后重新安装命令行工具",
            ));
        }
        let target = target.replace('%', "%%");
        Ok(format!(
            "@echo off\r\nsetlocal DisableDelayedExpansion\r\nchcp 65001 >nul\r\n\"{target}\" %*\r\nexit /b %errorlevel%\r\n"
        ))
    }
    #[cfg(not(windows))]
    {
        let target = binary.to_str().ok_or_else(|| io::Error::other("命令行程序路径不是有效 Unicode"))?;
        Ok(format!(
            "#!/bin/sh\n# Installed by WxLocated. Forwards to the bundled CLI.\nexec '{}' \"$@\"\n",
            target.replace('\'', "'\\''")
        ))
    }
}

/// Accepts the original wrapper and wrappers that also record an owned PATH entry.
pub fn cli_wrapper_matches(binary: &Path, contents: &str) -> bool {
    cli_wrapper(binary).is_ok_and(|expected| matches_body(binary, &expected, contents))
}

fn matches_body(binary: &Path, expected: &str, contents: &str) -> bool {
    #[cfg(windows)]
    let _ = binary;
    #[cfg(not(windows))]
    if let Some(target) = binary.to_str()
        && let Some(legacy) = contents.strip_prefix("#!/bin/sh\n# Installed by WxLocated. Forwards to the bundled CLI.\nexec \"")
        && legacy.strip_suffix("\" \"$@\"\n") == Some(target)
    {
        return true;
    }
    let Some(suffix) = contents.strip_prefix(expected) else { return false };
    #[cfg(windows)]
    if windows_path::Receipt::parse(suffix).is_some() {
        return true;
    }
    suffix.is_empty()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CliStatus {
    NotInstalled,
    Installed,
    Elsewhere,
}

#[derive(PartialEq, Eq)]
enum Shim {
    Missing,
    Owned(String),
    Foreign,
}

fn read_shim(path: &Path, binary: Option<&Path>, expected: Option<&str>) -> io::Result<Shim> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Shim::Missing),
        Err(error) => return Err(error),
    };
    let (Some(binary), Some(expected)) = (binary, expected) else { return Ok(Shim::Foreign) };
    if !metadata.is_file() {
        return Ok(Shim::Foreign);
    }
    // A Windows reparse point can refer to an object owned by someone else.
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(Shim::Foreign);
        }
    }
    match fs::read_to_string(path) {
        Ok(contents) if matches_body(binary, expected, &contents) => Ok(Shim::Owned(contents)),
        Ok(_) => Ok(Shim::Foreign),
        Err(error) if error.kind() == ErrorKind::InvalidData => Ok(Shim::Foreign),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Shim::Missing),
        Err(error) => Err(error),
    }
}

pub fn cli_status(path: &Path, binary: Option<&Path>) -> io::Result<CliStatus> {
    let expected = binary.map(cli_wrapper).transpose()?;
    Ok(match read_shim(path, binary, expected.as_deref())? {
        Shim::Missing => CliStatus::NotInstalled,
        Shim::Owned(_) => CliStatus::Installed,
        Shim::Foreign => CliStatus::Elsewhere,
    })
}

fn foreign_wrapper() -> io::Error {
    io::Error::new(ErrorKind::AlreadyExists, "安装位置存在其他文件，无法安全覆盖或删除")
}

fn lock_installation(path: &Path) -> io::Result<File> {
    // Separate from the shim inode, which can be replaced atomically while
    // other WxLocated instances are waiting on the same lock.
    let lock = File::options().read(true).write(true).create(true).truncate(false).open(path.with_file_name(".wxlocated-install.lock"))?;
    lock.lock()?;
    Ok(lock)
}

pub fn install_cli(path: &Path, binary: &Path) -> io::Result<()> {
    let expected = cli_wrapper(binary)?;
    let directory = path.parent().ok_or_else(|| io::Error::other("无法确定当前用户的命令行安装目录"))?;
    fs::create_dir_all(directory)?;
    let _lock = lock_installation(path)?;
    let previous = read_shim(path, Some(binary), Some(&expected))?;
    if previous == Shim::Foreign {
        return Err(foreign_wrapper());
    }

    #[cfg(windows)]
    let (contents, path_update) = {
        let old_receipt = match &previous {
            Shim::Owned(contents) => windows_path::Receipt::parse(&contents[expected.len()..]),
            _ => None,
        };
        let (receipt, update) = windows_path::install(directory, old_receipt)?;
        let mut contents = expected.clone();
        if let Some(receipt) = receipt {
            receipt.append_to(&mut contents);
        }
        (contents, update)
    };
    #[cfg(not(windows))]
    let contents = &expected;

    // Publish the complete shim before exposing the new PATH entry. The previous
    // shim is restored if the registry could not be updated.
    publish_shim(path, binary, &expected, &previous, contents.as_str())?;
    #[cfg(windows)]
    if let Some(update) = path_update
        && let Err(error) = update.commit()
    {
        let installed = Shim::Owned(contents);
        let rollback = match &previous {
            Shim::Missing => remove_shim(path, binary, &expected, &installed),
            Shim::Owned(old_contents) => publish_shim(path, binary, &expected, &installed, old_contents),
            Shim::Foreign => unreachable!(),
        };
        return Err(rollback_error(error, rollback));
    }
    Ok(())
}

pub fn uninstall_cli(path: &Path, binary: &Path) -> io::Result<()> {
    let expected = cli_wrapper(binary)?;
    match read_shim(path, Some(binary), Some(&expected))? {
        Shim::Missing => return Ok(()),
        Shim::Foreign => return Err(foreign_wrapper()),
        Shim::Owned(_) => {}
    }
    let _lock = lock_installation(path)?;
    let previous = read_shim(path, Some(binary), Some(&expected))?;
    match &previous {
        Shim::Missing => return Ok(()),
        Shim::Foreign => return Err(foreign_wrapper()),
        Shim::Owned(_) => {}
    }

    #[cfg(windows)]
    if let Shim::Owned(contents) = &previous
        && let Some(receipt) = windows_path::Receipt::parse(&contents[expected.len()..])
        && let Some(directory) = path.parent()
        && let Some(update) = windows_path::uninstall(directory, &receipt)?
    {
        // Leave the wrapper and receipt intact if PATH cleanup fails. If removal
        // then fails, restore PATH while the owned wrapper is still installed.
        update.commit()?;
        return remove_shim(path, binary, &expected, &previous).map_err(|error| rollback_error(error, update.revert()));
    }
    remove_shim(path, binary, &expected, &previous)
}

fn check_shim(path: &Path, binary: &Path, expected: &str, previous: &Shim) -> io::Result<()> {
    if &read_shim(path, Some(binary), Some(expected))? == previous {
        Ok(())
    } else {
        Err(io::Error::other("安装位置中的文件已更改，请重试"))
    }
}

fn remove_shim(path: &Path, binary: &Path, expected: &str, previous: &Shim) -> io::Result<()> {
    check_shim(path, binary, expected, previous)?;
    fs::remove_file(path)
}

fn publish_shim(path: &Path, binary: &Path, expected: &str, previous: &Shim, contents: &str) -> io::Result<()> {
    let staging = path.with_file_name(format!(".wxlocated-{}.tmp", uuid::Uuid::new_v4()));
    let file = File::create_new(&staging)?;
    let result = (|| {
        let mut file = file;
        file.write_all(contents.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()?;
        drop(file);
        check_shim(path, binary, expected, previous)?;
        match previous {
            Shim::Missing => publish_new_shim(&staging, path),
            Shim::Owned(_) => fs::rename(&staging, path),
            Shim::Foreign => Err(foreign_wrapper()),
        }
    })();
    let _ = fs::remove_file(staging);
    result
}

#[cfg(not(windows))]
fn publish_new_shim(staging: &Path, path: &Path) -> io::Result<()> {
    fs::hard_link(staging, path)
}

#[cfg(windows)]
fn publish_new_shim(staging: &Path, path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let staging = fs::canonicalize(staging)?;
    let destination = staging.with_file_name(path.file_name().ok_or_else(|| io::Error::other("命令行安装路径没有文件名"))?);
    let source: Vec<_> = staging.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both paths remain NUL-terminated and alive during the call. With
    // flags set to zero, MoveFileExW fails if the destination already exists.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn rollback_error(error: io::Error, rollback: io::Result<()>) -> io::Error {
    match rollback {
        Ok(()) => error,
        Err(rollback) => io::Error::other(format!("{error}；恢复原状态失败：{rollback}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_matching_rejects_foreign_commands_with_the_same_target() {
        #[cfg(windows)]
        let binary = Path::new(r"C:\Program Files\WxLocated\wxlocated.exe");
        #[cfg(not(windows))]
        let binary = Path::new("/Applications/WxLocated's.app/Contents/Helpers/wxlocated");

        let wrapper = cli_wrapper(binary).unwrap();
        assert!(cli_wrapper_matches(binary, &wrapper));
        assert!(!cli_wrapper_matches(binary, &format!("# custom wrapper\n{wrapper}")));
        assert!(!cli_wrapper_matches(binary, &format!("{wrapper}echo other\n")));
    }

    #[cfg(unix)]
    #[test]
    fn replaces_legacy_wrapper_without_touching_foreign_files() -> io::Result<()> {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&directory)?;
        let shim = directory.join("wxlocated");
        let binary = directory.join("WxLocated's CLI");
        let result = (|| {
            let legacy = format!("#!/bin/sh\n# Installed by WxLocated. Forwards to the bundled CLI.\nexec \"{}\" \"$@\"\n", binary.display());
            fs::write(&shim, &legacy)?;
            assert!(cli_wrapper_matches(&binary, &legacy));
            install_cli(&shim, &binary)?;
            assert_eq!(fs::read_to_string(&shim)?, cli_wrapper(&binary)?);
            uninstall_cli(&shim, &binary)?;
            assert!(!shim.exists());
            fs::write(&shim, "custom")?;
            assert_eq!(install_cli(&shim, &binary).unwrap_err().kind(), ErrorKind::AlreadyExists);
            assert_eq!(uninstall_cli(&shim, &binary).unwrap_err().kind(), ErrorKind::AlreadyExists);
            assert_eq!(fs::read_to_string(&shim)?, "custom");
            Ok(())
        })();
        let _ = fs::remove_dir_all(directory);
        result
    }
}
