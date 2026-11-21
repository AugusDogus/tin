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
    /// What a picked element holds, when its app describes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ElementContent>,
}

/// A picked element's text and links, read from its app's accessibility tree, so what was on
/// screen is kept as words and addresses, not only as a picture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementContent {
    pub text: String,
    /// In the order they appear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ElementContent {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.links.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SelectionKind {
    /// A whole window, clicked.
    Window,
    /// Something inside a window, clicked: from the app's accessibility tree (with its role and
    /// name) or found in the pixels (neither).
    Element {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// An area, dragged.
    Region,
}

impl Snapshot {
    /// Device pixels per point.
    pub fn scale(&self) -> f64 {
        if self.bounds.width > 0 { f64::from(self.image.width) / f64::from(self.bounds.width) } else { 1.0 }
    }

    /// The window under a point, by `stacking` (from [`Snapshot::stacking`]) when known: the
    /// highest window there. Without it, a guess: the focused one, else the smallest (dialogs
    /// sit over the bigger windows they belong to).
    pub fn window_at(&self, x: i32, y: i32, stacking: Option<&[i32]>) -> Option<&SnapWindow> {
        self.window_index_at(x, y, stacking).map(|i| &self.windows[i])
    }

    /// [`Snapshot::window_at`]'s index in `windows`.
    pub fn window_index_at(&self, x: i32, y: i32, stacking: Option<&[i32]>) -> Option<usize> {
        let under: Vec<usize> = (0..self.windows.len()).filter(|&i| self.windows[i].rect.contains(x, y)).collect();
        if let Some(z) = stacking.filter(|z| z.len() == self.windows.len()) {
            return under
                .iter()
                .copied()
                .max_by_key(|&i| (z[i], self.windows[i].focused, -self.windows[i].rect.area()));
        }
        under
            .iter()
            .copied()
            .find(|&i| self.windows[i].focused)
            .or_else(|| under.iter().copied().min_by_key(|&i| self.windows[i].rect.area()))
    }

    /// How high each window sits, from pictures of each window on its own (`None` where there
    /// is none): where two windows overlap, the one whose own pixels match what the monitor
    /// showed there is on top. A window's number counts the windows it's over.
    pub fn stacking(&self, pictures: &[Option<Image>]) -> Vec<i32> {
        let mut z = vec![0; self.windows.len()];
        for a in 0..self.windows.len() {
            for b in a + 1..self.windows.len() {
                let (Some(pa), Some(pb)) =
                    (pictures.get(a).and_then(Option::as_ref), pictures.get(b).and_then(Option::as_ref))
                else {
                    continue;
                };
                let Some(overlap) = self.windows[a].rect.clamp_to(self.windows[b].rect) else { continue };
                let (ma, mb) = (self.matches(a, pa, overlap), self.matches(b, pb, overlap));
                match ma.cmp(&mb) {
                    std::cmp::Ordering::Greater => z[a] += 1,
                    std::cmp::Ordering::Less => z[b] += 1,
                    std::cmp::Ordering::Equal => {}
                }
            }
        }
        z
    }

    /// At how many sample points of `area` window `i`'s own picture matches the monitor.
    fn matches(&self, i: usize, picture: &Image, area: Rect) -> usize {
        let scale = self.scale();
        let rect = self.windows[i].rect.scaled(scale);
        // A picture can include client-side shadows around the window: centre it on the window.
        let dx = ((picture.width as i32 - rect.width) / 2).max(0);
        let dy = ((picture.height as i32 - rect.height) - dx).max(0);
        let area = area.scaled(scale);
        let pixel = |image: &Image, x: i32, y: i32| -> Option<[u8; 3]> {
            if x < 0 || y < 0 || x >= image.width as i32 || y >= image.height as i32 {
                return None;
            }
            let k = (y as usize * image.width as usize + x as usize) * 4;
            image.rgba.get(k..k + 3).map(|p| [p[0], p[1], p[2]])
        };
        const GRID: i32 = 16;
        let mut count = 0;
        for gy in 0..GRID {
            for gx in 0..GRID {
                let x = area.x + (2 * gx + 1) * area.width / (2 * GRID);
                let y = area.y + (2 * gy + 1) * area.height / (2 * GRID);
                let shown = pixel(&self.image, x, y);
                let own = pixel(picture, x - rect.x + dx, y - rect.y + dy);
                if let (Some(s), Some(o)) = (shown, own)
                    && (0..3).all(|c| s[c].abs_diff(o[c]) <= 24)
                {
                    count += 1;
                }
            }
        }
        count
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
        assert_eq!(s.window_at(15, 15, None).and_then(|w| w.window.app.as_deref()), Some("dialog"));
        assert_eq!(s.window_at(50, 50, None).and_then(|w| w.window.app.as_deref()), Some("editor"));
        let s = snapshot(vec![window("editor", big, true), window("dialog", dialog, false)]);
        assert_eq!(s.window_at(15, 15, None).and_then(|w| w.window.app.as_deref()), Some("editor"));
        assert!(s.window_at(500, 500, None).is_none());
    }

    #[test]
    fn stacking_puts_the_window_whose_pixels_show_on_top() {
        // A 4×4 monitor: a big grey window everywhere, and a small one behind it (its own
        // picture is red, but the monitor shows grey over it).
        let grey = |w: u32, h: u32| Image { width: w, height: h, rgba: [90, 90, 90, 255].repeat((w * h) as usize) };
        let red = |w: u32, h: u32| Image { width: w, height: h, rgba: [200, 0, 0, 255].repeat((w * h) as usize) };
        let big = Rect { x: 0, y: 0, width: 4, height: 4 };
        let small = Rect { x: 1, y: 1, width: 2, height: 2 };
        let s = Snapshot {
            output: "DP-1".into(),
            bounds: big,
            image: grey(4, 4),
            windows: vec![window("helium", big, false), window("teams", small, true)],
        };
        let z = s.stacking(&[Some(grey(4, 4)), Some(red(2, 2))]);
        assert_eq!(z, [1, 0]);
        // With the order known, the covered (and even focused) small window loses.
        assert_eq!(s.window_at(2, 2, Some(&z)).and_then(|w| w.window.app.as_deref()), Some("helium"));
        // Without it, the guess picks the focused one.
        assert_eq!(s.window_at(2, 2, None).and_then(|w| w.window.app.as_deref()), Some("teams"));
    }

    #[test]
    fn crop_scales_points_to_pixels() {
        // 2×2 points shown at 4×4 pixels: point (1, 0) is pixels (2..4, 0..2).
        let crop = snapshot(vec![]).crop(Rect { x: 1, y: 0, width: 1, height: 1 }).unwrap_or_else(|| panic!("empty"));
        assert_eq!((crop.width, crop.height), (2, 2));
        assert_eq!(crop.rgba.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [2, 3, 6, 7]);
    }
}
