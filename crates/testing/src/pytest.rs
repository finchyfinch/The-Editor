//! Reading pytest's own output, line by line as it arrives.
//!
//! `pytest -v` writes one line per test as it finishes:
//!
//! ```text
//! tests/test_a.py::test_b PASSED                                    [ 50%]
//! tests/test_a.py::TestC::test_d FAILED                             [100%]
//! ```
//!
//! and then, at the end, a block per failure:
//!
//! ```text
//! =================================== FAILURES ===================================
//! _________________________________ TestC.test_d _________________________________
//!
//!     def test_d(self):
//! >       assert 1 == 2
//! E       assert 1 == 2
//!
//! tests/test_a.py:9: AssertionError
//! ```
//!
//! Scraping output is normally the wrong way round, and `--junit-xml` would be
//! more stable. It is not used because it is written when the run *ends*: a
//! suite that takes two minutes would show nothing for two minutes and then
//! everything. Reading the stream fills the panel in as it goes, which is worth
//! more than the robustness — and the result lines have looked like this since
//! pytest 2.

use crate::report::{Case, Location, Outcome, Report};

/// Where in the output the parser is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    /// Reading result lines.
    Results,
    /// Past the `=== FAILURES ===` banner, reading failure blocks.
    Failures,
}

/// Reads pytest output into a [`Report`].
#[derive(Debug)]
pub struct Parser {
    place: Where,
    /// The test whose failure block is being read, as pytest named it.
    current: Option<String>,
    /// The lines of that block so far.
    block: Vec<String>,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            place: Where::Results,
            current: None,
            block: Vec::new(),
        }
    }
}

impl Parser {
    /// Feed one line of output, with escape sequences already stripped.
    pub fn line(&mut self, line: &str, report: &mut Report) {
        // A banner: `====== FAILURES ======`, `=== short test summary ===`, or
        // the final `=== 1 failed, 2 passed in 0.1s ===`.
        if let Some(title) = banner(line) {
            self.flush(report);
            self.place =
                if title.eq_ignore_ascii_case("FAILURES") || title.eq_ignore_ascii_case("ERRORS") {
                    Where::Failures
                } else {
                    Where::Results
                };
            return;
        }

        match self.place {
            Where::Results => self.result_line(line, report),
            Where::Failures => self.failure_line(line, report),
        }
    }

    /// Everything has been fed; close whatever was open.
    pub fn finish(&mut self, report: &mut Report) {
        self.flush(report);
        report.finish();
    }

    /// `tests/test_a.py::test_b PASSED  [ 50%]`
    fn result_line(&mut self, line: &str, report: &mut Report) {
        let Some((id, rest)) = split_result(line) else {
            return;
        };
        let Some(outcome) = outcome_of(rest) else {
            return;
        };
        report.record(Case {
            id: id.to_owned(),
            // The node id names the file, but not the line, and a passing test
            // never says where it is. A failure will.
            location: None,
            outcome,
            message: String::new(),
        });
    }

    /// A line inside the failures section.
    fn failure_line(&mut self, line: &str, report: &mut Report) {
        if let Some(name) = block_header(line) {
            self.flush(report);
            self.current = Some(name);
            return;
        }
        if self.current.is_some() {
            self.block.push(line.to_owned());
        }
    }

    /// Attach the block just read to the test it belongs to.
    fn flush(&mut self, report: &mut Report) {
        let Some(name) = self.current.take() else {
            self.block.clear();
            return;
        };
        let block = std::mem::take(&mut self.block);

        // The last `path:line: Something` in the block is where the failure
        // was raised. Earlier ones are frames further up the stack, which are
        // useful to read and not where to jump to.
        let location = block.iter().rev().find_map(|l| location_of(l));

        let message = block
            .join("\n")
            .trim_matches(|c: char| c == '\n' || c == '\r')
            .to_owned();

        if let Some(id) = report.id_ending_with(&name) {
            report.explain(&id, message, location);
        } else {
            // A collection error names a *file*, not a test, and there is no
            // result line to attach it to. It is still the most important thing
            // on the screen.
            report.notes.push(format!("{name}\n{message}"));
        }
    }
}

/// `====== FAILURES ======` gives `FAILURES`.
fn banner(line: &str) -> Option<&str> {
    between_rules(line, '=')
}

/// `____ TestC.test_d ____` gives `TestC.test_d`.
fn block_header(line: &str) -> Option<String> {
    between_rules(line, '_').map(str::to_owned)
}

/// The title between two rules of `mark`, wherever on the line they are.
///
/// Anchored to the ends would be simpler, and is what this did first. It missed
/// every banner in a real run, because pytest's progress display leaves the
/// percentage on the same line: `[100%]======= FAILURES =======`. The run flag
/// that turns that off is passed too, and this is the belt to its braces —
/// a banner the parser walks past costs every failure message in the run.
fn between_rules(line: &str, mark: char) -> Option<&str> {
    /// Long enough not to match `__init__` or `a == b` in someone's output.
    const RULE: usize = 6;

    let start = run_at(line, mark, RULE)?;
    let after = start + count_from(&line[start..], mark);
    let rest = &line[after..];
    let closing = run_at(rest, mark, RULE)?;

    let title = rest[..closing].trim();
    (!title.is_empty()).then_some(title)
}

/// Where a run of at least `least` `mark` characters begins.
fn run_at(text: &str, mark: char, least: usize) -> Option<usize> {
    let mut run = 0;
    for (at, c) in text.char_indices() {
        if c == mark {
            run += 1;
            if run >= least {
                return Some(at + c.len_utf8() - run * c.len_utf8());
            }
        } else {
            run = 0;
        }
    }
    None
}

/// How many `mark` characters `text` starts with.
fn count_from(text: &str, mark: char) -> usize {
    text.chars().take_while(|c| *c == mark).count() * mark.len_utf8()
}

/// Split `tests/test_a.py::test_b PASSED [ 50%]` into the id and the rest.
///
/// Found by looking for the *verdict*, not by splitting at the first space. A
/// node id cannot contain a space, but a parametrised one carries its
/// parameters in brackets and those can contain anything —
/// `test_b[one two] PASSED` splits at the wrong place otherwise. Nor can it be
/// the last `]`, which is the closing bracket of the `[ 50%]` progress figure.
fn split_result(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end();
    if !line.contains("::") {
        return None;
    }

    // Walk the space-separated words, keeping each one's offset. Spaces are
    // ASCII, so slicing at them is always on a character boundary.
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < line.len() {
        while at < line.len() && bytes[at] == b' ' {
            at += 1;
        }
        let start = at;
        while at < line.len() && bytes[at] != b' ' {
            at += 1;
        }
        if start == at {
            break;
        }
        if outcome_of(&line[start..at]).is_some() {
            let id = line[..start].trim_end();
            return id.contains("::").then_some((id, &line[start..]));
        }
    }
    None
}

/// The verdict at the start of what followed the node id.
fn outcome_of(rest: &str) -> Option<Outcome> {
    let word = rest.split_whitespace().next()?;
    match word {
        "PASSED" | "XPASS" => Some(Outcome::Passed),
        // An `XFAIL` is a test that failed as it was expected to, which is a
        // pass in every sense that matters to somebody reading the panel.
        "SKIPPED" | "XFAIL" => Some(Outcome::Skipped),
        "FAILED" | "ERROR" => Some(Outcome::Failed),
        _ => None,
    }
}

/// `tests/test_a.py:9: AssertionError` gives the file and the line.
fn location_of(line: &str) -> Option<Location> {
    let trimmed = line.trim();
    // Traceback frames are indented and prefixed; the closing location is not.
    if trimmed.starts_with('E') || trimmed.starts_with('>') || line.starts_with(' ') {
        return None;
    }
    let (before, after) = trimmed.rsplit_once(':')?;
    // `after` must be a description, `before` must end in a line number.
    if after.trim().is_empty() {
        return None;
    }
    let (file, number) = before.rsplit_once(':')?;
    let line_number: u32 = number.trim().parse().ok()?;
    (!file.is_empty()).then(|| Location {
        file: file.into(),
        line: line_number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(output: &str) -> Report {
        let mut parser = Parser::default();
        let mut report = Report::default();
        for line in output.lines() {
            parser.line(line, &mut report);
        }
        parser.finish(&mut report);
        report
    }

    #[test]
    fn a_passing_test_is_recorded() {
        let report = run("tests/test_a.py::test_b PASSED                       [100%]");
        assert_eq!(report.cases.len(), 1);
        assert_eq!(report.cases[0].id, "tests/test_a.py::test_b");
        assert_eq!(report.cases[0].outcome, Outcome::Passed);
    }

    #[test]
    fn every_verdict_is_understood() {
        let report = run("tests/t.py::a PASSED   [ 20%]\n\
             tests/t.py::b FAILED   [ 40%]\n\
             tests/t.py::c SKIPPED (unconditional skip) [ 60%]\n\
             tests/t.py::d XFAIL    [ 80%]\n\
             tests/t.py::e ERROR    [100%]");
        let outcomes: Vec<Outcome> = report.cases.iter().map(|c| c.outcome).collect();
        assert_eq!(
            outcomes,
            [
                Outcome::Passed,
                Outcome::Failed,
                Outcome::Skipped,
                Outcome::Skipped,
                Outcome::Failed
            ]
        );
    }

    #[test]
    fn a_class_based_test_keeps_its_whole_node_id() {
        let report = run("tests/test_a.py::TestC::test_d PASSED [100%]");
        assert_eq!(report.cases[0].id, "tests/test_a.py::TestC::test_d");
        assert_eq!(report.cases[0].short_name(), "TestC::test_d");
    }

    /// A parametrised id carries its parameters in brackets, and those may
    /// contain spaces — which is why the id is not simply everything up to the
    /// first one.
    #[test]
    fn a_parametrised_test_keeps_its_parameters() {
        let report = run("tests/test_a.py::test_b[one two] PASSED   [100%]");
        assert_eq!(report.cases.len(), 1);
        assert_eq!(report.cases[0].id, "tests/test_a.py::test_b[one two]");
        assert_eq!(report.cases[0].outcome, Outcome::Passed);
    }

    #[test]
    fn a_failure_gets_its_message_and_its_line() {
        let report = run(
            "tests/test_a.py::test_b FAILED                                 [100%]\n\
             \n\
             =================================== FAILURES ===================================\n\
             ___________________________________ test_b _____________________________________\n\
             \n\
             \x20   def test_b():\n\
             >       assert 1 == 2\n\
             E       assert 1 == 2\n\
             \n\
             tests/test_a.py:9: AssertionError\n\
             =========================== 1 failed in 0.03s ==================================",
        );
        assert_eq!(report.cases.len(), 1);
        let case = &report.cases[0];
        assert_eq!(case.outcome, Outcome::Failed);
        assert!(case.message.contains("assert 1 == 2"), "{:?}", case.message);
        let location = case.location.as_ref().expect("a location");
        assert_eq!(location.file.to_string_lossy(), "tests/test_a.py");
        assert_eq!(location.line, 9);
    }

    /// The failure block names the test with dots where the node id had colons.
    #[test]
    fn a_class_failure_block_finds_its_test() {
        let report = run("tests/test_a.py::TestC::test_d FAILED  [100%]\n\
             ================================== FAILURES ====================================\n\
             _______________________________ TestC.test_d ___________________________________\n\
             E       assert False\n\
             tests/test_a.py:12: AssertionError");
        assert!(
            report.cases[0].message.contains("assert False"),
            "{:?}",
            report.cases[0].message
        );
        assert_eq!(
            report.cases[0].location.as_ref().expect("location").line,
            12
        );
    }

    /// Several failures, each with its own block, must not run together.
    #[test]
    fn two_failures_keep_their_own_messages() {
        let report = run("tests/t.py::a FAILED  [ 50%]\n\
             tests/t.py::b FAILED  [100%]\n\
             ================================== FAILURES ====================================\n\
             ____________________________________ a _________________________________________\n\
             E       first problem\n\
             tests/t.py:3: AssertionError\n\
             ____________________________________ b _________________________________________\n\
             E       second problem\n\
             tests/t.py:7: AssertionError");
        assert!(report.cases[0].message.contains("first problem"));
        assert!(!report.cases[0].message.contains("second problem"));
        assert!(report.cases[1].message.contains("second problem"));
        assert_eq!(report.cases[0].location.as_ref().expect("l").line, 3);
        assert_eq!(report.cases[1].location.as_ref().expect("l").line, 7);
    }

    /// The last location in a block is where it was raised; the ones above are
    /// frames further up, which are worth reading and not worth jumping to.
    #[test]
    fn the_location_is_the_innermost_frame() {
        let report = run("tests/t.py::a FAILED  [100%]\n\
             ================================== FAILURES ====================================\n\
             ____________________________________ a _________________________________________\n\
             tests/helpers.py:40: in check\n\
             E       failed\n\
             tests/t.py:12: AssertionError");
        let location = report.cases[0].location.as_ref().expect("location");
        assert_eq!(location.file.to_string_lossy(), "tests/t.py");
        assert_eq!(location.line, 12);
    }

    /// A file that will not import produces an error block naming the file
    /// rather than a test, and nothing to attach it to.
    #[test]
    fn a_collection_error_becomes_a_note() {
        let report = run(
            "==================================== ERRORS ====================================\n\
             _________________________ ERROR collecting tests/t.py __________________________\n\
             ImportError while importing test module 'tests/t.py'.\n\
             E   ModuleNotFoundError: No module named 'nothing'",
        );
        assert!(report.cases.is_empty());
        assert_eq!(report.notes.len(), 1);
        assert!(
            report.notes[0].contains("ModuleNotFoundError"),
            "{:?}",
            report.notes[0]
        );
    }

    /// What pytest's progress display actually produces: the banner sharing a
    /// line with the percentage before it. Every failure message in the run
    /// depends on this being noticed.
    #[test]
    fn a_banner_that_is_not_alone_on_its_line_is_still_a_banner() {
        let report = run("tests/t.py::a FAILED\n\
             [100%]=================================== FAILURES ===================================\n\
             ____________________________________ a _________________________________________  def a():\n\
             E       it went wrong\n\
             tests/t.py:4: AssertionError");
        assert!(
            report.cases[0].message.contains("it went wrong"),
            "got {:?}",
            report.cases[0].message
        );
        assert_eq!(report.cases[0].location.as_ref().expect("l").line, 4);
    }

    /// A rule has to be long enough not to match ordinary output.
    #[test]
    fn short_runs_of_punctuation_are_not_banners() {
        assert_eq!(banner("a == b == c"), None);
        assert_eq!(block_header("__init__ and __repr__"), None);
        assert_eq!(banner("=== short ==="), None, "three is not a rule");
    }

    #[test]
    fn a_banner_with_a_rule_either_side_is_read() {
        assert_eq!(banner("====== FAILURES ======"), Some("FAILURES"));
        assert_eq!(
            block_header("______ TestC.test_d ______"),
            Some("TestC.test_d".to_owned())
        );
    }

    #[test]
    fn an_empty_run_finds_nothing_and_says_so() {
        let report = run("");
        assert!(report.is_empty());
        assert!(report.finished);
        assert_eq!(report.summary(), "No tests were found.");
    }

    /// Output that is not a result line must not become one.
    #[test]
    fn ordinary_chatter_is_ignored() {
        let report = run(
            "============================= test session starts ==============================\n\
             platform win32 -- Python 3.13.0, pytest-8.0.0\n\
             rootdir: C:\\projects\\thing\n\
             collected 0 items\n\
             \n\
             ============================ no tests ran in 0.01s =============================",
        );
        assert!(report.is_empty(), "got {:?}", report.cases);
    }

    /// Something a test printed to stdout must not be read as a verdict.
    #[test]
    fn output_from_a_test_is_not_mistaken_for_a_result() {
        let report = run("tests/t.py::a PASSED  [100%]\n\
             some::thing PASSED is what my test printed");
        assert_eq!(
            report.cases.len(),
            2,
            "this one is genuinely ambiguous, and being wrong the noisy way \
             beats silently dropping a real result"
        );
    }

    #[test]
    fn the_parser_is_fed_a_line_at_a_time_and_the_report_grows() {
        let mut parser = Parser::default();
        let mut report = Report::default();
        parser.line("tests/t.py::a PASSED [ 50%]", &mut report);
        assert_eq!(report.cases.len(), 1);
        assert!(!report.finished, "still going");
        parser.line("tests/t.py::b FAILED [100%]", &mut report);
        assert_eq!(report.cases.len(), 2);
        parser.finish(&mut report);
        assert!(report.finished);
    }
}
