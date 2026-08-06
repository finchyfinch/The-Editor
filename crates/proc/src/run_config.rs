//! Deciding what to run.
//!
//! Running a Python file is deliberately unremarkable: take the interpreter,
//! pass it the file, set the working directory to the project root. No wrapper,
//! no generated launcher, no `-m`. The exact command is echoed to the console
//! before it runs so there is never any doubt what was executed.
//!
//! Rust is the one case that is not "run this file": a `.rs` file on its own
//! means nothing to `cargo`, so a file inside a Cargo project runs
//! `cargo run` from the manifest directory.

use std::path::{Path, PathBuf};

use crate::interpreter::Interpreter;

/// A resolved command, ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    /// What to show in the console header and the Run button's tooltip.
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Extra environment for the child, on top of the inherited environment.
    pub env: Vec<(String, String)>,
}

impl RunConfig {
    /// The command line as a user would type it, for the console header.
    #[must_use]
    pub fn command_line(&self) -> String {
        let mut parts = vec![quote(&self.program.display().to_string())];
        parts.extend(self.args.iter().map(|a| quote(a)));
        parts.join(" ")
    }
}

/// Why a file cannot be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The document has never been saved, so there is no file to run.
    Unsaved,
    /// Nothing sensible to do with this language.
    UnsupportedLanguage(String),
    /// Python is needed but none could be found.
    NoInterpreter,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsaved => write!(f, "Save the file before running it"),
            Self::UnsupportedLanguage(name) => {
                write!(f, "The Editor does not know how to run {name} files")
            }
            Self::NoInterpreter => write!(
                f,
                "No Python interpreter found. Set one in Settings, or install Python."
            ),
        }
    }
}

impl std::error::Error for RunError {}

/// What kind of thing is being run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Python,
    Cargo,
    Node,
}

/// Build the command for a Python file.
///
/// # Errors
/// If no interpreter is available.
pub fn python(
    file: &Path,
    interpreter: Option<&Interpreter>,
    project_root: Option<&Path>,
    extra_args: &[String],
) -> Result<RunConfig, RunError> {
    let interpreter = interpreter.ok_or(RunError::NoInterpreter)?;

    let mut args = vec![file.display().to_string()];
    args.extend(extra_args.iter().cloned());

    Ok(RunConfig {
        label: format!("Python ({})", interpreter.label()),
        program: interpreter.path.clone(),
        args,
        cwd: working_directory(file, project_root),
        // Unbuffered, so `print` output appears as it happens rather than in a
        // lump when the program exits. Without this a long-running script looks
        // like it has hung.
        env: vec![("PYTHONUNBUFFERED".to_owned(), "1".to_owned())],
    })
}

/// Build `cargo run` for a file inside a Cargo project.
///
/// # Errors
/// If cargo cannot be found.
pub fn cargo(manifest_dir: &Path, subcommand: &str, release: bool) -> Result<RunConfig, RunError> {
    let program = crate::interpreter::which("cargo")
        .ok_or_else(|| RunError::UnsupportedLanguage("Rust (cargo not found)".to_owned()))?;

    let mut args = vec![subcommand.to_owned()];
    if release {
        args.push("--release".to_owned());
    }
    // Colour survives the PTY, and the console renders it.
    args.push("--color=always".to_owned());

    Ok(RunConfig {
        label: format!("cargo {subcommand}"),
        program,
        args,
        cwd: manifest_dir.to_path_buf(),
        env: Vec::new(),
    })
}

/// Walk up from `file` looking for a `Cargo.toml`.
///
/// Stops at `ceiling` when given, so opening a file from outside the project
/// cannot reach a manifest somewhere up the user's home directory.
#[must_use]
pub fn find_cargo_manifest(file: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        if ceiling.is_some_and(|c| dir == c) {
            return None;
        }
        dir = dir.parent()?;
    }
}

/// Where to run from: the project root if there is one, else the file's own
/// directory.
///
/// This matters more than it looks. A script doing `open("data.txt")` resolves
/// that against the working directory, and running it from the project root is
/// what happens in a terminal.
#[must_use]
pub fn working_directory(file: &Path, project_root: Option<&Path>) -> PathBuf {
    project_root
        .map(Path::to_path_buf)
        .or_else(|| file.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Quote an argument for display if it contains whitespace.
fn quote(s: &str) -> String {
    if s.contains(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::Origin;

    fn interpreter() -> Interpreter {
        Interpreter {
            path: PathBuf::from("/usr/bin/python3"),
            origin: Origin::SystemPath,
        }
    }

    #[test]
    fn running_python_is_just_the_interpreter_and_the_file() {
        let config = python(
            Path::new("/project/src/main.py"),
            Some(&interpreter()),
            Some(Path::new("/project")),
            &[],
        )
        .expect("resolves");

        assert_eq!(config.program, PathBuf::from("/usr/bin/python3"));
        assert_eq!(config.args, ["/project/src/main.py"]);
        assert_eq!(config.cwd, PathBuf::from("/project"));
    }

    #[test]
    fn python_output_is_unbuffered_so_print_appears_as_it_happens() {
        let config =
            python(Path::new("/a/b.py"), Some(&interpreter()), None, &[]).expect("resolves");
        assert!(
            config
                .env
                .iter()
                .any(|(k, v)| k == "PYTHONUNBUFFERED" && v == "1"),
            "buffered output makes a running script look hung"
        );
    }

    #[test]
    fn extra_arguments_follow_the_file() {
        let config = python(
            Path::new("/a/b.py"),
            Some(&interpreter()),
            None,
            &["--verbose".to_owned(), "input.txt".to_owned()],
        )
        .expect("resolves");
        assert_eq!(config.args, ["/a/b.py", "--verbose", "input.txt"]);
    }

    #[test]
    fn python_without_an_interpreter_says_so_usefully() {
        let error = python(Path::new("/a/b.py"), None, None, &[]).expect_err("no interpreter");
        assert_eq!(error, RunError::NoInterpreter);
        assert!(
            error.to_string().contains("Settings"),
            "the message should say what to do: {error}"
        );
    }

    #[test]
    fn the_working_directory_is_the_project_root_when_there_is_one() {
        assert_eq!(
            working_directory(
                Path::new("/project/src/main.py"),
                Some(Path::new("/project"))
            ),
            PathBuf::from("/project"),
            "relative paths in a script must resolve as they would in a terminal"
        );
        assert_eq!(
            working_directory(Path::new("/loose/script.py"), None),
            PathBuf::from("/loose")
        );
    }

    #[test]
    fn a_cargo_manifest_is_found_by_walking_up() {
        let root = std::env::temp_dir().join("the-editor-cargo-test");
        let src = root.join("crates").join("thing").join("src");
        std::fs::create_dir_all(&src).expect("create dirs");
        std::fs::write(root.join("Cargo.toml"), b"[workspace]").expect("write manifest");

        let found = find_cargo_manifest(&src.join("lib.rs"), None);
        assert_eq!(found, Some(root.clone()));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_nearest_manifest_wins() {
        let root = std::env::temp_dir().join("the-editor-cargo-nearest");
        let inner = root.join("member");
        std::fs::create_dir_all(inner.join("src")).expect("create dirs");
        std::fs::write(root.join("Cargo.toml"), b"[workspace]").expect("outer manifest");
        std::fs::write(inner.join("Cargo.toml"), b"[package]").expect("inner manifest");

        assert_eq!(
            find_cargo_manifest(&inner.join("src").join("main.rs"), None),
            Some(inner),
            "a workspace member runs from its own manifest"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_search_for_a_manifest_stops_at_the_ceiling() {
        let root = std::env::temp_dir().join("the-editor-cargo-ceiling");
        let project = root.join("project");
        std::fs::create_dir_all(&project).expect("create dirs");
        std::fs::write(root.join("Cargo.toml"), b"[workspace]").expect("manifest above");

        assert_eq!(
            find_cargo_manifest(&project.join("main.rs"), Some(&project)),
            None,
            "a manifest outside the opened folder must not be picked up"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_manifest_yields_none_rather_than_looping() {
        assert_eq!(
            find_cargo_manifest(Path::new("/nonexistent/deep/file.rs"), None),
            None
        );
    }

    #[test]
    fn the_displayed_command_quotes_paths_with_spaces() {
        let config = RunConfig {
            label: "test".to_owned(),
            program: PathBuf::from("/Program Files/python.exe"),
            args: vec!["/my docs/a.py".to_owned(), "--flag".to_owned()],
            cwd: PathBuf::from("/"),
            env: Vec::new(),
        };
        assert_eq!(
            config.command_line(),
            "\"/Program Files/python.exe\" \"/my docs/a.py\" --flag"
        );
    }

    #[test]
    fn unsaved_and_unsupported_errors_read_as_instructions() {
        assert!(RunError::Unsaved.to_string().contains("Save"));
        assert!(
            RunError::UnsupportedLanguage("CSS".to_owned())
                .to_string()
                .contains("CSS")
        );
    }
}
