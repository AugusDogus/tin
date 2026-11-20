//! What's on screen when a capture starts: the selected text, the focused window, and the page.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Tin reads screen context on Linux and macOS only.");

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod shell;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;

use crate::browser;
use crate::capture::{CapturedText, TextSource, Window};
use crate::page::{Page, PageSource};

/// Which text to grab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextGrab {
    /// The current selection (Linux's primary selection; on macOS, copied with ⌘C and the
    /// clipboard restored after).
    Selection,
    Clipboard,
    /// The selection, else the clipboard (where the desktop doesn't share the selection, copying
    /// first still works). The capture says which it was.
    SelectionOrClipboard,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub text: TextGrab,
    pub page: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { text: TextGrab::Selection, page: true }
    }
}

/// What Tin found, plus a note for each part it couldn't get and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gathered {
    pub text: Option<CapturedText>,
    pub window: Option<Window>,
    pub page: Option<Page>,
    pub notes: Vec<Note>,
}

/// Why part of the context is missing, said for a person.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Note {
    pub part: Part,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Text,
    Window,
    Page,
}

/// Reads the screen's context. Never fails: each missing part leaves a note instead.
pub fn gather(options: Options) -> Gathered {
    let mut notes = Vec::new();
    // The window first: grabbing the selection on macOS sends ⌘C to it.
    let window = platform::focused_window().map_err(|message| notes.push(Note { part: Part::Window, message })).ok();

    let read = |grab: TextGrab| match grab {
        TextGrab::Selection => (platform::selection(), TextSource::Selection),
        TextGrab::Clipboard => (platform::clipboard(), TextSource::Clipboard),
        TextGrab::SelectionOrClipboard | TextGrab::None => (Ok(None), TextSource::Selection),
    };
    let mut take = |(result, from): (Result<Option<String>, String>, TextSource)| {
        result
            .map_err(|message| notes.push(Note { part: Part::Text, message }))
            .ok()
            .flatten()
            .filter(|t| !t.trim().is_empty())
            .map(|text| CapturedText { text, from })
    };
    let text = match options.text {
        TextGrab::SelectionOrClipboard => take(read(TextGrab::Selection)).or_else(|| take(read(TextGrab::Clipboard))),
        grab => take(read(grab)),
    };
    if text.is_none() && options.text != TextGrab::None && !notes.iter().any(|n| n.part == Part::Text) {
        let message = match options.text {
            TextGrab::Clipboard => "The clipboard has no text.",
            TextGrab::SelectionOrClipboard => "Nothing is selected and the clipboard has no text.",
            _ => {
                "Nothing is selected, or this desktop doesn't share the selection with other apps (COSMIC doesn't). Copy it and capture the clipboard instead."
            }
        };
        notes.push(Note { part: Part::Text, message: message.into() });
    }

    let page = if options.page { page(window.as_ref(), text.as_ref(), &mut notes) } else { None };
    Gathered { text, window, page, notes }
}

fn page(window: Option<&Window>, text: Option<&CapturedText>, notes: &mut Vec<Note>) -> Option<Page> {
    let mut note = |message: String| notes.push(Note { part: Part::Page, message });
    let from_text =
        || text.and_then(|t| Page::find_url(&t.text)).map(|url| Page::new(url, None, PageSource::CapturedText));
    let app = window.and_then(|w| w.app.as_deref());
    match app {
        Some(app) if browser::is_browser_window(app) => {
            let scripted = platform::browser_tab(app).map_err(&mut note).ok().flatten();
            let tab = scripted.or_else(|| {
                browser::active_tab(Some(app))
                    .map_err(|e| note(e.to_string()))
                    .ok()
                    .map(|t| (t.browser.to_owned(), t.tab))
            });
            tab.map(|(browser, tab)| Page::new(tab.url, tab.title, PageSource::BrowserTab { browser, focused: true }))
                .or_else(from_text)
        }
        // A non-browser window is in front: only a URL in the text itself is the page.
        Some(_) => from_text(),
        None => match browser::active_tab(None) {
            // `focused: false` tells the reader this is a guess.
            Ok(t) => Some(Page::new(
                t.tab.url,
                t.tab.title,
                PageSource::BrowserTab { browser: t.browser.to_owned(), focused: false },
            )),
            Err(e) => {
                note(e.to_string());
                from_text()
            }
        },
    }
}

/// Puts text on the system clipboard.
pub fn copy(text: &str) -> Result<(), String> {
    platform::copy(text)
}
