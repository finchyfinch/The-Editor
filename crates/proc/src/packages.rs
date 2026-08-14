//! What is installed in the project's environment, and changing it.
//!
//! Two jobs with different characters, kept apart on purpose.
//!
//! **Reading** — what is installed, and what has a newer release — runs on a
//! worker thread and reports back, because `pip list --outdated` talks to the
//! network and can take several seconds. Blocking the frame loop on that would
//! freeze the editor.
//!
//! **Changing** — install, upgrade, remove — produces a [`RunConfig`] for the
//! console instead of running here. The venv dialog settled this argument
//! already: nothing should happen invisibly, and when pip fails the useful
//! thing is pip's own message, in full, with its suggestion. A tidy
//! "installation failed" box throws away the only part worth reading.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use crate::run_config::RunConfig;

/// One installed distribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// A newer release, when `pip` reported one. `None` means either
    /// up to date or not yet checked — [`Report::checked_for_updates`] says
    /// which, so "no updates" is never confused with "did not look".
    pub latest: Option<String>,
}

/// What the worker found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub packages: Vec<Package>,
    /// True once the outdated check has finished. Until then the panel says it
    /// is still looking rather than implying everything is current.
    pub checked_for_updates: bool,
    /// Whatever went wrong, verbatim.
    pub error: Option<String>,
}

/// A listing in progress.
#[derive(Debug)]
pub struct Listing {
    updates: Receiver<Report>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Listing {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Listing {
    /// Start listing the packages in `interpreter`'s environment.
    ///
    /// Reports twice: once with the installed set, which is quick and local,
    /// and again once the network check for newer releases has finished.
    /// Waiting for the second before showing the first would make an offline
    /// machine look broken.
    #[must_use]
    pub fn start(interpreter: &Path) -> Self {
        let (tx, updates) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let python = interpreter.to_path_buf();
        let stop = Arc::clone(&cancelled);

        std::thread::spawn(move || {
            let installed = match run_pip(&python, &["list", "--format=json"]) {
                Ok(text) => parse_list(&text),
                Err(e) => {
                    let _ = tx.send(Report {
                        error: Some(e),
                        checked_for_updates: true,
                        ..Report::default()
                    });
                    return;
                }
            };
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let _ = tx.send(Report {
                packages: installed.clone(),
                checked_for_updates: false,
                error: None,
            });

            // The slow half. A failure here is not a failure of the listing:
            // no network is the usual cause, and the installed versions on
            // screen are still correct and still worth having.
            let outdated = run_pip(&python, &["list", "--outdated", "--format=json"])
                .map(|text| parse_list(&text))
                .unwrap_or_default();
            if stop.load(Ordering::Relaxed) {
                return;
            }

            let mut packages = installed;
            for package in &mut packages {
                package.latest = outdated
                    .iter()
                    .find(|o| o.name.eq_ignore_ascii_case(&package.name))
                    .map(|o| o.version.clone());
            }
            let _ = tx.send(Report {
                packages,
                checked_for_updates: true,
                error: None,
            });
        });

        Self { updates, cancelled }
    }

    /// The most recent report, if one has arrived. Never blocks.
    pub fn poll(&self) -> Option<Report> {
        self.updates.try_iter().last()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// Run pip and return its standard output.
fn run_pip(interpreter: &Path, args: &[&str]) -> Result<String, String> {
    // `python -m pip` rather than the `pip` on PATH: the environment we are
    // asking about is the interpreter's, and a bare `pip` may well belong to a
    // different one.
    let mut command = crate::spawn::quiet(interpreter);
    command.arg("-m").arg("pip").args(args);

    let output = command
        .output()
        .map_err(|e| format!("could not run {}: {e}", interpreter.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return Err(if message.is_empty() {
            format!("pip exited with {}", output.status)
        } else {
            message.to_owned()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `pip list --format=json`.
///
/// Both the installed and the outdated listings are arrays of objects with
/// `name` and `version`; the outdated one adds `latest_version`, which is what
/// its `version` field is read as by the caller.
fn parse_list(json: &str) -> Vec<Package> {
    #[derive(serde::Deserialize)]
    struct Row {
        name: String,
        version: String,
        #[serde(default)]
        latest_version: Option<String>,
    }
    let rows: Vec<Row> = serde_json::from_str(json).unwrap_or_default();
    rows.into_iter()
        .map(|row| Package {
            name: row.name,
            // For the outdated listing the interesting number is the newer
            // one, so it takes the place of the installed version here and the
            // caller reads it as "latest".
            version: row.latest_version.unwrap_or(row.version),
            latest: None,
        })
        .collect()
}

/// What to do to an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Install(String),
    Upgrade(String),
    Remove(String),
    /// Install everything named in a requirements file.
    InstallRequirements(PathBuf),
}

impl Change {
    /// A label for the console tab, so a run is identifiable while it happens.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Install(name) => format!("pip install {name}"),
            Self::Upgrade(name) => format!("pip upgrade {name}"),
            Self::Remove(name) => format!("pip uninstall {name}"),
            Self::InstallRequirements(path) => {
                format!("pip install -r {}", path.display())
            }
        }
    }
}

/// The command that carries out `change`, to be run in the console.
#[must_use]
pub fn command(interpreter: &Path, cwd: &Path, change: &Change) -> RunConfig {
    let mut args = vec!["-m".to_owned(), "pip".to_owned()];
    match change {
        Change::Install(name) => {
            args.push("install".to_owned());
            args.push(name.clone());
        }
        Change::Upgrade(name) => {
            args.extend(["install".to_owned(), "--upgrade".to_owned()]);
            args.push(name.clone());
        }
        Change::Remove(name) => {
            // Without `-y` pip stops to ask, and the console has no way to
            // answer a prompt that expects a terminal.
            args.extend(["uninstall".to_owned(), "-y".to_owned()]);
            args.push(name.clone());
        }
        Change::InstallRequirements(path) => {
            args.extend(["install".to_owned(), "-r".to_owned()]);
            args.push(path.display().to_string());
        }
    }
    RunConfig {
        label: change.label(),
        program: interpreter.to_path_buf(),
        args,
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
    }
}

/// Write `pip freeze` output to `path`.
///
/// Run here rather than in the console because the console cannot redirect —
/// and because writing the file ourselves is what lets a failure leave the
/// existing `requirements.txt` untouched rather than truncated.
///
/// # Errors
/// If pip cannot be run, or the file cannot be written.
pub fn freeze_to(interpreter: &Path, path: &Path) -> Result<usize, String> {
    let text = run_pip(interpreter, &["freeze"])?;
    // Normalise to LF and drop blank lines: pip's output on Windows arrives
    // with CRLF, and a requirements file is read by tools that are happier
    // without them.
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    std::fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(lines.len())
}

/// The name part of a requirements line, or `None` for a blank or a comment.
///
/// Used to tell whether a package is already pinned in the file, so adding one
/// that is there does not put it in twice. Deliberately simple: it handles the
/// forms people actually write — `name`, `name==1.2`, `name>=1.2,<2`,
/// `name[extra]==1.2` — and gives up on URLs and `-e` lines, which are not
/// things to be silently rewritten anyway.
#[must_use]
pub fn requirement_name(line: &str) -> Option<String> {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() || line.starts_with('-') || line.contains("://") {
        return None;
    }
    let name: String = line
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect();
    (!name.is_empty()).then(|| name.to_ascii_lowercase().replace('_', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_installed_listing_is_parsed() {
        let json = r#"[{"name":"ruff","version":"0.5.0"},{"name":"pytest","version":"8.2.1"}]"#;
        let got = parse_list(json);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "ruff");
        assert_eq!(got[0].version, "0.5.0");
    }

    /// The outdated listing carries both numbers; the one worth showing is the
    /// newer.
    #[test]
    fn an_outdated_listing_reports_the_newer_version() {
        let json = r#"[{"name":"ruff","version":"0.5.0","latest_version":"0.6.2",
                        "latest_filetype":"wheel"}]"#;
        let got = parse_list(json);
        assert_eq!(got[0].version, "0.6.2");
    }

    /// pip has printed a warning before its JSON before now, and a panel that
    /// dies on that is worse than one that shows nothing.
    #[test]
    fn unparseable_output_yields_no_packages_rather_than_panicking() {
        assert!(parse_list("WARNING: something\n").is_empty());
        assert!(parse_list("").is_empty());
        assert!(parse_list("{}").is_empty());
    }

    #[test]
    fn the_commands_are_what_pip_expects() {
        let python = Path::new("/venv/bin/python");
        let cwd = Path::new("/project");

        let install = command(python, cwd, &Change::Install("ruff".into()));
        assert_eq!(install.args, ["-m", "pip", "install", "ruff"]);

        let upgrade = command(python, cwd, &Change::Upgrade("ruff".into()));
        assert_eq!(upgrade.args, ["-m", "pip", "install", "--upgrade", "ruff"]);

        // Without -y pip waits for an answer the console cannot give.
        let remove = command(python, cwd, &Change::Remove("ruff".into()));
        assert_eq!(remove.args, ["-m", "pip", "uninstall", "-y", "ruff"]);
    }

    /// The environment asked about is the interpreter's, so every command has
    /// to go through that interpreter rather than whatever `pip` is on PATH.
    #[test]
    fn every_command_runs_through_the_chosen_interpreter() {
        let python = Path::new("/project/.venv/Scripts/python.exe");
        for change in [
            Change::Install("a".into()),
            Change::Upgrade("a".into()),
            Change::Remove("a".into()),
            Change::InstallRequirements("/project/requirements.txt".into()),
        ] {
            let config = command(python, Path::new("/project"), &change);
            assert_eq!(config.program, python);
            assert_eq!(&config.args[..2], ["-m", "pip"]);
            assert!(!config.label.is_empty());
        }
    }

    #[test]
    fn requirement_names_are_read_from_the_forms_people_write() {
        assert_eq!(requirement_name("ruff"), Some("ruff".into()));
        assert_eq!(requirement_name("ruff==0.5.0"), Some("ruff".into()));
        assert_eq!(requirement_name("ruff>=0.5,<1.0"), Some("ruff".into()));
        assert_eq!(requirement_name("ruff[extra]==0.5"), Some("ruff".into()));
        assert_eq!(
            requirement_name("  ruff == 0.5  # pin"),
            Some("ruff".into())
        );
        // Normalised, because pip treats these as the same distribution.
        assert_eq!(
            requirement_name("Typing_Extensions"),
            Some("typing-extensions".into())
        );
    }

    #[test]
    fn lines_that_are_not_simple_requirements_are_left_alone() {
        assert_eq!(requirement_name(""), None);
        assert_eq!(requirement_name("# a comment"), None);
        assert_eq!(requirement_name("-e ."), None);
        assert_eq!(
            requirement_name("--index-url https://example.invalid"),
            None
        );
        assert_eq!(requirement_name("https://example.invalid/pkg.whl"), None);
    }

    #[test]
    fn a_change_names_itself_for_the_console_tab() {
        assert_eq!(Change::Install("ruff".into()).label(), "pip install ruff");
        assert_eq!(Change::Remove("ruff".into()).label(), "pip uninstall ruff");
    }
}
