//! Draws Tin's one window: the capture panel when a capture is in progress, then the list.

use rune::tok::{self, Theme, type_};
use rune::{Color, Cursor, Cx, El, Key, KeyEvent, ScrollSpec, Tx, col, el, row, spacer, text, text_input};
use tin_core::capture::{Capture, TextSource};
use tin_core::intent::{Intent, Target};
use tin_core::page::{Page, PageKind, PageSource};

use crate::ui::{Composer, Filter, Status, Ui, Voice, describe_target};

struct Look {
    t: Theme,
    accent: Color,
    danger: Color,
}

fn look(dark: bool) -> Look {
    Look { t: tok::theme(dark), accent: tok::accent(4, dark), danger: Color { r: 0.86, g: 0.3, b: 0.28, a: 1.0 } }
}

pub fn view(s: &Ui, cx: &mut Cx<Ui>) -> El {
    let l = look(cx.system_dark());
    let mut root = col().w_full().h_full().padding(14.0).gap(10.0).fill(l.t.bg).child(header(s, &l));
    if let Err(e) = &s.tin {
        root = root.child(text(e.clone()).font(type_::BODY).color(l.danger).wrap());
    }
    if let Some(c) = &s.composer {
        root = root.child(composer(s, c, &l));
    }
    root.child(filters(s, &l)).child(list(s, &l)).child(quick_add(s, &l)).child(status_line(s, &l))
}

/// Window keys nothing focused took: copy and move the selection, Escape to close the panel.
pub fn on_key(s: &mut Ui, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match ev.key {
        _ if ev.is_command('c') && tx.focused().is_none() => s.copy_selected(tx),
        Key::Escape if s.composer.is_some() => {
            s.discard(tx);
            true
        }
        Key::Down if tx.focused().is_none() => {
            s.step_selection(1);
            true
        }
        Key::Up if tx.focused().is_none() => {
            s.step_selection(-1);
            true
        }
        _ => false,
    }
}

fn header(s: &Ui, l: &Look) -> El {
    let open = s.captures.iter().filter(|c| !c.is_done()).count();
    row()
        .w_full()
        .h(28.0)
        .gap(8.0)
        .center_y()
        .child(text("Tin").font(type_::TITLE.lh(20.0)).color(l.t.text))
        .child(text(format!("{open} open")).font(type_::LABEL).color(l.t.text_secondary))
        .child(spacer())
        .child(
            button("Capture clipboard", false, l)
                .key("capture-clipboard")
                .help("Start a capture from what you copied")
                .on_press(|s: &mut Ui| s.start_capture(tin_core::context::TextGrab::Clipboard, None)),
        )
}

fn composer(s: &Ui, c: &Composer, l: &Look) -> El {
    let source = match c.text_from {
        Some(TextSource::Selection) => "from your selection",
        Some(TextSource::Clipboard) => "from the clipboard",
        _ => "",
    };
    let body = text_input(&s.draft_text, type_::BODY.lh(19.0))
        .key("draft-text")
        .label("Captured text")
        .w_full()
        .min_h(56.0)
        .max_h(160.0)
        .padding(8.0)
        .radius(6.0)
        .fill(l.t.bg)
        .color(l.t.text)
        .caret(l.accent, l.accent.alpha(0.3))
        .placeholder("The text to keep. Type or paste it here.", l.t.text_secondary)
        .on_edit(|s: &mut Ui| &mut s.draft_text)
        .on_submit(|s: &mut Ui, tx| s.save(tx));
    let purpose = text_input(&s.purpose, type_::BODY.lh(20.0))
        .key("purpose")
        .label("What it's for")
        .single_line()
        .w_full()
        .padding(8.0)
        .radius(6.0)
        .fill(l.t.bg)
        .color(l.t.text)
        .caret(l.accent, l.accent.alpha(0.3))
        .placeholder("What's it for? the blog, game dev, new project: …", l.t.text_secondary)
        .on_edit(|s: &mut Ui| &mut s.purpose)
        .on_submit(|s: &mut Ui, tx| s.save(tx));

    let mut card = col()
        .key("composer")
        .w_full()
        .padding(12.0)
        .gap(8.0)
        .radius(10.0)
        .fill(l.t.card)
        .border(1.0, l.t.card_border)
        .child(
            row()
                .gap(6.0)
                .center_y()
                .child(text("New capture").font(type_::TITLE).color(l.t.text))
                .child(text(source).font(type_::LABEL).color(l.t.text_secondary)),
        )
        .child(body);
    if let Some(page) = &c.page {
        card = card.child(page_chip(page, l));
    }
    card = card.child(purpose);
    if let Some(intent) = s.preview_intent() {
        card = card.child(text(route_preview(&intent)).key("route-preview").font(type_::LABEL).color(
            match intent.target {
                Target::Undecided => l.t.text_secondary,
                _ => l.accent,
            },
        ));
    }
    card = card.child(voice_row(&c.voice, l));
    for note in &c.notes {
        card = card.child(text(note.clone()).font(type_::LABEL).color(l.t.text_secondary).wrap());
    }
    card.child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(text("Enter saves · Esc discards").font(type_::LABEL).color(l.t.text_secondary))
            .child(spacer())
            .child(button("Discard", false, l).key("discard").on_press_tx(|s: &mut Ui, tx| s.discard(tx)))
            .child(button("Save", true, l).key("save").on_press_tx(|s: &mut Ui, tx| s.save(tx))),
    )
}

fn page_chip(page: &Page, l: &Look) -> El {
    let (badge, label) = match &page.kind {
        PageKind::Tweet { handle, text: Some(t), .. } => ("Tweet", format!("@{handle}: {t}")),
        PageKind::Tweet { handle, text: None, .. } => ("Tweet", format!("@{handle} · {}", page.url)),
        PageKind::Link => {
            ("Link", page.title.clone().map_or_else(|| page.url.clone(), |t| format!("{t} · {}", page.host())))
        }
    };
    let guessed = matches!(page.via, PageSource::BrowserTab { focused: false, .. });
    let mut chip = row()
        .key("page")
        .w_full()
        .gap(8.0)
        .px(8.0)
        .h(30.0)
        .center_y()
        .radius(6.0)
        .fill(l.t.control_track)
        .child(text(badge).font(type_::LABEL).color(l.accent))
        .child(text(label).font(type_::LABEL).color(l.t.text).truncate().grow(1.0).shrink().min_w(0.0));
    if guessed {
        chip = chip
            .help("Tin couldn't see the focused window, so this is your browser's active tab. Drop it if it's wrong.")
            .child(text("guessed").font(type_::LABEL).color(l.t.text_secondary));
    }
    chip.child(icon_button("✕", l).key("drop-page").help("Don't attach this page").on_press(|s: &mut Ui| s.drop_page()))
}

fn voice_row(voice: &Voice, l: &Look) -> El {
    let (label, primary) = match voice {
        Voice::Idle | Voice::Failed(_) => ("🎙 Voice note", false),
        Voice::Heard(_) => ("🎙 Record again", false),
        Voice::Starting => ("Starting…", false),
        Voice::Recording => ("■ Stop", true),
        Voice::Transcribing => ("Transcribing…", false),
    };
    let mut r = row().w_full().gap(8.0).center_y().child(
        button(label, primary, l)
            .key("voice")
            .help("Record a voice note with Hex")
            .on_press(|s: &mut Ui| s.toggle_voice()),
    );
    r = match voice {
        Voice::Recording => r.child(text("Recording with Hex…").font(type_::LABEL).color(l.danger)),
        Voice::Heard(note) => r
            .child(
                text(format!("“{}”", note.transcript))
                    .key("transcript")
                    .font(type_::LABEL)
                    .color(l.t.text)
                    .wrap()
                    .grow(1.0)
                    .shrink()
                    .min_w(0.0),
            )
            .child(
                icon_button("✕", l).key("drop-voice").help("Drop the voice note").on_press(|s: &mut Ui| s.drop_voice()),
            ),
        Voice::Failed(e) => {
            r.child(text(e.clone()).font(type_::LABEL).color(l.danger).wrap().grow(1.0).shrink().min_w(0.0))
        }
        _ => r,
    };
    r
}

fn route_preview(intent: &Intent) -> String {
    let tags: Vec<String> = intent.tags.iter().map(|t| format!("#{t}")).collect();
    let target = match &intent.target {
        Target::Undecided if tags.is_empty() => return "Not routed yet: an agent can sort it later.".into(),
        Target::Undecided => "Tagged".to_owned(),
        Target::Project { path: Some(path), .. } => {
            format!("→ {} ({})", describe_target(&intent.target), path.display())
        }
        _ => format!("→ {}", describe_target(&intent.target)),
    };
    format!("{target}  {}", tags.join(" "))
}

fn filters(s: &Ui, l: &Look) -> El {
    let chip = |label: String, on: bool, key: String, f: Filter| {
        el().key(key)
            .h(24.0)
            .px(10.0)
            .radius(12.0)
            .center()
            .cursor(Cursor::Pointer)
            .fill(if on { l.accent.alpha(0.2) } else { l.t.control_track })
            .hover(l.t.hover)
            .child(text(label).font(type_::LABEL).color(if on { l.t.text } else { l.t.text_secondary }))
            .on_press(move |s: &mut Ui| s.filter = f.clone())
    };
    let mut r = row()
        .w_full()
        .gap(6.0)
        .flex_wrap()
        .child(chip("Open".into(), s.filter == Filter::Open, "filter-open".into(), Filter::Open))
        .child(chip("Done".into(), s.filter == Filter::Done, "filter-done".into(), Filter::Done));
    for tag in s.open_tags() {
        let f = Filter::Tag(tag.clone());
        r = r.child(chip(format!("#{tag}"), s.filter == f, format!("filter-{tag}"), f));
    }
    r
}

fn list(s: &Ui, l: &Look) -> El {
    let visible = s.visible();
    let mut items = col().key("list").w_full().gap(4.0);
    if visible.is_empty() {
        let hint = match s.filter {
            Filter::Done => "Nothing checked off yet.",
            _ => "Nothing captured yet. Select something anywhere and run `tin-app --capture` (bind it to a shortcut).",
        };
        items = items.child(text(hint).font(type_::BODY).color(l.t.text_secondary).wrap().padding(8.0));
    }
    for c in visible {
        items = items.child(capture_row(c, s.selected.as_ref() == Some(&c.id), l));
    }
    items.scroll(ScrollSpec { stick_bottom: false }).grow(1.0).min_h(0.0)
}

fn capture_row(c: &Capture, selected: bool, l: &Look) -> El {
    let id = c.id.clone();
    let check_id = id.clone();
    let copy_id = id.clone();
    let check = el()
        .key("check")
        .size(16.0, 16.0)
        .radius(8.0)
        .cursor(Cursor::Pointer)
        .border(1.5, if c.is_done() { l.accent } else { l.t.text_secondary })
        .fill(if c.is_done() { l.accent } else { Color::TRANSPARENT })
        .help(if c.is_done() { "Open it again" } else { "Check it off" })
        .on_press(move |s: &mut Ui| s.toggle_done(&check_id));

    let mut meta = row().gap(6.0).center_y();
    if let Some(name) = c.intent.target.name() {
        meta = meta.child(text(name.to_owned()).font(type_::LABEL).color(l.accent));
    }
    for tag in c.intent.tags.iter().filter(|t| c.intent.target.name().is_none_or(|n| tin_core::tag_of(n) != **t)) {
        meta = meta.child(text(format!("#{tag}")).font(type_::LABEL).color(l.t.text_secondary));
    }
    if let Some(p) = &c.page {
        let where_ = match &p.kind {
            PageKind::Tweet { handle, .. } => format!("tweet @{handle}"),
            PageKind::Link => p.host().to_owned(),
        };
        meta = meta.child(text(where_).font(type_::LABEL).color(l.t.text_secondary));
    }
    if c.voice.is_some() {
        meta = meta.child(text("🎙").font(type_::LABEL).color(l.t.text_secondary));
    }
    let headline = text(c.headline()).font(type_::BODY.lh(18.0)).max_lines(2).wrap().color(if c.is_done() {
        l.t.text_secondary
    } else {
        l.t.text
    });
    row()
        .key(format!("capture-{id}"))
        .w_full()
        .px(10.0)
        .py(8.0)
        .gap(10.0)
        .radius(8.0)
        .items_start()
        .fill(if selected { l.accent.alpha(0.14) } else { l.t.card })
        .hover(l.t.hover)
        .child(el().pt(1.0).child(check))
        .child(col().gap(3.0).grow(1.0).shrink().min_w(0.0).child(headline).child(meta))
        .help("Double-click (or select and ⌘/Ctrl+C) to copy")
        .on_press(move |s: &mut Ui| s.selected = Some(id.clone()))
        .on_double_press_tx(move |s: &mut Ui, tx| s.copy(&copy_id, tx))
}

fn quick_add(s: &Ui, l: &Look) -> El {
    text_input(&s.quick, type_::BODY.lh(20.0))
        .key("quick")
        .label("Add a note")
        .single_line()
        .w_full()
        .padding(8.0)
        .radius(8.0)
        .fill(l.t.card)
        .border(1.0, l.t.card_border)
        .color(l.t.text)
        .caret(l.accent, l.accent.alpha(0.3))
        .placeholder("Add a note or the next prompt…", l.t.text_secondary)
        .on_edit(|s: &mut Ui| &mut s.quick)
        .on_submit(|s: &mut Ui, _| s.add_quick())
}

fn status_line(s: &Ui, l: &Look) -> El {
    match &s.status {
        Some(Status::Info(m)) => text(m.clone()).key("status").font(type_::LABEL).color(l.t.text_secondary),
        Some(Status::Error(m)) => text(m.clone()).key("status").font(type_::LABEL).color(l.danger).wrap(),
        None => el().h(0.0),
    }
}

fn button(label: &str, primary: bool, l: &Look) -> El {
    el().h(28.0)
        .px(12.0)
        .radius(6.0)
        .center()
        .cursor(Cursor::Pointer)
        .focusable(true)
        .fill(if primary { l.accent } else { l.t.control_track })
        .hover(l.t.hover)
        .child(text(label.to_owned()).font(type_::LABEL).color(if primary { Color::WHITE } else { l.t.text }))
}

fn icon_button(glyph: &str, l: &Look) -> El {
    el().size(22.0, 22.0)
        .radius(5.0)
        .center()
        .cursor(Cursor::Pointer)
        .hover(l.t.hover)
        .child(text(glyph.to_owned()).font(type_::LABEL).color(l.t.text_secondary))
}
