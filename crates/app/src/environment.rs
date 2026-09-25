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
use std::time::{Duration, Instant};

use editor_config::editorconfig::{self, FileStyle};
use editor_proc::interpreter::{self, Interpreter};

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
}

impl Environment {
    /// Forget everything, so the next question looks again.
    pub(crate) fn invalidate(&mut self) {
        self.looked_at = None;
        self.editorconfig.clear();
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
        let mut env = Environment::default();
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
        let mut env = Environment::default();
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
