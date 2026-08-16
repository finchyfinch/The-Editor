//! Who last touched each line, and when.
//!
//! `git blame --porcelain`, which is the form meant for machines. It states
//! each commit's details *once* and then refers back to it by object name, so
//! a file whose thousand lines came from four commits carries four sets of
//! headers rather than a thousand. `--line-porcelain` repeats them all and is
//! much easier to parse; on a large file it is also several times the output
//! for the same information.
//!
//! The shape is:
//!
//! ```text
//! <sha> <line in the original> <line in the final file> <how many lines follow>
//! author Gareth Finch
//! author-time 1755340000
//! summary Make the terminal a terminal
//! <tab>the actual line of code
//! ```
//!
//! Every field after the first line is optional and may be omitted on a repeat
//! of a commit already described. The line beginning with a tab is the content,
//! and it is what ends one line's entry and begins the next.

use std::collections::HashMap;

/// The object name git uses for "not committed yet".
const UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

/// What is known about the commit a line came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    pub id: String,
    pub author: String,
    /// Seconds since the epoch, as git writes it.
    pub time: i64,
    /// The first line of the commit's message.
    pub summary: String,
}

impl Origin {
    /// Whether this line is not in any commit yet.
    #[must_use]
    pub fn is_uncommitted(&self) -> bool {
        self.id == UNCOMMITTED || self.id.is_empty()
    }

    /// The object name, abbreviated the way git abbreviates one.
    #[must_use]
    pub fn short(&self) -> &str {
        self.id.get(..7).unwrap_or(&self.id)
    }

    /// `YYYY-MM-DD`, from the epoch seconds.
    ///
    /// Worked out here rather than with a date crate. This is a civil date in
    /// UTC from a Unix timestamp, which is thirty lines of arithmetic that
    /// cannot drift, against a dependency that would be pulled in for exactly
    /// this one string.
    #[must_use]
    pub fn date(&self) -> String {
        if self.is_uncommitted() {
            return String::new();
        }
        civil_date(self.time)
    }

    /// What to draw in the margin: who, and when.
    #[must_use]
    pub fn label(&self) -> String {
        if self.is_uncommitted() {
            return "Not committed".to_owned();
        }
        format!("{} {}", self.date(), self.author)
    }
}

/// One line of the file, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// One-based, in the file as it is now.
    pub number: usize,
    pub origin: Origin,
}

/// Parse the output of `git blame --porcelain`.
///
/// Returns one entry per line of the file, in order. A file git has nothing to
/// say about yields nothing, which the caller shows as no annotations rather
/// than as an error.
#[must_use]
pub fn parse(output: &str) -> Vec<Line> {
    // Commits described once and referred to afterwards.
    let mut known: HashMap<String, Origin> = HashMap::new();
    let mut lines = Vec::new();

    // The entry being assembled. `None` between entries.
    let mut current: Option<(String, usize)> = None;
    let mut pending = Origin::default();

    for raw in output.lines() {
        // The content line — a tab, then the source — ends the entry. Nothing
        // else in the format starts with a tab.
        if let Some(_content) = raw.strip_prefix('\t') {
            if let Some((id, number)) = current.take() {
                // Fill in from what was said earlier about this commit, then
                // record anything new said about it this time.
                let origin = known.entry(id.clone()).or_default();
                if origin.id.is_empty() {
                    origin.id = id;
                }
                if !pending.author.is_empty() {
                    origin.author.clone_from(&pending.author);
                }
                if pending.time != 0 {
                    origin.time = pending.time;
                }
                if !pending.summary.is_empty() {
                    origin.summary.clone_from(&pending.summary);
                }
                lines.push(Line {
                    number,
                    origin: origin.clone(),
                });
            }
            pending = Origin::default();
            continue;
        }

        // A header line inside an entry.
        if current.is_some() {
            if let Some(rest) = raw.strip_prefix("author ") {
                pending.author = rest.to_owned();
                continue;
            }
            if let Some(rest) = raw.strip_prefix("author-time ") {
                pending.time = rest.trim().parse().unwrap_or(0);
                continue;
            }
            if let Some(rest) = raw.strip_prefix("summary ") {
                pending.summary = rest.to_owned();
                continue;
            }
            // Everything else — committer, filename, boundary, previous — is
            // real and simply not needed here.
            continue;
        }

        // Otherwise this begins an entry: <sha> <orig> <final> [<count>].
        let mut parts = raw.split_whitespace();
        let (Some(id), Some(_original), Some(number)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // A 40-character hex name, or the run of zeros meaning "not committed".
        if id.len() != 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let Ok(number) = number.parse::<usize>() else {
            continue;
        };
        current = Some((id.to_owned(), number));
    }

    lines
}

/// Index a parsed blame by line number, for a gutter that asks per row.
///
/// One-based, matching [`Line::number`], with gaps where git said nothing.
#[must_use]
pub fn by_line(lines: &[Line]) -> HashMap<usize, Origin> {
    lines
        .iter()
        .map(|line| (line.number, line.origin.clone()))
        .collect()
}

/// `YYYY-MM-DD` in UTC, from seconds since the epoch.
///
/// Civil-from-days, the standard algorithm. Correct for every date this will
/// ever be handed and free of the leap-year edge cases a hand-rolled loop
/// collects.
fn civil_date(timestamp: i64) -> String {
    let days = timestamp.div_euclid(86_400);

    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";
    const OTHER: &str = "b1b2c3d4e5f60718293a4b5c6d7e8f9012345679";

    #[test]
    fn one_line_from_one_commit() {
        let output = format!(
            "{SHA} 1 1 1\n\
             author Gareth Finch\n\
             author-time 1755340000\n\
             author-tz +0100\n\
             summary Make the terminal a terminal\n\
             filename src/main.rs\n\
             \timport sys\n"
        );
        let lines = parse(&output);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].number, 1);
        assert_eq!(lines[0].origin.author, "Gareth Finch");
        assert_eq!(lines[0].origin.summary, "Make the terminal a terminal");
        assert_eq!(lines[0].origin.short(), "a1b2c3d");
        assert!(!lines[0].origin.is_uncommitted());
    }

    /// The whole point of `--porcelain` over `--line-porcelain`: the second
    /// line from the same commit carries no headers at all, and its details
    /// have to come from the first.
    #[test]
    fn a_repeated_commit_is_described_only_once() {
        let output = format!(
            "{SHA} 1 1 2\n\
             author Gareth Finch\n\
             author-time 1755340000\n\
             summary Only stated here\n\
             \tfirst line\n\
             {SHA} 2 2\n\
             \tsecond line\n"
        );
        let lines = parse(&output);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1].origin.author, "Gareth Finch",
            "the second line must inherit what the first said"
        );
        assert_eq!(lines[1].origin.summary, "Only stated here");
        assert_eq!(lines[1].number, 2);
    }

    #[test]
    fn two_commits_keep_their_own_details() {
        let output = format!(
            "{SHA} 1 1 1\n\
             author First Author\n\
             author-time 1000000000\n\
             summary the first\n\
             \tone\n\
             {OTHER} 1 2 1\n\
             author Second Author\n\
             author-time 1700000000\n\
             summary the second\n\
             \ttwo\n"
        );
        let lines = parse(&output);
        assert_eq!(lines[0].origin.author, "First Author");
        assert_eq!(lines[1].origin.author, "Second Author");
        assert_eq!(lines[1].origin.summary, "the second");
    }

    #[test]
    fn an_uncommitted_line_says_so() {
        let output = format!(
            "{UNCOMMITTED} 1 1 1\n\
             author Not Committed Yet\n\
             author-time 1755340000\n\
             summary Version of src/main.rs from src/main.rs\n\
             \tjust typed this\n"
        );
        let lines = parse(&output);
        assert!(lines[0].origin.is_uncommitted());
        assert_eq!(lines[0].origin.label(), "Not committed");
        assert_eq!(lines[0].origin.date(), "");
    }

    /// A line of source that itself starts with what looks like a header must
    /// not be read as one — the tab is the only thing that distinguishes them.
    #[test]
    fn source_that_looks_like_a_header_is_still_source() {
        let output = format!(
            "{SHA} 1 1 2\n\
             author Real Author\n\
             author-time 1755340000\n\
             summary real summary\n\
             \tauthor Fake Author\n\
             {SHA} 2 2\n\
             \tsummary not a summary\n"
        );
        let lines = parse(&output);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].origin.author, "Real Author",
            "a tab-prefixed line is content, whatever it says"
        );
        assert_eq!(lines[1].origin.summary, "real summary");
    }

    #[test]
    fn an_empty_blame_yields_nothing() {
        assert_eq!(parse(""), []);
        assert_eq!(parse("not the porcelain format at all\n"), []);
    }

    #[test]
    fn line_numbers_come_from_the_file_as_it_is_now() {
        let output = format!(
            "{SHA} 40 7 1\n\
             author A\n\
             author-time 1000000000\n\
             summary s\n\
             \tmoved down the file\n"
        );
        // 40 is where it was in the original; 7 is where it is now, and 7 is
        // the number the gutter draws beside.
        assert_eq!(parse(&output)[0].number, 7);
    }

    #[test]
    fn the_index_is_keyed_by_the_line_it_annotates() {
        let output = format!(
            "{SHA} 1 3 1\n\
             author A\n\
             author-time 1000000000\n\
             summary s\n\
             \tthird line\n"
        );
        let index = by_line(&parse(&output));
        assert!(index.contains_key(&3));
        assert!(!index.contains_key(&1), "1 was its number in the original");
    }

    #[test]
    fn the_label_is_the_date_and_the_author() {
        let origin = Origin {
            id: SHA.to_owned(),
            author: "Gareth Finch".to_owned(),
            // 2026-08-13T10:26:40Z
            time: 1_786_616_800,
            summary: "s".to_owned(),
        };
        assert_eq!(origin.label(), "2026-08-13 Gareth Finch");
    }

    /// Dates are arithmetic here rather than a dependency, so they are worth
    /// checking against known answers — including the ones leap years break.
    #[test]
    fn dates_are_right_including_the_awkward_ones() {
        for (timestamp, expected) in [
            (0_i64, "1970-01-01"),
            (86_399, "1970-01-01"),
            (86_400, "1970-01-02"),
            // The end of a leap year, and the leap day itself.
            (951_782_400, "2000-02-29"),
            (1_078_012_800, "2004-02-29"),
            // 1900 was not a leap year; 2000 was.
            (4_107_542_400, "2100-03-01"),
            (1_735_689_599, "2024-12-31"),
            (1_735_689_600, "2025-01-01"),
            // Before the epoch, which a rebased or imported history can have.
            (-1, "1969-12-31"),
            (-86_400, "1969-12-31"),
        ] {
            assert_eq!(civil_date(timestamp), expected, "for {timestamp}");
        }
    }
}
