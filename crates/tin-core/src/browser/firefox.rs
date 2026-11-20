//! Reads the active tab from Firefox's session store (`sessionstore-backups/recovery.jsonlz4`).
//!
//! The file is `mozLz40\0`, a little-endian u32 decompressed size, then one LZ4 block of JSON.
//! Firefox rewrites it about every 15 seconds.

use serde::Deserialize;

use super::Tab;

const MAGIC: &[u8] = b"mozLz40\0";

#[derive(Debug, thiserror::Error)]
pub enum MozError {
    #[error("not a mozlz4 file (missing the mozLz40 header)")]
    NotMozLz4,
    #[error("couldn't decompress the session store: {0}")]
    Lz4(#[from] lz4_flex::block::DecompressError),
    #[error("the session store isn't the JSON Tin expects: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the session store has no selected tab")]
    NoSelectedTab,
}

#[derive(Deserialize)]
struct Session {
    windows: Vec<Window>,
    /// 1-based.
    #[serde(rename = "selectedWindow", default)]
    selected_window: usize,
}

#[derive(Deserialize)]
struct Window {
    tabs: Vec<FfTab>,
    /// 1-based.
    #[serde(default)]
    selected: usize,
}

#[derive(Deserialize)]
struct FfTab {
    entries: Vec<Entry>,
    /// 1-based.
    #[serde(default)]
    index: usize,
}

#[derive(Deserialize)]
struct Entry {
    url: String,
    #[serde(default)]
    title: Option<String>,
}

pub fn decompress(data: &[u8]) -> Result<Vec<u8>, MozError> {
    let rest = data.strip_prefix(MAGIC).ok_or(MozError::NotMozLz4)?;
    Ok(lz4_flex::block::decompress_size_prepended(rest)?)
}

pub fn active_tab(data: &[u8]) -> Result<Tab, MozError> {
    let session: Session = serde_json::from_slice(&decompress(data)?)?;
    let nth = |n: usize| n.saturating_sub(1);
    let window = session.windows.get(nth(session.selected_window)).ok_or(MozError::NoSelectedTab)?;
    let tab = window.tabs.get(nth(window.selected)).ok_or(MozError::NoSelectedTab)?;
    let entry = tab.entries.get(nth(tab.index)).or(tab.entries.last()).ok_or(MozError::NoSelectedTab)?;
    Ok(Tab { url: entry.url.clone(), title: entry.title.clone().filter(|t| !t.is_empty()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mozlz4(json: &str) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(lz4_flex::block::compress_prepend_size(json.as_bytes()));
        out
    }

    #[test]
    fn reads_the_selected_entry_of_the_selected_tab_of_the_selected_window() {
        let json = r#"{"selectedWindow": 2, "windows": [
            {"selected": 1, "tabs": [{"index": 1, "entries": [{"url": "https://w1.com/"}]}]},
            {"selected": 2, "tabs": [
                {"index": 1, "entries": [{"url": "https://no.com/"}]},
                {"index": 1, "entries": [{"url": "https://yes.com/", "title": "Yes"}, {"url": "https://fwd.com/"}]}
            ]}
        ]}"#;
        let tab = active_tab(&mozlz4(json)).ok();
        assert_eq!(tab, Some(Tab { url: "https://yes.com/".into(), title: Some("Yes".into()) }));
    }

    #[test]
    fn refuses_files_without_the_header() {
        assert!(matches!(active_tab(b"{}"), Err(MozError::NotMozLz4)));
    }
}
