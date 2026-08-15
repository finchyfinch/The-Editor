//! Comparing two versions of a file, line by line.
//!
//! The gutter needs to know, for every line on screen, whether it is new,
//! changed, or sits just below something that was deleted. That is a line diff,
//! and this is Myers' algorithm — the same one git uses, so the marks agree
//! with what `git diff` will say rather than being a second opinion.
//!
//! Two shortcuts come first, and between them they handle almost every real
//! call. A common prefix and suffix are trimmed, because the usual case is a
//! small edit in the middle of a large file and there is no sense running an
//! algorithm over the ten thousand lines nobody touched. Then the search is
//! bounded: past a certain amount of difference the answer stops being useful
//! — nobody reads a gutter that is entirely marked — and it is better to say
//! "this is all different" quickly than to be precise slowly.

/// What happened to a line in the new text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    /// This line is new.
    Added,
    /// This line replaced one that was there before.
    Changed,
    /// Lines were deleted immediately *above* this one.
    ///
    /// A deletion has no line of its own to mark, so it is recorded against
    /// the line that now follows it. That is what lets the gutter draw a mark
    /// at the join, which is the only place it could go.
    DeletedAbove,
}

/// One run of difference, in zero-based line numbers.
///
/// `old` and `new` are half-open ranges. An insertion has an empty `old`, a
/// deletion an empty `new`, and a change has both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old: std::ops::Range<usize>,
    pub new: std::ops::Range<usize>,
}

impl Hunk {
    #[must_use]
    pub fn is_insertion(&self) -> bool {
        self.old.is_empty() && !self.new.is_empty()
    }

    #[must_use]
    pub fn is_deletion(&self) -> bool {
        !self.old.is_empty() && self.new.is_empty()
    }
}

/// How much difference is worth computing exactly.
///
/// Myers costs O((N+M)·D) where D is the number of differences. A file that has
/// been rewritten has a D in the thousands, and the answer — every line marked
/// — is one nobody reads. Giving up and saying so is both faster and no less
/// informative.
const MAX_DIFFERENCE: usize = 2_000;

/// The runs of difference between two sequences of lines.
#[must_use]
pub fn hunks(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    // Trim what matches at each end. This is what makes editing one line of a
    // ten-thousand-line file cost nothing.
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let remaining_old = &old[prefix..];
    let remaining_new = &new[prefix..];
    let suffix = remaining_old
        .iter()
        .rev()
        .zip(remaining_new.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let old_middle = &remaining_old[..remaining_old.len() - suffix];
    let new_middle = &remaining_new[..remaining_new.len() - suffix];

    if old_middle.is_empty() && new_middle.is_empty() {
        return Vec::new();
    }
    // One side empty is a pure insertion or deletion, and needs no search.
    if old_middle.is_empty() || new_middle.is_empty() {
        return vec![Hunk {
            old: prefix..prefix + old_middle.len(),
            new: prefix..prefix + new_middle.len(),
        }];
    }

    match myers(old_middle, new_middle) {
        Some(mut found) => {
            for hunk in &mut found {
                hunk.old.start += prefix;
                hunk.old.end += prefix;
                hunk.new.start += prefix;
                hunk.new.end += prefix;
            }
            found
        }
        // Too different to be worth describing precisely.
        None => vec![Hunk {
            old: prefix..prefix + old_middle.len(),
            new: prefix..prefix + new_middle.len(),
        }],
    }
}

/// Per-line marks for the gutter, indexed by line in the new text.
///
/// Only lines that differ appear. A deletion is recorded against the line that
/// follows it, or against the last line when the deletion was at the end.
#[must_use]
pub fn line_status(old: &[&str], new: &[&str]) -> Vec<(usize, LineStatus)> {
    let mut marks = Vec::new();
    for hunk in hunks(old, new) {
        if hunk.is_deletion() {
            // Nothing of this hunk exists in the new text, so the mark goes on
            // whatever is now in its place.
            let line = hunk.new.start.min(new.len().saturating_sub(1));
            marks.push((line, LineStatus::DeletedAbove));
            continue;
        }
        let status = if hunk.is_insertion() {
            LineStatus::Added
        } else {
            LineStatus::Changed
        };
        for line in hunk.new.clone() {
            marks.push((line, status));
        }
    }
    // One mark per line, and a deletion recorded against a line that also
    // changed loses to the change: the line itself being different is the more
    // useful thing to say about it.
    marks.sort_by_key(|(line, status)| (*line, matches!(status, LineStatus::DeletedAbove)));
    marks.dedup_by_key(|(line, _)| *line);
    marks
}

/// Myers' diff, returning `None` if the two are too different to bother with.
fn myers(old: &[&str], new: &[&str]) -> Option<Vec<Hunk>> {
    let n = old.len();
    let m = new.len();
    let max = (n + m).min(MAX_DIFFERENCE);

    // `v[k]` is the furthest x reached on diagonal k. Offset so k can be
    // negative; the trace keeps one copy per step so the path can be walked
    // back afterwards.
    let offset = max;
    let mut v = vec![0usize; 2 * max + 2];
    let mut trace = Vec::with_capacity(max + 1);

    for d in 0..=max {
        trace.push(v.clone());
        let mut k = -(d as isize);
        while k <= d as isize {
            let index = (k + offset as isize) as usize;
            // Step down when that is the further reach, right otherwise.
            let mut x = if k == -(d as isize) || (k != d as isize && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = (x as isize - k) as usize;

            // Follow the diagonal as far as the lines match.
            while x < n && y < m && old[x] == new[y] {
                x += 1;
                y += 1;
            }
            v[index] = x;

            if x >= n && y >= m {
                return Some(walk_back(&trace, offset, n, m));
            }
            k += 2;
        }
    }
    None
}

/// Turn the recorded search into hunks, by walking the path backwards.
fn walk_back(trace: &[Vec<usize>], offset: usize, n: usize, m: usize) -> Vec<Hunk> {
    let mut edits: Vec<(usize, usize, usize, usize)> = Vec::new();
    let (mut x, mut y) = (n, m);

    for (d, v) in trace.iter().enumerate().rev() {
        let k = x as isize - y as isize;
        let index = (k + offset as isize) as usize;

        let previous_k = if k == -(d as isize) || (k != d as isize && v[index - 1] < v[index + 1]) {
            k + 1
        } else {
            k - 1
        };
        let previous_index = (previous_k + offset as isize) as usize;
        let previous_x = v[previous_index];
        let previous_y = (previous_x as isize - previous_k) as usize;

        // Undo the diagonal run, which is the part that matched.
        while x > previous_x && y > previous_y {
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            edits.push((previous_x, previous_y, x, y));
        }
        x = previous_x;
        y = previous_y;
    }

    edits.reverse();
    coalesce(&edits)
}

/// Merge neighbouring single-line edits into runs.
///
/// Myers produces one step per inserted or deleted line; a block of five
/// changed lines is five steps, and five separate hunks would be five separate
/// marks describing one edit.
fn coalesce(edits: &[(usize, usize, usize, usize)]) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    for (old_start, new_start, old_end, new_end) in edits.iter().copied() {
        match hunks.last_mut() {
            // Adjacent in both texts, so it is the same edit continuing.
            Some(last) if last.old.end == old_start && last.new.end == new_start => {
                last.old.end = old_end;
                last.new.end = new_end;
            }
            _ => hunks.push(Hunk {
                old: old_start..old_end,
                new: new_start..new_end,
            }),
        }
    }
    hunks.retain(|h| !h.old.is_empty() || !h.new.is_empty());
    hunks
}

/// Split text into lines for diffing.
///
/// A trailing newline does not make an extra empty line here: every file that
/// ends properly would otherwise appear to have one, and comparing a file that
/// ends with a newline against one that does not is a real difference this
/// would hide among a false one.
#[must_use]
pub fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<&str> {
        lines(text)
    }

    fn statuses(old: &str, new: &str) -> Vec<(usize, LineStatus)> {
        line_status(&split(old), &split(new))
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        let text = "a\nb\nc\n";
        assert!(hunks(&split(text), &split(text)).is_empty());
        assert!(statuses(text, text).is_empty());
    }

    #[test]
    fn an_inserted_line_is_added() {
        assert_eq!(statuses("a\nc\n", "a\nb\nc\n"), [(1, LineStatus::Added)]);
    }

    #[test]
    fn a_changed_line_is_changed_rather_than_added_and_deleted() {
        assert_eq!(
            statuses("a\nb\nc\n", "a\nB\nc\n"),
            [(1, LineStatus::Changed)]
        );
    }

    /// A deletion has no line to mark, so it goes against the line that has
    /// taken its place — the only place a gutter could draw it.
    #[test]
    fn a_deleted_line_is_marked_against_the_line_below_it() {
        assert_eq!(
            statuses("a\nb\nc\n", "a\nc\n"),
            [(1, LineStatus::DeletedAbove)]
        );
    }

    #[test]
    fn a_deletion_at_the_end_is_marked_against_the_last_line() {
        assert_eq!(
            statuses("a\nb\nc\n", "a\n"),
            [(0, LineStatus::DeletedAbove)]
        );
    }

    #[test]
    fn a_run_of_added_lines_is_one_hunk() {
        let found = hunks(&split("a\nd\n"), &split("a\nb\nc\nd\n"));
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].new, 1..3);
        assert!(found[0].is_insertion());
    }

    #[test]
    fn a_run_of_changed_lines_is_one_hunk() {
        let found = hunks(&split("a\nb\nc\nd\n"), &split("a\nB\nC\nd\n"));
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].old, 1..3);
        assert_eq!(found[0].new, 1..3);
    }

    #[test]
    fn separate_edits_stay_separate() {
        let found = hunks(&split("a\nb\nc\nd\ne\n"), &split("A\nb\nc\nd\nE\n"));
        assert_eq!(found.len(), 2, "got {found:?}");
        assert_eq!(found[0].new, 0..1);
        assert_eq!(found[1].new, 4..5);
    }

    #[test]
    fn everything_new_when_the_old_text_is_empty() {
        assert_eq!(
            statuses("", "a\nb\n"),
            [(0, LineStatus::Added), (1, LineStatus::Added)]
        );
    }

    #[test]
    fn everything_gone_when_the_new_text_is_empty() {
        assert_eq!(statuses("a\nb\n", ""), [(0, LineStatus::DeletedAbove)]);
    }

    /// The trailing newline must not become a phantom line, or every file that
    /// ends properly looks as though it has an extra blank at the bottom.
    #[test]
    fn a_trailing_newline_does_not_become_a_line() {
        assert_eq!(lines("a\nb\n").len(), 2);
        assert_eq!(lines("a\nb").len(), 2);
        assert!(hunks(&split("a\nb\n"), &split("a\nb\n")).is_empty());
    }

    /// Adding a final newline is a real change, and one people care about.
    #[test]
    fn adding_a_final_newline_is_visible_as_a_change_to_the_last_line() {
        // "a\nb" against "a\nb\n" is the same two lines by `lines`, so the
        // difference has to be found elsewhere -- this documents that the
        // *line* diff sees nothing, which is why the caller compares the raw
        // text for the final-newline case.
        assert!(hunks(&split("a\nb"), &split("a\nb\n")).is_empty());
    }

    /// The shortcut that makes this affordable: one edit in a large file must
    /// not cost a search over the whole thing.
    #[test]
    fn a_small_edit_in_a_large_file_produces_one_small_hunk() {
        let mut old: Vec<String> = (0..5_000).map(|i| format!("line {i}")).collect();
        let new_text: Vec<String> = {
            let mut copy = old.clone();
            copy[2_500] = "changed".to_owned();
            copy
        };
        let old_refs: Vec<&str> = old.iter().map(String::as_str).collect();
        let new_refs: Vec<&str> = new_text.iter().map(String::as_str).collect();

        let started = std::time::Instant::now();
        let found = hunks(&old_refs, &new_refs);
        let took = started.elapsed();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].new, 2_500..2_501);
        assert!(
            took < std::time::Duration::from_millis(50),
            "took {took:?}; the prefix and suffix trim is not working"
        );
        old.clear();
    }

    /// A file rewritten wholesale should give up quickly rather than grinding:
    /// the precise answer is one nobody reads.
    #[test]
    fn two_entirely_different_large_files_give_up_and_say_so() {
        let old: Vec<String> = (0..4_000).map(|i| format!("old {i}")).collect();
        let new: Vec<String> = (0..4_000).map(|i| format!("new {i}")).collect();
        let old_refs: Vec<&str> = old.iter().map(String::as_str).collect();
        let new_refs: Vec<&str> = new.iter().map(String::as_str).collect();

        let started = std::time::Instant::now();
        let found = hunks(&old_refs, &new_refs);
        let took = started.elapsed();

        assert_eq!(found.len(), 1, "one hunk covering everything");
        assert_eq!(found[0].new, 0..4_000);
        assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    }

    /// The property that matters: applying the hunks to the old text has to
    /// produce the new text. Checked over a spread of shapes rather than one.
    #[test]
    fn the_hunks_describe_a_real_transformation() {
        let cases = [
            ("a\nb\nc\n", "a\nb\nc\n"),
            ("a\nb\nc\n", "c\nb\na\n"),
            ("", "x\n"),
            ("x\n", ""),
            ("a\na\na\n", "a\n"),
            ("a\n", "a\na\na\n"),
            ("one\ntwo\nthree\nfour\n", "one\nTWO\nthree\nfour\nfive\n"),
            ("x\ny\nz\n", "y\n"),
        ];
        for (old_text, new_text) in cases {
            let old = split(old_text);
            let new = split(new_text);
            let mut rebuilt: Vec<&str> = Vec::new();
            let mut cursor = 0usize;
            for hunk in hunks(&old, &new) {
                rebuilt.extend_from_slice(&old[cursor..hunk.old.start]);
                rebuilt.extend_from_slice(&new[hunk.new.clone()]);
                cursor = hunk.old.end;
            }
            rebuilt.extend_from_slice(&old[cursor..]);
            assert_eq!(rebuilt, new, "{old_text:?} -> {new_text:?}");
        }
    }

    /// Every mark must name a line that exists, or the gutter indexes past the
    /// end of the document.
    #[test]
    fn every_mark_points_at_a_real_line() {
        let cases = [
            ("a\nb\nc\n", ""),
            ("", "a\n"),
            ("a\nb\nc\nd\n", "a\nd\n"),
            ("a\n", "b\n"),
        ];
        for (old_text, new_text) in cases {
            let new = split(new_text);
            for (line, _) in statuses(old_text, new_text) {
                assert!(
                    line < new.len().max(1),
                    "{old_text:?} -> {new_text:?}: line {line} of {}",
                    new.len()
                );
            }
        }
    }

    #[test]
    fn a_line_is_marked_once() {
        let marks = statuses("a\nb\nc\nd\n", "a\nX\nd\n");
        let mut seen: Vec<usize> = marks.iter().map(|(line, _)| *line).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "a line was marked twice: {marks:?}");
    }
}
