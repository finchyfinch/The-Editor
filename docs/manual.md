# The Editor — user manual

An IDE for Python and Rust. One executable, no installer, no account, nothing
phoning home.

This manual is shown inside the application under **Help > User Manual**, and
lives in `docs/manual.md` if you would rather read it in a browser.

---

## Getting started

**Open a folder** (`Ctrl+Shift+O`) rather than a single file wherever you can.
The folder becomes the project: it is what the explorer shows, what
project-wide search searches, where the terminal starts, and where The Editor
looks for a virtual environment.

**Open a file** with `Ctrl+O`, from the explorer, or from the command line:

```
the-editor main.py
the-editor .
```

A folder on the command line is opened as the project; files are opened in
tabs. `the-editor --help` prints the options.

**Find a file** with `Ctrl+P` and type any part of its name. The match is
fuzzy, so `mpy` finds `main.py`.

**Find a command** with `Ctrl+Shift+P` if you know what you want but not which
menu it is under. Every command in the application is there, with its shortcut.

### The first thing to check

**Tools > Check Toolchains** looks for Python, Rust, and the language servers,
and tells you what it found and how to install what it did not. Nothing about
The Editor breaks without them — you simply lose the feature that needs them.

---

## Editing

The basics behave the way you expect: arrow keys, `Home`/`End`, `Ctrl+Home`/
`Ctrl+End`, shift to select, `Ctrl` with the arrows to move a word at a time,
`Ctrl+Z` and `Ctrl+Shift+Z`.

A few that are worth knowing about:

| | |
|---|---|
| `Ctrl+D` | Select the word under the caret; press again for the next occurrence, each with its own caret |
| `Ctrl+Alt+Up` / `Ctrl+Alt+Down` | Add a caret on the line above or below |
| `Alt+click` | Add a caret there — or take it away if one is already there |
| `Alt+drag` | Select a rectangle, one caret per line |
| `Esc` | Back to one caret |
| `Ctrl+/` | Comment or uncomment the selected lines |
| `Ctrl+Shift+D` | Duplicate the line |
| `Ctrl+Shift+K` | Delete the line |
| `Alt+Up` / `Alt+Down` | Move the line up or down |
| `Ctrl+R` | Go to a symbol in this file |
| `Ctrl+Shift+[` | Fold or unfold the block around the caret |
| `Ctrl+Shift+G` | Show this file's changes since the last commit |
| `Ctrl+G` | Open the Source Control panel |
| `Ctrl+Shift+B` | Show who last touched each line |
| `Ctrl+F` | Find, in this file |
| `Ctrl+Shift+F` | Find across the project |
| `F12` | Go to the definition |
| `Shift+F12` | Find every use; `F8` and `Shift+F8` step through them |
| `F2` | Rename a symbol everywhere in the project |

**Multiple carets** are one edit, not several: typing a word at eight carets is
one `Ctrl+Z`, not eight. Undo drops the extra carets, because the history has
one caret position per step and stale carets would put the next keystroke
somewhere unrelated.

**Python method parameters.** Typing the `(` of a `def` inside a class fills
in `self` and leaves the caret ready for the next argument. `@classmethod`
gets `cls`; `@staticmethod` gets neither, and so does a plain function.

**Rename** (`F2`) asks the language server, so it renames the *symbol* rather
than the text — a variable called `id` will not take the `id` out of
`identity`. It needs the file saved first, because the server reads from disk.
There is no find-and-replace fallback: that is how the wrong things get
renamed.

**Folding.** A chevron appears beside every line that opens something
foldable — a class, a function, an `if`. Click it, or press `Ctrl+Shift+[` to
fold whatever the caret is inside. **View > Fold All** and **Unfold All** do
the whole file. Folds follow their code as you edit above them, and a fold
whose code is deleted goes with it.

### What you have changed

If the project is a git repository, a coloured bar appears in the far-left
margin beside every line that differs from the last commit: green for a new
line, blue for a changed one, and a short red mark at the join where lines were
deleted. The status bar names the branch you are on.

`Ctrl+Shift+G` shows the whole file against the committed version as a diff,
with the line numbers from both sides so you can find anything you see in it.

The comparison is against the last commit, not against the file on disk — so
saving does not clear the marks, and committing does.

### Staging

**Ctrl+G** opens the **Source Control** panel, which lists every file that
differs from the last commit in three groups: conflicts first, then what is
staged, then what is not. New files git has never seen sit with the unstaged
ones, because staging them is the same decision.

Each row has **Stage** or **Unstage**, and the buttons above do the whole list
at once. Click a file's name to open it; double-click to see its changes.

**Discard** throws the changes away, and asks first. It has to: unlike deleting
a file, which goes to the recycle bin, discarded changes are not anywhere —
not in the undo history, not in the recycle bin, and not in git. Untracked
files are never touched by it.

### Committing

Write a message in the box at the top of the panel and press **Commit**. The
button says how many files are going in, and when it is greyed out the hover
says why — nothing staged, no message, or a conflict still to resolve.

**Amend the last commit** rewrites the commit you are on instead of adding one,
and starts from its message so a considered one is not replaced by a hurried
one. It is the right tool for a typo in a message or a file you meant to
include, and the wrong one once the commit has been pushed anywhere.

Your `git` does the work, so your hooks run and your signing key signs. If a
hook refuses the commit, what it printed appears in the panel and **the message
is left alone** — fix the problem and press Commit again.

### History and blame

**History…** opens the log: commits on the left, and the selected one's message
and files on the right. Click a file to open it. **Load more** reads further
back; the window starts with the most recent hundred.

**Ctrl+Shift+B** annotates every line of the open file with who last touched it
and when. The annotations are read from the *saved* file, so unsaved edits shift
them until you save.

### When a file changes underneath you

If something else rewrites a file you have open — `git checkout`, a formatter,
another editor — The Editor notices. A file with nothing unsaved is re-read
silently. One with unsaved changes puts a bar above the editor offering
**Reload** or **Keep mine**; nothing is decided for you, because saving over
somebody else's work is not recoverable.

A file that is *deleted* while open keeps its tab and offers **Save it back**.
At that point the tab is the only copy of that text in existence.

### If it crashes

Unsaved buffers are copied aside a couple of seconds after you stop typing, and
the copy is deleted the moment you save. If The Editor is killed — a crash, a
power cut, `taskkill` — the next start offers the work back.

---

## Running and debugging

**F5** runs the current file. Python files run under the project's interpreter;
Rust runs `cargo run`. Output appears in the **Output** panel, which is a real
terminal: colours work, and programs that ask whether they are on a terminal
get the right answer. The toolbar's Run button shows whether something is
still running.

**Ctrl+`** opens a shell in the project folder, with the virtual environment's
directory already on `PATH` — so `python` and `pip` are the project's from the
first command, without activating anything.

It is a real terminal, not a log of output: full-screen programs work, so
`claude`, `vim`, `htop`, `git rebase -i` and `pytest --pdb` all run in it.
Every key reaches the program, including the arrows, Tab, Escape and Ctrl+C —
which means Ctrl+C interrupts rather than copying, as it does in every
terminal. Scroll up for history; it sticks to the bottom while output arrives.

The terminal takes its size from the panel, so drag the dock taller if a
program needs more room.

### Debugging Python

Set a breakpoint by clicking the left-hand gutter, or with `F9`. Press **F5**
and execution stops there, with the **Debug** panel showing the call stack and
the variables in scope. `F10` steps over, `F11` steps in, `Shift+F11` steps
out, and `F5` continues.

This needs `debugpy`:

```
pip install debugpy
```

Without it, The Editor says so rather than failing quietly.

---

## Language support

The Editor highlights nine languages from their grammars, and reports syntax
errors from the same parse — so broken code is visible with nothing installed.

For anything more (completion, go to definition, rename, type errors) it talks
to a language server:

| Language | Server | Install |
|---|---|---|
| Python | basedpyright | `pip install basedpyright` |
| Python | Ruff | `pip install ruff` |
| Rust | rust-analyzer | `rustup component add rust-analyzer` |

Each can be switched off individually in **Settings > Languages** — useful if
one of them is noisier than it is helpful on a particular project.

**Completion** appears as you type, and on `Ctrl+Space`. With no server running
it falls back to the words already in the file, which is less clever and still
better than nothing.

### Packages

**Tools > Packages** lists what is installed in the project's environment, with
its version and — once pip has finished asking — whichever have a newer
release. Install by name, upgrade or remove a package, and freeze everything to
`requirements.txt`. Every change runs in the Output panel so you see pip work
and read its own errors rather than a summary of them.

The update check talks to the network and can take twenty seconds on a large
environment. The installed list appears immediately and the newer versions fill
in when they arrive.

### Virtual environments

A `.venv` or `venv` folder in the project is found and used automatically. Use
**Tools > Select Interpreter** to choose a different one. The status bar shows
which is in use.

---

## Settings

**Tools > Settings** covers appearance, the editor, languages and behaviour.
Everything is written to a commented `settings.toml`; the file is the record,
and hand-editing it is supported — unknown keys and your own comments survive.

A project's `.editorconfig` overrides the indentation and save settings for the
files it covers, which is what you want when the project has a house style.

**Theme** follows the system by default and can be pinned to light or dark.

---

## Keyboard

**Help > Keyboard Shortcuts** lists every binding as the application actually
has it, rather than as a document that has drifted from it.

---

## When something is wrong

**Help > About** has the version, the commit it was built from, and a **Copy
diagnostics** button that gathers the versions, paths and detected tools worth
including in a bug report.

**Help > Open Log Folder** opens the logs. They are plain text, one file per
day, and they record what was started, what failed and why.

A few specific things:

**"No language server can do that here."** The server for that language is not
running. Check **Tools > Check Toolchains**.

**Hundreds of problems in a file that is fine.** A type checker that cannot
resolve the project's imports reports most of its lines. Point The Editor at
the right interpreter (**Tools > Select Interpreter**), or turn that server off
for now in **Settings > Languages**.

**A large file opened read-only.** Files over 5 MB open without highlighting or
a language server, because doing either at that size is slower than it is
useful. Over 100 MB, The Editor refuses: a text editor cannot open a binary
without corrupting it on save.

---

## What is where

| | |
|---|---|
| Settings | `settings.toml` in the configuration folder |
| Session | Which files and folder were open |
| Logs | One text file per day |
| Recovery | Unsaved buffers, deleted on a clean exit |

**Help > Open Settings Folder** and **Help > Open Log Folder** find them for
you. Put a file called `portable.txt` next to the executable and all of it
moves alongside the executable instead, which is what you want on a memory
stick.

---

## Licence

The Editor is MIT licensed. It is built from open-source Rust crates, listed
with their licences under **Help > Third-Party Licences**.
