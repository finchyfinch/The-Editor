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
| `Ctrl+Shift+T` | Run the test the caret is in |
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

**Python docstrings.** Typing `"""` on the first line of a `def` or `class`
body writes the skeleton of a docstring from the signature above it: an entry
per parameter with its type and whether it has a default, the return type,
what the body raises, and `Yields` instead of `Returns` for a generator. A
class describes what building one takes, which is `__init__`'s parameters.
`self` and `cls` are left out. The caret lands on the summary line, which is
the one part no signature can supply. One `Ctrl+Z` takes the whole thing back.

Three layouts are offered — Google, NumPy and Sphinx's reST — in
**Settings > Python**, along with **Off**, which leaves the quotes to you.
Typing `"""` anywhere else is just a string: the closing three go in with the
opening three and the caret sits between them, which is the one part of this
that applies to every triple-quoted string.

**Rename** (`F2`) asks the language server, so it renames the *symbol* rather
than the text — a variable called `id` will not take the `id` out of
`identity`. It needs the file saved first, because the server reads from disk.
There is no find-and-replace fallback: that is how the wrong things get
renamed.

**The gutter** is not text, and the pointer says so: an arrow over the line
numbers, a hand over the breakpoint strip and over a fold chevron, and the
I-beam back again over the code.

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

### Branches

The top of the Source Control panel names the branch you are on, what it tracks,
and how far ahead or behind it is. The status bar shows the same, shorter.

**Branches…** lists them. Switch with a click; type a name to create one, and
the box says at once if git would refuse the name rather than waiting for the
button. Remote branches are listed to show what is there — checking one out
directly would leave you on a detached HEAD, so The Editor does not offer it.

**Delete** tries the safe way first. When a branch has commits that exist
nowhere else git refuses, and that refusal becomes the offer to delete it
anyway — with a note that the reflog can still find them for a while and The
Editor cannot.

### Remotes

**Fetch** asks the remote what it has and changes nothing here. **Pull** brings
your branch up to date and is fast-forward only: if the histories have diverged
it refuses rather than starting a merge you did not ask for, and the merge or
rebase is then yours to do deliberately. **Push** sends the current branch, and
is never forced — a forced push can destroy somebody else's commits, and the
terminal is there for the rare case that genuinely needs one.

Whatever git prints appears under the buttons. One operation runs at a time and
the panel says which; the rest of the git display waits for it, because two git
processes on one repository get in each other's way.

Anything needing a password will fail rather than hang — The Editor never
answers a credential prompt on your behalf. Use the terminal (**Ctrl+`**) for
those, or an agent or credential helper that answers without asking.

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

### Tests

**Run → Run Tests** runs the whole suite; **Ctrl+Shift+T** runs just the test
the caret is in. Results appear in the **Tests** panel *as they happen*, not in
one lump at the end, so a slow suite tells you where it has got to.

| | |
|---|---|
| Run Tests | Everything the framework can find |
| Run Tests in This File | Python only — a `.rs` file does not name a cargo target |
| Run the Test at the Caret | `Ctrl+Shift+T` |
| Run Failed Tests Again | Only the ones that failed last time |

Failures come first and are the only thing shown until you tick **Show
passing** — in a run of four hundred with two failures, the two are the part
worth looking at. Click one to jump to the line it failed on and see the whole
message; click **Run** beside it to run that one again.

Python uses **pytest**, run as `python -m pytest` so it is the project's pytest
and not whichever is first on `PATH`. Rust uses **cargo test**. Both are your
own — your `conftest.py`, your fixtures, your `pytest.ini` and your
`[profile.test]` all apply. The full output is in the **Output** panel as
usual, which is where to look when a test printed something.

Tests run on plain pipes rather than in a terminal, unlike everything else the
Run menu starts. A test runner that can see a terminal redraws its lines to
keep a percentage at the right-hand edge, and what comes out cannot be read
reliably. Nothing is lost by it — nobody types into a test run.

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

**Hover** the pointer over a name and hold it still: a moment later a box
appears with the type, the signature and the documentation.

Hovering an underlined problem shows it at the top of that box, in full: how
serious it is, the rule (`reportArgumentType`, `F401`), which server reported
it, and the whole message, including the lines after the first that usually
explain it. Hover the marker in the gutter, or the line number beside it, to
see every problem on that line, including warnings that are not underlined.

**Right-click** on a problem for **Show in Problems**, which opens the Problems
panel scrolled to it and highlighted, and **Copy Problem**, which puts the file,
line, rule and whole message on the clipboard. Both work from anywhere on the
line, including a right-click in the gutter.

With no server running it still answers, from the file you are looking at — the
line the name was declared on, and its docstring or doc comment — and says that
is what it is doing. It cannot tell you a type, or anything about a name from
another file or a library, so it says so rather than letting a partial answer
pass for a whole one.

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
