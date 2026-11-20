//! Tin's window: a capture panel and the list of captures, built with Rune.
//!
//! `tin-app` opens the window; `tin-app --capture` reads the screen and opens the capture panel
//! (bind it to a desktop shortcut); `tin-app --background` starts it hidden. A second run hands
//! its request to the running one.

#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod canvas;
mod double_tap;
mod ipc;
mod keys;
mod picker;
mod scenes;
#[cfg(target_os = "linux")]
mod select;
mod ui;
mod view;

use std::sync::Arc;

use rune::{App, AppSpec, WindowSpec};

use crate::ipc::{Message, Request};
use crate::picker::Picker;
use crate::ui::{Status, Ui};

const USAGE: &str = "usage: tin-app [--capture | --background]

  (no flag)     open Tin's window
  --capture     freeze the screen you're on and pick a window or area to capture
                (double-tap Shift does the same)
  --background  start without a window (for login items); shortcuts and --capture still work";

enum Launch {
    Window,
    Capture,
    Background,
    /// Rune's headless tooling (`--snap`, `--tree`, `--script`, …): no IPC, no shortcut.
    Tooling,
    /// The same, for the capture card, with made-up scenes: `tin-app --picker --snap out.png --scene heard`.
    PickerTooling(Vec<String>),
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let launch = match (args.next().as_deref(), args.next()) {
        (None, _) => Launch::Window,
        (Some("--capture"), None) => Launch::Capture,
        (Some("--background"), None) => Launch::Background,
        (Some("--picker"), _) => Launch::PickerTooling(std::env::args().skip(2).collect()),
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
        Launch::Background | Launch::Tooling | Launch::PickerTooling(_) => None,
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

/// Starts a capture: freezes the monitor and opens the picker.
type Capture = Arc<dyn Fn() + Send + Sync>;

fn run(launch: Launch) -> std::process::ExitCode {
    let spec = AppSpec {
        on_key: Some(view::on_key),
        scenes: scenes::SCENES,
        ..AppSpec::new("Tin", (440.0, 620.0), || unreachable!("App::init builds the state"), view::view)
    };
    let main = App::new(spec).keep_running().on_focus(|s: &mut Ui, focused, _| {
        if focused {
            s.reload();
        }
    });
    let main = match launch {
        Launch::Window | Launch::Tooling | Launch::PickerTooling(_) => main,
        Launch::Capture | Launch::Background => main.windowless(),
    };
    let main_handle = main.handle();
    let main = main.init({
        let me = main_handle.clone();
        move || Ui::new(me)
    });
    if let Launch::Tooling = launch {
        main.run();
        return std::process::ExitCode::SUCCESS;
    }
    // Picker tooling never runs the list window; `main` only lends its handle.

    let picker_spec = AppSpec {
        on_key: Some(picker::on_key),
        ..AppSpec::new("Tin capture", (460.0, 420.0), || unreachable!("App::init builds the state"), picker::view)
    };
    if let Launch::PickerTooling(args) = launch {
        let picker = App::new(AppSpec { scenes: scenes::picker::SCENES, ..picker_spec }).args(args);
        let me = picker.handle();
        picker.init(move || Picker::new(me, main_handle)).run();
        return std::process::ExitCode::SUCCESS;
    }
    let picker = App::new(picker_spec).windowless().keep_running().on_close(|p: &mut Picker, _| p.closed());
    let picker_handle = picker.handle();
    let picker = picker.init({
        let (me, main) = (picker_handle.clone(), main_handle.clone());
        move || Picker::new(me, main)
    });
    let capture: Capture = Arc::new(move || picker_handle.update(|p, _| p.start()));

    let (on_message, capture_message) = (main_handle.clone(), capture.clone());
    if let Err(e) = ipc::listen(move |m: Message| match m.request {
        Request::Capture => capture_message(),
        Request::Show => match m.activation_token {
            Some(token) => on_message.activate(token),
            None => on_message.open(),
        },
    }) {
        eprintln!(
            "tin-app: couldn't listen on {} ({e}); `tin-app --capture` won't reach this window.",
            ipc::socket_path().display()
        );
    }

    if let Launch::Capture = launch {
        capture();
    }
    register_shortcut(&main_handle, capture.clone());
    watch_double_shift(&main_handle, capture);
    rune::run_windows(move |_| -> Vec<rune::AnyWindowSpec> {
        vec![WindowSpec::from_app("main", main).into(), WindowSpec::from_app("picker", picker).into()]
    });
    std::process::ExitCode::SUCCESS
}

/// Double-tap Shift to capture, unless `[app] double_shift = false`.
fn watch_double_shift(handle: &rune::AppHandle<Ui>, capture: Capture) {
    let config = tin_core::config::Config::load(&tin_core::Paths::from_env().config);
    if !config.is_ok_and(|c| c.double_shift()) {
        return;
    }
    let fail = handle.clone();
    keys::watch(move || capture(), move |message| fail.update(move |s, _| s.status = Some(Status::Error(message))));
}

/// Registers `[app] shortcut` from the config, once the app has started (registration before
/// then is refused), and off the UI thread: on Wayland the desktop portal may ask the user first.
fn register_shortcut(handle: &rune::AppHandle<Ui>, capture: Capture) {
    let handle = handle.clone();
    handle.clone().update(move |_, _| {
        std::thread::spawn(move || register_now(&handle, capture));
    });
}

fn register_now(handle: &rune::AppHandle<Ui>, capture: Capture) {
    let config = tin_core::config::Config::load(&tin_core::Paths::from_env().config);
    let Some(chord) = config.ok().and_then(|c| c.app.shortcut) else { return };
    let result = chord.parse::<rune::Shortcut>().map_err(|e| e.to_string()).and_then(|shortcut| {
        handle.register_shortcut(shortcut, move |_: &mut Ui, _| capture()).map_err(|e| e.to_string())
    });
    if let Err(e) = result {
        let message = format!(
            "The shortcut {chord} didn't register ({e}). Bind `tin-app --capture` to a key in your desktop's settings instead."
        );
        handle.update(move |s, _| s.status = Some(Status::Error(message)));
    }
}
