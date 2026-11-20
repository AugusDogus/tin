//! Linux: the primary selection through `wl-paste` or `xclip`/`xsel`, and the focused window
//! through the compositor's own tool (Hyprland, Sway, niri, KDE) or `xdotool` on X11.

use std::env;

use serde_json::Value;

use super::shell::{self, Ran};
use crate::capture::Window;

fn wayland() -> bool {
    env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn selection() -> Result<Option<String>, String> {
    paste(true)
}

pub fn clipboard() -> Result<Option<String>, String> {
    paste(false)
}

fn paste(primary: bool) -> Result<Option<String>, String> {
    let attempts: Vec<(&str, Vec<&str>)> = if wayland() {
        let mut args = vec!["--no-newline", "--type", "text"];
        if primary {
            args.push("--primary");
        }
        vec![("wl-paste", args)]
    } else {
        let selection = if primary { "primary" } else { "clipboard" };
        vec![
            ("xclip", vec!["-o", "-selection", selection]),
            ("xsel", vec!["--output", if primary { "--primary" } else { "--clipboard" }]),
        ]
    };
    let mut missing = Vec::new();
    for (program, args) in attempts {
        if !shell::exists(program) {
            missing.push(program);
            continue;
        }
        return Ok(match shell::run(program, &args)? {
            Ran::Ok(text) => Some(text),
            // Nothing selected (or nothing textual): not an error.
            Ran::Failed(_) => None,
        });
    }
    Err(format!(
        "Tin couldn't read the {} because {} isn't installed; install it to capture selected text.",
        if primary { "selection" } else { "clipboard" },
        missing.join(" or ")
    ))
}

pub fn copy(text: &str) -> Result<(), String> {
    let (program, args): (&str, &[&str]) =
        if wayland() { ("wl-copy", &[]) } else { ("xclip", &["-selection", "clipboard"]) };
    match shell::run_with_input(program, args, Some(text))? {
        Ran::Ok(_) => Ok(()),
        Ran::Failed(err) => Err(format!("`{program}` couldn't copy: {err}")),
    }
}

/// Linux browsers have no scripting interface; the session file is read instead.
pub fn browser_tab(_app: &str) -> Result<Option<(String, crate::browser::Tab)>, String> {
    Ok(None)
}

pub fn focused(want_screenshot: impl FnOnce(&Window) -> bool) -> Result<super::Focus, String> {
    if super::cosmic::available() {
        return super::cosmic::focused(want_screenshot);
    }
    // Only COSMIC's window capture is wired up so far.
    focused_window().map(|window| super::Focus { window, screenshot: None })
}

pub fn window_pictures(snap: &crate::snapshot::Snapshot) -> Result<Vec<Option<crate::screenshot::Image>>, String> {
    if super::cosmic::available() {
        return super::cosmic::window_pictures(snap);
    }
    Err("Working out which window is on top needs COSMIC for now.".into())
}

pub fn snapshots() -> Result<Vec<crate::snapshot::Snapshot>, String> {
    if super::cosmic::available() {
        return super::cosmic::snapshots();
    }
    Err("Picking a window or region needs COSMIC for now.".into())
}

fn focused_window() -> Result<Window, String> {
    let var = |k: &str| env::var_os(k).is_some();
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if var("HYPRLAND_INSTANCE_SIGNATURE") {
        let v = json("hyprctl", &["activewindow", "-j"])?;
        return window(&v["class"], &v["title"]);
    }
    if var("SWAYSOCK") {
        let tree = json("swaymsg", &["-t", "get_tree"])?;
        let node = focused_node(&tree).ok_or("Sway reports no focused window")?;
        let app = if node["app_id"].is_string() { &node["app_id"] } else { &node["window_properties"]["class"] };
        return window(app, &node["name"]);
    }
    if var("NIRI_SOCKET") {
        let v = json("niri", &["msg", "--json", "focused-window"])?;
        return window(&v["app_id"], &v["title"]);
    }
    if desktop.to_uppercase().contains("KDE") && shell::exists("kdotool") {
        let class = output("kdotool", &["getactivewindow", "getwindowclassname"])?;
        let title = output("kdotool", &["getactivewindow", "getwindowname"])?;
        return window(&Value::String(class), &Value::String(title));
    }
    if !wayland() && var("DISPLAY") {
        let class = output("xdotool", &["getactivewindow", "getwindowclassname"])?;
        let title = output("xdotool", &["getactivewindow", "getwindowname"])?;
        return window(&Value::String(class), &Value::String(title));
    }
    let desktop = if desktop.is_empty() { "This Wayland desktop".to_owned() } else { desktop };
    Err(format!(
        "{desktop} doesn't tell other apps which window is focused, so the capture has no window. \
         (COSMIC, Hyprland, Sway, niri, KDE with kdotool, and X11 do.)"
    ))
}

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    match shell::run(program, args)? {
        Ran::Ok(out) => Ok(out.trim().to_owned()),
        Ran::Failed(err) => Err(format!("`{program}` couldn't find the focused window: {err}")),
    }
}

fn json(program: &str, args: &[&str]) -> Result<Value, String> {
    let out = output(program, args)?;
    serde_json::from_str(&out).map_err(|e| format!("`{program}` printed something that isn't JSON: {e}"))
}

fn window(app: &Value, title: &Value) -> Result<Window, String> {
    let s = |v: &Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let w = Window { app: s(app), title: s(title) };
    if w.app.is_none() && w.title.is_none() { Err("no window is focused".into()) } else { Ok(w) }
}

fn focused_node(node: &Value) -> Option<&Value> {
    if node["focused"] == Value::Bool(true) && node["type"] != "workspace" && node["type"] != "output" {
        return Some(node);
    }
    ["nodes", "floating_nodes"].iter().filter_map(|k| node[k].as_array()).flatten().find_map(focused_node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sways_focused_window_in_floating_nodes_too() {
        let tree = serde_json::json!({"type": "root", "nodes": [{"type": "output", "nodes": [
            {"type": "workspace", "focused": false, "nodes": [{"type": "con", "focused": false, "app_id": "a"}],
             "floating_nodes": [{"type": "floating_con", "focused": true, "app_id": null,
                                 "window_properties": {"class": "Firefox"}, "name": "Page"}]}
        ]}]});
        let node = focused_node(&tree).unwrap_or_else(|| panic!("no focused node"));
        assert_eq!(node["name"], "Page");
    }

    #[test]
    fn an_empty_window_is_an_error() {
        assert!(window(&Value::Null, &Value::String(" ".into())).is_err());
        assert_eq!(
            window(&Value::String("ghostty".into()), &Value::Null),
            Ok(Window { app: Some("ghostty".into()), title: None })
        );
    }
}
