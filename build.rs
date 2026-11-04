//! Embeds the application icon, version information and manifest into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/mome.manifest");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    let icon = "assets/icon.ico";
    if std::path::Path::new(icon).exists() {
        res.set_icon(icon);
    } else {
        println!("cargo:warning=assets/icon.ico not found; building without an executable icon");
    }
    // ProductName and FileDescription come from [package.metadata.winresource] in Cargo.toml.
    if let Ok(manifest) = std::fs::read_to_string("assets/mome.manifest") {
        res.set_manifest(&manifest);
    }
    if let Err(err) = res.compile() {
        // Missing rc.exe (Windows SDK) should not break the build; only the icon is lost.
        println!("cargo:warning=failed to embed Windows resources: {err}");
    }
}
