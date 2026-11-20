//! COSMIC: the focused window, from the compositor's own toplevel protocols.
//!
//! `ext_foreign_toplevel_list_v1` names every window (title, app id); COSMIC's
//! `zcosmic_toplevel_info_v1` (v2+) adds each one's state, including "activated". COSMIC offers
//! both to ordinary clients, so no portal or permission is involved.

use std::time::{Duration, Instant};

use cosmic_protocols::toplevel_info::v1::client::{zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};

use crate::capture::Window;

const ACTIVATED: u32 = 2;
/// The longest Tin waits for COSMIC to report window states.
const STATE_WAIT: Duration = Duration::from_millis(400);

#[derive(Default)]
struct State {
    list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    toplevels: Vec<Toplevel>,
}

struct Toplevel {
    handle: ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    title: Option<String>,
    app_id: Option<String>,
    /// None until COSMIC reports the window's state.
    activated: Option<bool>,
}

/// True when this looks like a COSMIC session, worth asking.
pub fn available() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("COSMIC"))
}

pub fn focused_window() -> Result<Window, String> {
    let fail =
        |what: &str, e: &dyn std::fmt::Display| format!("couldn't ask COSMIC for the focused window ({what}: {e})");
    let conn = Connection::connect_to_env().map_err(|e| fail("connect", &e))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State::default();
    // Globals, then the toplevels and their titles.
    queue.roundtrip(&mut state).map_err(|e| fail("registry", &e))?;
    queue.roundtrip(&mut state).map_err(|e| fail("toplevels", &e))?;
    let info = state.info.clone().ok_or("COSMIC didn't offer zcosmic_toplevel_info_v1 version 2 or later")?;
    for t in &mut state.toplevels {
        t.cosmic = Some(info.get_cosmic_toplevel(&t.handle, &qh, ()));
    }
    // COSMIC sends each handle's state on its next refresh, not in reply to the request, so
    // poll briefly until every window has reported (or one says it's focused).
    let deadline = Instant::now() + STATE_WAIT;
    loop {
        queue.roundtrip(&mut state).map_err(|e| fail("states", &e))?;
        let all = state.toplevels.iter().all(|t| t.activated.is_some());
        if all || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let focused = state.toplevels.iter().find(|t| t.activated == Some(true)).ok_or("no window is focused")?;
    let window = Window { app: focused.app_id.clone(), title: focused.title.clone() };
    for t in &state.toplevels {
        if let Some(c) = &t.cosmic {
            c.destroy();
        }
        t.handle.destroy();
    }
    if let Some(list) = &state.list {
        list.stop();
    }
    let _ = conn.flush();
    Ok(window)
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else { return };
        match interface.as_str() {
            "ext_foreign_toplevel_list_v1" => state.list = Some(registry.bind(name, 1, qh, ())),
            "zcosmic_toplevel_info_v1" if version >= 2 => {
                state.info = Some(registry.bind(name, version.min(3), qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                handle: toplevel,
                cosmic: None,
                title: None,
                app_id: None,
                activated: None,
            });
        }
    }

    event_created_child!(State, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(t) = state.toplevels.iter_mut().find(|t| t.handle.id() == handle.id()) else { return };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => t.title = Some(title),
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => t.app_id = Some(app_id),
            _ => {}
        }
    }
}

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        _: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    // Only version 1 sends this (Tin binds 2 or later), but the type must say what it creates.
    event_created_child!(State, zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()),
    ]);
}

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_toplevel_handle_v1::Event::State { state: bytes } = event
            && let Some(t) =
                state.toplevels.iter_mut().find(|t| t.cosmic.as_ref().is_some_and(|c| c.id() == handle.id()))
        {
            t.activated =
                Some(bytes.chunks_exact(4).any(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) == ACTIVATED));
        }
    }
}
