//! The open folder: the file tree, project search, and whether the folder may
//! run its own tools.

use super::*;

impl EditorApp {
    /// Open a project folder and start watching it.
    pub(super) fn open_folder(&mut self, folder: PathBuf) {
        tracing::info!(path = %folder.display(), "opening folder");

        // Reopening the same folder — which session restore can do right after
        // startup — should not tear down a working watch and build another.
        if self
            .watcher
            .as_ref()
            .is_some_and(|w| w.root() == Some(folder.as_path()))
        {
            self.tree.set_root(folder);
            return;
        }

        self.environment.invalidate();
        self.trust_required = editor_config::trust::needs_asking(&folder);
        if self.trust_required && self.trust.decision(&folder).is_none() {
            self.trust_prompt = Some(folder.clone());
        }
        self.environment
            .set_trusted(self.folder_trusted_for(&folder));

        // A different folder is a different repository, or none. Asked for here
        // rather than per file: discovery is one subprocess and the answer is
        // what decides whether any of the rest is worth doing.
        self.git.set_project(Some(&folder));
        // The branch and how it stands against its upstream go in the status
        // bar, which is on screen whether or not the panel ever is.
        self.git.refresh_branches();

        if let Some(watcher) = self.watcher.as_mut()
            && let Err(e) = watcher.set_root(&folder)
        {
            // Not fatal: the tree still works, it just will not notice
            // changes made elsewhere.
            tracing::warn!("could not watch {}: {e}", folder.display());
            self.info("Changes made outside The Editor will not be noticed automatically");
        }
        self.tree.set_root(folder);
        self.sync_watched_files();
    }

    /// Keep the watcher's list of loose files in step with the open tabs.
    ///
    /// Cheap when nothing changed — the watcher compares against what it
    /// already has — so this can be called from anywhere a tab opens or closes
    /// rather than being carefully threaded through each one.
    pub(super) fn sync_watched_files(&mut self) {
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        let files: Vec<PathBuf> = self
            .docs
            .iter()
            .filter_map(|d| d.doc.path().map(Path::to_path_buf))
            .collect();
        watcher.set_files(&files);
    }

    /// Start a project-wide search over the open folder.
    pub(super) fn start_project_search(&mut self, query: &editor_search::query::Query) {
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            self.info("Open a folder first \u{2014} project search needs one");
            return;
        };
        // The same walk go-to-file uses, with the same skip list: searching
        // `target` or `node_modules` finds thousands of matches nobody wants.
        let listing = editor_search::files::list(&root);
        let search = editor_search::project::Search::start(&root, listing.files, query);
        self.search.started(search);
    }

    /// Carry out what the explorer's context menu asked for.
    pub(super) fn apply_tree_action(
        &mut self,
        action: editor_widgets::file_tree::Action,
        ctx: &egui::Context,
    ) {
        use editor_widgets::file_tree::Action;

        match action {
            Action::None => {}
            Action::Open(path) => self.open_path(&path, false),
            Action::Preview(path) => self.open_path(&path, true),
            Action::Refresh => self.tree.refresh(),

            Action::NewFileIn(directory) => self.new_file.open(directory),
            Action::NewFolderIn(directory) => {
                // A folder needs no dialog of its own: create it with a
                // placeholder name and drop straight into an in-place rename,
                // which is how every file manager does it.
                let path = unique_path(&directory, "New Folder", "");
                match std::fs::create_dir(&path) {
                    Ok(()) => {
                        self.tree.refresh();
                        self.tree.begin_rename(&path);
                    }
                    Err(e) => self.error(format!("Could not create folder: {e}")),
                }
            }

            Action::Rename { from, to } => self.rename_path(&from, &to),
            // Recoverable, but still a surprise if it was a mis-click on a
            // folder with a hundred files in it. Ask first.
            Action::Delete(path) => self.pending_delete = Some(path),

            Action::Reveal(path) => {
                if let Err(e) = reveal_in_file_manager(&path) {
                    self.error(format!("Could not reveal {}: {e}", path.display()));
                }
            }
            Action::CopyPath(path) => {
                ctx.copy_text(path.display().to_string());
                self.info("Path copied");
            }
            Action::CopyRelativePath(path) => {
                let relative = self
                    .tree
                    .root()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(&path);
                ctx.copy_text(relative.display().to_string());
                self.info("Relative path copied");
            }
        }
    }

    /// Rename a file or folder, keeping any open tab pointing at it.
    pub(super) fn rename_path(&mut self, from: &Path, to: &Path) {
        if to.exists() {
            self.error(format!("{} already exists", to.display()));
            return;
        }
        if let Some(name) = to.file_name().and_then(|n| n.to_str())
            && let Err(e) = editor_core::filename::validate(name)
        {
            self.error(e.to_string());
            return;
        }

        if let Err(e) = std::fs::rename(from, to) {
            self.error(format!("Could not rename: {e}"));
            return;
        }

        // An open document must follow its file, or the next save writes back
        // to the old name and resurrects it.
        for entry in &mut self.docs {
            if entry.doc.path() == Some(from) {
                entry.doc.set_path(to.to_path_buf());
                entry.language = to
                    .extension()
                    .and_then(|e| e.to_str())
                    .map_or(LanguageId::PlainText, LanguageId::from_extension);
                entry.highlighter = new_highlighter(entry.language, &entry.doc);
            }
        }

        self.tree.refresh();
        tracing::info!(from = %from.display(), to = %to.display(), "renamed");
    }

    /// Move a path to the trash, closing any tab that showed it.
    /// Confirm before moving something to the trash.
    ///
    /// The delete is recoverable — it goes to the recycle bin, not oblivion —
    /// but a mis-click on a folder still means fishing a hundred files back out
    /// of it, and the file tree is a place where a click lands one row from
    /// where you meant. Escape cancels, which is the safe answer.
    /// Whether the open folder may run its own tools. A folder with nothing
    /// in it that would run needs no trust and is never asked about.
    pub(super) fn folder_trusted(&self) -> bool {
        self.tree
            .root()
            .is_none_or(|root| self.folder_trusted_for(root))
    }

    pub(super) fn folder_trusted_for(&self, root: &Path) -> bool {
        !self.trust_required || self.trust.decision(root) == Some(true)
    }

    /// Record an answer about a folder, keep it, and act on it.
    pub(super) fn set_trust(&mut self, folder: &Path, trusted: bool) {
        self.trust.decide(folder, trusted);
        if let Err(e) = self.trust.save(&self.paths.trust_file()) {
            self.error(format!("Could not save the folder's trust: {e}"));
        }
        if self.tree.root() == Some(folder) {
            self.environment
                .set_trusted(self.folder_trusted_for(folder));
        }
    }

    /// Ask whether a folder may run its own tools.
    ///
    /// Asked once per folder, and only about one that contains something that
    /// would run. Until it is answered the folder is treated as untrusted,
    /// which costs nothing but the tools themselves: the file still opens,
    /// highlights, and is checked by the servers found on `PATH`.
    pub(super) fn trust_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(folder) = self.trust_prompt.clone() else {
            return;
        };
        let name = folder.file_name().map_or_else(
            || folder.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let current = self.trust.decision(&folder);
        let mut decision: Option<bool> = None;

        egui::Modal::new(egui::Id::new("folder_trust")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading("Trust this folder?");
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong(&name);
                ui.label(
                    "contains things The Editor would run to help you work on it: \
                     the tools in its virtual environment, and for a Rust project, \
                     rust-analyzer, which builds its build scripts and macros, and \
                     the toolchain it names.",
                );
            });
            ui.add_space(4.0);
            ui.label(
                "Trust it if you wrote it or know where it came from. If not, it \
                 still opens and highlights, and is checked by the tools installed \
                 on this machine, but nothing it contains is run.",
            );
            ui.add_space(4.0);
            ui.weak(folder.display().to_string());
            if let Some(trusted) = current {
                ui.weak(if trusted {
                    "Currently trusted."
                } else {
                    "Currently not trusted."
                });
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Don't Trust").clicked() {
                    decision = Some(false);
                }
                if ui.button("Trust").clicked() {
                    decision = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            // Dismissed, not answered: not trusted, and asked again next time.
            self.trust_prompt = None;
            return;
        }
        if let Some(trusted) = decision {
            self.trust_prompt = None;
            self.set_trust(&folder, trusted);
        }
    }

    pub(super) fn delete_prompt(&mut self, ctx: &egui::Context) {
        let Some(path) = self.pending_delete.clone() else {
            return;
        };

        let is_dir = path.is_dir();
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut decision: Option<bool> = None;

        egui::Modal::new(egui::Id::new("confirm_delete")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(if is_dir {
                "Delete folder?"
            } else {
                "Delete file?"
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Move");
                ui.strong(&name);
                ui.label("to the recycle bin?");
            });
            if is_dir {
                ui.add_space(4.0);
                ui.label("Everything inside it goes too.");
            }
            ui.add_space(4.0);
            ui.weak(path.display().to_string());

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if ui.button("Delete").clicked() {
                    decision = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }

        match decision {
            None => {}
            Some(false) => self.pending_delete = None,
            Some(true) => {
                self.pending_delete = None;
                self.delete_path(&path);
            }
        }
    }

    pub(super) fn delete_path(&mut self, path: &Path) {
        if let Err(e) = editor_widgets::file_tree::move_to_trash(path) {
            self.error(format!("Could not delete {}: {e}", path.display()));
            return;
        }

        // Close tabs for the deleted file, or for anything inside a deleted
        // folder. Leaving them open invites saving the file back into
        // existence.
        let doomed: Vec<usize> = self
            .docs
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.doc
                    .path()
                    .is_some_and(|p| p == path || p.starts_with(path))
            })
            .map(|(i, _)| i)
            .collect();
        for index in doomed.into_iter().rev() {
            self.force_close_tab(index);
        }

        self.tree.refresh();
        self.info(format!(
            "Moved {} to the recycle bin",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned()
            )
        ));
    }
}
