// On the GNU toolchain wry needs WebView2Loader.dll next to the exe. Copy the one that ships inside the
// webview2-com-sys crate (the version pinned in Cargo.lock) into the target profile folder.
use std::path::{Path, PathBuf};

fn locked_version(lock: &Path) -> Option<String> {
    let text = std::fs::read_to_string(lock).ok()?;
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "name = \"webview2-com-sys\"" {
            let v = lines.next()?.trim().strip_prefix("version = \"")?.trim_end_matches('"').to_string();
            return Some(v);
        }
    }
    None
}

fn find_in_registry(version: &str, arch: &str) -> Option<PathBuf> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| PathBuf::from(h).join(".cargo")))?;
    for e in std::fs::read_dir(cargo_home.join("registry").join("src")).ok()?.flatten() {
        let dll = e.path().join(format!("webview2-com-sys-{version}")).join(arch).join("WebView2Loader.dll");
        if dll.exists() {
            return Some(dll);
        }
    }
    None
}

/// Give avault-window.exe the project icon and version info (Windows only).
#[cfg(windows)]
fn embed_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico").set("ProductName", "AI Session Vault").set("FileDescription", "AI Session Vault");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    #[cfg(windows)]
    embed_icon();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/<profile>/build/vault-window-xxxx/out
    let Some(profile_dir) = out.ancestors().nth(3) else { return };
    let arch = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") { "arm64" } else { "x64" };
    let lock = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../Cargo.lock");
    let dll = locked_version(&lock).and_then(|v| find_in_registry(&v, arch));
    match dll {
        Some(d) if std::fs::copy(&d, profile_dir.join("WebView2Loader.dll")).is_ok() => {}
        _ => println!("cargo:warning=WebView2Loader.dll not found; copy it next to avault-window.exe"),
    }
}
