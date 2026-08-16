//! The branches, and how each one stands against the one it tracks.
//!
//! `git for-each-ref`, with the same control-character separators the log uses
//! and for the same reason: a branch name may not contain a space, but it may
//! contain almost anything else, and an upstream's name is a branch name with a
//! remote's name in front of it.
//!
//! "Ahead" and "behind" come from `%(upstream:track)`, which git renders as
//! `[ahead 3, behind 1]`. That is a human-readable field being read by a
//! machine, which is normally a mistake — the alternative, `%(ahead-behind:)`,
//! needs git 2.41, and the bracket form has been stable since 2011. Parsing it
//! loosely, so an unfamiliar phrase costs the counts rather than the branch, is
//! the compromise.

/// Field separator, `U+001F`. Matches [`crate::log`].
const FIELD: char = '\u{1f}';
/// Record separator, `U+001E`.
const RECORD: char = '\u{1e}';

/// The `--format` git is asked for, in the order [`parse`] reads it.
pub const FORMAT: &str =
    "%(refname:short)%1f%(HEAD)%1f%(upstream:short)%1f%(upstream:track)%1f%(objectname:short)%1e";

/// One branch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branch {
    /// As git spells it: `trial`, or `origin/trial` for a remote-tracking one.
    pub name: String,
    /// Whether this is the branch HEAD is on.
    pub is_head: bool,
    /// The branch this one tracks, if any.
    pub upstream: Option<String>,
    /// Commits this branch has that its upstream does not.
    pub ahead: usize,
    /// Commits its upstream has that this branch does not.
    pub behind: usize,
    /// The upstream has been deleted from the remote.
    pub upstream_gone: bool,
    /// The abbreviated object name it points at.
    pub short: String,
    /// Whether this came out of `refs/remotes` rather than `refs/heads`.
    ///
    /// Set from what was asked for, not from the name: a local branch called
    /// `origin/thing` is legal and looks identical.
    remote: bool,
}

impl Branch {
    /// Whether this is a remote-tracking branch rather than a local one.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// Whether there is anything to say about this branch's relation to its
    /// upstream.
    #[must_use]
    pub fn is_diverged(&self) -> bool {
        self.ahead > 0 && self.behind > 0
    }

    /// `↑2 ↓1`, or nothing when the branch is level with its upstream.
    ///
    /// Arrows rather than words because this goes in a status bar. The symbols
    /// are the ones [`crate::branch`]'s caller has checked against the fonts.
    #[must_use]
    pub fn track_summary(&self, ahead: &str, behind: &str) -> String {
        if self.upstream_gone {
            return "upstream gone".to_owned();
        }
        let mut text = String::new();
        if self.ahead > 0 {
            text.push_str(&format!("{ahead}{}", self.ahead));
        }
        if self.behind > 0 {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!("{behind}{}", self.behind));
        }
        text
    }
}

/// Parse the output of `git for-each-ref --format=`[`FORMAT`].
///
/// `remote` says which namespace was asked for, because a ref's name does not
/// say: a local branch called `origin/thing` is legal and indistinguishable.
#[must_use]
pub fn parse(output: &str, remote: bool) -> Vec<Branch> {
    let mut branches = Vec::new();

    for record in output.split(RECORD) {
        let record = record.trim_start_matches(['\n', '\r']);
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split(FIELD);
        let mut next = || fields.next().unwrap_or_default().trim().to_owned();

        let name = next();
        let head = next();
        let upstream = next();
        let track = next();
        let short = next();

        if name.is_empty() {
            continue;
        }
        // A remote's symbolic HEAD — `origin/HEAD` — is a pointer at another
        // branch in the list, not a branch of its own, and offering it for
        // checkout would put the user on a detached HEAD for no reason.
        if remote && name.ends_with("/HEAD") {
            continue;
        }

        let (ahead, behind, gone) = parse_track(&track);
        branches.push(Branch {
            name,
            // git writes `*` for the checked-out branch and a space otherwise,
            // and the space is trimmed away above.
            is_head: head == "*",
            upstream: (!upstream.is_empty()).then_some(upstream),
            ahead,
            behind,
            upstream_gone: gone,
            short,
            remote,
        });
    }

    branches
}

/// Read `[ahead 3, behind 1]`, `[gone]`, or nothing.
///
/// Anything unfamiliar yields zeroes rather than an error: the branch is still
/// worth listing, and being wrong about a count is a smaller failure than
/// dropping the row it belongs to.
fn parse_track(track: &str) -> (usize, usize, bool) {
    let inner = track
        .trim()
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .unwrap_or_default();
    if inner.is_empty() {
        return (0, 0, false);
    }
    if inner.trim() == "gone" {
        return (0, 0, true);
    }

    let mut ahead = 0;
    let mut behind = 0;
    for part in inner.split(',') {
        let mut words = part.split_whitespace();
        match (words.next(), words.next()) {
            (Some("ahead"), Some(count)) => ahead = count.parse().unwrap_or(0),
            (Some("behind"), Some(count)) => behind = count.parse().unwrap_or(0),
            _ => {}
        }
    }
    (ahead, behind, false)
}

/// Whether `name` is something git will accept as a branch name.
///
/// Checked before asking, so a mistyped name is refused with a sentence rather
/// than with `fatal: 'x..y' is not a valid branch name`. This is deliberately
/// stricter than git in one respect — no leading dash — because a name starting
/// with one would be read as an option however carefully it is passed.
#[must_use]
pub fn is_valid_name(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Some("A branch needs a name.");
    }
    if name.starts_with('-') {
        return Some("A branch name cannot start with a dash.");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Some("A branch name cannot start or end with a slash, or contain an empty part.");
    }
    if name.ends_with('.') || name.contains("..") {
        return Some("A branch name cannot contain '..' or end with a dot.");
    }
    if name.ends_with(".lock") {
        return Some("A branch name cannot end with '.lock'.");
    }
    if name == "@" || name.contains("@{") {
        return Some("A branch name cannot contain '@{' or be '@' alone.");
    }
    if let Some(bad) = name
        .chars()
        .find(|c| " ~^:?*[\\".contains(*c) || c.is_control())
    {
        return Some(match bad {
            ' ' => "A branch name cannot contain spaces.",
            _ => "A branch name cannot contain ~ ^ : ? * [ or a backslash.",
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fields: &[&str]) -> String {
        let mut text = fields.join(&FIELD.to_string());
        text.push(RECORD);
        text.push('\n');
        text
    }

    #[test]
    fn an_empty_listing_has_no_branches() {
        assert_eq!(parse("", false), []);
        assert_eq!(parse("\n", false), []);
    }

    #[test]
    fn the_checked_out_branch_is_marked() {
        let text = format!(
            "{}{}",
            record(&["trial", "*", "origin/trial", "", "a1b2c3d"]),
            record(&["other", " ", "", "", "9f8e7d6"])
        );
        let branches = parse(&text, false);
        assert_eq!(branches.len(), 2);
        assert!(branches[0].is_head);
        assert_eq!(branches[0].name, "trial");
        assert_eq!(branches[0].upstream.as_deref(), Some("origin/trial"));
        assert_eq!(branches[0].short, "a1b2c3d");
        assert!(!branches[1].is_head);
        assert_eq!(branches[1].upstream, None);
    }

    #[test]
    fn ahead_and_behind_are_read_from_the_track_field() {
        for (track, expected) in [
            ("", (0, 0, false)),
            ("[ahead 3]", (3, 0, false)),
            ("[behind 2]", (0, 2, false)),
            ("[ahead 3, behind 1]", (3, 1, false)),
            ("[gone]", (0, 0, true)),
        ] {
            assert_eq!(parse_track(track), expected, "for {track:?}");
        }
    }

    /// A phrase we do not recognise costs the counts, not the branch.
    #[test]
    fn an_unfamiliar_track_phrase_still_lists_the_branch() {
        let text = record(&["trial", "*", "origin/trial", "[something new]", "abc"]);
        let branches = parse(&text, false);
        assert_eq!(branches.len(), 1);
        assert_eq!((branches[0].ahead, branches[0].behind), (0, 0));
        assert!(!branches[0].upstream_gone);
    }

    #[test]
    fn a_deleted_upstream_says_so() {
        let text = record(&["trial", "*", "origin/trial", "[gone]", "abc"]);
        let branch = &parse(&text, false)[0];
        assert!(branch.upstream_gone);
        assert_eq!(
            branch.track_summary("\u{2191}", "\u{2193}"),
            "upstream gone"
        );
    }

    #[test]
    fn the_summary_is_empty_when_there_is_nothing_to_say() {
        let text = record(&["trial", "*", "origin/trial", "", "abc"]);
        assert_eq!(parse(&text, false)[0].track_summary("^", "v"), "");
    }

    #[test]
    fn the_summary_shows_both_directions_when_diverged() {
        let text = record(&["trial", "*", "origin/trial", "[ahead 2, behind 5]", "abc"]);
        let branch = &parse(&text, false)[0];
        assert_eq!(branch.track_summary("^", "v"), "^2 v5");
        assert!(branch.is_diverged());
    }

    #[test]
    fn a_branch_only_ahead_is_not_diverged() {
        let text = record(&["trial", "*", "origin/trial", "[ahead 2]", "abc"]);
        let branch = &parse(&text, false)[0];
        assert_eq!(branch.track_summary("^", "v"), "^2");
        assert!(!branch.is_diverged());
    }

    /// `origin/HEAD` is a pointer at another row of the same list, and offering
    /// it for checkout would land the user on a detached HEAD for no reason.
    #[test]
    fn a_remotes_symbolic_head_is_not_listed() {
        let text = format!(
            "{}{}",
            record(&["origin/HEAD", " ", "", "", "abc"]),
            record(&["origin/trial", " ", "", "", "abc"])
        );
        let branches = parse(&text, true);
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].name, "origin/trial");
        assert!(branches[0].is_remote());
    }

    /// A ref's name does not say which namespace it came from — a local branch
    /// called `origin/thing` is legal and looks identical.
    #[test]
    fn whether_a_branch_is_remote_comes_from_what_was_asked_for() {
        let text = record(&["origin/thing", " ", "", "", "abc"]);
        assert!(!parse(&text, false)[0].is_remote());
        assert!(parse(&text, true)[0].is_remote());
    }

    #[test]
    fn ordinary_branch_names_are_accepted() {
        for good in [
            "main",
            "trial",
            "feature/thing",
            "fix-123",
            "release/1.0.0",
            "user.name/topic",
        ] {
            assert_eq!(is_valid_name(good), None, "rejected {good:?}");
        }
    }

    #[test]
    fn names_git_would_refuse_are_refused_first_with_a_sentence() {
        for bad in [
            "",
            "   ",
            "-x",
            "/leading",
            "trailing/",
            "a//b",
            "a..b",
            "ends.",
            "a.lock",
            "@",
            "a@{0}",
            "has space",
            "tilde~",
            "caret^",
            "colon:",
            "question?",
            "star*",
            "bracket[",
            "back\\slash",
        ] {
            assert!(is_valid_name(bad).is_some(), "should have refused {bad:?}");
        }
    }

    /// The one place this is stricter than git, and on purpose: a name starting
    /// with a dash would be read as an option however carefully it is passed.
    #[test]
    fn a_leading_dash_is_refused_by_name() {
        let complaint = is_valid_name("-force").expect("refused");
        assert!(complaint.contains("dash"), "got {complaint:?}");
    }

    #[test]
    fn the_complaint_names_the_problem() {
        assert!(
            is_valid_name("has space")
                .expect("refused")
                .contains("spaces")
        );
        assert!(is_valid_name("").expect("refused").contains("needs a name"));
    }
}
