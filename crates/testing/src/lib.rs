//! Running tests, and reading what the test runner said.
//!
//! Two frameworks, because the editor supports two languages: pytest for Python
//! and `cargo test` for Rust. Both are *run*, not reimplemented — the same
//! choice the packages panel and the git support made, and for the same reason.
//! Your `pytest.ini`, your `conftest.py`, your fixtures and your `[profile.test]`
//! all apply because it is your own runner doing the work.
//!
//! Results are read from the runner's own output as it arrives rather than from
//! a report file written at the end. That is the less robust way round and it
//! is chosen deliberately: a suite that takes two minutes should fill the panel
//! in as it goes. See [`pytest`] and [`libtest`] for what each format looks like
//! and why scraping it is defensible.

pub mod discover;
pub mod libtest;
pub mod pytest;
pub mod report;

use std::path::{Path, PathBuf};

use editor_proc::run_config::{RunConfig, RunError};
use report::Report;

/// Which test runner a project uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    /// `pytest`, run as `python -m pytest` so it is the *project's* pytest and
    /// not whichever one happens to be first on `PATH`.
    Pytest,
    /// `cargo test`.
    CargoTest,
}

impl Framework {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pytest => "pytest",
            Self::CargoTest => "cargo test",
        }
    }
}

/// How much to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Everything the framework can find.
    All,
    /// Everything in one file.
    File(PathBuf),
    /// Named tests, by the ids a [`Report`] gave back.
    These(Vec<String>),
}

impl Scope {
    /// What to put in the console header and the panel's title.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::All => "all tests".to_owned(),
            Self::File(path) => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                format!("tests in {name}")
            }
            Self::These(ids) if ids.len() == 1 => ids[0].clone(),
            Self::These(ids) => format!("{} tests", ids.len()),
        }
    }
}

/// Build the command that runs `scope` under `framework`.
///
/// # Errors
/// If Python is needed and there is none.
pub fn command(
    framework: Framework,
    scope: &Scope,
    cwd: &Path,
    interpreter: Option<&Path>,
) -> Result<RunConfig, RunError> {
    match framework {
        Framework::Pytest => pytest_command(scope, cwd, interpreter),
        Framework::CargoTest => Ok(cargo_command(scope, cwd)),
    }
}

fn pytest_command(
    scope: &Scope,
    cwd: &Path,
    interpreter: Option<&Path>,
) -> Result<RunConfig, RunError> {
    let python = interpreter.ok_or(RunError::NoInterpreter)?;

    // `-m pytest` rather than the `pytest` on `PATH`: in a project with a
    // virtual environment those are different programs, and the one that can
    // import the project's code is this one.
    let mut args = vec![
        "-m".to_owned(),
        "pytest".to_owned(),
        // One line per test, which is what the parser reads.
        "-v".to_owned(),
        // And *only* one line per test. pytest's default progress display
        // writes a percentage at the right-hand end of the line and moves the
        // cursor about to keep it there, which runs the banner that follows
        // onto the same line as `[100%]` — and a banner that is not alone on
        // its line is a banner the parser walks straight past. `classic` is
        // the style pytest had before the progress display existed.
        "-o".to_owned(),
        "console_output_style=classic".to_owned(),
        // Otherwise a failure part-way stops the run and the rest of the tests
        // are neither passed nor failed but simply absent.
        "--continue-on-collection-errors".to_owned(),
    ];
    match scope {
        Scope::All => {}
        Scope::File(path) => args.push(display(path)),
        Scope::These(ids) => args.extend(ids.iter().cloned()),
    }

    Ok(RunConfig {
        label: format!("pytest: {}", scope.label()),
        program: python.to_path_buf(),
        args,
        cwd: cwd.to_path_buf(),
        // Unbuffered, so results reach the panel as each test finishes rather
        // than in one burst when the pipe buffer fills.
        env: vec![("PYTHONUNBUFFERED".to_owned(), "1".to_owned())],
    })
}

fn cargo_command(scope: &Scope, cwd: &Path) -> RunConfig {
    let mut args = vec!["test".to_owned()];

    match scope {
        // A file does not name a cargo target, and guessing which one it
        // belongs to from its path gets integration tests and examples wrong.
        // Running everything is the honest answer, and cargo's own caching
        // makes it cheaper than it sounds.
        Scope::All | Scope::File(_) => {}
        Scope::These(ids) => {
            // libtest filters by substring, and one filter is all it takes.
            // More than one test means running the lot and letting the panel
            // show what happened, which is what "re-run the failures" wants
            // anyway when the failures are spread across targets.
            if let [only] = ids.as_slice() {
                // The id carries the target in front of the test's own path;
                // libtest has never heard of the target.
                let name = only
                    .split_once("::")
                    .map_or(only.as_str(), |(_, rest)| rest);
                args.push("--".to_owned());
                args.push(name.to_owned());
                args.push("--exact".to_owned());
            }
        }
    }

    RunConfig {
        label: format!("cargo test: {}", scope.label()),
        program: PathBuf::from("cargo"),
        args,
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
    }
}

/// Which framework suits a file, if either does.
#[must_use]
pub fn framework_for(language: editor_syntax::LanguageId) -> Option<Framework> {
    match language {
        editor_syntax::LanguageId::Python => Some(Framework::Pytest),
        editor_syntax::LanguageId::Rust => Some(Framework::CargoTest),
        _ => None,
    }
}

/// A run in progress: the parser, its half-finished line, and the report.
///
/// Bytes arrive from a PTY on arbitrary boundaries — a line, an escape sequence
/// and a multi-byte character can all be torn in half between two reads — so
/// this buffers until it has a whole line before showing it to a parser that
/// only understands whole lines.
#[derive(Debug)]
pub struct Session {
    parser: Which,
    pending: String,
    pub report: Report,
    pub framework: Framework,
    pub scope: Scope,
}

#[derive(Debug)]
enum Which {
    Pytest(pytest::Parser),
    CargoTest(libtest::Parser),
}

impl Session {
    #[must_use]
    pub fn new(framework: Framework, scope: Scope) -> Self {
        Self {
            parser: match framework {
                Framework::Pytest => Which::Pytest(pytest::Parser::default()),
                Framework::CargoTest => Which::CargoTest(libtest::Parser::default()),
            },
            pending: String::new(),
            report: Report::default(),
            framework,
            scope,
        }
    }

    /// Feed output from the runner.
    ///
    /// Returns true when the report changed, so the caller can repaint without
    /// doing so on every byte of a chatty build.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        // Lossy on purpose: a test that prints half a character should not stop
        // the run being readable, and the replacement character is visible.
        self.pending.push_str(&String::from_utf8_lossy(bytes));

        let before = self.report.cases.len();
        let mut changed = false;
        while let Some(end) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=end).collect();
            let line = editor_proc::ansi::strip(line.trim_end_matches(['\n', '\r']));
            self.line(&line);
            changed = true;
        }
        changed || self.report.cases.len() != before
    }

    /// The process has gone.
    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let last = editor_proc::ansi::strip(&std::mem::take(&mut self.pending));
            self.line(&last);
        }
        match &mut self.parser {
            Which::Pytest(p) => p.finish(&mut self.report),
            Which::CargoTest(p) => p.finish(&mut self.report),
        }
    }

    fn line(&mut self, line: &str) {
        match &mut self.parser {
            Which::Pytest(p) => p.line(line, &mut self.report),
            Which::CargoTest(p) => p.line(line, &mut self.report),
        }
    }
}

/// A path as an argument: forward slashes, so it reads the same on every
/// platform and pytest is happy with it.
fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_and_rust_have_a_framework_and_nothing_else_does() {
        assert_eq!(
            framework_for(editor_syntax::LanguageId::Python),
            Some(Framework::Pytest)
        );
        assert_eq!(
            framework_for(editor_syntax::LanguageId::Rust),
            Some(Framework::CargoTest)
        );
        assert_eq!(framework_for(editor_syntax::LanguageId::Markdown), None);
    }

    #[test]
    fn pytest_needs_an_interpreter_and_says_so() {
        let outcome = command(Framework::Pytest, &Scope::All, Path::new("/project"), None);
        assert_eq!(outcome, Err(RunError::NoInterpreter));
    }

    /// `-m pytest`, not the `pytest` on `PATH`: in a project with a virtual
    /// environment those are different programs.
    #[test]
    fn pytest_runs_through_the_projects_interpreter() {
        let config = command(
            Framework::Pytest,
            &Scope::All,
            Path::new("/project"),
            Some(Path::new("/project/.venv/bin/python")),
        )
        .expect("a command");
        assert_eq!(config.program, PathBuf::from("/project/.venv/bin/python"));
        assert_eq!(&config.args[..2], ["-m", "pytest"]);
        assert!(config.args.contains(&"-v".to_owned()), "{:?}", config.args);
    }

    #[test]
    fn pytest_output_is_unbuffered_so_results_arrive_as_they_happen() {
        let config = command(
            Framework::Pytest,
            &Scope::All,
            Path::new("/project"),
            Some(Path::new("python")),
        )
        .expect("a command");
        assert!(
            config
                .env
                .iter()
                .any(|(k, v)| k == "PYTHONUNBUFFERED" && v == "1"),
            "{:?}",
            config.env
        );
    }

    #[test]
    fn a_file_scope_names_the_file() {
        let config = command(
            Framework::Pytest,
            &Scope::File(PathBuf::from("tests/test_a.py")),
            Path::new("/project"),
            Some(Path::new("python")),
        )
        .expect("a command");
        assert!(
            config.args.contains(&"tests/test_a.py".to_owned()),
            "{:?}",
            config.args
        );
    }

    #[test]
    fn named_tests_are_passed_as_node_ids() {
        let config = command(
            Framework::Pytest,
            &Scope::These(vec![
                "tests/test_a.py::test_b".to_owned(),
                "tests/test_a.py::test_c".to_owned(),
            ]),
            Path::new("/project"),
            Some(Path::new("python")),
        )
        .expect("a command");
        assert!(config.args.contains(&"tests/test_a.py::test_b".to_owned()));
        assert!(config.args.contains(&"tests/test_a.py::test_c".to_owned()));
    }

    #[test]
    fn cargo_test_runs_everything_by_default() {
        let config = command(
            Framework::CargoTest,
            &Scope::All,
            Path::new("/project"),
            None,
        )
        .expect("ok");
        assert_eq!(config.program, PathBuf::from("cargo"));
        assert_eq!(config.args, ["test"]);
    }

    /// A `.rs` file does not name a cargo target, and guessing which one it
    /// belongs to gets integration tests and examples wrong.
    #[test]
    fn cargo_test_ignores_a_file_scope_rather_than_guessing() {
        let config = command(
            Framework::CargoTest,
            &Scope::File(PathBuf::from("src/lib.rs")),
            Path::new("/project"),
            None,
        )
        .expect("ok");
        assert_eq!(config.args, ["test"]);
    }

    #[test]
    fn one_named_rust_test_is_run_exactly() {
        let config = command(
            Framework::CargoTest,
            &Scope::These(vec!["editor_vcs::module::a_test".to_owned()]),
            Path::new("/project"),
            None,
        )
        .expect("ok");
        assert_eq!(config.args, ["test", "--", "module::a_test", "--exact"]);
    }

    #[test]
    fn several_named_rust_tests_run_the_lot() {
        let config = command(
            Framework::CargoTest,
            &Scope::These(vec!["a::b".to_owned(), "a::c".to_owned()]),
            Path::new("/project"),
            None,
        )
        .expect("ok");
        assert_eq!(
            config.args,
            ["test"],
            "libtest takes one filter, so the panel sorts it out instead"
        );
    }

    #[test]
    fn a_scopes_label_says_what_is_being_run() {
        assert_eq!(Scope::All.label(), "all tests");
        assert_eq!(
            Scope::File(PathBuf::from("tests/test_a.py")).label(),
            "tests in test_a.py"
        );
        assert_eq!(Scope::These(vec!["a::b".to_owned()]).label(), "a::b");
        assert_eq!(
            Scope::These(vec!["a".to_owned(), "b".to_owned()]).label(),
            "2 tests"
        );
    }

    // ---- the streaming session ------------------------------------------

    #[test]
    fn a_session_reads_whole_lines_out_of_arbitrary_chunks() {
        let mut session = Session::new(Framework::CargoTest, Scope::All);
        // Split in the middle of a line, as a PTY read would.
        session.feed(b"test a ... ");
        assert!(session.report.is_empty(), "half a line says nothing yet");
        session.feed(b"ok\ntest b ... FAI");
        assert_eq!(session.report.cases.len(), 1);
        session.feed(b"LED\n");
        assert_eq!(session.report.cases.len(), 2);
        assert_eq!(session.report.cases[1].outcome, report::Outcome::Failed);
    }

    #[test]
    fn a_session_strips_the_colour_a_runner_adds() {
        let mut session = Session::new(Framework::Pytest, Scope::All);
        session
            .feed("tests/t.py::a \u{1b}[32mPASSED\u{1b}[0m\u{1b}[36m [100%]\u{1b}[0m\n".as_bytes());
        assert_eq!(session.report.cases.len(), 1);
        assert_eq!(session.report.cases[0].outcome, report::Outcome::Passed);
    }

    #[test]
    fn a_final_line_without_a_newline_is_still_read() {
        let mut session = Session::new(Framework::CargoTest, Scope::All);
        session.feed(b"test a ... ok");
        assert!(session.report.is_empty());
        session.finish();
        assert_eq!(session.report.cases.len(), 1);
        assert!(session.report.finished);
    }

    #[test]
    fn invalid_utf8_does_not_stop_the_run_being_read() {
        let mut session = Session::new(Framework::CargoTest, Scope::All);
        session.feed(&[b't', b'e', b's', b't', b' ', 0xff, b' ', b'.', b'.', b'.']);
        session.feed(b" ok\n");
        assert_eq!(session.report.cases.len(), 1, "the line still parsed");
    }
}
