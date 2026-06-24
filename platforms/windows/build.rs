use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        compile_windows_resources();
        // Framework-dependent keeps the app executable itself small, but WinUI 3
        // still needs the Windows App Runtime bootstrap and resources next to it.
        windows_reactor_setup::as_framework_dependent();
    }
}

fn compile_windows_resources() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let asset_dir = manifest_dir.join("assets");
    let icon_path = asset_dir.join("app.ico");

    println!("cargo:rerun-if-changed={}", icon_path.display());

    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        println!("cargo:warning=Windows app icon resource is only embedded for the GNU target");
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let rc_path = out_dir.join("app.rc");
    let res_obj = out_dir.join("app-resources.o");
    fs::write(&rc_path, windows_resource_script(&icon_path))
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", rc_path.display()));

    let windres = env::var("WINDRES").unwrap_or_else(|_| "windres".to_string());
    let status = Command::new(&windres)
        .arg("-O")
        .arg("coff")
        .arg("--codepage=65001")
        .arg("--target")
        .arg("pe-x86-64")
        .arg(&rc_path)
        .arg(&res_obj)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {windres}: {e}"));

    if !status.success() {
        panic!("windres failed with status {status}");
    }

    println!("cargo:rustc-link-arg-bins={}", res_obj.display());
}

fn windows_resource_script(icon_path: &PathBuf) -> String {
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION not set");
    let author = "\u{661f}\u{5149}\u{306e}\u{6bb2}\u{6ec5}\u{8005}";
    let copyright = format!("Copyright (c) 2026 {author}");
    let file_version = version_tuple(&version);
    let icon = icon_path.display().to_string().replace('\\', "\\\\");

    format!(
        r#"#pragma code_page(65001)

1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {file_version}
PRODUCTVERSION {file_version}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "Author", "{author}\0"
            VALUE "CompanyName", "{author}\0"
            VALUE "FileDescription", "Alma OneBot Bridge\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "AlmaOneBotBridge\0"
            VALUE "LegalCopyright", "{copyright}\0"
            VALUE "OriginalFilename", "AlmaOneBotBridge.exe\0"
            VALUE "ProductName", "Alma OneBot Bridge\0"
            VALUE "Publisher", "{author}\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    )
}

fn version_tuple(version: &str) -> String {
    let mut parts = version.split('.').map(|part| {
        part.split(|ch: char| !ch.is_ascii_digit())
            .next()
            .unwrap_or("0")
            .parse::<u16>()
            .unwrap_or(0)
    });

    format!(
        "{},{},{},{}",
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0)
    )
}
