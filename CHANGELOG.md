# Changelog

All notable changes to The Editor are recorded here, in the
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. Versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries are written as each milestone lands, not retroactively at release time.

## [Unreleased]

### Added

- **New File dialog** (Ctrl+N). Name, language, location, and an optional
  boilerplate template with a live preview of exactly what will be written.
  - Name and language stay in step both ways: choosing Rust sets `.rs`, and
    typing `.rs` switches the language to Rust. A dotfile such as `.gitignore`
    is taken as a complete name rather than having an extension appended.
  - 27 templates across Python, Rust, HTML, CSS, JavaScript, JSON, INI, TOML,
    Markdown and plain text, with `${NAME}`, `${FILENAME}`, `${CLASS_NAME}`,
    `${AUTHOR}` and `${DATE}` substitution, and a `$CURSOR` marker that places
    the caret where you would start typing.
  - Names are validated before anything is written, including the rules that
    only Windows enforces — reserved device names (`CON`, `NUL.txt`, `COM1.py`),
    trailing dots and spaces, and illegal characters — on every platform, so a
    project created on Linux does not become un-checkoutable on Windows.
  - An existing file requires a second, explicit click to overwrite.
  - Ctrl+Shift+N still creates a plain untitled buffer with no dialog.
- **Unsaved changes are now guarded.** Closing a tab, Close Others, Close All
  and quitting the application all prompt Save / Don't Save / Cancel when there
  is unsaved work, listing the affected files. Escape is Cancel, never the
  destructive option. A save that fails cancels the close rather than
  proceeding and losing the work.

- **M2 (in progress) — Editor core.** The editor pane is now editable.
  - Virtualised painting: only the visible rows are laid out, so cost tracks the
    viewport rather than the file size.
  - Typing, Enter with the previous line's indentation carried over, Tab to the
    next tab stop, Delete, and smart Backspace that clears one indent level
    inside leading whitespace and one character everywhere else.
  - Selection by click, drag, shift-click and double-click; double-click selects
    whole `snake_case` identifiers.
  - Caret motion by arrows, Page Up/Down, Home/End (Home toggles between the
    first non-whitespace character and column zero) and Ctrl+Home/End, with a
    sticky goal column so crossing a short line and coming back returns to the
    original column.
  - Cut, copy, paste and select all, from both the keyboard and the Edit menu.
  - Undo and redo, with a run of typing or backspaces coalesced into one step,
    and the caret restored to where the edit happened rather than wherever it
    drifted to since.
  - Caret and click positions use galley cursor mapping rather than assuming a
    fixed character width, so accented and CJK text behaves correctly.
  - Status bar shows Ln/Col and the selection size; opening a document or
    selecting its tab focuses the editor so it can be typed into immediately.
- **Interface text size** is now a setting, `[ui] font_size` (default 14.0),
  independent of `[editor] font_size` for the code pane. All interface text
  scales from it, and panel heights follow the text size rather than being
  fixed, so raising it does not clip the toolbar or status bar.

### Fixed

- **HiDPI displays rendered the whole interface at the wrong scale.** Applying
  the UI scale used `set_pixels_per_point`, which means "one physical pixel per
  logical point" and therefore *cancels* the display's own DPI scaling. On a
  150% display the interface came out at 67% of its intended size; on a 200%
  display, 50%. It now uses `set_zoom_factor`, which multiplies the native
  scale, so 1.0 means "whatever this monitor reports". `ui_scale` is now a true
  zoom on top of DPI, and Ctrl+`+` / Ctrl+`-` / Ctrl+`0` compose correctly with
  it.
- Hovering a tab showed the text I-beam instead of a pointer. Tabs were built
  from `Label` widgets, which set the text cursor because that is what a label
  does. Each tab is now a single measured, allocated widget with an explicit
  pointer cursor. The code pane, which really is text, keeps the I-beam.
- The tab's unsaved-dot / close-cross swap tested the pointer against a
  rectangle that had not been decided yet, so it responded to the wrong region.
  The tab is now measured before it is painted, making the hover state exact.
- Multi-edit transactions recorded their inverse in pre-edit coordinates, so
  undoing a transaction containing two edits of differing lengths corrupted the
  document. Single-edit undo was unaffected, which is why it was invisible until
  a test covered the multi-edit case. The inverse is now shifted by the
  cumulative length change of every preceding edit.

- **M1 (in progress) — Shell & layout.**
  - **Themes.** Dark (default), Light, and Follow System, switchable from the
    View menu, the command palette, or the status-bar indicator. Applied live,
    persisted immediately. Both themes are checked against WCAG AA contrast by
    unit tests rather than by eye, and share identical geometry so switching
    changes colour only.
  - **Command registry.** Every action is registered once with its title,
    category and shortcut; the menus, toolbar, keyboard handling, palette and
    the Help → Keyboard Shortcuts window are all generated from it. Tests
    enforce that no command is unregistered, duplicated, or sharing a shortcut.
  - **Command palette** (Ctrl+Shift+P) with `nucleo` fuzzy matching.
  - **Settings** at `config/settings.toml`, written documented on first run.
    Unknown keys, comments and key order survive a rewrite, so downgrading The
    Editor cannot silently delete settings written by a newer build. Malformed
    or wrongly typed values fall back to defaults with a visible message instead
    of preventing startup; out-of-range values are clamped.
  - **Documents** with encoding detection (UTF-8, UTF-8 BOM, UTF-16 LE/BE,
    Windows-1252 fallback) and line-ending preservation — a CRLF file opens,
    saves, and is byte-identical. Saves are atomic. Binary files are refused
    rather than corrupted; files over 5 MB open read-only; over 100 MB refused.
  - **Explorer** with lazy directory expansion, noise directories excluded,
    hidden-file toggle, and a filter box.
  - **Tabs** with close buttons, unsaved markers, preview tabs, middle-click
    close and a context menu.
  - Status bar showing language, encoding, line ending, indentation and line
    count; toast messages for errors; About and Keyboard Shortcuts windows.
  - The editor pane is a read-only viewer until M2 replaces it.

- **M0 — Bootstrap.** Cargo workspace with eight crates plus a throwaway
  rendering spike; pinned toolchain; MIT licence; `cargo-deny` policy allowing
  permissive licences only; cargo aliases for the standard checks.
- Per-platform config/data/log/backup path resolution, with a portable-install
  mode triggered by an `the-editor.portable` marker beside the executable.
- Rolling file logging via `tracing`, level controlled by `RUST_LOG`.
- Panic hook that logs a backtrace, invokes an emergency-save callback for
  unsaved buffers, and reports where the log and backups went. The callback
  registration exists now; M2 supplies the documents.
- Application shell: window, menu bar, toolbar, status bar, explorer panel,
  bottom dock and About dialog — all placeholders pending M1.
- Rendering spike (`cargo spike`) validating virtualised painting of a
  `ropey::Rope` with a working caret, and measuring paint and edit cost.

[Unreleased]: https://example.invalid/the-editor/compare/v1.0.0...HEAD
