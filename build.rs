// Embeds the icon and version info with MinGW's windres (GNU toolchain).
// If windres is not on PATH the build still succeeds, just without resources.
use std::process::Command;
fn main() {
    println!("cargo:rerun-if-changed=assets/wispshot.rc");
    println!("cargo:rerun-if-changed=assets/wispshot.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("wispshot_res.o");
    let ok = Command::new("windres")
        .args(["--input", "wispshot.rc", "--output-format=coff", "--output"])
        .arg(&out)
        .current_dir("assets")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=windres not found: building without icon/version info");
    }
}
