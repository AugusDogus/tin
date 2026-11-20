//! A frozen monitor: what it showed and where each visible window was, for picking a window or a
//! region to capture.

use serde::{Deserialize, Serialize};

use crate::capture::Window;
use crate::screenshot::{Image, Rect};

/// One monitor at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The output's name, such as `HDMI-A-1`.
    pub output: String,
    /// Where the monitor sits in the desktop, in points.
    pub bounds: Rect,
    /// What it showed, in device pixels.
    pub image: Image,
    /// Windows visible on it, positioned in points from its top-left. The focused one first.
    pub windows: Vec<SnapWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapWindow {
    pub window: Window,
    pub rect: Rect,
    pub focused: bool,
}

/// What the user picked on a snapshot: stored with the capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub output: String,
    /// In points from the monitor's top-left.
    pub rect: Rect,
    pub kind: SelectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SelectionKind {
    /// A whole window, clicked.
    Window,
    /// An area, dragged.
    Region,
}

impl Snapshot {
    /// Device pixels per point.
    pub fn scale(&self) -> f64 {
        if self.bounds.width > 0 { f64::from(self.image.width) / f64::from(self.bounds.width) } else { 1.0 }
    }

    /// The window under a point: the focused one if it's there, else the smallest that contains
    /// it (dialogs and popups sit over the bigger windows they belong to).
    pub fn window_at(&self, x: i32, y: i32) -> Option<&SnapWindow> {
        let under: Vec<&SnapWindow> = self.windows.iter().filter(|w| w.rect.contains(x, y)).collect();
        under.iter().find(|w| w.focused).or_else(|| under.iter().min_by_key(|w| w.rect.area())).copied()
    }

    /// The pixels inside `rect` (points from the monitor's top-left).
    pub fn crop(&self, rect: Rect) -> Option<Image> {
        self.image.crop(rect.scaled(self.scale()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(name: &str, rect: Rect, focused: bool) -> SnapWindow {
        SnapWindow { window: Window { app: Some(name.into()), title: None }, rect, focused }
    }

    fn snapshot(windows: Vec<SnapWindow>) -> Snapshot {
        let rgba: Vec<u8> = (0..16u8).flat_map(|i| [i, 0, 0, 255]).collect();
        Snapshot {
            output: "DP-1".into(),
            bounds: Rect { x: 0, y: 0, width: 2, height: 2 },
            image: Image { width: 4, height: 4, rgba },
            windows,
        }
    }

    #[test]
    fn the_focused_window_wins_then_the_smallest() {
        let big = Rect { x: 0, y: 0, width: 100, height: 100 };
        let dialog = Rect { x: 10, y: 10, width: 20, height: 20 };
        let s = snapshot(vec![window("editor", big, false), window("dialog", dialog, false)]);
        assert_eq!(s.window_at(15, 15).and_then(|w| w.window.app.as_deref()), Some("dialog"));
        assert_eq!(s.window_at(50, 50).and_then(|w| w.window.app.as_deref()), Some("editor"));
        let s = snapshot(vec![window("editor", big, true), window("dialog", dialog, false)]);
        assert_eq!(s.window_at(15, 15).and_then(|w| w.window.app.as_deref()), Some("editor"));
        assert!(s.window_at(500, 500).is_none());
    }

    #[test]
    fn crop_scales_points_to_pixels() {
        // 2×2 points shown at 4×4 pixels: point (1, 0) is pixels (2..4, 0..2).
        let crop = snapshot(vec![]).crop(Rect { x: 1, y: 0, width: 1, height: 1 }).unwrap_or_else(|| panic!("empty"));
        assert_eq!((crop.width, crop.height), (2, 2));
        assert_eq!(crop.rgba.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [2, 3, 6, 7]);
    }
}
