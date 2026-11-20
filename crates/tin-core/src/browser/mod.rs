//! Finds the tab the user is looking at by reading browsers' own session files.
//!
//! No extension and no remote debugging: Chromium-family browsers and Firefox both keep a session
//! file on disk that names the active tab. Tin reads the most recently written one, preferring the
//! browser that owns the focused window when that's known.

pub mod chromium;
pub mod firefox;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A browser tab's address and title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub url: String,
    pub title: Option<String>,
}

/// The active tab of one browser, and when its session file was last written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserTab {
    pub browser: &'static str,
    pub tab: Tab,
    pub written: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Chromium,
    Firefox,
}

/// One browser Tin knows how to read.
struct Known {
    name: &'static str,
    engine: Engine,
    /// Words that identify its window: app ids, process or app names (lowercase).
    window_hints: &'static [&'static str],
    /// Data folders relative to the platform's config folder (`~/.config`, or
    /// `~/Library/Application Support` on macOS) or, when starting with `~/`, the home folder.
    roots: &'static [&'static str],
}

const KNOWN: &[Known] = &[
    Known { name: "Helium", engine: Engine::Chromium, window_hints: &["helium"], roots: &["net.imput.helium"] },
    Known {
        name: "Google Chrome",
        engine: Engine::Chromium,
        window_hints: &["google-chrome", "google chrome", "chrome"],
        roots: &["google-chrome", "google-chrome-beta", "Google/Chrome", "Google/Chrome Beta"],
    },
    Known { name: "Chromium", engine: Engine::Chromium, window_hints: &["chromium"], roots: &["chromium", "Chromium"] },
    Known {
        name: "Brave",
        engine: Engine::Chromium,
        window_hints: &["brave"],
        roots: &["BraveSoftware/Brave-Browser"],
    },
    Known {
        name: "Microsoft Edge",
        engine: Engine::Chromium,
        window_hints: &["microsoft-edge", "microsoft edge", "msedge"],
        roots: &["microsoft-edge", "Microsoft Edge"],
    },
    Known { name: "Vivaldi", engine: Engine::Chromium, window_hints: &["vivaldi"], roots: &["vivaldi", "Vivaldi"] },
    Known { name: "Arc", engine: Engine::Chromium, window_hints: &["arc"], roots: &["Arc/User Data"] },
    Known { name: "Thorium", engine: Engine::Chromium, window_hints: &["thorium"], roots: &["thorium", "Thorium"] },
    Known {
        name: "Firefox",
        engine: Engine::Firefox,
        window_hints: &["firefox"],
        roots: &["~/.mozilla/firefox", "mozilla/firefox", "Firefox/Profiles"],
    },
    Known { name: "Zen", engine: Engine::Firefox, window_hints: &["zen"], roots: &["~/.zen", "zen/Profiles"] },
    Known { name: "LibreWolf", engine: Engine::Firefox, window_hints: &["librewolf"], roots: &["~/.librewolf"] },
];

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("no browser session file found (looked for Chromium-family and Firefox profiles)")]
    NoSession,
    #[error("couldn't read {browser}'s session at {path}: {reason}")]
    Unreadable { browser: &'static str, path: PathBuf, reason: String },
}

/// Whether a tab with this title is what a window with this title shows: browsers title their
/// windows "<tab title> - <browser>", sometimes with a profile name between, and some insert
/// invisible characters.
fn titles_window(tab: &str, window: &str) -> bool {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| !matches!(c, '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}'))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (tab, window) = (clean(tab), clean(window));
    tab.chars().count() >= 3
        && (window == tab || window.starts_with(&format!("{tab} - ")) || window.starts_with(&format!("{tab} — ")))
}

/// True when a window's app id or title names a browser Tin can read.
pub fn is_browser_window(app: &str) -> bool {
    hinted(app).is_some()
}

/// A hint matches a whole word of the app id (`arc` doesn't match `search`), or, when it has
/// several words itself (`google-chrome`), appears anywhere in it.
fn hinted(app: &str) -> Option<&'static Known> {
    let app = app.to_lowercase();
    let words: Vec<&str> = app.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let matches = |hint: &str| {
        if hint.contains(['-', ' ']) { app.contains(hint) } else { words.contains(&hint) }
    };
    KNOWN.iter().find(|k| k.window_hints.iter().any(|h| matches(h)))
}

/// The active tab of the most recently used browser, or of the one `window_app` names.
pub fn active_tab(window_app: Option<&str>, window_title: Option<&str>) -> Result<BrowserTab, BrowserError> {
    let preferred = window_app.and_then(hinted);
    let browsers: Vec<&Known> = match preferred {
        Some(k) => vec![k],
        None => KNOWN.iter().collect(),
    };
    let mut sessions: Vec<(SystemTime, &Known, PathBuf)> = browsers
        .into_iter()
        .flat_map(|k| session_files(k).into_iter().map(move |p| (k, p)))
        .filter_map(|(k, p)| Some((fs::metadata(&p).and_then(|m| m.modified()).ok()?, k, p)))
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.0));
    let (written, known, path) = sessions.into_iter().next().ok_or(BrowserError::NoSession)?;
    let unreadable = |reason: String| BrowserError::Unreadable { browser: known.name, path: path.clone(), reason };
    let data = fs::read(&path).map_err(|e| unreadable(e.to_string()))?;
    let tab = match known.engine {
        Engine::Chromium => chromium::active_tab(&data, window_title).map_err(|e| unreadable(e.to_string()))?,
        Engine::Firefox => firefox::active_tab(&data, window_title).map_err(|e| unreadable(e.to_string()))?,
    };
    Ok(BrowserTab { browser: known.name, tab, written })
}

fn session_files(known: &Known) -> Vec<PathBuf> {
    known
        .roots
        .iter()
        .filter_map(|r| resolve_root(r))
        .filter(|r| r.is_dir())
        .flat_map(|root| profiles(&root))
        .flat_map(|profile| match known.engine {
            Engine::Chromium => newest_chromium_session(&profile).into_iter().collect::<Vec<_>>(),
            Engine::Firefox => {
                let p = profile.join("sessionstore-backups").join("recovery.jsonlz4");
                if p.is_file() { vec![p] } else { vec![] }
            }
        })
        .collect()
}

fn resolve_root(root: &str) -> Option<PathBuf> {
    match root.strip_prefix("~/") {
        Some(rest) => Some(dirs::home_dir()?.join(rest)),
        None => Some(dirs::config_dir()?.join(root)),
    }
}

fn profiles(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()
}

fn newest_chromium_session(profile: &Path) -> Option<PathBuf> {
    fs::read_dir(profile.join("Sessions"))
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("Session_"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_hints_recognise_browsers_by_app_id() {
        assert!(is_browser_window("net.imput.helium"));
        assert!(is_browser_window("Google-chrome"));
        assert!(is_browser_window("org.mozilla.firefox"));
        assert!(!is_browser_window("com.mitchellh.ghostty"));
        assert!(!is_browser_window("org.gnome.Search"));
    }

    #[test]
    fn a_tab_titles_a_window_up_to_the_browsers_suffix() {
        assert!(titles_window("Ludwig - Twitch", "Ludwig - Twitch - Helium"));
        assert!(titles_window("Page", "Page - Profile 1 - Microsoft\u{200b} Edge"));
        assert!(titles_window("Docs", "Docs — Mozilla Firefox"));
        assert!(!titles_window("(1) Home / X", "Ludwig - Twitch - Helium"));
    }
}
