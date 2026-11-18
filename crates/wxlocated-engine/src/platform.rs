//! Shared platform paths and shell integration used by the engine, CLI and UI.

use std::path::{Path, PathBuf};

pub fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

pub fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .filter(|path| path.is_dir())
        .or_else(home_dir)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(std::env::temp_dir)
}

/// Expands the current user's home and Windows-style environment variables.
/// Unknown variables and other users' `~name` prefixes are preserved.
pub fn expand_path(value: &str) -> PathBuf {
    let mut expanded = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('%') else {
            expanded.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let variable = &tail[..end];
        match std::env::var(variable) {
            Ok(value) if !variable.is_empty() => expanded.push_str(&value),
            _ => expanded.push_str(&rest[start..start + end + 2]),
        }
        rest = &tail[end + 1..];
    }
    expanded.push_str(rest);
    if let Some(rest) = expanded.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with('/') || cfg!(windows) && rest.starts_with('\\'))
        && let Some(home) = home_dir()
    {
        return home.join(rest.trim_start_matches(if cfg!(windows) { &['/', '\\'][..] } else { &['/'][..] }));
    }
    PathBuf::from(expanded)
}

pub fn cli_install_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().join("WxLocated/bin/wxlocated.cmd"))
    }
    #[cfg(not(windows))]
    {
        home_dir().map(|home| home.join(".local/bin/wxlocated"))
    }
}

/// The shell wrapper installed by the desktop app. Keep filesystem paths
/// canonical internally, but convert Windows verbatim prefixes for cmd.exe.
pub fn cli_wrapper(binary: &Path) -> std::io::Result<String> {
    #[cfg(windows)]
    {
        let text = binary
            .to_str()
            .ok_or_else(|| std::io::Error::other("命令行程序路径不是有效 Unicode"))?;
        let target = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            text.strip_prefix(r"\\?\").unwrap_or(text).to_string()
        };
        if target.encode_utf16().count() >= 260
            || !Path::new(&target).is_absolute()
            || !Path::new(&target).components().all(|component| match component {
                std::path::Component::Normal(name) => name.to_str().is_some_and(windows_safe_component),
                _ => true,
            })
        {
            return Err(std::io::Error::other(
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
        Ok(format!(
            "#!/bin/sh\n# Installed by WxLocated. Forwards to the bundled CLI.\nexec '{}' \"$@\"\n",
            binary.to_string_lossy().replace('\'', "'\\''")
        ))
    }
}

pub fn directory_on_path(directory: &Path) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|entry| paths_equal(&entry, directory)))
}

pub fn paths_equal(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    if cfg!(windows) {
        left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

/// Adds the wrapper directory to the current user's PATH; existing entries
/// (including expandable variables) are preserved. Existing terminals keep
/// their inherited environment and must be reopened.
#[cfg(windows)]
pub fn add_cli_to_user_path(directory: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
    use winreg::{RegKey, RegValue, enums::*};
    let (environment, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey("Environment")?;
    let existing = match environment.get_value::<String, _>("Path") {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    if !std::env::split_paths(&existing).any(|entry| paths_equal(&expand_path(&entry.to_string_lossy()), directory)) {
        let separator = if existing.is_empty() || existing.ends_with(';') { "" } else { ";" };
        let updated = format!("{existing}{separator}{}", directory.display());
        let bytes = updated.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
        environment.set_raw_value(
            "Path",
            &RegValue {
                bytes,
                vtype: REG_EXPAND_SZ,
            },
        )?;
        let setting: Vec<u16> = "Environment\0".encode_utf16().collect();
        // SAFETY: the UTF-16 string stays alive throughout this synchronous
        // broadcast. No output pointer is supplied; hung windows time out.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                setting.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                1000,
                std::ptr::null_mut(),
            );
        }
    }
    Ok(())
}

/// Whether a single component can be written without Win32 name aliasing.
pub fn windows_safe_component(name: &str) -> bool {
    if name.is_empty() || name.ends_with(['.', ' ']) || name.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ').to_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        && !(stem.starts_with("COM") || stem.starts_with("LPT"))
            .then(|| &stem[3..])
            .is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
}

pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .take(100)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']);
    if cleaned.is_empty() {
        "wxapkg-source".into()
    } else if !windows_safe_component(cleaned) {
        format!("_{cleaned}")
    } else {
        cleaned.into()
    }
}

pub fn file_manager_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Finder"
    } else if cfg!(windows) {
        "资源管理器"
    } else {
        "文件管理器"
    }
}

/// Additional Windows cache roots, including a relocated Documents folder and
/// the legacy WeChat file-storage setting. Scan only applet caches, not chat data.
#[cfg(windows)]
pub fn wechat_cache_paths() -> Vec<String> {
    use winreg::{RegKey, enums::*};
    let mut roots = Vec::new();
    if let Some(dirs) = directories::UserDirs::new()
        && let Some(documents) = dirs.document_dir()
    {
        roots.push(documents.to_path_buf());
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for key_name in [r"Software\Tencent\WeChat", r"Software\Tencent\Weixin"] {
        if let Ok(key) = hkcu.open_subkey(key_name)
            && let Ok(path) = key.get_value::<String, _>("FileSavePath")
            && !path.is_empty()
            && path != "MyDocument:"
        {
            roots.push(expand_path(&path));
        }
    }
    let mut candidates = Vec::new();
    for root in roots {
        // Some clients store the base directory, others the WeChat Files folder.
        for base in [root.clone(), root.join("WeChat Files"), root.join("xwechat_files")] {
            for suffix in ["Applet", "*/Applet", "*/applet", "*/app_data/radium/Applet", "all_users/applet"] {
                candidates.push(base.join(suffix).to_string_lossy().into_owned());
            }
        }
    }
    candidates
}

#[cfg(not(windows))]
pub fn wechat_cache_paths() -> Vec<String> {
    Vec::new()
}

#[cfg(windows)]
pub fn windows_version() -> String {
    use winreg::{RegKey, enums::*};
    let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else {
        return "Windows".into();
    };
    let build = key.get_value::<String, _>("CurrentBuildNumber").unwrap_or_default();
    let revision = key.get_value::<u32, _>("UBR").unwrap_or_default();
    let release = key.get_value::<String, _>("DisplayVersion").unwrap_or_default();
    let mut product = key.get_value::<String, _>("ProductName").unwrap_or_else(|_| "Windows".into());
    if build.parse::<u32>().is_ok_and(|build| build >= 22000) {
        product = product.replace("Windows 10", "Windows 11");
    }
    format!("{product} {release} (Build {build}.{revision})")
}

#[cfg(windows)]
pub fn wechat_version() -> Option<String> {
    use winreg::{RegKey, enums::*};
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let root = RegKey::predef(hive);
        for key_name in [
            r"Software\Tencent\WeChat",
            r"Software\Tencent\Weixin",
            r"Software\WOW6432Node\Tencent\WeChat",
            r"Software\WOW6432Node\Tencent\Weixin",
        ] {
            let Ok(key) = root.open_subkey(key_name) else { continue };
            if let Ok(directory) = key.get_value::<String, _>("InstallPath") {
                for name in ["WeChat.exe", "Weixin.exe"] {
                    if let Some(version) = executable_version(&PathBuf::from(&directory).join(name)) {
                        return Some(version);
                    }
                }
            }
            if let Ok(version) = key.get_value::<String, _>("Version")
                && !version.is_empty()
            {
                return Some(version);
            }
        }
        for name in ["WeChat.exe", "Weixin.exe"] {
            if let Ok(key) = root.open_subkey(format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{name}"))
                && let Ok(path) = key.get_value::<String, _>("")
                && let Some(version) = executable_version(Path::new(path.trim_matches('"')))
            {
                return Some(version);
            }
        }
    }
    None
}

#[cfg(windows)]
fn executable_version(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW};
    if !path.is_file() {
        return None;
    }
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: all strings are NUL-terminated; the version buffer is allocated
    // to the size returned by Windows and remains alive while querying it.
    unsafe {
        let size = GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut());
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let mut info = std::ptr::null_mut();
        let mut length = 0;
        let query = [b'\\' as u16, 0];
        if VerQueryValueW(data.as_ptr().cast(), query.as_ptr(), &mut info, &mut length) == 0
            || info.is_null()
            || length < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
        {
            return None;
        }
        let info = std::ptr::read_unaligned(info.cast::<VS_FIXEDFILEINFO>());
        if info.dwSignature != 0xfeef04bd {
            return None;
        }
        Some(format!(
            "{}.{}.{}.{}",
            info.dwFileVersionMS >> 16,
            info.dwFileVersionMS & 0xffff,
            info.dwFileVersionLS >> 16,
            info.dwFileVersionLS & 0xffff
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_without_turning_an_absolute_suffix_into_a_root() {
        if let Some(home) = home_dir() {
            assert_eq!(expand_path("~/a b/包"), home.join("a b/包"));
            assert_eq!(expand_path("~"), home);
            #[cfg(windows)]
            assert_eq!(expand_path(r"~\a b\包"), home.join(r"a b\包"));
        }
        assert_eq!(expand_path("~someone/a"), PathBuf::from("~someone/a"));
        assert_eq!(expand_path("relative/a"), PathBuf::from("relative/a"));
    }

    #[cfg(windows)]
    #[test]
    fn installed_cmd_wrapper_executes_unicode_percent_paths_and_forwards_arguments() {
        use std::os::windows::process::CommandExt;
        let root = std::env::temp_dir().join(format!("WxLocated 命令行 % {}", crate::export::uuid_string()));
        let outcome = (|| -> std::io::Result<_> {
            std::fs::create_dir_all(&root)?;
            let binary = root.join("wxlocated.exe");
            std::fs::copy(std::env::current_exe()?, &binary)?;
            let canonical = std::fs::canonicalize(&binary)?;
            let wrapper = root.join("wxlocated.cmd");
            std::fs::write(&wrapper, cli_wrapper(&canonical)?)?;
            // The copied test harness handles --list without running tests.
            // raw_arg preserves cmd.exe's required outer quoting.
            std::process::Command::new(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()))
                .raw_arg(format!(r#"/D /S /C ""{}" --list""#, wrapper.display()))
                .creation_flags(0x08000000)
                .output()
        })();
        let _ = std::fs::remove_dir_all(&root);
        let output = outcome.unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("platform::tests::windows_names_do_not_alias_devices_or_strip_suffixes"));
    }

    #[test]
    fn windows_names_do_not_alias_devices_or_strip_suffixes() {
        for name in ["CON", "nul.txt", "COM1.js", "LPT²", "a.", "a ", "a:b", "a\\b", "a?b"] {
            assert!(!windows_safe_component(name), "{name}");
            assert!(windows_safe_component(&safe_file_name(name)), "{name}");
        }
        assert!(windows_safe_component("component.js"));
        assert!(windows_safe_component("COM10.txt"));
    }

    #[test]
    fn preserves_unknown_and_unterminated_variables() {
        assert_eq!(
            expand_path("%WXLOCATED_MISSING_TEST_VARIABLE%/a"),
            PathBuf::from("%WXLOCATED_MISSING_TEST_VARIABLE%/a")
        );
        assert_eq!(expand_path("abc%unfinished"), PathBuf::from("abc%unfinished"));
    }
}
