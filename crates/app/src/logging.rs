//! File and console logging.
//!
//! Logs go to a daily-rotated file under the platform data directory, and to
//! stderr when a terminal is attached. The level comes from `RUST_LOG` if set,
//! otherwise `info` for The Editor's own crates and `warn` for dependencies —
//! wgpu in particular is extremely chatty at `info`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Holds the background writer alive. Dropping it flushes and stops the
/// logging thread, so the returned guard must live until the process exits —
/// dropping it early silently truncates the log.
#[derive(Debug)]
pub(crate) struct LogGuard(#[allow(dead_code)] WorkerGuard);

/// Initialise logging. Returns the guard and the directory logs were written
/// to, so Help -> Open Log Folder can point at it.
pub(crate) fn init(log_dir: &Path) -> Result<(LogGuard, PathBuf)> {
    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("creating log directory {}", log_dir.display()))?;

    // `Builder` rather than `rolling::daily`, which names files
    // `the-editor.log.2026-08-08` -- a dot in the middle and no extension at
    // the end, so Windows has no idea what to open it with and the user is
    // asked to choose an application every time. `the-editor_2026-08-08.log`
    // opens in a text editor by double-clicking.
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("the-editor")
        .filename_suffix("log")
        .build(log_dir)
        .with_context(|| format!("opening the log in {}", log_dir.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "warn,\
             the_editor=info,\
             editor_core=info,\
             editor_syntax=info,\
             editor_lsp=info,\
             editor_proc=info,\
             editor_search=info,\
             editor_config=info,\
             editor_widgets=info",
        )
    });

    let file_layer = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true);

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_target(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("installing the tracing subscriber: {e}"))?;

    Ok((LogGuard(guard), log_dir.to_path_buf()))
}
