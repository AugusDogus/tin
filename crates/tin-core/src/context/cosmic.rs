//! COSMIC: the focused window, the windows on each monitor, and pictures of either, from the
//! compositor's own protocols.
//!
//! `ext_foreign_toplevel_list_v1` names every window; COSMIC's `zcosmic_toplevel_info_v1` (v2+)
//! adds its state (activated, minimized), its position on each output and (v3) its workspace;
//! `ext_workspace_v1` says which workspaces show; `zxdg_output_v1` places each output. Pictures
//! come from `ext_image_copy_capture_v1`, copied into a shared-memory file Tin reads back.
//! COSMIC offers all of these to ordinary clients: no portal, no permission prompt.

use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use cosmic_protocols::toplevel_info::v1::client::{zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1 as toplevel_v1, ext_foreign_toplevel_list_v1 as list_v1,
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1 as toplevel_sources, ext_image_capture_source_v1,
    ext_output_image_capture_source_manager_v1 as output_sources,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as frame_v1, ext_image_copy_capture_manager_v1 as copy_manager,
    ext_image_copy_capture_session_v1 as session_v1,
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1 as group_v1, ext_workspace_handle_v1 as workspace_v1,
    ext_workspace_manager_v1 as workspaces_v1,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

use super::Focus;
use crate::capture::Window;
use crate::screenshot::{Image, PixelOrder, Rect};
use crate::snapshot::{SnapWindow, Snapshot};

const ACTIVATED: u32 = 2;
const MINIMIZED: u32 = 1;
/// The longest Tin waits for COSMIC to describe windows and outputs.
const STATE_WAIT: Duration = Duration::from_millis(400);
/// The longest Tin waits for a picture.
const CAPTURE_WAIT: Duration = Duration::from_secs(2);

/// True when this looks like a COSMIC session, worth asking.
pub fn available() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("COSMIC"))
}

/// The focused window, and its picture when `want_screenshot` says so.
pub fn focused(want_screenshot: impl FnOnce(&Window) -> bool) -> Result<Focus, String> {
    let mut desktop = Desktop::connect()?;
    let focused = desktop.state.toplevels.iter().find(|t| t.has_state(ACTIVATED)).ok_or("no window is focused")?;
    let window = focused.window();
    let handle = focused.handle.clone();
    let screenshot = want_screenshot(&window).then(|| desktop.capture(Source::Toplevel(&handle)).and_then(|i| i.png()));
    Ok(Focus { window, screenshot })
}

/// Every monitor, frozen, with the windows visible on it. The focused window's monitor first.
pub fn snapshots() -> Result<Vec<Snapshot>, String> {
    let mut desktop = Desktop::connect()?;
    let state = &desktop.state;
    let focused_output = state
        .toplevels
        .iter()
        .find(|t| t.has_state(ACTIVATED))
        .and_then(|t| t.geometry.first())
        .map(|(o, _)| o.clone());
    let mut outputs: Vec<(wl_output::WlOutput, u32, String, Rect, Vec<SnapWindow>)> = Vec::new();
    for output in &state.outputs {
        let Some(bounds) = output.logical else { continue };
        let id = output.wl.id();
        let mut windows: Vec<SnapWindow> = state
            .toplevels
            .iter()
            .filter(|t| state.visible(t))
            .filter_map(|t| {
                let (_, rect) = t.geometry.iter().find(|(o, _)| *o == id)?;
                Some(SnapWindow { window: t.window(), rect: *rect, focused: t.has_state(ACTIVATED) })
            })
            .collect();
        windows.sort_by_key(|w| !w.focused);
        outputs.push((output.wl.clone(), output.global, output.name.clone().unwrap_or_default(), bounds, windows));
    }
    outputs.sort_by_key(|(wl, ..)| Some(wl.id()) != focused_output);
    if outputs.is_empty() {
        return Err("COSMIC reported no monitors".into());
    }
    outputs
        .into_iter()
        .map(|(wl, display, output, bounds, windows)| {
            let image = desktop.capture(Source::Output(&wl))?;
            Ok(Snapshot { output, display: Some(display), bounds, image, windows })
        })
        .collect()
}

/// Pictures of a snapshot's windows that overlap another window, each as its app draws it,
/// even where it's covered (`None` for the rest, or where it can't be found or taken). For
/// [`Snapshot::stacking`].
pub fn window_pictures(snap: &Snapshot) -> Result<Vec<Option<Image>>, String> {
    let mut desktop = Desktop::connect()?;
    let output = desktop
        .state
        .outputs
        .iter()
        .find(|o| o.name.as_deref() == Some(snap.output.as_str()))
        .map(|o| o.wl.id())
        .ok_or("that monitor isn't connected anymore")?;
    let overlaps = |i: usize| {
        let r = snap.windows[i].rect;
        snap.windows.iter().enumerate().any(|(j, w)| j != i && r.clamp_to(w.rect).is_some())
    };
    let handles: Vec<Option<toplevel_v1::ExtForeignToplevelHandleV1>> = (0..snap.windows.len())
        .map(|i| {
            let w = &snap.windows[i];
            overlaps(i)
                .then(|| {
                    desktop.state.toplevels.iter().find(|t| {
                        t.app_id == w.window.app
                            && t.title == w.window.title
                            && t.geometry.iter().any(|(o, r)| *o == output && *r == w.rect)
                    })
                })?
                .map(|t| t.handle.clone())
        })
        .collect();
    Ok(handles.iter().map(|h| h.as_ref().and_then(|h| desktop.capture(Source::Toplevel(h)).ok())).collect())
}

enum Source<'a> {
    Toplevel(&'a toplevel_v1::ExtForeignToplevelHandleV1),
    Output(&'a wl_output::WlOutput),
}

/// One connection's view of the desktop. Handles are only valid on the connection that made
/// them, so pictures are taken through the same one.
struct Desktop {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
}

#[derive(Default)]
struct State {
    list: Option<list_v1::ExtForeignToplevelListV1>,
    info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    xdg: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    shm: Option<wl_shm::WlShm>,
    toplevel_sources: Option<toplevel_sources::ExtForeignToplevelImageCaptureSourceManagerV1>,
    output_sources: Option<output_sources::ExtOutputImageCaptureSourceManagerV1>,
    copy: Option<copy_manager::ExtImageCopyCaptureManagerV1>,
    outputs: Vec<Output>,
    workspaces: Vec<Workspace>,
    toplevels: Vec<Toplevel>,
    session: Session,
    frame: Option<Result<(), String>>,
}

struct Output {
    wl: wl_output::WlOutput,
    /// Its registry name, which other clients (Rune's overlays) target it by.
    global: u32,
    xdg: Option<zxdg_output_v1::ZxdgOutputV1>,
    name: Option<String>,
    position: Option<(i32, i32)>,
    logical: Option<Rect>,
}

struct Workspace {
    handle: workspace_v1::ExtWorkspaceHandleV1,
    active: bool,
}

struct Toplevel {
    handle: toplevel_v1::ExtForeignToplevelHandleV1,
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    title: Option<String>,
    app_id: Option<String>,
    /// None until COSMIC reports the window's state.
    states: Option<Vec<u32>>,
    /// Where the window is on each output it's on, in points from the output's top-left.
    geometry: Vec<(ObjectId, Rect)>,
    workspaces: Vec<ObjectId>,
}

impl Toplevel {
    fn has_state(&self, state: u32) -> bool {
        self.states.as_ref().is_some_and(|s| s.contains(&state))
    }

    fn window(&self) -> Window {
        Window { app: self.app_id.clone(), title: self.title.clone() }
    }
}

impl State {
    /// Not minimized, and on a workspace that shows (or COSMIC didn't say which).
    fn visible(&self, t: &Toplevel) -> bool {
        let on_shown_workspace = t.workspaces.is_empty()
            || self.workspaces.iter().any(|w| w.active && t.workspaces.contains(&w.handle.id()));
        !t.has_state(MINIMIZED) && on_shown_workspace
    }
}

/// What the capture session says about the buffer it wants.
#[derive(Default)]
struct Session {
    size: Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    done: bool,
    stopped: bool,
}

impl Desktop {
    fn connect() -> Result<Desktop, String> {
        let fail = |what: &str, e: &dyn std::fmt::Display| format!("couldn't ask COSMIC about windows ({what}: {e})");
        let conn = Connection::connect_to_env().map_err(|e| fail("connect", &e))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State::default();
        // Globals; then windows, output names and workspaces.
        queue.roundtrip(&mut state).map_err(|e| fail("registry", &e))?;
        queue.roundtrip(&mut state).map_err(|e| fail("windows", &e))?;
        let info = state.info.clone().ok_or("COSMIC didn't offer zcosmic_toplevel_info_v1 version 2 or later")?;
        for t in &mut state.toplevels {
            t.cosmic = Some(info.get_cosmic_toplevel(&t.handle, &qh, ()));
        }
        if let Some(xdg) = state.xdg.clone() {
            for o in &mut state.outputs {
                o.xdg = Some(xdg.get_xdg_output(&o.wl, &qh, ()));
            }
        }
        let mut desktop = Desktop { conn, queue, qh, state };
        // COSMIC sends each window's state on its next refresh, not in reply to the request.
        desktop
            .wait(STATE_WAIT, |s| {
                s.toplevels.iter().all(|t| t.states.is_some()) && s.outputs.iter().all(|o| o.logical.is_some())
            })
            .map_err(|e| fail("states", &e))?;
        Ok(desktop)
    }

    /// Dispatches until `ready` holds or `limit` passes (not an error: the caller checks).
    fn wait(&mut self, limit: Duration, ready: impl Fn(&State) -> bool) -> Result<(), wayland_client::DispatchError> {
        let deadline = Instant::now() + limit;
        loop {
            self.queue.roundtrip(&mut self.state)?;
            if ready(&self.state) || Instant::now() >= deadline {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn capture(&mut self, source: Source<'_>) -> Result<Image, String> {
        let (Some(copy), Some(shm)) = (self.state.copy.clone(), self.state.shm.clone()) else {
            return Err("COSMIC didn't offer screen capture (ext-image-copy-capture)".into());
        };
        let qh = self.qh.clone();
        let source = match source {
            Source::Toplevel(handle) => self
                .state
                .toplevel_sources
                .as_ref()
                .ok_or("COSMIC can't capture windows")?
                .create_source(handle, &qh, ()),
            Source::Output(output) => self
                .state
                .output_sources
                .as_ref()
                .ok_or("COSMIC can't capture monitors")?
                .create_source(output, &qh, ()),
        };
        self.state.session = Session::default();
        let session = copy.create_session(&source, copy_manager::Options::empty(), &qh, ());
        let result = self.copy_frame(&session, &shm);
        session.destroy();
        source.destroy();
        result
    }

    fn copy_frame(
        &mut self,
        session: &session_v1::ExtImageCopyCaptureSessionV1,
        shm: &wl_shm::WlShm,
    ) -> Result<Image, String> {
        self.wait(CAPTURE_WAIT, |s| s.session.done || s.session.stopped).map_err(|e| e.to_string())?;
        let s = &self.state.session;
        let (width, height) = s.size.filter(|_| !s.stopped).ok_or("COSMIC stopped the capture")?;
        let (format, order) = [
            (wl_shm::Format::Xrgb8888, PixelOrder::Bgrx),
            (wl_shm::Format::Argb8888, PixelOrder::Bgra),
            (wl_shm::Format::Xbgr8888, PixelOrder::Rgbx),
            (wl_shm::Format::Abgr8888, PixelOrder::Rgba),
        ]
        .into_iter()
        .find(|(f, _)| s.formats.contains(f))
        .ok_or("COSMIC offered no 8-bit RGB format")?;
        let stride = width as usize * 4;
        let len = stride * height as usize;
        // Shared memory the compositor writes into, read back as a file: no mapping needed.
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, Into::into);
        let file: File = tempfile::tempfile_in(dir).map_err(|e| format!("couldn't make the capture buffer: {e}"))?;
        file.set_len(len as u64).map_err(|e| format!("couldn't size the capture buffer: {e}"))?;
        let size = i32::try_from(len).map_err(|_| "the picture is too large to capture")?;
        let qh = self.qh.clone();
        let pool = shm.create_pool(file.as_fd(), size, &qh, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, &qh, ());
        let frame = session.create_frame(&qh, ());
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, width as i32, height as i32);
        frame.capture();
        self.state.frame = None;
        let waited = self.wait(CAPTURE_WAIT, |s| s.frame.is_some());
        frame.destroy();
        buffer.destroy();
        pool.destroy();
        waited.map_err(|e| e.to_string())?;
        self.state.frame.take().ok_or("COSMIC didn't deliver the picture in time")??;
        let mut pixels = vec![0; len];
        file.read_exact_at(&mut pixels, 0).map_err(|e| format!("couldn't read the capture buffer: {e}"))?;
        Image::from_shm(&pixels, width, height, stride, order)
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        for t in &self.state.toplevels {
            if let Some(c) = &t.cosmic {
                c.destroy();
            }
            t.handle.destroy();
        }
        for o in &self.state.outputs {
            if let Some(x) = &o.xdg {
                x.destroy();
            }
        }
        if let Some(list) = &self.state.list {
            list.stop();
        }
        let _ = self.conn.flush();
    }
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
            "ext_workspace_manager_v1" => {
                registry.bind::<workspaces_v1::ExtWorkspaceManagerV1, _, _>(name, 1, qh, ());
            }
            "zxdg_output_manager_v1" => state.xdg = Some(registry.bind(name, version.min(3), qh, ())),
            "wl_output" if version >= 4 => {
                let wl = registry.bind(name, 4, qh, ());
                state.outputs.push(Output { wl, global: name, xdg: None, name: None, position: None, logical: None });
            }
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                state.toplevel_sources = Some(registry.bind(name, 1, qh, ()));
            }
            "ext_output_image_capture_source_manager_v1" => state.output_sources = Some(registry.bind(name, 1, qh, ())),
            "ext_image_copy_capture_manager_v1" => state.copy = Some(registry.bind(name, 1, qh, ())),
            _ => {}
        }
    }
}

impl Dispatch<list_v1::ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &list_v1::ExtForeignToplevelListV1,
        event: list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                handle: toplevel,
                cosmic: None,
                title: None,
                app_id: None,
                states: None,
                geometry: Vec::new(),
                workspaces: Vec::new(),
            });
        }
    }

    event_created_child!(State, list_v1::ExtForeignToplevelListV1, [
        list_v1::EVT_TOPLEVEL_OPCODE => (toplevel_v1::ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<toplevel_v1::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &toplevel_v1::ExtForeignToplevelHandleV1,
        event: toplevel_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(t) = state.toplevels.iter_mut().find(|t| t.handle.id() == handle.id()) else { return };
        match event {
            toplevel_v1::Event::Title { title } => t.title = Some(title),
            toplevel_v1::Event::AppId { app_id } => t.app_id = Some(app_id),
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
        let Some(t) = state.toplevels.iter_mut().find(|t| t.cosmic.as_ref().is_some_and(|c| c.id() == handle.id()))
        else {
            return;
        };
        match event {
            zcosmic_toplevel_handle_v1::Event::State { state: bytes } => {
                t.states = Some(bytes.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c)).collect());
            }
            zcosmic_toplevel_handle_v1::Event::Geometry { output, x, y, width, height } => {
                t.geometry.retain(|(o, _)| *o != output.id());
                t.geometry.push((output.id(), Rect { x, y, width, height }));
            }
            zcosmic_toplevel_handle_v1::Event::ExtWorkspaceEnter { workspace } => t.workspaces.push(workspace.id()),
            zcosmic_toplevel_handle_v1::Event::ExtWorkspaceLeave { workspace } => {
                t.workspaces.retain(|w| *w != workspace.id());
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(o) = state.outputs.iter_mut().find(|o| o.wl.id() == output.id())
        {
            o.name = Some(name);
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, ()> for State {
    fn event(
        state: &mut Self,
        xdg: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.iter_mut().find(|o| o.xdg.as_ref().is_some_and(|x| x.id() == xdg.id())) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => o.position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                let (x, y) = o.position.unwrap_or((0, 0));
                o.logical = Some(Rect { x, y, width, height });
            }
            _ => {}
        }
    }
}

impl Dispatch<workspaces_v1::ExtWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &workspaces_v1::ExtWorkspaceManagerV1,
        event: workspaces_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let workspaces_v1::Event::Workspace { workspace } = event {
            state.workspaces.push(Workspace { handle: workspace, active: false });
        }
    }

    event_created_child!(State, workspaces_v1::ExtWorkspaceManagerV1, [
        workspaces_v1::EVT_WORKSPACE_GROUP_OPCODE => (group_v1::ExtWorkspaceGroupHandleV1, ()),
        workspaces_v1::EVT_WORKSPACE_OPCODE => (workspace_v1::ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<workspace_v1::ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &workspace_v1::ExtWorkspaceHandleV1,
        event: workspace_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let workspace_v1::Event::State { state: WEnum::Value(flags) } = event
            && let Some(w) = state.workspaces.iter_mut().find(|w| w.handle.id() == handle.id())
        {
            w.active = flags.contains(workspace_v1::State::Active);
        }
    }
}

impl Dispatch<session_v1::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &session_v1::ExtImageCopyCaptureSessionV1,
        event: session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            session_v1::Event::BufferSize { width, height } => state.session.size = Some((width, height)),
            session_v1::Event::ShmFormat { format: WEnum::Value(f) } => state.session.formats.push(f),
            session_v1::Event::Done => state.session.done = true,
            session_v1::Event::Stopped => state.session.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<frame_v1::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &frame_v1::ExtImageCopyCaptureFrameV1,
        event: frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            frame_v1::Event::Ready => state.frame = Some(Ok(())),
            frame_v1::Event::Failed { reason } => {
                state.frame = Some(Err(format!("COSMIC couldn't capture it ({reason:?})")));
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore group_v1::ExtWorkspaceGroupHandleV1);
delegate_noop!(State: zxdg_output_manager_v1::ZxdgOutputManagerV1);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: toplevel_sources::ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(State: output_sources::ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ext_image_capture_source_v1::ExtImageCaptureSourceV1);
delegate_noop!(State: copy_manager::ExtImageCopyCaptureManagerV1);

#[cfg(test)]
mod tests {
    /// Needs a COSMIC session: `cargo test -p tin-core -- --ignored cosmic_live --nocapture`.
    /// Writes the focused window and the frozen monitor to `$TMPDIR/tin-cosmic-*.png`.
    #[test]
    #[ignore = "needs a running COSMIC session"]
    fn cosmic_live_focused_window_and_snapshot() {
        let focus = super::focused(|_| true).unwrap_or_else(|e| panic!("{e}"));
        let shot = focus.screenshot.unwrap_or_else(|| panic!("no picture taken")).unwrap_or_else(|e| panic!("{e}"));
        let dir = std::env::temp_dir();
        std::fs::write(dir.join("tin-cosmic-window.png"), &shot.png).unwrap_or_else(|e| panic!("{e}"));
        println!("focused {:?} {}×{}", focus.window, shot.width, shot.height);

        let started = std::time::Instant::now();
        let snaps = super::snapshots().unwrap_or_else(|e| panic!("{e}"));
        for snap in &snaps {
            let started = std::time::Instant::now();
            let pictures = super::window_pictures(snap).unwrap_or_else(|e| panic!("{e}"));
            let z = snap.stacking(&pictures);
            println!("{}: {} pictures in {:?}", snap.output, pictures.iter().flatten().count(), started.elapsed());
            for (w, z) in snap.windows.iter().zip(&z) {
                println!("  z={z} {:?} {:?}", w.window.app, w.rect);
            }
        }
        println!("froze {} monitors in {:?}", snaps.len(), started.elapsed());
        for snap in &snaps {
            let png = snap.image.png().unwrap_or_else(|e| panic!("{e}"));
            std::fs::write(dir.join(format!("tin-cosmic-{}.png", snap.output)), &png.png)
                .unwrap_or_else(|e| panic!("{e}"));
            println!("output {} {:?} image {}×{}", snap.output, snap.bounds, snap.image.width, snap.image.height);
            for w in &snap.windows {
                println!("  {:?} {:?} focused={}", w.window.app, w.rect, w.focused);
            }
        }
    }
}
