//! What the project's surroundings say, remembered between frames.
//!
//! Which Python runs the code, which virtual environment's tools come first,
//! whether there is a `requirements.txt`, and what `.editorconfig` says about
//! each file. Every one of those is an answer from the filesystem, and every
//! one was being asked for again on every frame: the interpreter search alone
//! walks each `PATH` entry for each `PATHEXT` suffix, which on an ordinary
//! Windows machine is a few hundred `stat` calls, sixty times a second while
//! typing, whether or not anything used the answer.
//!
//! The answers are kept here and thrown away when something could have changed
//! them: the file watcher reporting a change in the project, the settings
//! naming a different interpreter, a different folder being opened — and, for
//! what no watcher sees, such as a Python installed elsewhere onto `PATH`, a
//! few seconds going by.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use editor_config::editorconfig::{self, FileStyle};
use editor_proc::interpreter::{self, Interpreter};

/// Why a version was not asked for.
pub(crate) const NOT_TRUSTED: &str = "not run, because this folder is not trusted";

/// How long an answer is trusted with nothing to say it changed.
const FRESH_FOR: Duration = Duration::from_secs(5);

/// The project's surroundings, as last looked up.
#[derive(Debug, Default)]
pub(crate) struct Environment {
    /// The configured interpreter and project root the answers below are for.
    key: Option<(String, Option<PathBuf>)>,
    looked_at: Option<Instant>,
    interpreter: Option<Interpreter>,
    /// The project venv's executables directory, searched first for tools.
    venv_bin: Option<PathBuf>,
    requirements: Option<PathBuf>,
    /// `.editorconfig`'s answer per file, with when it was looked up.
    editorconfig: HashMap<PathBuf, (FileStyle, Instant)>,
    /// Versions found by running a program, keyed by the program and the
    /// directory it was run in.
    versions: HashMap<(PathBuf, Option<PathBuf>), Version>,
    /// Whether the project may run its own tools. Until it may, its virtual
    /// environment is not searched for them and nothing in it is run, not
    /// even to ask its version; see `editor_config::trust`.
    trusted: bool,
}

/// A version being asked for, or the answer.
#[derive(Debug)]
enum Version {
    Asking(Receiver<Result<String, String>>),
    Known(Result<String, String>),
}

impl Environment {
    /// Say whether the project may run its own tools.
    pub(crate) fn set_trusted(&mut self, trusted: bool) {
        self.trusted = trusted;
    }

    /// Forget everything, so the next question looks again.
    pub(crate) fn invalidate(&mut self) {
        self.looked_at = None;
        self.editorconfig.clear();
        self.versions.clear();
    }

    /// React to the file watcher: anything happening in the project may have
    /// made or removed a virtual environment or a requirements file, and an
    /// `.editorconfig` that changed changes every file under it.
    pub(crate) fn changed(&mut self, touched: &[PathBuf], structural: bool) {
        if structural || !touched.is_empty() {
            self.looked_at = None;
        }
        if touched
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == ".editorconfig"))
        {
            self.editorconfig.clear();
        }
    }

    /// The interpreter that would run Python code here.
    pub(crate) fn interpreter(
        &mut self,
        configured: &str,
        root: Option<&Path>,
    ) -> Option<Interpreter> {
        self.refresh(configured, root);
        self.interpreter.clone()
    }

    /// Directories to search for tools before `PATH`: the project venv's.
    pub(crate) fn tool_search_path(
        &mut self,
        configured: &str,
        root: Option<&Path>,
    ) -> Vec<PathBuf> {
        self.refresh(configured, root);
        if !self.trusted {
            return Vec::new();
        }
        self.venv_bin.iter().cloned().collect()
    }

    /// The project's `requirements.txt`, if it has one.
    pub(crate) fn requirements(
        &mut self,
        configured: &str,
        root: Option<&Path>,
    ) -> Option<PathBuf> {
        self.refresh(configured, root);
        self.requirements.clone()
    }

    /// What `.editorconfig` says about `file`.
    pub(crate) fn style_for(&mut self, file: &Path) -> FileStyle {
        let now = Instant::now();
        if let Some((style, at)) = self.editorconfig.get(file)
            && now.duration_since(*at) < FRESH_FOR
        {
            return *style;
        }
        let style = editorconfig::style_for(file);
        self.editorconfig.insert(file.to_path_buf(), (style, now));
        style
    }

    /// The version of the Python at `interpreter`: `None` while it is being
    /// asked, which takes a process and so happens on a thread of its own.
    ///
    /// A project environment's Python is not run in a folder that is not
    /// trusted; that is reported as an error saying so.
    pub(crate) fn python_version(
        &mut self,
        interpreter: &Interpreter,
    ) -> Option<Result<String, String>> {
        if !self.trusted && matches!(interpreter.origin, interpreter::Origin::Venv(_)) {
            return Some(Err(NOT_TRUSTED.to_owned()));
        }
        let path = interpreter.path.clone();
        self.version(path.clone(), None, move || interpreter::version_of(&path))
    }

    /// The Rust toolchain version in `root`, as `rustc` reports it there — run
    /// in the project so that rustup applies its `rust-toolchain.toml`.
    pub(crate) fn rust_version(&mut self, root: Option<&Path>) -> Option<Result<String, String>> {
        if !self.trusted {
            return Some(Err(NOT_TRUSTED.to_owned()));
        }
        let Some(rustc) = interpreter::which("rustc") else {
            return Some(Err("rustc is not on PATH".to_owned()));
        };
        let cwd = root.map(Path::to_path_buf);
        let dir = cwd.clone();
        self.version(rustc.clone(), cwd, move || {
            let mut command = editor_proc::spawn::quiet(&rustc);
            command.arg("--version");
            if let Some(dir) = &dir {
                command.current_dir(dir);
            }
            let output = command.output().map_err(|e| e.to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .next()
                    .unwrap_or("rustc failed")
                    .to_owned());
            }
            // `rustc 1.97.1 (a1b2c3d4 2026-08-01)`
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .nth(1)
                .map(str::to_owned)
                .ok_or_else(|| "rustc said nothing".to_owned())
        })
    }

    /// True while a version is being asked for, so the caller knows to keep
    /// asking for frames until it arrives.
    pub(crate) fn is_asking(&self) -> bool {
        self.versions
            .values()
            .any(|v| matches!(v, Version::Asking(_)))
    }

    fn version(
        &mut self,
        program: PathBuf,
        cwd: Option<PathBuf>,
        ask: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) -> Option<Result<String, String>> {
        let key = (program, cwd);
        match self.versions.get(&key) {
            Some(Version::Known(answer)) => return Some(answer.clone()),
            Some(Version::Asking(rx)) => {
                let answer = match rx.try_recv() {
                    Ok(answer) => answer,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Err("no answer".to_owned()),
                };
                self.versions.insert(key, Version::Known(answer.clone()));
                return Some(answer);
            }
            None => {}
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = std::thread::Builder::new()
            .name("version".to_owned())
            .spawn(move || {
                let _ = tx.send(ask());
            });
        let entry = match asked {
            Ok(_) => Version::Asking(rx),
            Err(e) => Version::Known(Err(e.to_string())),
        };
        self.versions.insert(key, entry);
        None
    }

    fn refresh(&mut self, configured: &str, root: Option<&Path>) {
        let key = (configured.to_owned(), root.map(Path::to_path_buf));
        let fresh = self.key.as_ref() == Some(&key)
            && self.looked_at.is_some_and(|at| at.elapsed() < FRESH_FOR);
        if fresh {
            return;
        }
        self.interpreter = interpreter::resolve(configured, root);
        self.venv_bin = root
            .and_then(interpreter::find_venv)
            .and_then(|venv| venv.path.parent().map(Path::to_path_buf));
        self.requirements = crate::packages_panel::requirements_file(root);
        self.key = Some(key);
        self.looked_at = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("the-editor-env-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        dir
    }

    #[test]
    fn an_answer_is_kept_until_something_says_it_changed() {
        let root = project("kept");
        let mut env = trusting();
        assert_eq!(env.requirements("", Some(&root)), None);

        std::fs::write(root.join("requirements.txt"), "requests\n").expect("write");
        assert_eq!(
            env.requirements("", Some(&root)),
            None,
            "still the remembered answer: nothing has said to look again"
        );

        env.changed(&[root.join("requirements.txt")], true);
        assert_eq!(
            env.requirements("", Some(&root)),
            Some(root.join("requirements.txt"))
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_different_setting_or_folder_is_a_different_question() {
        let a = project("key-a");
        let b = project("key-b");
        std::fs::write(b.join("requirements.txt"), "x\n").expect("write");
        let mut env = trusting();
        assert_eq!(env.requirements("", Some(&a)), None);
        assert!(
            env.requirements("", Some(&b)).is_some(),
            "a new folder looks again"
        );

        let configured = env.interpreter("C:/somewhere/python.exe", Some(&b));
        assert_eq!(
            configured.map(|i| i.path),
            Some(PathBuf::from("C:/somewhere/python.exe")),
            "a configured interpreter is used as given"
        );
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    fn trusting() -> Environment {
        let mut env = Environment::default();
        env.set_trusted(true);
        env
    }

    /// The bug: a repository shipping `.venv/Scripts/ruff.exe` had it started
    /// as a language server the moment one of its Python files was opened.
    #[test]
    fn an_untrusted_projects_environment_supplies_no_tools_and_is_not_run() {
        let root = project("untrusted");
        let bin = interpreter::venv_python(&root.join(".venv"));
        std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
        std::fs::write(&bin, "").expect("write");

        let mut env = Environment::default();
        assert!(env.tool_search_path("", Some(&root)).is_empty());
        let venv = env
            .interpreter("", Some(&root))
            .expect("the venv is still found");
        assert_eq!(
            env.python_version(&venv),
            Some(Err(NOT_TRUSTED.to_owned())),
            "asked without running it"
        );
        assert!(!env.is_asking());

        env.set_trusted(true);
        assert_eq!(
            env.tool_search_path("", Some(&root)),
            vec![bin.parent().expect("parent").to_path_buf()]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_version_is_asked_for_once_and_then_remembered() {
        let mut env = Environment::default();
        let mut runs = 0;
        let (program, cwd) = (PathBuf::from("prog"), None);
        let mut answer = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while answer.is_none() && Instant::now() < deadline {
            answer = env.version(program.clone(), cwd.clone(), || Ok("1.2.3".to_owned()));
            runs += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(answer, Some(Ok("1.2.3".to_owned())));
        assert!(runs >= 1);
        assert!(!env.is_asking());
        assert_eq!(
            env.version(program, cwd, || panic!("asked a second time")),
            Some(Ok("1.2.3".to_owned()))
        );
    }

    #[test]
    fn an_edited_editorconfig_is_read_again() {
        let root = project("editorconfig");
        let file = root.join("a.py");
        std::fs::write(
            root.join(".editorconfig"),
            "root = true\n[*]\nindent_size = 2\n",
        )
        .expect("write");
        let mut env = Environment::default();
        assert_eq!(env.style_for(&file).indent_width, Some(2));

        std::fs::write(
            root.join(".editorconfig"),
            "root = true\n[*]\nindent_size = 8\n",
        )
        .expect("write");
        env.changed(&[root.join(".editorconfig")], false);
        assert_eq!(env.style_for(&file).indent_width, Some(8));
        std::fs::remove_dir_all(&root).ok();
    }
}
