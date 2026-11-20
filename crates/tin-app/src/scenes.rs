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
