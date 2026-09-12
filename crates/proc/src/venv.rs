//! Creating Python virtual environments, and finding the interpreters that can
//! create them.
//!
//! Creation is deliberately three plain commands run in the visible console
//! rather than anything clever:
//!
//! ```text
//! <base> -m venv <target>
//! <venv-python> -m pip install --upgrade pip
//! <venv-python> -m pip install -r requirements.txt
//! ```
//!
//! The user sees exactly what ran and, when something fails, gets pip's actual
//! error rather than a summary of it.

use std::path::{Path, PathBuf};

use crate::interpreter::{self, venv_python};
use crate::run_config::RunConfig;

/// A Python installation offered as the base for a new environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub path: PathBuf,
    /// `3.14.6`, or empty if it could not be asked.
    pub version: String,
}

impl Discovered {
    /// `Python 3.14.6 — C:\Python\Python314\python.exe`
    #[must_use]
    pub fn label(&self) -> String {
        if self.version.is_empty() {
            self.path.display().to_string()
        } else {
            format!("Python {} \u{2014} {}", self.version, self.path.display())
        }
    }
}

/// Every Python installation that can be found, newest first.
///
/// Looks beyond `PATH`, because the interpreter someone wants to build an
/// environment from is often not the one `PATH` happens to point at: the `py`
/// launcher knows about every registered install on Windows, the Python Install
/// Manager keeps its runtimes under `%LocalAppData%`, and pyenv keeps its
/// versions well out of the way.
///
/// Each candidate is verified by actually running it, which is what filters out
/// the Microsoft Store stubs and any stale entry left behind by an uninstall.
#[must_use]
pub fn discover() -> Vec<Discovered> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if cfg!(windows) {
        candidates.extend(py_launcher_installs());
        // The Python Install Manager's own directories. Its runtimes are
        // registered with `py`, so the launcher above usually lists them too --
        // but only when `py` itself resolves, and on a machine where the
        // manager's aliases are switched off it does not.
        candidates.extend(interpreter::install_manager_pythons());
        candidates.extend(glob_dirs(&[
            r"C:\Python",
            r"C:\Program Files\Python",
            r"C:\Program Files (x86)\Python",
        ]));
        if let Some(home) = home_dir() {
            candidates.extend(glob_dirs(&[home
                .join(r"AppData\Local\Programs\Python")
                .to_string_lossy()
                .as_ref()]));
        }
    } else {
        for dir in ["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
            for name in [
                "python3",
                "python3.14",
                "python3.13",
                "python3.12",
                "python3.11",
            ] {
                let path = Path::new(dir).join(name);
                if path.is_file() {
                    candidates.push(path);
                }
            }
        }
        if let Some(home) = home_dir() {
            candidates.extend(pyenv_versions(&home.join(".pyenv").join("versions")));
        }
    }

    for name in ["python3", "python"] {
        if let Some(path) = interpreter::which(name) {
            candidates.push(path);
        }
    }

    // Verify, de-duplicate, and put the newest first.
    let mut found: Vec<Discovered> = Vec::new();
    for path in candidates {
        if interpreter::is_store_stub(&path) {
            continue;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if found
            .iter()
            .any(|d| d.path.canonicalize().unwrap_or_else(|_| d.path.clone()) == canonical)
        {
            continue;
        }
        if let Ok(version) = interpreter::version_of(&path) {
            found.push(Discovered { path, version });
        }
    }

    found.sort_by_key(|d| std::cmp::Reverse(version_key(&d.version)));
    found
}

/// Sort key for a dotted version, so 3.10 sorts above 3.9.
fn version_key(version: &str) -> (u32, u32, u32) {
    let mut parts = version.split('.').map(|p| p.parse().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Ask the Windows `py` launcher what it knows about.
fn py_launcher_installs() -> Vec<PathBuf> {
    let Ok(output) = crate::spawn::quiet("py").arg("-0p").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    // Lines look like `  -V:3.14 *        C:\Python\Python314\python.exe`.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let path = line.split_whitespace().last()?;
            path.to_lowercase()
                .ends_with("python.exe")
                .then(|| PathBuf::from(path))
        })
        .collect()
}

/// `python.exe` directly inside any subdirectory of the given directories.
fn glob_dirs(parents: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for parent in parents {
        let Ok(entries) = std::fs::read_dir(parent) else {
            continue;
        };
        for entry in entries.flatten() {
            let exe = entry.path().join("python.exe");
            if exe.is_file() {
                out.push(exe);
            }
        }
    }
    out
}

fn pyenv_versions(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path().join("bin").join("python"))
        .filter(|p| p.is_file())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// What the dialog asked for.
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// The interpreter to build the environment from.
    pub base: PathBuf,
    /// Where the environment goes, e.g. `<project>/.venv`.
    pub target: PathBuf,
    pub upgrade_pip: bool,
    /// A `requirements.txt` to install from, if the user asked and it exists.
    pub requirements: Option<PathBuf>,
    pub system_site_packages: bool,
}

/// Why an environment cannot be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    /// The target exists and is not empty.
    TargetExists(PathBuf),
    /// The base interpreter is not a file.
    NoBase(PathBuf),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetExists(path) => {
                write!(f, "{} already exists and is not empty", path.display())
            }
            Self::NoBase(path) => write!(f, "{} is not a Python interpreter", path.display()),
        }
    }
}

impl std::error::Error for CreateError {}

/// Build the sequence of commands that creates the environment.
///
/// Returned rather than run, so the caller can put them through the same
/// console as everything else and the user watches them happen.
///
/// # Errors
/// If the target already exists or the base interpreter is missing.
pub fn create_commands(options: &CreateOptions) -> Result<Vec<RunConfig>, CreateError> {
    if !options.base.is_file() {
        return Err(CreateError::NoBase(options.base.clone()));
    }
    if is_non_empty_dir(&options.target) {
        return Err(CreateError::TargetExists(options.target.clone()));
    }

    let parent = options
        .target
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

    let mut args = vec!["-m".to_owned(), "venv".to_owned()];
    if options.system_site_packages {
        args.push("--system-site-packages".to_owned());
    }
    args.push(options.target.display().to_string());

    let mut commands = vec![RunConfig {
        label: "Create virtual environment".to_owned(),
        program: options.base.clone(),
        args,
        cwd: parent.clone(),
        env: Vec::new(),
    }];

    let python = venv_python(&options.target);

    if options.upgrade_pip {
        commands.push(RunConfig {
            label: "Upgrade pip".to_owned(),
            program: python.clone(),
            args: vec![
                "-m".to_owned(),
                "pip".to_owned(),
                "install".to_owned(),
                "--upgrade".to_owned(),
                "pip".to_owned(),
            ],
            cwd: parent.clone(),
            env: Vec::new(),
        });
    }

    if let Some(requirements) = &options.requirements {
        commands.push(RunConfig {
            label: "Install requirements".to_owned(),
            program: python,
            args: vec![
                "-m".to_owned(),
                "pip".to_owned(),
                "install".to_owned(),
                "-r".to_owned(),
                requirements.display().to_string(),
            ],
            cwd: parent,
            env: Vec::new(),
        });
    }

    Ok(commands)
}

fn is_non_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

/// Add a line to `.gitignore` if it is not already there.
///
/// # Errors
/// If the file cannot be read or written.
pub fn add_to_gitignore(project_root: &Path, entry: &str) -> std::io::Result<bool> {
    let path = project_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    if existing
        .lines()
        .any(|line| line.trim().trim_end_matches('/') == entry.trim_end_matches('/'))
    {
        return Ok(false);
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(entry);
    updated.push('\n');
    std::fs::write(&path, updated)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(base: PathBuf, target: PathBuf) -> CreateOptions {
        CreateOptions {
            base,
            target,
            upgrade_pip: false,
            requirements: None,
            system_site_packages: false,
        }
    }

    /// A file that exists and can stand in for an interpreter.
    fn stub_interpreter(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).expect("create dir");
        let path = dir.join("python-stub");
        std::fs::write(&path, b"stub").expect("write stub");
        path
    }

    #[test]
    fn the_first_command_creates_the_environment() {
        let dir = std::env::temp_dir().join("the-editor-venv-cmds");
        let base = stub_interpreter(&dir);
        let target = dir.join(".venv");

        let commands = create_commands(&options(base.clone(), target.clone())).expect("builds");
        assert_eq!(commands.len(), 1, "only creation was asked for");
        assert_eq!(commands[0].program, base);
        assert_eq!(commands[0].args[0], "-m");
        assert_eq!(commands[0].args[1], "venv");
        assert!(commands[0].args.last().expect("target").contains(".venv"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn optional_steps_use_the_new_environments_python_not_the_base() {
        // Running `pip install` with the base interpreter would install into
        // the system Python, which is the exact mistake a venv exists to
        // prevent.
        let dir = std::env::temp_dir().join("the-editor-venv-steps");
        let base = stub_interpreter(&dir);
        let target = dir.join(".venv");
        let requirements = dir.join("requirements.txt");

        let commands = create_commands(&CreateOptions {
            upgrade_pip: true,
            requirements: Some(requirements),
            ..options(base.clone(), target.clone())
        })
        .expect("builds");

        assert_eq!(commands.len(), 3);
        let expected = venv_python(&target);
        assert_eq!(commands[1].program, expected, "pip upgrade");
        assert_eq!(commands[2].program, expected, "requirements install");
        assert_ne!(commands[1].program, base);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn system_site_packages_is_passed_through_when_asked() {
        let dir = std::env::temp_dir().join("the-editor-venv-ssp");
        let base = stub_interpreter(&dir);

        let without = create_commands(&options(base.clone(), dir.join("a"))).expect("builds");
        assert!(!without[0].args.iter().any(|a| a.contains("system-site")));

        let with = create_commands(&CreateOptions {
            system_site_packages: true,
            ..options(base, dir.join("b"))
        })
        .expect("builds");
        assert!(with[0].args.iter().any(|a| a == "--system-site-packages"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_base_interpreter_is_refused() {
        let error = create_commands(&options(
            PathBuf::from("/nonexistent/python"),
            PathBuf::from("/tmp/x"),
        ))
        .expect_err("should refuse");
        assert!(matches!(error, CreateError::NoBase(_)));
        assert!(error.to_string().contains("not a Python interpreter"));
    }

    #[test]
    fn an_existing_non_empty_target_is_refused() {
        let dir = std::env::temp_dir().join("the-editor-venv-exists");
        let base = stub_interpreter(&dir);
        let target = dir.join("occupied");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("something"), b"x").expect("occupy it");

        let error = create_commands(&options(base, target)).expect_err("should refuse");
        assert!(matches!(error, CreateError::TargetExists(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_target_directory_is_allowed() {
        // `python -m venv` is happy to populate an empty directory, and
        // refusing would be needlessly strict.
        let dir = std::env::temp_dir().join("the-editor-venv-empty");
        let base = stub_interpreter(&dir);
        let target = dir.join("empty");
        std::fs::create_dir_all(&target).expect("create target");

        assert!(create_commands(&options(base, target)).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gitignore_gains_the_entry_once() {
        let dir = std::env::temp_dir().join("the-editor-gitignore");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::remove_file(dir.join(".gitignore")).ok();

        assert_eq!(add_to_gitignore(&dir, ".venv/").ok(), Some(true));
        assert_eq!(
            add_to_gitignore(&dir, ".venv/").ok(),
            Some(false),
            "a second call must not duplicate the entry"
        );

        let contents = std::fs::read_to_string(dir.join(".gitignore")).expect("read");
        assert_eq!(contents.matches(".venv").count(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gitignore_recognises_the_entry_with_or_without_a_trailing_slash() {
        let dir = std::env::temp_dir().join("the-editor-gitignore-slash");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join(".gitignore"), b"target\n.venv\n").expect("seed");

        assert_eq!(
            add_to_gitignore(&dir, ".venv/").ok(),
            Some(false),
            "`.venv` and `.venv/` mean the same thing"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gitignore_keeps_what_was_already_there() {
        let dir = std::env::temp_dir().join("the-editor-gitignore-keep");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join(".gitignore"), b"# notes\ntarget\n").expect("seed");

        add_to_gitignore(&dir, ".venv/").expect("append");
        let contents = std::fs::read_to_string(dir.join(".gitignore")).expect("read");
        assert!(contents.contains("# notes"));
        assert!(contents.contains("target"));
        assert!(contents.contains(".venv/"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gitignore_without_a_trailing_newline_does_not_glue_lines_together() {
        let dir = std::env::temp_dir().join("the-editor-gitignore-newline");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join(".gitignore"), b"target").expect("seed without newline");

        add_to_gitignore(&dir, ".venv/").expect("append");
        let contents = std::fs::read_to_string(dir.join(".gitignore")).expect("read");
        assert!(contents.contains("target\n.venv/"), "got {contents:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn versions_sort_numerically_not_lexically() {
        assert!(
            version_key("3.10.0") > version_key("3.9.9"),
            "3.10 is newer than 3.9, though it sorts earlier as text"
        );
        assert!(version_key("3.14.6") > version_key("3.14.5"));
        assert_eq!(version_key("nonsense"), (0, 0, 0));
    }

    #[test]
    fn discovery_finds_only_interpreters_that_actually_run() {
        // Skipped where no Python is installed at all.
        let found = discover();
        for candidate in &found {
            assert!(
                !candidate.version.is_empty(),
                "{} was offered without a version",
                candidate.path.display()
            );
            assert!(
                !interpreter::is_store_stub(&candidate.path),
                "a Store stub was offered: {}",
                candidate.path.display()
            );
        }
        // Newest first.
        for pair in found.windows(2) {
            assert!(version_key(&pair[0].version) >= version_key(&pair[1].version));
        }
    }

    #[test]
    fn discovery_does_not_offer_the_same_interpreter_twice() {
        let found = discover();
        let mut seen = std::collections::HashSet::new();
        for candidate in &found {
            let key = candidate
                .path
                .canonicalize()
                .unwrap_or_else(|_| candidate.path.clone());
            assert!(
                seen.insert(key),
                "{} appears twice",
                candidate.path.display()
            );
        }
    }

    #[test]
    fn a_discovered_label_reads_usefully() {
        let d = Discovered {
            path: PathBuf::from("/usr/bin/python3"),
            version: "3.14.6".to_owned(),
        };
        assert_eq!(d.label(), "Python 3.14.6 \u{2014} /usr/bin/python3");

        let unknown = Discovered {
            path: PathBuf::from("/x/python"),
            version: String::new(),
        };
        assert_eq!(unknown.label(), "/x/python");
    }
}
