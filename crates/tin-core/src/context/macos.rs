//! macOS: the front app and window through System Events, browser tabs through each browser's
//! AppleScript dictionary, and the selection by sending ⌘C (then restoring the clipboard).
//! System Events needs Accessibility permission for Tin (or the terminal running it).

use super::shell::{self, Ran};
use crate::browser::Tab;
use crate::capture::Window;

fn osascript(lang: &str, script: &str) -> Result<String, String> {
    match shell::run("osascript", &["-l", lang, "-e", script])? {
        Ran::Ok(out) => Ok(out.trim_end_matches('\n').to_owned()),
        Ran::Failed(err) => Err(format!(
            "macOS refused Tin's script ({err}). Give Tin Accessibility (and Automation) permission \
             in System Settings › Privacy & Security."
        )),
    }
}

/// macOS window pictures aren't wired up yet.
pub fn focused(_want_screenshot: impl FnOnce(&Window) -> bool) -> Result<super::Focus, String> {
    focused_window().map(|window| super::Focus { window, screenshot: None })
}

/// Not built yet on macOS.
pub fn window_pictures(_snap: &crate::snapshot::Snapshot) -> Result<Vec<Option<crate::screenshot::Image>>, String> {
    Err("Working out which window is on top isn't built for macOS yet.".into())
}

/// Not built yet on macOS, where `screencapture -i` is the native picker.
pub fn snapshots() -> Result<Vec<crate::snapshot::Snapshot>, String> {
    Err("Picking a window or region isn't built for macOS yet.".into())
}

fn focused_window() -> Result<Window, String> {
    let app = osascript(
        "AppleScript",
        r#"tell application "System Events" to get name of first application process whose frontmost is true"#,
    )?;
    // The window title needs Accessibility; the app name alone is still useful without it.
    let title = osascript(
        "AppleScript",
        r#"tell application "System Events" to tell (first application process whose frontmost is true) to get name of front window"#,
    )
    .ok();
    let s = |s: String| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    Ok(Window { app: s(app), title: title.and_then(s) })
}

const SAFARI_LIKE: [&str; 3] = ["Safari", "Safari Technology Preview", "Orion"];
const CHROMIUM_LIKE: [&str; 8] = [
    "Google Chrome",
    "Google Chrome Canary",
    "Chromium",
    "Brave Browser",
    "Microsoft Edge",
    "Vivaldi",
    "Arc",
    "Helium",
];

/// The front tab through the browser's scripting dictionary, when it has one.
pub fn browser_tab(app: &str) -> Result<Option<(String, Tab)>, String> {
    let (url, title) = if SAFARI_LIKE.contains(&app) {
        ("URL of current tab of front window", "name of current tab of front window")
    } else if CHROMIUM_LIKE.contains(&app) {
        ("URL of active tab of front window", "title of active tab of front window")
    } else {
        return Ok(None);
    };
    let script = format!(r#"tell application "{app}" to return ({url}) & linefeed & ({title})"#);
    let out = osascript("AppleScript", &script)?;
    let (url, title) = out.split_once('\n').unwrap_or((out.as_str(), ""));
    Ok(Some((
        app.to_owned(),
        Tab { url: url.trim().to_owned(), title: Some(title.trim().to_owned()).filter(|t| !t.is_empty()) },
    )))
}

/// Copies the selection with ⌘C, reads it, and puts the previous clipboard text back.
pub fn selection() -> Result<Option<String>, String> {
    const SCRIPT: &str = r#"
ObjC.import('AppKit');
const pb = $.NSPasteboard.generalPasteboard;
const before = pb.changeCount;
const saved = pb.stringForType($.NSPasteboardTypeString);
Application('System Events').keystroke('c', { using: 'command down' });
let text = null;
for (let i = 0; i < 25 && text === null; i++) {
  delay(0.02);
  if (pb.changeCount !== before) {
    const s = pb.stringForType($.NSPasteboardTypeString);
    text = s.isNil() ? '' : s.js;
  }
}
if (text !== null) {
  pb.clearContents;
  if (!saved.isNil()) pb.setStringForType(saved, $.NSPasteboardTypeString);
}
text === null ? '' : text;
"#;
    let text = osascript("JavaScript", SCRIPT)?;
    Ok(Some(text).filter(|t| !t.is_empty()))
}

pub fn clipboard() -> Result<Option<String>, String> {
    match shell::run("pbpaste", &[])? {
        Ran::Ok(text) => Ok(Some(text).filter(|t| !t.is_empty())),
        Ran::Failed(err) => Err(format!("`pbpaste` failed: {err}")),
    }
}

pub fn copy(text: &str) -> Result<(), String> {
    match shell::run_with_input("pbcopy", &[], Some(text))? {
        Ran::Ok(_) => Ok(()),
        Ran::Failed(err) => Err(format!("`pbcopy` couldn't copy: {err}")),
    }
}
