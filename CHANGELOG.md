# Changelog

All notable changes to The Editor are recorded here, in the
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. Versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries are written as each milestone lands, not retroactively at release time.

## [Unreleased]

### Added

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
