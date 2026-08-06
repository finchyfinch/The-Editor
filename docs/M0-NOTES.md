# M0 — Bootstrap: what was built and what was decided

Completed 2026-08-06. Corresponds to milestone M0 in [PLAN.md](../PLAN.md) §9.

## Verified

| Check | Result |
|---|---|
| `cargo build --workspace` | clean |
| `cargo build --release --workspace` | clean, 1 m 12 s cold |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `cargo test --workspace` | 3 passed, 0 failed |
| `the-editor.exe` launches | window opens, log written, exits cleanly |
| `spike.exe` launches | window opens, no stderr, no panic |

Verified on Windows 11 / rustc 1.97.1 only. **Linux and macOS are unverified** —
that has to happen before M1 is called done, because winit and wgpu backend
differences surface at window creation, not at compile time.

## Structure

Eight crates plus the spike. The dependency rule that matters: `core`,
`syntax`, `lsp`, `proc`, `search` and `config` have no path to `egui`. Only
`widgets` and `app` do. PLAN.md §2.1 explains why; the practical effect is that
`editor-core`'s tests run in milliseconds with no window and no GPU.

## Decisions taken during implementation

**egui 0.36 reworked its API.** The panel types were unified: `SidePanel` and
`TopBottomPanel` are gone, replaced by `Panel::left/right/top/bottom`, and
`eframe::App` now has `fn ui(&mut self, ui: &mut Ui, frame: &mut Frame)` instead
of `update(&mut self, ctx, frame)`. Panels take `&mut Ui` rather than `&Context`.
Most tutorial and blog material online predates this and will not compile.
Read the docs.rs page for the exact version, or the source in
`~/.cargo/registry/src/*/egui-0.36.0/`.

Other renames encountered: `ctx.set_style` → `ctx.all_styles_mut` /
`set_style_of` (styles are now per-theme); `ui.fonts` → `ui.fonts_mut`;
`Panel::exact_height`/`default_width` → `exact_size`/`default_size`.

**`panic = "unwind"` is pinned in the release profile.** `panic = "abort"` would
shave a little binary size, but it skips the panic hook, which is what saves
unsaved buffers on a crash. That trade is not available to us.

**`[profile.dev.package."*"] opt-level = 2`.** An unoptimised rope and, later,
an unoptimised tree-sitter make debug builds unusable for actually editing text.
Dependencies are optimised even in dev; our own crates stay at `opt-level = 1`
so builds remain fast and backtraces stay readable.

**Build metadata comes from a dependency-free `build.rs`** shelling out to git,
rather than the `vergen` crate. Fewer dependencies, and it degrades to
`"unknown"` when built from a tarball with no `.git` present — which is what the
About box currently shows, correctly, since nothing has been committed yet.

**`unwrap_used` is `warn` workspace-wide but `deny` at the top of `core`, `lsp`
and `proc`.** Those three touch user data and external processes. `expect_used`
stays allowed: an `expect` with a message explaining the invariant is honest
documentation; a bare `unwrap` is not.

**Portable-install mode** was cheap to add now (a marker file next to the
executable redirects config and data beside the binary) and awkward to retrofit
once path resolution is assumed everywhere.

## Follow-ups before M1 closes

- Build and run on Linux and macOS. Expect winit/wayland packaging issues.
- Add the workspace dependency-graph test that enforces the no-egui rule.
- Add the bare backup remote: `git remote add origin <nas>/the-editor.git`.
- Embed JetBrains Mono rather than relying on egui's bundled Hack, so the
  editor looks identical on all three platforms.
