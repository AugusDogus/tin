//! Reads the active tab from a Chromium session file (`Sessions/Session_*`, the SNSS format).
//!
//! The file is a log of commands: `u16 size, u8 id, payload[size - 1]`. Tin replays the few it
//! needs: each tab's navigations, its selected navigation, when it was last active, and whether it
//! closed. The tab with the newest last-active time is the one in front.

use std::collections::{HashMap, HashSet};

use super::Tab;

const UPDATE_TAB_NAVIGATION: u8 = 6;
const SET_SELECTED_NAVIGATION_INDEX: u8 = 7;
const TAB_CLOSED: u8 = 16;
const LAST_ACTIVE_TIME: u8 = 21;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnssError {
    #[error("not a Chromium session file (missing the SNSS header)")]
    NotSnss,
    #[error("the session file has no tab navigations yet")]
    NoNavigations,
}

/// The tab in front, from a whole session file's bytes: the one whose title starts the focused
/// window's title when that's known (each window shows its own tab, so with several windows the
/// last tab activated may be in another one), else the tab activated last.
pub fn active_tab(data: &[u8], window_title: Option<&str>) -> Result<Tab, SnssError> {
    if data.get(..4) != Some(b"SNSS".as_slice()) {
        return Err(SnssError::NotSnss);
    }
    let mut navigations: HashMap<(i32, i32), Tab> = HashMap::new();
    let mut latest_navigation: HashMap<i32, i32> = HashMap::new();
    let mut selected: HashMap<i32, i32> = HashMap::new();
    let mut last_active: HashMap<i32, i64> = HashMap::new();
    let mut closed: HashSet<i32> = HashSet::new();
    let mut last_written: Option<(i32, i32)> = None;

    for (id, payload) in Commands::new(&data[8.min(data.len())..]) {
        let mut r = Reader::new(payload);
        match id {
            UPDATE_TAB_NAVIGATION => {
                // A Pickle: u32 payload size, then the fields.
                let parsed = (|| {
                    r.skip(4)?;
                    let (tab, index) = (r.i32()?, r.i32()?);
                    let url = r.string()?;
                    let title = r.string16()?;
                    Some((tab, index, url, title))
                })();
                if let Some((tab, index, url, title)) = parsed {
                    navigations.insert((tab, index), Tab { url, title: Some(title).filter(|t| !t.is_empty()) });
                    latest_navigation.insert(tab, index);
                    last_written = Some((tab, index));
                    closed.remove(&tab);
                }
            }
            SET_SELECTED_NAVIGATION_INDEX => {
                if let (Some(tab), Some(index)) = (r.i32(), r.i32()) {
                    selected.insert(tab, index);
                }
            }
            LAST_ACTIVE_TIME => {
                // struct { SessionID tab_id; (4 bytes padding) int64 time; }
                if let Some(tab) = r.i32()
                    && r.skip(4).is_some()
                    && let Some(time) = r.i64()
                {
                    last_active.insert(tab, time);
                }
            }
            TAB_CLOSED => {
                if let Some(tab) = r.i32() {
                    closed.insert(tab);
                }
            }
            _ => {}
        }
    }

    let tab_navigation = |tab: i32| {
        selected
            .get(&tab)
            .and_then(|i| navigations.get(&(tab, *i)))
            .or_else(|| latest_navigation.get(&tab).and_then(|i| navigations.get(&(tab, *i))))
    };
    let open: Vec<(i64, &Tab)> = latest_navigation
        .keys()
        .filter(|tab| !closed.contains(tab))
        .filter_map(|tab| tab_navigation(*tab).map(|nav| (last_active.get(tab).copied().unwrap_or(i64::MIN), nav)))
        .collect();
    let titled = window_title.and_then(|window| {
        open.iter()
            .filter(|(_, nav)| nav.title.as_deref().is_some_and(|t| super::titles_window(t, window)))
            .max_by_key(|(time, nav)| (nav.title.as_ref().map_or(0, String::len), *time))
            .map(|(_, nav)| *nav)
    });
    let front = titled.or_else(|| {
        open.iter().filter(|(time, _)| *time != i64::MIN).max_by_key(|(time, _)| *time).map(|(_, nav)| *nav)
    });
    front.or_else(|| last_written.and_then(|key| navigations.get(&key))).cloned().ok_or(SnssError::NoNavigations)
}

struct Commands<'a> {
    rest: &'a [u8],
}

impl<'a> Commands<'a> {
    fn new(rest: &'a [u8]) -> Self {
        Commands { rest }
    }
}

impl<'a> Iterator for Commands<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let size = usize::from(u16::from_le_bytes([*self.rest.first()?, *self.rest.get(1)?]));
        // A zero size or a record past the end is a torn write at the tail: stop there.
        let record = self.rest.get(2..2 + size).filter(|r| !r.is_empty())?;
        self.rest = &self.rest[2 + size..];
        Some((record[0], &record[1..]))
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(bytes)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    fn i32(&mut self) -> Option<i32> {
        self.take(4)?.try_into().ok().map(i32::from_le_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.take(8)?.try_into().ok().map(i64::from_le_bytes)
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.i32()?).ok()
    }

    /// Pickle strings pad their bytes to a multiple of four.
    fn padded(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.take(n)?;
        self.skip((4 - n % 4) % 4)?;
        Some(bytes)
    }

    fn string(&mut self) -> Option<String> {
        let n = self.len()?;
        Some(String::from_utf8_lossy(self.padded(n)?).into_owned())
    }

    fn string16(&mut self) -> Option<String> {
        let n = self.len()?;
        let units: Vec<u16> =
            self.padded(n.checked_mul(2)?)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        Some(String::from_utf16_lossy(&units))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds SNSS bytes from commands, the way Chromium writes them.
    pub struct Snss(Vec<u8>);

    impl Snss {
        pub fn new() -> Self {
            Snss(b"SNSS\x03\0\0\0".to_vec())
        }

        fn command(mut self, id: u8, payload: &[u8]) -> Self {
            let size = u16::try_from(payload.len() + 1).unwrap_or(u16::MAX);
            self.0.extend(size.to_le_bytes());
            self.0.push(id);
            self.0.extend(payload);
            self
        }

        pub fn navigation(self, tab: i32, index: i32, url: &str, title: &str) -> Self {
            let mut body = Vec::new();
            body.extend(tab.to_le_bytes());
            body.extend(index.to_le_bytes());
            body.extend(i32::try_from(url.len()).unwrap_or(0).to_le_bytes());
            body.extend(url.as_bytes());
            body.resize(body.len().next_multiple_of(4), 0);
            let units: Vec<u16> = title.encode_utf16().collect();
            body.extend(i32::try_from(units.len()).unwrap_or(0).to_le_bytes());
            body.extend(units.iter().flat_map(|u| u.to_le_bytes()));
            body.resize(body.len().next_multiple_of(4), 0);
            let mut payload = u32::try_from(body.len()).unwrap_or(0).to_le_bytes().to_vec();
            payload.extend(body);
            self.command(UPDATE_TAB_NAVIGATION, &payload)
        }

        pub fn selected(self, tab: i32, index: i32) -> Self {
            let payload: Vec<u8> = tab.to_le_bytes().into_iter().chain(index.to_le_bytes()).collect();
            self.command(SET_SELECTED_NAVIGATION_INDEX, &payload)
        }

        pub fn active(self, tab: i32, time: i64) -> Self {
            let payload: Vec<u8> = tab.to_le_bytes().into_iter().chain([0; 4]).chain(time.to_le_bytes()).collect();
            self.command(LAST_ACTIVE_TIME, &payload)
        }

        pub fn closed(self, tab: i32) -> Self {
            let payload: Vec<u8> = tab.to_le_bytes().into_iter().chain([0; 4]).chain(0i64.to_le_bytes()).collect();
            self.command(TAB_CLOSED, &payload)
        }

        pub fn bytes(&self) -> &[u8] {
            &self.0
        }
    }

    fn tab(url: &str, title: &str) -> Tab {
        Tab { url: url.into(), title: Some(title.into()) }
    }

    #[test]
    fn the_most_recently_active_tab_wins_at_its_selected_navigation() {
        let snss = Snss::new()
            .navigation(1, 0, "https://a.com/", "A")
            .navigation(1, 1, "https://a.com/next", "A next")
            .navigation(2, 0, "https://b.com/", "B — ünïcode")
            .selected(1, 0)
            .active(2, 100)
            .active(1, 200);
        assert_eq!(active_tab(snss.bytes(), None), Ok(tab("https://a.com/", "A")));
    }

    #[test]
    fn the_focused_windows_tab_wins_over_one_activated_later_elsewhere() {
        // Two windows: Twitch in one, X activated after it in the other. The focused window is
        // titled after Twitch.
        let snss = Snss::new()
            .navigation(1, 0, "https://twitch.tv/ludwig", "Ludwig - Twitch")
            .navigation(2, 0, "https://x.com/home", "(1) Home / X")
            .active(1, 100)
            .active(2, 200);
        assert_eq!(
            active_tab(snss.bytes(), Some("Ludwig - Twitch - Helium")),
            Ok(tab("https://twitch.tv/ludwig", "Ludwig - Twitch"))
        );
        assert_eq!(
            active_tab(snss.bytes(), Some("Something else - Helium")),
            Ok(tab("https://x.com/home", "(1) Home / X"))
        );
    }

    #[test]
    fn a_closed_tab_is_skipped() {
        let snss = Snss::new()
            .navigation(1, 0, "https://a.com/", "A")
            .navigation(2, 0, "https://b.com/", "B")
            .active(1, 100)
            .active(2, 200)
            .closed(2);
        assert_eq!(active_tab(snss.bytes(), None), Ok(tab("https://a.com/", "A")));
    }

    #[test]
    fn without_active_times_the_last_navigation_written_wins() {
        let snss = Snss::new().navigation(1, 0, "https://a.com/", "A").navigation(2, 0, "https://b.com/", "B");
        assert_eq!(active_tab(snss.bytes(), None), Ok(tab("https://b.com/", "B")));
    }

    #[test]
    fn a_torn_tail_is_ignored_and_garbage_is_refused() {
        let mut bytes = Snss::new().navigation(1, 0, "https://a.com/", "A").bytes().to_vec();
        bytes.extend([0xff, 0x00, 6, 1, 2]);
        assert_eq!(active_tab(&bytes, None), Ok(tab("https://a.com/", "A")));
        assert_eq!(active_tab(b"nope", None), Err(SnssError::NotSnss));
        assert_eq!(active_tab(Snss::new().bytes(), None), Err(SnssError::NoNavigations));
    }
}
