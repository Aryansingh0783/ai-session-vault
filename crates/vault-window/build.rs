// On the GNU toolchain wry needs WebView2Loader.dll next to the exe. Copy the one webview2-com-sys built.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/<profile>/build/vault-window-xxxx/out
    let Some(profile_dir) = out.ancestors().nth(3) else { return };
    let build = profile_dir.join("build");
    let arch = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") { "arm64" } else { "x64" };
    // webview2-com-sys is built before us (we depend on it through wry)
    if let Ok(rd) = std::fs::read_dir(&build) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("webview2-com-sys-") {
                let dll = e.path().join("out").join(arch).join("WebView2Loader.dll");
                if dll.exists() {
                    let _ = std::fs::copy(&dll, profile_dir.join("WebView2Loader.dll"));
                    return;
                }
            }
        }
    }
    println!("cargo:warning=WebView2Loader.dll not found; copy it next to avault-window.exe");
}
