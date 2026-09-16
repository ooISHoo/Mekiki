//! Embed the application manifest into the Windows binary.
//!
//! The tray menu needs Common Controls v6 (see `mekiki-mcp.manifest`). Rust
//! does not embed a manifest by itself, and the MSVC linker can do it without
//! an extra build dependency.

fn main() {
    println!("cargo:rerun-if-changed=mekiki-mcp.manifest");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("mekiki-mcp.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
