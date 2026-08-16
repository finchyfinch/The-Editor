//! Reading `cargo test`'s output, line by line as it arrives.
//!
//! Rust's test harness writes a line per test:
//!
//! ```text
//! running 3 tests
//! test module::a ... ok
//! test module::b ... FAILED
//! test module::c ... ignored
//! ```
//!
//! and then the failures in full:
//!
//! ```text
//! failures:
//!
//! ---- module::b stdout ----
//!
//! thread 'module::b' panicked at crates/thing/src/lib.rs:12:9:
//! assertion `left == right` failed
//! ```
//!
//! `cargo test` runs each target as a separate binary, so the same test name
//! can appear twice — once in the library's tests and once in an integration
//! test. The `Running target/debug/deps/…` line between them says which is
//! which, and that name is kept so two rows do not collapse into one.
//!
//! A JSON format exists and needs a nightly compiler. This one has looked the
//! same since 1.0.

use crate::report::{Case, Location, Outcome, Report};

/// Where in the output the parser is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    Results,
    /// Past `failures:`, reading `---- name stdout ----` blocks.
    Failures,
}

/// Reads `cargo test` output into a [`Report`].
#[derive(Debug, Default)]
pub struct Parser {
    place: Option<Where>,
    /// Which test binary is running, so identical names in two targets stay
    /// apart. Empty before the first `Running` line.
    target: String,
    /// The test whose failure block is being read.
    current: Option<String>,
    block: Vec<String>,
}

impl Parser {
    /// Feed one line of output, with escape sequences already stripped.
    pub fn line(&mut self, line: &str, report: &mut Report) {
        let trimmed = line.trim_end();

        // `Running unittests src/lib.rs (target/debug/deps/thing-1a2b3c.exe)`
        if let Some(target) = running_target(trimmed) {
            self.flush(report);
            self.target = target;
            self.place = Some(Where::Results);
            return;
        }
        // `failures:` appears twice — once before the blocks and once before a
        // bare list of names. The second is harmless to re-enter.
        if trimmed.trim_start() == "failures:" {
            self.flush(report);
            self.place = Some(Where::Failures);
            return;
        }
        // `test result: FAILED. 1 passed; 1 failed; …` ends a target.
        if trimmed.trim_start().starts_with("test result:") {
            self.flush(report);
            self.place = Some(Where::Results);
            return;
        }

        match self.place {
            Some(Where::Failures) => self.failure_line(trimmed, report),
            _ => self.result_line(trimmed, report),
        }
    }

    /// Everything has been fed; close whatever was open.
    pub fn finish(&mut self, report: &mut Report) {
        self.flush(report);
        report.finish();
    }

    /// `test module::a ... ok`
    fn result_line(&mut self, line: &str, report: &mut Report) {
        let Some((name, verdict)) = split_result(line) else {
            return;
        };
        let Some(outcome) = outcome_of(verdict) else {
            return;
        };
        report.record(Case {
            id: self.qualify(name),
            location: None,
            outcome,
            message: String::new(),
        });
    }

    fn failure_line(&mut self, line: &str, report: &mut Report) {
        if let Some(name) = block_header(line) {
            self.flush(report);
            self.current = Some(self.qualify(&name));
            return;
        }
        if self.current.is_some() {
            self.block.push(line.to_owned());
        }
    }

    fn flush(&mut self, report: &mut Report) {
        let Some(id) = self.current.take() else {
            self.block.clear();
            return;
        };
        let block = std::mem::take(&mut self.block);

        // `thread 'x' panicked at path:line:column:` — the first one, because
        // a panic inside a helper reports the helper and then the caller, and
        // the first is where the assertion actually is.
        let location = block.iter().find_map(|l| location_of(l));
        let message = block
            .join("\n")
            .trim_matches(|c: char| c == '\n' || c == '\r')
            .to_owned();

        report.explain(&id, message, location);
    }

    /// Prefix a bare test name with its target, so two targets with the same
    /// test name stay two rows.
    fn qualify(&self, name: &str) -> String {
        if self.target.is_empty() {
            name.to_owned()
        } else {
            format!("{}::{name}", self.target)
        }
    }
}

/// `Running unittests src/lib.rs (target/debug/deps/thing-1a2b3c.exe)` gives
/// `thing`.
///
/// The hash is dropped: it changes on every rebuild, and a row whose name
/// changes when nothing did is a row that cannot be compared between runs.
fn running_target(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("Running ")?;
    let inside = rest.rsplit_once('(')?.1.trim_end_matches(')');
    let file = inside.rsplit(['/', '\\']).next()?;
    let stem = file.strip_suffix(".exe").unwrap_or(file);
    let name = stem.rsplit_once('-').map_or(stem, |(before, _)| before);
    (!name.is_empty()).then(|| name.to_owned())
}

/// Split `test module::a ... ok` into the name and the verdict.
fn split_result(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("test ")?;
    let (name, verdict) = rest.rsplit_once(" ... ")?;
    (!name.is_empty()).then_some((name, verdict))
}

fn outcome_of(verdict: &str) -> Option<Outcome> {
    match verdict.trim() {
        "ok" => Some(Outcome::Passed),
        "FAILED" => Some(Outcome::Failed),
        // `ignored` may carry a reason: `ignored, needs a network`.
        v if v == "ignored" || v.starts_with("ignored,") => Some(Outcome::Skipped),
        // `bench: …` from a benchmark run.
        v if v.starts_with("bench:") => Some(Outcome::Passed),
        _ => None,
    }
}

/// `---- module::b stdout ----` gives `module::b`.
fn block_header(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("---- ")?;
    let name = rest.strip_suffix(" ----")?;
    // The suffix says which stream it was; the name is what comes before it.
    let name = name
        .strip_suffix(" stdout")
        .or_else(|| name.strip_suffix(" stderr"))
        .unwrap_or(name);
    (!name.is_empty()).then(|| name.to_owned())
}

/// `thread 'x' panicked at crates/thing/src/lib.rs:12:9:` gives file and line.
fn location_of(line: &str) -> Option<Location> {
    let after = line.split_once(" panicked at ")?.1;
    let after = after.trim().trim_end_matches(':');
    // `file:line:column`, and the column is not wanted.
    let (before, _column) = after.rsplit_once(':')?;
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
    fn every_verdict_is_understood() {
        let report = run("running 4 tests\n\
             test a ... ok\n\
             test b ... FAILED\n\
             test c ... ignored\n\
             test d ... ignored, needs a network");
        let outcomes: Vec<Outcome> = report.cases.iter().map(|c| c.outcome).collect();
        assert_eq!(
            outcomes,
            [
                Outcome::Passed,
                Outcome::Failed,
                Outcome::Skipped,
                Outcome::Skipped
            ]
        );
    }

    #[test]
    fn the_target_is_kept_so_two_binaries_do_not_collide() {
        let report = run(
            "     Running unittests src/lib.rs (target/debug/deps/thing-1a2b3c.exe)\n\
             test shared_name ... ok\n\
             \n\
             \x20    Running tests/integration.rs (target/debug/deps/integration-9f8e7d.exe)\n\
             test shared_name ... FAILED",
        );
        assert_eq!(report.cases.len(), 2, "same name, two targets, two rows");
        assert_eq!(report.cases[0].id, "thing::shared_name");
        assert_eq!(report.cases[1].id, "integration::shared_name");
    }

    /// The hash changes on every rebuild, and a row whose name changes when
    /// nothing did cannot be compared between runs.
    #[test]
    fn the_build_hash_is_not_part_of_the_name() {
        let report = run(
            "     Running unittests src/lib.rs (target/debug/deps/editor_vcs-bc72573a67145298.exe)\n\
             test a ... ok",
        );
        assert_eq!(report.cases[0].id, "editor_vcs::a");
    }

    #[test]
    fn a_failure_gets_its_panic_message_and_line() {
        let report = run(
            "     Running unittests src/lib.rs (target/debug/deps/thing-1a2b3c.exe)\n\
             test module::b ... FAILED\n\
             \n\
             failures:\n\
             \n\
             ---- module::b stdout ----\n\
             \n\
             thread 'module::b' panicked at crates/thing/src/lib.rs:12:9:\n\
             assertion `left == right` failed\n\
             \x20 left: 1\n\
             \x20right: 2\n\
             \n\
             \n\
             failures:\n\
             \x20   module::b\n\
             \n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored",
        );
        assert_eq!(report.cases.len(), 1);
        let case = &report.cases[0];
        assert_eq!(case.outcome, Outcome::Failed);
        assert!(
            case.message.contains("assertion `left == right` failed"),
            "{:?}",
            case.message
        );
        let location = case.location.as_ref().expect("a location");
        assert_eq!(location.file.to_string_lossy(), "crates/thing/src/lib.rs");
        assert_eq!(location.line, 12);
    }

    #[test]
    fn two_failures_keep_their_own_messages() {
        let report = run(
            "     Running unittests src/lib.rs (target/debug/deps/thing-1.exe)\n\
             test a ... FAILED\n\
             test b ... FAILED\n\
             failures:\n\
             ---- a stdout ----\n\
             thread 'a' panicked at src/lib.rs:3:1:\n\
             first problem\n\
             ---- b stdout ----\n\
             thread 'b' panicked at src/lib.rs:7:1:\n\
             second problem",
        );
        assert!(report.cases[0].message.contains("first problem"));
        assert!(!report.cases[0].message.contains("second problem"));
        assert_eq!(report.cases[0].location.as_ref().expect("l").line, 3);
        assert_eq!(report.cases[1].location.as_ref().expect("l").line, 7);
    }

    /// A panic inside a helper reports the helper and then the caller; the
    /// first is where the assertion is.
    #[test]
    fn the_location_is_the_first_panic_reported() {
        let report = run(
            "     Running unittests src/lib.rs (target/debug/deps/thing-1.exe)\n\
             test a ... FAILED\n\
             failures:\n\
             ---- a stdout ----\n\
             thread 'a' panicked at src/assertions.rs:40:5:\n\
             the real one\n\
             note: another panicked at src/elsewhere.rs:99:1:",
        );
        let location = report.cases[0].location.as_ref().expect("location");
        assert_eq!(location.file.to_string_lossy(), "src/assertions.rs");
        assert_eq!(location.line, 40);
    }

    #[test]
    fn a_stderr_block_is_read_the_same_way() {
        let report = run(
            "     Running unittests src/lib.rs (target/debug/deps/thing-1.exe)\n\
             test a ... FAILED\n\
             failures:\n\
             ---- a stderr ----\n\
             thread 'a' panicked at src/lib.rs:5:1:\n\
             said on stderr",
        );
        assert!(report.cases[0].message.contains("said on stderr"));
    }

    #[test]
    fn compiler_output_and_summaries_are_not_results() {
        let report = run("   Compiling thing v1.0.0 (C:\\projects\\thing)\n\
             warning: unused variable: `x`\n\
             \x20   Finished `test` profile [unoptimized] target(s) in 1.23s\n\
             \x20    Running unittests src/lib.rs (target/debug/deps/thing-1.exe)\n\
             \n\
             running 0 tests\n\
             \n\
             test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out");
        assert!(report.is_empty(), "got {:?}", report.cases);
        assert_eq!(report.summary(), "No tests were found.");
    }

    /// Something a test printed must not be read as a verdict.
    #[test]
    fn a_line_that_only_looks_like_a_result_is_ignored() {
        let report = run(
            "     Running unittests src/lib.rs (target/debug/deps/thing-1.exe)\n\
             test a ... ok\n\
             test something that is not a verdict",
        );
        assert_eq!(report.cases.len(), 1);
    }

    /// The half-finished state: the panel should show what has passed so far.
    #[test]
    fn results_appear_before_the_run_ends() {
        let mut parser = Parser::default();
        let mut report = Report::default();
        parser.line(
            "     Running unittests src/lib.rs (target/debug/deps/thing-1.exe)",
            &mut report,
        );
        parser.line("test a ... ok", &mut report);
        assert_eq!(report.cases.len(), 1);
        assert!(!report.finished);
        assert_eq!(report.summary(), "1 passed\u{2026} so far");
    }

    /// A run killed part-way leaves tests that started and never reported.
    #[test]
    fn a_test_that_never_reported_is_a_failure_not_a_pass() {
        let mut parser = Parser::default();
        let mut report = Report::default();
        parser.line("test a ... ok", &mut report);
        report.record(Case {
            id: "b".to_owned(),
            location: None,
            outcome: Outcome::Running,
            message: String::new(),
        });
        parser.finish(&mut report);
        assert_eq!(report.cases[1].outcome, Outcome::Failed);
    }
}
