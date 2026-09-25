//! Running code: the run console, the debugger, the test runner, and the
//! Python environment all of them use.

use super::*;

impl EditorApp {
    /// The interpreter the packages panel and the runner should use.
    pub(super) fn interpreter(&mut self) -> Option<editor_proc::interpreter::Interpreter> {
        self.environment
            .interpreter(&self.settings.python_interpreter(), self.tree.root())
    }

    /// Ask pip what is installed, if there is an interpreter to ask.
    pub(super) fn refresh_packages(&mut self) {
        match self.interpreter() {
            Some(interpreter) => self.packages.refresh(&interpreter.path),
            None => self.info("Select a Python interpreter first"),
        }
    }

    /// Carry out what the packages panel asked for.
    ///
    /// Installs and removals go to the console, which is where the user can
    /// read pip's own account of what happened. The listing is refreshed when
    /// that run finishes rather than immediately, because pip has not done
    /// anything yet.
    pub(super) fn apply_packages_action(&mut self, action: crate::packages_panel::Action) {
        use crate::packages_panel::Action;
        match action {
            Action::None => {}
            Action::Run(change) => {
                let Some(interpreter) = self.interpreter() else {
                    self.info("Select a Python interpreter first");
                    return;
                };
                let cwd = self
                    .tree
                    .root()
                    .map(Path::to_path_buf)
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| PathBuf::from("."));
                let config = editor_proc::packages::command(&interpreter.path, &cwd, &change);
                self.dock = DockTab::Output;
                self.show_output = true;
                if let Err(e) = self.runner.start(config, true) {
                    self.error(format!("{e:#}"));
                }
            }
            Action::Freeze => self.freeze_requirements(),
            Action::OpenRequirements => {
                if let Some(path) = crate::packages_panel::requirements_file(self.tree.root()) {
                    self.open_path(&path, false);
                }
            }
        }
    }

    /// Write `pip freeze` to the project's requirements file.
    pub(super) fn freeze_requirements(&mut self) {
        let Some(interpreter) = self.interpreter() else {
            self.info("Select a Python interpreter first");
            return;
        };
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            self.info("Open a folder first: there is nowhere to write the file");
            return;
        };
        let path = root.join("requirements.txt");
        match editor_proc::packages::freeze_to(&interpreter.path, &path) {
            Ok(count) => {
                self.tree.refresh();
                self.sync_watched_files();
                self.info(format!("Wrote {count} requirements to requirements.txt"));
            }
            Err(e) => self.error(format!("Could not freeze: {e}")),
        }
    }

    /// Open a shell in the project, if one is not already running.
    ///
    /// Started in the project root with the virtual environment's directory
    /// ahead of `PATH`, so `python` and `pip` are the project's from the first
    /// command rather than after activating something.
    pub(super) fn open_terminal(&mut self, ctx: &egui::Context) {
        if self.terminal.is_running() {
            return;
        }
        let cwd = self
            .tree
            .root()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let extra = self.tool_search_path();
        self.terminal.start(&cwd, &extra, ctx);
    }

    /// Toggle a breakpoint on the caret's line, and tell a running session.
    pub(super) fn toggle_breakpoint_at_caret(&mut self) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let line = entry.doc.line_of(entry.view.selection.head);
        self.toggle_breakpoint(line);
    }

    /// `line` is zero-based, as the editor counts; breakpoints are one-based.
    pub(super) fn toggle_breakpoint(&mut self, line: usize) {
        let Some(path) = self
            .active
            .and_then(|i| self.docs.get(i))
            .and_then(|e| e.doc.path())
            .map(Path::to_path_buf)
        else {
            self.info("Save the file before setting breakpoints");
            return;
        };
        let set = self.breakpoints.toggle(&path, line + 1);
        self.send_breakpoints(&path);

        // Said once, on the first breakpoint of a run, if nothing can act on
        // it. A dot appearing looks like it worked, and finding out that it
        // cannot be hit should not wait until Alt+F5.
        if set && !self.debugpy_warned && self.debug.is_none() {
            self.debugpy_warned = true;
            if let Some(interpreter) = editor_proc::interpreter::resolve(
                &self.settings.python_interpreter(),
                self.tree.root(),
            ) && !editor_debug::adapter::is_available(&interpreter.path)
            {
                self.info(format!(
                    "Breakpoint set, but debugging needs debugpy \u{2014} {}",
                    editor_debug::adapter::INSTALL
                ));
            }
        }
    }

    pub(super) fn send_breakpoints(&mut self, path: &Path) {
        let lines = self.breakpoints.for_file(path);
        if let Some(session) = self.debug.as_mut() {
            session.set_breakpoints(path, &lines);
        }
    }

    /// Alt+F5: start a session, or continue a paused one.
    ///
    /// One key for both because they are the same intention — "carry on" —
    /// and a debugger with separate Start and Continue keys makes you think
    /// about which state you are in before you can press anything.
    pub(super) fn debug_start_or_continue(&mut self) {
        if let Some(session) = self.debug.as_mut() {
            if session.is_paused() {
                session.resume(editor_debug::session::Step::Continue);
                self.debug_view.clear();
            }
            return;
        }

        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            self.info("Open a Python file to debug");
            return;
        };
        if entry.language != LanguageId::Python {
            self.info("The debugger is for Python only at present");
            return;
        }
        let Some(program) = entry.doc.path().map(Path::to_path_buf) else {
            self.info("Save the file before debugging it");
            return;
        };
        if entry.doc.is_dirty() {
            // Debugging a file that differs from the one on disk puts every
            // breakpoint on the wrong line, silently.
            self.info("Save the file first \u{2014} the debugger reads it from disk");
            return;
        }

        let Some(interpreter) = editor_proc::interpreter::resolve(
            &self.settings.python_interpreter(),
            self.tree.root(),
        ) else {
            self.error("No Python interpreter found");
            return;
        };
        if !editor_debug::adapter::is_available(&interpreter.path) {
            self.error(format!(
                "debugpy is not installed for {} \u{2014} {}",
                interpreter.path.display(),
                editor_debug::adapter::INSTALL
            ));
            return;
        }

        let cwd = self
            .tree
            .root()
            .map_or_else(|| program.parent().unwrap_or(Path::new(".")), |r| r)
            .to_path_buf();

        match editor_debug::Session::launch(&interpreter.path, &program, &cwd, &[]) {
            Ok(mut session) => {
                for file in self.breakpoints.files() {
                    let lines = self.breakpoints.for_file(&file);
                    session.set_breakpoints(&file, &lines);
                }
                // Echoed like a run, so the console says what is happening
                // rather than filling with output from nowhere.
                self.runner.push_banner(&format!(
                    "> debug {} ({})",
                    program.display(),
                    interpreter.path.display()
                ));
                self.debug = Some(session);
                self.debug_view.clear();
                self.dock = DockTab::Debug;
                self.show_output = true;
                self.info(format!("Debugging {}", program.display()));
            }
            Err(e) => self.error(format!("Could not start the debugger: {e:#}")),
        }
    }

    pub(super) fn debug_step(&mut self, how: editor_debug::session::Step) {
        if let Some(session) = self.debug.as_mut()
            && session.is_paused()
        {
            session.resume(how);
            self.debug_view.clear();
        }
    }

    pub(super) fn debug_stop(&mut self) {
        if let Some(session) = self.debug.as_mut() {
            session.stop();
            self.runner.push_banner("[Debugging stopped]");
        }
        self.debug = None;
        self.debug_view.clear();
    }

    /// Drain the debug session once per frame.
    pub(super) fn poll_debugger(&mut self, ctx: &egui::Context) {
        let Some(session) = self.debug.as_mut() else {
            return;
        };
        let events = session.poll();
        if events.is_empty() {
            // A paused session produces nothing until the user acts; a running
            // one is about to. Keep the frame loop turning while it lives.
            if session.is_alive() && !session.is_paused() {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
            return;
        }

        let mut jump_to = None;
        for event in events {
            match event {
                editor_debug::DebugEvent::StateChanged(state) => {
                    if state == editor_debug::State::Finished {
                        self.debug = None;
                        self.debug_view.clear();
                        self.runner.push_banner("[Debugging finished]");
                        self.info("Debugging finished");
                        return;
                    }
                }
                editor_debug::DebugEvent::Paused { reason } => {
                    self.debug_view.reason = reason;
                }
                editor_debug::DebugEvent::Stack(frames) => {
                    self.debug_view.selected = frames.first().map(|f| f.id);
                    self.debug_view.stack = frames;
                    jump_to = self.debug_view.location();
                }
                editor_debug::DebugEvent::Variables(vars) => {
                    self.debug_view.variables = vars;
                }
                editor_debug::DebugEvent::BreakpointsVerified { .. } => {}
                editor_debug::DebugEvent::Output(text) => {
                    self.runner.push_output(&text);
                }
                editor_debug::DebugEvent::Failed(message) => {
                    self.error(format!("Debugger: {message}"));
                }
            }
        }

        if let Some((path, line)) = jump_to {
            // One-based from the protocol, zero-based for the editor.
            self.open_at(&path, line.saturating_sub(1), 0);
        }
    }

    /// Kick off virtual environment creation.
    pub(super) fn create_venv(&mut self, request: &venv_dialog::Request) {
        let commands = match editor_proc::venv::create_commands(&request.options) {
            Ok(commands) => commands,
            Err(e) => {
                self.error(e.to_string());
                return;
            }
        };

        self.show_output = true;
        match self.runner.start_sequence(commands) {
            Ok(()) => {
                self.pending_venv = Some(venv_dialog::Completion::from_request(request));
            }
            Err(e) => self.error(format!("Could not create environment: {e:#}")),
        }
    }

    /// Adopt a newly created virtual environment, once its commands have run.
    pub(super) fn finish_venv(&mut self, completion: &venv_dialog::Completion, code: Option<i32>) {
        // Whatever happened, the project's surroundings may have changed.
        self.environment.invalidate();
        if code != Some(0) {
            self.error("Virtual environment was not created \u{2014} see the output");
            return;
        }
        // Trust the filesystem over the exit code: `python -m venv` can report
        // success and still leave nothing usable behind if, say, the target was
        // on a full disk.
        if !venv_dialog::interpreter_exists(&completion.interpreter) {
            self.error(format!(
                "Finished, but {} is not there",
                completion.interpreter.display()
            ));
            return;
        }

        // An environment made here, by the person at the keyboard, is theirs.
        if let Some(root) = self.tree.root().map(Path::to_path_buf)
            && !self.folder_trusted()
        {
            self.set_trust(&root, true);
        }
        if completion.set_as_project_interpreter {
            self.settings
                .set_python_interpreter(&completion.interpreter.display().to_string());
            self.persist_settings();
        }

        if completion.add_to_gitignore {
            match editor_proc::venv::add_to_gitignore(
                &completion.project_root,
                &completion.gitignore_entry(),
            ) {
                Ok(true) => tracing::info!("added {} to .gitignore", completion.gitignore_entry()),
                Ok(false) => {}
                Err(e) => self.error(format!("Could not update .gitignore: {e}")),
            }
        }

        self.tree.refresh();
        match editor_proc::interpreter::version_of(&completion.interpreter) {
            Ok(version) => self.info(format!(
                "Created {} with Python {version}",
                completion.folder_name
            )),
            Err(_) => self.info(format!("Created {}", completion.folder_name)),
        }
    }

    /// Work out what running the active document means, and do it.
    ///
    /// The whole point of the Run button is that it does the obvious thing
    /// without configuration: a Python file runs under the project's
    /// interpreter, a Rust file runs `cargo run` from its manifest directory.
    pub(super) fn run_active(&mut self, tests: bool) {
        let Some(entry) = self.active_doc() else {
            self.info("Open a file to run");
            return;
        };
        if entry.doc.is_dirty() {
            // Running stale code is a genuinely confusing way to lose an hour.
            if let Some(active) = self.active
                && !self.save_indices(&[active])
            {
                return;
            }
        }

        let Some(entry) = self.active_doc() else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            self.error(editor_proc::run_config::RunError::Unsaved.to_string());
            return;
        };
        let language = entry.language;
        let root = self.tree.root().map(Path::to_path_buf);

        let config = match language {
            LanguageId::Python => {
                let interpreter = editor_proc::interpreter::resolve(
                    &self.settings.python_interpreter(),
                    root.as_deref(),
                );
                editor_proc::run_config::python(&path, interpreter.as_ref(), root.as_deref(), &[])
            }
            LanguageId::Rust => {
                match editor_proc::run_config::find_cargo_manifest(&path, root.as_deref()) {
                    Some(manifest_dir) => editor_proc::run_config::cargo(
                        &manifest_dir,
                        if tests { "test" } else { "run" },
                        false,
                    ),
                    None => Err(editor_proc::run_config::RunError::UnsupportedLanguage(
                        "Rust outside a Cargo project".to_owned(),
                    )),
                }
            }
            other => Err(editor_proc::run_config::RunError::UnsupportedLanguage(
                other.display_name().to_owned(),
            )),
        };

        match config {
            Ok(config) => {
                self.show_output = true;
                if let Err(e) = self.runner.start(config, true) {
                    self.error(format!("Could not start: {e:#}"));
                }
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Which framework this project's tests use, and where to run it from.
    ///
    /// Decided by the *active file's* language rather than by scanning the
    /// project: a repository with Python and Rust in it has both, and the one
    /// you are looking at is the one you mean.
    pub(super) fn test_framework(&self) -> Option<(editor_testing::Framework, PathBuf)> {
        let entry = self.active_doc()?;
        let framework = editor_testing::framework_for(entry.language)?;
        let path = entry.doc.path()?;

        let cwd = match framework {
            // pytest resolves node ids against where it was started, so it must
            // start at the project root and nowhere else — otherwise every id
            // in the report is relative to a directory the next run will not
            // be using.
            editor_testing::Framework::Pytest => self
                .tree
                .root()
                .map(Path::to_path_buf)
                .or_else(|| path.parent().map(Path::to_path_buf))?,
            // Cargo needs a manifest above it.
            editor_testing::Framework::CargoTest => {
                editor_proc::run_config::find_cargo_manifest(path, self.tree.root())?
            }
        };
        Some((framework, cwd))
    }

    /// Run some tests, and read the results as they arrive.
    pub(super) fn run_tests(&mut self, scope: editor_testing::Scope) {
        // Testing stale code is the same trap as running it.
        if self.active_doc().is_some_and(|e| e.doc.is_dirty())
            && let Some(active) = self.active
            && !self.save_indices(&[active])
        {
            return;
        }

        let Some((framework, cwd)) = self.test_framework() else {
            self.error(
                "Open a Python or Rust file first \u{2014} the file decides which tests to run",
            );
            return;
        };

        // A file scope has to be expressed the way the framework expects, which
        // for pytest is relative to where it will be started.
        let scope = match scope {
            editor_testing::Scope::File(path) => {
                editor_testing::Scope::File(path.strip_prefix(&cwd).unwrap_or(&path).to_path_buf())
            }
            other => other,
        };

        let interpreter = self
            .interpreter()
            .map(|i| i.path)
            .filter(|_| framework == editor_testing::Framework::Pytest);
        let config = match editor_testing::command(framework, &scope, &cwd, interpreter.as_deref())
        {
            Ok(config) => config,
            Err(e) => {
                self.error(e.to_string());
                return;
            }
        };

        self.tests = Some(editor_testing::Session::new(framework, scope));
        self.dock = DockTab::Tests;
        self.show_output = true;
        // The console still shows the run in full — a test that prints
        // something is often the fastest way to find out why it failed, and the
        // panel only shows verdicts.
        self.runner.watch_output(true);
        // On pipes, not a terminal: this output is going to be *read*, and a
        // runner that can see a terminal formats for one. See editor_proc::pipe.
        self.runner.use_pipes(true);
        if let Err(e) = self.runner.start(config, true) {
            self.error(format!("Could not start: {e:#}"));
            self.runner.watch_output(false);
            self.tests = None;
        }
        // Only this run: an ordinary Run must still get its terminal.
        self.runner.use_pipes(false);
    }

    /// Run the one test the caret is in.
    pub(super) fn run_test_at_caret(&mut self) {
        let Some((framework, cwd)) = self.test_framework() else {
            self.error("Open a Python or Rust file first");
            return;
        };
        let Some(entry) = self.active_doc() else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            self.error("Save the file before running its tests");
            return;
        };
        let Some(tree) = entry.highlighter.as_ref().and_then(|h| h.tree()) else {
            self.error("The file has not been parsed yet");
            return;
        };

        let outline = editor_syntax::symbols::outline(tree, entry.doc.text());
        let caret = entry.view.selection.head;
        let Some(name) = editor_testing::discover::test_at(&outline, caret, framework) else {
            self.info("The caret is not inside a test");
            return;
        };

        let id = match framework {
            editor_testing::Framework::Pytest => {
                let relative = path.strip_prefix(&cwd).unwrap_or(&path);
                editor_testing::discover::node_id(&relative.to_string_lossy(), &name)
            }
            // libtest filters by name; the target is added by the parser when
            // the results come back.
            editor_testing::Framework::CargoTest => name,
        };
        self.run_tests(editor_testing::Scope::These(vec![id]));
    }

    /// Feed whatever the run has printed into the results parser.
    pub(super) fn poll_tests(&mut self) {
        let Some(session) = self.tests.as_mut() else {
            return;
        };
        let bytes = self.runner.take_output();
        if !bytes.is_empty() {
            session.feed(&bytes);
        }
    }

    /// Carry out what the Tests panel asked for.
    pub(super) fn apply_tests_action(&mut self, action: tests_panel::Action) {
        match action {
            tests_panel::Action::None => {}
            tests_panel::Action::RunAll => self.run_tests(editor_testing::Scope::All),
            tests_panel::Action::RunOne(id) => {
                self.run_tests(editor_testing::Scope::These(vec![id]));
            }
            tests_panel::Action::RunFailures => {
                let failures = self
                    .tests
                    .as_ref()
                    .map(|s| s.report.failures())
                    .unwrap_or_default();
                if !failures.is_empty() {
                    self.run_tests(editor_testing::Scope::These(failures));
                }
            }
            tests_panel::Action::Stop => self.runner.stop(),
            tests_panel::Action::Open { file, line } => {
                // Frameworks report paths relative to where they were started,
                // which is the runner's working directory.
                let path = self.runner.cwd().join(&file);
                let path = if path.exists() {
                    path
                } else {
                    // A path that is already absolute, or one relative to the
                    // project rather than the run.
                    self.tree
                        .root()
                        .map_or_else(|| PathBuf::from(&file), |root| root.join(&file))
                };
                self.open_at(&path, line.saturating_sub(1) as usize, 0);
            }
        }
    }

    /// Which Python will run this file, for the status bar.
    ///
    /// PLAN.md §3.8a calls this the indicator that prevents more confusion than
    /// any other: the Run button, the debugger, the test runner and the
    /// Packages panel all use this interpreter, and without it on screen
    /// nothing says when that quietly became a global Python instead of the
    /// project's environment.
    pub(super) fn python_status(&mut self) -> RuntimeStatus {
        let Some(interpreter) = self.interpreter() else {
            return RuntimeStatus {
                text: format!(
                    "{} No Python",
                    editor_lsp::diagnostics::Severity::Warning.glyph()
                ),
                hover: "No Python interpreter was found, so Python code cannot be run. \
                        Click to choose one."
                    .to_owned(),
                command: CommandId::SelectInterpreter,
            };
        };
        let origin = interpreter.label();
        let path = interpreter.path.display().to_string();
        let (text, hover) = match self.environment.python_version(&interpreter) {
            Some(Err(why)) if why == crate::environment::NOT_TRUSTED => (
                format!("Python ({origin}, not trusted)"),
                format!(
                    "{path}\nThis folder is not trusted, so nothing in its environment \
                     is run. Tools > Folder Trust changes that."
                ),
            ),
            Some(Ok(version)) => (
                format!("Python {version} ({origin})"),
                format!("{path}\nClick to choose another interpreter"),
            ),
            Some(Err(why)) => (
                format!(
                    "{} Python ({origin})",
                    editor_lsp::diagnostics::Severity::Warning.glyph()
                ),
                format!("{path} does not run: {why}\nClick to choose another interpreter"),
            ),
            None => (
                format!("Python ({origin})"),
                format!("{path}\nClick to choose another interpreter"),
            ),
        };
        RuntimeStatus {
            text,
            hover,
            command: CommandId::SelectInterpreter,
        }
    }

    /// Which Rust toolchain this project builds with, for the status bar.
    pub(super) fn rust_status(&mut self) -> RuntimeStatus {
        let (text, hover) = match self.environment.rust_version(self.tree.root()) {
            Some(Err(why)) if why == crate::environment::NOT_TRUSTED => (
                "Rust (not trusted)".to_owned(),
                "This folder is not trusted, so rust-analyzer and the toolchain it \
                 names are not run. Tools > Folder Trust changes that."
                    .to_owned(),
            ),
            Some(Ok(version)) => (
                format!("Rust {version}"),
                "The rustc that cargo uses in this project".to_owned(),
            ),
            Some(Err(why)) => (
                format!(
                    "{} Rust",
                    editor_lsp::diagnostics::Severity::Warning.glyph()
                ),
                format!("No working Rust toolchain: {why}"),
            ),
            None => ("Rust".to_owned(), "Asking rustc for its version".to_owned()),
        };
        RuntimeStatus {
            text,
            hover: format!("{hover}\nClick to check the toolchains"),
            command: CommandId::CheckToolchains,
        }
    }

    /// The interpreter that would actually be used, for display.
    ///
    /// The setting is only a preference: an empty one means "detect", and a
    /// project virtual environment wins over both. Showing the setting alone
    /// would tell the user nothing about what Run is going to do.
    pub(super) fn detected_interpreter(&mut self) -> Option<String> {
        self.interpreter()
            .map(|i| format!("{} ({})", i.path.display(), i.label()))
    }
}
