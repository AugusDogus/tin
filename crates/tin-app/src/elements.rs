//! What's under the pointer inside a window, for the picker: the app's own elements from the
//! accessibility bus when it publishes them, else blocks found in the frozen frame's pixels.
//!
//! Lookups take tens of milliseconds (a chain of D-Bus calls, or cutting a window's pixels into
//! blocks the first time), so they run on a worker thread. The picker asks for every pointer
//! move; the worker answers only the latest question and then wakes the picker.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use tin_core::capture::Window;
use tin_core::context::atspi::{Accessibility, Node};
use tin_core::regions::Region;
use tin_core::screenshot::Rect;
use tin_core::snapshot::{SelectionKind, Snapshot};

/// One thing the pointer is over, from the window inward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Points from the monitor's top-left.
    pub rect: Rect,
    pub label: String,
    pub kind: SelectionKind,
}

/// Everything under a point in one window, the window first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub output: String,
    pub window: usize,
    pub items: Vec<Item>,
}

impl Chain {
    /// Which item to show: the innermost one big enough to point at, then `level` steps out
    /// (positive) or in (negative).
    pub fn index(&self, level: i32) -> usize {
        let last = self.items.len().saturating_sub(1);
        let default =
            (0..=last).rev().find(|&i| self.items[i].rect.width >= 24 && self.items[i].rect.height >= 16).unwrap_or(0);
        (default as i64 - i64::from(level)).clamp(0, last as i64) as usize
    }
}

struct Request {
    snap: Arc<Snapshot>,
    window: usize,
    point: (i32, i32),
}

pub struct Finder {
    requests: Sender<Request>,
    answer: Arc<Mutex<Option<Chain>>>,
}

impl Finder {
    /// Starts the worker; `wake` runs after each answer, on the worker's thread.
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Finder {
        let (requests, rx) = mpsc::channel();
        let answer = Arc::new(Mutex::new(None));
        let slot = answer.clone();
        std::thread::Builder::new().name("tin-elements".into()).spawn(move || work(&rx, &slot, &wake)).ok();
        Finder { requests, answer }
    }

    pub fn ask(&self, snap: Arc<Snapshot>, window: usize, point: (i32, i32)) {
        let _ = self.requests.send(Request { snap, window, point });
    }

    /// The newest answer, once.
    pub fn take(&self) -> Option<Chain> {
        self.answer.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// A window's accessibility frame, and how far its content sits inside the frame (client-side
/// shadows the compositor's window geometry leaves out).
type Frame = Option<(Node, (i32, i32))>;

fn work(rx: &Receiver<Request>, answer: &Mutex<Option<Chain>>, wake: &dyn Fn()) {
    let a11y = Accessibility::connect().ok();
    let mut frames: HashMap<(String, usize), Frame> = HashMap::new();
    let mut blocks: HashMap<(String, usize), Region> = HashMap::new();
    while let Ok(mut request) = rx.recv() {
        // Only the newest question matters.
        while let Ok(newer) = rx.try_recv() {
            request = newer;
        }
        let Some(target) = request.snap.windows.get(request.window) else { continue };
        let key = (request.snap.output.clone(), request.window);
        let win = target.rect;
        let mut items = vec![Item { rect: win, label: window_label(&target.window), kind: SelectionKind::Window }];

        if let Some(a11y) = &a11y {
            let frame = frames.entry(key.clone()).or_insert_with(|| {
                a11y.frame_for(&target.window).map(|(node, f)| {
                    let dx = ((f.width - win.width) / 2).max(0);
                    let dy = (f.height - win.height - dx).max(0);
                    (node, (dx, dy))
                })
            });
            if let Some((node, (dx, dy))) = frame {
                let (x, y) = (request.point.0 - win.x + *dx, request.point.1 - win.y + *dy);
                for e in a11y.chain_at(node, x, y) {
                    let rect = Rect { x: e.rect.x - *dx + win.x, y: e.rect.y - *dy + win.y, ..e.rect };
                    // AccessKit apps (GPUI, Rune) don't answer GetRoleName: the role is empty there.
                    let role = Some(e.role).filter(|r| !r.is_empty());
                    let label = match (&role, &e.name) {
                        (Some(role), Some(name)) => format!("{role}: {name}"),
                        (Some(only), None) | (None, Some(only)) => only.clone(),
                        (None, None) => format!("{} × {}", rect.width, rect.height),
                    };
                    push(&mut items, rect, label, SelectionKind::Element { role, name: e.name });
                }
            }
        }

        // Nothing from the app: cut the frozen pixels into blocks instead.
        if items.len() == 1 {
            let scale = request.snap.scale();
            let region = blocks.entry(key).or_insert_with(|| Region::find(&request.snap.image, win.scaled(scale)));
            let (px, py) = (f64::from(request.point.0) * scale, f64::from(request.point.1) * scale);
            for r in region.path_at(px.round() as i32, py.round() as i32) {
                let rect = r.scaled(1.0 / scale);
                let label = format!("{} × {}", rect.width, rect.height);
                push(&mut items, rect, label, SelectionKind::Element { role: None, name: None });
            }
        }

        let chain = Chain { output: request.snap.output.clone(), window: request.window, items };
        *answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(chain);
        wake();
    }
}

/// Adds an item inside the window, skipping repeats of the one before (toolkits nest
/// same-sized panels) and anything outside the window.
fn push(items: &mut Vec<Item>, rect: Rect, label: String, kind: SelectionKind) {
    let Some(window) = items.first().map(|i| i.rect) else { return };
    let Some(rect) = rect.clamp_to(window) else { return };
    if items.last().is_some_and(|last| last.rect == rect) {
        // Keep the more specific description of the same box.
        if let Some(last) = items.last_mut()
            && matches!(last.kind, SelectionKind::Element { .. })
        {
            last.label = label;
            last.kind = kind;
        }
        return;
    }
    items.push(Item { rect, label, kind });
}

fn window_label(w: &Window) -> String {
    w.title.clone().or_else(|| w.app.clone()).unwrap_or_else(|| "Window".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(w: i32, h: i32) -> Item {
        Item { rect: Rect { x: 0, y: 0, width: w, height: h }, label: String::new(), kind: SelectionKind::Window }
    }

    #[test]
    fn the_default_is_the_innermost_pointable_item_and_levels_step_from_it() {
        let chain = Chain { output: "o".into(), window: 0, items: vec![item(800, 600), item(300, 80), item(10, 8)] };
        assert_eq!(chain.index(0), 1, "the 10×8 item is too small to be the default");
        assert_eq!(chain.index(1), 0);
        assert_eq!(chain.index(5), 0);
        assert_eq!(chain.index(-1), 2);
        assert_eq!(chain.index(-9), 2);
    }

    #[test]
    fn push_skips_repeats_and_clamps_to_the_window() {
        let mut items = vec![item(100, 100)];
        let el = |role: &str| SelectionKind::Element { role: Some(role.into()), name: None };
        push(&mut items, Rect { x: 0, y: 0, width: 100, height: 100 }, "panel".into(), el("panel"));
        assert_eq!(items.len(), 1, "the same box as the window adds nothing");
        push(&mut items, Rect { x: 50, y: 50, width: 100, height: 100 }, "panel".into(), el("panel"));
        push(&mut items, Rect { x: 50, y: 50, width: 100, height: 100 }, "button".into(), el("button"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].rect, Rect { x: 50, y: 50, width: 50, height: 50 });
        assert_eq!(items[1].label, "button");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// With the Tin probe page open in a Chromium started with `--force-renderer-accessibility`:
    /// `cargo test -p tin-app -- --ignored finder_live --nocapture`. The page's big button sits at
    /// (63, 285) and is 300 × 81 inside the window frame.
    #[test]
    #[ignore = "needs a COSMIC session with the probe page open"]
    fn finder_live() {
        let snaps = tin_core::context::snapshots().unwrap_or_else(|e| panic!("{e}"));
        let (snap, w) = snaps
            .into_iter()
            .find_map(|s| {
                let i = s
                    .windows
                    .iter()
                    .position(|w| w.window.title.as_deref().is_some_and(|t| t.contains("Tin probe")))?;
                Some((Arc::new(s), i))
            })
            .unwrap_or_else(|| panic!("no Tin probe window"));
        let win = snap.windows[w].rect;
        println!("window {win:?} on {}", snap.output);
        let (tx, rx) = mpsc::channel();
        let finder = Finder::spawn(move || {
            let _ = tx.send(());
        });
        // The frame's content starts (10, 13) in on Chromium; the button's centre is (213, 325).
        finder.ask(snap.clone(), w, (win.x + 213 - 10, win.y + 325 - 13));
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_else(|e| panic!("{e}"));
        let chain = finder.take().unwrap_or_else(|| panic!("no answer"));
        for item in &chain.items {
            println!("  {:?} {}", item.rect, item.label);
        }
        assert!(chain.items.last().is_some_and(|i| i.label.contains("Big button")));
    }
}
