//! Finding Python interpreters.
//!
//! Resolution order for "which Python runs this file", from PLAN.md §3.8:
//!
//! 1. the path in Settings → Python → Interpreter, if set;
//! 2. a virtual environment in the project (`.venv`, `venv`, `env`);
//! 3. whatever `python` is on `PATH`.
//!
//! The project venv comes second rather than first because an explicit setting
//! is an explicit instruction. It comes before `PATH` because a project with a
//! venv almost always means to use it, and silently running the system Python
//! against a venv's dependencies produces confusing import errors.

use std::path::{Path, PathBuf};

/// Directory names checked for a project virtual environment, in order.
const VENV_DIRS: &[&str] = &[".venv", "venv", "env", ".env"];

/// A Python interpreter and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interpreter {
    pub path: PathBuf,
    pub origin: Origin,
}

/// Why this interpreter was chosen, for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Explicitly configured in settings.
    Configured,
    /// A virtual environment found in the project, named by its directory.
    Venv(String),
    /// Found on `PATH`.
    SystemPath,
}

impl Interpreter {
    /// Short label for the status bar, e.g. `.venv` or `system`.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.origin {
            Origin::Configured => "configured".to_owned(),
            Origin::Venv(name) => name.clone(),
            Origin::SystemPath => "system".to_owned(),
        }
    }
}

/// The interpreter to use for a project.
///
/// `configured` is the settings value, empty when unset. `project_root` is
/// where to look for a virtual environment.
#[must_use]
pub fn resolve(configured: &str, project_root: Option<&Path>) -> Option<Interpreter> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return Some(Interpreter {
            path: PathBuf::from(configured),
            origin: Origin::Configured,
        });
    }

    if let Some(root) = project_root
        && let Some(found) = find_venv(root)
    {
        return Some(found);
    }

    // Order matters, and differs by platform.
    //
    // On Windows, `python3.exe` on PATH is almost always the Microsoft Store
    // "App Execution Alias" — a stub that prints "Python was not found; run
    // without arguments to install from the Microsoft Store" and exits 9009,
    // even when a real Python is installed and reachable as `python`. Trying
    // `python3` first there means the Run button appears to do nothing.
    //
    // On Unix the reverse holds: `python` may be absent or Python 2, and
    // `python3` is the one that means what we want.
    let candidates: &[&str] = if cfg!(windows) {
        &["python", "python3", "py"]
    } else {
        &["python3", "python"]
    };

    candidates
        .iter()
        .filter_map(|name| which(name))
        .find(|path| !is_store_stub(path))
        .map(|path| Interpreter {
            path,
            origin: Origin::SystemPath,
        })
}

/// True for a Microsoft Store App Execution Alias.
///
/// These are zero-byte reparse points in `WindowsApps` that exist only to
/// advertise the Store. They are never a working interpreter.
#[must_use]
pub fn is_store_stub(path: &Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("WindowsApps"))
    })
}

/// Look for a virtual environment directly inside `root`.
#[must_use]
pub fn find_venv(root: &Path) -> Option<Interpreter> {
    VENV_DIRS.iter().find_map(|name| {
        let exe = venv_python(&root.join(name));
        exe.exists().then(|| Interpreter {
            path: exe,
            origin: Origin::Venv((*name).to_owned()),
        })
    })
}

/// The interpreter path inside a virtual environment directory.
///
/// Windows puts it in `Scripts\python.exe`; everything else uses `bin/python`.
#[must_use]
pub fn venv_python(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// Find an executable on `PATH`.
///
/// Hand-rolled rather than pulled from a crate: it is fifteen lines, and the
/// Windows half — trying each `PATHEXT` suffix — is the part a naive
/// implementation gets wrong.
#[must_use]
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_owned())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };

    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        for extension in &extensions {
            let with_extension = dir.join(format!("{program}{extension}"));
            if with_extension.is_file() {
                return Some(with_extension);
            }
        }
    }
    None
}

/// Ask an interpreter for its version, to display and to confirm it runs.
///
/// # Errors
/// If the interpreter cannot be executed or does not answer.
pub fn version_of(interpreter: &Path) -> Result<String, String> {
    let output = crate::spawn::quiet(interpreter)
        .args([
            "-c",
            "import sys; print('.'.join(map(str, sys.version_info[:3])))",
        ])
        .output()
        .map_err(|e| format!("{}: {e}", interpreter.display()))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .lines()
            .next()
            .unwrap_or("interpreter failed")
            .to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_interpreter_wins_over_everything() {
        let resolved = resolve("C:/custom/python.exe", Some(Path::new("/project")));
        let resolved = resolved.expect("configured path is always accepted");
        assert_eq!(resolved.path, PathBuf::from("C:/custom/python.exe"));
        assert_eq!(resolved.origin, Origin::Configured);
    }

    #[test]
    fn whitespace_only_configuration_counts_as_unset() {
        // Otherwise a stray space in settings resolves to an interpreter named
        // " ", which fails with a baffling error at run time.
        let resolved = resolve("   ", None);
        // Falls through to PATH, which may or may not have Python; either way
        // it must not be the blank "configured" value.
        assert!(resolved.is_none_or(|r| r.origin != Origin::Configured));
    }

    #[test]
    fn venv_layout_matches_the_platform() {
        let venv = venv_python(Path::new("/project/.venv"));
        if cfg!(windows) {
            assert!(venv.ends_with("Scripts/python.exe") || venv.ends_with(r"Scripts\python.exe"));
        } else {
            assert!(venv.ends_with("bin/python"));
        }
    }

    #[test]
    fn a_project_venv_is_found_and_labelled() {
        let dir = std::env::temp_dir().join("the-editor-venv-test");
        let venv = dir.join(".venv");
        let exe = venv_python(&venv);
        std::fs::create_dir_all(exe.parent().expect("has a parent")).expect("create dirs");
        std::fs::write(&exe, b"not really python").expect("write stub");

        let found = find_venv(&dir).expect("venv should be found");
        assert_eq!(found.path, exe);
        assert_eq!(found.origin, Origin::Venv(".venv".to_owned()));
        assert_eq!(found.label(), ".venv");

        // ...and it takes precedence over PATH.
        let resolved = resolve("", Some(&dir)).expect("resolves");
        assert_eq!(resolved.origin, Origin::Venv(".venv".to_owned()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_project_without_a_venv_falls_through() {
        let dir = std::env::temp_dir().join("the-editor-no-venv-test");
        std::fs::create_dir_all(&dir).expect("create dir");
        assert_eq!(find_venv(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn venv_directories_are_checked_in_a_sensible_order() {
        assert_eq!(
            VENV_DIRS.first(),
            Some(&".venv"),
            ".venv is the modern convention and should win"
        );
    }

    #[test]
    fn which_finds_something_that_certainly_exists() {
        // Every platform has a shell on PATH.
        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let found = which(program);
        assert!(found.is_some(), "{program} should be on PATH");
        assert!(found.expect("checked").is_file());
    }

    #[test]
    fn which_returns_none_for_something_that_does_not_exist() {
        assert_eq!(which("definitely-not-a-real-program-xyzzy"), None);
    }

    /// Regression: `python3.exe` on Windows PATH is normally the Microsoft
    /// Store alias stub, which prints an advert and exits 9009. Resolving to it
    /// made the Run button produce no output at all.
    #[test]
    fn microsoft_store_alias_stubs_are_recognised() {
        let stub = Path::new(r"C:\Users\someone\AppData\Local\Microsoft\WindowsApps\python3.exe");
        let real = Path::new(r"C:\Python\Python314\python.exe");

        if cfg!(windows) {
            assert!(is_store_stub(stub), "the Store alias must be rejected");
            assert!(!is_store_stub(real), "a real install must be accepted");
        } else {
            assert!(!is_store_stub(stub), "this only applies to Windows");
        }
    }

    #[test]
    fn a_resolved_interpreter_is_never_a_store_stub() {
        if let Some(found) = resolve("", None) {
            assert!(
                !is_store_stub(&found.path),
                "resolved {} which is a Store stub",
                found.path.display()
            );
        }
    }

    #[test]
    fn a_resolved_interpreter_actually_runs() {
        // The whole point of discovery is to end up with something that works.
        // Skipped where no Python is installed.
        let Some(found) = resolve("", None) else {
            return;
        };
        let version = version_of(&found.path)
            .unwrap_or_else(|e| panic!("resolved {} but it failed: {e}", found.path.display()));
        assert!(
            version.starts_with('3'),
            "expected a Python 3 version, got {version:?}"
        );
    }
}
