//! UI theme selection: dark, light, or follow the operating system.
//!
//! This is the *chrome* theme — panels, menus, tabs, the file tree, dialogs.
//! The syntax theme that colours the code pane is a separate thing living in
//! `editor-syntax`, so that a dark frame around a light editor pane (or the
//! reverse) is expressible. See PLAN.md §3.11.
//!
//! Nothing in here mentions egui. The mapping from [`ResolvedTheme`] to actual
//! `Visuals` is `editor-widgets`' job, which keeps this crate toolkit-free and
//! testable.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// The default. An IDE is stared at for hours; dark is the safer default
    /// and the one most users of this kind of tool expect.
    #[default]
    Dark,
    Light,
    /// Track the OS preference, and keep tracking it if the user changes it
    /// while The Editor is running.
    System,
}

/// What the preference resolves to once the OS has been consulted. This is
/// what actually gets painted, and it is always one or the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResolvedTheme {
    #[default]
    Dark,
    Light,
}

impl ThemePreference {
    /// Every value, in menu order.
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::System];

    /// Resolve against the OS preference.
    ///
    /// `system` is whatever the platform reports, or `None` if it could not be
    /// determined — in which case we fall back to dark rather than guessing.
    #[must_use]
    pub fn resolve(self, system: Option<ResolvedTheme>) -> ResolvedTheme {
        match self {
            Self::Dark => ResolvedTheme::Dark,
            Self::Light => ResolvedTheme::Light,
            Self::System => system.unwrap_or(ResolvedTheme::Dark),
        }
    }

    /// Label for menus and the command palette.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::System => "Follow System",
        }
    }
}

impl fmt::Display for ThemePreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
        })
    }
}

impl FromStr for ThemePreference {
    type Err = UnknownTheme;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Ok(Self::Dark),
            "light" => Ok(Self::Light),
            "system" | "auto" | "follow_system" => Ok(Self::System),
            other => Err(UnknownTheme(other.to_owned())),
        }
    }
}

/// A theme name that isn't one of the three we understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTheme(pub String);

impl fmt::Display for UnknownTheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown theme {:?} (expected \"dark\", \"light\" or \"system\")",
            self.0
        )
    }
}

impl std::error::Error for UnknownTheme {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_the_default() {
        assert_eq!(ThemePreference::default(), ThemePreference::Dark);
        assert_eq!(
            ThemePreference::default().resolve(Some(ResolvedTheme::Light)),
            ResolvedTheme::Dark,
            "an explicit choice must override the OS preference"
        );
    }

    #[test]
    fn system_follows_the_os_and_falls_back_to_dark() {
        let sys = ThemePreference::System;
        assert_eq!(
            sys.resolve(Some(ResolvedTheme::Light)),
            ResolvedTheme::Light
        );
        assert_eq!(sys.resolve(Some(ResolvedTheme::Dark)), ResolvedTheme::Dark);
        assert_eq!(
            sys.resolve(None),
            ResolvedTheme::Dark,
            "an unreadable OS preference must not leave the UI unstyled"
        );
    }

    #[test]
    fn parses_what_users_actually_write() {
        for (input, expected) in [
            ("dark", ThemePreference::Dark),
            ("Light", ThemePreference::Light),
            ("  SYSTEM ", ThemePreference::System),
            ("auto", ThemePreference::System),
        ] {
            assert_eq!(input.parse::<ThemePreference>(), Ok(expected), "{input}");
        }
        assert!("solarized".parse::<ThemePreference>().is_err());
    }

    #[test]
    fn display_round_trips_through_from_str() {
        for pref in ThemePreference::ALL {
            assert_eq!(pref.to_string().parse::<ThemePreference>(), Ok(pref));
        }
    }
}
