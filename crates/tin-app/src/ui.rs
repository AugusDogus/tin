//! The app's state and what its controls do. `view.rs` draws it.

use rune::{AppHandle, TextEdit, Tx};
use tin_core::capture::{Capture, CaptureId, CapturedText, Draft, TextSource, VoiceNote, Window};
use tin_core::context::{self, Gathered, Options, TextGrab};
use tin_core::intent::{Intent, Target};
use tin_core::page::Page;
use tin_core::{Paths, Tin, tag_of};

pub struct Ui {
    pub me: AppHandle<Ui>,
    pub tin: Result<Tin, String>,
    pub captures: Vec<Capture>,
    pub filter: Filter,
    pub selected: Option<CaptureId>,
    /// The capture being made, if any. Its text fields live beside it (`draft_text`,
    /// `purpose`) because a field binds to a `TextEdit` the state always has.
    pub composer: Option<Composer>,
    pub draft_text: TextEdit,
    pub purpose: TextEdit,
    /// "Add a note or prompt…" at the bottom: the scratchpad and prompt queue.
    pub quick: TextEdit,
    pub status: Option<Status>,
}

/// The capture panel's state.
pub struct Composer {
    pub text_from: Option<TextSource>,
    pub page: Option<Page>,
    pub window: Option<Window>,
    pub screenshot: Option<tin_core::screenshot::Screenshot>,
    pub voice: Voice,
    /// Why parts of the context are missing.
    pub notes: Vec<String>,
}

/// The voice note's recording, as Hex reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Voice {
    Idle,
    Starting,
    Recording,
    Transcribing,
    Heard(VoiceNote),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    Open,
    Done,
    /// Open captures routed or tagged to this tag.
    Tag(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Info(String),
    Error(String),
}

impl Ui {
    pub fn new(me: AppHandle<Ui>) -> Ui {
        let mut ui = Ui {
            me,
            tin: Tin::open(Paths::from_env()).map_err(|e| e.to_string()),
            captures: Vec::new(),
            filter: Filter::Open,
            selected: None,
            composer: None,
            draft_text: TextEdit::new(""),
            purpose: TextEdit::new(""),
            quick: TextEdit::new(""),
            status: None,
        };
        ui.reload();
        ui
    }

    /// Reads the captures again; agents and the CLI change them too.
    pub fn reload(&mut self) {
        let Ok(tin) = &self.tin else { return };
        match tin.store.list() {
            Ok(listing) => {
                self.captures = listing.captures;
                if let Some(problem) = listing.unreadable.first() {
                    self.status = Some(Status::Error(problem.to_string()));
                }
            }
            Err(e) => self.status = Some(Status::Error(e.to_string())),
        }
    }

    pub fn visible(&self) -> Vec<&Capture> {
        self.captures
            .iter()
            .filter(|c| match &self.filter {
                Filter::Open => !c.is_done(),
                Filter::Done => c.is_done(),
                Filter::Tag(tag) => !c.is_done() && tags_of(c).any(|t| t == *tag),
            })
            .collect()
    }

    /// Tags of open captures, most used first, for the filter chips.
    pub fn open_tags(&self) -> Vec<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for tag in self.captures.iter().filter(|c| !c.is_done()).flat_map(tags_of) {
            match counts.iter_mut().find(|(t, _)| *t == tag) {
                Some((_, n)) => *n += 1,
                None => counts.push((tag, 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        counts.into_iter().map(|(t, _)| t).take(6).collect()
    }

    /// Reads the screen off the UI thread, then opens the panel with what it found.
    pub fn start_capture(&self, grab: TextGrab, activation_token: Option<String>) {
        let me = self.me.clone();
        std::thread::spawn(move || {
            let gathered = context::gather(Options { text: grab, page: true, screenshot: true });
            me.update(move |s, _| s.begin(gathered));
            match activation_token {
                Some(token) => me.activate(token),
                None => me.open(),
            }
        });
    }

    pub fn begin(&mut self, gathered: Gathered) {
        self.cancel_recording();
        let Gathered { text, window, page, screenshot, notes } = gathered;
        self.draft_text = TextEdit::new(text.as_ref().map_or("", |t| t.text.as_str()));
        self.purpose = TextEdit::new("");
        self.purpose.focused = true;
        self.composer = Some(Composer {
            text_from: text.map(|t| t.from),
            page,
            window,
            screenshot,
            voice: Voice::Idle,
            notes: notes.into_iter().filter(|n| n.part != context::Part::Window).map(|n| n.message).collect(),
        });
        self.status = None;
    }

    /// What the purpose routes to right now, for the panel's preview.
    pub fn preview_intent(&self) -> Option<Intent> {
        let tin = self.tin.as_ref().ok()?;
        let composer = self.composer.as_ref()?;
        let heard = match &composer.voice {
            Voice::Heard(note) => Some(note.transcript.as_str()),
            _ => None,
        };
        Some(tin.routes.resolve(Some(self.purpose.text.as_str()), heard))
    }

    pub fn save(&mut self, tx: &mut Tx) {
        let Some(composer) = self.composer.as_ref() else { return };
        if matches!(composer.voice, Voice::Starting | Voice::Recording | Voice::Transcribing) {
            self.status = Some(Status::Info("Finish the voice note first.".into()));
            return;
        }
        let Ok(tin) = &self.tin else { return };
        let typed = self.draft_text.text.trim().to_owned();
        let text = (!typed.is_empty())
            .then(|| CapturedText { text: typed, from: composer.text_from.unwrap_or(TextSource::Typed) });
        let draft = Draft {
            text,
            page: composer.page.clone(),
            window: composer.window.clone(),
            voice: match &composer.voice {
                Voice::Heard(note) => Some(note.clone()),
                _ => None,
            },
            note: None,
            screenshot: composer.screenshot.clone(),
            intent: Intent { said: Some(self.purpose.text.clone()), ..Intent::default() },
        };
        match tin.save(draft) {
            Ok(saved) => {
                let errors: Vec<String> = saved.hook_errors.iter().map(ToString::to_string).collect();
                self.status = Some(match errors.first() {
                    Some(e) => Status::Error(e.clone()),
                    None => Status::Info(format!("Saved {}", describe_target(&saved.capture.intent.target))),
                });
                self.selected = Some(saved.capture.id.clone());
                self.composer = None;
                self.reload();
                tx.close_window();
            }
            Err(e) => self.status = Some(Status::Error(e.to_string())),
        }
    }

    pub fn discard(&mut self, tx: &mut Tx) {
        self.cancel_recording();
        self.composer = None;
        tx.close_window();
    }

    pub fn drop_page(&mut self) {
        if let Some(c) = &mut self.composer {
            c.page = None;
        }
    }

    pub fn drop_voice(&mut self) {
        if let Some(c) = &mut self.composer {
            c.voice = Voice::Idle;
        }
    }

    /// The microphone button: start, or stop and transcribe.
    pub fn toggle_voice(&mut self) {
        let (Some(composer), Ok(tin)) = (self.composer.as_mut(), &self.tin) else { return };
        let hex = tin.hex();
        let me = self.me.clone();
        match composer.voice {
            Voice::Idle | Voice::Failed(_) | Voice::Heard(_) => {
                composer.voice = Voice::Starting;
                std::thread::spawn(move || {
                    let next = match hex.start() {
                        Ok(()) => Voice::Recording,
                        Err(e) => Voice::Failed(e.to_string()),
                    };
                    me.update(move |s, _| s.set_voice_from(&Voice::Starting, next));
                });
            }
            Voice::Recording => {
                composer.voice = Voice::Transcribing;
                std::thread::spawn(move || {
                    let next = match hex.stop() {
                        Ok(note) => Voice::Heard(note),
                        Err(e) => Voice::Failed(e.to_string()),
                    };
                    me.update(move |s, _| s.set_voice_from(&Voice::Transcribing, next));
                });
            }
            Voice::Starting | Voice::Transcribing => {}
        }
    }

    /// Applies a worker's result only if the recording is still where the worker left it (the
    /// panel may have been discarded or reopened meanwhile).
    fn set_voice_from(&mut self, expected: &Voice, next: Voice) {
        match &mut self.composer {
            Some(c) if c.voice == *expected => c.voice = next,
            // The panel went away while Hex started: don't leave it recording.
            _ if next == Voice::Recording => self.cancel_hex(),
            _ => {}
        }
    }

    fn cancel_recording(&mut self) {
        if let Some(c) = &self.composer
            && matches!(c.voice, Voice::Starting | Voice::Recording)
        {
            self.cancel_hex();
        }
    }

    fn cancel_hex(&self) {
        if let Ok(tin) = &self.tin {
            let hex = tin.hex();
            std::thread::spawn(move || {
                let _ = hex.cancel();
            });
        }
    }

    pub fn add_quick(&mut self) {
        let text = self.quick.text.trim().to_owned();
        let Ok(tin) = &self.tin else { return };
        if text.is_empty() {
            return;
        }
        let draft = Draft { text: Some(CapturedText { text, from: TextSource::Typed }), ..Draft::default() };
        match tin.save(draft) {
            Ok(_) => {
                self.quick.clear();
                self.reload();
            }
            Err(e) => self.status = Some(Status::Error(e.to_string())),
        }
    }

    pub fn toggle_done(&mut self, id: &CaptureId) {
        let Ok(tin) = &self.tin else { return };
        let done = self.captures.iter().find(|c| c.id == *id).is_some_and(|c| !c.is_done());
        if let Err(e) = tin.set_done(id, done) {
            self.status = Some(Status::Error(e.to_string()));
        }
        self.reload();
    }

    pub fn copy(&mut self, id: &CaptureId, tx: &mut Tx) {
        if let Some(c) = self.captures.iter().find(|c| c.id == *id) {
            tx.copy(c.clipboard_text());
            self.status = Some(Status::Info("Copied".into()));
        }
    }

    pub fn copy_selected(&mut self, tx: &mut Tx) -> bool {
        match self.selected.clone() {
            Some(id) => {
                self.copy(&id, tx);
                true
            }
            None => false,
        }
    }

    /// Moves the selection through the visible list.
    pub fn step_selection(&mut self, by: isize) {
        let ids: Vec<CaptureId> = self.visible().iter().map(|c| c.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let at = self.selected.as_ref().and_then(|s| ids.iter().position(|i| i == s));
        let next = match at {
            Some(i) => i.saturating_add_signed(by).min(ids.len() - 1),
            None => 0,
        };
        self.selected = ids.get(next).cloned();
    }
}

/// A capture's route and tags as filter tags.
fn tags_of(c: &Capture) -> impl Iterator<Item = String> + '_ {
    c.intent.target.name().map(tag_of).into_iter().chain(c.intent.tags.iter().cloned())
}

pub fn describe_target(target: &Target) -> String {
    match target {
        Target::Undecided => "for later".into(),
        Target::Project { name, .. } => format!("for {name}"),
        Target::NewProject { name: Some(name) } => format!("for a new project, “{name}”"),
        Target::NewProject { name: None } => "for a new project".into(),
    }
}
