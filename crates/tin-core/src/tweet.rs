//! Keeping a tweet in case it's deleted: its text, author, time, quoted tweet, photos and
//! videos, saved beside the capture.
//!
//! The data comes from X's public syndication endpoint (what its embedded-tweet widget reads),
//! which needs no account. Its raw JSON is kept as is, so nothing it said is lost, and the photos
//! (at original resolution) and videos (the best MP4) are downloaded.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::page::PageKind;

/// A kept tweet, as stored in the capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TweetArchive {
    pub url: String,
    pub id: String,
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted: Option<Quoted>,
    #[serde(default)]
    pub media: Vec<SavedMedia>,
    /// Media that couldn't be downloaded, with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// The endpoint's whole answer.
    pub raw: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quoted {
    pub url: String,
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedMedia {
    /// `photo`, `video` or `animated_gif`.
    pub kind: String,
    pub file: PathBuf,
    pub source: String,
}

/// `(handle, status id)` of the first tweet permalink among `urls`, tidied of extra path
/// (`/photo/1`, `/analytics`) and query. Links inside a tweet list its own timestamp link before
/// any quoted tweet's.
pub fn permalink(urls: impl IntoIterator<Item = impl AsRef<str>>) -> Option<(String, String)> {
    urls.into_iter().find_map(|u| match PageKind::classify(u.as_ref(), None) {
        PageKind::Tweet { handle, status_id, .. } => Some((handle, status_id)),
        PageKind::Link => None,
    })
}

pub fn url(handle: &str, id: &str) -> String {
    format!("https://x.com/{handle}/status/{id}")
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("couldn't fetch the tweet from X ({0}). The capture is saved without it; try `tin archive` later.")]
    Fetch(String),
    #[error("X didn't return a tweet for {0} (deleted, private, or age-restricted?). The capture is saved without it.")]
    NotATweet(String),
    #[error("couldn't write the tweet's files to {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
}

#[derive(Deserialize)]
struct Syndicated {
    #[serde(rename = "__typename", default)]
    typename: Option<String>,
    #[serde(default)]
    id_str: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    user: Option<User>,
    #[serde(default, rename = "mediaDetails")]
    media: Vec<MediaDetail>,
    #[serde(default)]
    quoted_tweet: Option<Box<Syndicated>>,
}

#[derive(Deserialize)]
struct User {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    screen_name: Option<String>,
}

#[derive(Deserialize)]
struct MediaDetail {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    media_url_https: Option<String>,
    #[serde(default)]
    video_info: Option<VideoInfo>,
}

#[derive(Deserialize)]
struct VideoInfo {
    #[serde(default)]
    variants: Vec<Variant>,
}

#[derive(Deserialize)]
struct Variant {
    #[serde(default)]
    bitrate: Option<u64>,
    #[serde(default)]
    content_type: String,
    url: String,
}

/// What to download for one media item: the photo at original size, or the highest-bitrate MP4.
fn source(m: &MediaDetail) -> Option<String> {
    match m.kind.as_str() {
        "photo" => m.media_url_https.as_ref().map(|u| format!("{u}?name=orig")),
        _ => m
            .video_info
            .as_ref()?
            .variants
            .iter()
            .filter(|v| v.content_type == "video/mp4")
            .max_by_key(|v| v.bitrate.unwrap_or(0))
            .map(|v| v.url.clone()),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).build().into()
}

/// Fetches a tweet and saves it under `dir` as `<stem>.tweet.json` and `<stem>.media/`.
pub fn archive(id: &str, dir: &Path, stem: &str) -> Result<TweetArchive, ArchiveError> {
    let agent = agent();
    // The endpoint wants a `token`; any value is accepted.
    let endpoint = format!("https://cdn.syndication.twimg.com/tweet-result?id={id}&lang=en&token=tin");
    let body = agent
        .get(&endpoint)
        .header("User-Agent", "Mozilla/5.0 (Tin)")
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .map_err(|e| ArchiveError::Fetch(e.to_string()))?;
    let raw = dir.join(format!("{stem}.tweet.json"));
    std::fs::write(&raw, &body).map_err(|source| ArchiveError::Write { path: raw.clone(), source })?;
    let tweet: Syndicated = serde_json::from_str(&body).map_err(|_| ArchiveError::NotATweet(id.to_owned()))?;
    if tweet.typename.as_deref() != Some("Tweet") || tweet.id_str.is_none() {
        return Err(ArchiveError::NotATweet(id.to_owned()));
    }
    let mut archive = from_syndicated(&tweet, raw);

    let media_dir = dir.join(format!("{stem}.media"));
    for (i, m) in tweet.media.iter().enumerate() {
        let Some(src) = source(m) else {
            archive.missing.push(format!("{} {}: no downloadable file", m.kind, i + 1));
            continue;
        };
        let ext = if m.kind == "photo" {
            Path::new(src.split('?').next().unwrap_or(&src))
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("jpg")
                .to_owned()
        } else {
            "mp4".to_owned()
        };
        let file = media_dir.join(format!("{}-{}.{ext}", i + 1, m.kind));
        match download(&agent, &src, &file) {
            Ok(()) => archive.media.push(SavedMedia { kind: m.kind.clone(), file, source: src }),
            Err(e) => archive.missing.push(format!("{} {}: {e}", m.kind, i + 1)),
        }
    }
    Ok(archive)
}

fn download(agent: &ureq::Agent, url: &str, file: &Path) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut out = std::fs::File::create(file).map_err(|e| e.to_string())?;
    std::io::copy(&mut response.body_mut().as_reader(), &mut out).map_err(|e| e.to_string())?;
    Ok(())
}

fn from_syndicated(t: &Syndicated, raw: PathBuf) -> TweetArchive {
    let handle = t.user.as_ref().and_then(|u| u.screen_name.clone()).unwrap_or_default();
    let id = t.id_str.clone().unwrap_or_default();
    TweetArchive {
        url: url(&handle, &id),
        id,
        author: t.user.as_ref().and_then(|u| u.name.clone()),
        handle,
        text: t.text.clone(),
        created_at: t.created_at.clone(),
        quoted: t.quoted_tweet.as_ref().map(|q| {
            let handle = q.user.as_ref().and_then(|u| u.screen_name.clone()).unwrap_or_default();
            Quoted {
                url: url(&handle, q.id_str.as_deref().unwrap_or_default()),
                author: q.user.as_ref().and_then(|u| u.name.clone()),
                handle,
                text: q.text.clone(),
            }
        }),
        media: Vec::new(),
        missing: Vec::new(),
        raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_status_link_is_the_permalink_without_extra_path() {
        let links = [
            "https://x.com/maria_rcks",
            "https://x.com/maria_rcks/status/1991?s=20",
            "https://x.com/AdamHoltererer/status/1880",
        ];
        assert_eq!(permalink(links), Some(("maria_rcks".into(), "1991".into())));
        assert_eq!(permalink(["https://x.com/a/status/5/analytics"]), Some(("a".into(), "5".into())));
        assert_eq!(permalink(["https://x.com/home", "https://example.com/a/status/1"]), None);
    }

    #[test]
    fn parses_text_quote_and_picks_original_photos_and_the_best_mp4() {
        let body = r#"{"__typename":"Tweet","id_str":"7","text":"look","created_at":"2026-11-20T10:00:00.000Z",
            "user":{"name":"Maria","screen_name":"maria_rcks"},
            "mediaDetails":[
              {"type":"photo","media_url_https":"https://pbs.twimg.com/media/A.jpg"},
              {"type":"video","video_info":{"variants":[
                {"content_type":"application/x-mpegURL","url":"https://video.twimg.com/a.m3u8"},
                {"bitrate":256000,"content_type":"video/mp4","url":"https://video.twimg.com/low.mp4"},
                {"bitrate":2176000,"content_type":"video/mp4","url":"https://video.twimg.com/high.mp4"}]}}],
            "quoted_tweet":{"__typename":"Tweet","id_str":"6","text":"original","user":{"name":"Adam","screen_name":"adam"}}}"#;
        let t: Syndicated = serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}"));
        let a = from_syndicated(&t, PathBuf::from("x.tweet.json"));
        assert_eq!(a.url, "https://x.com/maria_rcks/status/7");
        assert_eq!(a.author.as_deref(), Some("Maria"));
        assert_eq!(
            a.quoted.as_ref().map(|q| (q.url.as_str(), q.text.as_str())),
            Some(("https://x.com/adam/status/6", "original"))
        );
        let sources: Vec<Option<String>> = t.media.iter().map(source).collect();
        assert_eq!(
            sources,
            [
                Some("https://pbs.twimg.com/media/A.jpg?name=orig".into()),
                Some("https://video.twimg.com/high.mp4".into())
            ]
        );
    }

    /// Fetches jack's first tweet: `cargo test -p tin-core -- --ignored tweet_live --nocapture`.
    #[test]
    #[ignore = "needs the network"]
    fn tweet_live_archive_of_a_public_tweet() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let a = archive("20", dir.path(), "t").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((a.handle.as_str(), a.text.as_str()), ("jack", "just setting up my twttr"));
        assert!(a.raw.is_file());
    }
}
