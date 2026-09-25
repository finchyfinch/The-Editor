//! The toolkit boundary, checked rather than hoped for.
//!
//! PLAN.md §2.1: no library crate below `editor-widgets` may depend on egui,
//! directly or through anything it depends on. That is what keeps the choice of
//! toolkit reversible and those crates testable without a window. The plan
//! promised a test that reads the dependency graph "so it can't rot silently";
//! until this file there was none, and the rule was held by convention.
//!
//! Read from `Cargo.lock`, which records the whole resolved graph, so a
//! dependency that pulls egui in three levels down is caught as well as a
//! direct one.

use std::collections::{BTreeMap, BTreeSet};

/// The crates that must never see the toolkit.
const TOOLKIT_FREE: &[&str] = &[
    "editor-core",
    "editor-syntax",
    "editor-lsp",
    "editor-debug",
    "editor-proc",
    "editor-search",
    "editor-config",
    "editor-testing",
    "editor-vcs",
];

/// Any of these in a crate's closure means it knows the toolkit.
const TOOLKIT: &[&str] = &["egui", "eframe", "epaint", "emath", "winit", "egui-winit"];

/// Package name to the names it depends on, from the lock file.
fn graph() -> BTreeMap<String, Vec<String>> {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("Cargo.lock at the workspace root");
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name = None;
    let mut in_deps = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            name = None;
            in_deps = false;
        } else if let Some(n) = line.strip_prefix("name = ") {
            let n = n.trim_matches('"').to_owned();
            graph.entry(n.clone()).or_default();
            name = Some(n);
        } else if line.starts_with("dependencies = [") {
            in_deps = true;
        } else if in_deps && line == "]" {
            in_deps = false;
        } else if in_deps && let Some(package) = &name {
            // `"name"` or, where two versions exist, `"name 1.2.3"`.
            let dependency = line
                .trim_end_matches(',')
                .trim_matches('"')
                .split(' ')
                .next()
                .unwrap_or_default()
                .to_owned();
            graph.entry(package.clone()).or_default().push(dependency);
        }
    }
    graph
}

/// Everything `root` depends on, however indirectly.
fn closure(graph: &BTreeMap<String, Vec<String>>, root: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_owned()];
    while let Some(next) = stack.pop() {
        for dependency in graph.get(&next).into_iter().flatten() {
            if seen.insert(dependency.clone()) {
                stack.push(dependency.clone());
            }
        }
    }
    seen
}

#[test]
fn no_library_below_the_widgets_depends_on_the_toolkit() {
    let graph = graph();
    for krate in TOOLKIT_FREE {
        assert!(graph.contains_key(*krate), "{krate} is not in Cargo.lock");
        let reached = closure(&graph, krate);
        let toolkit: Vec<&&str> = TOOLKIT.iter().filter(|t| reached.contains(**t)).collect();
        assert!(
            toolkit.is_empty(),
            "{krate} depends on {toolkit:?}; only editor-widgets and the application may"
        );
    }
}

/// The check has to be able to fail, or it proves nothing.
#[test]
fn the_widgets_crate_is_seen_to_depend_on_the_toolkit() {
    assert!(closure(&graph(), "editor-widgets").contains("egui"));
}
