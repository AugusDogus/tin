//! What the user was looking at: a web page, recognised as a tweet when it is one.

use serde::{Deserialize, Serialize};

/// A page the capture points at, with where Tin learned about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: PageKind,
    pub via: PageSource,
}

/// What kind of page this is. Agents can branch on `kind` without parsing URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PageKind {
    Link,
    Tweet {
        /// The handle from the URL, without `@`.
        handle: String,
        status_id: String,
        /// The display name and text, read from the tab title when the browser exposes it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// Where the page came from, so a reader knows how much to trust it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PageSource {
    /// The active tab of a browser, read from its session file or scripting interface.
    BrowserTab {
        browser: String,
        /// False when Tin couldn't see which window was focused (some Wayland desktops hide
        /// it) and took the most recently used browser's tab instead.
        focused: bool,
    },
    /// A URL found in the captured text itself.
    CapturedText,
    /// Given explicitly (`--url`).
    Manual,
}

impl Page {
    /// Builds a page and classifies it from its URL and title.
    pub fn new(url: impl Into<String>, title: Option<String>, via: PageSource) -> Self {
        let url = url.into();
        let title = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
        let kind = PageKind::classify(&url, title.as_deref());
        Page { url, title, kind, via }
    }

    /// The first `http(s)://` URL in some text, if any.
    pub fn find_url(text: &str) -> Option<&str> {
        let start = text.find("https://").or_else(|| text.find("http://"))?;
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']'))
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        (url.len() > "https://".len()).then_some(url)
    }

    /// `example.com` for `https://www.example.com/a?b`, for compact display.
    pub fn host(&self) -> &str {
        let rest = self.url.split_once("://").map_or(self.url.as_str(), |(_, r)| r);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        host.strip_prefix("www.").unwrap_or(host)
    }
}

impl PageKind {
    pub fn classify(url: &str, title: Option<&str>) -> Self {
        match tweet_parts(url) {
            Some((handle, status_id)) => {
                let (author, text) = title.map(tweet_title).unwrap_or((None, None));
                PageKind::Tweet { handle, status_id, author, text }
            }
            None => PageKind::Link,
        }
    }
}

const TWEET_HOSTS: [&str; 6] =
    ["x.com", "www.x.com", "twitter.com", "www.twitter.com", "mobile.twitter.com", "mobile.x.com"];

/// `(handle, status id)` for `https://x.com/<handle>/status/<id>[/…]`.
fn tweet_parts(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.split(['?', '#']).next()?;
    let mut parts = rest.split('/');
    let host = parts.next()?;
    if !TWEET_HOSTS.contains(&host.to_ascii_lowercase().as_str()) {
        return None;
    }
    let handle = parts.next()?;
    let (status, id) = (parts.next()?, parts.next()?);
    let valid_handle =
        !handle.is_empty() && handle != "i" && handle.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let valid_id = !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());
    (valid_handle && status == "status" && valid_id).then(|| (handle.to_owned(), id.to_owned()))
}

/// X titles a tweet tab `(3) Name on X: "text" / X`; take the name and text from it.
fn tweet_title(title: &str) -> (Option<String>, Option<String>) {
    let title = strip_unread_count(title.trim());
    let Some((author, rest)) = title.split_once(" on X: ").or_else(|| title.split_once(" on Twitter: ")) else {
        return (None, None);
    };
    let text = rest
        .trim_end_matches(" / X")
        .trim_end_matches(" / Twitter")
        .trim()
        .trim_start_matches(['"', '“'])
        .trim_end_matches(['"', '”']);
    let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
    (some(author), some(text))
}

/// Drops a leading `(12) ` notification count.
fn strip_unread_count(title: &str) -> &str {
    title
        .strip_prefix('(')
        .and_then(|r| r.split_once(") "))
        .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '+'))
        .map_or(title, |(_, rest)| rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_url_is_a_tweet_with_author_and_text_from_the_title() {
        let page = Page::new(
            "https://x.com/shadcn/status/2082519375194763675?s=20",
            Some(r#"(1) shadcn on X: "Launching Copper, a Mac app" / X"#.into()),
            PageSource::Manual,
        );
        assert_eq!(
            page.kind,
            PageKind::Tweet {
                handle: "shadcn".into(),
                status_id: "2082519375194763675".into(),
                author: Some("shadcn".into()),
                text: Some("Launching Copper, a Mac app".into()),
            }
        );
    }

    #[test]
    fn twitter_dot_com_counts_and_an_odd_title_still_gives_a_tweet() {
        let kind = PageKind::classify("http://twitter.com/a_b/status/42/photo/1", Some("Home / X"));
        assert_eq!(kind, PageKind::Tweet { handle: "a_b".into(), status_id: "42".into(), author: None, text: None });
    }

    #[test]
    fn other_x_pages_and_other_sites_are_links() {
        for url in [
            "https://x.com/home",
            "https://x.com/i/status/1",
            "https://x.com/a/status/nope",
            "https://example.com/a/status/1",
        ] {
            assert_eq!(PageKind::classify(url, None), PageKind::Link, "{url}");
        }
    }

    #[test]
    fn find_url_takes_the_first_url_without_trailing_punctuation() {
        assert_eq!(Page::find_url("see (https://blog.com/docs). ok"), Some("https://blog.com/docs"));
        assert_eq!(Page::find_url("no links here, just https://"), None);
        assert_eq!(Page::find_url("plain text"), None);
    }

    #[test]
    fn host_drops_scheme_www_and_path() {
        let page = Page::new("https://www.example.com/a?b#c", None, PageSource::Manual);
        assert_eq!(page.host(), "example.com");
    }
}
