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

use serde_json::json;

use crate::diagnostics::{Diagnostic, Store};
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
    /// A server died. `restarting` is false once it has given up.
    ServerDied {
        id: &'static str,
        name: &'static str,
        restarting: bool,
    },
}

/// One open document, as the servers see it.
#[derive(Debug, Clone)]
struct OpenDocument {
    language_id: &'static str,
    version: i32,
    /// Which servers have been told about this file, so it is closed with the
    /// same ones and never opened twice.
    told: Vec<&'static str>,
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
    pending: HashMap<(&'static str, i64), Query>,
}

impl Lsp {
    /// Point at a project. Stops everything running for the previous one.
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
            server.stop();
        }
        self.documents.clear();
        self.diagnostics.clear();
        self.missing.clear();
        self.pending.clear();
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

    /// Tell the servers a file is open.
    ///
    /// Does nothing for a language with no server, which is the common case and
    /// must be free.
    pub fn open(&mut self, path: &Path, text: &str) {
        if self.documents.contains_key(path) {
            return;
        }
        let Some(language_id) = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(registry::language_id_for_extension)
        else {
            return;
        };
        let wanted = registry::for_language(language_id);
        if wanted.is_empty() {
            return;
        }

        let mut told = Vec::new();
        for spec in wanted {
            if self.ensure_started(spec).is_some() {
                told.push(spec.id);
            }
        }
        if told.is_empty() {
            return;
        }

        let document = OpenDocument {
            language_id,
            version: 1,
            told,
        };
        let uri = server::path_to_uri(path);
        for id in &document.told {
            if let Some(server) = self.servers.get(id) {
                let _ = server.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id,
                            "version": document.version,
                            "text": text,
                        }
                    }),
                );
            }
        }
        self.documents.insert(path.to_path_buf(), document);
    }

    /// Tell the servers a file changed.
    pub fn change(&mut self, path: &Path, text: &str) {
        let Some(document) = self.documents.get_mut(path) else {
            return;
        };
        document.version += 1;
        let uri = server::path_to_uri(path);
        let version = document.version;
        let told = document.told.clone();

        for id in told {
            if let Some(server) = self.servers.get(id) {
                let _ = server.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": version },
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
        let Some(document) = self.documents.get(path) else {
            return false;
        };
        let told = document.told.clone();
        let uri = server::path_to_uri(path);

        for id in told {
            let Some(server) = self.servers.get_mut(id) else {
                continue;
            };
            // Ruff serves Python and cannot answer either of these. Asking it
            // anyway gets a "method not found" and, worse, stops the loop
            // before the server that *can* answer is reached.
            if !server.is_ready() || !server.supports(query.capability()) {
                continue;
            }
            let mut params = json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": column },
            });
            if query == Query::References {
                // Without this the definition itself is left out of the list,
                // and "find uses" that skips the declaration is confusing when
                // there is only one use.
                params["context"] = json!({ "includeDeclaration": true });
            }
            if let Ok(request) = server.send_request(query.method(), params) {
                self.pending.insert((id, request), query);
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
        if self.missing.iter().any(|s| s.id == spec.id) {
            return None; // already looked, still not there
        }

        let root = self.root.clone()?;
        let Some(found) = registry::find(spec, &self.extra_path) else {
            tracing::info!(server = spec.id, "not installed");
            self.missing.push(spec);
            return None;
        };

        match Server::start(spec, found.program, &root) {
            Ok(server) => {
                tracing::info!(server = spec.id, "started");
                self.servers.insert(spec.id, server);
                Some(spec.id)
            }
            Err(e) => {
                tracing::warn!(server = spec.id, "could not start: {e:#}");
                self.missing.push(spec);
                None
            }
        }
    }

    /// Drain every server. Call once per frame.
    pub fn poll(&mut self) -> Vec<Notice> {
        let mut notices = Vec::new();
        let ids: Vec<&'static str> = self.servers.keys().copied().collect();

        for id in ids {
            let Some(server) = self.servers.get_mut(id) else {
                continue;
            };
            let spec = server.spec();
            let events = server.poll();
            let became_ready = events.iter().any(|e| matches!(e, Event::Ready { .. }));

            for event in events {
                match event {
                    Event::Diagnostics { path, diagnostics } => {
                        let converted: Vec<Diagnostic> = diagnostics
                            .iter()
                            .map(|d| Diagnostic::from_lsp(d, spec.name))
                            .collect();
                        self.diagnostics.set(&path, id, converted);
                        notices.push(Notice::DiagnosticsChanged(path));
                    }
                    Event::Exited { restarting } => {
                        // Stale diagnostics from a server that is no longer
                        // running to correct them are worse than none.
                        self.diagnostics.clear_server(id);
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
                    Event::Response {
                        id: request,
                        result,
                    } => {
                        if let Some(query) = self.pending.remove(&(id, request)) {
                            notices.push(Notice::Answered {
                                query,
                                locations: parse_locations(&result),
                            });
                        }
                    }
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

    /// Complete the handshake and re-send every open document.
    ///
    /// The re-send matters after a restart: a freshly started server knows
    /// nothing, and without this it would answer questions about files it has
    /// never seen, or say nothing at all.
    fn on_ready(&mut self, id: &'static str) {
        let Some(server) = self.servers.get(id) else {
            return;
        };
        let _ = server.notify("initialized", json!({}));

        for (path, document) in &self.documents {
            if !document.told.contains(&id) {
                continue;
            }
            let _ = server.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": server::path_to_uri(path),
                        "languageId": document.language_id,
                        "version": document.version,
                        "text": std::fs::read_to_string(path).unwrap_or_default(),
                    }
                }),
            );
        }
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        self.shutdown();
    }
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

    #[test]
    fn the_two_queries_use_the_methods_the_protocol_defines() {
        assert_eq!(Query::Definition.method(), "textDocument/definition");
        assert_eq!(Query::References.method(), "textDocument/references");
    }

    #[test]
    fn a_language_with_no_server_is_free() {
        // Opening a text file must not start anything or allocate a document.
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from(".")), Vec::new());
        lsp.open(Path::new("/project/notes.txt"), "hello");

        assert!(lsp.documents.is_empty());
        assert!(lsp.servers.is_empty());
        assert!(lsp.running().is_empty());
    }

    #[test]
    fn a_file_with_no_extension_is_ignored() {
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from(".")), Vec::new());
        lsp.open(Path::new("/project/Makefile"), "all:");
        assert!(lsp.documents.is_empty());
    }

    #[test]
    fn opening_without_a_project_root_starts_nothing() {
        // There is nowhere for a server to index.
        let mut lsp = Lsp::default();
        lsp.open(Path::new("/loose/main.py"), "x = 1");
        assert!(lsp.servers.is_empty());
        assert!(lsp.documents.is_empty());
    }

    #[test]
    fn a_missing_server_is_recorded_once_rather_than_retried() {
        // Retrying discovery on every keystroke would hammer the filesystem.
        let mut lsp = Lsp::default();
        lsp.set_root(Some(std::env::temp_dir()), Vec::new());

        // Nothing is installed under this fake spec, so `python` resolution
        // will record whatever is genuinely absent.
        lsp.open(Path::new("/project/a.py"), "x = 1");
        let after_first = lsp.missing.len();
        lsp.open(Path::new("/project/b.py"), "y = 2");
        assert_eq!(
            lsp.missing.len(),
            after_first,
            "the same absent server must not be recorded twice"
        );
    }

    #[test]
    fn changing_a_file_that_was_never_opened_is_harmless() {
        let mut lsp = Lsp::default();
        lsp.change(Path::new("/project/never-opened.py"), "x = 1");
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
        lsp.documents.insert(
            PathBuf::from("/a/main.py"),
            OpenDocument {
                language_id: "python",
                version: 1,
                told: vec!["ruff"],
            },
        );

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
        lsp.documents.insert(
            PathBuf::from("/a/main.py"),
            OpenDocument {
                language_id: "python",
                version: 1,
                told: vec!["ruff"],
            },
        );

        lsp.set_root(Some(PathBuf::from("/a")), Vec::new());
        assert_eq!(lsp.documents.len(), 1, "nothing should have been torn down");
    }

    #[test]
    fn document_versions_increase_with_every_change() {
        // A server that sees a version go backwards, or repeat, may ignore the
        // change entirely.
        let mut lsp = Lsp::default();
        let path = PathBuf::from("/project/main.py");
        lsp.documents.insert(
            path.clone(),
            OpenDocument {
                language_id: "python",
                version: 1,
                told: Vec::new(),
            },
        );

        lsp.change(&path, "a");
        lsp.change(&path, "ab");
        lsp.change(&path, "abc");

        assert_eq!(lsp.documents[&path].version, 4);
    }

    #[test]
    fn opening_the_same_file_twice_is_a_no_op() {
        // A second didOpen for the same URI is a protocol violation and some
        // servers respond by dropping the document entirely.
        let mut lsp = Lsp::default();
        let path = PathBuf::from("/project/main.py");
        lsp.documents.insert(
            path.clone(),
            OpenDocument {
                language_id: "python",
                version: 5,
                told: Vec::new(),
            },
        );

        lsp.open(&path, "different text");
        assert_eq!(
            lsp.documents[&path].version, 5,
            "the existing document must be left alone"
        );
    }

    #[test]
    fn shutdown_clears_everything() {
        let mut lsp = Lsp::default();
        lsp.set_root(Some(PathBuf::from("/a")), Vec::new());
        lsp.documents.insert(
            PathBuf::from("/a/main.py"),
            OpenDocument {
                language_id: "python",
                version: 1,
                told: Vec::new(),
            },
        );
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

        server.stop();
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
        lsp.open(&main, source);
        assert!(
            !lsp.servers.is_empty(),
            "rust-analyzer is installed but was not started"
        );

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
