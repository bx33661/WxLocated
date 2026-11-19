#[cfg(windows)]
#[path = "../../scripts/windows-resource.rs"]
mod windows;

fn main() {
    #[cfg(windows)]
    windows::compile("WxLocated CLI", true);
}
