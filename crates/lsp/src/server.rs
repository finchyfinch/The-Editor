//! Running one language server: spawn, handshake, talk, restart when it dies.
//!
//! Threads and channels rather than an async runtime. The plan called for
//! tokio, but everything else that talks to a child process — the run console —
//! is already built this way, and a handful of language servers does not need
//! a scheduler. One concurrency model in the codebase is worth more than the
//! theoretical efficiency of a second.
//!
//! Language servers crash. `rust-analyzer` runs out of memory on a large
//! workspace; a Python server hits a file it cannot parse. A crash must cost
//! the user their completions for a few seconds, not their editor — so a dead
//! server restarts with backoff, and gives up after a few attempts rather than
//! spinning forever.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::registry::ServerSpec;
use crate::transport;

/// How many times to restart a server that keeps dying before giving up.
const MAX_RESTARTS: u32 = 3;
/// Backoff between restarts, indexed by attempt.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(10),
];
/// A server that has run this long is considered to have started successfully,
/// so the restart counter resets. Without this, a server that works fine for an
/// hour and then crashes gets treated as the fourth failure of a broken one.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// Something the server told us.
#[derive(Debug, Clone)]
pub enum Event {
    /// The handshake completed; the server is ready for documents.
    ///
    /// Carries what the server said it can do, because the message is parsed
    /// on the reader thread, which has no access to the `Server` to store it.
    Ready { capabilities: Value },
    /// Diagnostics for one file.
    Diagnostics {
        path: PathBuf,
        diagnostics: Vec<lsp_types::Diagnostic>,
    },
    /// A response to a request we sent.
    Response { id: i64, result: Value },
    /// A request that failed.
    Error { id: i64, message: String },
    /// A request *from* the server, which must be answered or it waits.
    Request {
        id: i64,
        method: String,
        params: Value,
    },
    /// Something the server logged.
    Log(String),
    /// The process ended.
    Exited { restarting: bool },
}

/// What The Editor asks a Python server to do.
///
/// basedpyright's own default is its "recommended" mode, which turns on rules
/// pyright leaves off — import-cycle reporting among them — and treats a great
/// deal as an error. On a real project using libraries whose stubs do not
/// describe them fully, that produces hundreds of findings that are true of the
/// stubs and false of the code, and a Problems panel nobody reads is worth less
/// than none.
///
/// `standard` is pyright's own default and the level its documentation
/// describes. `openFilesOnly` keeps a server from reporting on files the user
/// has not opened.
fn configuration_for(section: &str) -> Value {
    // Servers ask for a dotted section and expect just that subtree back.
    let analysis = json!({
        "typeCheckingMode": "standard",
        "diagnosticMode": "openFilesOnly",
        "diagnosticSeverityOverrides": {
            "reportImportCycles": "none",
        },
    });
    match section {
        "python" | "basedpyright" | "pyright" => json!({ "analysis": analysis }),
        "python.analysis" | "basedpyright.analysis" | "pyright.analysis" => analysis,
        _ => Value::Null,
    }
}

/// Whether a capabilities object advertises a capability.
///
/// A free function so it can be tested without standing up a server process.
/// The protocol allows `true` or an options object, and both mean yes; only
/// `false`, `null` and absence mean no. rust-analyzer sends an options object
/// for `referencesProvider` where pyright sends `true`, so handling only the
/// boolean silently disables the feature for one of them.
fn advertises(capabilities: &Value, capability: &str) -> bool {
    match capabilities.get(capability) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(supported)) => *supported,
        Some(_) => true,
    }
}

/// A running language server.
pub struct Server {
    spec: ServerSpec,
    program: PathBuf,
    root: PathBuf,
    child: Option<Child>,
    outgoing: Option<Sender<String>>,
    events: Receiver<Event>,
    events_tx: Sender<Event>,
    next_id: i64,
    started_at: Instant,
    restarts: u32,
    /// When to try starting again after a crash.
    retry_at: Option<Instant>,
    ready: bool,
    /// What the server said it can do, from the initialize response.
    ///
    /// Kept because "which server should answer this question" cannot be
    /// decided from the registry: Ruff is a linter and serves Python, but
    /// asking it where something is defined gets an error at best. Only the
    /// server itself knows.
    capabilities: Value,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("id", &self.spec.id)
            .field("ready", &self.ready)
            .field("restarts", &self.restarts)
            .finish()
    }
}

impl Server {
    /// Start a server for a project.
    ///
    /// # Errors
    /// If the process cannot be spawned.
    pub fn start(spec: ServerSpec, program: PathBuf, root: &Path) -> Result<Self> {
        let (events_tx, events) = channel();
        let mut server = Self {
            spec,
            program,
            root: root.to_path_buf(),
            child: None,
            outgoing: None,
            events,
            events_tx,
            next_id: 1,
            started_at: Instant::now(),
            restarts: 0,
            retry_at: None,
            ready: false,
            capabilities: Value::Null,
        };
        server.spawn()?;
        Ok(server)
    }

    fn spawn(&mut self) -> Result<()> {
        // `quiet`, not `Command::new`: a release build has no console of its
        // own, so Windows would allocate one for the server and leave it on
        // screen behind the editor for as long as the server runs.
        let mut child = editor_proc::spawn::quiet(&self.program)
            .args(self.spec.args)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", self.program.display()))?;

        let stdin = child.stdin.take().context("the server has no stdin")?;
        let stdout = child.stdout.take().context("the server has no stdout")?;
        let stderr = child.stderr.take().context("the server has no stderr")?;

        let (outgoing, to_send) = channel::<String>();
        spawn_writer(self.spec.id, stdin, to_send);
        spawn_reader(self.spec.id, stdout, self.events_tx.clone());
        spawn_stderr_drain(self.spec.id, stderr, self.events_tx.clone());

        self.child = Some(child);
        self.outgoing = Some(outgoing);
        self.started_at = Instant::now();
        self.ready = false;

        self.send_initialize()?;
        Ok(())
    }

    fn send_initialize(&mut self) -> Result<()> {
        let root_uri = path_to_uri(&self.root);
        let id = self.take_id();
        // A deliberately modest set of capabilities. Claiming support for
        // something not implemented makes servers send things that are then
        // silently dropped, which looks like the server misbehaving.
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "The Editor", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
            "capabilities": {
                "textDocument": {
                    "synchronization": { "didSave": true, "dynamicRegistration": false },
                    "publishDiagnostics": { "relatedInformation": false },
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "completion": {
                        "completionItem": { "snippetSupport": false },
                        "contextSupport": false
                    },
                    "definition": { "dynamicRegistration": false },
                },
                // Declared so servers ask rather than assuming their own
                // defaults; see `configuration_for`.
                "workspace": { "workspaceFolders": true, "configuration": true },
            },
        });
        self.request(id, "initialize", params)?;
        Ok(())
    }

    fn take_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Send a request and return the id its response will carry.
    ///
    /// # Errors
    /// If the server is not running.
    pub fn request(&mut self, id: i64, method: &str, params: Value) -> Result<i64> {
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&message)?;
        Ok(id)
    }

    /// Whether the server advertised a capability, e.g. `definitionProvider`.
    ///
    /// The protocol allows `true` or an options object, and both mean yes; only
    /// `false`, `null` and absence mean no.
    #[must_use]
    pub fn supports(&self, capability: &str) -> bool {
        advertises(&self.capabilities, capability)
    }

    /// Send a request with a freshly allocated id.
    ///
    /// # Errors
    /// If the server is not running.
    pub fn send_request(&mut self, method: &str, params: Value) -> Result<i64> {
        let id = self.take_id();
        self.request(id, method, params)
    }

    /// Answer a request the server made of us.
    ///
    /// Only `workspace/configuration` is answered with anything; everything
    /// else gets `null`, which the protocol allows and every server copes with.
    /// The point is to answer *at all* — an unanswered request leaves some
    /// servers waiting indefinitely.
    fn answer(&self, id: i64, method: &str, params: &Value) {
        let result = if method == "workspace/configuration" {
            let sections = params
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Value::Array(
                sections
                    .iter()
                    .map(|item| {
                        item.get("section")
                            .and_then(Value::as_str)
                            .map_or(Value::Null, configuration_for)
                    })
                    .collect(),
            )
        } else {
            Value::Null
        };

        let _ = self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }));
    }

    /// Send a notification, which expects no reply.
    ///
    /// # Errors
    /// If the server is not running.
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn send(&self, message: &Value) -> Result<()> {
        let outgoing = self
            .outgoing
            .as_ref()
            .context("the language server is not running")?;
        outgoing
            .send(message.to_string())
            .map_err(|_| anyhow::anyhow!("the language server stopped listening"))
    }

    #[must_use]
    pub fn spec(&self) -> ServerSpec {
        self.spec
    }

    #[must_use]
    pub fn id(&self) -> &'static str {
        self.spec.id
    }

    /// True once the handshake has completed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// True while the server is up, or waiting to be restarted.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.outgoing.is_some() || self.retry_at.is_some()
    }

    /// How many times this server has been restarted after crashing.
    #[must_use]
    pub fn restart_count(&self) -> u32 {
        self.restarts
    }

    /// Take everything the server has said, and handle restarts.
    ///
    /// Never blocks: called once per frame.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            match &event {
                Event::Ready { capabilities } => {
                    self.ready = true;
                    self.capabilities = capabilities.clone();
                }
                Event::Request { id, method, params } => {
                    // Answered here rather than passed up: none of these are
                    // decisions the application makes, and a server left
                    // waiting on one stops answering anything else.
                    self.answer(*id, method, params);
                    continue;
                }
                Event::Exited { .. } => {
                    self.on_exit(&mut events);
                    continue;
                }
                _ => {}
            }
            events.push(event);
        }

        // Restart when the backoff has elapsed.
        if let Some(retry_at) = self.retry_at
            && Instant::now() >= retry_at
        {
            self.retry_at = None;
            match self.spawn() {
                Ok(()) => {
                    tracing::info!(server = self.spec.id, "restarted");
                    events.push(Event::Log(format!("{} restarted", self.spec.name)));
                }
                Err(e) => {
                    tracing::warn!(server = self.spec.id, "could not restart: {e:#}");
                    events.push(Event::Exited { restarting: false });
                }
            }
        }

        events
    }

    fn on_exit(&mut self, events: &mut Vec<Event>) {
        self.outgoing = None;
        self.ready = false;
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }

        // A server that ran for a while before dying is not a broken server; it
        // is a working one that hit something. Reset the counter so it is not
        // condemned for a fault an hour into the session.
        if self.started_at.elapsed() >= HEALTHY_AFTER {
            self.restarts = 0;
        }

        if self.restarts >= MAX_RESTARTS {
            tracing::error!(
                server = self.spec.id,
                "gave up after {MAX_RESTARTS} restarts"
            );
            events.push(Event::Exited { restarting: false });
            return;
        }

        let delay = BACKOFF
            .get(self.restarts as usize)
            .copied()
            .unwrap_or(Duration::from_secs(10));
        self.restarts += 1;
        self.retry_at = Some(Instant::now() + delay);
        tracing::warn!(
            server = self.spec.id,
            "exited; restarting in {:?} (attempt {})",
            delay,
            self.restarts
        );
        events.push(Event::Exited { restarting: true });
    }

    /// Ask the server to shut down, then make sure it does.
    pub fn stop(&mut self) {
        // Politely first: a server told to shut down flushes its state, which
        // matters for ones that cache an index on disk.
        let _ = self.notify("exit", json!(null));
        self.outgoing = None;
        self.retry_at = None;

        if let Some(mut child) = self.child.take() {
            // Give it a moment, then insist.
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // A dropped server would otherwise be left running, holding a
        // workspace index and a few hundred megabytes.
        self.stop();
    }
}

fn spawn_writer(id: &'static str, mut stdin: std::process::ChildStdin, messages: Receiver<String>) {
    let _ = std::thread::Builder::new()
        .name(format!("lsp-write({id})"))
        .spawn(move || {
            while let Ok(message) = messages.recv() {
                if transport::write_message(&mut stdin, &message).is_err() {
                    break;
                }
            }
            let _ = stdin.flush();
        });
}

fn spawn_reader(id: &'static str, stdout: std::process::ChildStdout, events: Sender<Event>) {
    let _ = std::thread::Builder::new()
        .name(format!("lsp-read({id})"))
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match transport::read_message(&mut reader) {
                    Ok(Some(text)) => {
                        for event in parse(&text, id) {
                            if events.send(event).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(server = id, "protocol error: {e:#}");
                        break;
                    }
                }
            }
            let _ = events.send(Event::Exited { restarting: false });
        });
}

/// Drain stderr so a chatty server cannot fill its pipe buffer and block.
fn spawn_stderr_drain(id: &'static str, stderr: std::process::ChildStderr, events: Sender<Event>) {
    let _ = std::thread::Builder::new()
        .name(format!("lsp-err({id})"))
        .spawn(move || {
            use std::io::BufRead;
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                tracing::debug!(server = id, "{line}");
                if events.send(Event::Log(line)).is_err() {
                    return;
                }
            }
        });
}

/// Turn one incoming JSON-RPC message into events.
fn parse(text: &str, server_id: &str) -> Vec<Event> {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        tracing::warn!(server = server_id, "unparseable message");
        return Vec::new();
    };

    // A response to something we sent.
    if let Some(id) = message.get("id").and_then(Value::as_i64) {
        if let Some(error) = message.get("error") {
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            return vec![Event::Error {
                id,
                message: text.to_owned(),
            }];
        }
        if let Some(result) = message.get("result") {
            // The initialize response is the handshake completing.
            let mut events = vec![Event::Response {
                id,
                result: result.clone(),
            }];
            if let Some(capabilities) = result.get("capabilities") {
                events.push(Event::Ready {
                    capabilities: capabilities.clone(),
                });
            }
            return events;
        }
        // A request from the server. It has to be answered: a server that asks
        // for its configuration and never hears back falls through to its own
        // defaults, which for basedpyright means its strictest mode.
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            return vec![Event::Request {
                id,
                method: method.to_owned(),
                params: message.get("params").cloned().unwrap_or(Value::Null),
            }];
        }
        return Vec::new();
    }

    // A notification.
    match message.get("method").and_then(Value::as_str) {
        Some("textDocument/publishDiagnostics") => {
            let Some(params) = message.get("params") else {
                return Vec::new();
            };
            let Some(path) = params
                .get("uri")
                .and_then(Value::as_str)
                .and_then(uri_to_path)
            else {
                return Vec::new();
            };
            let diagnostics = params
                .get("diagnostics")
                .and_then(|d| serde_json::from_value(d.clone()).ok())
                .unwrap_or_default();
            vec![Event::Diagnostics { path, diagnostics }]
        }
        Some("window/logMessage" | "window/showMessage") => message
            .get("params")
            .and_then(|p| p.get("message"))
            .and_then(Value::as_str)
            .map(|m| vec![Event::Log(m.to_owned())])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Convert a path to a `file://` URI.
///
/// Hand-rolled rather than pulled from `url`: the rules that matter are few,
/// and the Windows ones — a leading slash before the drive letter, backslashes
/// converted — are exactly what a general-purpose crate gets right and a naive
/// `format!` gets wrong.
#[must_use]
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        // A Windows path like `C:/x` becomes `file:///C:/x`.
        out.push('/');
    }
    for c in text.chars() {
        match c {
            // Unreserved characters, plus the ones a path legitimately needs.
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' | ':' => out.push(c),
            _ => {
                let mut buffer = [0u8; 4];
                for byte in c.encode_utf8(&mut buffer).as_bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}

/// Convert a `file://` URI back to a path.
#[must_use]
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);

    // `/C:/x` on Windows is `C:/x`; a leading slash elsewhere is part of the
    // path.
    let trimmed = decoded
        .strip_prefix('/')
        .filter(|r| {
            let mut chars = r.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':')
        })
        .unwrap_or(&decoded);

    Some(PathBuf::from(trimmed))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unix_path_becomes_a_file_uri_and_back() {
        let path = Path::new("/home/gareth/project/main.py");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///home/gareth/project/main.py");
        assert_eq!(uri_to_path(&uri), Some(path.to_path_buf()));
    }

    #[test]
    fn a_windows_path_gets_the_extra_slash_before_the_drive() {
        // `file://C:/x` is wrong — the host part would be `C:`. Servers reject
        // it or, worse, quietly treat the document as a different file.
        let uri = path_to_uri(Path::new(r"C:\project\src\main.rs"));
        assert_eq!(uri, "file:///C:/project/src/main.rs");
        assert_eq!(
            uri_to_path(&uri),
            Some(PathBuf::from("C:/project/src/main.rs"))
        );
    }

    #[test]
    fn spaces_and_other_characters_are_percent_encoded() {
        let uri = path_to_uri(Path::new("/home/a b/c#d/e.py"));
        assert!(
            !uri.contains(' '),
            "a raw space makes an invalid URI: {uri}"
        );
        assert!(uri.contains("%20"));
        assert_eq!(
            uri_to_path(&uri),
            Some(PathBuf::from("/home/a b/c#d/e.py")),
            "the round trip must survive encoding"
        );
    }

    #[test]
    fn non_ascii_paths_round_trip() {
        let path = Path::new("/home/caf\u{e9}/na\u{ef}ve.py");
        let uri = path_to_uri(path);
        assert!(uri.is_ascii(), "a URI must be ASCII: {uri}");
        assert_eq!(uri_to_path(&uri), Some(path.to_path_buf()));
    }

    #[test]
    fn a_uri_that_is_not_a_file_is_rejected() {
        assert_eq!(uri_to_path("https://example.com/x"), None);
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
    }

    #[test]
    fn a_diagnostics_notification_is_parsed() {
        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///project/main.py",
                "diagnostics": [{
                    "range": {
                        "start": { "line": 3, "character": 0 },
                        "end": { "line": 3, "character": 10 }
                    },
                    "severity": 1,
                    "code": "F401",
                    "source": "Ruff",
                    "message": "'os' imported but unused"
                }]
            }
        })
        .to_string();

        let events = parse(&message, "ruff");
        match events.as_slice() {
            [Event::Diagnostics { path, diagnostics }] => {
                assert_eq!(path, &PathBuf::from("/project/main.py"));
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0].message, "'os' imported but unused");
            }
            other => panic!("expected diagnostics, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_diagnostics_notification_is_still_delivered() {
        // This is how a server says "all fixed"; dropping it would leave stale
        // problems on screen forever.
        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///project/main.py", "diagnostics": [] }
        })
        .to_string();

        match parse(&message, "ruff").as_slice() {
            [Event::Diagnostics { diagnostics, .. }] => assert!(diagnostics.is_empty()),
            other => panic!("expected an empty diagnostics event, got {other:?}"),
        }
    }

    #[test]
    fn a_capability_may_be_true_or_an_options_object() {
        let capabilities = json!({
            "definitionProvider": true,
            "referencesProvider": { "workDoneProgress": false },
            "renameProvider": false,
            "hoverProvider": null,
        });
        assert!(advertises(&capabilities, "definitionProvider"));
        assert!(
            advertises(&capabilities, "referencesProvider"),
            "an options object means yes"
        );
        assert!(!advertises(&capabilities, "renameProvider"), "false is no");
        assert!(!advertises(&capabilities, "hoverProvider"), "null is no");
        assert!(!advertises(&capabilities, "neverHeardOfIt"), "absent is no");
        assert!(
            !advertises(&Value::Null, "definitionProvider"),
            "nothing is supported before the handshake"
        );
    }

    #[test]
    fn the_initialize_response_signals_readiness() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "capabilities": { "textDocumentSync": 1 } }
        })
        .to_string();

        let events = parse(&message, "test");
        assert!(
            events.iter().any(|e| matches!(e, Event::Ready { .. })),
            "got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Response { id: 1, .. }))
        );
    }

    #[test]
    fn an_ordinary_response_does_not_signal_readiness() {
        let message = json!({ "jsonrpc": "2.0", "id": 7, "result": [] }).to_string();
        let events = parse(&message, "test");
        assert!(!events.iter().any(|e| matches!(e, Event::Ready { .. })));
    }

    #[test]
    fn an_error_response_is_reported_against_its_request() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": { "code": -32601, "message": "method not found" }
        })
        .to_string();

        match parse(&message, "test").as_slice() {
            [Event::Error { id: 9, message }] => assert_eq!(message, "method not found"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn log_messages_are_captured() {
        let message = json!({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": { "type": 3, "message": "indexing finished" }
        })
        .to_string();

        match parse(&message, "test").as_slice() {
            [Event::Log(text)] => assert_eq!(text, "indexing finished"),
            other => panic!("expected a log, got {other:?}"),
        }
    }

    #[test]
    fn unknown_notifications_are_ignored_rather_than_failing() {
        // Servers send plenty we do not handle; each must be a no-op.
        let message = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": { "token": "x", "value": {} }
        })
        .to_string();
        assert!(parse(&message, "test").is_empty());
    }

    #[test]
    fn a_request_from_the_server_is_recognised_rather_than_misread_as_a_response() {
        // `workspace/configuration` has both an id and a method. Treating it as
        // a response would route it to whatever request happens to share the id;
        // ignoring it, which is what this used to do, leaves the server to fall
        // back on its own defaults.
        let message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "workspace/configuration",
            "params": { "items": [{ "section": "python.analysis" }] }
        })
        .to_string();
        let events = parse(&message, "test");
        assert!(
            matches!(events.as_slice(), [Event::Request { id: 1, method, .. }] if method == "workspace/configuration"),
            "got {events:?}"
        );
    }

    /// basedpyright's own default is its strictest mode, which on a project
    /// using libraries whose stubs are incomplete reports hundreds of findings
    /// that are true of the stubs and false of the code.
    #[test]
    fn python_servers_are_asked_for_the_standard_type_checking_mode() {
        let analysis = configuration_for("python.analysis");
        assert_eq!(analysis["typeCheckingMode"], "standard");
        assert_eq!(analysis["diagnosticMode"], "openFilesOnly");
        assert_eq!(
            analysis["diagnosticSeverityOverrides"]["reportImportCycles"],
            "none"
        );

        // Asked for by the parent section, the same settings arrive nested.
        assert_eq!(
            configuration_for("basedpyright")["analysis"]["typeCheckingMode"],
            "standard"
        );
    }

    #[test]
    fn a_section_we_have_nothing_to_say_about_is_answered_with_null() {
        // The protocol requires one entry per requested item; skipping one
        // shifts every later answer onto the wrong section.
        assert_eq!(configuration_for("editor.wibble"), Value::Null);
    }

    #[test]
    fn malformed_json_is_dropped_rather_than_crashing_the_reader() {
        assert!(parse("not json at all", "test").is_empty());
        assert!(parse("", "test").is_empty());
        assert!(parse("{}", "test").is_empty());
    }

    #[test]
    fn the_backoff_grows_and_is_capped() {
        assert!(BACKOFF[0] < BACKOFF[1]);
        assert!(BACKOFF[1] < BACKOFF[2]);
        assert_eq!(
            BACKOFF.len(),
            MAX_RESTARTS as usize,
            "there should be a delay for every attempt"
        );
    }

    #[test]
    fn starting_a_server_that_does_not_exist_is_an_error_not_a_panic() {
        let spec = ServerSpec {
            id: "missing",
            name: "Missing",
            commands: &["definitely-not-a-real-language-server-xyzzy"],
            args: &[],
            provides: "nothing",
            install: "",
            version_arg: None,
        };
        let result = Server::start(
            spec,
            PathBuf::from("definitely-not-a-real-language-server-xyzzy"),
            Path::new("."),
        );
        assert!(result.is_err());
    }
}
