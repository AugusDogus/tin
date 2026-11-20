//! Tin's window: a capture panel and the list of captures, built with Rune.
//!
//! `tin-app` opens the window; `tin-app --capture` reads the screen and opens the capture panel
//! (bind it to a desktop shortcut); `tin-app --background` starts it hidden. A second run hands
//! its request to the running one.

#![forbid(unsafe_code)]

mod ipc;
mod scenes;
mod ui;
mod view;

use rune::{App, AppSpec};
use tin_core::context::TextGrab;

use crate::ipc::{Message, Request};
use crate::ui::{Status, Ui};

const USAGE: &str = "usage: tin-app [--capture | --background]

  (no flag)     open Tin's window
  --capture     read the selection and the page you're on, then open the capture panel;
                bind this to a desktop shortcut
  --background  start without a window (for login items); shortcuts and --capture still work";

enum Launch {
    Window,
    Capture,
    Background,
    /// Rune's headless tooling (`--snap`, `--tree`, `--script`, …): no IPC, no shortcut.
    Tooling,
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let launch = match (args.next().as_deref(), args.next()) {
        (None, _) => Launch::Window,
        (Some("--capture"), None) => Launch::Capture,
        (Some("--background"), None) => Launch::Background,
        (Some(flag), _) if flag.starts_with("--") && !matches!(flag, "--capture" | "--background" | "--help") => {
            Launch::Tooling
        }
        (Some("-h" | "--help"), None) => {
            println!("{USAGE}");
            return std::process::ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    // The launcher's activation token lets Wayland focus whichever window ends up opening.
    let activation_token = ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"].iter().find_map(|k| std::env::var(k).ok());
    let request = match launch {
        Launch::Capture => Some(Request::Capture),
        Launch::Window => Some(Request::Show),
        Launch::Background | Launch::Tooling => None,
    };
    if let Some(request) = request {
        match ipc::send(&Message { request, activation_token: activation_token.clone() }) {
            Ok(true) => return std::process::ExitCode::SUCCESS,
            Ok(false) => {}
            Err(e) => eprintln!("tin-app: couldn't reach the running Tin ({e}); starting a new one."),
        }
    }
    run(launch)
}

fn run(launch: Launch) -> std::process::ExitCode {
    let spec = AppSpec {
        on_key: Some(view::on_key),
        scenes: scenes::SCENES,
        ..AppSpec::new("Tin", (440.0, 620.0), || unreachable!("App::init builds the state"), view::view)
    };
    let app = App::new(spec).keep_running().on_focus(|s: &mut Ui, focused, _| {
        if focused {
            s.reload();
        }
    });
    let app = match launch {
        Launch::Window | Launch::Tooling => app,
        Launch::Capture | Launch::Background => app.windowless(),
    };
    let handle = app.handle();
    let me = handle.clone();
    let app = app.init(move || Ui::new(me));
    if let Launch::Tooling = launch {
        app.run();
        return std::process::ExitCode::SUCCESS;
    }

    let on_message = handle.clone();
    if let Err(e) = ipc::listen(move |m: Message| {
        let token = m.activation_token;
        match m.request {
            Request::Capture => on_message.update(move |s, _| s.start_capture(TextGrab::Selection, token)),
            Request::Show => match token {
                Some(token) => on_message.activate(token),
                None => on_message.open(),
            },
        }
    }) {
        eprintln!(
            "tin-app: couldn't listen on {} ({e}); `tin-app --capture` won't reach this window.",
            ipc::socket_path().display()
        );
    }

    if let Launch::Capture = launch {
        handle.update(|s, _| s.start_capture(TextGrab::Selection, None));
    }
    register_shortcut(&handle);
    app.run();
    std::process::ExitCode::SUCCESS
}

/// Registers `[app] shortcut` from the config, once the app has started (registration before
/// then is refused), and off the UI thread: on Wayland the desktop portal may ask the user first.
fn register_shortcut(handle: &rune::AppHandle<Ui>) {
    let handle = handle.clone();
    handle.clone().update(move |_, _| {
        std::thread::spawn(move || register_now(&handle));
    });
}

fn register_now(handle: &rune::AppHandle<Ui>) {
    let config = tin_core::config::Config::load(&tin_core::Paths::from_env().config);
    let Some(chord) = config.ok().and_then(|c| c.app.shortcut) else { return };
    let result = chord.parse::<rune::Shortcut>().map_err(|e| e.to_string()).and_then(|shortcut| {
        handle
            .register_shortcut(shortcut, |s: &mut Ui, tx| {
                s.start_capture(TextGrab::Selection, tx.activation_token().map(str::to_owned))
            })
            .map_err(|e| e.to_string())
    });
    if let Err(e) = result {
        let message = format!(
            "The shortcut {chord} didn't register ({e}). Bind `tin-app --capture` to a key in your desktop's settings instead."
        );
        handle.update(move |s, _| s.status = Some(Status::Error(message)));
    }
}
