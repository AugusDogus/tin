//! Picking a window, something in it, or an area, on frozen monitors, with Rune's overlays.
//!
//! One full-screen overlay covers each monitor and shows what it showed when the picker opened,
//! dimmed. The element under the pointer (see `elements.rs`) shows bright and outlined on
//! whichever monitor the pointer is on; the scroll wheel grows or shrinks it, a click picks it, a
//! drag picks an area, and Escape cancels.
//!
//! On trial beside `select.rs`: Tin uses it when `TIN_PICKER=rune` is set.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use rune::tok::type_;
use rune::{
    App, Color, Cursor, Cx, DisplayTarget, Drag, El, KeyEvent, KeyPolicy, NativeDisplayId, OverlayHandle, OverlayInput,
    OverlayLevel, OverlayOptions, OverlayPlacement, OverlaySpec, Phase, Pixels, PointerPress, SurfaceFit, Tx,
    WheelEvent, el, row, text,
};
use tin_core::screenshot::Rect;
use tin_core::snapshot::{SelectionKind, Snapshot};

use crate::elements::{Chain, Finder, Item};
use crate::picker::Pick;

/// Monitors the picker covers; more than this many are left uncovered.
const MAX_SCREENS: usize = 4;
const NAMES: [&str; MAX_SCREENS] = ["tin-screen-0", "tin-screen-1", "tin-screen-2", "tin-screen-3"];
/// A press that moves less than this (points) is a click, not a drag.
const CLICK_SLOP: f32 = 6.0;
/// Wheel travel (points) per step out or in.
const WHEEL_STEP: f32 = 40.0;
/// How much of its brightness the frozen screen keeps around what's picked (as `canvas.rs`).
const DIM_KEEP: u16 = 115;
const ACCENT: Color = Color { r: 64.0 / 255.0, g: 196.0 / 255.0, b: 120.0 / 255.0, a: 1.0 };
const HINT: &str = "Click to pick  ·  Scroll to grow or shrink  ·  Drag an area  ·  Esc to cancel";

/// What's called with the pick, or `None` when the picker was cancelled.
type Done = Box<dyn FnOnce(Option<Pick>) + Send>;

/// The overlays, one per monitor, and what they share while picking.
#[derive(Clone)]
pub struct Screens {
    shared: Arc<Shared>,
}

struct Shared {
    overlays: Mutex<Vec<OverlayHandle<Screen>>>,
    session: Mutex<Option<Session>>,
    finder: OnceLock<Finder>,
}

/// One picking, from the frozen monitors to a pick or a cancel.
struct Session {
    snaps: Vec<Arc<Snapshot>>,
    /// Each monitor's window stacking (see `Snapshot::stacking`), filled in by a worker.
    stacking: HashMap<String, Vec<i32>>,
    /// The newest answer about what's under the pointer.
    chain: Option<Chain>,
    /// Steps out (positive) or in (negative) from the default element.
    level: i32,
    /// Wheel travel not yet turned into a step.
    wheel: f32,
    /// The pointer: which monitor, and where on it in points.
    at: Option<(usize, (f32, f32))>,
    /// An area being dragged out: which monitor, and the area.
    drag: Option<(usize, Rect)>,
    done: Option<Done>,
}

/// One overlay's state: which frozen monitor it shows.
pub struct Screen {
    shared: Weak<Shared>,
    index: usize,
    dim: Pixels,
    bright: Pixels,
}

impl Screens {
    /// Adds the overlays to `app`, hidden until a picking opens.
    pub fn install<S: 'static>(app: &mut App<S>) -> Screens {
        let shared =
            Arc::new(Shared { overlays: Mutex::default(), session: Mutex::default(), finder: OnceLock::new() });
        let overlays = (0..MAX_SCREENS)
            .map(|index| {
                let options = OverlayOptions {
                    placement: Some(OverlayPlacement::FullScreen),
                    level: OverlayLevel::AboveStatus,
                    input: OverlayInput::All,
                    keys: KeyPolicy::OnDemand,
                    ..Default::default()
                };
                let screen =
                    Screen { shared: Arc::downgrade(&shared), index, dim: Pixels::new(), bright: Pixels::new() };
                app.overlay(
                    OverlaySpec::new(NAMES[index], (800.0, 600.0), view).options(options).on_key(on_key),
                    screen,
                )
            })
            .collect();
        *lock(&shared.overlays) = overlays;
        let weak = Arc::downgrade(&shared);
        let _ = shared.finder.set(Finder::spawn(move || {
            if let Some(shared) = weak.upgrade() {
                shared.answered();
            }
        }));
        Screens { shared }
    }

    /// Covers each monitor with its frozen picture and waits for a pick; `done` gets it.
    pub fn open(&self, snaps: Vec<Arc<Snapshot>>, done: impl FnOnce(Option<Pick>) + Send + 'static) {
        let snaps: Vec<Arc<Snapshot>> = snaps.into_iter().take(MAX_SCREENS).collect();
        *lock(&self.shared.session) = Some(Session {
            snaps: snaps.clone(),
            stacking: HashMap::new(),
            chain: None,
            level: 0,
            wheel: 0.0,
            at: None,
            drag: None,
            done: Some(Box::new(done)),
        });
        let overlays = lock(&self.shared.overlays).clone();
        for (overlay, snap) in overlays.iter().zip(&snaps) {
            if let Some(id) = snap.display {
                overlay.set_display(DisplayTarget::Native(native(id)));
            }
            let (dim, bright) = (dimmed(&snap.image.rgba), snap.image.rgba.clone());
            let (width, height) = (snap.image.width, snap.image.height);
            overlay.update(move |s, _| {
                s.dim.set(width, height, dim);
                s.bright.set(width, height, bright);
            });
            overlay.show();
        }
        // The focused window's monitor comes first: Escape goes there.
        if let Some(first) = overlays.first() {
            first.request_keys();
        }
        // COSMIC doesn't say which window is on top; work it out from pictures of each window.
        let weak = Arc::downgrade(&self.shared);
        std::thread::spawn(move || {
            for snap in snaps {
                let Ok(pictures) = tin_core::context::window_pictures(&snap) else { continue };
                let z = snap.stacking(&pictures);
                let Some(shared) = weak.upgrade() else { return };
                if let Some(session) = lock(&shared.session).as_mut() {
                    session.stacking.insert(snap.output.clone(), z);
                }
                shared.look();
                shared.redraw();
            }
        });
    }

    /// Cancels a picking, if one is open.
    pub fn cancel(&self) {
        self.shared.finish(None);
    }
}

impl Shared {
    /// Ends the picking: hides the overlays and hands over the pick.
    fn finish(&self, pick: Option<Pick>) {
        let done = lock(&self.session).take().and_then(|mut s| s.done.take());
        for overlay in lock(&self.overlays).iter() {
            overlay.hide();
        }
        if let Some(done) = done {
            done(pick);
        }
    }

    /// Asks the element worker what's under the pointer.
    fn look(&self) {
        let guard = lock(&self.session);
        let Some(session) = guard.as_ref() else { return };
        if session.drag.is_some() {
            return;
        }
        let Some((i, point)) = session.at else { return };
        let snap = &session.snaps[i];
        let (x, y) = round(point);
        if let (Some(w), Some(finder)) =
            (snap.window_index_at(x, y, session.stacking_of(&snap.output)), self.finder.get())
        {
            finder.ask(snap.clone(), w, (x, y));
        }
    }

    /// The element worker answered: keep its answer (starting over at the default element when
    /// the pointer moved onto something else) and redraw.
    fn answered(&self) {
        let Some(chain) = self.finder.get().and_then(Finder::take) else { return };
        if let Some(session) = lock(&self.session).as_mut() {
            let innermost = |c: &Chain| (c.output.clone(), c.window, c.items.last().map(|i| i.rect));
            if session.chain.as_ref().map(innermost) != Some(innermost(&chain)) {
                session.level = 0;
            }
            session.chain = Some(chain);
        }
        self.redraw();
    }

    fn redraw(&self) {
        for overlay in lock(&self.overlays).iter() {
            overlay.update(|_, tx| tx.instant());
        }
    }
}

impl Session {
    fn stacking_of(&self, output: &str) -> Option<&[i32]> {
        self.stacking.get(output).map(Vec::as_slice)
    }

    /// What's lit under the pointer on monitor `i`: the chosen element when the worker has
    /// answered for this window, else the window itself.
    fn hovered(&self, i: usize) -> Option<(usize, Item)> {
        let (p, point) = self.at?;
        if p != i {
            return None;
        }
        let snap = &self.snaps[i];
        let (x, y) = round(point);
        let w = snap.window_index_at(x, y, self.stacking_of(&snap.output))?;
        match &self.chain {
            Some(chain) if chain.output == snap.output && chain.window == w => {
                chain.items.get(chain.index(self.level)).map(|item| (w, item.clone()))
            }
            _ => {
                let target = &snap.windows[w];
                let label = target.window.title.clone().or_else(|| target.window.app.clone()).unwrap_or_default();
                Some((w, Item { rect: target.rect, label, kind: SelectionKind::Window, node: None }))
            }
        }
    }

    /// The box and label monitor `i` shows.
    fn highlight(&self, i: usize) -> Option<(Rect, String)> {
        match self.drag {
            Some((d, rect)) => (d == i).then(|| (rect, format!("{} × {}", rect.width, rect.height))),
            None => self.hovered(i).map(|(_, item)| (item.rect, item.label)),
        }
    }

    /// The page the element worker last saw in window `w` of monitor `i`.
    fn page_of(&self, i: usize, w: usize) -> Option<tin_core::snapshot::WebPage> {
        let output = &self.snaps[i].output;
        self.chain.as_ref().filter(|c| &c.output == output && c.window == w).and_then(|c| c.page.clone())
    }

    /// What a click or a finished drag on monitor `i` picks.
    fn pick(&self, i: usize) -> Option<Pick> {
        let snap = self.snaps.get(i)?.clone();
        if let Some((d, rect)) = self.drag.filter(|(d, r)| *d == i && r.width >= 4 && r.height >= 4) {
            // The window and page the area was dragged over: under its centre.
            let (cx, cy) = (rect.x + rect.width / 2, rect.y + rect.height / 2);
            let w = snap.window_index_at(cx, cy, self.stacking_of(&snap.output));
            return Some(Pick {
                window: w.map(|w| snap.windows[w].window.clone()),
                page: w.and_then(|w| self.page_of(d, w)),
                node: None,
                snap,
                rect,
                kind: SelectionKind::Region,
            });
        }
        let (w, item) = self.hovered(i)?;
        Some(Pick {
            window: Some(snap.windows[w].window.clone()),
            page: self.page_of(i, w),
            node: item.node,
            snap,
            rect: item.rect,
            kind: item.kind,
        })
    }
}

fn pointer_move(s: &mut Screen, at: PointerPress) {
    let Some(shared) = s.shared.upgrade() else { return };
    if let Some(session) = lock(&shared.session).as_mut() {
        session.at = Some((s.index, (at.x, at.y)));
    }
    shared.look();
    shared.redraw();
}

fn drag(s: &mut Screen, d: &mut Drag, _: &mut Tx) {
    let Some(shared) = s.shared.upgrade() else { return };
    let pick = {
        let mut guard = lock(&shared.session);
        let Some(session) = guard.as_mut() else { return };
        session.at = Some((s.index, (d.x, d.y)));
        match d.phase {
            Phase::Down => {
                session.drag = None;
                None
            }
            Phase::Move => {
                if d.dx.hypot(d.dy) > CLICK_SLOP {
                    session.drag = Some((s.index, Rect::between(round((d.start_x, d.start_y)), round((d.x, d.y)))));
                }
                None
            }
            _ if d.cancelled() => {
                session.drag = None;
                None
            }
            _ => Some(session.pick(s.index)),
        }
    };
    match pick {
        Some(pick) => shared.finish(pick),
        None => shared.redraw(),
    }
}

/// Scrolling up grows the highlight to what's around it; down shrinks it.
fn wheel(s: &mut Screen, w: WheelEvent, _: &mut Tx) -> bool {
    let Some(shared) = s.shared.upgrade() else { return false };
    if let Some(session) = lock(&shared.session).as_mut() {
        session.wheel += w.dy;
        let len = session.chain.as_ref().map_or(1, |c| c.items.len()) as i32;
        while session.wheel.abs() >= WHEEL_STEP {
            let step = session.wheel.signum();
            session.level = (session.level + step as i32).clamp(-len, len);
            session.wheel -= step * WHEEL_STEP;
        }
    }
    shared.redraw();
    true
}

fn on_key(s: &mut Screen, ev: &KeyEvent, _: &mut Tx) -> bool {
    if ev.key != rune::Key::Escape {
        return false;
    }
    if let Some(shared) = s.shared.upgrade() {
        shared.finish(None);
    }
    true
}

fn view(s: &Screen, _: &mut Cx<Screen>) -> El {
    let root = el()
        .key("screen")
        // The box jumps to whatever is hovered; Rune would otherwise spring it there.
        .instant_changes()
        .w_full()
        .h_full()
        .pixels_fit(&s.dim, SurfaceFit::Fit)
        .cursor(Cursor::Crosshair)
        .on_pointer_move(pointer_move)
        .on_drag(drag)
        .on_wheel(wheel);
    let Some(shared) = s.shared.upgrade() else { return root };
    let guard = lock(&shared.session);
    let Some(session) = guard.as_ref() else { return root };
    let Some(snap) = session.snaps.get(s.index) else { return root };
    let mut root = root;
    if let Some((rect, label)) = session.highlight(s.index) {
        let (x, y, w, h) = (rect.x as f32, rect.y as f32, rect.width as f32, rect.height as f32);
        let (sw, sh) = (snap.bounds.width as f32, snap.bounds.height as f32);
        // The frozen screen at full brightness, seen through the picked box.
        root = root
            .child(
                el().key("lit")
                    .absolute(x, y)
                    .w(w)
                    .h(h)
                    .clip_rounded(0.0)
                    .child(el().key("lit-screen").absolute(-x, -y).w(sw).h(sh).pixels_fit(&s.bright, SurfaceFit::Fit)),
            )
            .child(el().key("outline").absolute(x, y).w(w).h(h).border(2.0, ACCENT))
            .child(pill("label", label, x + 6.0, if y >= 30.0 { y - 28.0 } else { y + 6.0 }));
    }
    let hint_here = session.at.map_or(s.index == 0, |(i, _)| i == s.index);
    if hint_here {
        root = root.child(row().key("hint-row").absolute(0.0, 14.0).w_full().center().child(pill(
            "hint",
            HINT.into(),
            0.0,
            0.0,
        )));
    }
    root
}

/// A dark label, as the picker's are drawn on Linux (`canvas.rs`).
fn pill(key: &'static str, label: String, x: f32, y: f32) -> El {
    let pill = row()
        .key(key)
        .h(25.0)
        .px(9.0)
        .center_y()
        .fill(Color::BLACK.alpha(0.784))
        .child(text(label).font(type_::LABEL).color(Color::WHITE));
    if x == 0.0 && y == 0.0 { pill } else { pill.absolute(x, y) }
}

/// The frozen screen at the brightness kept around what's picked.
fn dimmed(rgba: &[u8]) -> Vec<u8> {
    let keep = |c: u8| (u16::from(c) * DIM_KEEP / 255) as u8;
    rgba.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, _]| [keep(r), keep(g), keep(b), 255]).collect()
}

#[cfg(target_os = "linux")]
fn native(id: u32) -> NativeDisplayId {
    NativeDisplayId::Wayland(id)
}

#[cfg(target_os = "macos")]
fn native(id: u32) -> NativeDisplayId {
    NativeDisplayId::MacOS(id)
}

fn round((x, y): (f32, f32)) -> (i32, i32) {
    (x.round() as i32, y.round() as i32)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
