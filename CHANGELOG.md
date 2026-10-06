# Changelog

All notable changes to The Editor are recorded here, in the
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. Versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries are written as each milestone lands, not retroactively at release time.

## [Unreleased]

### Added

- **Select and copy text in the Problems panel.** Press in the space to the
  right of a problem and drag to select across rows, then Ctrl+C. Clicking
  the problem itself still goes to it.

- **Python type checking level.** Settings > Python > Python type checking
  sets how strictly Pyright checks: Off, Basic, Standard or Strict. A project's
  own `pyrightconfig.json` or `[tool.pyright]` still takes precedence.

### Changed

- **Pyright no longer type-checks by default.** At "standard" it reported
  hundreds of errors on working code that has no type hints. Nearly all of
  them were gaps in a library's stubs, or an attribute set to `None` in
  `__init__` and filled in later. Pyright still provides hover, completion and
  Go to Definition, and Ruff still reports undefined names and unused imports.
  The stricter levels are in Settings for code that uses type hints.

### Fixed

- **Unticked checkboxes are visible in the dark theme.** Idle controls now
  have an outline at 3:1 contrast in both themes.
- **A right-click on an underlined problem shows its menu.** The hover box
  was drawn on top of the context menu. It now closes while a menu is open.
- Several Settings hints had runs of spaces in the middle of a sentence.

## [1.2.0] - 2026-10-05

### Added

- **Hovering a problem says what it is.** The hover box over an underline now
  leads with the problem itself: its severity, rule, the server that reported
  it, and the whole message. Until now it showed only the type, and the reason
  for the red line had to be hunted for in the Problems panel. Hovering the
  gutter marker or the line number lists every problem on that line.

- **Right-click a problem to find it or copy it.** **Show in Problems** opens
  the panel scrolled to that entry and highlighted; **Copy Problem** puts its
  location, rule and full message on the clipboard.

### Changed

- **A file opened from the explorer stays open.** A single click opened the
  file in a preview tab that the next click replaced, so opening several
  files meant editing each one before clicking the next. Every file now gets
  its own tab. The preview behaviour is still there, off by default, as
  **Settings > Preview tabs**.

### Fixed

- **Warnings that are not underlined are still marked in the gutter.** With
  underlining set to errors only, warnings lost their gutter marker as well,
  although the setting promises they stay there.

## [1.1.0] - 2026-09-26

### Added

- **The status bar says what will run the file.** For a Python file, the
  language name is replaced by the interpreter and where it came from —
  `Python 3.14.6 (.venv)` — and clicking it chooses another; if no Python can
  be found, or the chosen one does not run, it says so. The Run button, the
  debugger, the test runner and the Packages panel all use that interpreter,
  and until now nothing on screen said when it had quietly become a global
  Python instead of the project's environment. A Rust file shows the version
  of `rustc` that the project's toolchain file selects, and opens Check
  Toolchains. Versions are asked for once, in the background.

- **Opening a folder no longer runs anything in it until you say so.** A
  project's virtual environment was searched for language servers before
  anything installed on the machine, so a repository that shipped its own
  `.venv/Scripts/ruff.exe` had it started as soon as one of its Python files
  was opened. rust-analyzer does the same in its own way — it builds a Rust
  project's build scripts and macros to understand it — and a
  `rust-toolchain.toml` can name a toolchain by path.

  The first time a folder containing any of those is opened, The Editor asks
  whether to trust it, and remembers the answer (in `trusted_folders.toml`,
  beside the settings). Until it is trusted the folder opens, highlights and is
  checked by the tools installed on the machine, but nothing it contains is
  run: no tools from its environment, no rust-analyzer, not even its Python to
  ask the version. **Tools > Folder Trust** changes the answer. Folders with
  nothing in them that would run are never asked about, and creating a
  virtual environment from the editor trusts the folder it is created in.

### Changed

- **The indentation in the status bar is the file's own.** It showed the
  global setting even where the project's `.editorconfig` said otherwise and
  the editor was following that instead.

- **Rename works on a file with unsaved changes.** It used to refuse, because
  a language server could be looking at the file on disk rather than at the
  buffer. It no longer can (see below), so the refusal has gone.

- **The gutter is narrower, and the line numbers sit where the gap was.** A
  column was reserved for the fold arrows but the arrows were drawn in the
  padding beside the numbers, so the reserved space showed up as a blank band
  to the left of them, and clicking the last digit of a line number could fold
  the function instead. The gutter is now laid out once and read by both the
  drawing and the clicking: breakpoints and the problem marker share one
  column (a breakpoint wins, and the squiggle still marks the problem), the
  numbers are exactly as wide as their digits, and the fold arrows have a
  column one character wide.

### Fixed

- **A shortcut with an extra modifier no longer runs a different command.**
  Ctrl+Shift+W, bound to nothing, closed the tab, because the extra Shift was
  ignored; and on keyboards with an AltGr key, typing a letter such as Polish
  `ą` (AltGr+A, which arrives as Ctrl+Alt+A) ran that letter's Ctrl shortcut.
  Letters, function keys and navigation keys now need exactly the modifiers
  they are bound with. Punctuation and digits still forgive a Shift, because
  on some layouts it is part of typing them — `/` is Shift+7 on a German
  keyboard — and Toggle Comment has to keep working there.

- **Long lines can be scrolled to their end.** The editor was a fixed hundred
  and twenty columns wide however long a line was, so the rest of a longer
  line could not be scrolled into view, and typing at its end put the caret
  off the edge of the window. It is now as wide as the widest line that has
  been on screen. Jumping to a place on a long line that is off screen also
  scrolls sideways to it, rather than to the start of the line.

- **Breakpoints stay on their statements through undo, replace and rename.**
  They were moved by comparing the line count before and after an edit and
  assuming the edit was at the caret — so an undo, a replace-all or a rename
  that changed lines anywhere else moved them onto the wrong statements. Each
  edit now records which line it began on and how many lines it added or
  removed, and breakpoints follow exactly that. One on a deleted line lands
  where the deletion was rather than above it.

- **Breakpoints below a folded block are drawn.** They were compared against
  screen rows as though they were line numbers, so once anything above was
  folded a breakpoint could be set and not shown.

- **Open files no longer lose their language server when a virtual environment
  is created or a folder opened.** Either one restarts the servers, and the
  restart forgot every open file — while the editor remembered having sent
  them and never sent them again. Tabs already open lost their diagnostics,
  completion, hover and Go to Definition until they were closed and reopened.
  Only the language-server session keeps that record now, so starting afresh
  there starts afresh everywhere.

- **Squiggles, jumps and renames land on the right character on lines with an
  emoji.** Language servers count columns in UTF-16 units unless told
  otherwise, and the editor counts characters, so every emoji before a
  position moved it one to the right — and a rename replaced the wrong
  characters. Positions are now converted both ways, and servers are offered
  the editor's own count first, which rust-analyzer accepts.

- **A language server that restarts is told what is in the buffer, not what
  is on disk.** After a crash — and Ruff restarts once on every startup — a
  server was re-sent the saved file under the buffer's version number, and
  until the next keystroke answered every question about text that was not on
  the screen. Files are also no longer sent to a server before its handshake
  has finished, which the protocol forbids — and which is what made Ruff
  crash and restart every time the editor started.

- **Stopping a language server no longer freezes the window.** Opening a
  folder or quitting waited on each server in turn, up to half a second
  apiece, on the thread that draws the window. They are asked to shut down
  properly — the protocol's `shutdown` before `exit` — and waited for in the
  background; quitting waits for all of them at once, for at most a second.

- **Opening the first Python or Rust file no longer freezes the window.**
  Finding a language server means running it once to check it works, and a
  Node-based one such as basedpyright takes most of a second to answer. That
  happened on the thread that draws the window. It happens in the background
  now, and the server starts when the answer arrives.

- **The editor stopped searching the disk on every frame.** Which Python to
  use, where the project's virtual environment is, whether there is a
  `requirements.txt`, and what `.editorconfig` says were each looked up again
  sixty times a second while typing — the Python search alone is a few hundred
  file checks on an ordinary Windows machine. They are remembered now, and
  looked up again when the file watcher sees the project change, when a
  different folder or interpreter is chosen, or after a few seconds.

- **A language server that will not stop is stopped with everything it
  started.** One that ignored the request to shut down was killed, but only
  the process the editor had started — which, for a server installed with
  npm on Windows, is the `cmd` shim in front of it, leaving the server itself
  running. The whole process tree goes now.

- **Files over 5 MB are no longer sent to language servers**, as they were
  always meant not to be.

- **Saving a Windows-1252 file no longer rewrites what it cannot store.** A
  file that was not valid UTF-8 opens as Windows-1252, and anything typed into
  it outside that character set — an arrow, a curly quote from another
  program, an emoji — was written to disk as an HTML entity: `→` became
  `&#8594;`, with no warning, and the next time the file was opened the entity
  was what it said. Saving now stops and asks, offering to save the file as
  UTF-8 instead; declining leaves the file on disk exactly as it was. Save All
  and the quit prompt name the character and its line rather than asking.

- **Saving no longer breaks links, or destroys a file that happened to have
  the wrong name.** A save wrote `<file>.tmp` and renamed it over the
  original, which went wrong four ways:
  - a file of your own called `notes.py.tmp` was overwritten by saving
    `notes.py`, then renamed away;
  - a hard-linked file was split in two, the other names keeping the old
    contents;
  - a symlink was replaced by an ordinary file, so the file it pointed to
    stopped receiving the edits;
  - on Linux and macOS, a script lost its executable bit.

  The temporary file now has a name nobody would choose and is created only if
  that name is free; its contents are flushed to disk before the rename, so a
  power cut cannot leave an empty file behind; a symlink is followed and its
  target written; a file with several names is written in place after a
  flushed copy has been put aside; and the original's permissions are kept.

- **About names the commit the program was built from.** The build noticed a
  new commit only when the branch changed, so a build made after committing
  on the same branch reported an older commit.

- **A mostly-LF file with a few CRLF lines no longer hides a character on
  them.** The stray `\r` stayed in the buffer, where the caret stepped over it
  and search could match it. Mixed endings are now resolved to whichever is
  more common, in both directions, as they always were meant to be.

## [1.0.1] - 2026-09-12

### Added

- **The declaration you are inside stays on screen.** Scroll past a `def`,
  `class`, `fn`, `struct`, `trait` or `mod` header and it pins itself to the
  top of the editor, up to four levels of nesting, so a method two hundred
  lines long is still a method you can name. Click a pinned row to jump back
  to it.

  The pinned rows are the real lines of the file, highlighted the way they are
  highlighted in place, with their real line numbers beside them. A header made
  of reconstructed names has to invent a notation for signatures, decorators
  and generics and gets it subtly wrong; the source line is already the
  notation the reader knows, and it is the line they would have scrolled back
  to look at.

  Driven by the scroll position rather than by the caret: the question it
  answers is "what am I looking at", and a header keyed to the caret would sit
  unchanged while the file moved underneath it. What it shows is what *encloses*
  the top of the viewport, not what merely precedes it — scroll into the blank
  lines between two functions and nothing is pinned, because you are not inside
  anything. That distinction needed the outline to record how far each
  declaration extends, which it now does.

  Costs no more per frame than the viewport already did: the enclosing
  declarations are walked out of the parse tree when the tree changes, not when
  the view scrolls, and each pinned row is highlighted as a single line rather
  than by colouring everything from the declaration down to the screen.

  On by default; **Settings > Editor > Sticky declarations** turns it off, or
  `sticky_scopes` in the settings file.

### Fixed

- **Python installed by the Python Install Manager is found.** On a machine
  whose only Python came from the installer python.org now recommends for
  Windows, The Editor found nothing at all: `python` ran perfectly well in a
  terminal, and the Run button, the packages panel and the language servers all
  behaved as though Python were not installed.

  The install manager is an MSIX app, so the `python`, `python3` and `py`
  commands it publishes are App Execution Aliases in `WindowsApps` -- the same
  folder the Microsoft Store puts its decoy `python.exe` in, the one that only
  prints "Python was not found" and offers to open the Store. The Editor refuses
  everything in that folder, and has to: the two are indistinguishable on disk,
  both being zero-byte reparse points identical in size, attributes and link
  target. Telling them apart means running them, and the detection runs on every
  frame the settings window is open.

  So rather than guess which alias is real, detection now looks for the
  interpreter the alias would have dispatched to, which is an ordinary file in
  an ordinary directory: the install manager's global commands in
  `%LocalAppData%\Python\bin`, then the runtimes beside them, newest first. A
  `global_dir` or `install_dir` set in `%AppData%\Python\pymanager.json` is
  honoured, since an administrator can move both.

  The commands directory is preferred over any single runtime because it follows
  the default the user has chosen and keeps following it when they change it,
  which is exactly what `python` means in their terminal. `PATH` still wins over
  all of it, so nothing changes on a machine that was already working.

  The same directories were added to the interpreter picker and the
  **Create Virtual Environment** dialog, which previously knew about the old
  per-user installer's `AppData\Local\Programs\Python` but not this one.

## [1.0.0] - 2026-08-25

### Added

- **Python docstrings are written from the signature.** Typing `"""` on the
  first line of a `def` or `class` body fills in an entry per parameter with
  its annotation and whether it has a default, the return type, whatever the
  body raises, and `Yields` in place of `Returns` for a generator. A class
  documents what building one takes, which is `__init__`'s parameters rather
  than the base classes in its own header; `self` and `cls` are left out
  because nobody passes them. The caret lands on the summary line, which is the
  one part a signature cannot supply, and one `Ctrl+Z` takes the whole thing
  back.

  Google, NumPy and Sphinx reST layouts, chosen in **Settings > Python**
  alongside **Off**. Read from the text rather than from the parse tree, for
  the same reason the `self` rule is: at the moment the third quote is typed
  the string is unterminated and the file does not parse.

- **Typing a triple quote no longer fights back.** With quotes auto-closing,
  three keystrokes used to leave four quotes and a caret in the middle of them,
  and the closing three had to be fought for. The third quote of a triple now
  opens and closes the string in one go, leaving the caret between the two —
  and typing the closing three by hand still steps over the ones already there.

- **The pointer changes over the gutter.** A hand over the breakpoint strip and
  over a fold chevron, an arrow over the line numbers and the blame column, and
  the I-beam over the code. The chevron column is mostly empty, so the hand
  appears only beside lines that have one: a hand promising a click that does
  nothing is worse than no hand at all. The pointer and the click handler now
  read one description of the gutter's geometry rather than two.

- **Clicking below the last line puts the caret at the end of it.** The blank
  space under a short file is part of the editor now, the way it is in every
  other editor. It was not before: the text area was allocated at exactly the
  height of its text, so a click underneath landed on the scroll area's
  background instead. Nothing happened at all — the caret did not move and the
  editor did not even take focus, so the next thing typed went nowhere.

  The caret goes to the end of the last line rather than to the column the
  pointer happened to be over, because down there is no column to be over. With
  the tail of the file folded away it stops at the end of the last row that is
  actually on screen, rather than jumping into text that is hidden.


- **`tools\make-release.bat`** builds, tests, and stages a Windows release into
  `dist\` — a folder, a zip, and its SHA-256. The checksum matters more than
  usual because the binary is deliberately unsigned, so a published hash is the
  only way for someone to check they received what was sent.

- **An application icon**, compiled into the executable as a Windows resource
  so Explorer, the taskbar and Alt+Tab show it rather than the generic one, and
  set on the window as well — those are read from different places and setting
  one does not set the other. The exe also carries its product name,
  description and copyright. Drawn at each size it is displayed at rather than
  scaled down from one large image, because the size that matters most is 16 px
  and thin strokes do not survive being resampled to it. `tools/make_icon.py`
  regenerates the assets.

- **A Python debugger.** Breakpoints (F9, or right-click → Toggle Breakpoint),
  single stepping (F10 over, F11 into, Shift+F11 out), Alt+F5 to start and to
  continue, Alt+Shift+F5 to stop. A **Debug** tab in the bottom dock shows the
  call stack and the selected frame's local variables; clicking a frame jumps
  to it. Breakpoints are drawn in the gutter — hollow if the debugger could not
  bind them — follow their lines as the file is edited, and are saved with the
  session. Needs `debugpy`; the editor says so with the command to install it.
  Python only: Rust needs a different adapter.

- **The problem under the caret is highlighted and scrolled to in the Problems
  panel.** Finding which of two hundred entries belongs to the squiggle you are
  looking at was otherwise a manual search.

- **Go to Definition searches the project when no language server can answer.**
  Previously it looked only in the open file, which for a function defined in
  another module means never finding anything. Files of the same language are
  skimmed for the name as plain text before being parsed, so in a real project
  almost none of them are.

- **Line manipulation**: Duplicate Line (Ctrl+Shift+D), Delete Line
  (Ctrl+Shift+K), Move Line Up/Down (Alt+Up/Down). Each acts on the whole block
  of lines the selection touches, keeps that block selected so the shortcut can
  be held down, and is a single undo step.
- **Go to File (Ctrl+P).** Fuzzy-matches the project's files with the same
  matcher as the command palette, biased towards the file name — typing `utils`
  wants `utils.py`, not the four files inside a directory called `utils` — while
  still matching across the whole path, so `mdl/utl` finds `models/utils.py`.
  The walk skips `.git`, `target`, `node_modules`, `__pycache__`, `.venv` and
  the like at any depth, does not follow symlinks, and is bounded by depth and
  by a cap it admits to when it hits.

- **Ctrl+Space forces the completion popup**, below the two-character floor
  that keeps it from appearing unbidden over a single letter.
- **Completion works with no language server**, from the parse tree: every
  distinct name already written in the file, labelled by what defines it and
  marked "in this file" so it cannot pass for a real completion. Not offered
  after a `.` — the members of an object have nothing to do with the names that
  happen to appear elsewhere in the file, and a list of them there would be
  actively misleading.

- **Completion popup.** Suggestions from the language server appear under the
  caret after two characters of a name, or immediately after a `.`. Up/Down to
  choose, Enter or Tab to accept, Escape to dismiss, or click. Accepting is one
  undo step. The list is fetched once per word and narrowed locally as you keep
  typing, rather than re-requested on every keystroke — a server answers several
  frames later, and replacing the list wholesale on each reply makes it flicker
  and reorder under the fingers. A reply that no longer describes the word being
  typed is dropped.

- **Go to Definition (F12) and Find Uses (Shift+F12)**, from the Edit menu or
  the editor's new right-click menu, with **Next/Previous Use (F8, Shift+F8)**
  to walk the results. Uses the language server where one can answer, so it
  works across files; falls back to a parse-tree search of the open file when
  none is running, and says so, because a list that looks complete but only
  covers one file is worse than an honest partial one. The fallback works off
  the tree rather than a text search, so a name inside a string or a comment is
  not counted as a use.
- **A right-click menu in the editor**: Go to Definition, Find Uses, Cut, Copy,
  Paste. Right-clicking moves the caret to the word under the pointer first,
  since otherwise the menu acts on wherever the caret happened to be.

- **Built-in syntax checking, with nothing installed.** Every language server is
  optional, but until now that meant a machine without one showed no diagnostics
  at all — a Python file containing `if bob = kate` looked perfectly healthy.
  The tree-sitter grammar that highlights a file is already parsing it on every
  keystroke and its error recovery marks exactly where it stopped making sense,
  so that is now reported: squiggle, gutter glyph, Problems panel entry and
  status-bar count, for every language with a grammar. It runs 400 ms after
  typing stops, so a half-written line is not flagged while it is being written.
  This finds what is not the language; undefined names, wrong arguments and type
  errors still need a language server.
- **Help → Check Toolchains**, which the plan has referred to since M0 and which
  did not exist. Lists every optional tool, whether it was found, where, what it
  would provide, and the one command that installs it. Reachable from the
  Problems panel too, which is where the question comes up.

### Changed

- **The release script runs the performance budgets, and stopped announcing
  ten empty test runs.** `tools\make-release.bat` ran `cargo test --workspace`,
  which does not build bench targets — so the PLAN.md §2.4 budgets were being
  shipped unmeasured — and did run the ten per-crate doc-test targets, which
  have nothing to run because every fenced block in the doc comments is
  `text` rather than a compiled example. Under `--quiet` those arrived as
  unlabelled "running 0 tests" blocks. It now runs `--all-targets`, which is
  the other way round on both counts, and builds and tests `--locked` so a
  release is built from the dependency versions in `Cargo.lock`.

- **The C runtime is linked statically on Windows.** The executable previously
  imported `VCRUNTIME140.dll` and would not start on a machine that had never
  had a Visual Studio redistributable installed — for something distributed as
  a portable zip, that is the difference between "copy one file" and "copy one
  file, then find and run an installer from Microsoft".

- **Tools → Open settings.toml is gone.** The Settings window replaced it, and
  its Advanced page still opens the file for anyone who wants it. The "Not yet
  implemented" note has gone from that page too.

- **The status bar always shows the problem count**, including `✓ No problems`.
  A blank space where a count should be reads as "nothing is wrong", which looks
  identical to "nothing is checking". Its hover now names what is actually
  checking the file, and says so explicitly when only the built-in syntax check
  is running.

- **The Problems panel says when a file cannot be checked any further.** A file
  that does not parse stops every linter dead — Ruff reports the syntax error
  and nothing else, and so does Pyright — so a missing import that goes
  unreported below a syntax error looks like the linter failing rather than
  waiting. The panel now says so under the file's name.

### Fixed

- **The window came back the size of the screen and could not be got out of
  it.** Quitting maximized wrote the *maximized* size down as the window's own
  size, so the next launch restored a window the size of the screen and
  "restore down" had nothing smaller to go back to — it un-maximized to the
  same near-fullscreen rectangle, hanging a few pixels over every edge, and
  once it was closed in that state it started that way for good. The size and
  position now come from the last frame the window was *not* maximized, which
  is what restore-down is asking for, and the maximized flag rides on top of
  them: quit maximized, and it opens maximized over the window you had, with
  that window still underneath it. A remembered size that fills the monitor is
  read as one of the old bad ones and dropped, so an existing session corrects
  itself on the first launch rather than the first resize.

- **Find said "0 of 3" while showing three matches.** The count and the strong
  highlight both come from the bar's current match, and nothing set one until
  somebody stepped — so a search that had just found matches sat on none of
  them, and reported a position no match has. The match list is only rebuilt
  when the query, options, or document change, and rebuilding it now anchors to
  the caret when there is no current match to keep: the first result is
  selected as soon as it is found, and `Ctrl+F` on a selected word lands on
  that word rather than the one after it, because the bar is handed the start
  of the selection rather than its far end. Typing another character refines
  onto the same occurrence for the same reason — it used to re-anchor past the
  match it had just revealed, so extending a query walked down the file a
  match at a time.

- **Discarded changes came back after a restart.** Choosing "Don't Save" on the
  way out and starting up again offered the same changes back, as unsaved work
  rescued from a crash. Quitting clears this session's recovery copies, and has
  to: they exist to survive a crash, and a window that closed on purpose did
  not have one. But the clearing lived in the branch that quits when nothing is
  unsaved, and the branch that quits *after* the user declines to save set the
  "quitting" flag by hand — which is the same flag that stops the first branch
  running a second time. So the one route where the copies certainly had to go
  was the one route that kept them. Confirming a quit now clears the store
  wherever it is confirmed from.

- **Session directories piled up in the backups folder.** A clean exit deletes
  this session's recovery directory. On Windows it silently did not: the
  session is still holding an open handle on the `alive` file *inside* that
  directory — the lock by which another instance can tell a crash from a second
  window — and Windows will not delete a directory that something has open. The
  failure was discarded, and nothing looked wrong, because a directory with no
  recovery files in it is skipped when the next start goes looking for work to
  recover. The lock is now released before the removal, and the directories
  already accumulated are swept up on the next start — but only once they are
  old enough to be certain about, so that a window still in the act of opening
  is never mistaken for debris and deleted out from under itself.

- **Restoring recovered work opened the file twice.** After a crash, the
  session restore reopens the tabs that were open at the time — including,
  usually, the very file the recovery prompt is about, read back from disk
  without the changes. Accepting the recovery then added a *second* tab for it,
  so the same file sat open twice under the same name with different text on
  each, and which one you got depended on which tab you clicked. The recovered
  buffer now takes over the tab already showing that file. A buffer that was
  never saved still gets a tab of its own: it has no file to collide over, and
  is not in the session file either.

- **Zoom drifted until it stuck at the smallest size.** Zooming in and out for
  a while ended with the interface at its minimum, after which zooming in
  worked and zooming out did nothing whatever.

  Two owners of one number. `Ctrl+Plus` is `Ctrl+Shift+=` on the usual
  keyboards, and `Ctrl+Shift+=` is Fold/Unfold All — so the View menu's Zoom In
  was listed under a key that unfolds the file, and the `Ctrl+=` people press
  instead was picked up by egui's own zoom handler, which moves its zoom factor
  and tells the application nothing. Zoom *out* was the application's, and
  moved a saved setting. So the display crept up on every zoom in and was
  yanked back down to the setting on every zoom out, while the setting itself
  marched down to the bottom of its range and clamped there — at which point
  zooming out changed nothing, so nothing was pushed to the display, while
  zooming in still worked because that had been egui all along.

  Zoom In is now bound to `Ctrl+=`, which is both what people press and what
  the menu shows; `Ctrl+Plus` remains as an unlisted second binding for
  keyboards that have a `+` of their own. egui's built-in zoom shortcuts are
  switched off, the setting is the single authority on the zoom factor and is
  re-asserted if anything else moves it, and each step is rounded back onto a
  tenth so that zooming out and back in returns to exactly 1.0 rather than to
  something that merely looks like it.

- **Backspacing a selection that spanned lines closed the application.** The
  row map is built at the top of a frame; the keystroke is handled later in
  that same frame and shortens the document immediately. In a file short enough
  for its last line to be on screen, the paint loop then walked to a row the
  document no longer had and asked the rope for the byte offset of a line past
  its end. Every accessor on the document itself clamps, but that one call went
  straight to the rope, so it panicked — and a panic in the paint pass takes the
  process with it. The painter now stops at the end of the document rather than
  at the end of the map.

- **Fold arrows appeared beside blank lines and single statements.** Three
  causes, each of them a fold read off a description of the file that was not
  the file.

  **Document versions are now unique across documents, not per document.**
  Everything derived from a buffer is cached against `Document::version` — the
  fold ranges, the search results, the outline, the copy a language server has
  been sent. Reloading a file after another program changed it builds a new
  `Document` and puts it behind the view that holds all of those caches, and
  each document used to start counting at zero: the replacement's first version
  was one the view had already seen, so nothing was recomputed. The chevrons
  stayed on the lines the *old* text had put them on, which in a file that had
  gained a line was beside blank ones.

  **Nodes inside a parse error are no longer offered as folds.** One unclosed
  bracket puts every statement after it inside a single ERROR node spanning
  hundreds of lines, and the multi-line nodes tree-sitter invents while
  recovering put a chevron beside plainly one-line statements.

  **And the fold list is rebuilt when a reparse that ran out of time catches
  up.** Catching up does not change the document, so a rebuild keyed on the
  document version alone left the folds taken off the half-finished tree in
  place until the next keystroke.

- **A second drag extended the first selection instead of replacing it.**
  Dragging over one stretch of text and then over another left everything
  between them highlighted, because a new drag never planted an anchor of its
  own: the press changes nothing on its own, so by the time the pointer had
  moved far enough to count as a drag, the previous selection's anchor was
  still in place and got extended. A drag now anchors where the button went
  down — and where it actually went down, not where the pointer had drifted to
  by the frame the drag was recognised. Shift+drag still extends, which is what
  it is for. Alt+drag column selections had the same fault and are fixed with
  it.
- **The console never said when debugging had ended.** A debug session shared
  the console with the runner but announced nothing, so a program that had run
  to completion looked exactly like one still paused. The session now echoes
  the command it is debugging, and prints `[Debugging finished]` or
  `[Debugging stopped]` when it ends, as a run does.
- **debugpy's own telemetry was printed as program output.** The adapter
  reports its name and version through the same event, with no trailing
  newline, so it ran into the first real line: `ptvsddebugpystart`. Only
  `stdout`, `stderr` and console output is shown now.
- **The toolbar shows whether anything is running.** Run greys out with a
  spinner beside it, and Stop turns red and becomes clickable — and stops the
  debugger when that is what is live, rather than doing nothing.
- **Two toolbar buttons could look identical.** With no icon font available,
  Run and Redo both fell back to `>` and Open and Find both to a circle. Every
  button now has a distinct last-resort label, with a test that says so.

- **Breakpoints were invisible.** The gutter had no column reserved for them,
  so the marker was painted underneath the line numbers and setting one looked
  like it had done nothing. Breakpoints now have a strip of their own at the
  far left, clicking in it toggles one, and setting a breakpoint with no
  `debugpy` installed says so once rather than leaving a dot that can never be
  hit.

- **Redo ran Undo**, Save As ran Save, Shift+F12 ran Go to Definition, and
  every other Shift-plus-something binding fired its unshifted twin. egui's
  `consume_shortcut` matches modifiers *logically*, so an extra Shift is
  ignored and Ctrl+Shift+Z satisfies a Ctrl+Z binding — whichever is checked
  first wins, and in registry order that was always the less specific one.
  Shortcuts are now tried most-specific first. This is also why Shift+F12 never
  appeared to work when Find Uses was added; that was not Windows reserving
  F12, as recorded at the time.
- **Log files are named `the-editor_2026-08-08.log`** rather than
  `the-editor.log.2026-08-08`, which had a dot in the middle and no extension
  at the end, so Windows asked which application to open it with every time.

- **Language servers are now told how to behave.** The Editor never answered
  `workspace/configuration`, so every server fell back on its own defaults —
  and basedpyright's default is its strictest mode, which reports import cycles
  and treats a great deal as an error. On a project using libraries whose stubs
  do not describe them fully, that is hundreds of findings that are true of the
  stubs and false of the code. Servers are now asked for `standard` type
  checking, `openFilesOnly`, and no import-cycle reporting.
- **Individual servers can be switched off** in Settings → Python, or via
  `[lsp] disabled` in the settings file. A checker whose findings you do not
  trust is worth less than none.
- **The Problems panel shows only the file you are looking at** by default,
  with an "All open files" toggle. Every tab's problems in one list buries the
  ones belonging to the line under the caret.
- **Settings hints wrap** instead of running off the edge of the panel.
- **Scrolling: another attempt at the jump at the end.** Watching the scroll
  offset was not enough on its own — during an ease, two consecutive frames can
  match, at which point the frames stopped and the remainder waited for
  something else to wake the loop. Frames now continue for a short window
  measured from the last actual movement.

- **The completion popup blocked every keyboard shortcut in the application.**
  It was listed as modal, so while a suggestion was on screen — which, while
  typing, is most of the time — Ctrl+F, Ctrl+S and F5 all did nothing. It
  already claims the five keys it actually needs, and now claims only those.
- **The word-list fallback is no longer offered unbidden**, only on Ctrl+Space.
  A list of names that happen to appear elsewhere in the file is a fair answer
  to "suggest something" and a poor reason to cover the text every time two
  letters are typed.
- **Two-finger scrolling still stalled and then lurched.** The previous attempt
  kept frames coming for a fixed 250 ms after the last wheel event, but egui's
  easing outlives any fixed window: the animation ran out of frames part-way
  and the remainder was applied in one jump when something else woke the loop.
  Frames are now requested while the scroll offset is still changing, which is
  the only honest signal that there is more to come.

### Changed

- **Only errors are underlined in the text by default.** A type checker that
  cannot resolve a project's imports reports most of its lines, and a file
  underlined end to end cannot be read, let alone edited. Warnings still appear
  in the gutter and the Problems panel, so nothing is hidden. Settings → Editor
  → *Underline in the text* offers all, errors only, or nothing.

- **Jumping to a search match or a definition moved the caret but not the
  view.** The scroll target was taken from the painted caret rectangle, which
  only exists when the caret is *already* on screen — so the one case that
  needed scrolling was the one case that could not ask for it. It is now derived
  from the line number, whether or not the line is visible.
- **A qualified call was reported as a definition.** Rust's `scoped_identifier`
  has a `name` field, so `word::next_boundary(x)` looked exactly like a
  declaration of `next_boundary`, and a search returned every call site
  alongside the real one. Python's `attribute` was the same trap. A name now
  counts as introduced only when its parent is a declaration node.
- **Two-finger scrolling arrived in lurches.** egui eases a scroll in over
  several frames, and nothing else was waking the frame loop between trackpad
  events — so the view only advanced when the caret-blink timer fired, every
  120 ms. Frames are now requested while a scroll is animating, and only then.
- **Buttons showed empty boxes instead of icons.** The bundled fonts do not
  cover everything, and which characters they miss is not guessable: `▶` and
  `■` render, `↑` and `↓` do not. Every glyph in the interface is now chosen
  through a helper that asks the font what it has and falls back, ending at
  plain ASCII, so a missing glyph degrades instead of vanishing.

- **Pyright was rejected as unusable and never started.** The check that a
  binary really works runs its version flag, and
  `basedpyright-langserver --version` does not print a version — it fails with
  "Connection input stream is not set", because it only ever expects to be
  handed a transport. It is now taken on trust; the rustup-shim problem that
  check exists for does not apply to it, and a binary that cannot speak LSP is
  caught by the handshake anyway.
- **Definition and reference requests went to whichever server got there
  first**, which for Python is Ruff — a linter that cannot answer either. Each
  server's advertised capabilities are now recorded from the handshake, and
  only a server claiming `definitionProvider` or `referencesProvider` is asked.
- **Find Uses results were walked in the server's order, not the file's.**
  Pyright does not answer in document order, so F8 jumped about instead of
  reading downwards. Results are sorted and de-duplicated, and the walk starts
  from the result the caret is already on rather than from the top.

- **A console window appeared behind the editor and stayed there.** A release
  build is a GUI application with no console of its own, so Windows allocated
  one for each language server it started; a long-lived server meant a black
  window for the whole session. Every background spawn — language servers,
  version probes, interpreter detection, `taskkill` — now goes through
  `editor_proc::spawn::quiet`, which sets `CREATE_NO_WINDOW`. Reproduced and
  confirmed fixed for both `ruff` and `basedpyright-langserver`. The run console
  is unaffected: it uses a pseudo-terminal precisely so its output *is* visible,
  inside the editor.
- **An egui ID clash painted a warning banner over the tab bar.** The tab bar
  and the editor view are laid out one after another in the same panel, and both
  took egui's automatically generated scroll-area id — which is derived from how
  many widgets the parent has already created, so they collided. Every
  scroll area that shares a parent with another now carries an explicit salt.

- **`[Finished]` appeared in the middle of a program's output.** The reader
  thread and the process waiter both send to one channel and nothing ordered
  them, so a program that printed several lines and exited immediately could
  have its exit announced while output it had already produced was still
  sitting in the terminal buffer. The exit is now announced only once the
  reader has seen end of stream — with a two-second grace period as a fallback,
  for the case where a grandchild process keeps the terminal open after its
  parent has gone. A session reports itself as still running until the exit
  event is on the channel, so a caller that stops draining when the run ends
  cannot miss it.
- **A banner injected into the console left a blank line behind it.** The
  `[Finished]` and `[Exited with code N]` lines finished the current line even
  when there was nothing on it, producing a stray gap before the banner.
- **Comment text was corrupted in seven source files** by an editing tool that
  re-encoded UTF-8 as cp1252; em dashes and section signs had turned into
  mojibake. Byte-order marks were stripped from three files at the same time.
- **Arrow keys moved focus out of the editor instead of the caret.** egui's
  focus navigation claims the arrow and Tab keys before a widget sees them
  unless the widget declares an event filter saying it wants them. `TextEdit`
  does this; the custom editor did not, so pressing Up jumped to the toolbar.
  Escape is deliberately still left to egui, so it continues to close the find
  bar and dismiss dialogs.
- **The output panel could grow to fill the entire window**, hiding the editor
  with no way to get it back. In egui 0.36 a panel lays its content out against
  `size_range.max` and then stores whatever size the content settled at — so a
  panel containing a `ScrollArea` that fills its space grows to the maximum on
  the second frame and stays there. The dock now has a fixed default height it
  owns itself, with its own drag strip along the top edge to make it taller,
  and a clamp that always leaves the editor a usable strip whatever the window
  size or drag distance.

### Added

- **M6 (in progress) — Language servers.** Diagnostics from `rust-analyzer`,
  `ruff`, `pyright`/`basedpyright`, `pylsp` and `taplo`, whichever are
  installed.
  - Squiggles under the offending text, a gutter glyph per line, a **Problems**
    panel grouped by file (Ctrl+Shift+M), and error/warning counts in the
    status bar. Severity is shown by glyph as well as colour.
  - **Several servers per language.** Python is served by `ruff` for linting
    alongside a type checker, and each publishes its own complete set for a
    file — so diagnostics are stored per source and merged, or each server
    would erase the other's findings.
  - A crashed server restarts with backoff and gives up after three attempts;
    a server that ran healthily for a minute before dying has its counter
    reset, so a fault an hour in is not treated as the fourth failure of a
    broken server. Its diagnostics are cleared when it dies, since it is no
    longer running to correct them, and every open document is re-sent when it
    comes back.
  - Servers in a project's virtual environment are preferred over globally
    installed ones, so a project pinning `ruff` is linted by that version.
  - **With no server installed the editor is unaffected**: opening a file of a
    language with no server allocates nothing, and an absent server costs one
    filesystem check rather than one per keystroke. The Problems panel says
    which servers would help and what they provide, rather than showing an
    empty list that looks like "no problems".
  - Everything stops on exit, including via `Drop` — a language server left
    running holds a workspace index and a few hundred megabytes.

### Fixed

- **A binary on `PATH` was assumed to work.** `~/.cargo/bin/rust-analyzer`
  exists whenever `rustup` is installed even when the component is not; running
  it exits 1 with "Unknown binary in official toolchain". Speaking LSP to it
  produced an immediate end of stream indistinguishable from a server crashing
  on startup. Discovery now runs each candidate's version flag before offering
  it — the same class of trap as the Microsoft Store Python stub.

- **Session restore.** The open folder, the open files with their caret
  positions, the active tab, the output panel and the window geometry all come
  back on the next launch. Controlled by `[ui] restore_session`.
  - Files and folders that have since been deleted are dropped rather than
    reopening as errors, and an active-tab index past the end is discarded.
  - A remembered window position is checked twice: once for obvious garbage
    when the file is read, and again against the monitors actually attached
    once the window exists. A window remembered on a monitor that has since
    been unplugged keeps its size and lets the window manager place it, rather
    than opening where it cannot be seen.
  - A corrupt session file is discarded silently — losing a session is a mild
    annoyance, a dialog about it on every launch is worse.
- **Filesystem watching.** The explorer now notices files created, renamed or
  deleted outside The Editor. Events are debounced, and the editor's own
  atomic-save temporaries and build directories are ignored so the tree does
  not flicker on every save.
  - An open document whose file changes on disk is **reloaded silently when it
    has no unsaved edits**, and warned about when it does — saving over a file
    that `git checkout` has just rewritten is how people lose work.
- **File tree context menu**: New File, New Folder, Rename, Delete, Copy Path,
  Copy Relative Path, Reveal in Explorer, and Refresh.
  - Renaming happens in place, and an open tab follows its file — otherwise the
    next save writes to the old name and resurrects it.
  - Delete moves to the **recycle bin**, never an unrecoverable delete, and
    closes any tab showing the deleted file or anything inside a deleted folder.
  - New Folder creates and drops straight into an in-place rename.

- **Create Virtual Environment** (Run → Create Virtual Environment…). Pick a
  base interpreter, a folder name, and three checkboxes; the commands run in
  the output panel where you can watch them.
  - The base-interpreter list looks beyond `PATH` — the Windows `py` launcher,
    the usual install directories, pyenv versions — and **verifies each
    candidate by running it**, which filters out Microsoft Store stubs and
    entries left behind by uninstalls. Newest version first.
  - Optionally upgrades pip and installs from `requirements.txt` when one
    exists. Those steps use the **new environment's** Python, not the base
    interpreter — running `pip install` with the base is the exact mistake a
    virtual environment exists to prevent.
  - On success it can adopt the environment as the project's interpreter and
    add it to `.gitignore` (without duplicating an entry already there, and
    without gluing itself onto a file with no trailing newline).
  - Existing non-empty targets, invalid folder names and paths-as-names are
    refused before anything runs.
  - Success is confirmed against the filesystem, not the exit code alone:
    `python -m venv` can report success and still leave nothing usable behind.
- The runner can run a **sequence** of commands, stopping at the first failure
  — installing requirements into an environment that failed to be created only
  produces a second, more confusing error. Completion is reported once, at the
  end of the sequence.

- **M7 (in progress) — Running code.** F5 to run, Shift+F5 to stop, Ctrl+F5 to
  restart, Ctrl+J for the output panel.
  - **Python** runs as `<interpreter> <file>` from the project root — no
    wrapper, no generated launcher. The interpreter is the one configured in
    settings, else a `.venv`/`venv`/`env` in the project, else `python` on
    `PATH`. `PYTHONUNBUFFERED` is set so `print` output appears as it happens
    rather than in a lump at exit.
  - **Rust** runs `cargo run` from the nearest `Cargo.toml`, so a workspace
    member runs from its own manifest. Run Tests runs `cargo test`.
  - The exact command and working directory are echoed before anything starts,
    so there is never any doubt about what ran or with which interpreter.
  - Output goes through a **pseudo-terminal**, so `input()` prompts work,
    colour survives, and progress bars that redraw with `\r` behave. There is
    an input box for answering prompts.
  - **File references in the output are clickable** — rustc's `--> src/x.rs:1:2`,
    Python's `File "x.py", line 3`, and the generic `path:line:col` — and
    resolve against the directory the program ran in. Diagnostic prefixes,
    timestamps and URLs are deliberately not matched.
  - Stopping kills the whole process tree, so `cargo run`'s compiled binary
    does not survive stopping cargo.
  - Scrollback is capped at 50,000 lines; the console follows new output unless
    you scroll up to read something.
  - The status bar shows what is running and takes you back to its output.

### Fixed

- **Python discovery picked the Microsoft Store stub on Windows.** `python3.exe`
  on `PATH` is normally an App Execution Alias that prints "Python was not
  found; run without arguments to install from the Microsoft Store" and exits
  9009 — even with a working Python installed as `python`. Trying `python3`
  first meant Run produced no output at all. Candidate order is now
  platform-specific and `WindowsApps` stubs are rejected outright.
- **Windows ConPTY waits for a Device Status Report before letting the child
  proceed.** A console that never answered looked exactly like a program that
  produced no output and never exited — every process test timed out at 20
  seconds. The reader thread now replies, as a terminal is supposed to.

- **M5 (in progress) — Find and replace within a file.** Ctrl+F to find,
  Ctrl+H to replace, F3 and Shift+F3 to step through results.
  - Match case, whole word, and regular expression toggles. Literal searches
    escape the pattern, so searching for `a.c` does not match `abc`, and a
    literal replacement of `$5` inserts `$5` rather than expanding a capture
    group. In regex mode `$1` and `${name}` do expand.
  - Every match is highlighted, with the current one outlined so it stays
    visible even under a selection. `n of m` counts the results; an invalid
    regular expression reports its error in the bar instead of clearing to
    "no results".
  - Stepping wraps in both directions, and the first step goes to the next
    match *after the caret* rather than jumping back to the top of the file.
  - Replace All is a single transaction and therefore a single undo step.
  - Results are cached against the query and the document version, so typing
    does not re-search a large file on every repaint.
  - Empty matches are skipped: `a*` matches at every position, which would be
    useless to step through and destructive to Replace All.
  - The query is per-document, so switching tabs keeps each one's search.

- **M4 (in progress) — Language-aware editing.**
  - **Python indentation**, the case that has to be right, because getting a
    newline wrong there changes what the program means:
    - a line ending in `:` opens a block, so the next line indents;
    - continuation lines inside an unclosed bracket align to the column after
      the opener, or take a hanging indent of one level when nothing follows the
      opener on its line — both as PEP 8 asks;
    - `return`, `pass`, `raise`, `break` and `continue` dedent the next line;
    - typing `else`, `elif`, `except`, `finally` or `case` re-aligns the line to
      the block it continues, as soon as the word is complete.
    Brackets and colons inside strings and comments are ignored, escaped quotes
    do not end a string early, and the backwards scan is bounded so Enter costs
    the same in a long file as a short one.
  - Rust, JavaScript, CSS, JSON and HTML indent after their openers, and a
    closing brace typed on its own line re-aligns to match its opener.
  - **Auto-closing brackets and quotes**, with the behaviours that stop them
    being a nuisance: typing the closer steps over an auto-inserted one instead
    of doubling it; typing an opener with text selected surrounds the selection
    rather than replacing it; nothing auto-closes in front of a word; and an
    apostrophe after a word character stays an apostrophe, so `don't` types
    normally. Enter between a pair opens the block out. Switchable off with
    `[editor] auto_close_brackets`.
  - **Toggle Comment** (Ctrl+/), using the right token per language, aligning
    markers to the shallowest line in the block so it keeps its shape, and
    restoring the original exactly when toggled back. A partly commented block
    comments rather than uncomments.
  - **Block indent and outdent** — Tab and Shift+Tab with a multi-line
    selection, or Ctrl+] and Ctrl+[ from the Edit menu. The selection survives,
    so Tab can be pressed repeatedly; blank lines are not indented into trailing
    whitespace; outdenting stops at the margin.

- **M3 — Syntax highlighting.** Tree-sitter grammars compiled in for Python,
  Rust, JSON, JavaScript, HTML, CSS, TOML and Markdown; INI gets a small
  hand-written line highlighter, since the format is strictly line-oriented and
  a parser would buy nothing.
  - Highlighting is computed for the visible rows only, so a 50,000-line file
    costs the same per frame as a 50-line one.
  - Edits reparse incrementally. Multi-edit transactions (which arrive with
    multi-cursor and project-wide replace) fall back to a full reparse rather
    than risk a subtly wrong incremental update; a test asserts an incremental
    reparse matches a clean one.
  - Syntax themes for dark and light, following the UI theme. Every colour is
    checked against the code-pane background for WCAG AA contrast by a unit
    test, and the keyword/string/comment/function set is asserted to be
    mutually distinguishable.
  - Half-typed, syntactically invalid code still highlights what it can rather
    than going blank — which is the normal state of a file being edited.
  - Files past the large-file threshold stay unhighlighted, as does plain text.
- Documents now carry a change outbox, drained once per frame to drive the
  incremental parser. The language server will take the same route in M6, so
  no future edit path has to remember to notify either of them.

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

[Unreleased]: https://github.com/finchyfinch/The-Editor/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/finchyfinch/The-Editor/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/finchyfinch/The-Editor/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/finchyfinch/The-Editor/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/finchyfinch/The-Editor/releases/tag/v1.0.0
