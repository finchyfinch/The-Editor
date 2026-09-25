//! Per-platform locations for config, data, logs, backups and session state.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | config | `%APPDATA%\Gareth Finch\The Editor\config` | `~/.config/the-editor` | `~/Library/Application Support/The Editor` |
//! | data | `%APPDATA%\Gareth Finch\The Editor\data` | `~/.local/share/the-editor` | `~/Library/Application Support/The Editor` |
//!
//! A portable build (an `the-editor.portable` marker file next to the
//! executable) redirects everything into a `data/` folder beside the binary,
//! so The Editor can run from a USB stick without touching the host profile.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;

use crate::APP_DIRS;

/// Resolved application directories. Created on demand, never lazily assumed
/// to exist — a user can delete them between runs.
#[derive(Debug, Clone)]
pub struct AppPaths {
    config: PathBuf,
    data: PathBuf,
    portable: bool,
}

impl AppPaths {
    /// Resolve the directories for this installation.
    ///
    /// # Errors
    /// If the platform provides no home directory at all.
    pub fn resolve() -> Result<Self> {
        if let Some(root) = portable_root() {
            return Ok(Self {
                config: root.join("config"),
                data: root.join("data"),
                portable: true,
            });
        }

        let (qualifier, org, app) = APP_DIRS;
        let dirs = ProjectDirs::from(qualifier, org, app)
            .context("no home directory available for this user")?;

        Ok(Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
            portable: false,
        })
    }

    /// True if running as a portable installation.
    #[must_use]
    pub fn is_portable(&self) -> bool {
        self.portable
    }

    /// Directory holding `settings.toml`, `keymap.toml` and user themes.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// User settings file.
    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    /// Which folders may run their own tools; see [`crate::trust`].
    #[must_use]
    pub fn trust_file(&self) -> PathBuf {
        self.config.join("trusted_folders.toml")
    }

    /// User keybinding overrides.
    #[must_use]
    pub fn keymap_file(&self) -> PathBuf {
        self.config.join("keymap.toml")
    }

    /// User-supplied colour themes, merged over the built-in ones.
    #[must_use]
    pub fn themes_dir(&self) -> PathBuf {
        self.config.join("themes")
    }

    /// User-supplied New File templates, merged over the built-in ones.
    #[must_use]
    pub fn templates_dir(&self) -> PathBuf {
        self.config.join("templates")
    }

    /// Rolling log files.
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.data.join("logs")
    }

    /// Autosaved copies of dirty buffers, for crash recovery.
    #[must_use]
    pub fn backup_dir(&self) -> PathBuf {
        self.data.join("backups")
    }

    /// Open tabs, cursor positions, window geometry, recent projects.
    #[must_use]
    pub fn session_file(&self) -> PathBuf {
        self.data.join("session.toml")
    }

    /// Create every directory The Editor writes to.
    ///
    /// # Errors
    /// If any directory cannot be created.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            self.config.clone(),
            self.themes_dir(),
            self.templates_dir(),
            self.data.clone(),
            self.log_dir(),
            self.backup_dir(),
        ] {
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(())
    }
}

/// A portable install is signalled by a marker file next to the executable.
fn portable_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    dir.join("the-editor.portable")
        .exists()
        .then(|| dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_lives_under_config_or_data() {
        let paths = AppPaths {
            config: PathBuf::from("/cfg"),
            data: PathBuf::from("/data"),
            portable: false,
        };
        assert!(paths.settings_file().starts_with("/cfg"));
        assert!(paths.keymap_file().starts_with("/cfg"));
        assert!(paths.themes_dir().starts_with("/cfg"));
        assert!(paths.log_dir().starts_with("/data"));
        assert!(paths.backup_dir().starts_with("/data"));
        assert!(paths.session_file().starts_with("/data"));
    }
}
