//! Finding Python interpreters.
//!
//! Resolution order for "which Python runs this file", from PLAN.md §3.8:
//!
//! 1. the path in Settings → Python → Interpreter, if set;
//! 2. a virtual environment in the project (`.venv`, `venv`, `env`);
//! 3. whatever `python` is on `PATH`;
//! 4. an installation made by the Windows Python Install Manager.
//!
//! The project venv comes second rather than first because an explicit setting
//! is an explicit instruction. It comes before `PATH` because a project with a
//! venv almost always means to use it, and silently running the system Python
//! against a venv's dependencies produces confusing import errors.
//!
//! Step 4 exists because of how the Python Install Manager -- the installer
//! python.org now recommends on Windows -- reaches `PATH`. It is an MSIX app,
//! so the `python`, `python3` and `py` commands it provides are App Execution
//! Aliases in `WindowsApps`, which is also where the Microsoft Store puts the
//! decoy `python.exe` that only advertises the Store. The two are
//! indistinguishable on disk: both are zero-byte reparse points. Everything in
//! `WindowsApps` is therefore refused (see [`is_store_stub`]), and a machine
//! whose only Python came from the install manager was left with nothing --
//! `python` worked in a terminal and the Run button did not. Rather than guess
//! which alias is real, step 4 goes looking for the interpreter the aliases
//! point at, which is an ordinary file in an ordinary directory.

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
    /// Installed by the Windows Python Install Manager, and found where it
    /// keeps things rather than through `PATH`.
    InstallManager,
}

impl Interpreter {
    /// Short label for the status bar, e.g. `.venv` or `system`.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.origin {
            Origin::Configured => "configured".to_owned(),
            Origin::Venv(name) => name.clone(),
            Origin::SystemPath => "system".to_owned(),
            Origin::InstallManager => "install manager".to_owned(),
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

    let on_path = candidates
        .iter()
        .filter_map(|name| which(name))
        .find(|path| !is_store_stub(path));
    if let Some(path) = on_path {
        return Some(Interpreter {
            path,
            origin: Origin::SystemPath,
        });
    }

    // Nothing on `PATH` that can be trusted. On Windows that is the ordinary
    // state of a machine whose Python came from the Python Install Manager:
    // the commands it publishes are aliases in `WindowsApps`, refused above,
    // and adding its own directory to `PATH` is offered during install and can
    // be declined. Look where it keeps the interpreters instead.
    install_manager_pythons()
        .into_iter()
        .next()
        .map(|path| Interpreter {
            path,
            origin: Origin::InstallManager,
        })
}

/// Every interpreter the Windows Python Install Manager provides, best first.
///
/// The manager keeps two things: a directory of *global commands* -- the
/// `python.exe`, `python3.exe` and `python3.14.exe` a terminal runs once that
/// directory is on `PATH` -- and the runtimes those commands dispatch to. Both
/// default to under `%LocalAppData%\Python` and both can be moved, so the
/// user's configuration is consulted first. See
/// <https://docs.python.org/3/using/windows.html>.
///
/// Empty off Windows, where the install manager does not exist.
#[must_use]
pub fn install_manager_pythons() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    let root = local.join("Python");
    let (global, install) = configured_dirs();
    pythons_under(
        &global.unwrap_or_else(|| root.join("bin")),
        &install.unwrap_or_else(|| root.clone()),
    )
}

/// `global_dir` and `install_dir` from the user's `pymanager.json`, if it sets
/// them.
///
/// Absent almost always: the file only exists once something has been
/// configured, and these two keys are the ones an administrator moves. A value
/// is taken literally, so a setting written with `%LocalAppData%` in it simply
/// finds nothing and the search comes up empty rather than wrong.
fn configured_dirs() -> (Option<PathBuf>, Option<PathBuf>) {
    let none = (None, None);
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return none;
    };
    let config = PathBuf::from(appdata).join("Python").join("pymanager.json");
    let Ok(text) = std::fs::read_to_string(&config) else {
        return none;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return none;
    };
    let dir = |key: &str| -> Option<PathBuf> { Some(PathBuf::from(json.get(key)?.as_str()?)) };
    (dir("global_dir"), dir("install_dir"))
}

/// The interpreters in an install manager's two directories, best first.
///
/// Split from [`install_manager_pythons`] so the layout can be tested without
/// a Python Install Manager, or a Windows, to hand.
fn pythons_under(global_dir: &Path, install_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    // The global command first. It is what `python` means in a terminal on this
    // machine, so it is the closest thing to the interpreter the user chose --
    // and it follows their default when they change it, which a path to one
    // specific runtime would not.
    if let Some(exe) = global_command(global_dir) {
        found.push(exe);
    }

    // Then the runtimes, newest first, for when there are no global commands:
    // writing them is a setting, and the directory is not always created.
    let mut runtimes: Vec<((u32, u32), PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let dir = entry.path();
            // `bin` sits inside the install root by default, and its command
            // is already the first entry.
            if dir == global_dir {
                continue;
            }
            let exe = dir.join("python.exe");
            if exe.is_file() {
                runtimes.push((tag_version(&entry.file_name().to_string_lossy()), exe));
            }
        }
    }
    runtimes.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    found.extend(runtimes.into_iter().map(|(_, exe)| exe));
    found
}

/// The `python` command in the install manager's global-commands directory.
///
/// `python.exe` when it is there. Which aliases get written is configurable, so
/// fall back to `python3.exe` and then to the highest `python3.N.exe` -- what a
/// machine that kept only the versioned commands has.
fn global_command(dir: &Path) -> Option<PathBuf> {
    for name in ["python.exe", "python3.exe"] {
        let exe = dir.join(name);
        if exe.is_file() {
            return Some(exe);
        }
    }
    let mut versioned: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let minor = name.strip_prefix("python3.")?.strip_suffix(".exe")?;
            minor.parse().ok().map(|minor| (minor, entry.path()))
        })
        .collect();
    versioned.sort_by_key(|(minor, _)| std::cmp::Reverse(*minor));
    versioned.into_iter().next().map(|(_, exe)| exe)
}

/// The version in a runtime directory name such as `pythoncore-3.14-64`.
///
/// The first `N.N` in the name, so the `-64` naming the platform is not read as
/// a patch number. A name with no version in it sorts last rather than being
/// rejected: an unrecognised layout is still an interpreter worth offering.
fn tag_version(name: &str) -> (u32, u32) {
    let Some(start) = name.find(|c: char| c.is_ascii_digit()) else {
        return (0, 0);
    };
    let digits: String = name[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = digits.split('.').map(|part| part.parse().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// True for a path that is an App Execution Alias rather than an interpreter.
///
/// The Microsoft Store's decoy `python.exe` lives in `WindowsApps` and exists
/// only to advertise the Store: running it prints "Python was not found" and
/// exits 9009. Refusing it is what stops the Run button doing nothing on a
/// machine that has a perfectly good Python under another name.
///
/// It refuses *every* alias in `WindowsApps`, including the working ones the
/// Python Install Manager publishes, because on disk they are the same thing --
/// zero-byte reparse points, identical in size, attributes and link target. The
/// only way to tell them apart is to run them, and this is called from the
/// settings form on every frame. An install manager's Python is found by
/// [`install_manager_pythons`] instead, which returns the real file the alias
/// would have dispatched to.
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

    // ---- the Windows Python Install Manager ------------------------------

    /// A throwaway directory that cleans itself up.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("the-editor-{name}"));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        /// An empty file standing in for an executable.
        fn touch(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("has a parent")).expect("create dirs");
            std::fs::write(&path, b"not really python").expect("write stub");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn the_global_command_comes_before_the_runtimes() {
        let dir = TempDir::new("pymanager-order");
        let command = dir.touch("bin/python.exe");
        let runtime = dir.touch("pythoncore-3.14-64/python.exe");

        let found = pythons_under(&dir.0.join("bin"), &dir.0);
        assert_eq!(
            found,
            vec![command, runtime],
            "the command follows the user's default; a runtime path does not"
        );
    }

    /// The case that started this: the commands directory was never written,
    /// so the only Python on the machine is the runtime itself.
    #[test]
    fn a_runtime_is_found_with_no_global_commands_at_all() {
        let dir = TempDir::new("pymanager-no-bin");
        let runtime = dir.touch("pythoncore-3.14-64/python.exe");

        assert_eq!(pythons_under(&dir.0.join("bin"), &dir.0), vec![runtime]);
    }

    #[test]
    fn runtimes_are_offered_newest_first() {
        let dir = TempDir::new("pymanager-versions");
        let old = dir.touch("pythoncore-3.9-64/python.exe");
        let new = dir.touch("pythoncore-3.14-64/python.exe");

        // 3.14 above 3.9, which sorting the names as text would get backwards.
        assert_eq!(pythons_under(&dir.0.join("bin"), &dir.0), vec![new, old]);
    }

    /// `bin` lives inside the install root by default, and its command is
    /// already the first entry.
    #[test]
    fn the_commands_directory_is_not_also_listed_as_a_runtime() {
        let dir = TempDir::new("pymanager-no-double");
        let command = dir.touch("bin/python.exe");

        assert_eq!(pythons_under(&dir.0.join("bin"), &dir.0), vec![command]);
    }

    /// Which aliases get written is a setting; a machine may have kept only the
    /// versioned ones.
    #[test]
    fn a_versioned_command_stands_in_for_a_missing_plain_one() {
        let dir = TempDir::new("pymanager-versioned");
        dir.touch("bin/python3.9.exe");
        let newest = dir.touch("bin/python3.14.exe");
        dir.touch("bin/pip.exe");

        assert_eq!(pythons_under(&dir.0.join("bin"), &dir.0), vec![newest]);
    }

    #[test]
    fn nothing_installed_finds_nothing() {
        let dir = TempDir::new("pymanager-empty");
        assert!(pythons_under(&dir.0.join("bin"), &dir.0).is_empty());
    }

    /// The plumbing from `%LocalAppData%` to the two directories.
    ///
    /// A real invariant on any machine, and the one the search rests on: point
    /// `LOCALAPPDATA` at a prepared layout and this does the whole job.
    #[test]
    fn the_search_starts_from_local_app_data() {
        let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
            return;
        };
        let command = local.join("Python").join("bin").join("python.exe");
        if !cfg!(windows) || !command.is_file() {
            return;
        }
        assert_eq!(
            install_manager_pythons().first(),
            Some(&command),
            "the global command is what the manager's directory offers first"
        );
    }

    #[test]
    fn a_runtime_directory_name_yields_its_version() {
        assert_eq!(tag_version("pythoncore-3.14-64"), (3, 14));
        assert_eq!(tag_version("pythoncore-3.9-64"), (3, 9));
        // The platform suffix is not a patch number.
        assert_ne!(tag_version("pythoncore-3.14-64"), (3, 14 + 64));
        // An unrecognised name sorts last rather than being thrown away.
        assert_eq!(tag_version("some-other-layout"), (0, 0));
    }

    /// Every alias in `WindowsApps` is refused, the install manager's working
    /// ones included -- which is why the manager's own directories are searched.
    #[test]
    fn app_execution_aliases_are_refused_on_windows() {
        let alias = PathBuf::from(r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\python.exe");
        assert_eq!(is_store_stub(&alias), cfg!(windows));
        assert!(!is_store_stub(Path::new(r"C:\Python\Python314\python.exe")));
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
