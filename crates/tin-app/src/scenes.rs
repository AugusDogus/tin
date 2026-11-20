//! Named states for Rune's headless snapshots: `tin-app --snap out.png --scene composer`.

use rune::TextEdit;
use tin_core::capture::{CapturedText, TextSource, VoiceNote};
use tin_core::context::Gathered;
use tin_core::page::{Page, PageSource};

use crate::ui::{Ui, Voice};

type Scene = (&'static str, fn(&mut Ui));

pub const SCENES: &[Scene] = &[("composer", composer), ("heard", heard), ("recording", recording)];

fn composer(s: &mut Ui) {
    s.begin(Gathered {
        text: Some(CapturedText {
            text: "The more I use AI, the more I find myself collecting little things I don't want to lose.".into(),
            from: TextSource::Selection,
        }),
        page: Some(Page::new(
            "https://x.com/shadcn/status/2082519375194763675",
            Some(r#"shadcn on X: "Launching Copper, a Mac app for capturing things" / X"#.into()),
            PageSource::BrowserTab { browser: "Helium".into(), focused: false },
        )),
        window: None,
        screenshot: None,
        notes: Vec::new(),
    });
    s.purpose = TextEdit::new("ask blog about this #pricing");
}

fn heard(s: &mut Ui) {
    composer(s);
    if let Some(c) = &mut s.composer {
        c.voice = Voice::Heard(VoiceNote {
            transcript: "Compare this with how we queue prompts in the onboarding flow.".into(),
            engine: "hex".into(),
        });
    }
}

fn recording(s: &mut Ui) {
    composer(s);
    if let Some(c) = &mut s.composer {
        c.voice = Voice::Recording;
    }
}

/// The capture card's scenes: `tin-app --picker --snap out.png --scene heard`. A made-up desktop,
/// so no real screen is read, and no voice note is recorded.
pub mod picker {
    use std::sync::Arc;
    use tin_core::capture::{VoiceNote, Window};
    use tin_core::screenshot::{Image, Rect};

    use tin_core::snapshot::{SelectionKind, SnapWindow, Snapshot};

    use crate::picker::{Pick, Picker};
    use crate::ui::Voice;

    type Scene = (&'static str, fn(&mut Picker));

    pub const SCENES: &[Scene] = &[("noting", noting), ("heard", heard), ("area", area)];

    const W: u32 = 1280;
    const H: u32 = 800;

    fn rects() -> [(&'static str, &'static str, Rect, [u8; 3]); 3] {
        [
            (
                "helium",
                "shadcn on X: Launching Copper - Helium",
                Rect { x: 40, y: 60, width: 760, height: 620 },
                [40, 44, 52],
            ),
            ("dev.zed.Zed", "tin — picker.rs", Rect { x: 640, y: 120, width: 600, height: 560 }, [30, 34, 30]),
            ("com.mitchellh.ghostty", "ghostty", Rect { x: 180, y: 420, width: 420, height: 300 }, [20, 20, 24]),
        ]
    }

    fn snapshot() -> Snapshot {
        let mut rgba = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let paint = rects()
                    .into_iter()
                    .rev()
                    .find(|(_, _, r, _)| r.contains(x, y))
                    .map(|(_, _, r, c)| if y < r.y + 28 { [c[0] + 30, c[1] + 30, c[2] + 30] } else { c });
                let [r, g, b] = paint.unwrap_or([(x / 8) as u8, 60, (y / 6) as u8]);
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        let windows = rects()
            .into_iter()
            .map(|(app, title, rect, _)| SnapWindow {
                window: Window { app: Some(app.into()), title: Some(title.into()) },
                rect,
                focused: app == "helium",
            })
            .collect();
        Snapshot {
            output: "HDMI-A-1".into(),
            bounds: Rect { x: 0, y: 0, width: W as i32, height: H as i32 },
            image: Image { width: W, height: H, rgba },
            windows,
        }
    }

    fn show(s: &mut Picker, pick_window: bool, voice: Voice) {
        let snap = Arc::new(snapshot());
        let w = snap.windows[0].clone();
        let pick = if pick_window {
            Pick { snap: snap.clone(), rect: w.rect, kind: SelectionKind::Window, window: Some(w.window), page: None }
        } else {
            Pick {
                snap: snap.clone(),
                rect: Rect { x: 700, y: 200, width: 420, height: 260 },
                kind: SelectionKind::Region,
                window: None,
                page: None,
            }
        };
        s.show(Arc::new(vec![snap]), pick, voice);
    }

    fn noting(s: &mut Picker) {
        show(s, true, Voice::Recording);
    }

    fn heard(s: &mut Picker) {
        show(
            s,
            true,
            Voice::Heard(VoiceNote {
                transcript: "This tweet is the UX I want for Tin, ask blog whether we can do the same.".into(),
                engine: "hex".into(),
            }),
        );
    }

    fn area(s: &mut Picker) {
        show(s, false, Voice::Recording);
    }
}
