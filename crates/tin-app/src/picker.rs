//! A capture by picking: every monitor frozen (`select.rs`), a window or area picked on one, then
//! this small card with what you picked and a voice note about it.

use std::sync::Arc;

use rune::tok::{self, type_};
use rune::{
    AppHandle, Color, Cx, El, Key, KeyEvent, Pixels, SurfaceFit, TextEdit, Tx, col, el, row, spacer, text, text_input,
};
use tin_core::Tin;
use tin_core::capture::{Draft, Window};
use tin_core::context;
use tin_core::intent::Intent;
use tin_core::screenshot::Rect;
use tin_core::snapshot::{Selection, SelectionKind, Snapshot};

use crate::ui::{Status, Ui, Voice};

pub struct Picker {
    pub me: AppHandle<Picker>,
    pub main: AppHandle<Ui>,
    tin: Result<Tin, String>,
    /// The picked area, for the card's thumbnail.
    thumbnail: Pixels,
    pub stage: Stage,
    pub purpose: TextEdit,
}

/// What was picked, on which frozen monitor.
#[derive(Clone)]
pub struct Pick {
    pub snap: Arc<Snapshot>,
    /// Points from the monitor's top-left.
    pub rect: Rect,
    pub kind: SelectionKind,
    pub window: Option<Window>,
}

pub enum Stage {
    /// Nothing on screen.
    Idle,
    /// Freezing the monitors, or waiting for a pick on them.
    Selecting,
    /// The card is open: what was picked, the voice note, what it's for. `snaps` stay so Esc can
    /// go back to picking on the same frozen screens.
    Noting { snaps: Arc<Vec<Arc<Snapshot>>>, pick: Pick, voice: Voice },
    /// Writing the capture; the card closes when it's done.
    Saving,
}

impl Picker {
    pub fn new(me: AppHandle<Picker>, main: AppHandle<Ui>) -> Picker {
        Picker {
            me,
            main,
            tin: Tin::open(tin_core::Paths::from_env()).map_err(|e| e.to_string()),
            thumbnail: Pixels::new(),
            stage: Stage::Idle,
            purpose: TextEdit::new(""),
        }
    }

    /// Freezes every monitor and lets the user pick on them.
    pub fn start(&mut self) {
        if !matches!(self.stage, Stage::Idle) {
            return;
        }
        self.stage = Stage::Selecting;
        let (me, main) = (self.me.clone(), self.main.clone());
        std::thread::spawn(move || match context::snapshots() {
            Ok(snaps) => select(me, main, Arc::new(snaps.into_iter().map(Arc::new).collect())),
            Err(e) => fail(&me, &main, e),
        });
    }

    /// Opens the card for a pick (also the headless scenes' way in).
    pub fn show(&mut self, snaps: Arc<Vec<Arc<Snapshot>>>, pick: Pick, voice: Voice) {
        if let Some(image) = pick.snap.crop(pick.rect) {
            self.thumbnail.set(image.width, image.height, image.rgba);
        }
        self.purpose = TextEdit::new("");
        self.purpose.focused = true;
        self.stage = Stage::Noting { snaps, pick, voice };
    }

    fn picked(&mut self, snaps: Arc<Vec<Arc<Snapshot>>>, pick: Pick) {
        self.show(snaps, pick, Voice::Starting);
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
                    let next = match hex.stop() {
                        Ok(note) => Voice::Heard(note),
                        Err(e) => Voice::Failed(e.to_string()),
                    };
                    me.update(move |s, _| {
                        s.set_voice(&Voice::Transcribing, next);
                        s.save();
                    });
                });
            }
            Voice::Idle | Voice::Heard(_) | Voice::Failed(_) => self.save(),
        }
    }

    fn save(&mut self) {
        let Ok(tin) = self.tin.clone() else { return };
        let Stage::Noting { pick, voice, .. } = std::mem::replace(&mut self.stage, Stage::Saving) else { return };
        let said = self.purpose.text.clone();
        let (me, main) = (self.me.clone(), self.main.clone());
        std::thread::spawn(move || {
            let picture = pick.snap.crop(pick.rect).ok_or("the selection is empty".to_owned()).and_then(|i| i.png());
            let draft = Draft {
                page: pick.window.as_ref().and_then(context::page_for),
                window: pick.window,
                voice: match voice {
                    Voice::Heard(note) => Some(note),
                    _ => None,
                },
                screenshot: picture.as_ref().ok().cloned(),
                selection: Some(Selection { output: pick.snap.output.clone(), rect: pick.rect, kind: pick.kind }),
                intent: Intent { said: Some(said), ..Intent::default() },
                ..Draft::default()
            };
            let status = match (picture, tin.save(draft)) {
                (_, Err(e)) => Status::Error(e.to_string()),
                (Err(e), Ok(_)) => Status::Error(format!("Saved without a picture: {e}")),
                (Ok(_), Ok(saved)) => match saved.hook_errors.first() {
                    Some(e) => Status::Error(e.to_string()),
                    None => Status::Info(format!("Saved {}", crate::ui::describe_target(&saved.capture.intent.target))),
                },
            };
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

    /// Esc: drop the note and pick again on the same frozen screens.
    fn back(&mut self, tx: &mut Tx) {
        let Stage::Noting { snaps, voice, .. } = std::mem::replace(&mut self.stage, Stage::Selecting) else {
            return;
        };
        if matches!(voice, Voice::Starting | Voice::Recording) {
            self.cancel_hex();
        }
        tx.close_window();
        let (me, main) = (self.me.clone(), self.main.clone());
        std::thread::spawn(move || select(me, main, snaps));
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

/// Shows the frozen screens and waits for a pick (on a worker thread), then opens the card.
fn select(me: AppHandle<Picker>, main: AppHandle<Ui>, snaps: Arc<Vec<Arc<Snapshot>>>) {
    #[cfg(target_os = "linux")]
    let picked = crate::select::pick(&snaps)
        .map(|p| p.map(|p| Pick { snap: p.snap, rect: p.rect, kind: p.kind, window: p.window }));
    #[cfg(not(target_os = "linux"))]
    let picked: Result<Option<Pick>, String> = Err("Picking isn't built for this OS yet.".into());
    match picked {
        Ok(Some(pick)) => {
            me.update(move |s, _| s.picked(snaps, pick));
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
            s.back(tx);
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
        .on_submit(|s: &mut Picker, _| s.finish());
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
        .child(
            row()
                .child(text("Enter saves · Esc picks again").font(type_::LABEL).color(t.text_secondary))
                .child(spacer()),
        )
}
