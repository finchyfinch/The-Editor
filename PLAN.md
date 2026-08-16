# The Editor — Development Plan (target release: **1.0.0**)

**Name:** **The Editor** — binary `the-editor`, library crates `editor-*`.
**Author / copyright holder:** **Gareth Finch**. Licence: MIT (change in `LICENSE` and the workspace
manifest if you'd rather it be proprietary — it only affects the About box and `cargo-about`).
**Config identity:** `ProjectDirs::from("", "Gareth Finch", "The Editor")` →
`%APPDATA%\Gareth Finch\The Editor` / `~/.config/the-editor` / `~/Library/Application Support/…`.
**Targets:** Windows 10/11 (x86_64, MSVC), Linux (x86_64 + aarch64, glibc 2.31+), macOS 12+
(aarch64 + x86_64).
**Toolchain baseline:** Rust 1.97.1 (current on this machine), edition 2024, MSRV pinned via
`rust-version` in the workspace manifest.
**Source control:** local `git` only (no GitHub). See §12 for the non-GitHub workflow.

---

## 1. Decisions made up front

These are the calls that shape everything else. Each one records the alternative and the point at
which switching is still cheap, so they can be revisited deliberately rather than by accident.

| # | Decision | Rationale | Alternative & switch cost |
|---|---|---|---|
| D1 | **GUI toolkit: `egui` + `eframe`** (**glow backend**, wgpu available) — *confirmed; renderer reversed by measurement, see below* | Pure Rust, zero system GUI dependencies, one binary per platform, pixel-identical on all three OSes, trivial custom painting — which matters because the code editor **will** be a custom widget no matter which toolkit is chosen. Fastest path to a working IDE for a solo developer. | `floem` (what Lapce uses — better built-in text/vector stack, far thinner docs) or `slint` (excellent tooling, but GPLv3-or-commercial licensing and the editor is still custom). Switch cost is low until M2 lands, high afterwards — the editor widget is the only piece with deep toolkit coupling, so keep all toolkit types out of `ide-core`/`ide-syntax`/`ide-lsp`. |
| D2 | **Text storage: `ropey`** rope, one buffer per document | O(log n) edits and line indexing, UTF-8 native, handles multi-MB files without the O(n) copies a `String` would cost. Battle-tested (Helix uses it). | `crop`, or a piece table. Contained inside `ide-core::Buffer`. |
| D3 | **Highlighting: `tree-sitter`** with per-language grammars, incremental reparse | Real parse trees → correct highlighting, plus free bracket matching, code folding ranges, "select enclosing node", and indentation heuristics. One mechanism serves five features. | Regex/`syntect` (`.sublime-syntax`). Keep a `Highlighter` trait so a regex fallback can serve any language without a grammar (INI can go either way). |
| D4 | **Intelligence: LSP client**, not hand-written analysis | Completion, diagnostics, hover, go-to-definition, rename and formatting all come from `rust-analyzer` / `ruff` / `pyright` for free and stay correct as those languages evolve. Writing a Python type inferencer is a multi-year project on its own. | None sensible. Must ship a graceful degradation path: buffer-word + keyword completion when no server is installed, so the IDE is never *broken* by a missing server. |
| D5 | ~~**Concurrency: `tokio` runtime**~~ → **threads and channels**, UI thread never blocks | *Revised during M7.* Every slow thing runs off the UI thread, which only drains channels — that part held. But the PTY, the filesystem watcher and the LSP client each turned out to need one or two threads and a channel, not a scheduler. Adding tokio for the LSP client alone would have meant two concurrency models in one codebase, which is worth more than the efficiency it would have bought. Revisit if project-wide search or a plugin host ever needs real task management. | Tokio, as originally planned. |
| D6 | **Run/console: PTY-backed** (`portable-pty` + `alacritty_terminal` for VT parsing) | Programs behave the same as in a real terminal: `input()` works, colours work, Ctrl-C works, progress bars work. Piped stdout would break interactive Python immediately. | Plain `Command` + pipes. Cheap now, painful later — do the PTY from the start. |
| D7 | **Config: TOML**, layered *defaults → user → project* | Human-editable, diffable, comment-friendly; `serde` gives the settings UI a typed source of truth. | JSON (no comments) / YAML (indentation footguns). |
| D8 | **No telemetry, no auto-update, no network calls** in 1.0 | Simpler, privacy-clean, no server infrastructure to run, and no signing/hosting requirement beyond the release artefacts themselves. | Add an opt-in update *check* post-1.0 if ever wanted. |
| D9 | **Zero-cost dependencies only** — every language server, linter and formatter shipped or recommended must be free and permissively licensed | No paid tooling anywhere in the stack. Verified: `ruff` (MIT), `basedpyright` (MIT), `pyright` (MIT), `python-lsp-server` (MIT), `rust-analyzer` (MIT/Apache-2.0), `taplo` (MIT), `vscode-langservers-extracted` (MIT), all tree-sitter grammars (MIT/Apache-2.0), JetBrains Mono + Inter (SIL OFL). `cargo deny check licenses` enforces this on the Rust side with a strict allow-list. | None needed — the free tools are also the best ones here. |
| D10 | **No code signing / notarisation** — unsigned binaries on all platforms | Explicit decision by the author. Saves ~$300–500/yr and the whole certificate-renewal treadmill. | Consequences to own and document in the README: Windows SmartScreen shows "Windows protected your PC" until enough downloads accrue reputation (user clicks *More info → Run anyway*); macOS Gatekeeper blocks the `.app` until the user right-clicks → Open, or runs `xattr -dr com.apple.quarantine`. Linux is unaffected. Publish SHA-256 checksums alongside every artefact so integrity is still verifiable. |

---

## 2. Architecture

### 2.1 Layer diagram

```
┌──────────────────────────────────────────────────────────────────────────┐
│  the-editor (binary) ── eframe shell, window/menus, layout, command router│
├──────────────────────────────────────────────────────────────────────────┤
│ editor-widgets  editor view · file tree · tab bar · palette · find bar ·  │
│                 output console · settings form · diff/problem lists       │
├───────────┬───────────┬───────────┬───────────┬──────────────────────────┤
│editor-core│ed-syntax  │ editor-lsp│editor-proc│ed-search   │editor-config│
│ rope,     │tree-sitter│ client,   │ PTY, run  │ ripgrep-   │ settings,   │
│ edits,    │ parsing,  │ server    │ configs,  │ backed     │ keymap,     │
│ undo,     │ themes,   │ lifecycle,│ ANSI,     │ project    │ themes,     │
│ selection,│ folds,    │ diagnostic│ output    │ search &   │ project     │
│ indent    │ brackets  │ routing   │ links     │ replace    │ detection   │
└───────────┴───────────┴───────────┴───────────┴────────────┴─────────────┘
                    ↑ no crate below this line knows the GUI toolkit exists ↑
```

**Hard rule:** `editor-core`, `editor-syntax`, `editor-lsp`, `editor-proc`, `editor-search` and
`editor-config` must not depend on `egui`. That is what makes D1 reversible and what makes those
crates unit-testable without a window. A `#[test]` in `editor-app` asserts this by parsing the
dependency graph, so it can't rot silently.

### 2.2 Cargo workspace layout

```
IDE/
├── Cargo.toml              # [workspace], shared dep versions in [workspace.dependencies]
├── rust-toolchain.toml     # pinned toolchain
├── deny.toml               # cargo-deny: licenses + advisories
├── .cargo/config.toml      # cargo aliases: xfmt, xlint, xtest, xcheck, spike
├── PLAN.md  CHANGELOG.md  LICENSE  README.md
├── crates/
│   ├── app/                # bin: the-editor
│   ├── core/               # Buffer, Document, Edit, History, Selection, Indent
│   ├── syntax/             # LanguageRegistry, Highlighter, tree-sitter queries
│   ├── lsp/                # LspClient, ServerRegistry, transport
│   ├── proc/               # PtySession, RunConfig, AnsiSink, OutputLinkParser
│   ├── search/             # in-file + project-wide search/replace
│   ├── config/             # Settings, Keymap, Theme, ProjectKind detection
│   ├── widgets/            # egui widgets (the only GUI-aware library crate)
│   └── spike/              # throwaway: virtualised rope rendering validation
├── assets/
│   ├── fonts/              # JetBrains Mono NL (OFL) + Inter (OFL) for UI
│   ├── icons/              # toolbar SVGs, app icon at 16..1024px
│   ├── themes/             # *.toml colour themes
│   ├── queries/            # tree-sitter highlights.scm / indents.scm / folds.scm per language
│   └── templates/          # New-File boilerplate (§7)
├── docs/                   # user manual (rendered into Help)
└── packaging/              # wix/, appimage/, deb/, macos-bundle/
```

### 2.3 Core data model (`ide-core`)

```rust
pub struct Document {
    pub id: DocId,
    pub path: Option<PathBuf>,   // None => untitled
    pub text: Rope,
    pub language: LanguageId,
    pub encoding: Encoding,      // UTF-8 / UTF-8-BOM / UTF-16LE/BE / Latin-1
    pub line_ending: LineEnding, // LF / CRLF — detected on load, preserved on save
    pub version: i32,            // monotonic; the LSP textDocument version
    pub history: History,        // undo/redo, transaction-grouped
    pub dirty: bool,
    pub disk_mtime: Option<SystemTime>, // external-change detection
    pub indent: IndentStyle,     // spaces(4) | tabs(width)
}

pub struct View {           // a Document may have several Views (split panes, post-1.0)
    pub doc: DocId,
    pub selections: Vec<Selection>, // multi-cursor; selections[0] is primary
    pub scroll: ScrollState,
    pub folds: FoldSet,
}
```

**Edits.** All mutation flows through one function:

```rust
pub fn apply(doc: &mut Document, tx: Transaction) -> Vec<Change>;
```

A `Transaction` is a list of `(range, replacement)` applied right-to-left so earlier offsets stay
valid. `apply` returns the changes, which fan out to: undo history, tree-sitter `edit()`, the LSP
`didChange` queue, and any open search results needing offset adjustment. **Nothing else may touch
`doc.text` directly** — this single choke point is what keeps highlighting, LSP state and undo from
ever drifting out of sync. It is the most important invariant in the codebase.

**Undo.** Inverse transactions on a stack, coalesced by a 300 ms idle timer + boundary rules (a
newline, a cursor jump, a save, or a non-typing command forces a boundary). Redo stack cleared on
new edit. Selections are stored with each entry so undo restores the cursor.

### 2.4 The editor widget — the one genuinely hard piece

Everything else in this project is assembly. This is the part that needs care.

- **Virtualised rendering.** Compute the visible line range from scroll offset and row height; lay
  out and paint *only* those lines plus ~20 rows of margin. A 200k-line file must cost the same per
  frame as a 50-line file.
- **Layout cache.** `LruCache<LineIdx, Galley>` keyed by (line content hash, wrap width, font size);
  invalidated per-line on edit. Soft wrap is off by default (a toggle in view settings).
- **Gutter** painted as a separate column: line numbers (right-aligned, dimmed except the cursor
  line), diagnostic severity dots, fold arrows, and a modified-lines bar.
- **Cursor & selection**: caret blink on a 530 ms timer, block/line/word selection via
  click/double/triple, shift-click extend, Alt+click adds a cursor, Alt+drag column select.
- **Input handling** goes through the keymap resolver (§6), never hardcoded `match` on keys.
- **IME** (`egui`'s `Event::Ime`) must be wired for CJK input — easy to forget, painful to retrofit.
- **Minimap**: post-1.0. A modified-lines/diagnostics scrollbar overlay in 1.0 gives 80 % of the
  value for 5 % of the work.

**Performance budget** (enforced by a bench in `crates/widgets/benches/`): < 4 ms/frame to paint a
full screen of highlighted code; < 1 ms to apply a single-character edit including reparse;
< 150 ms to open a 5 MB file.

### 2.5 Threading

```
UI thread (60 fps)                    Tokio runtime (background thread)
──────────────────                    ────────────────────────────────
egui frame                            LSP servers (one task per server)
  drain rx: Vec<AppEvent>  ◄────────  file watcher (notify, 200 ms debounce)
  mutate app state                    project search (ripgrep walker)
  paint                               PTY readers (run console)
  tx: Vec<Request>         ────────►  file I/O (open/save large files)
                                      workspace indexing (fuzzy-open file list)
```

Channels: `crossbeam_channel` both ways. Every inbound message calls
`egui::Context::request_repaint()` so the UI wakes only when something happened — idle CPU at ~0 %.

---

## 3. Feature specification

### 3.1 Window layout

```
┌────────────────────────────────────────────────────────────────────────────────┐
│ File  Edit  View  Run  Tools  Help                              ← menu bar     │
├────────────────────────────────────────────────────────────────────────────────┤
│ [New][Open][Save][Save All] │ [Undo][Redo] │ [Find] │ [▶ Run][■ Stop][Fmt] │ ⚙ │
├──────────────┬─────────────────────────────────────────────────────────────────┤
│ EXPLORER     │ main.py ×│ lib.rs ×│ config.ini ×│ index.html ×                  │
│              ├─────────────────────────────────────────────────────────────────┤
│ ▾ my_project │  1  #!/usr/bin/env python3                                      │
│   ▾ src      │  2  """Module docstring."""                                     │
│     main.py  │  3                                                              │
│     util.py  │  4  def main() -> int:                                          │
│   ▸ tests    │  5      ...                                                     │
│   README.md  │                                                                 │
│              ├─────────────────────────────────────────────────────────────────┤
│              │ OUTPUT │ PROBLEMS (3) │ SEARCH │ TERMINAL       ← bottom dock    │
│              │ $ python src/main.py                                            │
│              │ Hello, world!                                                    │
├──────────────┴─────────────────────────────────────────────────────────────────┤
│ Python 3.14.6 (.venv) │ UTF-8 │ LF │ Spaces: 4 │ Ln 12, Col 5 │ ⚠2 ✖1          │
└────────────────────────────────────────────────────────────────────────────────┘
```

All three docks (left panel, bottom panel, right panel [post-1.0]) are collapsible and their sizes
persist in the session file.

### 3.2 File manager pane (left)

- Root = the opened folder ("project"). Multi-root is post-1.0.
- Lazy expansion — a directory's children are read on first expand, not up front.
- **Double-click opens the file in a new tab**; single click previews it in a reusable *preview
  tab* (italic title, replaced by the next preview) — the behaviour people expect from VS Code, and
  it stops 40 tabs from accumulating during exploration.
- Context menu: New File, New Folder, Rename (F2), Duplicate, Delete (→ OS trash via the `trash`
  crate, never an unrecoverable `remove_file`), Cut/Copy/Paste, Copy Path, Copy Relative Path,
  Reveal in Explorer/Finder/File Manager, Open in Terminal, Set as Run Target.
- Live refresh via `notify` (debounced 200 ms); expansion state preserved across refreshes.
- Filter box; hidden-file toggle; ignore rules from `.gitignore` + a settings-level list
  (`__pycache__`, `target`, `node_modules`, `.venv`, …) using the `ignore` crate.
- Icons per file type; drag-and-drop to move files (with a confirm dialog).

### 3.3 Tabs & editor area

- Per tab: language icon, filename, dirty dot (● replacing the ×, × on hover), **× to close**.
- Middle-click closes. Ctrl+W closes. Ctrl+Shift+T reopens the last closed tab.
- Overflow: horizontal scroll + a "⌄" dropdown listing all open tabs.
- Drag to reorder. Right-click: Close, Close Others, Close to the Right, Close Saved, Copy Path,
  Reveal in Explorer, Split Right (post-1.0), Pin.
- Closing a dirty tab prompts **Save / Don't Save / Cancel**; quitting prompts once for all dirty
  tabs with a checklist.
- Ctrl+Tab cycles in most-recently-used order (with a visible overlay), not left-to-right.

### 3.4 Editing

Core: line numbers (absolute; relative optional), current-line highlight, copy/cut/paste
(cut/copy with no selection acts on the whole line), duplicate line (Ctrl+D), move line up/down
(Alt+↑/↓), delete line (Ctrl+Shift+K), select all, word-wise motion honouring `camelCase` and
`snake_case`, home-key toggling between first-non-whitespace and column 0, column/box selection,
multi-cursor (Alt+click, Ctrl+Alt+↑/↓, Ctrl+D for next occurrence), bracket match highlight, jump
to matching bracket (Ctrl+M), auto-close brackets/quotes (with type-over of the closer), surround
selection with brackets/quotes, toggle line comment (Ctrl+/) and block comment (Ctrl+Shift+/),
indent/dedent selection (Tab / Shift+Tab), code folding (from tree-sitter ranges), go to line
(Ctrl+G), whitespace rendering toggle, indent guides, a configurable column ruler, zoom
(Ctrl+`+`/`-`/`0`), and a **read-only mode** auto-applied to files inside `site-packages`,
`.cargo/registry`, etc.

**Language-aware indentation** (`ide-core::indent`, driven by
`assets/queries/<lang>/indents.scm` plus per-language rules):

*Python* — the case that needs to be right:
- 4 spaces, spaces only, hard-tab insertion disabled; Tab inserts to the next 4-column stop.
- Newline after a line ending in `:` → indent one level.
- Newline inside an unclosed `(`/`[`/`{` → align to the opening delimiter's content column
  (PEP 8 continuation lines); if the opener is the last char on its line, use a hanging indent of
  one level instead.
- Typing `else`/`elif`/`except`/`finally`/`case` as the first token on a line → dedent one level to
  match its opener.
- Newline after `return`/`pass`/`raise`/`break`/`continue` → dedent one level.
- Backspace in leading whitespace deletes back to the previous 4-column stop, not one space.
- Paste re-indents the block to the target context (Ctrl+Shift+V pastes raw).
- Docstring/triple-quote awareness: no auto-indent games inside string literals.
- On save (configurable): trim trailing whitespace, ensure a single final newline, convert stray
  tabs to spaces.
- Mixed tabs/spaces detected on open → status-bar warning with a one-click "Normalise Indentation".

*Rust* — 4 spaces, indent after `{`, dedent on `}` typed as first token, chained-method alignment
left to `rustfmt` (format-on-save is the real answer). *JSON/JS/CSS/HTML* — 2 spaces by default,
brace/tag-aware. *INI* — no auto-indent, section-aware highlighting. `.editorconfig` support if
present overrides the defaults (small crate, big compatibility win).

### 3.5 Syntax highlighting

| Extension(s) | Language | Grammar | Notes |
|---|---|---|---|
| `.py` `.pyw` `.pyi` | Python | `tree-sitter-python` | f-string interpolation highlighted as code |
| `.rs` | Rust | `tree-sitter-rust` | macro bodies best-effort |
| `.json` `.jsonc` | JSON | `tree-sitter-json` | duplicate-key + trailing-comma diagnostics built in |
| `.js` `.mjs` `.cjs` | JavaScript | `tree-sitter-javascript` | JSX highlighted |
| `.html` `.htm` | HTML | `tree-sitter-html` | **injections**: `<script>`→JS, `<style>`→CSS |
| `.css` | CSS | `tree-sitter-css` | |
| `.ini` `.cfg` `.conf` `.toml`* | INI | `tree-sitter-ini` (or regex fallback) | *`.toml` gets its own grammar — `Cargo.toml` matters here |
| `.txt` `.md` `.log` | Plain text | none | Markdown grammar is a cheap freebie; add it |

Mechanics: parse on open, `Tree::edit()` + incremental reparse on every transaction (sub-millisecond
for typical edits), highlight the visible range only via `tree-sitter-highlight`'s capture iterator.
Grammar failure or file > 5 MB → fall back to plain text and say so in the status bar. Language is
detected by extension first, then shebang (`#!/usr/bin/env python3`), then a manual override in the
status bar which is remembered per file.

Themes are TOML: a capture-name → colour/style map (`keyword`, `function`, `string`, `comment`,
`type`, `constant`, `operator`, `punctuation`, `variable.parameter`, …). Ship a dark and a light
theme built to WCAG AA contrast; theme switching is live, no restart.

### 3.6 Linting, diagnostics, completion (LSP)

**Server matrix** (all optional, auto-detected on PATH / in the active venv, path-overridable in
Settings):

| Language | Server | Provides |
|---|---|---|
| Rust | `rust-analyzer` | completion, diagnostics, hover, goto, references, rename, inlay hints, code actions, `rustfmt` |
| Python | `ruff server` | lint + fix + format (fast, one binary, no config needed) |
| Python | `basedpyright` / `pyright` / `pylsp` | types, completion, hover, goto, rename |
| JSON/HTML/CSS/JS | `vscode-langservers-extracted`, `typescript-language-server` | schema validation, completion |
| TOML | `taplo` | validation + format |

Two servers may serve one language; diagnostics are merged and tagged with their source.

**Client** (`ide-lsp`): `lsp-types` for the protocol, hand-rolled JSON-RPC framing over stdio,
one tokio task per server. Must handle: initialize/shutdown handshake, `didOpen`/`didChange`
(incremental sync, debounced 150 ms) / `didSave` / `didClose`, `publishDiagnostics`, completion +
`completionItem/resolve`, signature help, hover, definition/references, rename with a preview and
atomic multi-file apply through the normal undo system, code actions/quick fixes, and formatting.
Crash recovery: exponential-backoff restart, max 3 attempts, then a status-bar badge with a
"Restart language server" command and an LSP log viewer under Help → Diagnostics.

**On Windows, kill the whole process tree** — assign child processes to a Job Object, otherwise
`rust-analyzer` and friends survive as orphans on quit. (`Command::kill()` alone is not enough.)

**Degradation ladder** — the IDE must stay useful with nothing installed:
1. Full LSP if a server is present.
2. Otherwise: buffer-word completion + language keyword list + tree-sitter-derived symbol list
   (function/class names in the file), fuzzy-matched.
3. Always: bracket/quote checks and the JSON parse errors that come from tree-sitter's own error
   nodes.

**Presentation:** squiggles under diagnostic ranges, gutter dots, hover tooltip, a **Problems**
panel (grouped by file, click to jump, filter by severity), status-bar error/warning counts, and
Alt+Enter for the quick-fix menu. Completion popup: fuzzy-ranked, icon per kind, detail + docs
pane, Tab/Enter to accept, snippet placeholder navigation with Tab.

### 3.7 Find / replace

- **In-file bar** (Ctrl+F / Ctrl+H): live incremental match, all-matches highlighted plus scrollbar
  ticks, `n/m` counter, Enter/Shift+Enter to step, toggles for case, whole word, regex
  (`regex` crate), and *selection only*. Replace / Replace All, with `$1` capture references.
- **Project-wide** (Ctrl+Shift+F / Ctrl+Shift+H): the `grep-searcher` + `ignore` crates (i.e. the
  ripgrep engine as a library) on the tokio runtime — results stream in, cancellable. Include/exclude
  globs, results grouped by file with context lines, click to jump, and **Replace All across the
  project** as a single undoable operation with a preview list and a per-match exclude checkbox.

### 3.8 Running code

`Run ▶` (F5) resolves a **run configuration** in this order:
1. An explicit config the user selected from the Run dropdown (stored in
   `.ide/run.toml` in the project).
2. Project-kind inference:
   - `Cargo.toml` present → `cargo run` (workspace root, `--bin <name>` if the active file is a bin
     target); `Ctrl+F5` → `cargo test`; profile toggle debug/release in the Run menu.
   - **Python file → `<interpreter> <file>`.** That is the whole command. The interpreter is
     resolved as (a) the path stored in Settings → Python → Interpreter, (b) the project's
     `./.venv/`, `./venv/`, `./env/` if one exists, (c) whatever `python` is on `PATH`. cwd is the
     project root. Optional user args are appended. No wrapper, no `-m`, no generated launcher
     script — the exact command line is echoed into the Output panel before it runs so there is
     never any doubt about what was executed.
   - `.js` → `node`; `.html` → open in the default browser.
3. Otherwise the Run button is disabled with an explanatory tooltip.

Execution: PTY session (D6) → the **Output** dock tab. Features: full ANSI colour, a working stdin
box so `input()` and `cargo run` prompts work, Stop (SIGTERM then SIGKILL / `TerminateJobObject`),
Restart, Clear, wrap toggle, copy, exit-code banner with wall time, and a **scrollback limit**
(50k lines, configurable) so a runaway loop cannot exhaust RAM.

**Clickable error links** — regex-matched over output lines, jumping to file:line:col:
- Rust: `--> src/main.rs:12:5`
- Python tracebacks: `File "src/main.py", line 12`
- Generic: `path:line:col`
Relative paths resolve against the run cwd. This is a small feature with a disproportionate
day-to-day payoff; do not defer it.

Environment: settings supply extra `PATH` entries, `PYTHONPATH` additions, and arbitrary `KEY=VALUE`
pairs, layered global → project. `.env` file loading is a settings toggle.

### 3.8a Python environment management

**Tools → Python → Create Virtual Environment…** — a small modal:

```
┌─ Create Virtual Environment ──────────────────┐
│ Base interpreter: [ Python 3.14.6  (C:\…)  ▾] │
│                   [ Browse… ]                 │
│ Location:         [ C:\proj\.venv          ]  │
│ Name:             [ .venv                  ]  │
│ [x] Upgrade pip after creation                │
│ [x] Install from requirements.txt (found)     │
│ [ ] Inherit global site-packages              │
│ [x] Set as this project's interpreter         │
│ [x] Add to .gitignore                         │
│                        [ Cancel ] [  Create ] │
└───────────────────────────────────────────────┘
```

Mechanics — deliberately thin, no magic:
- The base-interpreter dropdown is populated by a discovery scan: `PATH`, the Windows registry
  (`HKLM/HKCU\SOFTWARE\Python\PythonCore\*\InstallPath`), the `py` launcher (`py -0p`),
  `/usr/bin/python3*`, Homebrew, pyenv (`~/.pyenv/versions/*`), and conda envs — each shown with its
  version string, verified by actually running `<path> -c "import sys; print(sys.version)"`.
  Anything not found is reachable via Browse.
- Creation runs `<base> -m venv <location>` (plus `--system-site-packages` if ticked) in the run
  console so the user sees exactly what happened, then optionally `<venv-python> -m pip install
  --upgrade pip` and `<venv-python> -m pip install -r requirements.txt`.
- On success it writes the interpreter path into `.ide/settings.toml` under `[python] interpreter`,
  appends `.venv/` to `.gitignore` if asked, restarts the Python language server against the new
  environment, and updates the status bar.
- Failure (no `venv` module, permission denied, path exists and is non-empty) surfaces the real
  stderr rather than a generic message.

Related, same machinery, all under **Tools → Python**:
- **Select Interpreter…** — the same discovery list plus any `.venv` found in the project; also
  clickable from the status bar's interpreter indicator.
- **Install Packages…** — a `pip install` box targeting the active interpreter, output in the
  console. Deliberately not a package-manager UI; just a shortcut for the common case.
- **Show Installed Packages** — `pip list` into the Output panel.
- The New Project wizard (§7) offers "Create a virtual environment" as a checkbox, reusing all of
  the above.

The status bar's interpreter segment (`Python 3.14.6 (.venv)`) is the single always-visible answer
to "which Python is this going to run with?" — clicking it opens Select Interpreter. That indicator
prevents more confusion than any other element in the UI.

### 3.9 Settings

Layered: built-in defaults → user (`{config_dir}/ide/settings.toml`) → project
(`<root>/.ide/settings.toml`). The UI is a searchable form generated from the typed struct, with a
"Open settings.toml" escape hatch and a per-setting reset. Live-applied; no restart except for the
renderer backend.

```toml
[editor]
font_family = "JetBrains Mono NL"
font_size = 13.0
tab_width = 4
insert_spaces = true
show_whitespace = false
show_indent_guides = true
rulers = [88]
word_wrap = false
auto_close_brackets = true
trim_trailing_whitespace_on_save = true
insert_final_newline = true
format_on_save = true
autosave = "off"            # off | after_delay | on_focus_change
autosave_delay_ms = 1000

[ui]
theme = "dark"              # dark | light | <custom theme name>
ui_scale = 1.0
show_file_tree = true
restore_session = true

[python]
interpreter = ""            # "" = auto-detect (.venv → venv → env → PATH)
extra_paths = []            # appended to PYTHONPATH
lsp_server = "auto"         # auto | basedpyright | pyright | pylsp | none
linter = "ruff"             # ruff | flake8 | none
formatter = "ruff"          # ruff | black | autopep8 | none
args = []

[rust]
cargo_path = ""             # "" = PATH
toolchain = ""              # "" = rustup default; else +nightly etc.
rust_analyzer_path = ""
check_command = "clippy"    # check | clippy
format_on_save = true

[run]
env = {}
extra_path = []
clear_output_before_run = true
scrollback_lines = 50000

[files]
exclude = ["**/__pycache__", "**/target", "**/node_modules", "**/.venv", "**/.git"]
hot_exit = true             # restore unsaved buffers after a crash
```

**Settings must be forward-compatible:** unknown keys are preserved on rewrite (`toml_edit`, not
`toml`), so downgrading the IDE never silently deletes a user's newer settings.

### 3.10 Help / About

- **About** — app name, version `1.0.0`, build hash + build date (`vergen` at compile time), author
  `<AUTHOR NAME>` and copyright line, licence, Rust version used, a "Copy diagnostics" button
  (versions, paths, detected servers) for bug reports, and third-party licence attributions
  generated by `cargo-about` (a legal requirement for the fonts and grammars, not optional).
- **Help** — Keyboard Shortcuts (searchable, generated from the live keymap so it can't go stale),
  the user manual from `docs/` rendered in-app, "Open Log Folder", "Open Settings Folder",
  "Language Server Diagnostics", "Check Toolchains" (a panel showing detected python/cargo/servers
  with ✓/✗ and remediation hints — this will absorb most of the "why doesn't it work" questions).

### 3.11 Appearance and themes

**Dark is the default.** The Editor ships dark and light themes and can follow
the operating system. This is a first-class feature, not a preference buried in
a settings file, so it is reachable three ways: **View → Theme ▸**, a
click-target in the status bar, and the command palette (`Theme: Dark`,
`Theme: Light`, `Theme: Follow System`).

**Two layers, deliberately separable.** Conflating them is why so many editors
end up with a light UI frame around a dark code pane, or vice versa:

| Layer | Controls | Source |
|---|---|---|
| **UI theme** | window chrome, panels, menus, tabs, file tree, dialogs, buttons | `[ui] theme = "dark" \| "light" \| "system"` |
| **Syntax theme** | the code pane: token colours, selection, current line, gutter, diagnostics | `[ui] syntax_theme = "<name>"`, a TOML file (§3.5) |

By default the syntax theme follows the UI theme — pick dark, get the dark
syntax theme — but either can be pinned independently for people who want a
dark frame around a light editor pane or the reverse.

**Follow-system** reads the OS preference at startup and reacts to changes at
runtime (Windows `AppsUseLightTheme`, macOS `AppleInterfaceStyle`, Linux
`org.freedesktop.appearance color-scheme` via the XDG settings portal, falling
back to the GTK theme name). egui surfaces this through winit, so it costs
almost nothing; a manual choice always overrides it.

**Switching is live and instant** — no restart, no reload of open files, no
flash. The setting persists immediately on change.

**Constraints that apply to every theme, built-in or user-supplied:**
- Both themes meet WCAG AA (4.5:1) for body text and 3:1 for UI borders and
  icons. This is checked by a unit test over the theme files, not by eye — a
  contrast regression in a hand-edited colour must fail CI, not ship.
- Syntax colours must remain distinguishable under the common forms of colour
  blindness; don't let "keyword" and "function" differ only in red/green.
- Diagnostics never rely on colour alone: errors get a squiggle shape and a
  gutter glyph as well as a red.
- A theme that fails to load falls back to the built-in dark theme with a
  toast, rather than rendering an unreadable window.

**Implementation.** UI theme lives in `editor-config::theme`, applied through
egui's per-theme `Style`/`Visuals`; syntax theme lives in `editor-syntax::theme`
as a capture-name → style map. Both are plain TOML and user-extensible from
`{config_dir}/themes/`. Custom theme authoring gets a documented format in M8;
a live theme editor is post-1.0.

The basic UI light/dark/system toggle lands in **M1** rather than M8 — it is
nearly free once the settings layer exists, and building the rest of the UI
against a theme that can already change catches hardcoded colours immediately
instead of after nine milestones of accumulation.

---

## 4. Menus & toolbar

**File** — New… (Ctrl+N, opens the wizard §7) · New Window · Open File… (Ctrl+O) · Open Folder…
(Ctrl+K Ctrl+O) · Open Recent ▸ · Save (Ctrl+S) · Save As… · Save All (Ctrl+Alt+S) · Revert File ·
Close Tab (Ctrl+W) · Close Folder · Settings (Ctrl+,) · Exit

**Edit** — Undo/Redo · Cut/Copy/Paste · Paste Without Formatting · Find/Replace · Find in
Files/Replace in Files · Go to Line · Go to File (Ctrl+P) · Go to Symbol (Ctrl+Shift+O) ·
Toggle Comment · Indent/Outdent · Duplicate/Delete Line · Move Line Up/Down · Add Cursor Above/Below

**View** — Explorer · Problems · Output · Terminal · Toggle Word Wrap · Show Whitespace ·
Show Indent Guides · Zoom In/Out/Reset · **Theme ▸ (Dark · Light · Follow System · ─ · Syntax
Theme ▸)** · Full Screen (F11)

**Run** — Run (F5) · Run Without Building · Stop (Shift+F5) · Restart · Run Tests (Ctrl+F5) ·
Build (Ctrl+Shift+B) · Select Run Configuration ▸ · Edit Run Configurations…

**Tools** — Format Document (Ctrl+Alt+F) · Organise Imports · Lint Project · Open Terminal Here ·
Python ▸ (Select Interpreter… · **Create Virtual Environment…** · Install Packages… · Show Installed
Packages) · Rust ▸ (Select Toolchain… · `cargo clean` · `cargo update`) · Command Palette
(Ctrl+Shift+P)

**Help** — Documentation · Keyboard Shortcuts (Ctrl+K Ctrl+S) · Check Toolchains ·
Language Server Diagnostics · Open Log Folder · About

Toolbar (icon + tooltip with the shortcut, overflow menu when the window is narrow):
`New · Open · Save · Save All ‖ Undo · Redo ‖ Find · Replace ‖ Run · Stop · Format ‖ Problems ·
Terminal ‖ Settings`.

On macOS the menu bar must be the **native** one (`muda`) with the standard App menu, and shortcuts
must map Ctrl→Cmd. Handle this in the keymap layer from day one; retrofitting is miserable.

---

## 5. Command palette (Ctrl+Shift+P)

Every user-facing action is a `Command { id, title, category, keybinding, enabled_when }` in a
central registry. Menus, the toolbar, the keymap, the Help→Shortcuts page and the palette are **all
generated from that registry**. This is a modest amount of work in M1 that prevents a permanent
class of drift bugs (a menu item that does something different from its shortcut). Ctrl+P is the
same widget in file-search mode; `>` switches to commands, `:` to go-to-line, `@` to go-to-symbol.

---

## 6. Keymap

Default bindings ship as a TOML table; users can override in `keymap.toml`. Resolution is
context-aware (`editor`, `file_tree`, `output`, `global`) and supports two-key chords (`Ctrl+K
Ctrl+S`). Conflicts are detected at load and reported in the Problems panel rather than silently
resolved.

| Action | Windows/Linux | macOS |
|---|---|---|
| New / Open / Save / Save All | Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Alt+S | ⌘N / ⌘O / ⌘S / ⌥⌘S |
| Close tab / Reopen closed | Ctrl+W / Ctrl+Shift+T | ⌘W / ⇧⌘T |
| Find / Replace / in Files | Ctrl+F / Ctrl+H / Ctrl+Shift+F | ⌘F / ⌥⌘F / ⇧⌘F |
| Command palette / Go to file / symbol / line | Ctrl+Shift+P / Ctrl+P / Ctrl+Shift+O / Ctrl+G | ⇧⌘P / ⌘P / ⇧⌘O / ⌃G |
| Run / Stop / Build / Test | F5 / Shift+F5 / Ctrl+Shift+B / Ctrl+F5 | F5 / ⇧F5 / ⇧⌘B / ⌃F5 |
| Format document | Ctrl+Alt+F | ⌥⇧F |
| Toggle comment | Ctrl+/ | ⌘/ |
| Multi-cursor next occurrence / add above-below | Ctrl+D / Ctrl+Alt+↑↓ | ⌘D / ⌥⌘↑↓ |
| Go to definition / references / rename | F12 / Shift+F12 / F2 | F12 / ⇧F12 / F2 |
| Quick fix | Alt+Enter | ⌥⏎ |
| Settings | Ctrl+, | ⌘, |

---

## 7. New File wizard (File → New)

A modal dialog:

```
┌─ New File ────────────────────────────────────┐
│ Name:      [ main                       ] .py │
│ Language:  [ Python              ▾ ]          │
│ Location:  [ C:\proj\src            ] [ … ]   │
│ [x] Include boilerplate                       │
│ Template:  [ Script with main()   ▾ ]         │
│ ┌─ Preview ─────────────────────────────────┐ │
│ │ #!/usr/bin/env python3                    │ │
│ │ """main.py"""                             │ │
│ │ ...                                       │ │
│ └───────────────────────────────────────────┘ │
│                       [ Cancel ] [  Create  ] │
└───────────────────────────────────────────────┘
```

Behaviour: the extension follows the language selection (and vice versa if the user types one);
Location defaults to the file-tree selection or the project root; name validation blocks illegal
characters, reserved Windows names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`…), and existing paths;
**Include boilerplate** is a checkbox that reveals the Template dropdown and a live preview; Create
writes the file to disk, refreshes the tree, **opens it in a new tab**, and places the cursor at the
template's `$CURSOR` marker.

Templates live in `assets/templates/<lang>/<template>.tmpl` with `${NAME}`, `${DATE}`, `${AUTHOR}`,
`${CLASS_NAME}` (PascalCase of the filename) and `$CURSOR` substitutions — user-extensible via a
templates folder in the config directory.

Shipped templates:

- **Python**: *Empty* · *Script with `main()`* · *Class module* · *Unittest test case* ·
  *Pytest test module* · *Dataclass module* · *CLI (argparse)*
- **Rust**: *Empty* · *Binary `main.rs`* · *Library `lib.rs` with tests* · *Module* ·
  *Struct + impl* · *Trait* · *Integration test*
- **HTML**: *HTML5 skeleton* (linked CSS + JS) · **CSS**: *Reset + variables* ·
  **JS**: *Module* / *IIFE* · **JSON**: *Empty object* / *`package.json`* ·
  **INI**: *Sectioned config* · **TXT**: *Empty*

Reference — Python "Script with main()":

```python
#!/usr/bin/env python3
"""${NAME}.py

Created: ${DATE}
Author: ${AUTHOR}
"""

from __future__ import annotations

import sys


def main(argv: list[str] | None = None) -> int:
    """Entry point."""
    argv = list(sys.argv[1:] if argv is None else argv)
    $CURSOR
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

Reference — Rust "Binary main.rs":

```rust
//! ${NAME}
//!
//! Created: ${DATE}
//! Author: ${AUTHOR}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    $CURSOR
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
```

A sibling **New Project** action (Rust: `cargo new` wrapped; Python: `src/` + `tests/` +
`pyproject.toml` + `.gitignore` + optional `python -m venv .venv`) is a small addition on top of the
same machinery and worth including in 1.0.

---

## 8. Cross-cutting concerns worth designing now

| Concern | Approach |
|---|---|
| **External file changes** | `notify` watches open files; on change → reload silently if the buffer is clean, otherwise a non-modal bar: "Changed on disk — [Reload] [Keep Mine] [Diff]". On delete → mark the tab as orphaned, keep contents. |
| **Crash / hot exit** | Dirty buffers written to `{data_dir}/ide/backups/<doc-uuid>` every 3 s of idle; recovered on next launch with a prompt. Non-negotiable for trust. |
| **Large files** | > 5 MB: no highlighting, no LSP, no minimap, soft-wrap off. > 100 MB: refuse with a clear message. Detect binary content (NUL in the first 8 KB) and open a read-only hex-ish view instead of garbling it. |
| **Encoding & line endings** | Detect BOM → UTF-8/16; validate UTF-8; fall back to Latin-1 with a status-bar notice. Line endings detected and **preserved** on save (default for new files: platform native, overridable). Both shown and switchable in the status bar. |
| **Atomic saves** | Write to `<file>.tmp` in the same directory, fsync, then rename over the original. Preserve permissions and (on Unix) ownership where possible. Never truncate-in-place. |
| **Errors** | `anyhow` at the app boundary, `thiserror` in libraries. **No `unwrap()` on any path reachable from user input** — enforced by a clippy lint (`clippy::unwrap_used` denied in `ide-core`/`ide-lsp`/`ide-proc`). User-visible failures go to a toast + the log, never a panic. |
| **Panics** | `std::panic::set_hook` writes the backtrace to the log, attempts an emergency save of all dirty buffers, and shows a dialog with a "Copy report" button. |
| **Logging** | `tracing` + `tracing-subscriber` → rolling file in `{data_dir}/ide/logs/`, level from `RUST_LOG` or settings, in-app log viewer. |
| **Accessibility** | egui's AccessKit integration on; full keyboard navigation of every pane; respect OS "reduce motion" (kills the caret blink and animations); minimum 4.5:1 contrast in both themes; UI scale setting independent of editor font size. |
| **HiDPI / multi-monitor** | Honour per-monitor DPI; persist window geometry and validate it against current monitors on restore (a window remembered at `-3000,0` must not vanish off-screen). **The UI scale setting must be a zoom factor multiplying the native scale, never an absolute pixels-per-point** — setting the latter to 1.0 silently cancels the display's DPI scaling and renders the whole interface at 67% on a 150% display. Verify on a scaled display, not just at 100%; the bug is invisible at 1.0x. |
| **i18n** | Not in 1.0, but route every user-facing string through a `t!("key")` macro backed by `fluent` so 1.1 doesn't need a full sweep. Cheap now, expensive later. |
| **Single instance** | Second launch with a file argument forwards to the running instance (named pipe / Unix socket) and focuses it. Configurable. |
| **Security posture** | The IDE runs arbitrary user code by design — that's the point. But: never auto-run anything on folder open (no VS Code-style tasks-on-open), never execute anything from a project config file without a prompt, and treat `.ide/*.toml` as data only. |

---

## 9. Milestones

Effort assumes one experienced developer working steadily; the ranges are honest, not optimistic.
Each milestone ends with a tagged build and a manual smoke test on all three platforms.

| # | Milestone | Deliverables | Acceptance criteria | Est. |
|---|---|---|---|---|
| **M0** | Bootstrap | Workspace, crate skeletons, `rust-toolchain.toml`, deny.toml, logging, panic hook, blank window on all 3 OSes, build scripts | `cargo build --release` clean on Windows/Linux/macOS; window opens; log file written | 3–5 d |
| **M1** | Shell & layout | Docks with draggable splitters, menu bar (native on macOS), toolbar, status bar, tab bar with ×/dirty/reorder/overflow, file tree with lazy expand + watcher, open/save/save-as, session persistence, **command registry + palette**, settings persistence, **UI theme: dark/light/follow-system (§3.11)** | Open a folder, double-click 4 files → 4 tabs, close via ×, edit + save, quit and relaunch → same tabs and scroll positions; theme switches live and survives a restart | 3–4 wk |
| **M2** | Editor core | Ropey buffer, custom virtualised editor widget, gutter/line numbers, cursor + selection + multi-cursor, undo/redo with coalescing, clipboard, all navigation/line-manipulation commands, encoding + line-ending handling, atomic save, external-change detection | 200k-line file scrolls at 60 fps; 5 MB file opens < 150 ms; undo/redo survives a 10k-edit fuzz test; no data loss under the save/reload matrix | 5–7 wk |
| **M3** | Syntax highlighting | tree-sitter integration, 9 languages, HTML injections, incremental reparse, theme format, dark + light themes, bracket matching, folding, indent guides | Highlighting correct on a golden corpus; single-char edit reparse < 1 ms in a 5k-line file; unknown extension degrades to plain text | 2–3 wk |
| **M4** | Editing intelligence | Indent engine (esp. the Python rules in §3.4), auto-close/surround, comment toggle, smart backspace/home, re-indent on paste, `.editorconfig`, whitespace-on-save policies | The Python indentation test suite (≈60 cases) passes; hand-editing a real Django/Flask file feels right | 2–3 wk |
| **M5** | Search | In-file find/replace bar with regex, project-wide search + replace (ripgrep engine, streaming, cancellable), go-to-file/symbol/line | Search 50k files in < 2 s; project replace across 500 matches is a single undo | 1.5–2 wk |
| **M6** | Language servers | LSP client + lifecycle + crash recovery, diagnostics/Problems panel, completion popup with docs & snippets, signature help, hover, goto/references, rename, code actions, format-on-save, degradation ladder, toolchain detection panel | rust-analyzer and ruff+basedpyright all working on Windows/Linux/macOS; killing a server mid-session recovers; **with no servers installed the IDE still edits fine** | 5–7 wk |
| **M7** | Run & console | PTY session, output dock, ANSI, stdin, stop/restart, run configurations, interpreter discovery + **venv creation/selection UI (§3.8a)**, cargo detection, clickable error links, env/PATH settings | `input()` works; Ctrl-C stops a loop; a Rust compile error and a Python traceback both jump to the right line; creating a venv from the menu and running a script against it works on all 3 OSes; no orphan processes on quit | 4–5 wk |
| **M8** | Settings, themes, New File | Settings form + TOML round-trip preserving unknown keys, keymap file + conflict detection, theme switching, New File wizard + all templates, New Project | Every setting applies live; a hand-edited settings file survives a round trip; wizard creates + opens + positions the cursor correctly | 2–3 wk |
| **M9** | Polish & docs | Help/About, shortcut sheet, user manual, third-party licences, accessibility pass, empty/error states, icons, app icon, first-run experience, perf pass against the budgets | Full manual test matrix green; no `unwrap` lints; startup to interactive < 500 ms cold | 2–3 wk |
| **M10** | Release 1.0.0 | Windows portable zip + MSI, macOS .app + dmg (universal, unsigned), Linux AppImage + .deb + tarball, SHA-256 checksums, CHANGELOG, release notes, unsigned-binary instructions | Clean install on a fresh VM of each OS opens, edits, runs, and quits without a toolchain installed | 1.5–2 wk |

**Total: roughly 7–9 months** solo at a steady pace. M2 and M6 are where estimates slip; treat them
as the schedule risk. A useful internal preview exists after M4.

Pre-1.0 tags: `1.0.0-alpha.1` at M4, `1.0.0-alpha.2` at M6, `1.0.0-beta.1` at M8,
`1.0.0-rc.1` at M9, `1.0.0` at M10.

---

## 10. Dependency shortlist

Pin exact versions at M0 with `cargo add` and commit `Cargo.lock` (this is a binary — the lockfile
is part of the build contract). Vet each with `cargo deny` before adopting.

**UI** `eframe`/`egui` · `egui_extras` · `muda` (native menus) · `rfd` (native file dialogs) ·
`arboard` (clipboard incl. images) · `image` (icons)
**Text** `ropey` · `unicode-segmentation` · `unicode-width` · `encoding_rs` · `similar` (diffs)
**Syntax** `tree-sitter` + `tree-sitter-highlight` + the nine grammar crates
**LSP** `lsp-types` · `serde`/`serde_json` · `tokio` (process, io-util, sync, time)
**Process** `portable-pty` · `alacritty_terminal` (VT parsing) · `shell-words` ·
`windows` (Job Objects) / `nix` (process groups)
**Files** `notify` + `notify-debouncer-full` · `ignore` · `grep-searcher`/`grep-regex` · `walkdir` ·
`trash` · `directories` · `open` (reveal in file manager)
**Config** `toml_edit` · `serde` · `figment` (layered config) — or hand-rolled layering, it's small
**Misc** `anyhow` · `thiserror` · `tracing` + `tracing-appender` · `nucleo` (fuzzy matching — the
Helix matcher, much better than naive subsequence scoring) · `regex` · `once_cell` · `vergen`
**Dev** `criterion` · `insta` (snapshot tests) · `egui_kittest` · `proptest` · `cargo-about` ·
`cargo-deny`

**Build prerequisite to document:** tree-sitter grammars compile C, so contributors need a C
toolchain (MSVC Build Tools on Windows, `cc` elsewhere). Note it in the README — it is the single
most likely first-build failure.

---

## 11. Testing strategy

- **Unit** (`ide-core`): rope edit/undo invariants, offset↔line/col round-trips, indent engine cases,
  encoding detection, selection arithmetic. Target: high coverage here, this is where correctness
  bugs cost the most.
- **Property tests** (`proptest`): apply N random transactions then undo N times → the buffer must
  byte-equal the original. Random edits must never make tree-sitter's tree disagree with a from-
  scratch parse.
- **Golden/snapshot** (`insta`): highlight spans per language against a checked-in corpus; template
  rendering; settings TOML round-trips.
- **Integration**: a mock LSP server (a small binary replaying scripted JSON-RPC) exercising
  handshake, diagnostics, completion, crash + restart — no real toolchain needed in the test suite.
- **UI**: `egui_kittest` for widget-level interaction and image snapshots of the editor with a fixed
  embedded font (so snapshots are stable across machines).
- **Bench** (`criterion`): frame paint, edit-to-reparse, file open, project search — asserted
  against the §2.4 budgets so regressions are caught, not discovered.
- **Manual matrix** per release, scripted in `docs/test-matrix.md`: each OS × {open/edit/save, Unicode
  + CJK/IME input, CRLF vs LF, run Python, run Rust, kill a running process, HiDPI, multi-monitor,
  no toolchains installed, read-only file, file deleted underneath you}.

---

## 12. Non-GitHub logistics

- **Repository**: local `git init`; push to a bare repo on a second disk / NAS / USB as `origin`
  (`git remote add origin /mnt/backup/ide.git`). If a web UI and issue tracker are ever wanted,
  self-host **Forgejo** or **Gitea** — both are a single binary and run happily on a NAS or a small
  VPS. Verify the backup by cloning it fresh once a month; an untested backup is not a backup.
- **Issue tracking**: `docs/TODO.md` with a simple `- [ ] (P1) …` convention, or a local
  Forgejo/Jira-lite. Keep the milestone acceptance criteria from §9 as the checklists.
- **CI**: no hosted runners. Use a git `pre-push` hook running `cargo fmt --check && cargo clippy
  -- -D warnings && cargo test`, plus a `just release` / `cargo-make` task that builds, tests,
  audits (`cargo deny check`) and packages. If a Linux VM or WSL is available, a nightly cron
  building all three targets gets you 80 % of CI's value.
- **Building for all three platforms** without CI: Windows natively; Linux via WSL2 or a VM (build
  against an old glibc — Ubuntu 20.04 in Docker — or glibc symbol errors will hit users);
  **macOS requires a Mac** to produce a `.app` at all, signed or not.
- **No code signing** (D10). Ship SHA-256 checksums with every artefact and put the bypass steps in
  the README and on the download page: Windows → *More info → Run anyway*; macOS → right-click the
  app → *Open* (or `xattr -dr com.apple.quarantine /Applications/The\ Editor.app`). Prefer the
  portable `.zip` and Linux tarball/AppImage as the primary distribution formats — installers draw
  more SmartScreen attention than a plain executable does.
- **Versioning**: SemVer from `1.0.0`, single source of truth in the workspace `Cargo.toml`,
  surfaced in About via `env!("CARGO_PKG_VERSION")` + `vergen` build metadata.
  Manual `CHANGELOG.md` in Keep-a-Changelog format, updated per milestone, not at release time.

---

## 13. Post-1.0 roadmap

| Version | Theme | Contents |
|---|---|---|
| 1.1 | Layout & terminal | Split panes / editor groups, full multi-tab integrated terminal, multi-root workspaces, minimap |
| 1.2 | Version control | Git pane: status, stage/unstage, diff view, commit, branch switch, blame gutter, inline change markers |
| 1.3 | Debugging | DAP client — breakpoints, step, variables, watch, call stack; `debugpy` for Python, `codelldb` for Rust |
| 1.4 | Extensibility | WASM plugin API (commands, themes, language registration), snippet manager, user tasks |
| 1.5 | Reach | Jupyter/`.ipynb` support, remote/SSH editing, i18n, Markdown preview, refactoring beyond LSP rename |

Explicitly **out of scope for 1.0** so the release actually ships: split panes, git integration,
debugging, plugins, notebooks, remote editing, collaborative editing, AI assistance.

---

## 14. Status

Audited against the code on 2026-08-07, not from memory. Every "still to do"
below was checked by looking for the thing itself, because a status section
that drifts from the source is worse than none.

- [x] Name, author, licence, toolkit, linting policy and signing policy settled (§1).
- [x] **M0 — Bootstrap.** Workspace + 8 crates, logging, panic hook with emergency-save
      scaffold, blank `eframe` window, cargo aliases, git repo. See `docs/M0-NOTES.md`.
- [x] **Spike — virtualised rope rendering.** `cargo spike` runs it; findings in
      `docs/SPIKE-NOTES.md`.

Roughly 22,500 lines of Rust and 453 tests. Nothing has ever been built or run on
Linux or macOS, which remains the single largest piece of unknown work.

### Where each milestone stands

| # | Milestone | State | What is missing |
|---|---|---|---|
| M1 | Shell & layout | Substantially done | Native macOS menu bar via `muda`. Tab overflow and most-recently-used Ctrl+Tab cycling are done. |
| M2 | Editor core | **Done** | — |
| M3 | Syntax highlighting | Done for 9 languages | HTML injections (embedded `<script>`/`<style>`); shebang and manual language override; the user-editable TOML theme format |
| M4 | Editing intelligence | Indent engine done | Re-indent on paste. `.editorconfig`, save-time tidying, bracket matching and code folding are done. |
| M5 | Search | In-file done | Project-wide search and replace on the ripgrep engine — the crate holds the query engine and a file walk so far; go-to-symbol |
| M6 | Language servers | Mostly done | Hover, rename, code actions, format-on-save. Diagnostics, Go to Definition, Find Uses and the completion popup are done; all four have a parse-tree fallback for when no server can answer |
| M7 | Run & console | Running done | Install Packages / Show Installed Packages; run configurations in `.ide/run.toml` |
| M8 | Settings, themes, New File | Form done | The keymap file and conflict detection (only its path exists), user themes, and New Project. `editor.word_wrap` and `ui.syntax_theme` are read from the file and ignored by everything else, so the form does not offer them |
| M9 | Polish & docs | Mostly done | Remaining: a 4.5:1 contrast audit of both themes, and reading the operating system's own reduce-motion preference rather than a setting of our own |
| M10 | Release 1.0.0 | Windows done | macOS `.app`/dmg and Linux AppImage/deb; release notes. Windows ships as a single statically linked exe in a zip with a SHA-256, built by `tools/make-release.bat` |

### D1's renderer, reversed

D1 chose wgpu with glow as the fallback. Measuring M9's startup budget turned
that round. Instrumenting the phases showed our own code takes 5 ms of a 1.3
second start: everything else is eframe creating the window and the graphics
device. On this machine wgpu takes about 2,470 ms to the first frame and glow
about 80 ms, and the two render identically — the same screenshot, pixel for
pixel.

Thirty times the startup for nothing anybody can see is not a trade worth
keeping, so glow is the default. wgpu remains selectable in Settings, because
OpenGL drivers are the weaker link on some machines and a renderer that will
not start needs an alternative; `THE_EDITOR_RENDERER=wgpu` is the escape hatch
for when the window never appears, which is exactly when the settings form
cannot be reached.

### Landed beyond the plan

- **A recent files list** (File → Open Recent, and on the welcome screen), and a
  confirmation before deleting from the file tree.
- **Go to Definition / Find Uses with a parse-tree fallback**, so both work with
  no language server installed — within the open file, and saying so.

- **Built-in syntax checking** from the tree-sitter tree, so broken code is
  flagged with no language server installed. Not in the original plan; the
  degradation ladder in §3.6 said the IDE must stay usable without servers, and
  showing nothing at all was a poor reading of that.
- **Help → Check Toolchains** with per-tool install commands (§3.6 referred to
  it from M0; it did not exist until now).
- **The New File dialog** and all its templates, pulled forward from M8.
- The Save / Don't Save / Cancel guard on closing a dirty tab, Close Others,
  Close All and quitting.
- `editor_proc::spawn::quiet`, so background children get no console window on
  Windows.
- **A glyph audit, and a test that keeps it true.** egui's bundled fonts cover
  less than they appear to, and `Fonts::has_glyph` — the API for asking which —
  is wrong in both directions in epaint 0.36: it reports plain `a` as absent
  from the monospace family and `⚠` as absent from the proportional one. So
  `icon::pick` had been silently falling through to its ASCII fallbacks and the
  toolbar read `Un Re SA Fi`, while the glyphs that do *not* go through it —
  the error marker, the theme indicator, the dirty-tab dot, the explorer's
  chevrons — drew as empty boxes. Both now ask what was actually rasterised: a
  character the fonts lack lands on the same rectangle of the font atlas as
  `U+FFFD`, and that comparison cannot disagree with the screen.
  `editor_widgets::glyphs` names every symbol drawn outside `pick`, with the
  font family it is drawn in, and a test checks all of them.
- **`editor-vcs`**, and with it the read-only half of version control: change
  markers in the gutter, the branch in the status bar, and **View → Changes
  Since Last Commit** (`Ctrl+Shift+G`) showing the buffer against HEAD as a
  unified diff. Git is *run*, not linked, so the user's own configuration,
  credential helper, hooks and signing key all apply. The committed version of
  a file is fetched once on a worker thread and diffed in process against the
  buffer, which is what turns "a subprocess per keystroke" into "a subprocess
  per file"; the cache is dropped whole when HEAD moves.
- **A Source Control panel** (`Ctrl+G`): the working tree in three groups —
  conflicts, staged, not staged — with stage, unstage and discard, and the
  whole list at once. `git status --porcelain=v1 -z`, so a filename containing
  a newline is read correctly rather than unescaped by a second parser.
  Discarding is the one action in the editor that destroys work git cannot get
  back, so the panel cannot do it: it reports the request and the application
  confirms first, naming the files and saying plainly that neither Undo nor git
  will help. The worker re-reads the status after every action rather than
  letting the caller ask, because a caller that asks for itself can ask too
  early and get the state from before the action — which is what makes a
  staging panel look like it does nothing.
- **Committing, amending, history and blame.** The message box lives in the
  panel and survives a commit git refused: a hook that rejects the change must
  not also throw away the sentence explaining it, so only a commit that actually
  landed empties it. Amending starts from the message it is replacing. **History**
  (from the panel) is the log in one pane and the selected commit's message and
  files in the other, a hundred at a time. **Ctrl+Shift+B** annotates every line
  of the open file with who last touched it, read from `git blame --porcelain`
  — the form that states each commit once and refers back to it, rather than
  repeating the headers a thousand times for a thousand lines.
  `%x1f`/`%x1e` separate the log's fields, because every separator a human would
  choose is one a commit subject can contain.
- **A test runner.** pytest for Python and `cargo test` for Rust, both *run*
  rather than reimplemented. Results are read from the runner's own output as
  it arrives rather than from a report file: `--junit-xml` would be more stable
  and is written when the run *ends*, so a suite taking two minutes would show
  nothing for two minutes and then everything. The Tests panel fills in as it
  goes, failures first, click one to land on the line it failed on.

  Two things that were not obvious. The test the caret is in comes from the
  *outline* the syntax layer already builds — nearest declaration above,
  enclosing ones above that — rather than from a second set of tree-sitter
  queries, which also keeps it working in a file that does not currently parse.
  And test runs go through **plain pipes**, not the console's pseudo-terminal:
  pytest on a terminal redraws its lines to keep a percentage at the right-hand
  edge, and what comes out has almost no newlines in it. Half the results were
  being lost. `editor_proc::pipe` exists for that, and nothing is given up by
  it — nobody types into a test run.

  Not done: a run button in the gutter beside each test, which §13 asked for.
  The gutter already carries four columns and a fifth is too many; the command,
  the menu and the panel cover the same ground.

- **Branches and remotes**, which completes it. The panel names the branch and
  how far it is ahead of or behind its upstream; **Branches…** switches, creates
  and deletes. Branch names are validated before git sees them, so a mistyped
  one is refused with a sentence as it is typed. Deleting tries the safe way
  first and turns git's refusal into the offer to force — which is the shape
  worth copying elsewhere: attempt the safe thing, and only raise the dangerous
  one when the safe one is impossible.

  The three remote operations are deliberately conservative. `pull` is
  `--ff-only`, because a pull that merges can leave a conflicted working tree
  behind a button click; `push` is never forced, because no dialog makes a
  button for that a good idea. `GIT_TERMINAL_PROMPT=0` means anything wanting a
  password fails rather than hanging, and the terminal is there for those. They
  run on the same single worker as everything else — two git processes on one
  repository contend for the index lock — so the panel says which one is
  running rather than appearing to have stopped.

### Sequencing changes made along the way

M2's editing widget was brought forward ahead of the rest of M1: an IDE you
cannot type into is impossible to evaluate. The New File dialog moved from M8 to
just after M2 for the same reason — a "new file" command that cannot name the
file or pick its language is only half a feature.

### Not in the original plan, and arguably should have been

Measured against PyCharm and VS Code rather than against §3. None of these were
considered when the plan was written; several are more valuable than things
that were.

**Since built**, and struck from this list: the integrated terminal, rename and
refactor, crash-recovery autosave, the accessibility pass, package and
requirements management, go to symbol, code folding, the **test runner**, and
**version control** —
all of it: gutter change markers, staging, committing, history, blame, branches
and remotes. That was "the largest omission by a distance" when this list was
written, and it is now the largest thing on the other side of it. Two more that
appeared here in an earlier revision turned out to exist already —
most-recently-used Ctrl+Tab cycling and tab overflow — which is its own lesson
about reviewing from memory rather than from the code.

| | Why it matters | Rough size |
|---|---|---|
| **Hover** | Type and docstring under the pointer. The language-server plumbing is all there and no `textDocument/hover` is ever sent. The cheapest remaining LSP win. | 2–3 d |
| **Format on save** | `textDocument/formatting`, or Ruff directly. Expected of any Python IDE. | 2–3 d |
| **Outline / breadcrumbs / go to symbol** | `documentSymbol` gives all three. Navigating a 3,000-line file is currently scrolling. | 0.5 wk |
| **Auto-import** | Type `Path`, get `from pathlib import Path`. High value in Python specifically, and a code action the servers already offer. | 1 wk |
| **Split panes** | Two files side by side, or two places in one file. Structural — the editor pane assumes a single active document — so cheaper now than after more is built on that assumption. | 1.5–2 wk |
| **Diff / merge viewer** | Needed by version control, and useful on its own for comparing two files. | 1 wk |
| **A project-wide "changed on disk" signal** | The reload bar only appears for the *active* tab, so a tool that rewrites a dozen files is discovered one tab at a time as you switch between them. A count in the status bar and a "reload all unmodified" action. Small, and it is what makes working alongside a coding agent bearable. | 1 d |

#### On coding-agent integration

Claude Code and its like are command-line tools, and the integrated terminal
runs them in the project with the virtual environment on `PATH`. It became a
real terminal for this reason: a full-screen program needs cursor addressing,
the alternate screen and raw keys, and a scrollback that understood colour gave
it none of those while claiming through `TERM` that it did.

What was still missing was not a chat panel — that would duplicate a working CLI
and tie the editor to one vendor's interface. It was being able to see what an
agent changed and undo part of it, which is version control, plus the
project-wide change signal below. Version control has since landed in full: the
gutter marks what an agent touched, the Source Control panel stages or discards
it a file at a time, and the diff view shows exactly what it did. The change
signal is still open, and is now the only part of this missing.

Deliberately out of scope, recorded so the decision is not re-litigated: remote
development over SSH or containers, Jupyter notebooks, database tools, and
profiling. Each is a product in itself.

### Outstanding logistics

- Set up the bare backup remote (`git remote add origin <path-to-nas>/the-editor.git`).
- Decide the distribution channel for releases (a plain web page with checksums
  is enough).
- No command-line file argument: `the-editor foo.py` opens an empty window, so
  "Open with…" and double-clicking a file in Explorer do not work.
- Ruff's language server exits once on startup and is restarted by the recovery
  path. Harmless, but it should not be happening.
- A wgpu renderer panic ("Failed to create staging buffer for index data") took
  the whole editor down once, on a machine started with a stripped `PATH` —
  probably a software-renderer fallback. Not reproducible with a normal
  environment, but a renderer panic should not be fatal.
- Shift+F12 (Find Uses) could not be confirmed working from a synthetic
  keystroke, though plain F12, F8 and Shift+F8 all were, and the command itself
  works from the menu. Windows reserves F12 for debuggers; worth checking by
  hand.
