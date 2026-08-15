//! A unified diff: the hunks, with enough surrounding lines to read them by.
//!
//! [`crate::diff`] answers "which lines differ", which is all the gutter needs.
//! Showing a diff needs more: the lines themselves, the unchanged ones around
//! them for context, and the numbering on both sides so a line on screen can be
//! found in the file. That is what this builds.
//!
//! The shape is git's, deliberately. Anyone who reads diffs knows what `@@` and
//! a leading `-` mean, and inventing a clearer notation for an audience that
//! already has one is not an improvement.

use crate::diff::{Hunk, hunks};

/// What happened to one line of a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Unchanged, shown for context.
    Context,
    /// In the buffer, not in the committed version.
    Added,
    /// In the committed version, not in the buffer.
    Removed,
}

/// One line of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub mark: Mark,
    /// Its one-based number in the committed version, if it has one.
    pub old: Option<usize>,
    /// Its one-based number in the buffer, if it has one.
    pub new: Option<usize>,
    pub text: String,
}

/// A run of changes and the context around it — one `@@` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The `@@ -a,b +c,d @@` header, as git would write it.
    pub header: String,
    pub lines: Vec<Line>,
}

/// How many unchanged lines to show either side of a change.
pub const DEFAULT_CONTEXT: usize = 3;

/// Build the unified diff of `new` against `old`.
///
/// Returns nothing at all when the two are identical, which is the difference
/// between "no changes" and "one section containing no changes" — and the
/// caller needs to say those differently.
#[must_use]
pub fn unified(old: &[&str], new: &[&str], context: usize) -> Vec<Section> {
    let found = hunks(old, new);
    if found.is_empty() {
        return Vec::new();
    }

    group(&found, context, old.len())
        .into_iter()
        .map(|group| section(&found[group], old, new, context))
        .collect()
}

/// Split the hunks into groups that will each become one `@@` section.
///
/// Two hunks close enough that their context would overlap belong together:
/// printing them separately would print the lines between them twice, and a
/// reader would have to work out that the two `@@` blocks describe one region.
fn group(hunks: &[Hunk], context: usize, old_len: usize) -> Vec<std::ops::Range<usize>> {
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for (index, hunk) in hunks.iter().enumerate() {
        let joins = groups.last().is_some_and(|last| {
            let previous = &hunks[last.end - 1];
            // The gap between them, measured on the old side, where every line
            // is a line both versions agree about.
            let gap = hunk.old.start.saturating_sub(previous.old.end);
            gap <= context * 2
        });
        if joins {
            if let Some(last) = groups.last_mut() {
                last.end = index + 1;
            }
        } else {
            groups.push(index..index + 1);
        }
    }
    debug_assert!(
        hunks.iter().all(|h| h.old.end <= old_len),
        "a hunk should never reach past the text it came from"
    );
    groups
}

/// Render one group of hunks as a section.
fn section(group: &[Hunk], old: &[&str], new: &[&str], context: usize) -> Section {
    let first = group.first().expect("a group holds at least one hunk");
    let last = group.last().expect("a group holds at least one hunk");

    // The leading context cannot reach further back than there are lines, and
    // it is the same number of lines on both sides — everything before the
    // first hunk is common to both versions.
    let lead = context.min(first.old.start).min(first.new.start);
    let mut old_cursor = first.old.start - lead;
    let mut new_cursor = first.new.start - lead;
    let old_from = old_cursor;
    let new_from = new_cursor;

    let mut lines = Vec::new();

    for hunk in group {
        // Context between the previous hunk and this one.
        while old_cursor < hunk.old.start {
            lines.push(Line {
                mark: Mark::Context,
                old: Some(old_cursor + 1),
                new: Some(new_cursor + 1),
                text: old[old_cursor].to_owned(),
            });
            old_cursor += 1;
            new_cursor += 1;
        }
        // Removals before additions, which is the order every diff tool uses:
        // it reads as "this became that".
        for index in hunk.old.clone() {
            lines.push(Line {
                mark: Mark::Removed,
                old: Some(index + 1),
                new: None,
                text: old[index].to_owned(),
            });
        }
        for index in hunk.new.clone() {
            lines.push(Line {
                mark: Mark::Added,
                old: None,
                new: Some(index + 1),
                text: new[index].to_owned(),
            });
        }
        old_cursor = hunk.old.end;
        new_cursor = hunk.new.end;
    }

    // Trailing context, bounded by both texts running out.
    let trail_end = (last.old.end + context).min(old.len());
    while old_cursor < trail_end && new_cursor < new.len() {
        lines.push(Line {
            mark: Mark::Context,
            old: Some(old_cursor + 1),
            new: Some(new_cursor + 1),
            text: old[old_cursor].to_owned(),
        });
        old_cursor += 1;
        new_cursor += 1;
    }

    let old_count = old_cursor - old_from;
    let new_count = new_cursor - new_from;
    Section {
        // Git numbers an empty range from the line before it, which is why the
        // `+ 1` is conditional. Copying that exactly matters: these headers get
        // pasted into `git apply`.
        header: format!(
            "@@ -{},{} +{},{} @@",
            if old_count == 0 {
                old_from
            } else {
                old_from + 1
            },
            old_count,
            if new_count == 0 {
                new_from
            } else {
                new_from + 1
            },
            new_count
        ),
        lines,
    }
}

/// How many lines were added and removed across a whole diff.
#[must_use]
pub fn totals(sections: &[Section]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in sections.iter().flat_map(|s| &s.lines) {
        match line.mark {
            Mark::Added => added += 1,
            Mark::Removed => removed += 1,
            Mark::Context => {}
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::lines;

    fn diff(old: &str, new: &str, context: usize) -> Vec<Section> {
        unified(&lines(old), &lines(new), context)
    }

    /// The diff as text, in git's notation, for readable assertions.
    fn render(sections: &[Section]) -> String {
        let mut out = String::new();
        for section in sections {
            out.push_str(&section.header);
            out.push('\n');
            for line in &section.lines {
                out.push(match line.mark {
                    Mark::Context => ' ',
                    Mark::Added => '+',
                    Mark::Removed => '-',
                });
                out.push_str(&line.text);
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn identical_texts_produce_no_sections() {
        assert!(diff("a\nb\nc\n", "a\nb\nc\n", 3).is_empty());
    }

    #[test]
    fn one_added_line_with_its_context() {
        let sections = diff("a\nb\nc\n", "a\nb\nNEW\nc\n", 1);
        assert_eq!(
            render(&sections),
            "@@ -2,2 +2,3 @@\n b\n+NEW\n c\n",
            "the added line, one line of context either side"
        );
    }

    #[test]
    fn one_removed_line() {
        let sections = diff("a\nb\nc\n", "a\nc\n", 1);
        assert_eq!(render(&sections), "@@ -1,3 +1,2 @@\n a\n-b\n c\n");
    }

    #[test]
    fn a_changed_line_reads_as_a_removal_then_an_addition() {
        let sections = diff("a\nb\nc\n", "a\nB\nc\n", 1);
        assert_eq!(render(&sections), "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
    }

    /// The numbering is the point of the exercise: a line on screen has to be
    /// findable in the file it came from.
    #[test]
    fn every_line_carries_the_numbers_it_has() {
        let sections = diff("a\nb\nc\n", "a\nB\nc\n", 1);
        let lines = &sections[0].lines;
        assert_eq!((lines[0].old, lines[0].new), (Some(1), Some(1)));
        assert_eq!(
            (lines[1].old, lines[1].new),
            (Some(2), None),
            "a removed line has no place in the buffer"
        );
        assert_eq!(
            (lines[2].old, lines[2].new),
            (None, Some(2)),
            "an added line has no place in the committed version"
        );
        assert_eq!((lines[3].old, lines[3].new), (Some(3), Some(3)));
    }

    #[test]
    fn distant_changes_become_separate_sections() {
        let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let mut changed: Vec<String> = old.lines().map(str::to_owned).collect();
        changed[2] = "CHANGED".to_owned();
        changed[30] = "ALSO CHANGED".to_owned();
        let new: String = changed.iter().map(|l| format!("{l}\n")).collect();

        let sections = diff(&old, &new, 3);
        assert_eq!(sections.len(), 2, "two edits, far apart, two sections");
    }

    #[test]
    fn nearby_changes_share_one_section() {
        let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let mut changed: Vec<String> = old.lines().map(str::to_owned).collect();
        changed[10] = "CHANGED".to_owned();
        changed[12] = "ALSO CHANGED".to_owned();
        let new: String = changed.iter().map(|l| format!("{l}\n")).collect();

        let sections = diff(&old, &new, 3);
        assert_eq!(
            sections.len(),
            1,
            "their context overlaps, so printing them apart would repeat lines"
        );
        // And nothing is printed twice.
        let context: Vec<_> = sections[0]
            .lines
            .iter()
            .filter(|l| l.mark == Mark::Context)
            .map(|l| l.old)
            .collect();
        let mut sorted = context.clone();
        sorted.dedup();
        assert_eq!(context, sorted, "a context line appears once");
    }

    #[test]
    fn a_change_at_the_very_start_has_no_leading_context() {
        let sections = diff("a\nb\nc\n", "A\nb\nc\n", 3);
        assert_eq!(render(&sections), "@@ -1,3 +1,3 @@\n-a\n+A\n b\n c\n");
    }

    #[test]
    fn a_change_at_the_very_end_has_no_trailing_context() {
        let sections = diff("a\nb\nc\n", "a\nb\nC\n", 3);
        assert_eq!(render(&sections), "@@ -1,3 +1,3 @@\n a\n b\n-c\n+C\n");
    }

    #[test]
    fn everything_added_to_an_empty_file() {
        let sections = diff("", "a\nb\n", 3);
        assert_eq!(render(&sections), "@@ -0,0 +1,2 @@\n+a\n+b\n");
    }

    #[test]
    fn everything_removed() {
        let sections = diff("a\nb\n", "", 3);
        assert_eq!(render(&sections), "@@ -1,2 +0,0 @@\n-a\n-b\n");
    }

    #[test]
    fn the_totals_count_what_changed_and_not_the_context() {
        let sections = diff("a\nb\nc\nd\n", "a\nB\nc\nD\nE\n", 3);
        assert_eq!(totals(&sections), (3, 2), "B, D and E added; b and d gone");
    }

    /// Applying the diff to the old text has to reproduce the new one. If that
    /// does not hold, the numbering or the context is wrong somewhere and every
    /// other assertion here could still pass.
    #[test]
    fn the_sections_describe_a_real_transformation() {
        let old: String = (0..60).map(|i| format!("line {i}\n")).collect();
        let mut changed: Vec<String> = old.lines().map(str::to_owned).collect();
        changed.remove(5);
        changed[20] = "CHANGED".to_owned();
        changed.insert(40, "INSERTED".to_owned());
        let new: String = changed.iter().map(|l| format!("{l}\n")).collect();

        let old_lines = lines(&old);
        let new_lines = lines(&new);
        let sections = unified(&old_lines, &new_lines, 3);

        // Every line the diff claims is in the buffer must be there, at the
        // number it claims, and likewise for the committed side.
        for line in sections.iter().flat_map(|s| &s.lines) {
            if let Some(n) = line.new {
                assert_eq!(new_lines[n - 1], line.text, "buffer line {n}");
            }
            if let Some(n) = line.old {
                assert_eq!(old_lines[n - 1], line.text, "committed line {n}");
            }
        }
        // And every changed line is accounted for: "CHANGED" and "INSERTED"
        // added, "line 5" and the line "CHANGED" replaced taken away.
        assert_eq!(totals(&sections), (2, 2));
    }
}
