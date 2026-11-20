//! A capture: what was grabbed, what the user was looking at, their voice note, and what it's for.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::intent::Intent;
use crate::page::Page;

/// One saved capture. Stored as `<id>.json`; this is the schema agents read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub id: CaptureId,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<CapturedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<Page>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceNote>,
    /// A typed note, separate from the captured text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub intent: Intent,
}

/// Open captures are the to-do list; done ones are checked off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Open,
    Done { at: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedText {
    pub text: String,
    pub from: TextSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSource {
    /// The text selected in another app when the capture started.
    Selection,
    Clipboard,
    /// Typed into Tin.
    Typed,
    /// Piped to `tin capture`.
    Stdin,
}

/// The focused window when the capture started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A spoken note, transcribed on device by Hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceNote {
    pub transcript: String,
    pub engine: String,
}

/// A capture before it is saved; the capture panel edits one of these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub text: Option<CapturedText>,
    pub page: Option<Page>,
    pub window: Option<Window>,
    pub voice: Option<VoiceNote>,
    pub note: Option<String>,
    pub intent: Intent,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "there is nothing to save: no selected text, no page, no voice note, no note and no purpose. \
     Select some text or open a page first, or pass --text, --url, --note or --voice."
)]
pub struct EmptyCapture;

impl Draft {
    /// True when saving it would keep nothing but its context.
    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.page.is_none()
            && self.voice.is_none()
            && self.note.is_none()
            && self.intent.said.is_none()
    }
}

impl Capture {
    pub fn new(draft: Draft, now: SystemTime) -> Result<Capture, EmptyCapture> {
        if draft.is_empty() {
            return Err(EmptyCapture);
        }
        let blank = |s: Option<String>| s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        Ok(Capture {
            id: CaptureId::at(now),
            created_at: humantime::format_rfc3339_millis(now).to_string(),
            status: Status::Open,
            text: draft.text.filter(|t| !t.text.trim().is_empty()),
            page: draft.page,
            window: draft.window.filter(|w| w.app.is_some() || w.title.is_some()),
            voice: draft.voice.filter(|v| !v.transcript.trim().is_empty()),
            note: blank(draft.note),
            intent: draft.intent,
        })
    }

    pub fn is_done(&self) -> bool {
        matches!(self.status, Status::Done { .. })
    }

    /// The one line a list shows: the text, else the note, voice note, page title or URL.
    pub fn headline(&self) -> String {
        let line = self
            .text
            .as_ref()
            .map(|t| t.text.as_str())
            .or(self.note.as_deref())
            .or(self.voice.as_ref().map(|v| v.transcript.as_str()))
            .or(self.page.as_ref().and_then(|p| p.title.as_deref()))
            .or(self.page.as_ref().map(|p| p.url.as_str()))
            .or(self.intent.said.as_deref())
            .unwrap_or("");
        line.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// What Copy puts on the clipboard: the text, with the page's URL when there is one.
    pub fn clipboard_text(&self) -> String {
        let body = self
            .text
            .as_ref()
            .map(|t| t.text.clone())
            .or(self.note.clone())
            .or(self.voice.as_ref().map(|v| v.transcript.clone()));
        match (body, &self.page) {
            (Some(body), Some(page)) if !body.contains(&page.url) => format!("{body}\n\n{}", page.url),
            (Some(body), _) => body,
            (None, Some(page)) => page.url.clone(),
            (None, None) => self.headline(),
        }
    }
}

/// `20261120-123712-482-3fa9`: UTC time to the millisecond, then four random hex digits.
/// Ids sort by time as strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CaptureId(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("`{0}` isn't a capture id (expected something like 20261120-123712-482-3fa9)")]
pub struct BadId(pub String);

impl CaptureId {
    pub fn at(now: SystemTime) -> CaptureId {
        let stamp: String =
            humantime::format_rfc3339_millis(now).to_string().chars().filter(char::is_ascii_digit).collect();
        // yyyymmddHHMMSSmmm
        let (date, rest) = stamp.split_at(8.min(stamp.len()));
        let (time, millis) = rest.split_at(6.min(rest.len()));
        CaptureId(format!("{date}-{time}-{millis}-{:04x}", random_u16()))
    }

    pub fn parse(s: &str) -> Result<CaptureId, BadId> {
        let parts: Vec<&str> = s.split('-').collect();
        let shape = [8, 6, 3, 4];
        let ok = parts.len() == 4
            && parts.iter().zip(shape).all(|(p, n)| p.len() == n)
            && parts[..3].iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
            && parts[3].chars().all(|c| c.is_ascii_hexdigit());
        if ok { Ok(CaptureId(s.to_owned())) } else { Err(BadId(s.to_owned())) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CaptureId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for CaptureId {
    type Error = BadId;
    fn try_from(s: String) -> Result<Self, BadId> {
        CaptureId::parse(&s)
    }
}

impl From<CaptureId> for String {
    fn from(id: CaptureId) -> String {
        id.0
    }
}

/// Enough randomness to keep two captures in the same millisecond apart.
fn random_u16() -> u16 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.write_u32(std::process::id());
    (h.finish() & 0xffff) as u16
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::page::PageSource;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(secs)
    }

    #[test]
    fn ids_read_as_utc_time_and_round_trip() {
        let id = CaptureId::at(at(1_795_000_000_123));
        // `date -u -d @1795000000` is Wed Nov 18 11:06:40 UTC 2026.
        assert_eq!(&id.as_str()[..19], "20261118-110640-123", "{id}");
        assert_eq!(CaptureId::parse(id.as_str()), Ok(id.clone()));
        assert!(CaptureId::parse("../etc/passwd").is_err());
        assert!(CaptureId::parse("20261204-085320-123-zzzz").is_err());
    }

    #[test]
    fn an_empty_draft_is_refused_and_blank_parts_are_dropped() {
        assert_eq!(Capture::new(Draft::default(), at(0)), Err(EmptyCapture));
        let draft = Draft {
            note: Some("  hi  ".into()),
            voice: Some(VoiceNote { transcript: " ".into(), engine: "hex".into() }),
            ..Draft::default()
        };
        let capture = Capture::new(draft, at(0)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(capture.note.as_deref(), Some("hi"));
        assert_eq!(capture.voice, None);
        assert_eq!(capture.status, Status::Open);
    }

    #[test]
    fn copy_adds_the_page_url_once() {
        let page = Page::new("https://blog.com/", Some("BLOG".into()), PageSource::Manual);
        let text = |t: &str| Some(CapturedText { text: t.into(), from: TextSource::Selection });
        let make = |text, page| {
            Capture::new(Draft { text, page, ..Draft::default() }, at(0)).unwrap_or_else(|e| panic!("{e}"))
        };
        assert_eq!(make(text("Pricing"), Some(page.clone())).clipboard_text(), "Pricing\n\nhttps://blog.com/");
        assert_eq!(make(text("see https://blog.com/"), Some(page.clone())).clipboard_text(), "see https://blog.com/");
        assert_eq!(make(None, Some(page)).clipboard_text(), "https://blog.com/");
    }

    #[test]
    fn the_json_shape_is_stable() {
        let mut capture =
            Capture::new(Draft { note: Some("n".into()), ..Draft::default() }, at(0)).unwrap_or_else(|e| panic!("{e}"));
        capture.id = CaptureId("19700101-000000-000-abcd".into());
        let json = serde_json::to_value(&capture).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            json,
            serde_json::json!({
                "id": "19700101-000000-000-abcd",
                "created_at": "1970-01-01T00:00:00.000Z",
                "status": {"state": "open"},
                "note": "n",
                "intent": {"target": {"type": "undecided"}, "tags": []}
            })
        );
    }
}
