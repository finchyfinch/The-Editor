//! The Editor — an IDE for Python and Rust.
//!
//! Copyright © 2026 Gareth Finch. MIT licensed.

// Release builds on Windows must not open a console window behind the app.
// Debug builds keep it, because that is where the log tail is.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod logging;
mod new_file;
mod palette;
mod panic_hook;
mod runner;
mod venv_dialog;

use anyhow::{Context, Result};
use editor_config::paths::AppPaths;
use eframe::egui;

fn main() -> Result<()> {
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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("The Editor")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0])
            .with_app_id("uk.garethfinch.the-editor"),
        // M1 restores the previous window geometry from the session file and
        // validates it against the monitors actually present.
        persist_window: false,
        ..Default::default()
    };

    eframe::run_native(
        "The Editor",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::EditorApp::new(cc, paths, log_dir_display)))),
    )
    .map_err(|e| anyhow::anyhow!("starting the window: {e}"))?;

    tracing::info!("The Editor exiting cleanly");
    Ok(())
}
