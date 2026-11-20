//! `config.toml`: routes, project folders, hooks, and how to reach Hex.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Folders whose child folders count as existing projects. Unset means the usual
    /// `~/dev`, `~/code`, `~/src`, `~/projects` and `~/Developer` that exist.
    #[serde(default)]
    pub project_roots: Option<Vec<String>>,
    #[serde(default, rename = "route")]
    pub routes: Vec<RouteConfig>,
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(default)]
    pub hex: HexConfig,
    #[serde(default)]
    pub app: AppConfig,
}

/// A name the user can say to route a capture.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    pub name: String,
    /// Other ways to say it: `ask blog`, `blog.com`.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// A project folder. A route without one is a topic: it tags, but names no project.
    #[serde(default)]
    pub path: Option<String>,
    /// Runs after a capture routes here, with the capture's JSON on stdin.
    #[serde(default)]
    pub hook: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    /// Runs after every capture, with the capture's JSON on stdin.
    #[serde(default)]
    pub on_capture: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HexConfig {
    /// The `hex` executable; `hex` on `PATH` when unset.
    #[serde(default)]
    pub bin: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// The global shortcut Tin's window registers, such as `ctrl+alt+c`.
    #[serde(default)]
    pub shortcut: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("couldn't read the config at {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("the config at {path} isn't valid: {source}Fix the file, or move it aside to start from the defaults.")]
    Parse { path: PathBuf, source: Box<toml::de::Error> },
}

pub const STARTER: &str = r#"# Tin's config. Routes let you say what a capture is for ("ask blog", "game dev")
# and have it land tagged for the right project.

# Folders whose subfolders count as existing projects (say "for <folder name>").
# project_roots = ["~/dev"]

# [[route]]
# name = "blog"
# aliases = ["ask blog", "blog.com"]
# path = "~/dev/blog"                 # a project; leave out for a topic tag
# hook = ["opencode", "run", "--dir", "~/dev/blog", "Triage the capture on stdin"]

# [[route]]
# name = "game dev"                    # a topic: tags captures, names no project

# [hooks]
# on_capture = ["notify-send", "Tin", "Captured"]   # gets the capture JSON on stdin

# [hex]
# bin = "hex"

# [app]
# shortcut = "ctrl+alt+c"
"#;

impl Config {
    /// Reads the config, or the defaults when there is no file.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|source| ConfigError::Parse { path: path.to_owned(), source: Box::new(source) }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(ConfigError::Read { path: path.to_owned(), source }),
        }
    }

    pub fn project_roots(&self) -> Vec<PathBuf> {
        match &self.project_roots {
            Some(roots) => roots.iter().map(|r| expand_home(r)).collect(),
            None => ["dev", "code", "src", "projects", "Developer"]
                .iter()
                .filter_map(|d| dirs::home_dir().map(|h| h.join(d)))
                .filter(|p| p.is_dir())
                .collect(),
        }
    }

    pub fn hex_bin(&self) -> &str {
        self.hex.bin.as_deref().unwrap_or("hex")
    }
}

/// `~/x` to `$HOME/x`; anything else as is.
pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_config_parses_and_its_example_does_too() {
        assert_eq!(toml::from_str::<Config>(STARTER).ok(), Some(Config::default()));
        let uncommented: String = STARTER
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| l.contains('=') || l.starts_with('['))
            .collect::<Vec<_>>()
            .join("\n");
        let config: Config = toml::from_str(&uncommented).unwrap_or_else(|e| panic!("{e}\n{uncommented}"));
        assert_eq!(config.routes.len(), 2);
        assert_eq!(config.routes[0].aliases, ["ask blog", "blog.com"]);
        assert_eq!(config.app.shortcut.as_deref(), Some("ctrl+alt+c"));
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_a_typo_is_an_error() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(Config::load(&dir.path().join("none.toml")).ok(), Some(Config::default()));
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "[[rout]]\nname = 'x'\n").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(Config::load(&bad), Err(ConfigError::Parse { .. })));
    }
}
