//! How The Editor starts a child process it does not want you to see.
//!
//! A release build is a GUI application with no console of its own
//! (`windows_subsystem = "windows"`). When such a process starts a
//! console-subsystem child — `ruff server`, `basedpyright-langserver`,
//! `python --version`, `taskkill` — Windows helpfully allocates a console
//! window for it. That window is not the child's output going anywhere useful;
//! it is a black rectangle that appears behind the editor and, for a long-lived
//! language server, stays there for the rest of the session.
//!
//! Every background spawn therefore goes through [`quiet`]. The exception is the
//! run console, which deliberately runs programs under a pseudo-terminal so the
//! user *can* see them; that path does not come through here.
//!
//! On every other platform this is a no-op, which is why it is a function rather
//! than a `cfg!` at each call site.

use std::process::Command;

/// Windows `CREATE_NO_WINDOW`.
///
/// Spelled out rather than pulled from `windows-sys`, which would be a
/// dependency the size of the operating system for one integer.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a command that will not flash or leave a console window behind.
///
/// Use for anything the user is not meant to watch. Prefer this over
/// `Command::new` everywhere in the workspace except the PTY runner.
#[must_use]
pub fn quiet(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    hide_console(&mut command);
    command
}

/// Apply the no-window flag to a command built elsewhere.
pub fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_command_still_runs_and_still_reports_its_output() {
        // The flag must not change what the process does, only whether a window
        // appears. Easy to get wrong by reaching for DETACHED_PROCESS, which
        // also detaches the standard handles and loses the output.
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            ("cmd", &["/C", "echo quiet_marker"])
        } else {
            ("/bin/sh", &["-c", "echo quiet_marker"])
        };
        let output = quiet(program).args(args).output().expect("spawns");
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("quiet_marker"),
            "stdout was lost: {output:?}"
        );
    }

    #[test]
    fn a_quiet_command_reports_a_failure_exit_code() {
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            ("cmd", &["/C", "exit 3"])
        } else {
            ("/bin/sh", &["-c", "exit 3"])
        };
        let status = quiet(program).args(args).status().expect("spawns");
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn hiding_the_console_on_an_existing_command_works_the_same_way() {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "echo built_elsewhere"]);
            c
        } else {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", "echo built_elsewhere"]);
            c
        };
        hide_console(&mut command);
        let output = command.output().expect("spawns");
        assert!(String::from_utf8_lossy(&output.stdout).contains("built_elsewhere"));
    }
}
