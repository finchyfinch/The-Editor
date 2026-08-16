//! The commit history, as far back as anyone asked for.
//!
//! `git log --format=…`, with the fields separated by control characters rather
//! than by anything that could appear in a commit message. A subject containing
//! a tab, a pipe or a newline is ordinary — the first line of a squashed merge
//! is often several of those — and any separator a human would choose is one a
//! message can contain.
//!
//! So: unit separator between fields, record separator between commits. Neither
//! is legal in a git identity or a ref name, and a message containing one is
//! pathological enough that mangling it is acceptable where mangling a tab is
//! not.

/// Field separator, `U+001F`.
const FIELD: char = '\u{1f}';
/// Record separator, `U+001E`.
const RECORD: char = '\u{1e}';

/// The `--format` git is asked for.
///
/// In the order [`parse`] reads them. `%x1f` and `%x1e` are how a format string
/// spells a literal byte.
pub const FORMAT: &str = "%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%ar%x1f%P%x1f%s%x1e";

/// One commit, as much of it as a list needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// The full object name.
    pub id: String,
    /// The abbreviation git chose, which is as short as is unambiguous here.
    pub short: String,
    pub author: String,
    pub email: String,
    /// When it was authored, ISO 8601 with an offset.
    pub when: String,
    /// The same, as git phrases it: "3 days ago".
    pub relative: String,
    /// Parent object names. Two or more means a merge.
    pub parents: Vec<String>,
    /// The first line of the message.
    pub subject: String,
}

impl Commit {
    /// Whether this commit joined two or more histories.
    #[must_use]
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }

    /// The date alone, without the time, for a column that has to be narrow.
    ///
    /// Taken from the front of the ISO timestamp rather than parsed: this needs
    /// the ten characters git already formatted, and a date library to re-derive
    /// them would be a dependency bought for nothing.
    #[must_use]
    pub fn date(&self) -> &str {
        self.when.get(..10).unwrap_or(&self.when)
    }
}

/// Parse the output of `git log --format=`[`FORMAT`].
#[must_use]
pub fn parse(output: &str) -> Vec<Commit> {
    let mut commits = Vec::new();

    for record in output.split(RECORD) {
        // Git puts a newline after each record; it is not part of the next
        // commit's object name.
        let record = record.trim_start_matches(['\n', '\r']);
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split(FIELD);
        let mut next = || fields.next().unwrap_or_default().to_owned();

        let id = next();
        let short = next();
        let author = next();
        let email = next();
        let when = next();
        let relative = next();
        let parents = next();
        let subject = next();

        // A record with no object name is not a commit, however git produced it.
        if id.is_empty() {
            continue;
        }

        commits.push(Commit {
            id,
            short,
            author,
            email,
            when,
            relative,
            parents: parents.split_whitespace().map(str::to_owned).collect(),
            subject,
        });
    }

    commits
}

/// One commit in full: its message, and what it touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detail {
    /// The whole message, subject and body.
    pub message: String,
    /// What changed, as a status letter and a path.
    pub files: Vec<(crate::status::Change, String)>,
}

/// Parse `git show --name-status -z --format=`.
///
/// Records are a status letter and a path, NUL-separated. A rename or a copy
/// carries a similarity score on the letter (`R100`) and spends *two* path
/// fields — the old name and the new — which is the same trap as the status
/// listing and gets the same treatment.
#[must_use]
pub fn changed_files(output: &str) -> Vec<(crate::status::Change, String)> {
    let mut files = Vec::new();
    let mut fields = output.split('\0').filter(|f| !f.is_empty());

    while let Some(letter) = fields.next() {
        let Some(first) = letter.chars().next() else {
            continue;
        };
        let change = crate::status::Change::from_status_letter(first);
        let Some(path) = fields.next() else { break };
        if matches!(
            change,
            crate::status::Change::Renamed | crate::status::Change::Copied
        ) {
            // `path` was where it came from; the name it has now follows.
            match fields.next() {
                Some(to) => files.push((change, to.to_owned())),
                None => break,
            }
        } else {
            files.push((change, path.to_owned()));
        }
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build what git would have written, from the fields in order.
    fn record(fields: &[&str]) -> String {
        let mut text = fields.join(&FIELD.to_string());
        text.push(RECORD);
        text.push('\n');
        text
    }

    fn one(subject: &str) -> String {
        record(&[
            "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
            "a1b2c3d",
            "Gareth Finch",
            "someone@example.invalid",
            "2026-08-16T11:22:33+01:00",
            "2 hours ago",
            "0000000000000000000000000000000000000000",
            subject,
        ])
    }

    #[test]
    fn an_empty_log_has_no_commits() {
        assert_eq!(parse(""), []);
        assert_eq!(parse("\n"), []);
    }

    #[test]
    fn every_field_arrives_where_it_belongs() {
        let commits = parse(&one("Make the terminal a terminal"));
        assert_eq!(commits.len(), 1);
        let commit = &commits[0];
        assert_eq!(commit.id, "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678");
        assert_eq!(commit.short, "a1b2c3d");
        assert_eq!(commit.author, "Gareth Finch");
        assert_eq!(commit.email, "someone@example.invalid");
        assert_eq!(commit.when, "2026-08-16T11:22:33+01:00");
        assert_eq!(commit.relative, "2 hours ago");
        assert_eq!(commit.subject, "Make the terminal a terminal");
        assert_eq!(commit.date(), "2026-08-16");
        assert!(!commit.is_merge());
    }

    #[test]
    fn several_commits_keep_their_order() {
        let text = format!("{}{}", one("first"), one("second"));
        let commits = parse(&text);
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(
            subjects,
            ["first", "second"],
            "newest first, as git gave them"
        );
    }

    /// The reason the separators are control characters. All of these are
    /// perfectly ordinary things to put in a subject line.
    #[test]
    fn a_subject_containing_punctuation_survives_intact() {
        for awkward in [
            "Fix the | pipe handling",
            "Add\ta tab, somehow",
            "Use --format=%H, not %h",
            "Handle 'quotes' and \"quotes\"",
            "Refactor: A -> B",
        ] {
            let commits = parse(&one(awkward));
            assert_eq!(commits[0].subject, awkward, "mangled {awkward:?}");
        }
    }

    #[test]
    fn a_merge_says_it_is_one() {
        let text = record(&[
            "1111111111111111111111111111111111111111",
            "1111111",
            "Someone",
            "s@example.invalid",
            "2026-01-01T00:00:00+00:00",
            "8 months ago",
            "2222222222222222222222222222222222222222 3333333333333333333333333333333333333333",
            "Merge branch 'trial'",
        ]);
        let commit = &parse(&text)[0];
        assert!(commit.is_merge());
        assert_eq!(commit.parents.len(), 2);
    }

    #[test]
    fn the_first_commit_has_no_parents() {
        let text = record(&[
            "4444444444444444444444444444444444444444",
            "4444444",
            "Someone",
            "s@example.invalid",
            "2026-01-01T00:00:00+00:00",
            "8 months ago",
            "",
            "initial",
        ]);
        let commit = &parse(&text)[0];
        assert_eq!(commit.parents, Vec::<String>::new());
        assert!(!commit.is_merge());
    }

    /// A truncated record should cost the fields it is missing, not the ones
    /// before it, and certainly not a panic.
    #[test]
    fn a_short_record_keeps_what_it_has() {
        let text = format!("abc123{FIELD}abc{RECORD}\n");
        let commit = &parse(&text)[0];
        assert_eq!(commit.id, "abc123");
        assert_eq!(commit.short, "abc");
        assert_eq!(commit.subject, "");
    }

    #[test]
    fn a_record_with_no_object_name_is_not_a_commit() {
        let text = format!("{FIELD}{FIELD}{FIELD}{RECORD}\n");
        assert_eq!(parse(&text), []);
    }

    #[test]
    fn a_malformed_timestamp_still_yields_a_date_column() {
        let text = record(&["abc", "abc", "A", "a@b", "not-a-date", "?", "", "s"]);
        // Ten characters, or all of it if there are fewer — never a panic on a
        // string that turned out shorter than expected.
        assert_eq!(parse(&text)[0].date(), "not-a-date");
    }
}
