//! Windows user PATH updates and the ownership record embedded in the CMD shim.

use std::borrow::Cow;
use std::io::{self, ErrorKind};
use std::path::Path;

use winreg::{RegKey, RegValue, enums::*};

use super::super::{expand_path, paths_equal};

const RECEIPT_PREFIX: &str = "rem WxLocated PATH v1 ";
const LOCAL_ENTRY: &str = r"%LOCALAPPDATA%\WxLocated\bin";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Missing,
    Literal,
    Expandable,
}

impl Origin {
    fn value_type(self) -> RegType {
        match self {
            Self::Literal => REG_SZ,
            Self::Missing | Self::Expandable => REG_EXPAND_SZ,
        }
    }

    fn resolve(self, entry: &str) -> std::path::PathBuf {
        match self {
            Self::Literal => entry.into(),
            Self::Missing | Self::Expandable => expand_path(entry),
        }
    }
}

pub(super) struct Receipt {
    origin: Origin,
    entry: String,
}

impl Receipt {
    pub(super) fn parse(suffix: &str) -> Option<Self> {
        let (origin, hex) = suffix.strip_prefix(RECEIPT_PREFIX)?.strip_suffix("\r\n")?.split_once(' ')?;
        let origin = match origin {
            "missing" => Origin::Missing,
            "literal" => Origin::Literal,
            "expandable" => Origin::Expandable,
            _ => return None,
        };
        let entry = String::from_utf8(hex::decode(hex).ok()?).ok()?;
        valid_entry(&entry).then_some(Self { origin, entry })
    }

    pub(super) fn append_to(&self, wrapper: &mut String) {
        let origin = match self.origin {
            Origin::Missing => "missing",
            Origin::Literal => "literal",
            Origin::Expandable => "expandable",
        };
        // The receipt follows `exit /b` and consists solely of CMD-safe text.
        wrapper.push_str(RECEIPT_PREFIX);
        wrapper.push_str(origin);
        wrapper.push(' ');
        wrapper.push_str(&hex::encode(&self.entry));
        wrapper.push_str("\r\n");
    }

    fn matches_install(&self, directory: &Path, value: &RegValue<'_>) -> bool {
        value.vtype == self.origin.value_type()
            && (directory.to_str() == Some(self.entry.as_str())
                || self.origin != Origin::Literal
                    && self.entry == LOCAL_ENTRY
                    && paths_equal(&self.origin.resolve(&self.entry), directory))
    }
}

fn valid_entry(entry: &str) -> bool {
    !entry.is_empty() && !entry.contains([';', '"', '\0', '\r', '\n'])
}

pub(super) fn install(directory: &Path, previous: Option<Receipt>) -> io::Result<(Option<Receipt>, Option<PathUpdate>)> {
    let before = read_value()?;
    let (origin, text) = decode(before.as_ref())?;
    if std::env::split_paths(&text).any(|entry| paths_equal(&origin.resolve(&entry.to_string_lossy()), directory)) {
        // An unreceipted entry could belong to the user or have been left by
        // older WxLocated versions. Its presence does not establish ownership.
        let receipt = previous.filter(|receipt| {
            before.as_ref().is_some_and(|value| receipt.matches_install(directory, value))
                && path_entries(&text).any(|entry| entry == receipt.entry)
        });
        return Ok((receipt, None));
    }
    let literal = directory.to_str().ok_or_else(|| io::Error::other("命令行安装路径不是有效 Unicode"))?;
    if !valid_entry(literal) {
        return Err(io::Error::other("无法将命令行安装目录安全加入用户 PATH"));
    }
    let entry = if origin == Origin::Literal || !literal.contains('%') {
        literal
    } else if paths_equal(&origin.resolve(LOCAL_ENTRY), directory) {
        // Expand %LOCALAPPDATA% once to avoid interpreting percent signs in
        // a literal profile path as expandable variables in a REG_EXPAND_SZ.
        LOCAL_ENTRY
    } else {
        return Err(io::Error::other("无法将命令行安装目录安全加入用户 PATH"));
    };
    let updated = append_entry(text, entry)?;
    let after = Some(encode(origin, &updated));
    Ok((Some(Receipt { origin, entry: entry.into() }), Some(PathUpdate { before, after })))
}

pub(super) fn uninstall(directory: &Path, receipt: &Receipt) -> io::Result<Option<PathUpdate>> {
    let before = read_value()?;
    let Some(value) = before.as_ref() else { return Ok(None) };
    if !receipt.matches_install(directory, value) {
        return Ok(None);
    }
    let (origin, text) = decode(Some(value))?;
    let Some(updated) = remove_entry(&text, &receipt.entry) else { return Ok(None) };
    let after = if receipt.origin == Origin::Missing && text == receipt.entry {
        None
    } else {
        Some(encode(origin, &updated))
    };
    Ok(Some(PathUpdate { before, after }))
}

fn append_entry(mut path: String, entry: &str) -> io::Result<String> {
    if path.bytes().filter(|&byte| byte == b'"').count() % 2 != 0 {
        return Err(io::Error::other("用户 PATH 含有未闭合的引号"));
    }
    if !path.is_empty() {
        // Keep any trailing separator: it represents an existing empty entry.
        path.push(';');
    }
    path.push_str(entry);
    Ok(path)
}

fn path_entries(path: &str) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    path.split(move |character| {
        if character == '"' {
            quoted = !quoted;
        }
        character == ';' && !quoted
    })
}

fn remove_entry(path: &str, owned: &str) -> Option<String> {
    let mut offset = 0;
    let mut found = None;
    for entry in path_entries(path) {
        let end = offset + entry.len();
        if entry == owned {
            // An exact duplicate makes it impossible to identify the original.
            if found.is_some() {
                return None;
            }
            found = Some(if offset == 0 { (0, (end + 1).min(path.len())) } else { (offset - 1, end) });
        }
        offset = end + 1;
    }
    let (start, end) = found?;
    let mut remaining = String::with_capacity(path.len() - (end - start));
    remaining.push_str(&path[..start]);
    remaining.push_str(&path[end..]);
    Some(remaining)
}

fn decode(value: Option<&RegValue<'_>>) -> io::Result<(Origin, String)> {
    let Some(value) = value else { return Ok((Origin::Missing, String::new())) };
    let invalid_value = || io::Error::new(ErrorKind::InvalidData, "无法读取用户 PATH 的注册表值");
    let origin = match value.vtype {
        REG_SZ => Origin::Literal,
        REG_EXPAND_SZ => Origin::Expandable,
        _ => return Err(invalid_value()),
    };
    if value.bytes.len() % 2 != 0 {
        return Err(invalid_value());
    }
    let mut text: String = char::decode_utf16(value.bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])))
        .collect::<Result<_, _>>()
        .map_err(|_| invalid_value())?;
    text.truncate(text.trim_end_matches('\0').len());
    if text.contains('\0') {
        return Err(invalid_value());
    }
    Ok((origin, text))
}

fn encode(origin: Origin, text: &str) -> RegValue<'static> {
    let bytes = text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
    RegValue { bytes: Cow::Owned(bytes), vtype: origin.value_type() }
}

fn read_value() -> io::Result<Option<RegValue<'static>>> {
    match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Environment", KEY_QUERY_VALUE) {
        Ok(environment) => read_path(&environment),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_path(environment: &RegKey) -> io::Result<Option<RegValue<'static>>> {
    match environment.get_raw_value("Path") {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) struct PathUpdate {
    before: Option<RegValue<'static>>,
    after: Option<RegValue<'static>>,
}

impl PathUpdate {
    pub(super) fn commit(&self) -> io::Result<()> {
        write_value(self.before.as_ref(), self.after.as_ref())
    }

    pub(super) fn revert(&self) -> io::Result<()> {
        write_value(self.after.as_ref(), self.before.as_ref())
    }
}

fn write_value(expected: Option<&RegValue<'_>>, updated: Option<&RegValue<'_>>) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let flags = KEY_QUERY_VALUE | KEY_SET_VALUE;
    let environment = if expected.is_none() {
        hkcu.create_subkey_with_flags("Environment", flags)?.0
    } else {
        hkcu.open_subkey_with_flags("Environment", flags)?
    };
    if read_path(&environment)?.as_ref() != expected {
        return Err(io::Error::other("用户 PATH 已更改，请重试"));
    }
    if let Some(value) = updated {
        environment.set_raw_value("Path", value)?;
    } else {
        environment.delete_value("Path")?;
    }
    broadcast_path_change();
    Ok(())
}

fn broadcast_path_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};

    let setting: Vec<u16> = "Environment\0".encode_utf16().collect();
    // SAFETY: The UTF-16 string lives throughout the synchronous broadcast.
    // No output pointer is supplied. Hung windows time out.
    unsafe {
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, setting.as_ptr() as isize, SMTO_ABORTIFHUNG, 1000, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_is_bound_to_the_wrapper_body() {
        let binary = Path::new(r"C:\Program Files\WxLocated\wxlocated.exe");
        let mut wrapper = super::super::cli_wrapper(binary).unwrap();
        Receipt { origin: Origin::Expandable, entry: LOCAL_ENTRY.into() }.append_to(&mut wrapper);
        assert!(super::super::cli_wrapper_matches(binary, &wrapper));
        assert!(!super::super::cli_wrapper_matches(Path::new(r"C:\other\wxlocated.exe"), &wrapper));
        wrapper.push_str("echo unexpected\r\n");
        assert!(!super::super::cli_wrapper_matches(binary, &wrapper));
    }

    #[test]
    fn removes_only_the_unambiguous_owned_token_and_preserves_path_spelling() {
        let existing = r#"%USERPROFILE%\tools;"C:\SDK;tools";"#;
        let entry = r"C:\AppData\Local\WxLocated\bin";
        let installed = append_entry(existing.into(), entry).unwrap();
        assert_eq!(remove_entry(&installed, entry).as_deref(), Some(existing));
        assert_eq!(remove_entry(&format!("{installed};D:\\user"), entry), Some(format!("{existing};D:\\user")));
        assert_eq!(remove_entry(&format!("{installed};{entry}"), entry), None);
        assert_eq!(remove_entry(&format!("\"{entry}\""), entry), None);
    }

    #[test]
    fn registry_encoding_keeps_string_type_and_expandable_variables() {
        let text = r"%USERPROFILE%\tools";
        for origin in [Origin::Literal, Origin::Expandable] {
            let value = encode(origin, text);
            assert_eq!(decode(Some(&value)).ok(), Some((origin, text.into())));
        }
    }
}
