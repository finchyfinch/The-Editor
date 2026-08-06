# The Editor

A cross-platform IDE for Python and Rust development, written in Rust.

Copyright © 2026 Gareth Finch. MIT licensed.

**Status: pre-release.** M0 (bootstrap) is complete — the workspace builds and
opens a window. The editor itself starts at M2. See [PLAN.md](PLAN.md) for the
full design and milestone schedule.

## Building

Requirements:

- Rust 1.97.1 (pinned in `rust-toolchain.toml`; `rustup` picks it up automatically)
- A C toolchain, because the tree-sitter grammars adopted in M3 compile C:
  - **Windows** — Visual Studio Build Tools with the "Desktop development with C++" workload
  - **Linux** — `build-essential` (plus `libxkbcommon-dev libwayland-dev libxcb1-dev` for winit)
  - **macOS** — Xcode Command Line Tools (`xcode-select --install`)

```bash
cargo build --workspace
```

## Running

```bash
cargo editor
```

The rendering spike — the prototype that validates virtualised text rendering
over a rope — runs separately. Build it in release mode; the timings mean
nothing otherwise:

```bash
cargo spike
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
| `crates/proc` | Running user code: PTY, run configs, output parsing |
| `crates/search` | In-file and project-wide search |
| `crates/config` | Settings, keymap, themes, paths |
| `crates/widgets` | egui widgets — the only library crate that knows the toolkit |
| `crates/spike` | Throwaway rendering prototype; deleted after M2 |

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
