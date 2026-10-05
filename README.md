# The Editor

An IDE for Python and Rust development, written in Rust.

Copyright © 2026 Gareth Finch. MIT licensed.

**Status: v1.2.0.** The Editor handles day-to-day Python and Rust work:
tree-sitter highlighting, completion, and diagnostics from `rust-analyzer`,
Pyright and Ruff; Go to Definition and Find Uses; project-wide search and Go to
File; a file explorer; an integrated console for running code; and a Python
debugger with breakpoints, stepping, the call stack and local variables. A
Windows build is on the [releases page](../../releases); Windows is the only
platform it has been built and run on. [CHANGELOG.md](CHANGELOG.md) records
what has landed; [PLAN.md](PLAN.md) has the full design and the milestone
schedule.

## Building

Requirements:

- Rust 1.97.1 (pinned in `rust-toolchain.toml`; `rustup` picks it up automatically)
- A C toolchain, because the tree-sitter grammars compile C:
  - **Windows** — Visual Studio Build Tools with the "Desktop development with C++" workload
  - **Linux** — `build-essential` (plus `libxkbcommon-dev libwayland-dev libxcb1-dev` for winit)
  - **macOS** — Xcode Command Line Tools (`xcode-select --install`)

Windows is the only platform The Editor has been built on. The Linux and macOS
entries are what the dependencies call for, and the code carries the paths for
both, but neither has been compiled or run.

```bash
cargo build --workspace
```

## Running

```bash
cargo editor
```

## Checks

The same three commands the `pre-push` hook runs:

```bash
cargo xfmt
```

```bash
cargo xlint
```

```bash
cargo xtest
```

[docs/INVARIANTS.md](docs/INVARIANTS.md) lists the rules the code relies on and
the test that enforces each.

Dependency licence and advisory audit (needs `cargo install cargo-deny`):

```bash
cargo deny check
```

## Layout

| Path | Contents |
|---|---|
| `crates/app` | The `the-editor` binary: window, menus, layout, command routing |
| `crates/core` | Buffers, edits, undo, selections, indentation |
| `crates/syntax` | Language registry and tree-sitter highlighting |
| `crates/lsp` | Language server client and lifecycle |
| `crates/debug` | Debug Adapter Protocol client: `debugpy`, breakpoints, stepping |
| `crates/proc` | Running user code: PTY, run configs, output parsing |
| `crates/search` | In-file and project-wide search |
| `crates/config` | Settings, keymap, themes, paths |
| `crates/testing` | Running pytest and `cargo test`, and reading the results |
| `crates/vcs` | Git: what the repository says about the files being edited |
| `crates/widgets` | egui widgets — the only library crate that knows the toolkit |

Only `crates/widgets` and `crates/app` depend on egui. Keeping that boundary
intact is what makes the toolkit choice reversible.

## Where The Editor keeps its files

| | Windows | Linux | macOS |
|---|---|---|---|
| Config | `%APPDATA%\Gareth Finch\The Editor\config` | `~/.config/the-editor` | `~/Library/Application Support/The Editor` |
| Logs, backups | `…\The Editor\data\logs`, `\backups` | `~/.local/share/the-editor/…` | as above |

Placing a file named `the-editor.portable` next to the executable makes it a
portable install: everything goes into `config/` and `data/` beside the binary
instead, leaving the host profile untouched.

## A note on unsigned binaries

Releases are not code-signed or notarised — a deliberate choice. Verify the
SHA-256 checksum published with each release, then:

- **Windows**: SmartScreen may warn. Click *More info* → *Run anyway*.
- **macOS**: Gatekeeper will block the first launch. Right-click the app →
  *Open*, or run `xattr -dr com.apple.quarantine "/Applications/The Editor.app"`.
- **Linux**: unaffected.
