//! The performance budgets from PLAN.md §2.4, measured and enforced.
//!
//! Three numbers were written down at the start of the project, and until now
//! nothing checked them:
//!
//! * under 4 ms to produce a screenful of highlighted code
//! * under 1 ms to apply a single-character edit including the reparse
//! * under 150 ms to open a 5 MB file
//!
//! Run with `cargo bench -p editor-widgets`. This prints what each one actually
//! costs and exits non-zero if any budget is blown, so it can be run in a
//! pipeline as well as read by a person. A regression here is the difference
//! between an editor that feels immediate and one that does not, and that is
//! not something anybody notices from reading a diff.
//!
//! No benchmarking framework. These are millisecond-scale operations with a
//! generous margin, so repeating each one and taking the median says everything
//! a sampling harness would, without a dependency or a five-minute run.
//!
//! What is *not* measured here is the painting itself: laying out glyphs and
//! filling rectangles needs a font stack and a GPU, so a headless number for it
//! would be measuring the harness. The highlighting that feeds the painter is
//! the part this project controls and the part that scales with file size, so
//! that is what stands in for the frame budget.

use std::hint::black_box;
use std::time::{Duration, Instant};

use editor_core::document::Document;
use editor_core::edit::Transaction;
use editor_core::selection::Selection;
use editor_syntax::LanguageId;
use editor_syntax::highlight::Highlighter;
use editor_syntax::theme::SyntaxTheme;
use ropey::Rope;

/// Rows on screen at a normal window size, which is what "a screenful" means.
const SCREENFUL: usize = 60;

/// PLAN.md §2.4.
const FRAME_BUDGET: Duration = Duration::from_millis(4);
const EDIT_BUDGET: Duration = Duration::from_millis(1);
const OPEN_BUDGET: Duration = Duration::from_millis(150);

/// The size of file the edit budget is about: a large source file, not a
/// generated one. Two thousand lines is a big module by any standard.
const TYPICAL_LINES: usize = 2_000;

/// A file well past anything hand-written, used for the worst case below.
const HUGE_LINES: usize = 20_000;

/// What a keystroke may cost when the reparse cannot reuse anything.
///
/// Not a §2.4 budget — a guarantee about the cap that `editor-syntax` puts on
/// the parse. Without it, a character typed at the start of a line in a
/// 10,000-line Rust file cost 117 ms, three times what parsing the whole file
/// from scratch takes, because tree-sitter pays for the failed reuse on top of
/// the reparse. This is the number that proves the cap is doing its job; the
/// allowance over the cap itself is for the parser finishing the step it was
/// in when the clock ran out.
const WORST_CASE_BUDGET: Duration = Duration::from_millis(20);

fn main() {
    let mut failures = Vec::new();

    report(
        "highlight a screenful",
        FRAME_BUDGET,
        bench_highlight_screenful(),
        &mut failures,
    );
    report(
        "one character, with reparse",
        EDIT_BUDGET,
        bench_single_character_edit(),
        &mut failures,
    );
    report(
        "worst-case keystroke (capped)",
        WORST_CASE_BUDGET,
        bench_worst_case_keystroke(),
        &mut failures,
    );
    report(
        "open a 5 MB file",
        OPEN_BUDGET,
        bench_open_large(),
        &mut failures,
    );

    if failures.is_empty() {
        println!("\nAll §2.4 budgets met.");
        return;
    }
    eprintln!("\nOver budget:");
    for line in &failures {
        eprintln!("  {line}");
    }
    std::process::exit(1);
}

fn report(name: &str, budget: Duration, took: Duration, failures: &mut Vec<String>) {
    let headroom = budget.as_secs_f64() / took.as_secs_f64().max(f64::EPSILON);
    println!(
        "{name:<32} {:>9.3} ms   budget {:>6.1} ms   {headroom:.1}x headroom",
        took.as_secs_f64() * 1000.0,
        budget.as_secs_f64() * 1000.0,
    );
    if took > budget {
        failures.push(format!(
            "{name}: {:.3} ms against a {:.1} ms budget",
            took.as_secs_f64() * 1000.0,
            budget.as_secs_f64() * 1000.0
        ));
    }
}

/// Median of `runs` measurements. The median, not the mean: one scheduling
/// hiccup should not decide whether the build passes.
fn median(runs: usize, mut body: impl FnMut() -> Duration) -> Duration {
    let mut times: Vec<Duration> = (0..runs).map(|_| body()).collect();
    times.sort_unstable();
    times[times.len() / 2]
}

/// Python that looks like Python: functions, classes, strings, comments and
/// nesting, rather than one token repeated. Highlighting cost follows the shape
/// of the tree, so a file of `x = 1` would flatter the result.
fn sample_python(lines: usize) -> String {
    let block = "\
import os
from typing import Iterable


class Widget:
    \"\"\"A thing with a name and a size.\"\"\"

    def __init__(self, name: str, size: int = 0) -> None:
        self.name = name
        self.size = size  # in millimetres

    def scaled(self, factor: float) -> \"Widget\":
        if factor <= 0:
            raise ValueError(f\"bad factor: {factor!r}\")
        return Widget(self.name, int(self.size * factor))


def widths(items: Iterable[Widget]) -> list[int]:
    return [w.size for w in items if w.size > 0]
";
    let per_block = block.lines().count();
    block.repeat(lines.div_ceil(per_block))
}

fn bench_highlight_screenful() -> Duration {
    // A large file, so the measurement includes finding the visible window in a
    // real tree rather than highlighting the whole document.
    let text = Rope::from_str(&sample_python(20_000));
    let theme = SyntaxTheme::for_ui(editor_config::theme::ResolvedTheme::Dark);
    let mut highlighter =
        Highlighter::new(LanguageId::Python, &text).expect("Python has a grammar");

    // Somewhere in the middle, which is where scrolling actually spends its
    // time; the first screenful is the easy case.
    let first_line = text.len_lines() / 2;
    let from = text.line_to_byte(first_line);
    let to = text.line_to_byte((first_line + SCREENFUL).min(text.len_lines() - 1));

    median(21, || {
        let started = Instant::now();
        let spans = highlighter.spans(&text, from..to, &theme);
        let elapsed = started.elapsed();
        assert!(!black_box(&spans).is_empty(), "nothing was highlighted");
        elapsed
    })
}

fn bench_single_character_edit() -> Duration {
    let mut doc = Document::untitled();
    let source = sample_python(TYPICAL_LINES);
    let end = source.chars().count();
    doc.apply(
        &Transaction::insert(0, source),
        Selection::at(0),
        Selection::at(end),
    );
    let mut highlighter =
        Highlighter::new(LanguageId::Python, doc.text()).expect("Python has a grammar");
    doc.take_changes();

    // Type into the middle of the file, one character at a time, reparsing
    // after each — which is exactly what happens while somebody types.
    let at = doc.len_chars() / 2;
    median(51, || {
        let started = Instant::now();
        doc.apply(
            &Transaction::insert(at, "x"),
            Selection::at(at),
            Selection::at(at + 1),
        );
        let changes = doc.take_changes();
        highlighter.update(&changes, doc.text());
        started.elapsed()
    })
}

/// The pathological case: a big Rust file, edited where it breaks the syntax.
///
/// Typing a character at the start of a `fn` line invalidates everything the
/// parser could have reused, so this is the shape of edit that used to freeze
/// the editor. Rust rather than Python because its grammar is the more
/// expensive of the two, and column zero rather than mid-line because that is
/// where an edit does the most damage to the tree.
fn bench_worst_case_keystroke() -> Duration {
    let block = "\
fn scaled(a: u32, b: u32) -> u32 {
    a + b // note
}

struct Widget {
    name: String,
    size: u32,
}

";
    let mut doc = Document::untitled();
    let source = block.repeat(HUGE_LINES / block.lines().count());
    let end = source.chars().count();
    doc.apply(
        &Transaction::insert(0, source),
        Selection::at(0),
        Selection::at(end),
    );
    let mut highlighter =
        Highlighter::new(LanguageId::Rust, doc.text()).expect("Rust has a grammar");
    doc.take_changes();

    let at = doc.line_start(doc.line_count() / 2);
    median(11, || {
        let started = Instant::now();
        doc.apply(
            &Transaction::insert(at, "x"),
            Selection::at(at),
            Selection::at(at + 1),
        );
        let changes = doc.take_changes();
        highlighter.update(&changes, doc.text());
        started.elapsed()
    })
}

fn bench_open_large() -> Duration {
    let dir = std::env::temp_dir().join("the-editor-bench");
    std::fs::create_dir_all(&dir).expect("create the bench directory");
    let path = dir.join("large.py");

    // Just under the 5 MB read-only threshold, so this measures the fully
    // editable path rather than the plain-text fallback.
    let target = 5 * 1024 * 1024 - 64 * 1024;
    let mut text = String::with_capacity(target + 4096);
    let block = sample_python(200);
    while text.len() < target {
        text.push_str(&block);
    }
    text.truncate(target);
    std::fs::write(&path, &text).expect("write the sample file");

    let took = median(7, || {
        let started = Instant::now();
        let doc = Document::open(&path).expect("the sample file opens");
        let elapsed = started.elapsed();
        assert!(black_box(&doc).len_chars() > 0);
        elapsed
    });

    std::fs::remove_dir_all(&dir).ok();
    took
}
