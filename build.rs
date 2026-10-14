//! Links the logo and version details (assets/pocketmedia.res, made by
//! assets/make-icon.py) into pocketmedia.exe, so Explorer, the taskbar and
//! Alt+Tab show the PocketMedia icon. Only the std library is used.

fn main() {
    println!("cargo:rerun-if-changed=assets/pocketmedia.res");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|o| o == "windows");
    if !(windows && msvc) {
        return; // Other toolchains build without the icon (the app falls back to Windows' own).
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let res = std::path::Path::new(&dir).join("assets").join("pocketmedia.res");
    // link.exe takes a compiled .res file as an ordinary input.
    println!("cargo:rustc-link-arg-bins={}", res.display());
}
