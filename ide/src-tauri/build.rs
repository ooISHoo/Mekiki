fn main() {
    // Rerun winres when only an icon changes, or the executable keeps its old
    // RT_GROUP_ICON resource.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/128x128@2x.png");
    println!("cargo:rerun-if-changed=icons/256x256.png");
    println!("cargo:rerun-if-changed=icons/icon.png");
    tauri_build::build();
}
