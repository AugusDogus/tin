//! Watches the keyboard system-wide for the double Shift tap, and for Escape (the picker's way
//! out where its overlays can't take the keyboard).
//!
//! Linux reads keyboards from `/dev/input` (evdev), which works the same on X11 and every Wayland
//! desktop but needs the `input` group. Keyboards plugged in later are picked up by a rescan.
//! macOS uses a Quartz event tap (rdev), which needs Accessibility permission. Only Shift presses
//! presses, Escape and "some other key" leave the listener; nothing typed is kept.

/// What the listener heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    DoubleShift,
    Escape,
}

#[cfg(target_os = "linux")]
pub use linux::watch;
#[cfg(target_os = "macos")]
pub use macos::watch;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use evdev::{Device, EventSummary, KeyCode};

    use super::Heard;
    use crate::double_tap::{DoubleTap, Press, Side};

    const RESCAN: Duration = Duration::from_secs(3);

    type Watched = Arc<Mutex<HashSet<PathBuf>>>;

    /// Calls `heard` on every double Shift tap and Escape, or `fail` once if no keyboard can be
    /// read.
    pub fn watch(heard: impl Fn(Heard) + Send + 'static, fail: impl Fn(String) + Send + 'static) {
        let (tx, rx) = mpsc::channel::<Press>();
        let watched: Watched = Arc::default();
        std::thread::spawn(move || {
            let mut reported = false;
            loop {
                let scan = scan(&tx, &watched);
                let none = watched.lock().unwrap_or_else(PoisonError::into_inner).is_empty();
                if none && !reported {
                    reported = true;
                    fail(match scan.denied {
                        0 => "Double Shift is off: Tin found no keyboard in /dev/input.".to_owned(),
                        n => format!(
                            "Double Shift is off: Tin can't read the keyboard ({n} input devices refused access). \
                             Add yourself to the `input` group (`sudo usermod -aG input $USER`, then log in again), \
                             or bind `tin-app --capture` to a shortcut instead."
                        ),
                    });
                }
                std::thread::sleep(RESCAN);
            }
        });
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut tap = DoubleTap::default();
            for press in rx {
                if press == Press::Escape {
                    heard(Heard::Escape);
                }
                if tap.feed(press, start.elapsed()) {
                    heard(Heard::DoubleShift);
                }
            }
        });
    }

    struct Scan {
        denied: usize,
    }

    /// Opens keyboards not already watched and starts a reader for each.
    fn scan(tx: &Sender<Press>, watched: &Watched) -> Scan {
        let mut denied = 0;
        let paths = std::fs::read_dir("/dev/input").into_iter().flatten().flatten().map(|e| e.path());
        for path in paths.filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event"))) {
            if watched.lock().unwrap_or_else(PoisonError::into_inner).contains(&path) {
                continue;
            }
            let device = match Device::open(&path) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    denied += 1;
                    continue;
                }
                Err(_) => continue,
            };
            let keyboard = device
                .supported_keys()
                .is_some_and(|k| k.contains(KeyCode::KEY_LEFTSHIFT) && k.contains(KeyCode::KEY_A));
            if keyboard {
                watched.lock().unwrap_or_else(PoisonError::into_inner).insert(path.clone());
                let (tx, watched) = (tx.clone(), watched.clone());
                std::thread::spawn(move || {
                    read(device, &tx);
                    // Unplugged (or the detector stopped): let a rescan open it again.
                    watched.lock().unwrap_or_else(PoisonError::into_inner).remove(&path);
                });
            }
        }
        Scan { denied }
    }

    fn read(mut device: Device, tx: &Sender<Press>) {
        while let Ok(events) = device.fetch_events() {
            for event in events {
                // 1 is a press, 0 a release, 2 a key repeat (ignored).
                let EventSummary::Key(_, code, value @ (0 | 1)) = event.destructure() else { continue };
                let press = match (code, value) {
                    (KeyCode::KEY_LEFTSHIFT, 1) => Press::ShiftDown(Side::Left),
                    (KeyCode::KEY_LEFTSHIFT, _) => Press::ShiftUp(Side::Left),
                    (KeyCode::KEY_RIGHTSHIFT, 1) => Press::ShiftDown(Side::Right),
                    (KeyCode::KEY_RIGHTSHIFT, _) => Press::ShiftUp(Side::Right),
                    (KeyCode::KEY_ESC, 1) => Press::Escape,
                    (_, 1) => Press::Other,
                    _ => continue,
                };
                if tx.send(press).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::Instant;

    use rdev::{EventType, Key};

    use super::Heard;
    use crate::double_tap::{DoubleTap, Press, Side};

    /// Calls `heard` on every double Shift tap and Escape, or `fail` if macOS refuses the event tap.
    pub fn watch(heard: impl Fn(Heard) + Send + 'static, fail: impl Fn(String) + Send + 'static) {
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut tap = DoubleTap::default();
            let result = rdev::listen(move |event| {
                let press = match event.event_type {
                    EventType::KeyPress(Key::ShiftLeft) => Press::ShiftDown(Side::Left),
                    EventType::KeyRelease(Key::ShiftLeft) => Press::ShiftUp(Side::Left),
                    EventType::KeyPress(Key::ShiftRight) => Press::ShiftDown(Side::Right),
                    EventType::KeyRelease(Key::ShiftRight) => Press::ShiftUp(Side::Right),
                    EventType::KeyPress(Key::Escape) => Press::Escape,
                    EventType::KeyPress(_) => Press::Other,
                    _ => return,
                };
                if press == Press::Escape {
                    heard(Heard::Escape);
                }
                if tap.feed(press, start.elapsed()) {
                    heard(Heard::DoubleShift);
                }
            });
            if let Err(e) = result {
                fail(format!(
                    "Double Shift is off: macOS refused Tin's keyboard listener ({e:?}). Give Tin Accessibility \
                     permission in System Settings › Privacy & Security, then restart it."
                ));
            }
        });
    }
}
