//! Images and videos on a web page where something was picked: found by Tin's browser
//! extension, then downloaded beside the capture so they outlive the page.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The largest file kept. Pages can point at whole films; a capture keeps what was looked at.
const MAX_BYTES: u64 = 500 * 1024 * 1024;
/// How long one download may take.
const TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    pub kind: MediaKind,
    /// Where the page loaded it from.
    pub url: String,
    /// The page's description of it (an image's alt text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    pub kept: Kept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
}

/// Whether a copy of it sits beside the capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Kept {
    /// Found; the download hasn't run yet.
    NotYet,
    File {
        path: PathBuf,
    },
    /// Couldn't be kept, and why. The capture's picture still shows it.
    Missing {
        reason: String,
    },
}

impl Media {
    /// An item the extension reported, if it's a kind Tin keeps (`photo` or `video`).
    pub fn found(kind: &str, url: String, alt: Option<String>) -> Option<Media> {
        let kind = match kind {
            "photo" => MediaKind::Photo,
            "video" => MediaKind::Video,
            _ => return None,
        };
        Some(Media { kind, url, alt: alt.filter(|a| !a.trim().is_empty()), kept: Kept::NotYet })
    }
}

/// Downloads each item not kept yet into `dir`, as `page-<n>.<ext>`. `referrer` is the page the
/// media was on: some sites only serve their images to their own pages.
pub fn keep(media: &mut [Media], dir: &Path, referrer: Option<&str>) {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().into();
    for (i, m) in media.iter_mut().enumerate() {
        if m.kept != Kept::NotYet {
            continue;
        }
        m.kept = if m.url.starts_with("https://") || m.url.starts_with("http://") {
            match download(&agent, &m.url, referrer, &dir.join(format!("page-{}", i + 1)), m.kind) {
                Ok(path) => Kept::File { path },
                Err(reason) => Kept::Missing { reason },
            }
        } else {
            let scheme = m.url.split(':').next().unwrap_or_default();
            Kept::Missing {
                reason: format!(
                    "the page made it in memory (a {scheme}: address), so there's no file to download; the capture's picture shows it"
                ),
            }
        };
    }
}

/// Downloads `url` to `stem` with the extension its type calls for, and returns the file.
fn download(
    agent: &ureq::Agent,
    url: &str,
    referrer: Option<&str>,
    stem: &Path,
    kind: MediaKind,
) -> Result<PathBuf, String> {
    let mut request = agent.get(url).header("User-Agent", "Mozilla/5.0 (Tin)");
    if let Some(page) = referrer {
        request = request.header("Referer", page);
    }
    let mut response = request.call().map_err(|e| format!("couldn't download it: {e}"))?;
    let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let file = stem.with_extension(extension(content_type, url, kind));
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    }
    let mut out = std::fs::File::create(&file).map_err(|e| format!("couldn't write {}: {e}", file.display()))?;
    let mut body = response.body_mut().with_config().limit(MAX_BYTES + 1).reader().take(MAX_BYTES + 1);
    let written = std::io::copy(&mut body, &mut out);
    match written {
        Ok(n) if n <= MAX_BYTES => Ok(file),
        Ok(_) => {
            let _ = std::fs::remove_file(&file);
            Err(format!("it's larger than {} MB, so Tin didn't keep it", MAX_BYTES / 1024 / 1024))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            Err(format!("the download broke off: {e}"))
        }
    }
}

/// A file extension for a download: from its content type, else its address, else its kind.
fn extension(content_type: &str, url: &str, kind: MediaKind) -> String {
    let mime = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    let from_type = match mime.as_str() {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/avif" => Some("avif"),
        "image/svg+xml" => Some("svg"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "video/quicktime" => Some("mov"),
        _ => None,
    };
    if let Some(ext) = from_type {
        return ext.to_owned();
    }
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let from_url = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| (2..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()));
    from_url.unwrap_or_else(|| match kind {
        MediaKind::Photo => "jpg".into(),
        MediaKind::Video => "mp4".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_come_from_the_type_then_the_address_then_the_kind() {
        assert_eq!(extension("image/webp; charset=binary", "https://a.com/x.jpg", MediaKind::Photo), "webp");
        assert_eq!(extension("application/octet-stream", "https://a.com/car.PNG?w=400", MediaKind::Photo), "png");
        assert_eq!(extension("", "https://a.com/image?id=1", MediaKind::Photo), "jpg");
        assert_eq!(extension("", "https://a.com/v/stream", MediaKind::Video), "mp4");
        assert_eq!(extension("", "https://a.com/archive.tar.verylong", MediaKind::Photo), "jpg");
    }

    #[test]
    fn only_photos_and_videos_are_kept_and_empty_alt_text_is_dropped() {
        let photo = Media::found("photo", "https://a.com/1.jpg".into(), Some(" ".into()));
        assert_eq!(photo.as_ref().map(|m| (m.kind, m.alt.clone())), Some((MediaKind::Photo, None)));
        assert!(Media::found("audio", "https://a.com/1.mp3".into(), None).is_none());
    }

    #[test]
    fn addresses_the_page_made_in_memory_are_missing_without_a_download() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut media = vec![Media {
            kind: MediaKind::Video,
            url: "blob:https://x.com/1234".into(),
            alt: None,
            kept: Kept::NotYet,
        }];
        keep(&mut media, dir.path(), None);
        assert!(matches!(&media[0].kept, Kept::Missing { reason } if reason.contains("blob:")));
    }
}
