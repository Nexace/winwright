//! Embeds `winwright.exe.manifest` (common controls v6, per-monitor-v2 DPI) into the binary.

fn main() {
    println!("cargo:rerun-if-changed=winwright.exe.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let manifest = std::path::Path::new(&dir).join("winwright.exe.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
