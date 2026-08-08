//! One debug session: launch, breakpoints, stepping, stack and variables.
//!
//! DAP is request/response plus events, over the same framing as LSP. Requests
//! are numbered and answered out of order, so what a reply *means* has to be
//! remembered when it is sent — the same bookkeeping the language-server client
//! does, for the same reason.
//!
//! The state machine is small and worth stating, because every button in the
//! UI is enabled or disabled by it:
//!
//! ```text
//!   Starting ──initialized──▶ Running ──stopped──▶ Paused
//!                              ▲                     │
//!                              └───continue/step─────┘
//!   any state ──terminated/exited──▶ Finished
//! ```
//!
//! Breakpoints are owned by the application, not by this session: they outlive
//! it, they exist before one starts, and a file's set has to be re-sent whenever
//! it changes. [`Session::set_breakpoints`] takes the whole set for one file,
//! which is what the protocol wants anyway.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Where a session has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Spawned, handshake not finished.
    Starting,
    /// The program is running; nothing to inspect.
    Running,
    /// Stopped at a breakpoint or after a step. The stack can be read.
    Paused,
    /// Over. The process has gone.
    Finished,
}

/// A breakpoint the user placed, in a file, one-based like the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Breakpoint {
    pub line: usize,
    /// False once the adapter says it could not bind it — a line with no code
    /// on it, most often. Shown differently, because a breakpoint that will
    /// never be hit should not look like one that will.
    pub verified: bool,
}

/// One frame of the call stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub line: usize,
}

/// One name and its value, from a scope of the selected frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub kind: Option<String>,
}

/// Something the application should react to.
#[derive(Debug, Clone)]
pub enum DebugEvent {
    /// The state changed; redraw the buttons.
    StateChanged(State),
    /// The program stopped. Carries why, for the status line.
    Paused { reason: String },
    /// A fresh call stack, innermost first.
    Stack(Vec<Frame>),
    /// Variables for the frame last asked about.
    Variables(Vec<Variable>),
    /// The adapter reported which breakpoints it could actually bind.
    BreakpointsVerified {
        path: PathBuf,
        lines: Vec<Breakpoint>,
    },
    /// Output from the program, for the run console.
    Output(String),
    /// Something went wrong, phrased for a person.
    Failed(String),
}

/// What a sent request was about, so its reply can be understood.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Awaiting {
    Initialize,
    Launch,
    SetBreakpoints(PathBuf),
    StackTrace,
    Scopes,
    Variables,
    /// Fire and forget: continue, step, disconnect.
    Ignored,
}

/// A running debug session.
pub struct Session {
    child: Option<Child>,
    outgoing: Option<Sender<String>>,
    events: Receiver<Wire>,
    state: State,
    next_seq: i64,
    awaiting: std::collections::HashMap<i64, Awaiting>,
    /// The thread the adapter stopped, needed by every stack and step request.
    stopped_thread: Option<i64>,
    /// The frame whose variables are on screen.
    selected_frame: Option<i64>,
    /// Sent once `initialized` arrives, because breakpoints cannot be set
    /// before then and the user has usually placed them before starting.
    pending_breakpoints: Vec<(PathBuf, Vec<usize>)>,
    launch: Value,
    launched: bool,
}

/// A message off the wire, before it means anything.
#[derive(Debug)]
enum Wire {
    Message(Value),
    Closed,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("stopped_thread", &self.stopped_thread)
            .finish()
    }
}

impl Session {
    /// Start the adapter and ask it to launch `program`.
    ///
    /// # Errors
    /// If the adapter cannot be spawned.
    pub fn launch(interpreter: &Path, program: &Path, cwd: &Path, args: &[String]) -> Result<Self> {
        let mut child = editor_proc::spawn::quiet(interpreter)
            .args(crate::adapter::adapter_args())
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting debugpy with {}", interpreter.display()))?;

        let stdin = child.stdin.take().context("the adapter has no stdin")?;
        let stdout = child.stdout.take().context("the adapter has no stdout")?;

        let (outgoing, to_send) = channel::<String>();
        let (events_tx, events) = channel();
        spawn_writer(stdin, to_send);
        spawn_reader(stdout, events_tx);

        let mut session = Self {
            child: Some(child),
            outgoing: Some(outgoing),
            events,
            state: State::Starting,
            next_seq: 1,
            awaiting: std::collections::HashMap::new(),
            stopped_thread: None,
            selected_frame: None,
            pending_breakpoints: Vec::new(),
            launch: json!({
                "request": "launch",
                "type": "python",
                "program": program.display().to_string(),
                "cwd": cwd.display().to_string(),
                "args": args,
                // The program's own output comes back as DAP `output` events
                // and goes to the run console, rather than to a terminal the
                // editor would then have to own.
                "console": "internalConsole",
                // Stepping through the standard library is almost never what
                // is wanted and makes Step Into unusable.
                "justMyCode": true,
                "redirectOutput": true,
            }),
            launched: false,
        };

        let seq = session.request(
            "initialize",
            json!({
                "clientID": "the-editor",
                "clientName": "The Editor",
                "adapterID": "debugpy",
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsVariableType": true,
            }),
        )?;
        session.awaiting.insert(seq, Awaiting::Initialize);
        Ok(session)
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.state == State::Paused
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.state != State::Finished
    }

    /// Replace the breakpoints for one file.
    ///
    /// The protocol has no "add one" — a `setBreakpoints` carries the complete
    /// set for a source and replaces whatever was there. Before the handshake
    /// finishes these are queued, because the user places them before pressing
    /// start far more often than after.
    pub fn set_breakpoints(&mut self, path: &Path, lines: &[usize]) {
        if self.state == State::Starting {
            self.pending_breakpoints
                .retain(|(existing, _)| existing != path);
            self.pending_breakpoints
                .push((path.to_path_buf(), lines.to_vec()));
            return;
        }
        self.send_breakpoints(path, lines);
    }

    fn send_breakpoints(&mut self, path: &Path, lines: &[usize]) {
        let breakpoints: Vec<Value> = lines.iter().map(|line| json!({ "line": line })).collect();
        let arguments = json!({
            "source": { "path": path.display().to_string() },
            "breakpoints": breakpoints,
        });
        if let Ok(seq) = self.request("setBreakpoints", arguments) {
            self.awaiting
                .insert(seq, Awaiting::SetBreakpoints(path.to_path_buf()));
        }
    }

    /// Resume, or step. Ignored unless paused.
    pub fn resume(&mut self, how: Step) {
        if self.state != State::Paused {
            return;
        }
        let Some(thread) = self.stopped_thread else {
            return;
        };
        let arguments = json!({ "threadId": thread });
        if self.request(how.command(), arguments).is_ok() {
            self.state = State::Running;
            self.selected_frame = None;
        }
    }

    /// Ask for the variables of a frame, which the UI does when one is picked.
    pub fn select_frame(&mut self, frame: i64) {
        if self.state != State::Paused {
            return;
        }
        self.selected_frame = Some(frame);
        if let Ok(seq) = self.request("scopes", json!({ "frameId": frame })) {
            self.awaiting.insert(seq, Awaiting::Scopes);
        }
    }

    /// Stop the program and the adapter.
    pub fn stop(&mut self) {
        let _ = self.request("disconnect", json!({ "terminateDebuggee": true }));
        self.state = State::Finished;
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }

    /// Drain everything the adapter has said. Never blocks.
    pub fn poll(&mut self) -> Vec<DebugEvent> {
        let mut out = Vec::new();
        while let Ok(wire) = self.events.try_recv() {
            match wire {
                Wire::Message(message) => self.handle(&message, &mut out),
                Wire::Closed => {
                    if self.state != State::Finished {
                        self.state = State::Finished;
                        out.push(DebugEvent::StateChanged(State::Finished));
                    }
                }
            }
        }
        out
    }

    fn handle(&mut self, message: &Value, out: &mut Vec<DebugEvent>) {
        match message.get("type").and_then(Value::as_str) {
            Some("event") => self.handle_event(message, out),
            Some("response") => self.handle_response(message, out),
            _ => {}
        }
    }

    fn handle_event(&mut self, message: &Value, out: &mut Vec<DebugEvent>) {
        let body = message.get("body").cloned().unwrap_or(Value::Null);
        match message.get("event").and_then(Value::as_str) {
            Some("initialized") => {
                // Breakpoints can only be set now, between `initialized` and
                // `configurationDone`. Anything the user placed beforehand has
                // been waiting for this moment.
                for (path, lines) in std::mem::take(&mut self.pending_breakpoints) {
                    self.send_breakpoints(&path, &lines);
                }
                let _ = self.request("configurationDone", json!({}));
                self.state = State::Running;
                out.push(DebugEvent::StateChanged(State::Running));
            }
            Some("stopped") => {
                self.stopped_thread = body.get("threadId").and_then(Value::as_i64);
                self.state = State::Paused;
                let reason = body
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("paused")
                    .to_owned();
                out.push(DebugEvent::StateChanged(State::Paused));
                out.push(DebugEvent::Paused { reason });
                if let Some(thread) = self.stopped_thread
                    && let Ok(seq) = self.request(
                        "stackTrace",
                        json!({ "threadId": thread, "startFrame": 0, "levels": 40 }),
                    )
                {
                    self.awaiting.insert(seq, Awaiting::StackTrace);
                }
            }
            Some("continued") => {
                self.state = State::Running;
                out.push(DebugEvent::StateChanged(State::Running));
            }
            Some("terminated" | "exited") => {
                self.state = State::Finished;
                out.push(DebugEvent::StateChanged(State::Finished));
            }
            Some("output") => {
                // Only what the program actually printed. debugpy also reports
                // its own telemetry through this event -- the adapter's name
                // and version, with no trailing newline -- which ran together
                // with the first line of real output as `ptvsddebugpystart`.
                let category = body
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("console");
                let wanted = matches!(category, "stdout" | "stderr" | "console" | "important");
                if wanted && let Some(text) = body.get("output").and_then(Value::as_str) {
                    out.push(DebugEvent::Output(text.to_owned()));
                }
            }
            _ => {}
        }
    }

    fn handle_response(&mut self, message: &Value, out: &mut Vec<DebugEvent>) {
        let seq = message
            .get("request_seq")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let awaiting = self.awaiting.remove(&seq).unwrap_or(Awaiting::Ignored);

        if message.get("success").and_then(Value::as_bool) == Some(false) {
            // A failed `setBreakpoints` is worth saying out loud; a failed step
            // usually means the program ended underneath it and is not.
            if matches!(awaiting, Awaiting::Initialize | Awaiting::Launch) {
                let text = message
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the debugger refused to start");
                out.push(DebugEvent::Failed(text.to_owned()));
                self.state = State::Finished;
                out.push(DebugEvent::StateChanged(State::Finished));
            }
            return;
        }

        let body = message.get("body").cloned().unwrap_or(Value::Null);
        match awaiting {
            Awaiting::Initialize => {
                // `launch` goes out now; the adapter answers with `initialized`
                // when it is ready for breakpoints.
                if !self.launched {
                    self.launched = true;
                    let arguments = self.launch.clone();
                    if let Ok(seq) = self.request("launch", arguments) {
                        self.awaiting.insert(seq, Awaiting::Launch);
                    }
                }
            }
            Awaiting::SetBreakpoints(path) => {
                let lines = body
                    .get("breakpoints")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|b| {
                                Some(Breakpoint {
                                    line: usize::try_from(b.get("line")?.as_i64()?).ok()?,
                                    verified: b
                                        .get("verified")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(DebugEvent::BreakpointsVerified { path, lines });
            }
            Awaiting::StackTrace => {
                let frames = parse_frames(&body);
                // The innermost frame is the one being looked at, so its
                // variables are fetched without waiting to be asked.
                if let Some(first) = frames.first() {
                    self.select_frame(first.id);
                }
                out.push(DebugEvent::Stack(frames));
            }
            Awaiting::Scopes => {
                // Only the first scope, which for Python is the frame's locals.
                // Globals and builtins are hundreds of entries of no interest.
                if let Some(reference) = body
                    .get("scopes")
                    .and_then(Value::as_array)
                    .and_then(|s| s.first())
                    .and_then(|s| s.get("variablesReference"))
                    .and_then(Value::as_i64)
                    && let Ok(seq) =
                        self.request("variables", json!({ "variablesReference": reference }))
                {
                    self.awaiting.insert(seq, Awaiting::Variables);
                }
            }
            Awaiting::Variables => out.push(DebugEvent::Variables(parse_variables(&body))),
            Awaiting::Launch | Awaiting::Ignored => {}
        }
    }

    fn request(&mut self, command: &str, arguments: Value) -> Result<i64> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let message = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let outgoing = self
            .outgoing
            .as_ref()
            .context("the debug adapter is not running")?;
        outgoing
            .send(message.to_string())
            .context("the debug adapter stopped listening")?;
        Ok(seq)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A dropped session must not leave a paused Python process holding the
        // file open forever.
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

/// The four ways to leave a paused state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Over,
    Into,
    Out,
}

impl Step {
    fn command(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Over => "next",
            Self::Into => "stepIn",
            Self::Out => "stepOut",
        }
    }
}

fn parse_frames(body: &Value) -> Vec<Frame> {
    body.get("stackFrames")
        .and_then(Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .filter_map(|f| {
                    Some(Frame {
                        id: f.get("id")?.as_i64()?,
                        name: f.get("name")?.as_str()?.to_owned(),
                        path: f
                            .get("source")
                            .and_then(|s| s.get("path"))
                            .and_then(Value::as_str)
                            .map(PathBuf::from),
                        line: usize::try_from(f.get("line")?.as_i64()?).ok()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_variables(body: &Value) -> Vec<Variable> {
    body.get("variables")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| {
                    Some(Variable {
                        name: v.get("name")?.as_str()?.to_owned(),
                        value: v
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        kind: v.get("type").and_then(Value::as_str).map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn spawn_writer(mut stdin: std::process::ChildStdin, messages: Receiver<String>) {
    let _ = std::thread::Builder::new()
        .name("dap-writer".to_owned())
        .spawn(move || {
            while let Ok(payload) = messages.recv() {
                if editor_lsp::transport::write_message(&mut stdin, &payload).is_err() {
                    break;
                }
                let _ = stdin.flush();
            }
        });
}

fn spawn_reader(stdout: std::process::ChildStdout, events: Sender<Wire>) {
    let _ = std::thread::Builder::new()
        .name("dap-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(text)) = editor_lsp::transport::read_message(&mut reader) {
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    tracing::warn!("unparseable debug adapter message");
                    continue;
                };
                if events.send(Wire::Message(value)).is_err() {
                    break;
                }
            }
            let _ = events.send(Wire::Closed);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stack_trace_is_read_innermost_first() {
        let body = json!({
            "stackFrames": [
                { "id": 3, "name": "inner", "line": 12,
                  "source": { "path": "C:/p/main.py" } },
                { "id": 2, "name": "outer", "line": 40,
                  "source": { "path": "C:/p/main.py" } },
            ]
        });
        let frames = parse_frames(&body);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].name, "inner");
        assert_eq!(frames[0].line, 12);
        assert_eq!(frames[0].path, Some(PathBuf::from("C:/p/main.py")));
    }

    #[test]
    fn a_frame_without_a_source_is_still_listed() {
        // Frames inside the interpreter have no file. Dropping them would make
        // the stack lie about its own depth.
        let body = json!({ "stackFrames": [{ "id": 1, "name": "<module>", "line": 1 }] });
        let frames = parse_frames(&body);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].path, None);
    }

    #[test]
    fn a_malformed_frame_is_skipped_rather_than_losing_the_rest() {
        let body = json!({
            "stackFrames": [
                { "nonsense": true },
                { "id": 1, "name": "ok", "line": 2 },
            ]
        });
        assert_eq!(parse_frames(&body).len(), 1);
    }

    #[test]
    fn variables_keep_their_type_when_the_adapter_sends_one() {
        let body = json!({
            "variables": [
                { "name": "rows", "value": "['a', 'b']", "type": "list" },
                { "name": "n", "value": "2" },
            ]
        });
        let vars = parse_variables(&body);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].kind.as_deref(), Some("list"));
        assert_eq!(vars[1].kind, None, "a missing type is not an error");
        assert_eq!(vars[1].value, "2");
    }

    #[test]
    fn only_the_programs_own_output_reaches_the_console() {
        // debugpy reports its own name and version as `telemetry` output, with
        // no trailing newline, so it ran straight into the first line the
        // program printed: `ptvsddebugpystart`.
        let wanted =
            |category: &str| matches!(category, "stdout" | "stderr" | "console" | "important");
        assert!(wanted("stdout"));
        assert!(wanted("stderr"));
        assert!(!wanted("telemetry"));
    }

    #[test]
    fn an_empty_body_yields_nothing_rather_than_failing() {
        assert!(parse_frames(&Value::Null).is_empty());
        assert!(parse_variables(&Value::Null).is_empty());
    }

    #[test]
    fn each_step_uses_the_command_the_protocol_defines() {
        // Getting `next` and `stepIn` the wrong way round is invisible until
        // someone steps over a function call and lands inside it.
        assert_eq!(Step::Continue.command(), "continue");
        assert_eq!(Step::Over.command(), "next");
        assert_eq!(Step::Into.command(), "stepIn");
        assert_eq!(Step::Out.command(), "stepOut");
    }

    #[test]
    fn an_unverified_breakpoint_is_reported_as_such() {
        // A breakpoint on a blank line binds to nothing, and must not look like
        // one that will be hit.
        let body = json!({
            "breakpoints": [
                { "line": 10, "verified": true },
                { "line": 11, "verified": false },
                { "line": 12 },
            ]
        });
        let parsed: Vec<Breakpoint> = body["breakpoints"]
            .as_array()
            .expect("an array")
            .iter()
            .filter_map(|b| {
                Some(Breakpoint {
                    line: usize::try_from(b.get("line")?.as_i64()?).ok()?,
                    verified: b.get("verified").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        assert_eq!(
            parsed[0],
            Breakpoint {
                line: 10,
                verified: true
            }
        );
        assert!(!parsed[1].verified);
        assert!(!parsed[2].verified, "absent means not verified");
    }
}
