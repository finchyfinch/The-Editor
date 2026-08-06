//! Crash handling: log the panic, try to save the user's unsaved work, tell
//! them where the report went.
//!
//! An editor that loses a buffer on a crash is not trustworthy, so the
//! emergency save is wired up before the window is even created. At M0 there
//! is nothing registered to save; M2 registers the open documents. The
//! mechanism exists first so that no future change has to remember to add it.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Writes every dirty buffer into the given directory and returns how many
/// were written. Must not allocate unboundedly or block — it runs inside a
/// panic hook, possibly on a thread that is already in a bad state.
type EmergencySave = Box<dyn Fn(&Path) -> usize + Send + Sync + 'static>;

static EMERGENCY_SAVE: OnceLock<EmergencySave> = OnceLock::new();
static CRASH_DIRS: OnceLock<CrashDirs> = OnceLock::new();

#[derive(Debug, Clone)]
struct CrashDirs {
    backups: PathBuf,
    logs: PathBuf,
}

/// Install the panic hook. Call once, as early in `main` as possible.
pub(crate) fn install(backup_dir: PathBuf, log_dir: PathBuf) {
    let _ = CRASH_DIRS.set(CrashDirs {
        backups: backup_dir,
        logs: log_dir,
    });

    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Whatever else happens, the default hook's output must still reach
        // stderr, so run ours first and defensively.
        let saved = save_what_we_can();

        let location = info
            .location()
            .map_or_else(|| "unknown location".to_owned(), ToString::to_string);
        let message = payload_message(info);
        let backtrace = std::backtrace::Backtrace::force_capture();

        tracing::error!(
            target: "panic",
            %location,
            %message,
            recovered_buffers = saved,
            "The Editor panicked\n{backtrace}"
        );

        if let Some(dirs) = CRASH_DIRS.get() {
            eprintln!(
                "\nThe Editor crashed. Logs: {}\nRecovered unsaved buffers: {saved} (in {})",
                dirs.logs.display(),
                dirs.backups.display()
            );
        }

        previous(info);
    }));
}

/// Register the callback that flushes dirty buffers to the backup directory.
///
/// Returns `false` if a callback was already registered, which would be a
/// programming error rather than a runtime condition.
// Wired up in M2, when there are Documents to save. Declared now so the panic
// hook above is complete rather than something that has to be remembered later.
#[allow(dead_code)]
pub(crate) fn register_emergency_save<F>(f: F) -> bool
where
    F: Fn(&Path) -> usize + Send + Sync + 'static,
{
    EMERGENCY_SAVE.set(Box::new(f)).is_ok()
}

fn save_what_we_can() -> usize {
    let (Some(save), Some(dirs)) = (EMERGENCY_SAVE.get(), CRASH_DIRS.get()) else {
        return 0;
    };
    if std::fs::create_dir_all(&dirs.backups).is_err() {
        return 0;
    }
    // A panic inside a panic hook aborts the process immediately, taking the
    // user's work with it. Contain it.
    panic::catch_unwind(AssertUnwindSafe(|| save(&dirs.backups))).unwrap_or(0)
}

fn payload_message(info: &panic::PanicHookInfo<'_>) -> String {
    let p = info.payload();
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}
