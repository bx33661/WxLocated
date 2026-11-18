//! Shared build-time Windows icon, manifest and executable version resources.

pub fn compile(name: &str, include_manifest: bool) {
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let manifest = root.join("resources/windows.manifest");
    let icon = root.join("crates/wxlocated-app/assets/AppIcon.ico");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", icon.display());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers = format!("{},0", version.replace('.', ","));
    // GPUI already embeds its Windows DPI/common-controls manifest. Linking
    // a second resource with ID 1 produces CVT1100; only the headless CLI
    // supplies its own manifest here.
    let manifest_resource = if include_manifest {
        format!("1 24 \"{}\"", manifest.to_string_lossy().replace('\\', "/"))
    } else {
        String::new()
    };
    let resource = format!(
        r#"
#pragma code_page(65001)
1 ICON "{icon}"
{manifest_resource}
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS 0x40004
FILETYPE 1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "{name}\0"
      VALUE "FileVersion", "{version}\0"
      VALUE "ProductName", "WxLocated\0"
      VALUE "ProductVersion", "{version}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.to_string_lossy().replace('\\', "/"),
    );
    let path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("wxlocated.rc");
    std::fs::write(&path, resource).unwrap();
    embed_resource::compile(path, embed_resource::NONE).manifest_required().unwrap();
}
