//! Managing the set of running servers and the documents they know about.
//!
//! One server process per (project, server) pair, shared by every open file of
//! that language — starting a `rust-analyzer` per file would index the
//! workspace once per tab.
//!
//! The invariant that matters: a server's idea of a document must match ours
//! exactly, because it answers questions in terms of positions. Every open,
//! change and close is forwarded with a version number, and full-text sync is
//! used rather than incremental. Incremental sync is more efficient and much
//! easier to get subtly wrong — one dropped change and every position the
//! server reports is silently off by a line, for the rest of the session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use editor_core::document::Document;
use ropey::Rope;
use serde_json::json;

use crate::diagnostics::{Diagnostic, Store};
use crate::position::{self, Encoding};
use crate::registry::{self, ServerSpec};
use crate::server::{self, Event, Server};

/// The source id for diagnostics The Editor produced itself, rather than
/// received from a server.
///
/// A reserved id rather than a server's, so [`Store::clear_server`] on a
/// crashed server never takes these with it and the two never overwrite each
/// other.
pub const BUILTIN_SOURCE: &str = "syntax";

/// A question asked of a language server that returns places in the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// Where is the thing under the caret defined?
    Definition,
    /// Where else is it used?
    References,
}

impl Query {
    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::References => "textDocument/references",
        }
    }

    /// The capability a server must advertise to be worth asking.
    fn capability(self) -> &'static str {
        match self {
            Self::Definition => "definitionProvider",
            Self::References => "referencesProvider",
        }
    }

    /// For messages: "no definition found", "no uses found".
    #[must_use]
    pub fn noun(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::References => "uses",
        }
    }
}

/// Somewhere in the project, in the protocol's zero-based line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
}

/// One suggestion from a language server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// What to show in the list.
    pub label: String,
    /// What to put in the document. Often the same as `label`, but not always:
    /// a function may be labelled `parse(data)` and insert only `parse`.
    pub insert: String,
    /// A type signature or module path, shown greyed beside the label.
    pub detail: Option<String>,
    /// The protocol's numeric kind, for the glyph. 3 is Function, 6 Variable,
    /// 7 Class, 9 Module, 21 Constant -- see `CompletionItemKind`.
    pub kind: Option<u8>,
    /// What the server wants this sorted by, which is not the label: servers
    /// use it to float likely candidates, and ignoring it makes a good list
    /// look random.
    pub sort_text: Option<String>,
}

impl Completion {
    /// A one-character cue for the kind, so the list is skimmable without
    /// relying on colour alone.
    #[must_use]
    pub fn glyph(&self) -> &'static str {
        // Drawn in a proportional label, where the bundled fonts are thinner
        // than they look: `\u{25c7}`, `\u{25c8}`, `\u{25a6}`, `\u{25cf}` and
        // `\u{25b8}` all drew as empty boxes here, however plausible they
        // seem. Everything below is checked by `editor_widgets::glyphs`.
        match self.kind {
            // Method, function.
            Some(2 | 3) => "\u{192}",
            // Field, variable.
            Some(5) => "\u{25ab}",
            Some(6) => "\u{25aa}",
            // Class, struct.
            Some(7 | 22) => "\u{25ce}",
            // Interface.
            Some(8) => "\u{25cb}",
            // Module.
            Some(9) => "\u{25a0}",
            // Keyword.
            Some(14) => "\u{203a}",
            // Constant.
            Some(21) => "\u{2022}",
            _ => "\u{b7}",
        }
    }
}

/// One file's worth of a rename, as ranges to replace.
///
/// Whole files rather than a live document: a rename can touch twenty files,
/// most of them closed, and reconciling ranges against documents the editor has
/// not loaded is far more ways to be wrong than reading the file, applying the
/// server's ranges, and writing it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    /// Sorted **last-first**, so applying them cannot disturb the others.
    pub edits: Vec<TextEdit>,
}

/// One replacement within a file, in the protocol's zero-based coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub text: String,
}

/// A request sent and not yet answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Locations(Query),
    Completions,
    Rename,
    /// A hover, and where it was asked about. The position comes back with the
    /// answer because the pointer will have moved on by then, and a description
    /// of something else under the pointer is worse than none.
    Hover {
        line: u32,
        column: u32,
    },
}

/// What happened that the application should react to.
#[derive(Debug, Clone)]
pub enum Notice {
    /// Diagnostics changed for a file.
    DiagnosticsChanged(PathBuf),
    /// A server became usable.
    ServerReady(&'static str),
    /// A [`Query`] came back. An empty list means the server had no answer,
    /// which is different from no server having been asked.
    Answered {
        query: Query,
        locations: Vec<Location>,
    },
    /// Suggestions came back for the position last asked about.
    Completions(Vec<Completion>),
    /// A description of what is at a position. Empty text means the server had
    /// nothing to say about it, which is an answer and not a failure.
    Hovered {
        line: u32,
        column: u32,
        text: String,
    },
    /// A rename came back, as the files it would change. Empty means the
    /// server declined -- renaming a keyword, or a symbol it cannot resolve.
    Rename(Vec<FileEdit>),
    /// A server died. `restarting` is false once it has given up.
    ServerDied {
        id: &'static str,
        name: &'static str,
        restarting: bool,
    },
}

/// One open document, as the servers see it.
///
/// Holds the text itself, not just a version number. The servers must be told
/// what is in the *buffer*: a server started late, or restarted after a crash,
/// used to be sent the file as it was on disk — with the buffer's version
/// number on it — so until the next keystroke it answered questions about a
/// text nobody was looking at, and a rename computed against that text
/// landed in the wrong places. The text is also what converts columns between
/// the editor's count and the protocol's; see [`crate::position`].
#[derive(Debug, Clone)]
struct OpenDocument {
    /// `None` for a file no server handles, which is still recorded so that it
    /// is not offered again on every frame.
    language_id: Option<&'static str>,
    /// The protocol's version, bumped with every change sent.
    version: i32,
    /// The editor's version of the text below, so an unchanged document is
    /// not sent again.
    source_version: u64,
    text: Rope,
    /// Servers that should know this document.
    serving: Vec<&'static str>,
    /// The subset that have been sent it. A server joins this list when it
    /// is sent `didOpen`, which waits for its handshake to finish, and leaves
    /// it when it dies, so a restarted server is sent the document afresh.
    told: Vec<&'static str>,
}

impl OpenDocument {
    /// The text of `line`, or `None` past the end.
    fn line(&self, line: u32) -> Option<String> {
        let line = line as usize;
        (line < self.text.len_lines()).then(|| self.text.line(line).to_string())
    }
}

/// Every running server, and what they know.
#[derive(Debug, Default)]
pub struct Lsp {
    servers: HashMap<&'static str, Server>,
    documents: HashMap<PathBuf, OpenDocument>,
    diagnostics: Store,
    root: Option<PathBuf>,
    /// Extra directories searched before `PATH`, so a project venv's tools win.
    extra_path: Vec<PathBuf>,
    /// Servers that were wanted but could not be found, so the toolchain check
    /// can say which and the editor does not retry them every keystroke.
    missing: Vec<ServerSpec>,
    /// Requests sent and not yet answered, keyed by server and request id.
    ///
    /// Request ids are allocated per server, so the server id has to be part of
    /// the key or two servers would collide on id 1.
    pending: HashMap<(&'static str, i64), Pending>,
    /// Server ids the user has switched off. Checked before starting one, so a
    /// disabled server is never spawned rather than started and ignored.
    disabled: Vec<String>,
    /// Threads waiting for stopped servers to exit, joined when this is
    /// dropped so that none outlives the editor.
    stopping: Vec<std::thread::JoinHandle<()>>,
    /// Servers being looked for on a thread of their own, each with the
    /// channel its answer arrives on.
    probing: HashMap<
        &'static str,
        (
            ServerSpec,
            std::sync::mpsc::Receiver<Option<registry::Found>>,
        ),
    >,
}

impl Lsp {
    /// Switch servers off by id. Anything already running for a newly
    /// disabled server is stopped, and a server switched back on is started
    /// for the files that want it.
    pub fn set_disabled(&mut self, disabled: Vec<String>) {
        if self.disabled == disabled {
            return;
        }
        self.disabled = disabled;
        let stopping: Vec<&'static str> = self
            .servers
            .keys()
            .filter(|id| self.disabled.iter().any(|d| d == *id))
            .copied()
            .collect();
        for id in stopping {
            if let Some(mut server) = self.servers.remove(id) {
                self.stopping.extend(server.stop());
            }
            // Its findings go with it: a server that is not running is not
            // there to correct them.
            self.diagnostics.clear_server(id);
            for document in self.documents.values_mut() {
                document.serving.retain(|s| *s != id);
                document.told.retain(|t| *t != id);
            }
        }
        // Anything re-enabled: every document asks again for what it wants.
        let paths: Vec<PathBuf> = self.documents.keys().cloned().collect();
        for path in paths {
            self.attach(&path);
        }
    }

    /// Point at a project. Stops everything running for the previous one.
    ///
    /// Every document is forgotten along with the servers, so the next
    /// [`Self::sync`] offers each open file to the new project's servers.
    /// Forgetting them here and remembering them anywhere else is how open
    /// tabs used to lose their language server for good after a virtual
    /// environment was created.
    pub fn set_root(&mut self, root: Option<PathBuf>, extra_path: Vec<PathBuf>) {
        if self.root == root && self.extra_path == extra_path {
            return;
        }
        self.shutdown();
        self.root = root;
        self.extra_path = extra_path;
    }

    /// Stop every server and forget everything.
    pub fn shutdown(&mut self) {
        for (_, mut server) in self.servers.drain() {
            self.stopping.extend(server.stop());
        }
        self.stopping.retain(|reaper| !reaper.is_finished());
        self.documents.clear();
        self.diagnostics.clear();
        self.missing.clear();
        self.pending.clear();
        // An answer still on its way is about the old project's search path.
        self.probing.clear();
    }

    #[must_use]
    pub fn diagnostics(&self) -> &Store {
        &self.diagnostics
    }

    /// Publish diagnostics The Editor produced itself.
    ///
    /// Everything a language server offers is optional — PLAN.md §3.6 — so The
    /// Editor has to be able to say something about broken code with nothing
    /// installed. Those findings go through the same store as a server's, under
    /// a reserved source id, so they merge with real diagnostics instead of
    /// replacing them and are cleared on the same paths.
    ///
    /// An empty list means "no complaints", exactly as a `publishDiagnostics`
    /// with an empty array does.
    pub fn set_builtin(&mut self, path: &Path, diagnostics: Vec<Diagnostic>) {
        self.diagnostics.set(path, BUILTIN_SOURCE, diagnostics);
    }

    /// Servers currently running, for the status bar and the toolchain check.
    #[must_use]
    pub fn running(&self) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = self
            .servers
            .values()
            .filter(|s| s.is_ready())
            .map(Server::id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Servers that were wanted for an open file but are not installed.
    #[must_use]
    pub fn missing(&self) -> &[ServerSpec] {
        &self.missing
    }

    /// Bring the servers' copy of a document up to date with the editor's.
    ///
    /// `version` is the document's own version; `text` is asked for only when
    /// the servers are behind, so calling this every frame for every tab costs
    /// a hash lookup. This is the only record of what the servers have been
    /// told — the application keeps none of its own, which is what lets
    /// [`Self::set_root`] start afresh without anyone else having to notice.
    pub fn sync(&mut self, path: &Path, version: u64, text: impl FnOnce() -> String) {
        match self.documents.get(path) {
            Some(document) if document.source_version == version => {}
            Some(_) => self.change(path, version, &text()),
            None => self.open(path, version, &text()),
        }
    }

    /// Close every document for which `keep` says no.
    pub fn retain(&mut self, keep: impl Fn(&Path) -> bool) {
        let closing: Vec<PathBuf> = self
            .documents
            .keys()
            .filter(|path| !keep(path))
            .cloned()
            .collect();
        for path in closing {
            self.close(&path);
        }
    }

    /// Record a document and offer it to the servers for its language.
    ///
    /// Cheap for a language with no server, which is the common case: the
    /// document is recorded so it is not offered again, and nothing starts.
    /// A file too large to edit is recorded the same way — PLAN.md §8 keeps
    /// language servers away from it.
    fn open(&mut self, path: &Path, version: u64, text: &str) {
        let too_large = text.len() as u64 > editor_core::document::LARGE_FILE_BYTES;
        let language_id = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(registry::language_id_for_extension)
            .filter(|_| !too_large);
        self.documents.insert(
            path.to_path_buf(),
            OpenDocument {
                language_id,
                version: 1,
                source_version: version,
                text: Rope::from_str(text),
                serving: Vec::new(),
                told: Vec::new(),
            },
        );
        self.attach(path);
    }

    /// Start whatever servers a document wants and has not got, and send it
    /// to any that are ready. One that is still starting is sent it by
    /// [`Self::on_ready`].
    fn attach(&mut self, path: &Path) {
        let Some(language_id) = self.documents.get(path).and_then(|d| d.language_id) else {
            return;
        };
        let started: Vec<&'static str> = registry::for_language(language_id)
            .into_iter()
            .filter_map(|spec| self.ensure_started(spec))
            .collect();
        let Some(document) = self.documents.get_mut(path) else {
            return;
        };
        for id in started {
            if !document.serving.contains(&id) {
                document.serving.push(id);
            }
        }
        let uri = server::path_to_uri(path);
        for id in document.serving.clone() {
            if document.told.contains(&id) {
                continue;
            }
            if let Some(server) = self.servers.get(id)
                && server.is_ready()
            {
                send_open(server, &uri, document);
                document.told.push(id);
            }
        }
    }

    /// Tell the servers a document changed.
    fn change(&mut self, path: &Path, version: u64, text: &str) {
        let Some(document) = self.documents.get_mut(path) else {
            return;
        };
        document.text = Rope::from_str(text);
        document.source_version = version;
        document.version += 1;
        let uri = server::path_to_uri(path);
        for id in &document.told {
            if let Some(server) = self.servers.get(id) {
                let _ = server.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": document.version },
                        // Full-text sync: one range covering everything.
                        "contentChanges": [{ "text": text }],
                    }),
                );
            }
        }
    }

    /// Tell the servers a file was saved, which is when some of them lint.
    pub fn save(&self, path: &Path, text: &str) {
        let Some(document) = self.documents.get(path) else {
            return;
        };
        let uri = server::path_to_uri(path);
        for id in &document.told {
            if let Some(server) = self.servers.get(id) {
                let _ = server.notify(
                    "textDocument/didSave",
                    json!({ "textDocument": { "uri": uri }, "text": text }),
                );
            }
        }
    }

    /// The servers that know `path` and can answer `capability`, each with the
    /// position in its own column count. Empty when nobody can be asked.
    ///
    /// `line` and `column` are the editor's: zero-based, counted in characters.
    fn askable(
        &self,
        path: &Path,
        capability: &str,
        line: u32,
        column: u32,
    ) -> Vec<(&'static str, serde_json::Value)> {
        let Some(document) = self.documents.get(path) else {
            return Vec::new();
        };
        let text = document.line(line).unwrap_or_default();
        document
            .told
            .iter()
            .filter_map(|id| {
                let server = self.servers.get(id)?;
                // Ruff serves Python and cannot answer most of these. Asking it
                // anyway gets a "method not found" and, worse, stops the loop
                // before the server that *can* answer is reached.
                if !server.is_ready() || !server.supports(capability) {
                    return None;
                }
                let character = position::to_protocol(&text, column, server.position_encoding());
                Some((*id, json!({ "line": line, "character": character })))
            })
            .collect()
    }

    /// Ask a server where something is defined, or where else it is used.
    ///
    /// Returns false if nothing could be asked — no server for this language,
    /// none running yet, or the file was never opened — so the caller knows to
    /// fall back rather than waiting for an answer that is not coming.
    ///
    /// Only the first server that knows the file is asked. Two servers on one
    /// Python file would both answer, and merging "where is this defined"
    /// results from a linter and a type checker produces a list with the same
    /// place in it twice.
    pub fn ask(&mut self, query: Query, path: &Path, line: u32, column: u32) -> bool {
        let uri = server::path_to_uri(path);
        for (id, position) in self.askable(path, query.capability(), line, column) {
            let mut params = json!({ "textDocument": { "uri": uri }, "position": position });
            if query == Query::References {
                // Without this the definition itself is left out of the list,
                // and "find uses" that skips the declaration is confusing when
                // there is only one use.
                params["context"] = json!({ "includeDeclaration": true });
            }
            if let Some(server) = self.servers.get_mut(id)
                && let Ok(request) = server.send_request(query.method(), params)
            {
                self.pending
                    .insert((id, request), Pending::Locations(query));
                return true;
            }
        }
        false
    }

    /// Ask for completions at a position.
    ///
    /// Returns false if nothing could be asked, so the caller does not sit
    /// waiting for a popup that is never going to appear.
    ///
    /// Any answer still outstanding is forgotten first. Completion requests are
    /// sent while the user types, so several can be in flight at once and only
    /// the last one is about the text now on screen; without this, an older,
    /// slower reply would arrive last and replace the right list with a stale
    /// one.
    pub fn complete(&mut self, path: &Path, line: u32, column: u32) -> bool {
        self.pending.retain(|_, kind| *kind != Pending::Completions);
        let uri = server::path_to_uri(path);
        for (id, position) in self.askable(path, "completionProvider", line, column) {
            let params = json!({ "textDocument": { "uri": uri }, "position": position });
            if let Some(server) = self.servers.get_mut(id)
                && let Ok(request) = server.send_request("textDocument/completion", params)
            {
                self.pending.insert((id, request), Pending::Completions);
                return true;
            }
        }
        false
    }

    /// Ask what is at a position — a type, a signature, a docstring.
    ///
    /// Returns false when nothing could be asked, so the caller can fall back
    /// to what the parse tree knows rather than waiting for a reply that is
    /// never coming.
    ///
    /// Any hover still outstanding is forgotten first, for the same reason
    /// completions are: the pointer moves while the request is in flight, and
    /// an older, slower reply arriving last would describe somewhere the
    /// pointer has left.
    ///
    /// **Every** server that can answer is asked, not the first one. This is
    /// where hover differs from [`Self::ask`]: Ruff advertises `hoverProvider`
    /// and means it — it explains its own rule codes — but has nothing to say
    /// about ordinary code. Stopping at the first server that *supports* hover
    /// meant Ruff answered `null` in ten milliseconds and basedpyright, which
    /// knows the type, was never asked at all. The caller keeps the first
    /// non-empty answer.
    ///
    /// The answer carries back the position asked about, in the editor's
    /// terms, so the caller can tell whether it still applies.
    pub fn hover(&mut self, path: &Path, line: u32, column: u32) -> bool {
        self.pending
            .retain(|_, kind| !matches!(kind, Pending::Hover { .. }));
        let uri = server::path_to_uri(path);
        let mut asked = false;
        for (id, position) in self.askable(path, "hoverProvider", line, column) {
            let params = json!({ "textDocument": { "uri": uri }, "position": position });
            if let Some(server) = self.servers.get_mut(id)
                && let Ok(request) = server.send_request("textDocument/hover", params)
            {
                self.pending
                    .insert((id, request), Pending::Hover { line, column });
                asked = true;
            }
        }
        asked
    }

    /// Ask a server to rename the symbol at a position.
    ///
    /// Returns false if nothing could be asked. Rename is the one feature here
    /// with no parse-tree fallback: renaming by textual match is how people
    /// rename the wrong things, and a wrong rename is silent until something
    /// breaks much later.
    pub fn rename(&mut self, path: &Path, line: u32, column: u32, new_name: &str) -> bool {
        let uri = server::path_to_uri(path);
        for (id, position) in self.askable(path, "renameProvider", line, column) {
            let params = json!({
                "textDocument": { "uri": uri },
                "position": position,
                "newName": new_name,
            });
            if let Some(server) = self.servers.get_mut(id)
                && let Ok(request) = server.send_request("textDocument/rename", params)
            {
                self.pending.insert((id, request), Pending::Rename);
                return true;
            }
        }
        false
    }

    /// Tell the servers a file is closed, and forget its diagnostics.
    pub fn close(&mut self, path: &Path) {
        // Diagnostics go first and unconditionally. A server can publish for a
        // file we never opened — rust-analyzer reports on the whole workspace —
        // so tying the clear to having a document would leave those on screen
        // with no tab to fix them in.
        self.diagnostics.clear_file(path);

        let Some(document) = self.documents.remove(path) else {
            return;
        };
        let uri = server::path_to_uri(path);
        for id in &document.told {
            if let Some(server) = self.servers.get(id) {
                let _ = server.notify(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": uri } }),
                );
            }
        }
    }

    /// Start a server if it is wanted and not already running.
    ///
    /// Returns `None` when the server is not installed, which is not an error —
    /// see the degradation ladder in PLAN.md §3.6.
    fn ensure_started(&mut self, spec: ServerSpec) -> Option<&'static str> {
        if self.servers.contains_key(spec.id) {
            return Some(spec.id);
        }
        // Checked before anything else, so a switched-off server is never
        // spawned rather than started and then ignored.
        if self.disabled.iter().any(|d| d == spec.id) {
            return None;
        }
        if self.missing.iter().any(|s| s.id == spec.id) {
            return None; // already looked, still not there
        }

        if self.probing.contains_key(spec.id) {
            return None; // still being looked for
        }
        self.root.as_ref()?;

        // Looking for a server means running it with `--version`, and a
        // Node-based one takes the best part of a second to answer. That used
        // to happen here, on the thread that draws the window, the moment the
        // first file of a language was opened. It happens on a thread of its
        // own now, and `poll` starts the server when the answer comes back.
        let (tx, rx) = std::sync::mpsc::channel();
        let extra_path = self.extra_path.clone();
        let probe = std::thread::Builder::new()
            .name(format!("lsp-find-{}", spec.id))
            .spawn(move || {
                let _ = tx.send(registry::find(spec, &extra_path));
            });
        match probe {
            Ok(_) => {
                self.probing.insert(spec.id, (spec, rx));
            }
            Err(e) => {
                tracing::warn!(server = spec.id, "could not look for it: {e}");
                self.missing.push(spec);
            }
        }
        None
    }

    /// Start every server whose search has finished, and offer it the
    /// documents that were waiting for it.
    fn finish_probes(&mut self) {
        let finished: Vec<(ServerSpec, Option<registry::Found>)> = self
            .probing
            .values()
            .filter_map(|(spec, rx)| match rx.try_recv() {
                Ok(found) => Some((*spec, found)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some((*spec, None)),
            })
            .collect();
        if finished.is_empty() {
            return;
        }

        for (spec, found) in finished {
            self.probing.remove(spec.id);
            let (Some(found), Some(root)) = (found, self.root.clone()) else {
                tracing::info!(server = spec.id, "not installed");
                self.missing.push(spec);
                continue;
            };
            match Server::start(spec, found.program, &root) {
                Ok(server) => {
                    tracing::info!(server = spec.id, "started");
                    self.servers.insert(spec.id, server);
                }
                Err(e) => {
                    tracing::warn!(server = spec.id, "could not start: {e:#}");
                    self.missing.push(spec);
                }
            }
        }

        let paths: Vec<PathBuf> = self.documents.keys().cloned().collect();
        for path in paths {
            self.attach(&path);
        }
    }

    /// True while a server is being looked for, so a caller waiting on one
    /// knows to keep asking for frames.
    #[must_use]
    pub fn is_starting(&self) -> bool {
        !self.probing.is_empty() || self.servers.values().any(|s| !s.is_ready())
    }

    /// Drain every server. Call once per frame.
    pub fn poll(&mut self) -> Vec<Notice> {
        self.finish_probes();
        let mut notices = Vec::new();
        let ids: Vec<&'static str> = self.servers.keys().copied().collect();

        for id in ids {
            let Some(server) = self.servers.get_mut(id) else {
                continue;
            };
            let spec = server.spec();
            let events = server.poll();
            // Read after polling: the handshake that fixes it may be among the
            // events just drained.
            let encoding = server.position_encoding();
            let became_ready = events.iter().any(|e| matches!(e, Event::Ready { .. }));

            for event in events {
                match event {
                    Event::Diagnostics { path, diagnostics } => {
                        let mut converted: Vec<Diagnostic> = diagnostics
                            .iter()
                            .map(|d| Diagnostic::from_lsp(d, spec.name))
                            .collect();
                        // A file nobody has open is left in protocol columns:
                        // reading it from disk for every publish would cost
                        // more than a column on an emoji line is worth, and
                        // opening it gets fresh diagnostics anyway.
                        if let Some(document) = self.documents.get(&path) {
                            for d in &mut converted {
                                d.column = from_protocol(document, d.line, d.column, encoding);
                                d.end_column =
                                    from_protocol(document, d.end_line, d.end_column, encoding);
                            }
                        }
                        self.diagnostics.set(&path, id, converted);
                        notices.push(Notice::DiagnosticsChanged(path));
                    }
                    Event::Exited { restarting } => {
                        // Stale diagnostics from a server that is no longer
                        // running to correct them are worse than none.
                        self.diagnostics.clear_server(id);
                        // A restarted server knows nothing, so every document
                        // has to be sent to it again once it is ready.
                        for document in self.documents.values_mut() {
                            document.told.retain(|t| *t != id);
                        }
                        self.pending.retain(|(server, _), _| *server != id);
                        notices.push(Notice::ServerDied {
                            id,
                            name: spec.name,
                            restarting,
                        });
                    }
                    Event::Log(text) => tracing::debug!(server = id, "{text}"),
                    Event::Error {
                        id: request,
                        message,
                    } => {
                        tracing::debug!(server = id, request, "request failed: {message}");
                    }
                    Event::Ready { .. } => {}
                    // Answered by the server wrapper before it gets here.
                    Event::Request { .. } => {}
                    Event::Response {
                        id: request,
                        result,
                    } => match self.pending.remove(&(id, request)) {
                        Some(Pending::Locations(query)) => {
                            let mut locations = parse_locations(&result);
                            let mut texts = Texts::new(&self.documents);
                            for location in &mut locations {
                                location.column = texts.column(
                                    &location.path,
                                    location.line,
                                    location.column,
                                    encoding,
                                );
                            }
                            notices.push(Notice::Answered { query, locations });
                        }
                        Some(Pending::Completions) => {
                            notices.push(Notice::Completions(parse_completions(&result)));
                        }
                        Some(Pending::Rename) => {
                            let mut files = parse_workspace_edit(&result);
                            let mut texts = Texts::new(&self.documents);
                            for file in &mut files {
                                for edit in &mut file.edits {
                                    edit.start_column = texts.column(
                                        &file.path,
                                        edit.start_line,
                                        edit.start_column,
                                        encoding,
                                    );
                                    edit.end_column = texts.column(
                                        &file.path,
                                        edit.end_line,
                                        edit.end_column,
                                        encoding,
                                    );
                                }
                            }
                            notices.push(Notice::Rename(files));
                        }
                        Some(Pending::Hover { line, column }) => {
                            notices.push(Notice::Hovered {
                                line,
                                column,
                                text: parse_hover(&result),
                            });
                        }
                        None => {}
                    },
                }
            }

            if became_ready {
                self.on_ready(id);
                notices.push(Notice::ServerReady(id));
            }
        }

        // Drop servers that have given up, so they are not polled forever.
        self.servers.retain(|_, server| server.is_alive());
        notices
    }

    /// Complete the handshake, then send every document waiting for this
    /// server — from the buffer, as it is now.
    fn on_ready(&mut self, id: &'static str) {
        let Some(server) = self.servers.get(id) else {
            return;
        };
        let _ = server.notify("initialized", json!({}));

        for (path, document) in &mut self.documents {
            if !document.serving.contains(&id) || document.told.contains(&id) {
                continue;
            }
            send_open(server, &server::path_to_uri(path), document);
            document.told.push(id);
        }
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        self.shutdown();
        // All at once rather than one after another, so a quit waits for the
        // slowest server rather than for the sum of them.
        for reaper in self.stopping.drain(..) {
            let _ = reaper.join();
        }
    }
}

/// Send `didOpen` for a document, with the text the editor holds.
fn send_open(server: &Server, uri: &str, document: &OpenDocument) {
    let _ = server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": document.language_id.unwrap_or("plaintext"),
                "version": document.version,
                "text": document.text.to_string(),
            }
        }),
    );
}

/// A protocol column on `line` of an open document, as a character column.
fn from_protocol(document: &OpenDocument, line: u32, column: u32, encoding: Encoding) -> u32 {
    match document.line(line) {
        Some(text) => position::from_protocol(&text, column, encoding),
        None => column,
    }
}

/// The text of files a server's answer refers to, for converting its columns.
///
/// Open documents are read from the buffer, which is what the server was
/// told. Anything else is read from disk once per answer, the same way it is
/// read when the answer is acted on — through [`Document::open`], so a
/// byte-order mark or CRLF line endings do not shift the count.
struct Texts<'a> {
    open: &'a HashMap<PathBuf, OpenDocument>,
    closed: HashMap<PathBuf, Option<Rope>>,
}

impl<'a> Texts<'a> {
    fn new(open: &'a HashMap<PathBuf, OpenDocument>) -> Self {
        Self {
            open,
            closed: HashMap::new(),
        }
    }

    fn column(&mut self, path: &Path, line: u32, column: u32, encoding: Encoding) -> u32 {
        if encoding == Encoding::Utf32 {
            return column;
        }
        if let Some(document) = self.open.get(path) {
            return from_protocol(document, line, column, encoding);
        }
        let rope = self
            .closed
            .entry(path.to_path_buf())
            .or_insert_with(|| Document::open(path).ok().map(|d| d.text().clone()));
        match rope {
            Some(rope) if (line as usize) < rope.len_lines() => {
                position::from_protocol(&rope.line(line as usize).to_string(), column, encoding)
            }
            _ => column,
        }
    }
}

/// Read a `WorkspaceEdit` into per-file edits.
///
/// The protocol offers two shapes -- `changes`, a map of URI to edits, and
/// `documentChanges`, an array carrying document versions -- and servers pick
/// either, so both are read.
///
/// Each file's edits are sorted last-first, because applying them in document
/// order shifts every range after the first. That is the easiest way to corrupt
/// a file during a rename, and it fails quietly: the result is still valid text,
/// just wrong.
/// Read a `Hover` into the text to show.
///
/// The protocol has accumulated four shapes for `contents` over the years and
/// every one is still legal, so every one is read: a plain string, a
/// `MarkedString` object with a language and a value, an array of either, and a
/// `MarkupContent` with a kind and a value. rust-analyzer sends the last,
/// basedpyright the last, and older servers the first two.
///
/// Fenced code blocks are unwrapped rather than rendered. A hover is a few
/// lines in a small window; the fences are the only markdown in it that would
/// be *lost* by showing the source, and the rest — a bullet, an underscore —
/// reads perfectly well as it is.
fn parse_hover(result: &serde_json::Value) -> String {
    fn one(value: &serde_json::Value) -> Option<String> {
        // A plain string, or `MarkupContent`/`MarkedString`'s `value`.
        if let Some(text) = value.as_str() {
            return Some(text.to_owned());
        }
        value
            .get("value")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }

    let contents = result.get("contents").unwrap_or(&serde_json::Value::Null);
    let text = match contents {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(one)
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => one(other).unwrap_or_default(),
    };

    unfence(&text)
}

/// Strip markdown code fences, keeping what was inside them.
///
/// The lines between the fences are the signature and the type, which is the
/// part of a hover anyone reads. Leaving ```` ```rust ```` on screen is three
/// characters of noise above every one of them.
fn unfence(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut previous_was_blank = true;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            // The fence itself goes, and so does a blank line that would be
            // left stranded where two blocks met.
            continue;
        }
        let blank = line.trim().is_empty();
        if blank && previous_was_blank {
            continue;
        }
        previous_was_blank = blank;
        out.push(line);
    }

    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

fn parse_workspace_edit(result: &serde_json::Value) -> Vec<FileEdit> {
    fn edits_of(value: &serde_json::Value) -> Vec<TextEdit> {
        let mut out: Vec<TextEdit> = value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|e| {
                        let range = e.get("range")?;
                        let (start, end) = (range.get("start")?, range.get("end")?);
                        Some(TextEdit {
                            start_line: u32::try_from(start.get("line")?.as_u64()?).ok()?,
                            start_column: u32::try_from(start.get("character")?.as_u64()?).ok()?,
                            end_line: u32::try_from(end.get("line")?.as_u64()?).ok()?,
                            end_column: u32::try_from(end.get("character")?.as_u64()?).ok()?,
                            text: e.get("newText")?.as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| {
            b.start_line
                .cmp(&a.start_line)
                .then(b.start_column.cmp(&a.start_column))
        });
        out
    }

    let mut files: Vec<FileEdit> = Vec::new();

    if let Some(changes) = result.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            if let Some(path) = server::uri_to_path(uri) {
                let edits = edits_of(edits);
                if !edits.is_empty() {
                    files.push(FileEdit { path, edits });
                }
            }
        }
    }

    if let Some(changes) = result.get("documentChanges").and_then(|c| c.as_array()) {
        for change in changes {
            // A create/rename/delete file operation, which renaming a symbol
            // does not produce and which this deliberately does not perform.
            let Some(uri) = change
                .get("textDocument")
                .and_then(|d| d.get("uri"))
                .and_then(|u| u.as_str())
            else {
                continue;
            };
            if let Some(path) = server::uri_to_path(uri)
                && let Some(edits) = change.get("edits")
            {
                let edits = edits_of(edits);
                if !edits.is_empty() {
                    files.push(FileEdit { path, edits });
                }
            }
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    files
}

/// The most suggestions kept from one response.
///
/// A bare `.` on a Python module can return several thousand. Past a few
/// hundred the list is a scrolling wall nobody reads, and the filtering the
/// user is about to do narrows it in a keystroke or two anyway.
const MAX_COMPLETIONS: usize = 300;

/// Read completions out of a response.
///
/// The protocol allows either a bare array or a `CompletionList` with an
/// `items` field, and servers use both.
fn parse_completions(result: &serde_json::Value) -> Vec<Completion> {
    let items = match result {
        serde_json::Value::Array(items) => items.as_slice(),
        serde_json::Value::Object(_) => match result.get("items") {
            Some(serde_json::Value::Array(items)) => items.as_slice(),
            _ => &[],
        },
        _ => &[],
    };

    let mut out: Vec<Completion> = items
        .iter()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.trim().to_owned();
            if label.is_empty() {
                return None;
            }
            // `insertText` wins where present, because a label may be
            // decorative -- pyright labels a function `parse` but a snippet
            // server may label it `parse(data)`, which must not be typed in
            // whole. A `textEdit` would be more correct still, but applying one
            // needs its range, and the range is stated against a document that
            // may have moved on since the request was sent.
            let insert = item
                .get("insertText")
                .and_then(|v| v.as_str())
                .map_or_else(|| label.clone(), str::to_owned);
            Some(Completion {
                detail: item
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                kind: item
                    .get("kind")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|k| u8::try_from(k).ok()),
                sort_text: item
                    .get("sortText")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                label,
                insert,
            })
        })
        .collect();

    // Servers rank with `sortText`, falling back to the label. Sorting by label
    // alone throws that ranking away and buries the obvious candidate.
    out.sort_by(|a, b| {
        let key = |c: &Completion| c.sort_text.clone().unwrap_or_else(|| c.label.clone());
        key(a).cmp(&key(b)).then_with(|| a.label.cmp(&b.label))
    });
    out.dedup_by(|a, b| a.label == b.label && a.insert == b.insert);
    out.truncate(MAX_COMPLETIONS);
    out
}

/// Read a `Location`, `Location[]` or `LocationLink[]` out of a response.
///
/// The protocol allows all three for `textDocument/definition`, and servers
/// genuinely differ: `rust-analyzer` sends `LocationLink`s, `pyright` sends
/// plain `Location`s, and a server with nothing to say sends `null`. Handling
/// only one shape means the feature works for one language and silently does
/// nothing for the other.
fn parse_locations(result: &serde_json::Value) -> Vec<Location> {
    fn one(value: &serde_json::Value) -> Option<Location> {
        // `LocationLink` names things differently from `Location`; take
        // whichever pair is present.
        let uri = value
            .get("uri")
            .or_else(|| value.get("targetUri"))?
            .as_str()?;
        let range = value
            .get("range")
            .or_else(|| value.get("targetSelectionRange"))
            .or_else(|| value.get("targetRange"))?;
        let start = range.get("start")?;
        Some(Location {
            path: server::uri_to_path(uri)?,
            line: u32::try_from(start.get("line")?.as_u64()?).ok()?,
            column: u32::try_from(start.get("character")?.as_u64()?).ok()?,
        })
    }

    match result {
        serde_json::Value::Array(items) => items.iter().filter_map(one).collect(),
        serde_json::Value::Object(_) => one(result).into_iter().collect(),
        // `null` is a valid answer meaning "I do not know".
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod hover_tests {
    use super::{parse_hover, unfence};
    use serde_json::json;

    #[test]
    fn a_server_with_nothing_to_say_says_nothing() {
        assert_eq!(parse_hover(&json!(null)), "");
        assert_eq!(parse_hover(&json!({})), "");
        assert_eq!(parse_hover(&json!({ "contents": [] })), "");
    }

    /// The oldest shape, and still legal.
    #[test]
    fn a_plain_string_is_read() {
        let value = json!({ "contents": "fn add(a: i32) -> i32" });
        assert_eq!(parse_hover(&value), "fn add(a: i32) -> i32");
    }

    /// `MarkupContent`, which is what rust-analyzer and basedpyright send.
    #[test]
    fn markup_content_is_read_and_unfenced() {
        let value = json!({
            "contents": {
                "kind": "markdown",
                "value": "```rust\nfn add(a: i32, b: i32) -> i32\n```\n\nAdds two numbers.",
            }
        });
        assert_eq!(
            parse_hover(&value),
            "fn add(a: i32, b: i32) -> i32\n\nAdds two numbers."
        );
    }

    /// `MarkedString`: a language and a value.
    #[test]
    fn a_marked_string_is_read() {
        let value = json!({
            "contents": { "language": "python", "value": "def add(a, b)" }
        });
        assert_eq!(parse_hover(&value), "def add(a, b)");
    }

    /// An array of any of the above, which older servers send freely.
    #[test]
    fn an_array_of_pieces_is_joined() {
        let value = json!({
            "contents": [
                { "language": "rust", "value": "fn add" },
                "Adds two numbers.",
            ]
        });
        assert_eq!(parse_hover(&value), "fn add\n\nAdds two numbers.");
    }

    #[test]
    fn fences_go_and_what_was_inside_them_stays() {
        assert_eq!(unfence("```rust\ncode\n```"), "code");
        assert_eq!(unfence("```\ncode\n```"), "code");
        assert_eq!(unfence("no fences here"), "no fences here");
    }

    /// Two blocks back to back leave a run of blank lines where the fences
    /// were, and a hover is a small window.
    #[test]
    fn blank_lines_left_by_the_fences_are_collapsed() {
        let text = "```rust\nfirst\n```\n\n```rust\nsecond\n```";
        assert_eq!(unfence(text), "first\n\nsecond");
    }

    #[test]
    fn trailing_blank_lines_are_dropped() {
        assert_eq!(unfence("text\n\n\n"), "text");
    }

    /// Everything except the fences is left as it is: a hover is a few lines,
    /// and a bullet or an underscore reads perfectly well unrendered.
    #[test]
    fn other_markdown_is_left_alone() {
        let value = json!({
            "contents": { "kind": "markdown", "value": "- one\n- two\n\n*emphasis*" }
        });
        assert_eq!(parse_hover(&value), "- one\n- two\n\n*emphasis*");
    }

    /// A real answer from rust-analyzer, which puts the module path above the
    /// signature in its own fence.
    #[test]
    fn a_realistic_rust_analyzer_answer_reads_cleanly() {
        let value = json!({
            "contents": {
                "kind": "markdown",
                "value": "```rust\neditor_core::document\n```\n\n```rust\npub fn line_of(&self, offset: usize) -> usize\n```\n\n---\n\nThe line `offset` falls on.",
            }
        });
        assert_eq!(
            parse_hover(&value),
            "editor_core::document\n\npub fn line_of(&self, offset: usize) -> usize\n\n---\n\nThe line `offset` falls on."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `rust-analyzer` answers with `LocationLink`, `pyright` with `Location`,
    /// and a server with nothing to say answers `null`. Handling one shape
    /// means the feature works for one language and silently does nothing for
    /// the other.
    #[test]
    fn every_shape_the_protocol_allows_is_understood() {
        let uri = server::path_to_uri(Path::new("/project/main.py"));

        let single = json!({
            "uri": uri,
            "range": { "start": { "line": 4, "character": 8 },
                       "end": { "line": 4, "character": 13 } }
        });
        let from_single = parse_locations(&single);
        assert_eq!(from_single.len(), 1, "a bare Location");
        assert_eq!(from_single[0].line, 4);
        assert_eq!(from_single[0].column, 8);

        let array = json!([single]);
        assert_eq!(parse_locations(&array).len(), 1, "a Location array");

        let link = json!([{
            "targetUri": uri,
            "targetSelectionRange": { "start": { "line": 9, "character": 3 },
                                      "end": { "line": 9, "character": 7 } }
        }]);
        let from_link = parse_locations(&link);
        assert_eq!(from_link.len(), 1, "a LocationLink array");
        assert_eq!(from_link[0].line, 9);

        assert!(
            parse_locations(&serde_json::Value::Null).is_empty(),
            "null means the server had no answer"
        );
        assert!(parse_locations(&json!([])).is_empty());
    }

    #[test]
    fn a_malformed_entry_is_skipped_rather_than_taking_the_rest_with_it() {
        // One server sending something unexpected must not lose the results
        // from the entries either side of it.
        let uri = server::path_to_uri(Path::new("/project/main.py"));
        let mixed = json!([
            { "uri": uri, "range": { "start": { "line": 1, "character": 0 } } },
            { "nonsense": true },
            { "uri": uri, "range": { "start": { "line": 2, "character": 0 } } },
        ]);
        assert_eq!(parse_locations(&mixed).len(), 2);
    }

    #[test]
    fn asking_with_no_server_running_says_so_rather_than_waiting() {
        // The caller uses this to decide whether to fall back to the in-file
        // search; a silent false-positive would mean no answer ever appears.
        let mut lsp = Lsp::default();
        assert!(!lsp.ask(Query::Definition, Path::new("/project/main.py"), 0, 0));
    }

    /// Ruff serves Python and cannot answer either query. Sending it a
    /// definition request gets "method not found" and, worse, means the server
    /// that *can* answer is never reached.
    #[test]
    fn a_query_only_goes_to_a_server_that_advertises_it() {
        assert_eq!(Query::Definition.capability(), "definitionProvider");
        assert_eq!(Query::References.capability(), "referencesProvider");
    }

    /// Applying a rename's edits in document order shifts every range after
    /// the first. The file stays valid text and is quietly wrong, which is the
    /// worst way for a refactor to fail.
    #[test]
    fn a_files_edits_come_back_last_first() {
        let uri = server::path_to_uri(Path::new("/p/main.py"));
        let edit = |line: u64, col: u64| {
            json!({
                "range": {
                    "start": { "line": line, "character": col },
                    "end": { "line": line, "character": col + 3 }
                },
                "newText": "new"
            })
        };
        let result = json!({ "changes": { uri: [edit(1, 0), edit(9, 4), edit(9, 20)] } });

        let files = parse_workspace_edit(&result);
        assert_eq!(files.len(), 1);
        let lines: Vec<(u32, u32)> = files[0]
            .edits
            .iter()
            .map(|e| (e.start_line, e.start_column))
            .collect();
        assert_eq!(lines, [(9, 20), (9, 4), (1, 0)]);
    }

    #[test]
    fn both_shapes_of_workspace_edit_are_understood() {
        // Servers pick either; handling one silently renames nothing for the
        // other half of them.
        let uri = server::path_to_uri(Path::new("/p/main.py"));
        let edit = json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 3 }
            },
            "newText": "new"
        });

        let via_changes = json!({ "changes": { uri.clone(): [edit.clone()] } });
        assert_eq!(parse_workspace_edit(&via_changes).len(), 1, "changes");

        let via_documents = json!({
            "documentChanges": [
                { "textDocument": { "uri": uri, "version": 3 }, "edits": [edit] }
            ]
        });
        assert_eq!(
            parse_workspace_edit(&via_documents).len(),
            1,
            "documentChanges"
        );
    }

    #[test]
    fn a_file_creation_in_the_middle_of_a_rename_is_skipped() {
        // `documentChanges` may carry create/rename/delete operations. Renaming
        // a symbol does not produce them, and acting on one would be a
        // considerably larger surprise than ignoring it.
        let result = json!({
            "documentChanges": [{ "kind": "create", "uri": "file:///p/new.py" }]
        });
        assert!(parse_workspace_edit(&result).is_empty());
    }

    #[test]
    fn a_server_that_declines_yields_no_files_rather_than_an_error() {
        // Renaming a keyword, or a symbol the server cannot resolve.
        assert!(parse_workspace_edit(&serde_json::Value::Null).is_empty());
        assert!(parse_workspace_edit(&json!({})).is_empty());
    }

    #[test]
    fn the_two_queries_use_the_methods_the_protocol_defines() {
        assert_eq!(Query::Definition.method(), "textDocument/definition");
        assert_eq!(Query::References.method(), "textDocument/references");
    }

    /// A document as a test wants it, served by nobody.
    fn document(language_id: Option<&'static str>, version: i32) -> OpenDocument {
        OpenDocument {
            language_id,
            version,
            source_version: 1,
            text: Rope::from_str(""),
            serving: Vec::new(),
            told: Vec::new(),
        }
    }

    #[test]
    fn a_language_with_no_server_is_free() {
        // Opening a text file must not start anything. It is recorded, so that
        // it is not offered again on the next frame, and that is all.
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from(".")), Vec::new());
        lsp.sync(Path::new("/project/notes.txt"), 1, || "hello".to_owned());

        assert!(lsp.servers.is_empty());
        assert!(lsp.running().is_empty());
        assert!(
            lsp.documents[Path::new("/project/notes.txt")]
                .serving
                .is_empty()
        );
    }

    #[test]
    fn an_unchanged_document_is_not_even_asked_for_its_text() {
        // The application calls this every frame for every tab.
        let mut lsp = Lsp::default();
        let path = Path::new("/project/notes.txt");
        lsp.sync(path, 7, || "hello".to_owned());
        lsp.sync(path, 7, || panic!("the text was built for nothing"));
    }

    #[test]
    fn a_file_with_no_extension_is_ignored() {
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from(".")), Vec::new());
        lsp.sync(Path::new("/project/Makefile"), 1, || "all:".to_owned());
        assert!(lsp.servers.is_empty());
    }

    #[test]
    fn opening_without_a_project_root_starts_nothing() {
        // There is nowhere for a server to index.
        let mut lsp = Lsp::default();
        lsp.sync(Path::new("/loose/main.py"), 1, || "x = 1".to_owned());
        assert!(lsp.servers.is_empty());
    }

    /// The bug: `set_root` forgot every document, while the application kept
    /// its own list of what it had sent and so never sent anything again. Open
    /// tabs lost their language server for good after a virtual environment
    /// was created, or a folder opened with files already open.
    #[test]
    fn documents_are_offered_again_after_the_project_changes() {
        let mut lsp = Lsp::default();
        let path = Path::new("/a/main.py");
        lsp.set_root(Some(PathBuf::from("/a")), Vec::new());
        lsp.sync(path, 3, || "x = 1".to_owned());
        assert!(lsp.documents.contains_key(path));

        // A venv appeared: the tool search path changed.
        lsp.set_root(
            Some(PathBuf::from("/a")),
            vec![PathBuf::from("/a/.venv/bin")],
        );
        assert!(lsp.documents.is_empty());

        // The very next frame, with the document unchanged.
        let mut asked = false;
        lsp.sync(path, 3, || {
            asked = true;
            "x = 1".to_owned()
        });
        assert!(asked, "an unchanged document must still be re-sent");
        assert!(lsp.documents.contains_key(path));
    }

    /// Looking for a server runs it, which can take a second. That must not
    /// happen on the caller's thread.
    #[test]
    fn looking_for_a_server_does_not_block_the_caller() {
        let mut lsp = Lsp::default();
        lsp.set_root(Some(std::env::temp_dir()), Vec::new());
        lsp.sync(Path::new("/project/main.py"), 1, || "x = 1".to_owned());
        assert!(
            lsp.servers.is_empty(),
            "nothing may be started before the search has answered"
        );

        // Whatever the machine has installed, the search finishes and each
        // server ends up either running or recorded as missing.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !lsp.probing.is_empty() && std::time::Instant::now() < deadline {
            lsp.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(lsp.probing.is_empty(), "the search never finished");
        lsp.shutdown();
    }

    #[test]
    fn a_missing_server_is_recorded_once_rather_than_retried() {
        // Retrying discovery on every keystroke would hammer the filesystem.
        let mut lsp = Lsp::default();
        lsp.set_root(Some(std::env::temp_dir()), Vec::new());

        // Nothing is installed under this fake spec, so `python` resolution
        // will record whatever is genuinely absent.
        lsp.sync(Path::new("/project/a.py"), 1, || "x = 1".to_owned());
        let after_first = lsp.missing.len();
        lsp.sync(Path::new("/project/b.py"), 1, || "y = 2".to_owned());
        assert_eq!(
            lsp.missing.len(),
            after_first,
            "the same absent server must not be recorded twice"
        );
    }

    #[test]
    fn changing_a_file_that_was_never_opened_is_harmless() {
        let mut lsp = Lsp::default();
        lsp.change(Path::new("/project/never-opened.py"), 1, "x = 1");
        lsp.save(Path::new("/project/never-opened.py"), "x = 1");
        lsp.close(Path::new("/project/never-opened.py"));
    }

    #[test]
    fn closing_a_file_forgets_its_diagnostics() {
        let mut lsp = Lsp::default();
        let path = PathBuf::from("/project/main.py");
        lsp.diagnostics.set(
            &path,
            "ruff",
            vec![Diagnostic {
                severity: crate::diagnostics::Severity::Error,
                line: 0,
                column: 0,
                end_line: 0,
                end_column: 1,
                message: "x".to_owned(),
                code: None,
                source: "ruff".to_owned(),
            }],
        );
        assert!(!lsp.diagnostics.is_empty());

        lsp.close(&path);
        assert!(
            lsp.diagnostics.is_empty(),
            "a closed file must not keep problems in the panel"
        );
    }

    #[test]
    fn changing_the_project_root_shuts_everything_down() {
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from("/a")), Vec::new());
        lsp.documents
            .insert(PathBuf::from("/a/main.py"), document(Some("python"), 1));

        lsp.set_root(Some(PathBuf::from("/b")), Vec::new());
        assert!(
            lsp.documents.is_empty(),
            "documents from the previous project must not linger"
        );
    }

    #[test]
    fn setting_the_same_root_again_does_not_restart_everything() {
        // Session restore reopens the same folder; tearing down a working
        // rust-analyzer to start another would cost a full re-index.
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from("/a")), Vec::new());
        lsp.documents
            .insert(PathBuf::from("/a/main.py"), document(Some("python"), 1));

        lsp.set_root(Some(PathBuf::from("/a")), Vec::new());
        assert_eq!(lsp.documents.len(), 1, "nothing should have been torn down");
    }

    #[test]
    fn document_versions_increase_with_every_change() {
        // A server that sees a version go backwards, or repeat, may ignore the
        // change entirely.
        let mut lsp = Lsp::default();
        let path = PathBuf::from("/project/main.py");
        lsp.documents
            .insert(path.clone(), document(Some("python"), 1));

        lsp.change(&path, 2, "a");
        lsp.change(&path, 3, "ab");
        lsp.change(&path, 4, "abc");

        assert_eq!(lsp.documents[&path].version, 4);
    }

    #[test]
    fn a_known_document_is_changed_rather_than_opened_again() {
        // A second didOpen for the same URI is a protocol violation and some
        // servers respond by dropping the document entirely.
        let mut lsp = Lsp::default();
        let path = PathBuf::from("/project/main.py");
        lsp.documents
            .insert(path.clone(), document(Some("python"), 5));

        lsp.sync(&path, 2, || "different text".to_owned());
        assert_eq!(lsp.documents[&path].version, 6, "sent as a change");
        assert_eq!(lsp.documents[&path].text.to_string(), "different text");
    }

    #[test]
    fn shutdown_clears_everything() {
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from("/a")), Vec::new());
        lsp.documents
            .insert(PathBuf::from("/a/main.py"), document(Some("python"), 1));
        lsp.missing.push(registry::RUFF);

        lsp.shutdown();
        assert!(lsp.documents.is_empty());
        assert!(lsp.servers.is_empty());
        assert!(lsp.missing.is_empty());
        assert!(lsp.diagnostics.is_empty());
    }

    #[test]
    fn polling_with_nothing_running_is_harmless() {
        let mut lsp = Lsp::default();
        assert!(lsp.poll().is_empty());
    }

    /// A language server written in Python, for testing the client against
    /// something that really speaks the protocol over a real pipe.
    ///
    /// Depending on rust-analyzer or ruff being installed would mean the client
    /// is only ever tested on machines that happen to have them — and this
    /// machine does not: its `rust-analyzer` is a rustup proxy for an
    /// uninstalled component. Python is already required to test the run
    /// feature, so the mock costs nothing extra.
    const MOCK_SERVER: &str = r#"
import json, sys

def read():
    length = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b'content-length:'):
            length = int(line.split(b':')[1])
    return json.loads(sys.stdin.buffer.read(length))

def send(payload):
    body = json.dumps(payload).encode('utf-8')
    sys.stdout.buffer.write(b'Content-Length: %d\r\n\r\n' % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read()
    if message is None:
        break
    method = message.get('method')
    if method == 'initialize':
        send({'jsonrpc': '2.0', 'id': message['id'],
              'result': {'capabilities': {'textDocumentSync': 1}}})
    elif method == 'textDocument/didOpen':
        uri = message['params']['textDocument']['uri']
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics',
              'params': {'uri': uri, 'diagnostics': [{
                  'range': {'start': {'line': 2, 'character': 4},
                            'end': {'line': 2, 'character': 9}},
                  'severity': 1, 'code': 'E999', 'source': 'mock',
                  'message': 'a deliberate problem'}]}})
    elif method == 'textDocument/didChange':
        uri = message['params']['textDocument']['uri']
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics',
              'params': {'uri': uri, 'diagnostics': []}})
    elif method == 'exit':
        break
"#;

    fn mock_spec() -> ServerSpec {
        ServerSpec {
            id: "mock",
            name: "Mock",
            commands: &[],
            // Leaked so the args can be `&'static`, which the spec requires.
            // A test process is about to exit anyway.
            args: Box::leak(vec!["-c", MOCK_SERVER].into_boxed_slice()),
            provides: "a deliberate problem",
            install: "",
            version_arg: None,
        }
    }

    /// Drive the whole client against a server that really speaks the protocol:
    /// handshake, didOpen, a published diagnostic, then didChange clearing it.
    #[test]
    fn the_client_completes_a_handshake_and_receives_diagnostics() {
        let Some(python) = editor_proc_python() else {
            eprintln!("skipping: no Python available to run the mock server");
            return;
        };

        let root = std::env::temp_dir().join("the-editor-lsp-mock");
        std::fs::create_dir_all(&root).expect("create dir");
        let file = root.join("thing.py");
        std::fs::write(&file, b"a = 1\nb = 2\n    oops\n").expect("write");

        let mut server = Server::start(mock_spec(), python, &root).expect("the mock server starts");

        // Wait for the handshake.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut ready = false;
        while std::time::Instant::now() < deadline && !ready {
            for event in server.poll() {
                if matches!(event, crate::server::Event::Ready { .. }) {
                    ready = true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(ready, "the handshake never completed");
        assert!(server.is_ready());

        // Open a document and wait for its diagnostic.
        server
            .notify(
                "textDocument/didOpen",
                json!({ "textDocument": {
                    "uri": server::path_to_uri(&file),
                    "languageId": "python",
                    "version": 1,
                    "text": "a = 1\nb = 2\n    oops\n",
                }}),
            )
            .expect("notifies");

        let mut received = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline && received.is_none() {
            for event in server.poll() {
                if let crate::server::Event::Diagnostics { path, diagnostics } = event {
                    received = Some((path, diagnostics));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let (path, diagnostics) = received.expect("no diagnostic arrived");
        assert_eq!(
            path.canonicalize().ok(),
            file.canonicalize().ok(),
            "the URI did not round-trip back to the right file"
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "a deliberate problem");
        assert_eq!(diagnostics[0].range.start.line, 2);

        if let Some(reaper) = server.stop() {
            reaper.join().expect("the reaper finishes");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Answers `didOpen` with one diagnostic whose message is the first line of
    /// the text it was sent, at UTF-16 column 12 — so a test can see both *what*
    /// the client sent and whether it converted the column coming back. A
    /// `didOpen` that arrives before `initialized` says so in the message.
    const ECHO_SERVER: &str = r#"
import json, sys

def read():
    length = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b'content-length:'):
            length = int(line.split(b':')[1])
    return json.loads(sys.stdin.buffer.read(length))

def send(payload):
    body = json.dumps(payload).encode('utf-8')
    sys.stdout.buffer.write(b'Content-Length: %d\r\n\r\n' % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

initialized = False
while True:
    message = read()
    if message is None:
        break
    method = message.get('method')
    if method == 'initialize':
        send({'jsonrpc': '2.0', 'id': message['id'],
              'result': {'capabilities': {'textDocumentSync': 1}}})
    elif method == 'initialized':
        initialized = True
    elif method == 'textDocument/didOpen':
        doc = message['params']['textDocument']
        first = doc['text'].split('\n')[0]
        if not initialized:
            first = 'PROTOCOL VIOLATION: didOpen before initialized'
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics',
              'params': {'uri': doc['uri'], 'diagnostics': [{
                  'range': {'start': {'line': 0, 'character': 12},
                            'end': {'line': 0, 'character': 17}},
                  'severity': 1, 'message': first}]}})
    elif method == 'exit':
        break
"#;

    /// The server is sent the buffer, not the file on disk, only once its
    /// handshake is done, and its UTF-16 columns come back as characters.
    #[test]
    fn a_server_is_sent_the_buffer_after_its_handshake_and_its_columns_are_converted() {
        let Some(python) = editor_proc_python() else {
            eprintln!("skipping: no Python available to run the mock server");
            return;
        };
        let root = std::env::temp_dir().join("the-editor-lsp-echo");
        std::fs::create_dir_all(&root).expect("create dir");
        let file = root.join("thing.py");
        std::fs::write(&file, b"on disk, and out of date\n").expect("write");

        let spec = ServerSpec {
            id: "echo",
            name: "Echo",
            commands: &[],
            args: Box::leak(vec!["-c", ECHO_SERVER].into_boxed_slice()),
            provides: "",
            install: "",
            version_arg: None,
        };
        // No project root, so no real server that happens to be installed
        // here -- Ruff, say -- starts for the file and answers first.
        let mut lsp = Lsp::default();
        lsp.servers.insert(
            "echo",
            Server::start(spec, python, &root).expect("the mock server starts"),
        );

        // An emoji before `total`: character 11, UTF-16 column 12.
        let buffer = "print(\"\u{1F600}\", total)\n";
        lsp.sync(&file, 1, || buffer.to_owned());
        lsp.documents
            .get_mut(file.as_path())
            .expect("recorded")
            .serving
            .push("echo");
        // Offered now, while the server is certainly still starting: this must
        // wait for the handshake rather than go out ahead of it.
        lsp.attach(&file);
        assert!(lsp.documents[file.as_path()].told.is_empty());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline && lsp.diagnostics.for_file(&file).is_empty() {
            lsp.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let found = lsp.diagnostics.for_file(&file);
        let diagnostic = found.first().expect("no diagnostic arrived");
        assert_eq!(
            diagnostic.message, "print(\"\u{1F600}\", total)",
            "the server must be told what is in the buffer"
        );
        assert_eq!(diagnostic.column, 11, "UTF-16 column 12 is character 11");
        assert_eq!(diagnostic.end_column, 16);

        drop(lsp);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Starts a child of its own, writes the child's pid to the file named by
    /// its argument, and then ignores everything, including being told to exit
    /// and its input closing -- the server that has to be killed.
    const STUBBORN_SERVER: &str = r#"
import subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'])
open(sys.argv[1], 'w').write(str(child.pid))
while True:
    time.sleep(1)
"#;

    fn is_running(pid: &str) -> bool {
        if cfg!(windows) {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .expect("tasklist runs");
            String::from_utf8_lossy(&out.stdout).contains(pid)
        } else {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .is_ok_and(|s| s.success())
        }
    }

    /// A server that will not stop is killed, and so is everything it started:
    /// on Windows a server installed by npm is a `.cmd` shim, and killing only
    /// the process that was started leaves the server itself running.
    #[test]
    fn a_server_that_will_not_stop_is_killed_with_everything_it_started() {
        let Some(python) = editor_proc_python() else {
            eprintln!("skipping: no Python available to run the mock server");
            return;
        };
        let root = std::env::temp_dir().join("the-editor-lsp-stubborn");
        std::fs::create_dir_all(&root).expect("create dir");
        let pid_file = root.join("child.pid");
        let _ = std::fs::remove_file(&pid_file);

        let spec = ServerSpec {
            id: "stubborn",
            name: "Stubborn",
            commands: &[],
            args: Box::leak(
                vec![
                    "-c",
                    STUBBORN_SERVER,
                    Box::leak(pid_file.display().to_string().into_boxed_str()),
                ]
                .into_boxed_slice(),
            ),
            provides: "",
            install: "",
            version_arg: None,
        };
        let mut server = Server::start(spec, python, &root).expect("starts");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                && !pid.trim().is_empty()
            {
                break pid.trim().to_owned();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(is_running(&pid), "the child is running to begin with");

        server
            .stop()
            .expect("a thread to see it stopped")
            .join()
            .expect("the reaper finishes");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while is_running(&pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!is_running(&pid), "the server's own child outlived it");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A Python interpreter for the mock server, or `None`.
    fn editor_proc_python() -> Option<PathBuf> {
        for name in if cfg!(windows) {
            ["python", "python3"]
        } else {
            ["python3", "python"]
        } {
            if let Some(path) = registry::which(name, &[])
                && !path.to_string_lossy().contains("WindowsApps")
                && editor_proc::spawn::quiet(&path)
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success())
            {
                return Some(path);
            }
        }
        None
    }

    /// End-to-end against a real server: build a tiny Cargo project with a
    /// deliberate type error, open it, and wait for the diagnostic to arrive.
    ///
    /// Unit tests of the framing and the store cannot catch the handshake being
    /// subtly wrong, which is the part that actually breaks. Skipped where
    /// rust-analyzer is not installed.
    #[test]
    fn a_real_server_reports_a_real_error() {
        let Some(found) = registry::find(registry::RUST_ANALYZER, &[]) else {
            eprintln!("skipping: rust-analyzer is not installed");
            return;
        };
        let _ = found;

        let root = std::env::temp_dir().join("the-editor-lsp-e2e");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("src")).expect("create project");
        std::fs::write(
            root.join("Cargo.toml"),
            b"[package]\nname = \"e2e\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write manifest");
        // `let x: u32 = "text";` is a type error every version of rustc reports.
        let main = root.join("src").join("main.rs");
        let source = "fn main() {\n    let x: u32 = \"text\";\n    println!(\"{x}\");\n}\n";
        std::fs::write(&main, source).expect("write source");

        let mut lsp = Lsp::default();
        lsp.set_root(Some(root.clone()), Vec::new());
        lsp.sync(&main, 1, || source.to_owned());

        // rust-analyzer has to index and run `cargo check`, which is slow on a
        // cold cache.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            for notice in lsp.poll() {
                if matches!(notice, Notice::ServerReady(_)) {
                    ready = true;
                }
            }
            if !lsp.diagnostics.for_file(&main).is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        assert!(ready, "the handshake never completed");
        let diagnostics = lsp.diagnostics.for_file(&main);
        assert!(
            !diagnostics.is_empty(),
            "no diagnostic arrived for a file with an obvious type error"
        );
        assert!(
            diagnostics.iter().any(|d| d.line == 1),
            "the diagnostic should be on line 2 (zero-based 1): {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.severity == crate::diagnostics::Severity::Error),
            "a type error should be an error: {diagnostics:?}"
        );

        lsp.shutdown();
        std::fs::remove_dir_all(&root).ok();
    }
}
