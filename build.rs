//! Embeds the icon, manifest (DPI awareness, Windows 8+ compatibility,
//! common controls v6) and version info into the Windows executable.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=res");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut nums = version.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let (major, minor, patch) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0), nums.next().unwrap_or(0));

    // The .rc is generated with the version written in literally: passing
    // it as a macro needs different quote escaping for rc.exe and windres.
    let res = Path::new(env!("CARGO_MANIFEST_DIR")).join("res");
    let path = |name: &str| res.join(name).display().to_string().replace('\\', "\\\\");
    let rc = format!(
        r#"#include <winver.h>
1 ICON "{icon}"
1 24 "{manifest}"
VS_VERSION_INFO VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "Claude Taskbar Usage"
      VALUE "ProductName", "Claude Taskbar Usage"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "ClaudeTaskbarUsage.exe"
      VALUE "LegalCopyright", "MIT License"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = path("app.ico"),
        manifest = path("app.manifest"),
    );
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("app.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_required().unwrap();
}
