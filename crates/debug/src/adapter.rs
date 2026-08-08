//! Finding `debugpy`.
//!
//! Not bundled and not required: without it the editor still runs Python, it
//! just cannot pause it. Missing is a fact to report with the command that
//! installs it, exactly as a missing language server is.

use std::path::Path;

/// The pip command that installs the adapter, for the message shown when it is
/// missing.
pub const INSTALL: &str = "pip install debugpy";

/// Whether this interpreter can run `debugpy`.
///
/// Asks the interpreter itself rather than looking for a file. `debugpy` is a
/// module, so where it lives depends on the interpreter, and a virtual
/// environment's copy must win over one installed globally — which is exactly
/// what `-m` resolves.
#[must_use]
pub fn is_available(interpreter: &Path) -> bool {
    editor_proc::spawn::quiet(interpreter)
        .args(["-c", "import debugpy"])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success())
}

/// The arguments that start the adapter on stdio.
#[must_use]
pub fn adapter_args() -> &'static [&'static str] {
    // `-Xfrozen_modules=off` because debugpy warns, on every start, that frozen
    // modules may make it miss breakpoints. It is right, and the warning is
    // noise once the flag is passed.
    &["-Xfrozen_modules=off", "-m", "debugpy.adapter"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapter_runs_on_stdio_with_frozen_modules_off() {
        // Without the flag debugpy prints a warning about missing breakpoints
        // on every single start, and it is not wrong.
        let args = adapter_args();
        assert!(args.contains(&"-m"));
        assert!(args.contains(&"debugpy.adapter"));
        assert!(args.iter().any(|a| a.contains("frozen_modules")));
    }

    #[test]
    fn a_nonexistent_interpreter_is_simply_not_available() {
        assert!(!is_available(Path::new("definitely-not-python-xyzzy")));
    }

    #[test]
    fn the_install_command_is_the_one_a_user_can_paste() {
        assert_eq!(INSTALL, "pip install debugpy");
    }
}
