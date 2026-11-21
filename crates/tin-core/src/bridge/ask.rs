//! Questions Tin asks a browser's extension while it captures, such as which images and videos
//! sit where something was picked.
//!
//! The extension keeps one native-messaging port open to Tin's helper (the `tin` command, which
//! the browser starts). That helper listens on a socket for Tin, passes each question to the
//! extension, and passes its answer back.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{read_frame, write_frame};
use crate::media::Media;
use crate::screenshot::Rect;

/// How long the helper waits for the extension's answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(3);
/// How long Tin waits for the helper (a little longer, so the helper's own timeout answers first).
const ASK_TIMEOUT: Duration = Duration::from_secs(4);

/// A question for the extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The images and videos in part of the page a window shows.
    MediaIn { window: i64, area: Area },
}

/// Part of a page's view, as fractions of its width and height from its top-left, so the page
/// can place it whatever its zoom.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Area {
    /// Where `pick` falls within `page` (both in the same units), clipped to the page. `None`
    /// when they don't overlap.
    pub fn within(pick: Rect, page: Rect) -> Option<Area> {
        if page.width <= 0 || page.height <= 0 {
            return None;
        }
        let left = pick.x.max(page.x);
        let top = pick.y.max(page.y);
        let right = (pick.x + pick.width).min(page.x + page.width);
        let bottom = (pick.y + pick.height).min(page.y + page.height);
        if right <= left || bottom <= top {
            return None;
        }
        let (w, h) = (f64::from(page.width), f64::from(page.height));
        Some(Area {
            left: f64::from(left - page.x) / w,
            top: f64::from(top - page.y) / h,
            width: f64::from(right - left) / w,
            height: f64::from(bottom - top) / h,
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AskError {
    #[error(
        "{browser}'s Tin extension isn't connected ({reason}). Reload the extension at chrome://extensions; \
         if it still isn't, run `tin extension install` and reload it again."
    )]
    NotConnected { browser: String, reason: String },
    #[error("asking {browser}'s Tin extension broke off: {reason}")]
    Broken { browser: String, reason: String },
    #[error("{browser}'s Tin extension couldn't answer: {message}")]
    Refused { browser: String, message: String },
    #[error("{browser}'s Tin extension answered with something Tin doesn't understand: {reason}")]
    Unexpected { browser: String, reason: String },
}

/// Where a browser's helper listens: `$XDG_RUNTIME_DIR/tin-<browser>.sock`, else a per-user file
/// in the temp folder.
pub fn socket_path(browser: &str) -> PathBuf {
    let browser: String = browser.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect();
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join(format!("tin-{browser}.sock")),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("tin-{user}-{browser}.sock"))
        }
    }
}

#[derive(Deserialize)]
struct Found {
    kind: String,
    url: String,
    #[serde(default)]
    alt: Option<String>,
}

/// The extension's answer: `{"ok": true, …}` or `{"ok": false, "error": …}`.
#[derive(Deserialize)]
struct Answer<T> {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    body: Option<T>,
}

#[derive(Deserialize)]
struct MediaAnswer {
    media: Vec<Found>,
}

/// The images and videos in `area` of the page window `window` shows, in `browser`.
pub fn media_in(browser: &str, window: i64, area: Area) -> Result<Vec<Media>, AskError> {
    let answer: MediaAnswer = ask(&socket_path(browser), browser, &Request::MediaIn { window, area })?;
    Ok(answer.media.into_iter().filter_map(|f| Media::found(&f.kind, f.url, f.alt)).collect())
}

fn ask<T: for<'de> Deserialize<'de>>(socket: &Path, browser: &str, request: &Request) -> Result<T, AskError> {
    let broken = |e: std::io::Error| AskError::Broken { browser: browser.into(), reason: e.to_string() };
    let unexpected = |reason: String| AskError::Unexpected { browser: browser.into(), reason };
    let mut stream = UnixStream::connect(socket).map_err(|e| AskError::NotConnected {
        browser: browser.into(),
        reason: format!("{}: {e}", socket.display()),
    })?;
    stream.set_read_timeout(Some(ASK_TIMEOUT)).map_err(broken)?;
    let json = serde_json::to_vec(request).map_err(|e| unexpected(e.to_string()))?;
    write_frame(&mut stream, &json).map_err(broken)?;
    let frame = read_frame(&mut stream).map_err(broken)?.ok_or_else(|| AskError::Broken {
        browser: browser.into(),
        reason: "the helper closed without answering".into(),
    })?;
    let answer: Answer<T> = serde_json::from_slice(&frame).map_err(|e| unexpected(e.to_string()))?;
    match answer {
        Answer { ok: true, body: Some(body), .. } => Ok(body),
        Answer { ok: true, body: None, .. } => Err(unexpected("an answer without its contents".into())),
        Answer { ok: false, error, .. } => Err(AskError::Refused {
            browser: browser.into(),
            message: error.unwrap_or_else(|| "no reason given".into()),
        }),
    }
}

type Pending = Arc<Mutex<HashMap<u64, Sender<serde_json::Value>>>>;
type Shared<W> = Arc<Mutex<W>>;

/// The helper's side: listens on `socket` and passes each question to the extension (written to
/// `output` as `{"type":"request","id":…,"request":…}`), and its answer (read from `input` as
/// `{"type":"reply","id":…,"reply":…}`) back. Every other message from the extension goes to
/// `other`. Returns when the extension disconnects, and removes the socket.
pub fn relay<W: Write + Send + 'static>(
    socket: &Path,
    mut input: impl Read,
    output: W,
    mut other: impl FnMut(Vec<u8>),
) -> std::io::Result<()> {
    // A helper left behind by a browser that crashed leaves its socket file.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let pending: Pending = Arc::default();
    let output: Shared<W> = Arc::new(Mutex::new(output));
    {
        let (pending, output) = (pending.clone(), output.clone());
        let next = Arc::new(AtomicU64::new(1));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (pending, output, next) = (pending.clone(), output.clone(), next.clone());
                std::thread::spawn(move || answer(stream, &pending, &output, &next));
            }
        });
    }
    let result = loop {
        match read_frame(&mut input) {
            Ok(Some(frame)) => match serde_json::from_slice::<Reply>(&frame) {
                Ok(reply) => {
                    let waiting = pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&reply.id);
                    if let Some(tx) = waiting {
                        let _ = tx.send(reply.reply);
                    }
                }
                Err(_) => other(frame),
            },
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    let _ = std::fs::remove_file(socket);
    result
}

#[derive(Deserialize)]
#[serde(tag = "type", rename = "reply")]
struct Reply {
    id: u64,
    reply: serde_json::Value,
}

/// One question from Tin: pass it on, wait for the extension, answer.
fn answer<W: Write>(mut stream: UnixStream, pending: &Pending, output: &Shared<W>, next: &AtomicU64) {
    let Ok(Some(frame)) = read_frame(&mut stream) else { return };
    let reply = match serde_json::from_slice::<serde_json::Value>(&frame) {
        Ok(request) => {
            let id = next.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = mpsc::channel();
            pending.lock().unwrap_or_else(PoisonError::into_inner).insert(id, tx);
            let message = serde_json::json!({ "type": "request", "id": id, "request": request });
            let sent = write_frame(
                &mut *output.lock().unwrap_or_else(PoisonError::into_inner),
                message.to_string().as_bytes(),
            );
            let reply = match sent {
                Ok(()) => rx.recv_timeout(ANSWER_TIMEOUT).unwrap_or_else(|_| {
                    serde_json::json!({ "ok": false, "error": format!("the extension didn't answer within {} s", ANSWER_TIMEOUT.as_secs()) })
                }),
                Err(e) => serde_json::json!({ "ok": false, "error": format!("couldn't reach the extension: {e}") }),
            };
            pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            reply
        }
        Err(e) => serde_json::json!({ "ok": false, "error": format!("not a question the helper can read: {e}") }),
    };
    let _ = write_frame(&mut stream, reply.to_string().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_becomes_fractions_of_the_page_clipped_to_it() {
        let page = Rect { x: 100, y: 200, width: 1000, height: 500 };
        let area = Area::within(Rect { x: 350, y: 300, width: 200, height: 100 }, page);
        assert_eq!(area, Some(Area { left: 0.25, top: 0.2, width: 0.2, height: 0.2 }));
        // A window pick takes in the browser's toolbar too: only the page part counts.
        let whole = Area::within(Rect { x: 100, y: 120, width: 1000, height: 580 }, page);
        assert_eq!(whole, Some(Area { left: 0.0, top: 0.0, width: 1.0, height: 1.0 }));
        assert_eq!(Area::within(Rect { x: 100, y: 120, width: 300, height: 60 }, page), None, "the tab strip");
    }

    #[test]
    fn questions_go_to_the_extension_and_answers_come_back() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let socket = dir.path().join("tin-test.sock");
        // The extension's end of the native-messaging pipe, and the helper's.
        let (mut extension, helper) = UnixStream::pair().unwrap_or_else(|e| panic!("{e}"));
        let helper_out = helper.try_clone().unwrap_or_else(|e| panic!("{e}"));
        let others = Arc::new(Mutex::new(Vec::new()));
        let seen = others.clone();
        let relay_socket = socket.clone();
        let relaying = std::thread::spawn(move || {
            relay(&relay_socket, helper, helper_out, |frame| {
                seen.lock().unwrap_or_else(PoisonError::into_inner).push(frame)
            })
        });
        // A report on the same port isn't a reply: it goes to `other`.
        write_frame(&mut extension, br#"{"type":"focus","window":1}"#).unwrap_or_else(|e| panic!("{e}"));
        // The extension: answers one question with a photo, an unknown kind and a video.
        let fake = std::thread::spawn(move || {
            let frame = read_frame(&mut extension).ok().flatten().unwrap_or_default();
            let question: serde_json::Value = serde_json::from_slice(&frame).unwrap_or_default();
            assert_eq!(question["request"]["type"], "media_in");
            assert_eq!(question["request"]["window"], 7);
            let reply = serde_json::json!({ "type": "reply", "id": question["id"], "reply": { "ok": true, "media": [
                { "kind": "photo", "url": "https://a.com/car.jpg", "alt": "A black Passat" },
                { "kind": "audio", "url": "https://a.com/a.mp3" },
                { "kind": "video", "url": "blob:https://a.com/1" },
            ] } });
            write_frame(&mut extension, reply.to_string().as_bytes()).unwrap_or_else(|e| panic!("{e}"));
            extension
        });
        // Tin, once the helper listens.
        let area = Area { left: 0.0, top: 0.0, width: 1.0, height: 1.0 };
        let mut media = Err(AskError::Broken { browser: "test".into(), reason: "never asked".into() });
        for _ in 0..100 {
            media = ask::<MediaAnswer>(&socket, "test", &Request::MediaIn { window: 7, area })
                .map(|a| a.media.into_iter().filter_map(|f| Media::found(&f.kind, f.url, f.alt)).collect::<Vec<_>>());
            if !matches!(media, Err(AskError::NotConnected { .. })) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let media = media.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            media.iter().map(|m| m.url.as_str()).collect::<Vec<_>>(),
            ["https://a.com/car.jpg", "blob:https://a.com/1"]
        );
        assert_eq!(media[0].alt.as_deref(), Some("A black Passat"));
        // The browser closes the port: the helper stops and its socket goes.
        drop(fake.join().unwrap_or_else(|_| panic!("the fake extension failed")));
        relaying.join().unwrap_or_else(|_| panic!("the relay panicked")).unwrap_or_else(|e| panic!("{e}"));
        assert!(!socket.exists());
        assert_eq!(others.lock().unwrap_or_else(PoisonError::into_inner).len(), 1);
    }

    #[test]
    fn an_extension_that_isnt_listening_says_so() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let result = ask::<MediaAnswer>(
            &dir.path().join("none.sock"),
            "helium",
            &Request::MediaIn { window: 1, area: Area { left: 0.0, top: 0.0, width: 1.0, height: 1.0 } },
        );
        assert!(matches!(result, Err(AskError::NotConnected { .. })));
    }
}
