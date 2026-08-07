//! Which language servers to use, and where to find them.
//!
//! Several servers may serve one language: Python is best covered by `ruff` for
//! linting and formatting alongside `basedpyright` for types, because neither
//! does the other's job well. Diagnostics from both are shown, tagged with
//! their source.
//!
//! Everything here is optional. The Editor must be perfectly usable with none
//! of these installed — see PLAN.md §3.6 — so a missing server is a fact to
//! report in Help → Check Toolchains, never an error.

use std::path::{Path, PathBuf};

/// A language server The Editor knows how to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerSpec {
    /// Stable identifier, used as the diagnostic source and in logs.
    pub id: &'static str,
    /// What to show a human.
    pub name: &'static str,
    /// Executable names to try, in order of preference.
    pub commands: &'static [&'static str],
    /// Arguments the server needs to speak LSP on stdio.
    pub args: &'static [&'static str],
    /// What it is for, shown in the toolchain check.
    pub provides: &'static str,
    /// The command that installs it.
    ///
    /// Carried on the spec rather than in a lookup table beside it, so a new
    /// server cannot be added with no way to obtain it. Every one of these is a
    /// single free command — no account, no download page, no licence to buy
    /// (decision D9).
    pub install: &'static str,
    /// An argument that makes the server print its version and exit zero.
    ///
    /// Used to check a binary actually works before trying to speak LSP to it.
    /// `None` where the server has no such flag, in which case only its
    /// existence is checked.
    pub version_arg: Option<&'static str>,
}

/// A server that was actually found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub spec: ServerSpec,
    pub program: PathBuf,
}

/// Rust.
pub const RUST_ANALYZER: ServerSpec = ServerSpec {
    id: "rust-analyzer",
    name: "rust-analyzer",
    commands: &["rust-analyzer"],
    args: &[],
    provides: "completion, diagnostics, go to definition, rename",
    install: "rustup component add rust-analyzer",
    version_arg: Some("--version"),
};

/// Python linting and formatting. Fast, one binary, no configuration needed.
pub const RUFF: ServerSpec = ServerSpec {
    id: "ruff",
    name: "Ruff",
    commands: &["ruff"],
    args: &["server"],
    provides: "linting and formatting",
    install: "pip install ruff",
    version_arg: Some("--version"),
};

/// Python types and navigation. `basedpyright` is preferred over `pyright`
/// because it needs no separate Node installation, but either works.
pub const PYRIGHT: ServerSpec = ServerSpec {
    id: "pyright",
    name: "Pyright",
    commands: &["basedpyright-langserver", "pyright-langserver"],
    args: &["--stdio"],
    provides: "types, completion, go to definition",
    install: "pip install basedpyright",
    version_arg: Some("--version"),
};

/// A fallback Python server for people who already use it.
pub const PYLSP: ServerSpec = ServerSpec {
    id: "pylsp",
    name: "python-lsp-server",
    commands: &["pylsp"],
    args: &[],
    provides: "completion, diagnostics",
    install: "pip install python-lsp-server",
    version_arg: Some("--version"),
};

/// TOML, so `Cargo.toml` and `pyproject.toml` get validated.
pub const TAPLO: ServerSpec = ServerSpec {
    id: "taplo",
    name: "Taplo",
    commands: &["taplo"],
    args: &["lsp", "stdio"],
    provides: "TOML validation and formatting",
    install: "cargo install taplo-cli --locked",
    version_arg: Some("--version"),
};

/// Every server The Editor knows about, in the order they are offered.
pub const ALL: &[ServerSpec] = &[RUST_ANALYZER, RUFF, PYRIGHT, PYLSP, TAPLO];

/// The servers that serve a language, most important first.
///
/// Uses the language's identifier rather than `LanguageId` so this crate does
/// not depend on `editor-syntax` — the dependency would only run one way, but
/// the protocol layer has no business knowing about grammars.
#[must_use]
pub fn for_language(language: &str) -> Vec<ServerSpec> {
    match language {
        "rust" => vec![RUST_ANALYZER],
        // Ruff first: it is the one that will be installed, and its diagnostics
        // are the ones people act on most.
        "python" => vec![RUFF, PYRIGHT, PYLSP],
        "toml" => vec![TAPLO],
        _ => Vec::new(),
    }
}

/// The LSP `languageId` for a file extension.
///
/// These strings are defined by the protocol, not by us, so they are listed
/// rather than derived from anything.
#[must_use]
pub fn language_id_for_extension(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" | "pyw" | "pyi" => "python",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "js" | "mjs" | "cjs" => "javascript",
        "html" | "htm" => "html",
        "css" => "css",
        "md" | "markdown" => "markdown",
        _ => return None,
    })
}

/// Look for a server's executable, and check it actually runs.
///
/// The existence check alone is not enough, because a shim on `PATH` is
/// indistinguishable from the real thing until you run it:
///
/// * `~/.cargo/bin/rust-analyzer` exists whenever `rustup` is installed, even
///   when the component is not. Running it prints
///   "error: Unknown binary 'rust-analyzer.exe' in official toolchain" and
///   exits 1 — and speaking LSP to it produces an immediate end of stream that
///   looks exactly like a server crashing on startup.
/// * `pylsp` and friends can be left behind by an uninstalled virtual
///   environment.
///
/// So a candidate that has a `version_arg` has to answer it successfully.
#[must_use]
pub fn find(spec: ServerSpec, extra_path: &[PathBuf]) -> Option<Found> {
    spec.commands.iter().find_map(|command| {
        let program = which(command, extra_path)?;
        works(&program, spec.version_arg).then_some(Found { spec, program })
    })
}

/// Run a candidate's version flag to see whether it is real.
///
/// A spec with no version flag is taken on trust; a failure to *spawn* is
/// treated as "not usable", which is the same conclusion by a different route.
fn works(program: &Path, version_arg: Option<&str>) -> bool {
    let Some(arg) = version_arg else {
        return true;
    };
    match std::process::Command::new(program)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) => {
            if !output.status.success() {
                tracing::info!(
                    program = %program.display(),
                    "found but not usable: {}",
                    String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("no output")
                );
            }
            output.status.success()
        }
        Err(_) => false,
    }
}

/// Every server that is installed, for the toolchain check.
#[must_use]
pub fn find_all(extra_path: &[PathBuf]) -> Vec<Found> {
    ALL.iter()
        .filter_map(|spec| find(*spec, extra_path))
        .collect()
}

/// Which of a language's servers are available.
#[must_use]
pub fn available_for(language: &str, extra_path: &[PathBuf]) -> Vec<Found> {
    for_language(language)
        .into_iter()
        .filter_map(|spec| find(spec, extra_path))
        .collect()
}

/// Find an executable, checking `extra_path` before `PATH`.
///
/// `extra_path` is how a project virtual environment's `Scripts`/`bin`
/// directory gets searched first: a project with `ruff` installed in its venv
/// should use that one, not a different version installed globally.
#[must_use]
pub fn which(program: &str, extra_path: &[PathBuf]) -> Option<PathBuf> {
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_owned())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };

    let system: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    for dir in extra_path.iter().chain(system.iter()) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        for extension in &extensions {
            let with_extension = dir.join(format!("{program}{extension}"));
            if with_extension.is_file() {
                return Some(with_extension);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_is_served_by_rust_analyzer() {
        assert_eq!(for_language("rust"), vec![RUST_ANALYZER]);
    }

    #[test]
    fn python_gets_a_linter_and_a_type_checker() {
        let servers = for_language("python");
        assert!(
            servers.len() > 1,
            "neither ruff nor pyright does the other's job"
        );
        assert_eq!(
            servers.first().map(|s| s.id),
            Some("ruff"),
            "ruff is the one people will have installed, and its diagnostics \
             are the ones they act on"
        );
    }

    #[test]
    fn a_language_with_no_server_yields_an_empty_list_rather_than_a_default() {
        assert!(for_language("plaintext").is_empty());
        assert!(for_language("nonsense").is_empty());
    }

    #[test]
    fn language_ids_match_the_protocols_names() {
        // These are defined by the LSP specification; a server will reject or
        // ignore a document whose languageId it does not recognise.
        assert_eq!(language_id_for_extension("rs"), Some("rust"));
        assert_eq!(language_id_for_extension("py"), Some("python"));
        assert_eq!(language_id_for_extension("PY"), Some("python"));
        assert_eq!(language_id_for_extension("pyi"), Some("python"));
        assert_eq!(language_id_for_extension("toml"), Some("toml"));
        assert_eq!(language_id_for_extension("txt"), None);
    }

    #[test]
    fn every_server_has_the_arguments_it_needs_to_speak_lsp() {
        // Getting these wrong is a silent failure: the server starts, prints
        // its help, and never answers.
        assert_eq!(RUFF.args, ["server"]);
        assert_eq!(PYRIGHT.args, ["--stdio"]);
        assert_eq!(TAPLO.args, ["lsp", "stdio"]);
        assert!(RUST_ANALYZER.args.is_empty(), "it speaks LSP by default");
    }

    #[test]
    fn every_spec_is_described_for_the_toolchain_check() {
        for spec in ALL {
            assert!(!spec.id.is_empty());
            assert!(!spec.name.is_empty());
            assert!(!spec.provides.is_empty(), "{} says nothing useful", spec.id);
            assert!(!spec.commands.is_empty(), "{} has no executable", spec.id);
            // Naming a tool the user has not got, without saying how to get it,
            // is the report that started this: three names and no next step.
            assert!(
                !spec.install.is_empty(),
                "{} can be reported missing with no way to install it",
                spec.id
            );
        }
    }

    #[test]
    fn server_ids_are_unique_so_diagnostics_can_be_attributed() {
        let mut seen = std::collections::HashSet::new();
        for spec in ALL {
            assert!(seen.insert(spec.id), "duplicate server id {}", spec.id);
        }
    }

    #[test]
    fn a_missing_server_is_absent_rather_than_an_error() {
        let missing = ServerSpec {
            id: "nope",
            name: "Nope",
            commands: &["definitely-not-a-real-language-server-xyzzy"],
            args: &[],
            provides: "nothing",
            install: "",
            version_arg: None,
        };
        assert_eq!(find(missing, &[]), None);
    }

    #[test]
    fn extra_path_entries_are_searched_before_the_system_path() {
        // A project venv's ruff must win over a different version installed
        // globally, or the diagnostics do not match the project's config.
        let dir = std::env::temp_dir().join("the-editor-lsp-which");
        std::fs::create_dir_all(&dir).expect("create dir");
        let name = if cfg!(windows) {
            "fake-server.exe"
        } else {
            "fake-server"
        };
        let planted = dir.join(name);
        std::fs::write(&planted, b"stub").expect("plant");

        let found = which("fake-server", std::slice::from_ref(&dir)).expect("finds the stub");
        // Compared case-insensitively: on Windows the extension comes from
        // PATHEXT (`.EXE`) while the file on disk is `.exe`, which matters not
        // at all for running it and would make an exact compare fail.
        assert_eq!(
            found.display().to_string().to_lowercase(),
            planted.display().to_string().to_lowercase()
        );
        assert!(found.is_file());

        // ...and it is not found without that directory.
        assert_eq!(which("fake-server", &[]), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression: a binary existing on `PATH` does not mean it works.
    ///
    /// `~/.cargo/bin/rust-analyzer` exists whenever rustup is installed, even
    /// when the component is not; running it exits 1 with "Unknown binary".
    /// Discovery that only checked for the file started it, got an immediate
    /// end of stream, and reported the server as crashing on startup.
    #[test]
    fn a_binary_that_fails_its_version_check_is_not_offered() {
        let dir = std::env::temp_dir().join("the-editor-lsp-shim");
        std::fs::create_dir_all(&dir).expect("create dir");

        // A "server" that exists and is executable but always fails, standing
        // in for a rustup proxy with its component uninstalled.
        let (name, body) = if cfg!(windows) {
            (
                "fake-shim.cmd",
                "@echo Unknown binary 1>&2\r\n@exit /b 1\r\n",
            )
        } else {
            (
                "fake-shim",
                "#!/bin/sh\necho 'Unknown binary' >&2\nexit 1\n",
            )
        };
        let shim = dir.join(name);
        std::fs::write(&shim, body).expect("plant shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .expect("make executable");
        }

        let spec = ServerSpec {
            id: "shim",
            name: "Shim",
            commands: &["fake-shim"],
            args: &[],
            provides: "nothing",
            install: "",
            version_arg: Some("--version"),
        };
        assert_eq!(
            find(spec, std::slice::from_ref(&dir)),
            None,
            "a binary that fails its version check must not be offered"
        );

        // ...and without a version check it is taken on trust, which is the
        // documented behaviour for servers that have no such flag.
        let unchecked = ServerSpec {
            version_arg: None,
            ..spec
        };
        assert!(find(unchecked, std::slice::from_ref(&dir)).is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_offered_server_answers_its_version_flag() {
        // Whatever is installed on this machine, anything discovery offers must
        // actually run — that is the entire point of the check.
        for found in find_all(&[]) {
            let Some(arg) = found.spec.version_arg else {
                continue;
            };
            let output = std::process::Command::new(&found.program)
                .arg(arg)
                .output()
                .unwrap_or_else(|e| panic!("{} could not run: {e}", found.program.display()));
            assert!(
                output.status.success(),
                "{} was offered but fails {arg}",
                found.program.display()
            );
        }
    }

    #[test]
    fn discovery_never_panics_whatever_is_installed() {
        // Whatever this machine has, listing servers must work.
        let found = find_all(&[]);
        for server in &found {
            assert!(server.program.is_file());
        }
        let _ = available_for("python", &[]);
        let _ = available_for("nonsense", &[]);
    }
}
