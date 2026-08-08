//! What the command line asked for.
//!
//! `the-editor foo.py`, `the-editor .`, and Explorer's "Open with" all arrive
//! the same way: as paths in `argv`. Without this, the only way to open a file
//! is to already have the window in front of you, which rules out the shell
//! habit (`e main.rs`), file associations, and every "open this in your editor"
//! button in another program.
//!
//! Flag parsing is separated from touching the filesystem so it can be tested
//! without one. Deciding whether a path is a folder or a file is left to the
//! caller, which is doing filesystem work anyway.

use std::ffi::OsString;
use std::path::PathBuf;

/// What to do about this invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Startup {
    /// Open the window, with these paths.
    Open(Vec<PathBuf>),
    /// Print this and exit without a window. Usage, the version, or an error.
    Print { text: String, failed: bool },
}

/// Usage, kept here next to the parsing that has to agree with it.
const USAGE: &str = "\
The Editor - an IDE for Python and Rust

USAGE:
    the-editor [PATH]...

ARGS:
    <PATH>...    Files to open, and a folder to open as the project. A folder
                 given alongside files becomes the project; several folders and
                 only the first is used.

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print the version

With no arguments, the previous session is restored if that is switched on in
Settings.";

/// Read `args`, which must *not* include the program name.
pub(crate) fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Startup {
    let mut paths = Vec::new();
    let mut literal = false;

    for arg in args {
        // `--` ends option parsing, so a file genuinely named `-h` is openable.
        if !literal && arg == "--" {
            literal = true;
            continue;
        }
        if !literal
            && let Some(text) = arg.to_str()
            && text.starts_with('-')
            && text != "-"
        {
            match text {
                "-h" | "--help" => {
                    return Startup::Print {
                        text: USAGE.to_owned(),
                        failed: false,
                    };
                }
                "-V" | "--version" => {
                    return Startup::Print {
                        text: format!("The Editor {}", env!("CARGO_PKG_VERSION")),
                        failed: false,
                    };
                }
                // Refuse rather than treating it as a filename. Silently
                // opening a file called `--recurse` because the flag was
                // misremembered is worse than saying so.
                other => {
                    return Startup::Print {
                        text: format!("Unknown option `{other}`\n\n{USAGE}"),
                        failed: true,
                    };
                }
            }
        }
        paths.push(PathBuf::from(arg));
    }

    Startup::Open(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments_opens_nothing_in_particular() {
        assert_eq!(parse(args(&[])), Startup::Open(Vec::new()));
    }

    #[test]
    fn paths_are_collected_in_the_order_given() {
        assert_eq!(
            parse(args(&["a.py", "../b.rs", "."])),
            Startup::Open(vec![
                PathBuf::from("a.py"),
                PathBuf::from("../b.rs"),
                PathBuf::from("."),
            ])
        );
    }

    #[test]
    fn help_and_version_ask_for_output_rather_than_a_window() {
        for flag in ["-h", "--help"] {
            let Startup::Print { text, failed } = parse(args(&[flag])) else {
                panic!("{flag} should print");
            };
            assert!(text.contains("USAGE"), "{flag}: {text}");
            assert!(!failed);
        }
        for flag in ["-V", "--version"] {
            let Startup::Print { text, failed } = parse(args(&[flag])) else {
                panic!("{flag} should print");
            };
            assert!(text.contains(env!("CARGO_PKG_VERSION")), "{flag}: {text}");
            assert!(!failed);
        }
    }

    /// An unrecognised flag must not become a filename. Creating or opening a
    /// file called `--recurse` because someone misremembered an option is a
    /// worse outcome than being told the option does not exist.
    #[test]
    fn an_unknown_option_is_refused_and_reported_as_a_failure() {
        let Startup::Print { text, failed } = parse(args(&["--recurse", "src"])) else {
            panic!("an unknown option should not open a window");
        };
        assert!(text.contains("--recurse"), "{text}");
        assert!(failed, "the exit status has to say it went wrong");
    }

    /// Files really can be named `-h`, and `--` is how every other tool says
    /// "stop reading options".
    #[test]
    fn a_double_dash_makes_everything_after_it_a_path() {
        assert_eq!(
            parse(args(&["--", "-h", "--version"])),
            Startup::Open(vec![PathBuf::from("-h"), PathBuf::from("--version")])
        );
    }

    /// A bare `-` is a filename here, not an option: The Editor has no use for
    /// standard input, so treating it as one would only produce a confusing
    /// "unknown option" for a path that might well exist.
    #[test]
    fn a_bare_dash_is_treated_as_a_path() {
        assert_eq!(parse(args(&["-"])), Startup::Open(vec![PathBuf::from("-")]));
    }

    /// The help text has to actually mention the options it accepts, or it is
    /// worse than none.
    #[test]
    fn the_usage_text_documents_every_flag_that_is_accepted() {
        for flag in ["-h", "--help", "-V", "--version"] {
            assert!(USAGE.contains(flag), "usage does not mention {flag}");
        }
    }
}
