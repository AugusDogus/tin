//! The picker: the monitor you're on, frozen, to pick a window (click) or an area (drag) from,
//! then a voice note about it.

use std::sync::Arc;

use rune::tok::type_;
use rune::{
    AppHandle, Color, Cursor, Cx, Drag, El, Key, KeyEvent, Phase, Pixels, TextEdit, Tx, col, el, row, text, text_input,
};
use tin_core::Tin;
use tin_core::capture::{Draft, Window};
use tin_core::context;
use tin_core::intent::Intent;
use tin_core::screenshot::Rect;
use tin_core::snapshot::{Selection, SelectionKind, Snapshot};

use crate::ui::{Status, Ui, Voice};

/// A drag shorter than this is a click.
const CLICK_SLOP: f32 = 6.0;

pub struct Picker {
    pub me: AppHandle<Picker>,
    pub main: AppHandle<Ui>,
    tin: Result<Tin, String>,
    pixels: Pixels,
    pub stage: Stage,
    pub purpose: TextEdit,
    /// The pointer in window points.
    pub pointer: (f32, f32),
}

pub enum Stage {
    /// Nothing on screen.
    Idle,
    /// Taking the snapshot; the window isn't open yet.
    Freezing,
    Picking {
        snap: Arc<Snapshot>,
        drag: Option<Rect>,
    },
    Noting {
        snap: Arc<Snapshot>,
        selection: Selection,
        window: Option<Window>,
        voice: Voice,
    },
    /// Writing the capture; the window closes when it's done.
    Saving,
}

impl Picker {
    pub fn new(me: AppHandle<Picker>, main: AppHandle<Ui>) -> Picker {
        Picker {
            me,
            main,
            tin: Tin::open(tin_core::Paths::from_env()).map_err(|e| e.to_string()),
            pixels: Pixels::new(),
            stage: Stage::Idle,
            purpose: TextEdit::new(""),
            pointer: (0.0, 0.0),
        }
    }

    /// Freezes the monitor you're on and opens the picker over it.
    pub fn start(&mut self) {
        if !matches!(self.stage, Stage::Idle) {
            return;
        }
        self.stage = Stage::Freezing;
        let (me, main) = (self.me.clone(), self.main.clone());
        std::thread::spawn(move || match context::snapshot() {
            Ok(snap) => {
                me.update(move |s, _| {
                    s.pixels.set(snap.image.width, snap.image.height, snap.image.rgba.clone());
                    s.stage = Stage::Picking { snap: Arc::new(snap), drag: None };
                });
                me.open();
            }
            Err(e) => {
                me.update(|s, _| s.stage = Stage::Idle);
                main.update(move |s, _| s.status = Some(Status::Error(e)));
                main.open();
            }
        });
    }

    fn on_drag(&mut self, d: &Drag) {
        let Stage::Picking { snap, drag } = &mut self.stage else { return };
        let [x, y] = d.window_point();
        self.pointer = (x, y);
        let start = (d.start_x, d.start_y);
        let moved = (x - start.0).hypot(y - start.1) > CLICK_SLOP;
        match d.phase {
            Phase::Down => *drag = None,
            Phase::Move if moved => *drag = Some(Rect::between(point(start), point((x, y)))),
            Phase::Move => {}
            Phase::Up if d.cancelled() => *drag = None,
            Phase::Up => {
                let snap = snap.clone();
                let picked = match drag.take() {
                    Some(rect) if rect.width >= 4 && rect.height >= 4 => Some((rect, SelectionKind::Region, None)),
                    _ => {
                        let (px, py) = point((x, y));
                        snap.window_at(px, py).map(|w| (w.rect, SelectionKind::Window, Some(w.window.clone())))
                    }
                };
                if let Some((rect, kind, window)) = picked {
                    self.select(snap, rect, kind, window);
                }
            }
        }
    }

    /// Shows a snapshot without taking one (headless scenes).
    pub fn show(&mut self, snap: Snapshot) {
        self.pixels.set(snap.image.width, snap.image.height, snap.image.rgba.clone());
        self.stage = Stage::Picking { snap: Arc::new(snap), drag: None };
    }

    fn select(&mut self, snap: Arc<Snapshot>, rect: Rect, kind: SelectionKind, window: Option<Window>) {
        let selection = Selection { output: snap.output.clone(), rect, kind };
        self.purpose = TextEdit::new("");
        self.purpose.focused = true;
        self.stage = Stage::Noting { snap, selection, window, voice: Voice::Starting };
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
        let Stage::Noting { snap, selection, window, voice } = std::mem::replace(&mut self.stage, Stage::Saving) else {
            return;
        };
        let said = self.purpose.text.clone();
        let (me, main) = (self.me.clone(), self.main.clone());
        std::thread::spawn(move || {
            let picture = snap.crop(selection.rect).ok_or("the selection is empty".to_owned()).and_then(|i| i.png());
            let page = window.as_ref().and_then(context::page_for);
            let draft = Draft {
                page,
                window,
                voice: match voice {
                    Voice::Heard(note) => Some(note),
                    _ => None,
                },
                screenshot: picture.as_ref().ok().cloned(),
                selection: Some(selection),
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
            me.update(|s, tx| s.close(tx));
        });
    }

    /// Esc: back from the note to picking, or close.
    fn back(&mut self, tx: &mut Tx) {
        match std::mem::replace(&mut self.stage, Stage::Idle) {
            Stage::Noting { snap, voice, .. } => {
                if matches!(voice, Voice::Starting | Voice::Recording) {
                    self.cancel_hex();
                }
                self.stage = Stage::Picking { snap, drag: None };
            }
            Stage::Saving => self.stage = Stage::Saving,
            _ => self.close(tx),
        }
    }

    fn close(&mut self, tx: &mut Tx) {
        if let Stage::Noting { voice: Voice::Starting | Voice::Recording, .. } = self.stage {
            self.cancel_hex();
        }
        self.stage = Stage::Idle;
        tx.close_window();
    }

    fn cancel_hex(&self) {
        if let Ok(tin) = &self.tin {
            let hex = tin.hex();
            std::thread::spawn(move || {
                let _ = hex.cancel();
            });
        }
    }

    /// The frozen monitor's size in points.
    fn size(&self) -> (f32, f32) {
        match &self.stage {
            Stage::Picking { snap, .. } | Stage::Noting { snap, .. } => {
                (snap.bounds.width as f32, snap.bounds.height as f32)
            }
            _ => (0.0, 0.0),
        }
    }

    /// What to outline: the drag, the window under the pointer, or the selection.
    fn highlight(&self) -> Option<(Rect, Option<String>)> {
        match &self.stage {
            Stage::Picking { drag: Some(rect), .. } => Some((*rect, Some(format!("{} × {}", rect.width, rect.height)))),
            Stage::Picking { snap, drag: None } => {
                let (x, y) = point(self.pointer);
                snap.window_at(x, y).map(|w| (w.rect, Some(window_label(&w.window))))
            }
            Stage::Noting { selection, window, .. } => Some((selection.rect, window.as_ref().map(window_label))),
            _ => None,
        }
    }
}

/// Window points to snapshot points. The picker covers the monitor it froze, so they match.
fn point((x, y): (f32, f32)) -> (i32, i32) {
    (x.round() as i32, y.round() as i32)
}

fn window_label(w: &Window) -> String {
    w.title.clone().or_else(|| w.app.clone()).unwrap_or_else(|| "Window".into())
}

pub fn on_key(s: &mut Picker, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match ev.key {
        Key::Escape => {
            s.back(tx);
            true
        }
        Key::Enter if matches!(s.stage, Stage::Noting { .. }) => {
            s.finish();
            true
        }
        _ => false,
    }
}

pub fn view(s: &Picker, _: &mut Cx<Picker>) -> El {
    let accent = rune::tok::accent(4, true);
    let shade = Color::BLACK.alpha(0.55);
    let mut root = el()
        .key("picker")
        .w_full()
        .h_full()
        .fill(Color::BLACK)
        .cursor(Cursor::Crosshair)
        .child(el().absolute(0.0, 0.0).w_full().h_full().pixels(&s.pixels))
        .on_pointer_move(|s: &mut Picker, p| s.pointer = (p.x, p.y))
        .on_drag(|s: &mut Picker, d, _| s.on_drag(d));

    let (bw, bh) = s.size();
    match s.highlight() {
        Some((r, label)) => {
            let (x, y, w, h) = (r.x as f32, r.y as f32, r.width as f32, r.height as f32);
            // Shade everything but the highlight: above, below, left and right of it.
            for (k, (sx, sy, sw, sh)) in [
                ("above", (0.0, 0.0, bw, y)),
                ("below", (0.0, y + h, bw, bh - y - h)),
                ("left", (0.0, y, x, h)),
                ("right", (x + w, y, bw - x - w, h)),
            ] {
                if sw > 0.0 && sh > 0.0 {
                    root = root.child(el().key(k).absolute(sx, sy).size(sw, sh).fill(shade));
                }
            }
            root = root.child(el().key("outline").absolute(x, y).size(w, h).border(2.0, accent).radius(3.0));
            if let Some(label) = label {
                let label_y = if y >= 30.0 { y - 28.0 } else { y + 6.0 };
                root = root.child(
                    row()
                        .key("label")
                        .absolute(x + 6.0, label_y)
                        .h(22.0)
                        .px(8.0)
                        .radius(6.0)
                        .center_y()
                        .fill(Color::BLACK.alpha(0.75))
                        .child(text(label).font(type_::LABEL).color(Color::WHITE).truncate().max_w(420.0)),
                );
            }
            if let Stage::Noting { voice, .. } = &s.stage {
                let below = y + h + 10.0;
                let card_y = if below + 120.0 < bh { below } else { (y + h - 130.0).max(10.0) };
                root = root.child(note_card(s, voice, accent).absolute(x.max(10.0), card_y));
            }
        }
        None => root = root.child(el().key("shade").absolute(0.0, 0.0).w_full().h_full().fill(shade)),
    }

    let hint = match s.stage {
        Stage::Picking { .. } => "Click a window or drag an area · Esc to cancel",
        Stage::Noting { .. } => "Say what this is about · Enter saves · Esc goes back",
        Stage::Saving => "Saving…",
        _ => "",
    };
    root.child(
        row().key("hint").absolute(0.0, 14.0).w_full().center().child(
            el().h(30.0)
                .px(14.0)
                .radius(15.0)
                .center()
                .fill(Color::BLACK.alpha(0.75))
                .child(text(hint).font(type_::LABEL).color(Color::WHITE)),
        ),
    )
}

fn note_card(s: &Picker, voice: &Voice, accent: Color) -> El {
    let white = Color::WHITE;
    let dim = Color::WHITE.alpha(0.6);
    let (dot, status): (Color, String) = match voice {
        Voice::Starting => (dim, "Starting Hex…".into()),
        Voice::Recording => {
            (Color { r: 0.95, g: 0.3, b: 0.3, a: 1.0 }, "Listening… press Enter when you're done".into())
        }
        Voice::Transcribing => (dim, "Transcribing…".into()),
        Voice::Heard(note) => (accent, format!("“{}”", note.transcript)),
        Voice::Failed(e) => (dim, e.clone()),
        Voice::Idle => (dim, String::new()),
    };
    let purpose = text_input(&s.purpose, type_::BODY.lh(20.0))
        .key("purpose")
        .label("What it's for")
        .single_line()
        .w_full()
        .color(white)
        .caret(accent, accent.alpha(0.3))
        .placeholder("What's it for? (optional)", dim)
        .on_edit(|s: &mut Picker| &mut s.purpose)
        .on_submit(|s: &mut Picker, _| s.finish());
    col()
        .key("note")
        .w(380.0)
        .padding(12.0)
        .gap(8.0)
        .radius(10.0)
        .fill(rune::tok::theme(true).card)
        .border(1.0, rune::tok::theme(true).card_border)
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.5))
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(el().size(8.0, 8.0).radius(4.0).fill(dot))
                .child(text(status).font(type_::LABEL).color(white).wrap().grow(1.0).shrink().min_w(0.0)),
        )
        .child(purpose)
}
