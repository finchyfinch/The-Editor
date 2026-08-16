//! What a test run found.
//!
//! One shape for both frameworks, because the panel should not care which one
//! produced a result — and because "re-run the failures" has to work the same
//! way whichever it was.
//!
//! Results arrive *as the run proceeds*, not at the end. That is the whole
//! reason these parsers read the runner's own output rather than a report file:
//! a suite that takes two minutes should fill the panel in as it goes, and a
//! JUnit XML written at the end cannot do that.

use std::path::PathBuf;

/// How a test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// Reported as failing.
    ///
    /// First in the ordering on purpose: sorting by outcome puts the failures
    /// at the top, which is the only part of a green-but-for-two run anyone
    /// wants to look at.
    Failed,
    /// Started and not yet reported. A run that dies half-way leaves these
    /// behind, which is more honest than quietly calling them passes.
    Running,
    /// Skipped, ignored, or expected to fail.
    Skipped,
    Passed,
}

impl Outcome {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Running => "running",
            Self::Skipped => "skipped",
            Self::Passed => "passed",
        }
    }
}

/// One test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// What the framework calls it, and what to hand back to run only this one.
    ///
    /// A pytest node id (`tests/test_a.py::TestB::test_c`) or a libtest path
    /// (`module::test_name`).
    pub id: String,
    /// Where it is, once something has said. A failure usually says; a pass
    /// never does.
    pub location: Option<Location>,
    pub outcome: Outcome,
    /// Why it failed, in the framework's own words. Empty for anything else.
    pub message: String,
}

impl Case {
    /// The part worth showing in a list, without the file path in front.
    ///
    /// `tests/test_a.py::TestB::test_c` becomes `TestB::test_c`; a libtest name
    /// is already short enough to show whole.
    #[must_use]
    pub fn short_name(&self) -> &str {
        match self.id.split_once("::") {
            Some((_, rest)) if self.id.contains(".py::") => rest,
            _ => &self.id,
        }
    }

    /// The file the test lives in, when the id says.
    #[must_use]
    pub fn file_hint(&self) -> Option<&str> {
        self.id.split_once("::").map(|(file, _)| file)
    }
}

/// A place in a file, as a framework reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Relative to wherever the run was started, usually.
    pub file: PathBuf,
    /// One-based, as every test runner and compiler counts them.
    pub line: u32,
}

/// Everything known about a run so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// In the order the runner reported them.
    pub cases: Vec<Case>,
    /// Set when the process exits, so "no failures yet" can be told from
    /// "nothing failed".
    pub finished: bool,
    /// Whatever the runner said that was not a result — a collection error, a
    /// compiler diagnostic, the reason there were no tests at all.
    pub notes: Vec<String>,
}

impl Report {
    /// Record a result, replacing any earlier one for the same test.
    ///
    /// Replacing matters: a test is often seen twice, once when it starts and
    /// again when it finishes, and two rows for one test is a bug the panel
    /// would show rather than hide.
    pub fn record(&mut self, case: Case) {
        match self
            .cases
            .iter_mut()
            .find(|existing| existing.id == case.id)
        {
            Some(existing) => *existing = case,
            None => self.cases.push(case),
        }
    }

    /// Attach a failure message and location to a test already recorded.
    ///
    /// The frameworks report *that* something failed early and *why* at the
    /// end, so the two halves arrive minutes apart and have to be joined by
    /// name.
    pub fn explain(&mut self, id: &str, message: String, location: Option<Location>) {
        if let Some(case) = self.cases.iter_mut().find(|c| c.id == id) {
            case.message = message;
            if location.is_some() {
                case.location = location;
            }
        }
    }

    /// Find a test whose id ends with `suffix`.
    ///
    /// pytest names a failure block `TestB.test_c` while the result line called
    /// it `tests/test_a.py::TestB::test_c`, so the two have to be matched by
    /// their tails rather than compared.
    #[must_use]
    pub fn id_ending_with(&self, suffix: &str) -> Option<String> {
        let wanted = suffix.replace('.', "::");
        self.cases
            .iter()
            .find(|c| c.id == wanted || c.id.ends_with(&format!("::{wanted}")))
            .map(|c| c.id.clone())
    }

    #[must_use]
    pub fn count(&self, outcome: Outcome) -> usize {
        self.cases.iter().filter(|c| c.outcome == outcome).count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }

    /// Every test that failed, for "run the failures again".
    #[must_use]
    pub fn failures(&self) -> Vec<String> {
        self.cases
            .iter()
            .filter(|c| c.outcome == Outcome::Failed)
            .map(|c| c.id.clone())
            .collect()
    }

    /// Mark anything still running as failed, once the process has gone.
    ///
    /// A run killed part-way through — a segfault, a stop button, a test that
    /// hung — leaves tests that started and never reported. Calling those
    /// passes would be a lie; leaving them "running" for ever is a puzzle.
    pub fn finish(&mut self) {
        self.finished = true;
        for case in &mut self.cases {
            if case.outcome == Outcome::Running {
                case.outcome = Outcome::Failed;
                if case.message.is_empty() {
                    case.message = "The run ended before this test reported.".to_owned();
                }
            }
        }
    }

    /// The one-line summary for the panel's header.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.cases.is_empty() {
            return if self.finished {
                "No tests were found.".to_owned()
            } else {
                "Looking for tests\u{2026}".to_owned()
            };
        }
        let failed = self.count(Outcome::Failed);
        let passed = self.count(Outcome::Passed);
        let skipped = self.count(Outcome::Skipped);

        let mut parts = Vec::new();
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        parts.push(format!("{passed} passed"));
        if skipped > 0 {
            parts.push(format!("{skipped} skipped"));
        }
        let mut text = parts.join(", ");
        if !self.finished {
            text.push_str("\u{2026} so far");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, outcome: Outcome) -> Case {
        Case {
            id: id.to_owned(),
            location: None,
            outcome,
            message: String::new(),
        }
    }

    #[test]
    fn a_new_report_has_nothing_in_it() {
        let report = Report::default();
        assert!(report.is_empty());
        assert!(!report.finished);
        assert_eq!(report.summary(), "Looking for tests\u{2026}");
    }

    #[test]
    fn an_empty_finished_run_says_it_found_nothing() {
        let mut report = Report::default();
        report.finish();
        assert_eq!(report.summary(), "No tests were found.");
    }

    /// A test seen twice — once starting, once finishing — is one row.
    #[test]
    fn recording_the_same_test_twice_replaces_it() {
        let mut report = Report::default();
        report.record(case("a", Outcome::Running));
        report.record(case("a", Outcome::Passed));
        assert_eq!(report.cases.len(), 1);
        assert_eq!(report.cases[0].outcome, Outcome::Passed);
    }

    #[test]
    fn the_summary_counts_each_kind() {
        let mut report = Report::default();
        report.record(case("a", Outcome::Passed));
        report.record(case("b", Outcome::Passed));
        report.record(case("c", Outcome::Failed));
        report.record(case("d", Outcome::Skipped));
        report.finished = true;
        assert_eq!(report.summary(), "1 failed, 2 passed, 1 skipped");
    }

    #[test]
    fn a_run_still_going_says_so() {
        let mut report = Report::default();
        report.record(case("a", Outcome::Passed));
        assert_eq!(report.summary(), "1 passed\u{2026} so far");
    }

    #[test]
    fn a_green_run_does_not_mention_failures_or_skips() {
        let mut report = Report::default();
        report.record(case("a", Outcome::Passed));
        report.finished = true;
        assert_eq!(report.summary(), "1 passed");
    }

    /// The two halves of a failure arrive minutes apart and are joined by name.
    #[test]
    fn a_failure_can_be_explained_after_the_fact() {
        let mut report = Report::default();
        report.record(case("tests/test_a.py::test_b", Outcome::Failed));
        report.explain(
            "tests/test_a.py::test_b",
            "assert 1 == 2".to_owned(),
            Some(Location {
                file: PathBuf::from("tests/test_a.py"),
                line: 5,
            }),
        );
        assert_eq!(report.cases[0].message, "assert 1 == 2");
        assert_eq!(report.cases[0].location.as_ref().expect("location").line, 5);
    }

    /// pytest names a failure block with dots where the node id had colons.
    #[test]
    fn a_failure_block_name_is_matched_back_to_its_node_id() {
        let mut report = Report::default();
        report.record(case("tests/test_a.py::TestB::test_c", Outcome::Failed));
        assert_eq!(
            report.id_ending_with("TestB.test_c").as_deref(),
            Some("tests/test_a.py::TestB::test_c")
        );
        assert_eq!(
            report.id_ending_with("test_c").as_deref(),
            Some("tests/test_a.py::TestB::test_c"),
            "the last segment alone should find it too"
        );
        assert_eq!(report.id_ending_with("test_nothing"), None);
    }

    /// A libtest name has no file in front of it and must match whole.
    #[test]
    fn a_libtest_name_matches_itself() {
        let mut report = Report::default();
        report.record(case("module::inner::a_test", Outcome::Failed));
        assert_eq!(
            report.id_ending_with("module::inner::a_test").as_deref(),
            Some("module::inner::a_test")
        );
    }

    /// A run that dies part-way leaves tests that started and never reported.
    #[test]
    fn finishing_turns_unreported_tests_into_failures() {
        let mut report = Report::default();
        report.record(case("a", Outcome::Passed));
        report.record(case("b", Outcome::Running));
        report.finish();

        assert_eq!(report.cases[1].outcome, Outcome::Failed);
        assert!(
            report.cases[1].message.contains("ended before"),
            "and says why: {:?}",
            report.cases[1].message
        );
        assert_eq!(report.cases[0].outcome, Outcome::Passed, "untouched");
    }

    #[test]
    fn the_failures_are_what_a_re_run_needs() {
        let mut report = Report::default();
        report.record(case("a", Outcome::Passed));
        report.record(case("b", Outcome::Failed));
        report.record(case("c", Outcome::Failed));
        assert_eq!(report.failures(), ["b", "c"]);
    }

    #[test]
    fn a_python_node_id_shows_without_its_file() {
        assert_eq!(
            case("tests/test_a.py::TestB::test_c", Outcome::Passed).short_name(),
            "TestB::test_c"
        );
        assert_eq!(
            case("tests/test_a.py::test_b", Outcome::Passed).file_hint(),
            Some("tests/test_a.py")
        );
    }

    /// A libtest name is already short, and cutting at its first `::` would
    /// throw away the module that makes it unique.
    #[test]
    fn a_libtest_name_is_shown_whole() {
        assert_eq!(
            case("module::inner::a_test", Outcome::Passed).short_name(),
            "module::inner::a_test"
        );
    }

    /// Sorting by outcome puts the failures first, which is the only part of a
    /// nearly-green run anyone reads.
    #[test]
    fn failures_sort_above_everything_else() {
        let mut outcomes = [
            Outcome::Passed,
            Outcome::Skipped,
            Outcome::Failed,
            Outcome::Running,
        ];
        outcomes.sort_unstable();
        assert_eq!(outcomes[0], Outcome::Failed);
    }
}
