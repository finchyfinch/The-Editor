//! The Editor — an IDE for Python and Rust.
//!
//! Copyright © 2026 Gareth Finch. MIT licensed.

// Release builds on Windows must not open a console window behind the app.
// Debug builds keep it, because that is where the log tail is.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod branches_view;
mod cli;
mod commands;
mod completion;
mod debugger;
mod diff_view;
mod docs_window;
mod environment;
mod file_picker;
mod git_panel;
mod history_view;
mod logging;
mod new_file;
mod packages_panel;
mod palette;
mod panic_hook;
mod project_search;
mod recovery;
mod runner;
mod settings_window;
mod symbol_picker;
mod terminal;
mod terminal_keys;
mod tests_panel;
mod venv_dialog;
mod watcher;

use anyhow::{Context, Result};
use editor_config::paths::AppPaths;
use eframe::egui;

fn main() -> Result<()> {
    // Taken before anything else, so the figure logged at the first frame is
    // startup as the user experiences it: process start to something on screen.
    let started = std::time::Instant::now();

    let open = match cli::parse(std::env::args_os().skip(1)) {
        cli::Startup::Open(paths) => paths,
        cli::Startup::Print { text, failed } => {
            attach_parent_console();
            if failed {
                eprintln!("{text}");
                std::process::exit(2);
            }
            println!("{text}");
            return Ok(());
        }
    };

    let paths = AppPaths::resolve().context("resolving application directories")?;
    paths
        .ensure_dirs()
        .context("creating application directories")?;

    // Logging and crash handling come up before anything that can fail
    // interestingly, so that failures after this point are diagnosable.
    let (_log_guard, log_dir) = logging::init(&paths.log_dir())?;
    panic_hook::install(paths.backup_dir(), log_dir.clone());

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        commit = env!("BUILD_COMMIT"),
        portable = paths.is_portable(),
        config = %paths.config_dir().display(),
        "The Editor starting"
    );

    let log_dir_display = log_dir.display().to_string();

    // Read before the window is built, because it decides how to build it.
    let renderer = editor_config::settings::Settings::load(&paths.settings_file())
        .0
        .renderer();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("The Editor")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0])
            .with_app_id("uk.garethfinch.the-editor")
            .with_icon(window_icon()),
        // M1 restores the previous window geometry from the session file and
        // validates it against the monitors actually present.
        persist_window: false,
        renderer: match renderer {
            editor_config::settings::Renderer::Glow => eframe::Renderer::Glow,
            editor_config::settings::Renderer::Wgpu => eframe::Renderer::Wgpu,
        },
        ..Default::default()
    };

    eframe::run_native(
        "The Editor",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::EditorApp::new(
                cc,
                paths,
                log_dir_display,
                open,
                started,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("starting the window: {e}"))?;

    tracing::info!("The Editor exiting cleanly");
    Ok(())
}

/// Borrow the console that launched us, so `--version` has somewhere to print.
///
/// Release builds are linked as a GUI application — otherwise a console window
/// flashes up behind the editor every time it starts — and a GUI application
/// begins life with no standard output, so `println!` writes into nothing at
/// all and says it succeeded. Attaching to the parent process's console gets
/// the console back, but not the handles: those have to be opened against
/// `CONOUT$` and installed by hand.
///
/// Only fills in handles that are missing, so `the-editor --version > out.txt`
/// still redirects. Started from Explorer there is no parent console and this
/// does nothing, which is the right answer — there is nowhere to print.
#[cfg(windows)]
fn attach_parent_console() {
    use std::os::windows::ffi::OsStrExt;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const INVALID_HANDLE_VALUE: isize = -1;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;

    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> isize;
        fn SetStdHandle(which: u32, handle: isize) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut core::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: isize,
        ) -> isize;
    }

    // Safety: every call below takes integers and pointers to buffers this
    // function owns, and each reports failure by return value rather than by
    // misbehaving. `AttachConsole` fails when there is no parent console.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE.
        for (which, name, access) in [
            (-10i32 as u32, "CONIN$", GENERIC_READ | GENERIC_WRITE),
            (-11i32 as u32, "CONOUT$", GENERIC_READ | GENERIC_WRITE),
            (-12i32 as u32, "CONOUT$", GENERIC_READ | GENERIC_WRITE),
        ] {
            let existing = GetStdHandle(which);
            if existing != 0 && existing != INVALID_HANDLE_VALUE {
                continue; // Redirected to a file or a pipe; leave it be.
            }
            let wide: Vec<u16> = std::ffi::OsStr::new(name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let handle = CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                0,
            );
            if handle != INVALID_HANDLE_VALUE {
                SetStdHandle(which, handle);
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// The icon shown in the title bar, the taskbar and Alt+Tab.
///
/// Separate from the one compiled into the executable: that is read from the
/// *file* by Explorer, this is asked of the running *process*, and setting one
/// does not set the other.
///
/// Stored as raw RGBA rather than a PNG so no image decoder is needed for one
/// 64-pixel square. Generated alongside `assets/icon.ico`.
fn window_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let rgba = include_bytes!("../../../assets/icon-64.rgba").to_vec();
    debug_assert_eq!(rgba.len(), (SIZE * SIZE * 4) as usize);
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}
