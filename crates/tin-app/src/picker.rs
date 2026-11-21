//! A capture by picking: every monitor frozen (`select.rs`), a window or area picked on one, then
//! this small card with what you picked and a voice note about it.

use std::sync::Arc;

use rune::tok::{self, type_};
use rune::{
    AppHandle, Color, Cx, El, Key, KeyEvent, Pixels, SurfaceFit, TextEdit, Tx, col, el, row, spacer, text, text_input,
};
use tin_core::Tin;
use tin_core::bridge::ask::{Area, AskError};
use tin_core::capture::{CapturedText, Draft, TextSource, Window};
use tin_core::context;
use tin_core::intent::Intent;
use tin_core::media::{Kept, Media};
use tin_core::page::{Page, PageSource};
use tin_core::screenshot::Rect;
use tin_core::snapshot::{ElementContent, Selection, SelectionKind, Snapshot, WebPage};

use crate::ui::{Status, Ui, Voice};

pub struct Picker {
    pub me: AppHandle<Picker>,
    pub main: AppHandle<Ui>,
    tin: Result<Tin, String>,
    /// The picked area, for the card's thumbnail.
    thumbnail: Pixels,
    pub stage: Stage,
    pub purpose: TextEdit,
    /// Rune's overlays to pick on, when trying them (`TIN_PICKER=rune`) instead of `select.rs`.
    screens: Option<Screens>,
}

/// The Rune picker's handle, where it's built.
#[cfg(target_os = "linux")]
pub type Screens = crate::screens::Screens;
#[cfg(not(target_os = "linux"))]
pub type Screens = ();

/// What was picked, on which frozen monitor.
#[derive(Clone)]
pub struct Pick {
    pub snap: Arc<Snapshot>,
    /// Points from the monitor's top-left.
    pub rect: Rect,
    pub kind: SelectionKind,
    pub window: Option<Window>,
    /// The web page picked from, as the browser described it.
    pub page: Option<WebPage>,
    /// The app's own element picked, when it came from the accessibility tree.
    #[cfg_attr(not(target_os = "linux"), expect(dead_code, reason = "only Linux reads apps' elements so far"))]
    pub node: Option<ElementRef>,
}

/// An element in another app, where Tin can reach apps' accessibility trees.
#[cfg(target_os = "linux")]
pub type ElementRef = context::atspi::Node;
#[cfg(not(target_os = "linux"))]
pub type ElementRef = ();

pub enum Stage {
    /// Nothing on screen.
    Idle,
    /// Freezing the monitors, or waiting for a pick on them.
    Selecting,
    /// The card is open: what was picked, the voice note, what it's for.
    Noting { pick: Box<Pick>, voice: Voice },
    /// Writing the capture; the card closes when it's done.
    Saving,
}

impl Picker {
    pub fn new(me: AppHandle<Picker>, main: AppHandle<Ui>, screens: Option<Screens>) -> Picker {
        Picker {
            me,
            main,
            screens,
            tin: Tin::open(tin_core::Paths::from_env()).map_err(|e| e.to_string()),
            thumbnail: Pixels::new(),
            stage: Stage::Idle,
            purpose: TextEdit::new(""),
        }
    }

    /// Freezes every monitor and lets the user pick on them.
    pub fn start(&mut self) {
        // Double-tapping again while picking on Rune's overlays cancels: the way out even if the
        // overlays never get the keyboard.
        #[cfg(target_os = "linux")]
        if let (Stage::Selecting, Some(screens)) = (&self.stage, &self.screens) {
            screens.cancel();
            return;
        }
        if !matches!(self.stage, Stage::Idle) {
            return;
        }
        self.stage = Stage::Selecting;
        let (me, main, screens) = (self.me.clone(), self.main.clone(), self.screens.clone());
        std::thread::spawn(move || {
            // Something in the session can switch accessibility off again; apps only publish
            // their elements while it's on.
            #[cfg(target_os = "linux")]
            let _ = context::atspi::enable();
            match context::snapshots() {
                Ok(snaps) => select(me, main, screens, Arc::new(snaps.into_iter().map(Arc::new).collect())),
                Err(e) => fail(&me, &main, e),
            }
        });
    }

    /// Opens the card for a pick (also the headless scenes' way in).
    pub fn show(&mut self, pick: Pick, voice: Voice) {
        if let Some(image) = pick.snap.crop(pick.rect) {
            self.thumbnail.set(image.width, image.height, image.rgba);
        }
        self.purpose = TextEdit::new("");
        self.purpose.focused = true;
        self.stage = Stage::Noting { pick: Box::new(pick), voice };
    }

    fn picked(&mut self, pick: Pick) {
        self.show(pick, Voice::Starting);
        let Ok(tin) = &self.tin else { return };
        let (hex, me) = (tin.hex(), self.me.clone());
        std::thread::spawn(move || {
            let next = match hex.start() {
                Ok(()) => Voice::Recording,
                Err(e) => Voice::Failed(e.to_string()),
            };
            me.update(move |s, _| s.set_voice(&Voice::Starting, next));
        });
    }

    /// Applies a worker's result if the note is still where the worker left it.
    fn set_voice(&mut self, expected: &Voice, next: Voice) {
        match &mut self.stage {
            Stage::Noting { voice, .. } if voice == expected => *voice = next,
            _ if next == Voice::Recording => self.cancel_hex(),
            _ => {}
        }
    }

    /// Enter: stop the recording if one runs, then save.
    fn finish(&mut self) {
        let Stage::Noting { voice, .. } = &mut self.stage else { return };
        match voice {
            Voice::Starting | Voice::Transcribing => {}
            Voice::Recording => {
                *voice = Voice::Transcribing;
                let Ok(tin) = &self.tin else { return };
                let (hex, me) = (tin.hex(), self.me.clone());
                std::thread::spawn(move || {
                    let stopped = hex.stop();
                    me.update(move |s, _| match stopped {
                        Ok(note) => {
                            s.set_voice(&Voice::Transcribing, Voice::Heard(note));
                            s.save();
                        }
                        // Keep the card open on the error, so a lost note isn't saved silently:
                        // Enter again saves without it.
                        Err(e) => s.set_voice(&Voice::Transcribing, Voice::Failed(e.to_string())),
                    });
                });
            }
            Voice::Idle | Voice::Heard(_) | Voice::Failed(_) => self.save(),
        }
    }

    fn save(&mut self) {
        let Ok(tin) = self.tin.clone() else { return };
        let Stage::Noting { pick, voice, .. } = std::mem::replace(&mut self.stage, Stage::Saving) else { return };
        let pick = *pick;
        let said = self.purpose.text.clone();
        let (me, main) = (self.me.clone(), self.main.clone());
        std::thread::spawn(move || {
            let picture = pick.snap.crop(pick.rect).ok_or("the selection is empty".to_owned()).and_then(|i| i.png());
            let app = pick.window.as_ref().and_then(|w| w.app.clone()).unwrap_or_else(|| "browser".into());
            let window_title = pick.window.as_ref().and_then(|w| w.title.clone());
            // What the picked element says and links to, kept as words beside its picture.
            #[cfg(target_os = "linux")]
            let content = pick.node.as_ref().and_then(|node| {
                let a11y = context::atspi::Accessibility::connect().ok()?;
                Some(a11y.content_in(node, 400)).filter(|c| !c.is_empty())
            });
            #[cfg(not(target_os = "linux"))]
            let content: Option<tin_core::snapshot::ElementContent> = None;
            // A tweet picked on X: its own link, from the links inside the picked element.
            let tweet = content.as_ref().and_then(|c| {
                let (handle, id) = tin_core::tweet::permalink(c.links.iter().map(|l| &l.url))?;
                Some(Page::new(tin_core::tweet::url(&handle, &id), None, PageSource::PickedPage { app: app.clone() }))
            });
            // The images and videos where it was picked, from Tin's extension. A tweet's are kept
            // with the tweet instead.
            let found = match (&tweet, &pick.page, window_title.as_deref()) {
                (None, Some(page), Some(title)) => page_media(page, pick.rect, title),
                _ => Ok(Vec::new()),
            };
            let media_error = found.as_ref().err().map(ToString::to_string);
            let content = match (content, found.unwrap_or_default()) {
                (content, media) if media.is_empty() => content,
                (Some(content), media) => Some(ElementContent { media, ..content }),
                (None, media) => Some(ElementContent { media, ..ElementContent::default() }),
            };
            let has_media = content.as_ref().is_some_and(|c| !c.media.is_empty());
            let draft = Draft {
                // The tweet picked; else the page the browser said was picked; else the browser's
                // session file.
                page: match (tweet, pick.page, &pick.window) {
                    (Some(tweet), ..) => Some(tweet),
                    (None, Some(page), _) => Some(Page::new(page.url, page.title, PageSource::PickedPage { app })),
                    (None, None, window) => window.as_ref().and_then(context::page_for),
                },
                window: pick.window,
                voice: match voice {
                    Voice::Heard(note) => Some(note),
                    _ => None,
                },
                // Text selected in that browser window, as Tin's extension reported it.
                text: window_title.as_deref().and_then(|title| {
                    tin_core::bridge::selection_for(&tin_core::Paths::from_env().data, title)
                        .map(|s| CapturedText { text: s.text, from: TextSource::Selection })
                }),
                screenshot: picture.as_ref().ok().cloned(),
                selection: Some(Selection {
                    output: pick.snap.output.clone(),
                    rect: pick.rect,
                    kind: pick.kind,
                    content,
                }),
                intent: Intent { said: Some(said), ..Intent::default() },
                ..Draft::default()
            };
            let saved = tin.save(draft);
            let status = match (picture, &saved, media_error) {
                (_, Err(e), _) => Status::Error(e.to_string()),
                (Err(e), Ok(_), _) => Status::Error(format!("Saved without a picture: {e}")),
                (Ok(_), Ok(saved), media_error) => match (saved.hook_errors.first(), media_error) {
                    (Some(e), _) => Status::Error(e.to_string()),
                    (None, Some(e)) => Status::Error(format!("Saved, without the page's images and videos: {e}")),
                    (None, None) => {
                        Status::Info(format!("Saved {}", crate::ui::describe_target(&saved.capture.intent.target)))
                    }
                },
            };
            // Keep the tweet, and the images and videos, in case they're deleted: in the
            // background, so the card closes now. One after the other, since each rewrites the
            // capture.
            if let Ok(saved) = saved {
                let is_tweet = matches!(
                    saved.capture.page.as_ref().map(|p| &p.kind),
                    Some(tin_core::page::PageKind::Tweet { .. })
                );
                let (tin, main, id) = (tin.clone(), main.clone(), saved.capture.id.clone());
                std::thread::spawn(move || {
                    let mut statuses = Vec::new();
                    if is_tweet {
                        statuses.push(match tin.archive_tweet(&id) {
                            Ok(c) => Status::Info(format!(
                                "Kept the tweet: its text and {} media file(s)",
                                c.tweet.as_ref().map_or(0, |t| t.media.len())
                            )),
                            Err(e) => Status::Error(e.to_string()),
                        });
                    }
                    if has_media {
                        statuses.push(match tin.keep_media(&id) {
                            Ok(c) => kept_status(&c),
                            Err(e) => Status::Error(format!("Couldn't keep the page's images and videos: {e}")),
                        });
                    }
                    if let Some(status) =
                        statuses.into_iter().reduce(|a, b| if matches!(a, Status::Error(_)) { a } else { b })
                    {
                        main.update(move |s, _| {
                            s.reload();
                            s.status = Some(status);
                        });
                    }
                });
            }
            main.update(move |s, _| {
                s.reload();
                s.status = Some(status);
            });
            me.update(|s, tx| {
                s.stage = Stage::Idle;
                tx.close_window();
            });
        });
    }

    /// Esc: throw the capture away (Hex stops, nothing is saved) and close the card.
    fn discard(&mut self, tx: &mut Tx) {
        self.closed();
        tx.close_window();
    }

    /// The card closed without saving (Esc, its close button, the compositor): throw the note
    /// away and make sure Hex isn't left recording. Saving moves the stage on before it closes
    /// the window, so it never gets here with a note.
    pub fn closed(&mut self) {
        let (stage, cancel) = after_close(std::mem::replace(&mut self.stage, Stage::Idle));
        self.stage = stage;
        if cancel {
            self.cancel_hex();
        }
    }

    fn cancel_hex(&self) {
        if let Ok(tin) = &self.tin {
            let hex = tin.hex();
            std::thread::spawn(move || {
                let _ = hex.cancel();
            });
        }
    }
}

/// What a closing card leaves behind, and whether Hex must stop recording. Only a showing note
/// is dropped; saving moves on before it closes the window. A transcription
/// already under way finishes, but its result finds no note and is dropped.
fn after_close(stage: Stage) -> (Stage, bool) {
    match stage {
        Stage::Noting { voice, .. } => (Stage::Idle, matches!(voice, Voice::Starting | Voice::Recording)),
        other => (other, false),
    }
}

/// The images and videos Tin's extension finds where `pick` (monitor points) falls on `page`.
/// Nothing when the pick misses the page or no extension reports the window.
fn page_media(page: &WebPage, pick: Rect, window_title: &str) -> Result<Vec<Media>, AskError> {
    let Some(area) = Area::within(pick, page.rect) else { return Ok(Vec::new()) };
    let Some((browser, window)) = tin_core::bridge::browser_window(&tin_core::Paths::from_env().data, window_title)
    else {
        return Ok(Vec::new());
    };
    tin_core::bridge::ask::media_in(&browser, window, area)
}

/// How keeping a capture's images and videos went.
fn kept_status(capture: &tin_core::capture::Capture) -> Status {
    let media = capture.selection.as_ref().and_then(|s| s.content.as_ref()).map_or(&[][..], |c| &c.media[..]);
    let kept = media.iter().filter(|m| matches!(m.kept, Kept::File { .. })).count();
    match media.iter().find_map(|m| match &m.kept {
        Kept::Missing { reason } => Some(reason),
        Kept::File { .. } | Kept::NotYet => None,
    }) {
        Some(reason) => {
            Status::Error(format!("Kept {kept} of {} images and videos; one wasn't: {reason}", media.len()))
        }
        None => Status::Info(format!("Kept {kept} image(s) and video(s) from the page")),
    }
}

/// Shows the frozen screens and waits for a pick (on a worker thread), then opens the card.
fn select(me: AppHandle<Picker>, main: AppHandle<Ui>, screens: Option<Screens>, snaps: Arc<Vec<Arc<Snapshot>>>) {
    #[cfg(target_os = "linux")]
    if let Some(screens) = screens {
        screens.open(snaps.to_vec(), move |picked| match picked {
            Some(pick) => {
                me.update(move |s, _| s.picked(pick));
                me.open();
            }
            None => me.update(|s, _| s.stage = Stage::Idle),
        });
        return;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = screens;
    #[cfg(target_os = "linux")]
    let picked = crate::select::pick(&snaps).map(|p| {
        p.map(|p| Pick { snap: p.snap, rect: p.rect, kind: p.kind, window: p.window, page: p.page, node: p.node })
    });
    #[cfg(not(target_os = "linux"))]
    let picked: Result<Option<Pick>, String> = {
        drop(snaps);
        Err("Picking isn't built for this OS yet.".into())
    };
    match picked {
        Ok(Some(pick)) => {
            me.update(move |s, _| s.picked(pick));
            me.open();
        }
        Ok(None) => me.update(|s, _| s.stage = Stage::Idle),
        Err(e) => fail(&me, &main, e),
    }
}

fn fail(me: &AppHandle<Picker>, main: &AppHandle<Ui>, error: String) {
    me.update(|s, _| s.stage = Stage::Idle);
    main.update(move |s, _| s.status = Some(Status::Error(error)));
    main.open();
}

pub fn on_key(s: &mut Picker, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match ev.key {
        Key::Escape => {
            s.discard(tx);
            true
        }
        Key::Enter => {
            s.finish();
            true
        }
        _ => false,
    }
}

/// The card: what was picked, the voice note, what it's for.
pub fn view(s: &Picker, cx: &mut Cx<Picker>) -> El {
    let t = tok::theme(cx.system_dark());
    let accent = tok::accent(4, cx.system_dark());
    let Stage::Noting { pick, voice, .. } = &s.stage else {
        let saving = if matches!(s.stage, Stage::Saving) { "Saving…" } else { "" };
        return col()
            .w_full()
            .h_full()
            .center()
            .fill(t.bg)
            .child(text(saving).font(type_::LABEL).color(t.text_secondary));
    };
    let what = match (&pick.kind, &pick.window) {
        (SelectionKind::Window, Some(w)) => {
            w.title.clone().or_else(|| w.app.clone()).unwrap_or_else(|| "Window".into())
        }
        _ => format!("Area, {} × {}", pick.rect.width, pick.rect.height),
    };
    let (dot, status): (Color, String) = match voice {
        Voice::Starting => (t.text_secondary, "Starting Hex…".into()),
        Voice::Recording => (Color { r: 0.9, g: 0.2, b: 0.2, a: 1.0 }, "Listening… say what this is about".into()),
        Voice::Transcribing => (t.text_secondary, "Transcribing…".into()),
        Voice::Heard(note) => (accent, format!("“{}”", note.transcript)),
        Voice::Failed(e) => (t.text_secondary, e.clone()),
        Voice::Idle => (t.text_secondary, String::new()),
    };
    let hint = match voice {
        Voice::Failed(_) => "Enter saves without the voice note · Esc discards",
        _ => "Enter saves · Esc discards",
    };
    let purpose = text_input(&s.purpose, type_::BODY.lh(20.0))
        .key("purpose")
        .label("What it's for")
        .single_line()
        .w_full()
        .padding(8.0)
        .radius(6.0)
        .fill(t.card)
        .color(t.text)
        .caret(accent, accent.alpha(0.3))
        .placeholder("What's it for? (optional)", t.text_secondary)
        .on_edit(|s: &mut Picker| &mut s.purpose)
        .on_submit(|s: &mut Picker, _| s.finish())
        // A focused field would otherwise take the first Esc to drop its focus.
        .on_input_key(|s: &mut Picker, ev: &KeyEvent, tx: &mut Tx| {
            let discard = ev.key == Key::Escape;
            if discard {
                s.discard(tx);
            }
            discard
        });
    col()
        .key("card")
        .w_full()
        .h_full()
        .padding(14.0)
        .gap(10.0)
        .fill(t.bg)
        .child(
            el().key("thumbnail")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .radius(8.0)
                .fill(Color::BLACK.alpha(0.3))
                .pixels_fit(&s.thumbnail, SurfaceFit::Fit),
        )
        .child(text(what).key("what").font(type_::LABEL).color(t.text_secondary).truncate())
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(el().size(8.0, 8.0).radius(4.0).fill(dot))
                .child(text(status).key("voice").font(type_::BODY).color(t.text).wrap().grow(1.0).shrink().min_w(0.0)),
        )
        .child(purpose)
        .child(row().child(text(hint).font(type_::LABEL).color(t.text_secondary)).child(spacer()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tin_core::screenshot::Image;

    fn noting(voice: Voice) -> Stage {
        let snap = Arc::new(Snapshot {
            output: "DP-1".into(),
            display: None,
            bounds: Rect { x: 0, y: 0, width: 1, height: 1 },
            image: Image { width: 1, height: 1, rgba: vec![0; 4] },
            windows: Vec::new(),
        });
        let pick = Pick {
            snap: snap.clone(),
            rect: Rect { x: 0, y: 0, width: 1, height: 1 },
            kind: SelectionKind::Region,
            window: None,
            page: None,
            node: None,
        };
        Stage::Noting { pick: Box::new(pick), voice }
    }

    #[test]
    fn closing_a_card_that_records_drops_the_note_and_stops_hex() {
        for voice in [Voice::Starting, Voice::Recording] {
            let (stage, cancel) = after_close(noting(voice));
            assert!(matches!(stage, Stage::Idle) && cancel);
        }
    }

    #[test]
    fn closing_after_hex_stopped_drops_the_note_without_a_cancel() {
        for voice in [Voice::Idle, Voice::Transcribing, Voice::Failed("x".into())] {
            let (stage, cancel) = after_close(noting(voice));
            assert!(matches!(stage, Stage::Idle) && !cancel);
        }
    }

    #[test]
    fn closing_while_picking_again_or_saving_changes_nothing() {
        assert!(matches!(after_close(Stage::Selecting), (Stage::Selecting, false)));
        assert!(matches!(after_close(Stage::Saving), (Stage::Saving, false)));
    }
}
