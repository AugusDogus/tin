//! What Tin's browser extension reports: each window's active tab (and, later, selected text
//! and tweets), kept in small files under `<data>/browser/` by Tin's native-messaging helper.
//!
//! The browser starts the helper (the `tin` command) for each message; it applies the message to
//! the browser's state file and exits. Tin reads the files when it captures, so the extension
//! works whether or not Tin's window app is running.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The extension's ID, fixed by the public key in `extension/manifest.json`.
pub const EXTENSION_ID: &str = "geidgjnjciocnkjggolocgfgfdbnmilo";
/// The native-messaging host name the extension talks to.
pub const HOST_NAME: &str = "dev.tin.host";

/// One message from the extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// A window's active tab changed (or its page did).
    Tab {
        window: i64,
        url: String,
        title: String,
    },
    /// A window took focus.
    Focus {
        window: i64,
    },
    Closed {
        window: i64,
    },
    /// The text selected in a window's page (empty when nothing is).
    Selection {
        window: i64,
        url: String,
        text: String,
    },
    /// Tweets X's app loaded, whole: kept so a captured tweet's full text and media are known.
    Tweets {
        tweets: Vec<CachedTweet>,
    },
}

/// A tweet as Tin's extension saw it in X's own data (long posts in full).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTweet {
    pub id: String,
    pub handle: String,
    #[serde(default)]
    pub author: Option<String>,
    pub text: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub quoted_id: Option<String>,
    #[serde(default)]
    pub media: Vec<CachedMedia>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMedia {
    /// `photo`, `video` or `animated_gif`.
    pub kind: String,
    pub url: String,
}

/// One browser's windows, as the extension last reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserState {
    #[serde(default)]
    pub windows: BTreeMap<i64, Tab>,
    #[serde(default)]
    pub focused: Option<i64>,
    /// Each window's selected text, while something is selected.
    #[serde(default)]
    pub selections: BTreeMap<i64, Selected>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selected {
    pub url: String,
    pub text: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub url: String,
    pub title: String,
    /// When the extension reported it, in milliseconds since the Unix epoch.
    pub at_ms: u64,
}

impl BrowserState {
    pub fn apply(&mut self, message: Message, now_ms: u64) {
        match message {
            Message::Tab { window, url, title } => {
                self.windows.insert(window, Tab { url, title, at_ms: now_ms });
            }
            Message::Focus { window } => self.focused = Some(window),
            Message::Selection { window, url, text } => {
                if text.trim().is_empty() {
                    self.selections.remove(&window);
                } else {
                    self.selections.insert(window, Selected { url, text, at_ms: now_ms });
                }
            }
            // Tweets are kept as files of their own (see `record`), not in the browser's state.
            Message::Tweets { .. } => {}
            Message::Closed { window } => {
                self.windows.remove(&window);
                self.selections.remove(&window);
                if self.focused == Some(window) {
                    self.focused = None;
                }
            }
        }
    }

    /// The tab a window titled `window_title` shows: browsers title windows after their active
    /// tab ("<tab> - Helium", sometimes with a profile name between).
    pub fn tab_for(&self, window_title: &str) -> Option<&Tab> {
        self.window_for(window_title).and_then(|w| self.windows.get(&w))
    }

    /// The extension's id for the window titled `window_title`.
    pub fn window_for(&self, window_title: &str) -> Option<i64> {
        self.windows
            .iter()
            .filter(|(_, t)| crate::browser::titles_window(&t.title, window_title))
            .max_by_key(|(_, t)| (t.title.len(), t.at_ms))
            .map(|(id, _)| *id)
    }

    /// The text selected in the window titled `window_title`, if any.
    pub fn selection_for(&self, window_title: &str) -> Option<&Selected> {
        self.window_for(window_title).and_then(|w| self.selections.get(&w))
    }
}

/// Where the extension's reports live.
pub fn dir(data: &Path) -> PathBuf {
    data.join("browser")
}

fn state_path(data: &Path, browser: &str) -> PathBuf {
    dir(data).join(format!("{browser}.json"))
}

pub fn load(data: &Path, browser: &str) -> BrowserState {
    std::fs::read(state_path(data, browser)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Every browser's state, by browser name.
pub fn load_all(data: &Path) -> Vec<(String, BrowserState)> {
    let Ok(entries) = std::fs::read_dir(dir(data)) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.strip_suffix(".json")?.to_owned();
            Some((name.clone(), load(data, &name)))
        })
        .collect()
}

/// The text selected in the browser window titled `window_title`, from any browser's report.
pub fn selection_for(data: &Path, window_title: &str) -> Option<Selected> {
    load_all(data).into_iter().find_map(|(_, state)| state.selection_for(window_title).cloned())
}

/// Applies one message to the browser's state file (written whole, then renamed into place).
pub fn record(data: &Path, browser: &str, message: Message) -> std::io::Result<()> {
    if let Message::Tweets { tweets } = &message {
        return keep_tweets(data, tweets);
    }
    let mut state = load(data, browser);
    state.apply(message, now_ms());
    std::fs::create_dir_all(dir(data))?;
    let path = state_path(data, browser);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&state).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

fn tweets_dir(data: &Path) -> PathBuf {
    dir(data).join("tweets")
}

/// A tweet the extension saw, if it did.
pub fn cached_tweet(data: &Path, id: &str) -> Option<CachedTweet> {
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    std::fs::read(tweets_dir(data).join(format!("{id}.json"))).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// How long seen tweets are kept: a capture usually follows within minutes.
const TWEET_DAYS: u64 = 14;

fn keep_tweets(data: &Path, tweets: &[CachedTweet]) -> std::io::Result<()> {
    let dir = tweets_dir(data);
    std::fs::create_dir_all(&dir)?;
    for t in tweets.iter().filter(|t| !t.id.is_empty() && t.id.chars().all(|c| c.is_ascii_digit())) {
        let path = dir.join(format!("{}.json", t.id));
        let tmp = dir.join(format!(".{}.{}.tmp", t.id, std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(t).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)?;
    }
    prune(&dir);
    Ok(())
}

/// Drops seen tweets older than [`TWEET_DAYS`], at most once an hour.
fn prune(dir: &Path) {
    let marker = dir.join(".pruned");
    let hour = std::time::Duration::from_secs(3600);
    let fresh =
        std::fs::metadata(&marker).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|e| e < hour));
    if fresh {
        return;
    }
    let _ = std::fs::write(&marker, b"");
    let limit = std::time::Duration::from_secs(TWEET_DAYS * 24 * 3600);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|e| e > limit));
        if old && entry.file_name().to_string_lossy().ends_with(".json") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Reads one native-messaging frame: a native-endian u32 length, then that much JSON. `None`
/// at a clean end of input.
pub fn read_frame(input: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len) as usize;
    // Chrome caps messages to a host at 4 GiB; anything this large is not ours to hold.
    if len > 64 * 1024 * 1024 {
        return Err(std::io::Error::other(format!("a {len}-byte message is too large")));
    }
    let mut body = vec![0; len];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

pub fn write_frame(output: &mut impl Write, json: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(json.len()).map_err(std::io::Error::other)?;
    output.write_all(&len.to_ne_bytes())?;
    output.write_all(json)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_update_windows_and_focus() {
        let mut s = BrowserState::default();
        s.apply(Message::Tab { window: 1, url: "https://x.com/home".into(), title: "(1) Home / X".into() }, 10);
        s.apply(
            Message::Tab { window: 2, url: "https://twitch.tv/ludwig".into(), title: "Ludwig - Twitch".into() },
            20,
        );
        s.apply(Message::Focus { window: 2 }, 30);
        assert_eq!(s.tab_for("Ludwig - Twitch - Helium").map(|t| t.url.as_str()), Some("https://twitch.tv/ludwig"));
        assert_eq!(s.tab_for("(1) Home / X - Helium - augie").map(|t| t.url.as_str()), Some("https://x.com/home"));
        s.apply(Message::Selection { window: 2, url: "https://twitch.tv/ludwig".into(), text: "gg".into() }, 35);
        assert_eq!(s.selection_for("Ludwig - Twitch - Helium").map(|t| t.text.as_str()), Some("gg"));
        assert!(s.selection_for("(1) Home / X - Helium").is_none());
        s.apply(Message::Selection { window: 2, url: "https://twitch.tv/ludwig".into(), text: " ".into() }, 36);
        assert!(s.selection_for("Ludwig - Twitch - Helium").is_none(), "an empty selection clears it");
        s.apply(Message::Closed { window: 2 }, 40);
        assert_eq!(s.focused, None);
        assert!(s.tab_for("Ludwig - Twitch - Helium").is_none());
    }

    #[test]
    fn messages_parse_from_the_extensions_json() {
        let m: Message = serde_json::from_str(r#"{"type":"tab","window":7,"url":"https://a.com/","title":"A"}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m, Message::Tab { window: 7, url: "https://a.com/".into(), title: "A".into() });
    }

    #[test]
    fn tweets_are_kept_as_files_and_read_back() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let tweet = CachedTweet {
            id: "2123".into(),
            handle: "southpolesteve".into(),
            author: Some("Steve".into()),
            text: "the whole long post".into(),
            created_at: None,
            quoted_id: None,
            media: vec![CachedMedia {
                kind: "photo".into(),
                url: "https://pbs.twimg.com/media/A.jpg?name=orig".into(),
            }],
        };
        record(dir.path(), "helium", Message::Tweets { tweets: vec![tweet.clone()] }).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cached_tweet(dir.path(), "2123"), Some(tweet));
        assert_eq!(cached_tweet(dir.path(), "../2123"), None, "ids are digits only");
        assert!(load_all(dir.path()).is_empty(), "tweets don't touch the browser's window state");
    }

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let mut buf = Vec::new();
        write_frame(&mut buf, br#"{"ok":true}"#).unwrap_or_else(|e| panic!("{e}"));
        let mut input = std::io::Cursor::new(buf);
        assert_eq!(read_frame(&mut input).ok().flatten(), Some(br#"{"ok":true}"#.to_vec()));
        assert_eq!(read_frame(&mut input).ok(), Some(None));
    }

    #[test]
    fn record_writes_state_a_reader_sees() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        record(dir.path(), "helium", Message::Tab { window: 1, url: "https://a.com/".into(), title: "Apple".into() })
            .unwrap_or_else(|e| panic!("{e}"));
        let all = load_all(dir.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "helium");
        assert_eq!(all[0].1.tab_for("Apple - Helium").map(|t| t.url.as_str()), Some("https://a.com/"));
    }
}
