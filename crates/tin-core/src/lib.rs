//! Tin: capture what you're looking at, say what it's for, and hand it to an agent.
//!
//! The pieces: [`context`] reads the selection, focused window and page; [`hex`] records a voice
//! note; [`intent`] routes the purpose to a project; [`store`] keeps captures as JSON files; and
//! [`Tin`] ties them together for the CLI and the app.

pub mod bridge;
pub mod browser;
pub mod capture;
pub mod config;
pub mod context;
pub mod hex;
pub mod hook;
pub mod intent;
pub mod media;
pub mod page;
pub mod regions;
pub mod screenshot;
pub mod snapshot;
pub mod store;
pub mod tweet;

use std::path::PathBuf;
use std::time::SystemTime;

use capture::{Capture, CaptureId, Draft, EmptyCapture, ScreenshotFile, Status};
use config::{Config, ConfigError};
use hex::Hex;
use hook::HookError;
use intent::Routes;
use store::{Store, StoreError};

/// Where Tin keeps things. `TIN_DATA_DIR` and `TIN_CONFIG` override the defaults.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    /// `~/.local/share/tin` and `~/.config/tin/config.toml` on Linux;
    /// `~/Library/Application Support/tin` for both on macOS.
    pub fn from_env() -> Paths {
        let data = std::env::var_os("TIN_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|d| d.join("tin")))
            .unwrap_or_else(|| PathBuf::from(".tin"));
        let config = std::env::var_os("TIN_CONFIG")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|d| d.join("tin").join("config.toml")))
            .unwrap_or_else(|| data.join("config.toml"));
        Paths { data, config }
    }

    pub fn captures(&self) -> PathBuf {
        self.data.join("captures")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Empty(#[from] EmptyCapture),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveTweetError {
    #[error("this capture's page isn't a tweet, so there's nothing to keep")]
    NotATweet,
    #[error(transparent)]
    Archive(#[from] tweet::ArchiveError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A saved capture, where it went, and any hook that didn't start.
#[derive(Debug)]
pub struct Saved {
    pub capture: Capture,
    pub file: PathBuf,
    pub hook_errors: Vec<HookError>,
}

/// Tin's state on disk, opened.
#[derive(Debug, Clone)]
pub struct Tin {
    pub paths: Paths,
    pub config: Config,
    pub routes: Routes,
    pub store: Store,
}

impl Tin {
    pub fn open(paths: Paths) -> Result<Tin, OpenError> {
        let config = Config::load(&paths.config)?;
        let routes = Routes::from_config(&config);
        let store = Store::open(paths.captures())?;
        Ok(Tin { paths, config, routes, store })
    }

    pub fn hex(&self) -> Hex {
        Hex::new(self.config.hex_bin())
    }

    /// Resolves the draft's purpose (with its voice note) and saves it, then starts hooks.
    pub fn save(&self, mut draft: Draft) -> Result<Saved, SaveError> {
        let said = draft.intent.said.take();
        draft.intent = self.routes.resolve(said.as_deref(), draft.voice.as_ref().map(|v| v.transcript.as_str()));
        let shot = draft.screenshot.take();
        let mut capture = Capture::new(draft, SystemTime::now())?;
        if let Some(shot) = shot {
            let path = self.store.save_screenshot(&capture.id, &shot.png)?;
            capture.screenshot = Some(ScreenshotFile { path, width: shot.width, height: shot.height });
        }
        let file = self.store.save(&capture)?;
        let hook_errors = self.run_hooks(&capture, &file);
        Ok(Saved { capture, file, hook_errors })
    }

    /// Routes an existing capture again from a new purpose.
    pub fn reroute(&self, id: &CaptureId, said: &str) -> Result<Saved, StoreError> {
        let mut capture = self.store.load(id)?;
        capture.intent = self.routes.resolve(Some(said), capture.voice.as_ref().map(|v| v.transcript.as_str()));
        let file = self.store.save(&capture)?;
        let hook_errors = self.run_hooks(&capture, &file);
        Ok(Saved { capture, file, hook_errors })
    }

    /// Keeps a copy of the tweet a capture is of (text, photos, videos) beside it, and records it
    /// in the capture. Only for captures whose page is a tweet.
    pub fn archive_tweet(&self, id: &CaptureId) -> Result<Capture, ArchiveTweetError> {
        let mut capture = self.store.load(id)?;
        let status = match capture.page.as_ref().map(|p| &p.kind) {
            Some(page::PageKind::Tweet { status_id, .. }) => status_id.clone(),
            _ => return Err(ArchiveTweetError::NotATweet),
        };
        // What Tin's extension saw of it (and its quoted tweet), when it's installed.
        let tweet = bridge::cached_tweet(&self.paths.data, &status);
        let quoted =
            tweet.as_ref().and_then(|t| t.quoted_id.as_deref()).and_then(|q| bridge::cached_tweet(&self.paths.data, q));
        let archive = tweet::archive(&status, self.store.dir(), id.as_str(), &tweet::Seen { tweet, quoted })?;
        capture.tweet = Some(archive);
        self.store.save(&capture)?;
        Ok(capture)
    }

    /// Downloads the images and videos found where the capture was picked into `<id>.media/`,
    /// and records which were kept.
    pub fn keep_media(&self, id: &CaptureId) -> Result<Capture, StoreError> {
        let mut capture = self.store.load(id)?;
        let referrer = capture.page.as_ref().map(|p| p.url.clone());
        if let Some(content) = capture.selection.as_mut().and_then(|s| s.content.as_mut()) {
            let dir = self.store.dir().join(format!("{}.media", id.as_str()));
            media::keep(&mut content.media, &dir, referrer.as_deref());
            self.store.save(&capture)?;
        }
        Ok(capture)
    }

    /// Checks a capture off, or opens it again.
    pub fn set_done(&self, id: &CaptureId, done: bool) -> Result<Capture, StoreError> {
        let mut capture = self.store.load(id)?;
        capture.status = if done {
            Status::Done { at: humantime::format_rfc3339_millis(SystemTime::now()).to_string() }
        } else {
            Status::Open
        };
        self.store.save(&capture)?;
        Ok(capture)
    }

    fn run_hooks(&self, capture: &Capture, file: &std::path::Path) -> Vec<HookError> {
        let global = self.config.hooks.on_capture.iter();
        let routed = self
            .routes
            .iter()
            .filter(|r| capture.intent.tags.iter().any(|t| *t == tag_of(&r.name)))
            .filter_map(|r| r.hook.as_ref());
        global.chain(routed).filter_map(|cmd| hook::spawn(cmd, capture, file).err()).collect()
    }
}

/// The tag a route name becomes: `Game Dev` is `game-dev`.
pub fn tag_of(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}
