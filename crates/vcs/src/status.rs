//! What the working tree looks like compared with the index and with HEAD.
//!
//! `git status --porcelain=v1 -z`, parsed. The porcelain format is the one git
//! promises not to change, and `-z` separates records with NUL, which is the
//! only way to be right about a filename containing a newline or a quote — the
//! human-readable format escapes those, and unescaping is a second parser with
//! its own bugs.
//!
//! Every record is two letters and a path. The first letter is what the *index*
//! has to say about the file — what would be committed — and the second is what
//! the *working tree* has to say — what would not. A file can be both at once,
//! and usually is while you are half-way through staging it, which is the whole
//! reason the two are reported separately rather than merged into one verdict.

use std::path::{Path, PathBuf};

/// What happened to a file, on one side of the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing, on this side.
    Unmodified,
    Modified,
    /// A file became a directory, a symlink, or the other way round.
    TypeChanged,
    Added,
    Deleted,
    Renamed,
    Copied,
    /// A merge conflict. Both sides carry it, in one of several combinations.
    Unmerged,
    /// Git has never been told about this file.
    Untracked,
    /// Excluded by an ignore rule.
    Ignored,
}

impl Change {
    /// Parse one of the two status letters.
    ///
    /// Unrecognised letters become [`Change::Modified`] rather than an error:
    /// a future git that reports something new should leave the file *visible*
    /// in the panel, described imprecisely, rather than dropped from it.
    #[must_use]
    fn from_letter(letter: char) -> Self {
        match letter {
            ' ' | '.' => Self::Unmodified,
            'A' => Self::Added,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            'T' => Self::TypeChanged,
            'U' => Self::Unmerged,
            '?' => Self::Untracked,
            '!' => Self::Ignored,
            _ => Self::Modified,
        }
    }

    /// A word for it, for the panel.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unmodified => "unchanged",
            Self::Modified => "modified",
            Self::TypeChanged => "type changed",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::Unmerged => "conflicted",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
        }
    }

    /// A single letter, as git writes it, for the tight column in the panel.
    #[must_use]
    pub fn letter(self) -> &'static str {
        match self {
            Self::Unmodified => " ",
            Self::Modified => "M",
            Self::TypeChanged => "T",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::Unmerged => "U",
            Self::Untracked => "?",
            Self::Ignored => "!",
        }
    }
}

/// One file, and what each side of the index says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the repository's top level, as git spells it.
    pub path: PathBuf,
    /// Where a rename or a copy came from.
    pub original: Option<PathBuf>,
    /// What would be committed.
    pub index: Change,
    /// What would not.
    pub worktree: Change,
}

impl Entry {
    /// Whether anything of this file is staged.
    #[must_use]
    pub fn is_staged(&self) -> bool {
        !matches!(
            self.index,
            Change::Unmodified | Change::Untracked | Change::Ignored
        ) && !self.is_conflicted()
    }

    /// Whether anything of this file is *not* staged.
    ///
    /// A file can be both at once — stage it, edit it again, and it is.
    #[must_use]
    pub fn is_unstaged(&self) -> bool {
        !matches!(self.worktree, Change::Unmodified | Change::Ignored) && !self.is_conflicted()
    }

    /// Whether git has never been told about this file.
    #[must_use]
    pub fn is_untracked(&self) -> bool {
        self.index == Change::Untracked || self.worktree == Change::Untracked
    }

    /// Whether this file is in conflict from a merge.
    ///
    /// `U` on either side means so, and `AA` and `DD` — both added, both
    /// deleted — are conflicts that git spells without using `U` at all.
    #[must_use]
    pub fn is_conflicted(&self) -> bool {
        self.index == Change::Unmerged
            || self.worktree == Change::Unmerged
            || (self.index == Change::Added && self.worktree == Change::Added)
            || (self.index == Change::Deleted && self.worktree == Change::Deleted)
    }

    /// How to describe the file in one word.
    ///
    /// The unstaged side wins when both have something to say, because that is
    /// the more recent thing to have happened to the file.
    #[must_use]
    pub fn label(&self) -> &'static str {
        if self.is_conflicted() {
            return "conflicted";
        }
        if self.is_unstaged() {
            return self.worktree.label();
        }
        self.index.label()
    }

    /// The file's own name, for the panel's first column.
    #[must_use]
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into(),
        )
    }

    /// The directory it sits in, or nothing at the top level.
    #[must_use]
    pub fn folder(&self) -> Option<String> {
        let parent = self.path.parent()?;
        let text = parent.to_string_lossy();
        (!text.is_empty()).then(|| text.into_owned())
    }
}

/// Everything git reported, in the order it reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub entries: Vec<Entry>,
}

impl Status {
    /// Files with something staged.
    pub fn staged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_staged())
    }

    /// Tracked files with something not staged.
    pub fn unstaged(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| e.is_unstaged() && !e.is_untracked())
    }

    /// Files git has never been told about.
    pub fn untracked(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_untracked())
    }

    /// Files in conflict, which must be dealt with before anything else can be.
    pub fn conflicted(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_conflicted())
    }

    /// Whether the working tree matches HEAD entirely.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }

    /// What is known about one file, if anything.
    #[must_use]
    pub fn entry(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }
}

/// Parse the output of `git status --porcelain=v1 -z`.
///
/// Records are `XY <path>` separated by NUL. A rename or a copy is followed by
/// a *second* NUL-separated field holding where it came from, which is why this
/// cannot be a simple `split` and `map`.
#[must_use]
pub fn parse(output: &str) -> Status {
    let mut entries = Vec::new();
    // `split('\0')` on a NUL-terminated stream leaves a trailing empty field,
    // and an empty output leaves exactly one. Both are skipped below.
    let mut fields = output.split('\0');

    while let Some(record) = fields.next() {
        // A record is two status letters, a space, and the path. Anything
        // shorter is the trailing empty field, or a line git should not have
        // written; either way there is nothing to show.
        let mut chars = record.chars();
        let (Some(index), Some(worktree)) = (chars.next(), chars.next()) else {
            continue;
        };
        let path = record.get(3..).unwrap_or_default();
        if path.is_empty() {
            continue;
        }

        let index = Change::from_letter(index);
        let worktree = Change::from_letter(worktree);

        // A rename or a copy spends a second field on where it came from. It
        // has to be consumed whether or not it is wanted, or every record after
        // it is read one field out of step.
        let original = if index == Change::Renamed
            || index == Change::Copied
            || worktree == Change::Renamed
            || worktree == Change::Copied
        {
            fields.next().filter(|s| !s.is_empty()).map(PathBuf::from)
        } else {
            None
        };

        entries.push(Entry {
            path: PathBuf::from(path),
            original,
            index,
            worktree,
        });
    }

    Status { entries }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the NUL-separated form from readable records.
    fn porcelain(records: &[&str]) -> String {
        let mut text = String::new();
        for record in records {
            text.push_str(record);
            text.push('\0');
        }
        text
    }

    #[test]
    fn a_clean_tree_has_no_entries() {
        let status = parse("");
        assert!(status.is_clean());
        assert_eq!(status.entries, []);
    }

    #[test]
    fn a_modified_but_unstaged_file() {
        let status = parse(&porcelain(&[" M src/main.rs"]));
        let entry = &status.entries[0];
        assert_eq!(entry.path, PathBuf::from("src/main.rs"));
        assert_eq!(entry.index, Change::Unmodified);
        assert_eq!(entry.worktree, Change::Modified);
        assert!(!entry.is_staged());
        assert!(entry.is_unstaged());
        assert_eq!(entry.label(), "modified");
    }

    #[test]
    fn a_staged_file() {
        let status = parse(&porcelain(&["M  src/main.rs"]));
        let entry = &status.entries[0];
        assert!(entry.is_staged());
        assert!(!entry.is_unstaged());
    }

    /// The case the two columns exist for: staged, then edited again.
    #[test]
    fn a_file_can_be_staged_and_unstaged_at_once() {
        let status = parse(&porcelain(&["MM src/main.rs"]));
        let entry = &status.entries[0];
        assert!(entry.is_staged());
        assert!(entry.is_unstaged());
        assert_eq!(status.staged().count(), 1);
        assert_eq!(status.unstaged().count(), 1);
    }

    #[test]
    fn an_untracked_file() {
        let status = parse(&porcelain(&["?? notes.txt"]));
        let entry = &status.entries[0];
        assert!(entry.is_untracked());
        assert!(!entry.is_staged());
        assert_eq!(entry.label(), "untracked");
        assert_eq!(status.untracked().count(), 1);
        assert_eq!(
            status.unstaged().count(),
            0,
            "an untracked file is listed on its own, not among the modified ones"
        );
    }

    #[test]
    fn a_new_file_that_has_been_staged() {
        let status = parse(&porcelain(&["A  new.rs"]));
        let entry = &status.entries[0];
        assert!(entry.is_staged());
        assert!(!entry.is_untracked());
        assert_eq!(entry.label(), "added");
    }

    #[test]
    fn a_deleted_file() {
        let status = parse(&porcelain(&[" D gone.rs", "D  also-gone.rs"]));
        assert_eq!(status.entries[0].worktree, Change::Deleted);
        assert!(status.entries[0].is_unstaged());
        assert_eq!(status.entries[1].index, Change::Deleted);
        assert!(status.entries[1].is_staged());
    }

    /// A rename carries a second field, and everything after it must still
    /// line up — this is the record that breaks a naive split.
    #[test]
    fn a_rename_reports_where_it_came_from() {
        let status = parse(&porcelain(&[
            "R  new/name.rs",
            "old/name.rs",
            " M other.rs",
        ]));
        assert_eq!(status.entries.len(), 2, "two files, three fields");
        assert_eq!(status.entries[0].path, PathBuf::from("new/name.rs"));
        assert_eq!(
            status.entries[0].original,
            Some(PathBuf::from("old/name.rs"))
        );
        assert_eq!(
            status.entries[1].path,
            PathBuf::from("other.rs"),
            "the record after a rename must not be read one field out of step"
        );
    }

    #[test]
    fn a_copy_also_reports_its_source() {
        let status = parse(&porcelain(&["C  copy.rs", "original.rs"]));
        assert_eq!(status.entries[0].index, Change::Copied);
        assert_eq!(
            status.entries[0].original,
            Some(PathBuf::from("original.rs"))
        );
    }

    #[test]
    fn a_conflict_is_neither_staged_nor_unstaged() {
        let status = parse(&porcelain(&["UU both.rs"]));
        let entry = &status.entries[0];
        assert!(entry.is_conflicted());
        assert!(
            !entry.is_staged() && !entry.is_unstaged(),
            "a conflict is not something to stage or unstage; it is something to resolve"
        );
        assert_eq!(entry.label(), "conflicted");
        assert_eq!(status.conflicted().count(), 1);
    }

    /// Git spells two of the conflict states without using `U` at all.
    #[test]
    fn both_added_and_both_deleted_are_conflicts_too() {
        let status = parse(&porcelain(&["AA both-added.rs", "DD both-deleted.rs"]));
        assert!(status.entries[0].is_conflicted(), "AA");
        assert!(status.entries[1].is_conflicted(), "DD");
        assert_eq!(status.conflicted().count(), 2);
    }

    /// The reason for `-z`. A filename with a newline in it is legal, and the
    /// readable format would have escaped and quoted it.
    #[test]
    fn a_filename_containing_a_newline_survives() {
        let status = parse(&porcelain(&[" M odd\nname.txt"]));
        assert_eq!(status.entries.len(), 1);
        assert_eq!(status.entries[0].path, PathBuf::from("odd\nname.txt"));
    }

    #[test]
    fn a_filename_containing_spaces_keeps_all_of_them() {
        let status = parse(&porcelain(&[" M some folder/a file.txt"]));
        assert_eq!(
            status.entries[0].path,
            PathBuf::from("some folder/a file.txt")
        );
    }

    #[test]
    fn an_unrecognised_letter_still_lists_the_file() {
        // A future git reporting something new must not make files vanish from
        // the panel; describing it imprecisely is much the lesser evil.
        let status = parse(&porcelain(&["X  strange.rs"]));
        assert_eq!(status.entries.len(), 1);
        assert_eq!(status.entries[0].path, PathBuf::from("strange.rs"));
    }

    #[test]
    fn the_name_and_folder_are_split_for_the_panel() {
        let status = parse(&porcelain(&[" M crates/app/src/main.rs", " M top.rs"]));
        assert_eq!(status.entries[0].name(), "main.rs");
        assert_eq!(
            status.entries[0].folder().as_deref(),
            Some("crates/app/src")
        );
        assert_eq!(status.entries[1].name(), "top.rs");
        assert_eq!(
            status.entries[1].folder(),
            None,
            "a file at the top level has no folder worth showing"
        );
    }

    #[test]
    fn a_file_can_be_looked_up_by_path() {
        let status = parse(&porcelain(&[" M a.rs", "M  b.rs"]));
        assert!(
            status
                .entry(Path::new("b.rs"))
                .is_some_and(Entry::is_staged)
        );
        assert_eq!(status.entry(Path::new("nothing.rs")), None);
    }

    /// A whole realistic status, of the kind that arrives mid-way through
    /// staging a change.
    #[test]
    fn a_mixed_working_tree_sorts_into_its_three_groups() {
        let status = parse(&porcelain(&[
            "M  staged.rs",
            " M unstaged.rs",
            "MM both.rs",
            "?? new.txt",
            "A  added.rs",
            "UU conflict.rs",
            "R  moved.rs",
            "was.rs",
        ]));
        assert_eq!(status.entries.len(), 7);
        assert_eq!(
            status.staged().map(Entry::name).collect::<Vec<_>>(),
            ["staged.rs", "both.rs", "added.rs", "moved.rs"]
        );
        assert_eq!(
            status.unstaged().map(Entry::name).collect::<Vec<_>>(),
            ["unstaged.rs", "both.rs"]
        );
        assert_eq!(
            status.untracked().map(Entry::name).collect::<Vec<_>>(),
            ["new.txt"]
        );
        assert_eq!(
            status.conflicted().map(Entry::name).collect::<Vec<_>>(),
            ["conflict.rs"]
        );
        assert!(!status.is_clean());
    }
}
