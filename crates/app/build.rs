//! Captures build metadata for the About box, and embeds the Windows icon.
//!
//! Emits `BUILD_COMMIT` and `BUILD_DATE`. Both degrade to "unknown" rather than
//! failing the build, so The Editor still compiles from a source tarball with
//! no git repository present.
//!
//! The icon is a Windows resource compiled into the executable. Without one,
//! Explorer, the taskbar and Alt+Tab all show the generic application icon —
//! which is not something the program can fix at runtime, because those are
//! read from the file rather than asked of the process. The window's own icon
//! is a separate thing, set in `main`.

use std::process::Command;

fn main() {
    embed_windows_icon();

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());

    // Committer date of HEAD, ISO-8601. Using git rather than the system clock
    // keeps release builds reproducible.
    let date = Command::new("git")
        .args(["log", "-1", "--format=%cs"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    let rustc = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned()))
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=BUILD_DATE={date}");
    println!("cargo:rustc-env=BUILD_RUSTC={rustc}");

    // Rerun when HEAD moves, but do not fail if .git is absent.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}

/// Compile the icon and version details into the executable's resources.
///
/// Windows only, and only when the icon is present: building from a checkout
/// without the assets should still produce a working editor, just a plainer
/// one.
#[cfg(windows)]
fn embed_windows_icon() {
    let icon = std::path::Path::new("../../assets/icon.ico");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if !icon.exists() {
        println!("cargo:warning=assets/icon.ico is missing; the exe will have no icon");
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().unwrap_or("../../assets/icon.ico"));
    res.set("ProductName", "The Editor");
    res.set("FileDescription", "The Editor - an IDE for Python and Rust");
    res.set("LegalCopyright", "Copyright (c) 2026 Gareth Finch. MIT licensed.");
    if let Err(e) = res.compile() {
        // Not fatal. A missing resource compiler on someone else's machine
        // should cost them an icon, not a build.
        println!("cargo:warning=could not embed the icon: {e}");
    }
}

#[cfg(not(windows))]
fn embed_windows_icon() {}
