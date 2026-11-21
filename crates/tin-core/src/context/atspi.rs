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

/// AT-SPI's `ATSPI_STATE_SHOWING` bit: the element is on screen.
const STATE_SHOWING: u32 = 25;
/// AT-SPI's `ATSPI_COORD_TYPE_WINDOW`: relative to the window.
const WINDOW_COORDS: u32 = 1;
/// How long one call may take. A frozen app mustn't freeze the picker.
const CALL_TIMEOUT: Duration = Duration::from_millis(300);
/// Deepest element chain followed.
const MAX_DEPTH: usize = 48;
/// The fewest leading characters a frame's name and a window's title must share to match.
const MIN_SHARED: usize = 6;
/// The most elements one lookup asks about.
const MAX_VISITS: usize = 1500;

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
    /// For a web page (a browser's document): its address and title.
    pub page: Option<(String, Option<String>)>,
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

    /// Whether the element is on screen (AT-SPI's SHOWING state). An app that doesn't answer is
    /// taken at its word that the element exists.
    fn showing(&self, node: &Node) -> bool {
        self.call::<_, Vec<u32>>(node, "org.a11y.atspi.Accessible", "GetState", &())
            .is_none_or(|words| words.first().is_some_and(|w| w >> STATE_SHOWING & 1 == 1))
    }

    /// A document's address and title (Chromium and Firefox give `URI` and `Title`).
    fn page(&self, node: &Node) -> Option<(String, Option<String>)> {
        let attributes: std::collections::HashMap<String, String> =
            self.call(node, "org.a11y.atspi.Document", "GetAttributes", &())?;
        let uri = attributes.get("URI").or_else(|| attributes.get("DocURL")).filter(|u| !u.is_empty())?.clone();
        Some((uri, attributes.get("Title").filter(|t| !t.is_empty()).cloned()))
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
    /// The frame's extents are `None` when it doesn't report them (AccessKit apps answer -1);
    /// its elements' boxes are then taken as relative to the window's content.
    pub fn frame_for(&self, window: &Window) -> Option<(Node, Option<Rect>)> {
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
        let extents = self.extents(&frame);
        Some((frame, extents))
    }

    /// The elements under a point (window coordinates), outermost first, below `frame`. At each
    /// level [`topmost`] picks which child under the point to follow.
    pub fn chain_at(&self, frame: &Node, x: i32, y: i32) -> Vec<Element> {
        let mut budget = MAX_VISITS;
        let mut chain = Vec::new();
        let mut node = frame.clone();
        while chain.len() < MAX_DEPTH {
            let under = self.under(&node, x, y, &mut budget);
            // One step of look-ahead, not a full search: pages nest dozens of same-sized
            // wrappers, and exploring every one of them multiplies at each level.
            let has_inside: Vec<bool> =
                under.iter().map(|(child, _)| !self.under(child, x, y, &mut budget).is_empty()).collect();
            let Some((child, rect)) = topmost(&has_inside).and_then(|i| under.into_iter().nth(i)) else { break };
            let role = self.role(&child);
            let page = role.starts_with("document").then(|| self.page(&child)).flatten();
            chain.push(Element { rect, role, name: self.name(&child), page });
            node = child;
        }
        chain
    }

    /// The children of `node` that are showing and contain the point, in tree order, within
    /// the call budget. Toolkits keep hidden views in the tree (Chromium has hidden containers
    /// over the page); only what's showing can be under the pointer.
    fn under(&self, node: &Node, x: i32, y: i32, budget: &mut usize) -> Vec<(Node, Rect)> {
        let mut under = Vec::new();
        for child in self.children(node) {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            if let Some(r) = self.extents(&child).filter(|r| r.contains(x, y))
                && self.showing(&child)
            {
                under.push((child, r));
            }
        }
        under
    }
}

/// Which of the siblings under the point to follow, given whether each has a showing child under
/// the point. Later siblings are drawn over earlier ones (an open menu over the view
/// behind it), so the last one with something inside wins; Chromium's empty same-sized panels
/// beside the page are passed over. Among leaves, the last one wins.
fn topmost(has_inside: &[bool]) -> Option<usize> {
    has_inside.iter().rposition(|inside| *inside).or_else(|| has_inside.len().checked_sub(1))
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
    fn the_topmost_sibling_with_something_inside_is_followed() {
        // Chromium: an empty panel, the page, then more empty panels the same size.
        assert_eq!(topmost(&[false, true, false, false]), Some(1));
        // An open menu (last) over a view that also has elements under the point.
        assert_eq!(topmost(&[true, true]), Some(1));
        // Leaves: the last one, drawn on top.
        assert_eq!(topmost(&[false, false]), Some(1));
        assert_eq!(topmost(&[]), None);
    }

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
        let extents = extents.unwrap_or_else(|| panic!("the frame reports no extents"));
        let (x, y) = match std::env::var("TIN_A11Y_POINT")
            .ok()
            .and_then(|p| p.split_once(',').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))))
        {
            Some(point) => point,
            None => (extents.x + extents.width / 3, extents.y + extents.height / 2),
        };
        let started = std::time::Instant::now();
        let chain = a11y.chain_at(&frame, x, y);
        println!("chain at ({x}, {y}) in {:?}:", started.elapsed());
        for e in &chain {
            println!(
                "  {} {:?} {:?} {:?}",
                e.role,
                e.rect,
                e.name,
                e.page.as_ref().map(|(u, _)| u.split('/').take(3).collect::<Vec<_>>().join("/"))
            );
        }
        assert!(!chain.is_empty());
    }
}
