//! Documents: a rope, where it came from, and how to write it back unchanged.
//!
//! The rope always holds text with `\n` line endings, whatever was on disk.
//! That keeps every offset calculation in the editor honest — a CRLF file
//! would otherwise make "column" mean two different things depending on
//! platform. The original line ending is recorded and restored on save, so
//! opening a CRLF file and saving it produces a byte-identical CRLF file and
//! not a diff touching every line. Same for the byte-order mark.
//!
//! M2 adds edits, undo and selections on top of this. M1 needs only load,
//! display and save.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ropey::Rope;

use crate::edit::{self, Change, Transaction};
use crate::history::History;
use crate::selection::Selection;

/// Files above this size open as read-only plain text with no highlighting and
/// no language server. See PLAN.md §8 "Large files".
pub const LARGE_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Files above this size are refused outright.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// How much of the file to inspect when deciding whether it is binary.
const SNIFF_BYTES: usize = 8192;

/// How the file was encoded on disk, so it can be written back the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Not valid UTF-8; decoded as Windows-1252 so the file is at least
    /// readable. Saving re-encodes to the same.
    Latin1,
}

impl Encoding {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 BOM",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Latin1 => "Windows-1252",
        }
    }
}

/// Line terminator used by the file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    /// What a new file gets: whatever is native to this platform.
    #[must_use]
    pub const fn platform_default() -> Self {
        if cfg!(windows) { Self::Crlf } else { Self::Lf }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
        }
    }

    /// Detect from content. Mixed endings resolve to whichever is more common,
    /// which is what saving will then normalise the file to.
    #[must_use]
    pub fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        if crlf > lf { Self::Crlf } else { Self::Lf }
    }
}

/// Why a document cannot be edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    /// The file is marked read-only on disk.
    FilePermissions,
    /// Too big to edit safely with highlighting and language servers off.
    TooLarge,
}

/// An open file.
#[derive(Debug)]
pub struct Document {
    path: Option<PathBuf>,
    text: Rope,
    encoding: Encoding,
    line_ending: LineEnding,
    read_only: Option<ReadOnlyReason>,
    /// True when the file exceeded [`LARGE_FILE_BYTES`], so callers know to
    /// skip highlighting and language-server registration.
    large: bool,

    history: History,
    /// Changes applied since the last [`Self::take_changes`].
    ///
    /// An outbox rather than a callback: the incremental highlighter (M3) and
    /// the language server (M6) both need to see every edit exactly once, and
    /// edits originate from several places — typing, paste, undo, redo, a
    /// project-wide replace. Queueing them here means no new edit path can
    /// forget to notify anyone.
    pending: Vec<Change>,
    /// Bumped by every mutation, including undo and redo. Compared against
    /// [`Self::saved_version`] to decide whether the tab shows an unsaved
    /// marker.
    ///
    /// Deliberately not "history depth at last save": undoing past the save
    /// point and then editing again can land back at the same depth with
    /// different content, and a document that wrongly reports itself clean
    /// loses the user's work. Over-reporting dirty costs an unnecessary save.
    version: u64,
    saved_version: u64,
}

impl Document {
    /// A new unsaved document.
    #[must_use]
    pub fn untitled() -> Self {
        Self {
            path: None,
            text: Rope::new(),
            encoding: Encoding::Utf8,
            line_ending: LineEnding::platform_default(),
            read_only: None,
            large: false,
            history: History::default(),
            pending: Vec::new(),
            version: 0,
            saved_version: 0,
        }
    }

    /// Read a file from disk.
    ///
    /// # Errors
    /// If the file cannot be read, is larger than [`MAX_FILE_BYTES`], or looks
    /// like binary content. Opening a binary file in a text editor corrupts it
    /// on save, so this refuses rather than doing its best.
    pub fn open(path: &Path) -> Result<Self> {
        let meta =
            std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;

        if meta.len() > MAX_FILE_BYTES {
            bail!(
                "{} is {:.1} MB; the limit is {} MB",
                path.display(),
                meta.len() as f64 / (1024.0 * 1024.0),
                MAX_FILE_BYTES / (1024 * 1024)
            );
        }

        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;

        if looks_binary(&bytes) {
            bail!(
                "{} looks like a binary file; opening it as text would corrupt it on save",
                path.display()
            );
        }

        let (contents, encoding) = decode(&bytes);
        let line_ending = LineEnding::detect(&contents);
        // Normalise to LF in the buffer; line_ending remembers what to restore.
        let normalised = if line_ending == LineEnding::Crlf {
            contents.replace("\r\n", "\n")
        } else {
            contents
        };

        let large = meta.len() > LARGE_FILE_BYTES;
        let read_only = if meta.permissions().readonly() {
            Some(ReadOnlyReason::FilePermissions)
        } else if large {
            Some(ReadOnlyReason::TooLarge)
        } else {
            None
        };

        Ok(Self {
            path: Some(path.to_path_buf()),
            text: Rope::from_str(&normalised),
            encoding,
            line_ending,
            read_only,
            large,
            history: History::default(),
            pending: Vec::new(),
            version: 0,
            saved_version: 0,
        })
    }

    /// Write back to disk, restoring the original encoding and line endings.
    ///
    /// The write is atomic — temporary file in the same directory, then rename
    /// — so an interrupted save cannot truncate the user's work.
    ///
    /// # Errors
    /// If there is no path (use `save_as`), or the write fails.
    pub fn save(&mut self) -> Result<()> {
        let path = self
            .path
            .clone()
            .context("this document has never been saved; use Save As")?;
        self.save_as(&path)
    }

    /// Write to `path` and adopt it as this document's path.
    ///
    /// # Errors
    /// If the write fails.
    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        let text = self.text.to_string();
        let restored = if self.line_ending == LineEnding::Crlf {
            text.replace('\n', "\r\n")
        } else {
            text
        };
        let bytes = encode(&restored, self.encoding);

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;

        // The temporary file must be on the same filesystem as the target, or
        // the rename is not atomic. Same directory guarantees that.
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|e| e.to_str()).unwrap_or("")
        ));
        std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;

        self.path = Some(path.to_path_buf());
        self.saved_version = self.version;
        // A save is an undo boundary: typing before and after it must not be
        // folded into one step.
        self.history.break_run();
        Ok(())
    }

    // ---- editing ---------------------------------------------------------

    /// Apply a transaction. **The only way this document's text changes.**
    ///
    /// `before` and `after` are the selection either side of the edit, so undo
    /// can restore the caret to where the change happened.
    pub fn apply(&mut self, tx: &Transaction, before: Selection, after: Selection) -> Vec<Change> {
        if tx.is_empty() {
            return Vec::new();
        }
        let applied = edit::apply(&mut self.text, tx);
        self.history
            .push(tx.clone(), applied.inverse, before, after);
        self.version = self.version.wrapping_add(1);
        self.pending.extend(applied.changes.iter().cloned());
        applied.changes
    }

    /// Take every change applied since this was last called.
    ///
    /// Drives the incremental highlighter now and the language server in M6.
    /// Draining rather than peeking is deliberate: a consumer that forgets to
    /// call this shows up as stale highlighting, not as silently duplicated
    /// edits sent to a language server.
    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.pending)
    }

    /// True if there are changes no consumer has seen yet.
    #[must_use]
    pub fn has_pending_changes(&self) -> bool {
        !self.pending.is_empty()
    }

    /// A counter bumped by every mutation.
    ///
    /// Lets a consumer cache something derived from the text — search results,
    /// a symbol list — and notice cheaply when it has gone stale, without
    /// comparing the contents.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Undo one step. Returns the selection to restore, or `None` if there is
    /// nothing to undo.
    pub fn undo(&mut self) -> Option<Selection> {
        let step = self.history.undo()?;
        let applied = edit::apply(&mut self.text, &step.transaction);
        self.version = self.version.wrapping_add(1);
        self.pending.extend(applied.changes);
        Some(step.selection.clamped(self.text.len_chars()))
    }

    /// Redo one step. Returns the selection to restore.
    pub fn redo(&mut self) -> Option<Selection> {
        let step = self.history.redo()?;
        let applied = edit::apply(&mut self.text, &step.transaction);
        self.version = self.version.wrapping_add(1);
        self.pending.extend(applied.changes);
        Some(step.selection.clamped(self.text.len_chars()))
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Force the next edit to begin a new undo entry.
    pub fn break_undo_run(&mut self) {
        self.history.break_run();
    }

    // ---- positions -------------------------------------------------------

    /// Total characters, the upper bound for any offset.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    /// Line containing `offset`, clamped into range.
    #[must_use]
    pub fn line_of(&self, offset: usize) -> usize {
        self.text.char_to_line(offset.min(self.text.len_chars()))
    }

    /// First character offset of `line`, clamped into range.
    #[must_use]
    pub fn line_start(&self, line: usize) -> usize {
        self.text
            .line_to_char(line.min(self.text.len_lines().saturating_sub(1)))
    }

    /// Characters in `line`, excluding its trailing newline.
    #[must_use]
    pub fn line_len(&self, line: usize) -> usize {
        if line >= self.text.len_lines() {
            return 0;
        }
        let slice = self.text.line(line);
        let mut len = slice.len_chars();
        // Trim the terminator, however it is represented.
        if slice.chars_at(len).reversed().next() == Some('\n') {
            len -= 1;
            if slice.chars_at(len).reversed().next() == Some('\r') {
                len -= 1;
            }
        }
        len
    }

    /// The text of `line` without its terminator.
    #[must_use]
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.text.len_lines() {
            return String::new();
        }
        let start = self.line_start(line);
        let end = start + self.line_len(line);
        self.text.slice(start..end).to_string()
    }

    /// One-based line and column, for the status bar.
    #[must_use]
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len_chars());
        let line = self.text.char_to_line(offset);
        (line + 1, offset - self.text.line_to_char(line) + 1)
    }

    /// Offset of `column` on `line`, clamped to that line's length so that
    /// moving down onto a shorter line lands at its end rather than wrapping.
    #[must_use]
    pub fn offset_at(&self, line: usize, column: usize) -> usize {
        let line = line.min(self.text.len_lines().saturating_sub(1));
        self.line_start(line) + column.min(self.line_len(line))
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Filename for the tab label, or "Untitled" for a new document.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "Untitled".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    #[must_use]
    pub fn text(&self) -> &Rope {
        &self.text
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.text.len_lines()
    }

    #[must_use]
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    #[must_use]
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Change the line ending the file will be written with.
    pub fn set_line_ending(&mut self, ending: LineEnding) {
        if self.line_ending != ending {
            self.line_ending = ending;
            self.version = self.version.wrapping_add(1);
        }
    }

    /// True when there are changes not yet written to disk.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    /// True when edits should be refused — a read-only file, or one too large
    /// to edit safely.
    #[must_use]
    pub fn is_editable(&self) -> bool {
        self.read_only.is_none()
    }

    #[must_use]
    pub fn read_only(&self) -> Option<ReadOnlyReason> {
        self.read_only
    }

    /// True if highlighting and language servers should be skipped.
    #[must_use]
    pub fn is_large(&self) -> bool {
        self.large
    }
}

/// A NUL byte in the first few KB is the standard heuristic, and it is right
/// far more often than it is wrong.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|&b| b == 0)
        // ...except UTF-16, which is full of NULs and is genuinely text.
        && !starts_with_utf16_bom(bytes)
}

fn starts_with_utf16_bom(bytes: &[u8]) -> bool {
    matches!(bytes.first_chunk::<2>(), Some([0xFF, 0xFE] | [0xFE, 0xFF]))
}

fn decode(bytes: &[u8]) -> (String, Encoding) {
    // `strip_prefix` rather than indexing: this crate denies
    // `clippy::indexing_slicing`, because a panic here loses a file.
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        let (text, _, _) = encoding_rs::UTF_8.decode(rest);
        return (text.into_owned(), Encoding::Utf8Bom);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let (text, _, _) = encoding_rs::UTF_16LE.decode(rest);
        return (text.into_owned(), Encoding::Utf16Le);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let (text, _, _) = encoding_rs::UTF_16BE.decode(rest);
        return (text.into_owned(), Encoding::Utf16Be);
    }

    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), Encoding::Utf8),
        Err(_) => {
            // Not UTF-8. Windows-1252 never fails to decode, so the file opens
            // readable rather than mangled with replacement characters.
            let (text, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            (text.into_owned(), Encoding::Latin1)
        }
    }
}

fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(text.as_bytes());
            out
        }
        Encoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE];
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out
        }
        Encoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF];
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
            out
        }
        Encoding::Latin1 => {
            let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(text);
            bytes.into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_line_endings_and_prefers_the_majority_when_mixed() {
        assert_eq!(LineEnding::detect("a\nb\nc"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("a\r\nb\r\nc"), LineEnding::Crlf);
        assert_eq!(LineEnding::detect("a\r\nb\r\nc\nd"), LineEnding::Crlf);
        assert_eq!(LineEnding::detect("a\nb\nc\r\nd"), LineEnding::Lf);
        assert_eq!(
            LineEnding::detect("no newlines at all"),
            LineEnding::Lf,
            "a single-line file must not be guessed as CRLF"
        );
    }

    #[test]
    fn decodes_each_encoding_and_reports_it() {
        assert_eq!(decode(b"plain"), ("plain".to_owned(), Encoding::Utf8));

        let bom = [&[0xEF, 0xBB, 0xBF][..], b"plain"].concat();
        assert_eq!(decode(&bom), ("plain".to_owned(), Encoding::Utf8Bom));

        let mut le = vec![0xFF, 0xFE];
        for u in "hi".encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode(&le), ("hi".to_owned(), Encoding::Utf16Le));

        // 0xA9 is invalid UTF-8 but is (c) in Windows-1252.
        let (text, enc) = decode(b"caf\xE9 \xA9");
        assert_eq!(enc, Encoding::Latin1);
        assert!(text.starts_with("caf\u{e9}"), "got {text:?}");
    }

    #[test]
    fn every_encoding_round_trips() {
        for enc in [
            Encoding::Utf8,
            Encoding::Utf8Bom,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Latin1,
        ] {
            let original = "hello \u{e9} world";
            let (decoded, detected) = decode(&encode(original, enc));
            assert_eq!(decoded, original, "{enc:?} content");
            assert_eq!(detected, enc, "{enc:?} detection");
        }
    }

    #[test]
    fn binary_content_is_rejected_but_utf16_is_not() {
        assert!(looks_binary(b"\x7fELF\0\0\0\0"));
        assert!(!looks_binary(b"def main():\n    pass\n"));

        let mut utf16 = vec![0xFF, 0xFE];
        for u in "hi".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        assert!(
            !looks_binary(&utf16),
            "UTF-16 is full of NUL bytes and is still text"
        );
    }

    #[test]
    fn new_documents_are_clean_and_untitled() {
        let doc = Document::untitled();
        assert!(!doc.is_dirty());
        assert_eq!(doc.display_name(), "Untitled");
        assert!(doc.path().is_none());
        assert_eq!(doc.line_ending(), LineEnding::platform_default());
    }

    #[test]
    fn a_crlf_file_survives_open_and_save_byte_for_byte() {
        let dir = std::env::temp_dir().join("the-editor-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("crlf.txt");
        let original = b"line one\r\nline two\r\nline three\r\n";
        std::fs::write(&path, original).expect("write fixture");

        let mut doc = Document::open(&path).expect("open");
        assert_eq!(doc.line_ending(), LineEnding::Crlf);
        assert_eq!(
            doc.text().to_string(),
            "line one\nline two\nline three\n",
            "the buffer must hold LF regardless of what is on disk"
        );

        doc.save().expect("save");
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            original,
            "saving an untouched CRLF file must not rewrite every line"
        );

        std::fs::remove_file(&path).ok();
    }
}
