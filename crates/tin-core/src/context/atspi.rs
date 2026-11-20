//! UI elements inside other apps' windows, from the Linux accessibility bus (AT-SPI).
//!
//! Apps that publish their UI there (GTK, Qt, Firefox; Chromium and Electron apps started with
//! `--force-renderer-accessibility`) describe each element's role, name and position in its
//! window. Wayland apps don't know where their window is on screen, so positions are relative to
//! the window, and the caller places them using the compositor's window geometry.

use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::capture::Window;
use crate::screenshot::Rect;

/// AT-SPI's `ATSPI_COORD_TYPE_WINDOW`: relative to the window.
const WINDOW_COORDS: u32 = 1;
/// How long one call may take. A frozen app mustn't freeze the picker.
const CALL_TIMEOUT: Duration = Duration::from_millis(300);
/// Deepest element chain followed.
const MAX_DEPTH: usize = 48;
/// The fewest leading characters a frame's name and a window's title must share to match.
const MIN_SHARED: usize = 6;
/// The most elements one lookup asks about.
const MAX_VISITS: usize = 600;

/// An object on the accessibility bus.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Node {
    bus: String,
    path: OwnedObjectPath,
}

/// One element under a point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Relative to the window, in points.
    pub rect: Rect,
    pub role: String,
    pub name: Option<String>,
}

/// A connection to the accessibility bus.
pub struct Accessibility {
    conn: Connection,
}

/// Switches the session's accessibility bus on, so toolkits publish their UI.
pub fn enable() -> Result<(), String> {
    let session = Connection::session().map_err(|e| format!("couldn't reach the session bus: {e}"))?;
    session
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &("org.a11y.Status", "IsEnabled", Value::from(true)),
        )
        .map(|_| ())
        .map_err(|e| format!("couldn't switch accessibility on: {e}"))
}

impl Accessibility {
    pub fn connect() -> Result<Accessibility, String> {
        let fail =
            |what: &str, e: &dyn std::fmt::Display| format!("couldn't reach the accessibility bus ({what}: {e})");
        let session = Connection::session().map_err(|e| fail("session bus", &e))?;
        let reply = session
            .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
            .map_err(|e| fail("address", &e))?;
        let address: String = reply.body().deserialize().map_err(|e| fail("address", &e))?;
        let conn = zbus::blocking::connection::Builder::address(address.as_str())
            .map_err(|e| fail("address", &e))?
            .method_timeout(CALL_TIMEOUT)
            .build()
            .map_err(|e| fail("connect", &e))?;
        Ok(Accessibility { conn })
    }

    fn call<B, R>(&self, node: &Node, interface: &str, method: &str, body: &B) -> Option<R>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let reply =
            self.conn.call_method(Some(node.bus.as_str()), node.path.as_str(), Some(interface), method, body).ok()?;
        reply.body().deserialize::<R>().ok()
    }

    fn children(&self, node: &Node) -> Vec<Node> {
        self.call::<_, Vec<(String, OwnedObjectPath)>>(node, "org.a11y.atspi.Accessible", "GetChildren", &())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, path)| !path.as_str().ends_with("/null"))
            .map(|(bus, path)| Node { bus, path })
            .collect()
    }

    fn name(&self, node: &Node) -> Option<String> {
        let value: OwnedValue =
            self.call(node, "org.freedesktop.DBus.Properties", "Get", &("org.a11y.atspi.Accessible", "Name"))?;
        String::try_from(value).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    }

    fn role(&self, node: &Node) -> String {
        self.call::<_, String>(node, "org.a11y.atspi.Accessible", "GetRoleName", &()).unwrap_or_default()
    }

    fn extents(&self, node: &Node) -> Option<Rect> {
        let (x, y, width, height): (i32, i32, i32, i32) =
            self.call(node, "org.a11y.atspi.Component", "GetExtents", &(WINDOW_COORDS,))?;
        (width > 0 && height > 0).then_some(Rect { x, y, width, height })
    }

    /// The frame (top-level accessible) named like the window's title, and its extents. Names
    /// and titles can differ in what follows the page's name (Chromium puts the profile in the
    /// window title but not the frame's name), so the frame sharing the longest start with the
    /// title wins, if they share at least [`MIN_SHARED`] characters.
    pub fn frame_for(&self, window: &Window) -> Option<(Node, Rect)> {
        let title = normalize(window.title.as_deref()?);
        let registry = Node {
            bus: "org.a11y.atspi.Registry".into(),
            path: OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").ok()?,
        };
        let mut best: Option<(usize, Node)> = None;
        for frame in self.children(&registry).into_iter().flat_map(|app| self.children(&app)) {
            let Some(name) = self.name(&frame).map(|n| normalize(&n)) else { continue };
            let shared = if name == title { usize::MAX } else { shared_start(&name, &title) };
            if shared >= MIN_SHARED && best.as_ref().is_none_or(|(b, _)| shared > *b) {
                best = Some((shared, frame));
            }
        }
        let (_, frame) = best?;
        self.extents(&frame).map(|r| (frame, r))
    }

    /// The elements under a point (window coordinates), outermost first, below `frame`.
    /// Toolkits stack same-sized panels (Chromium has empty ones beside the page), so each
    /// child under the point is tried and the deepest chain wins, within a budget of calls.
    pub fn chain_at(&self, frame: &Node, x: i32, y: i32) -> Vec<Element> {
        let mut budget = MAX_VISITS;
        self.descend(frame, x, y, 0, &mut budget)
    }

    fn descend(&self, node: &Node, x: i32, y: i32, depth: usize, budget: &mut usize) -> Vec<Element> {
        if depth >= MAX_DEPTH || *budget == 0 {
            return Vec::new();
        }
        let mut under: Vec<(Node, Rect)> = Vec::new();
        for child in self.children(node) {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            if let Some(r) = self.extents(&child).filter(|r| r.contains(x, y)) {
                under.push((child, r));
            }
        }
        under.sort_by_key(|(_, r)| r.area());
        let mut best: Vec<Element> = Vec::new();
        for (child, rect) in under {
            let below = self.descend(&child, x, y, depth + 1, budget);
            if best.is_empty() || below.len() + 1 > best.len() {
                let mut chain = vec![Element { rect, role: self.role(&child), name: self.name(&child) }];
                chain.extend(below);
                best = chain;
            }
        }
        best
    }
}

/// How many leading characters two strings share.
fn shared_start(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// A title without invisible characters (COSMIC shows Edge as "Microsoft\u{200b} Edge") and with
/// runs of spaces collapsed, for comparing a window's title with its frame's name.
fn normalize(title: &str) -> String {
    let visible: String =
        title.chars().filter(|c| !matches!(c, '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}')).collect();
    visible.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_titles_by_their_shared_start() {
        assert_eq!(shared_start("Tin probe - Microsoft Edge", "Tin probe - Profile 1 - Microsoft Edge"), 12);
        assert_eq!(shared_start("abc", "xyz"), 0);
    }

    #[test]
    fn titles_compare_without_invisible_characters() {
        assert_eq!(normalize(" Page -  Microsoft\u{200b} Edge "), "Page - Microsoft Edge");
    }

    /// Needs a running session: `TIN_A11Y_TITLE='<window title>' cargo test -p tin-core -- --ignored atspi_live --nocapture`.
    #[test]
    #[ignore = "needs a desktop session with an accessible app open"]
    fn atspi_live_chain_in_a_window() {
        let title = std::env::var("TIN_A11Y_TITLE").unwrap_or_default();
        let a11y = Accessibility::connect().unwrap_or_else(|e| panic!("{e}"));
        let window = Window { app: None, title: Some(title.clone()) };
        let started = std::time::Instant::now();
        let (frame, extents) = a11y.frame_for(&window).unwrap_or_else(|| panic!("no frame titled {title:?}"));
        println!("frame {extents:?} found in {:?}", started.elapsed());
        let (x, y) = (extents.x + extents.width / 3, extents.y + extents.height / 2);
        let started = std::time::Instant::now();
        let chain = a11y.chain_at(&frame, x, y);
        println!("chain at ({x}, {y}) in {:?}:", started.elapsed());
        for e in &chain {
            println!("  {} {:?} {:?}", e.role, e.rect, e.name);
        }
        assert!(!chain.is_empty());
    }
}
