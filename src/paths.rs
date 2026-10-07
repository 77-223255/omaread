//! Where omaread keeps its files, and the settings that point there.
//!
//! Three locations with three jobs, following the XDG base directories:
//!
//! - `~/.config/omaread/config.toml` holds settings and belongs in version
//!   control.
//! - `~/.local/share/omaread/` holds the read model, which is derived and may
//!   be deleted at any time.
//! - The journal directory holds the source of truth. It is one local file,
//!   rewritten in place when it is folded; it is not meant to be shared
//!   between machines.
//!
//! Omarchy's theme files are answered from here too: the reader follows a
//! theme Omarchy renders, and Omarchy keeps its own files by the same rules.
//! One environment — `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` —
//! therefore decides every path the program reads or writes, and with the
//! variables unset every path is the home-directory path it has always been.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const APP: &str = "omaread";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Directory holding the journal files. One file per machine.
    pub journal_dir: Option<PathBuf>,
    /// Maximum reading width in columns. `None` uses the full window.
    pub max_width: Option<u16>,
    /// How to draw pictures: `kitty`, `sixel` or `half-blocks`. `None` asks the
    /// terminal.
    pub images: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_file()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("{} is malformed", path.display()))
    }

    /// Writes a commented starter file, unless one exists already.
    pub fn write_default_if_missing() -> Result<()> {
        let path = config_file()?;
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let default_journal = data_dir()?.join("journal");
        let contents = format!(
            "# omaread settings\n\
             \n\
             # Where the journal lives: one local file, the source of truth for\n\
             # reading positions. It is folded in place as it grows, so it is\n\
             # not meant to be shared between machines.\n\
             # journal_dir = \"{}\"\n\
             \n\
             # Reading width in columns. Comment out to use the full window.\n\
             # max_width = 66\n\
             \n\
             # How pictures are drawn. Left out, the terminal is asked and the\n\
             # best of kitty, sixel and half-blocks is used. Inside tmux only\n\
             # half-blocks work, because tmux manages the screen itself.\n\
             # images = \"sixel\"\n",
            default_journal.display()
        );
        std::fs::write(&path, contents)
            .with_context(|| format!("cannot write {}", path.display()))?;
        Ok(())
    }

    pub fn journal_dir(&self) -> Result<PathBuf> {
        match &self.journal_dir {
            Some(dir) => Ok(expand_tilde(dir)),
            None => Ok(data_dir()?.join("journal")),
        }
    }
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// The base configuration directory: `XDG_CONFIG_HOME`, else `~/.config`.
///
/// Omaread's settings and Omarchy's theme files are both built on this, so
/// the one variable moves every configuration path together.
fn config_base() -> Result<PathBuf> {
    dirs::config_dir().context("cannot determine the configuration directory")
}

/// The base state directory: `XDG_STATE_HOME`, else `~/.local/state`.
fn state_base() -> Result<PathBuf> {
    dirs::state_dir().context("cannot determine the state directory")
}

fn config_dir() -> Result<PathBuf> {
    Ok(config_base()?.join(APP))
}

/// Where Omarchy renders the template this reader installed. The template
/// belongs to Omarchy, so it lives in Omarchy's own `themed` directory under
/// the configuration directory — wherever the environment says that is.
pub fn omarchy_theme_template() -> Result<PathBuf> {
    Ok(config_base()?.join("omarchy/themed/omaread.toml.tpl"))
}

/// A file Omarchy keeps in both the state and the config directories,
/// newest layout first.
///
/// Quattro moved the current theme out of the config directory and into the
/// state one, and a machine may run either release, so both are offered and
/// the first that exists wins. The state directory comes first because that
/// is where the running release writes.
fn omarchy_candidates(config: &Path, state: &Path, relative: &str) -> Vec<PathBuf> {
    vec![state.join(relative), config.join(relative)]
}

/// Where Omarchy writes the rendered theme file this reader follows.
pub fn omarchy_theme_files() -> Result<Vec<PathBuf>> {
    Ok(omarchy_candidates(
        &config_base()?,
        &state_base()?,
        "omarchy/current/theme/omaread.toml",
    ))
}

/// Where Omarchy records the name of the theme in use.
pub fn omarchy_theme_name_files() -> Result<Vec<PathBuf>> {
    Ok(omarchy_candidates(
        &config_base()?,
        &state_base()?,
        "omarchy/current/theme.name",
    ))
}

/// Derived data: the read model and anything else that can be rebuilt.
pub fn data_dir() -> Result<PathBuf> {
    dirs::data_dir()
        .map(|d| d.join(APP))
        .context("cannot determine the data directory")
}

/// Replaces a leading `~` with the home directory.
fn expand_tilde(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => path.to_path_buf(),
        },
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_read_from_toml_and_absent_values_stay_default() {
        let config: Config = toml::from_str("max_width = 72\njournal_dir = \"~/box\"\n").unwrap();
        assert_eq!(config.max_width, Some(72));
        assert_eq!(config.journal_dir, Some(PathBuf::from("~/box")));

        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.max_width, None);
        assert_eq!(config.journal_dir, None);
    }

    #[test]
    fn omarchy_files_ask_the_state_directory_first() {
        // The current theme moved from the config directory to the state one,
        // and a machine may run either release. Both candidates must be built
        // from the base directories given, state first, or one variable would
        // move half of what the reader follows.
        let config = PathBuf::from("/cfg");
        let state = PathBuf::from("/state");
        assert_eq!(
            omarchy_candidates(&config, &state, "omarchy/current/theme/omaread.toml"),
            vec![
                PathBuf::from("/state/omarchy/current/theme/omaread.toml"),
                PathBuf::from("/cfg/omarchy/current/theme/omaread.toml"),
            ]
        );
        assert_eq!(
            omarchy_candidates(&config, &state, "omarchy/current/theme.name"),
            vec![
                PathBuf::from("/state/omarchy/current/theme.name"),
                PathBuf::from("/cfg/omarchy/current/theme.name"),
            ]
        );
    }
}
