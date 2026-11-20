//! COSMIC: the focused window, from the compositor's own toplevel protocols, and a picture of it.
//!
//! `ext_foreign_toplevel_list_v1` names every window (title, app id); COSMIC's
//! `zcosmic_toplevel_info_v1` (v2+) adds each one's state, including "activated". The picture
//! comes from `ext_image_copy_capture_v1` with that window as the source, copied into a
//! shared-memory file Tin reads back. COSMIC offers all of these to ordinary clients, so no
//! portal or permission prompt is involved.

use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use cosmic_protocols::toplevel_info::v1::client::{zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1 as source_manager, ext_image_capture_source_v1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as frame_v1, ext_image_copy_capture_manager_v1 as copy_manager,
    ext_image_copy_capture_session_v1 as session_v1,
};

use super::Focus;
use crate::capture::Window;
use crate::screenshot::{PixelOrder, Screenshot};

const ACTIVATED: u32 = 2;
/// The longest Tin waits for COSMIC to report window states.
const STATE_WAIT: Duration = Duration::from_millis(400);
/// The longest Tin waits for a window's picture.
const CAPTURE_WAIT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    shm: Option<wl_shm::WlShm>,
    sources: Option<source_manager::ExtForeignToplevelImageCaptureSourceManagerV1>,
    copy: Option<copy_manager::ExtImageCopyCaptureManagerV1>,
    toplevels: Vec<Toplevel>,
    session: Session,
    frame: Option<Result<(), String>>,
}

struct Toplevel {
    handle: ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    title: Option<String>,
    app_id: Option<String>,
    /// None until COSMIC reports the window's state.
    activated: Option<bool>,
}

/// What the capture session says about the buffer it wants.
#[derive(Default)]
struct Session {
    size: Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    done: bool,
    stopped: bool,
}

/// True when this looks like a COSMIC session, worth asking.
pub fn available() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("COSMIC"))
}

/// The focused window, and its picture when `want_screenshot` says so.
pub fn focused(want_screenshot: impl FnOnce(&Window) -> bool) -> Result<Focus, String> {
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
    // poll briefly until every window has reported.
    wait(&mut queue, &mut state, STATE_WAIT, |s| s.toplevels.iter().all(|t| t.activated.is_some()))
        .map_err(|e| fail("states", &e))?;
    let focused = state.toplevels.iter().find(|t| t.activated == Some(true)).ok_or("no window is focused")?;
    let window = Window { app: focused.app_id.clone(), title: focused.title.clone() };
    let handle = focused.handle.clone();
    let screenshot = want_screenshot(&window).then(|| screenshot(&mut queue, &mut state, &qh, &handle));

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
    Ok(Focus { window, screenshot })
}

fn screenshot(
    queue: &mut EventQueue<State>,
    state: &mut State,
    qh: &QueueHandle<State>,
    handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
) -> Result<Screenshot, String> {
    let (Some(sources), Some(copy), Some(shm)) = (state.sources.clone(), state.copy.clone(), state.shm.clone()) else {
        return Err("COSMIC didn't offer window capture (ext-image-copy-capture)".into());
    };
    let source = sources.create_source(handle, qh, ());
    let session = copy.create_session(&source, copy_manager::Options::empty(), qh, ());
    let result = (|| {
        wait(queue, state, CAPTURE_WAIT, |s| s.session.done || s.session.stopped).map_err(|e| e.to_string())?;
        let (width, height) =
            state.session.size.filter(|_| !state.session.stopped).ok_or("COSMIC stopped the capture")?;
        let (format, order) = [
            (wl_shm::Format::Xrgb8888, PixelOrder::Bgrx),
            (wl_shm::Format::Argb8888, PixelOrder::Bgra),
            (wl_shm::Format::Xbgr8888, PixelOrder::Rgbx),
            (wl_shm::Format::Abgr8888, PixelOrder::Rgba),
        ]
        .into_iter()
        .find(|(f, _)| state.session.formats.contains(f))
        .ok_or("COSMIC offered no 8-bit RGB format for the window")?;
        let stride = width as usize * 4;
        let len = stride * height as usize;
        // Shared memory the compositor writes into; read back as a file, so no mapping is needed.
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, Into::into);
        let file: File = tempfile::tempfile_in(dir).map_err(|e| format!("couldn't make the capture buffer: {e}"))?;
        file.set_len(len as u64).map_err(|e| format!("couldn't size the capture buffer: {e}"))?;
        let size = i32::try_from(len).map_err(|_| "the window is too large to capture")?;
        let pool = shm.create_pool(file.as_fd(), size, qh, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, ());
        let frame = session.create_frame(qh, ());
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, width as i32, height as i32);
        frame.capture();
        state.frame = None;
        let waited = wait(queue, state, CAPTURE_WAIT, |s| s.frame.is_some());
        frame.destroy();
        buffer.destroy();
        pool.destroy();
        waited.map_err(|e| e.to_string())?;
        state.frame.take().ok_or("COSMIC didn't deliver the window's picture in time")??;
        let mut pixels = vec![0; len];
        file.read_exact_at(&mut pixels, 0).map_err(|e| format!("couldn't read the capture buffer: {e}"))?;
        Screenshot::encode(&pixels, width, height, stride, order)
    })();
    session.destroy();
    source.destroy();
    result
}

/// Dispatches until `ready` holds or `limit` passes (not an error: the caller checks).
fn wait(
    queue: &mut EventQueue<State>,
    state: &mut State,
    limit: Duration,
    ready: impl Fn(&State) -> bool,
) -> Result<(), wayland_client::DispatchError> {
    let deadline = Instant::now() + limit;
    loop {
        queue.roundtrip(state)?;
        if ready(state) || Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
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
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                state.sources = Some(registry.bind(name, 1, qh, ()))
            }
            "ext_image_copy_capture_manager_v1" => state.copy = Some(registry.bind(name, 1, qh, ())),
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
            t.activated = Some(bytes.as_chunks::<4>().0.iter().any(|c| u32::from_ne_bytes(*c) == ACTIVATED));
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
                state.frame = Some(Err(format!("COSMIC couldn't capture the window ({reason:?})")));
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: source_manager::ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(State: ext_image_capture_source_v1::ExtImageCaptureSourceV1);
delegate_noop!(State: copy_manager::ExtImageCopyCaptureManagerV1);

#[cfg(test)]
mod tests {
    /// Needs a COSMIC session: `cargo test -p tin-core -- --ignored cosmic_live --nocapture`.
    /// Writes the focused window's picture to `$TMPDIR/tin-cosmic-live.png`.
    #[test]
    #[ignore = "needs a running COSMIC session"]
    fn cosmic_live_focused_window_and_picture() {
        let focus = super::focused(|_| true).unwrap_or_else(|e| panic!("{e}"));
        let shot = focus.screenshot.unwrap_or_else(|| panic!("no picture taken")).unwrap_or_else(|e| panic!("{e}"));
        let path = std::env::temp_dir().join("tin-cosmic-live.png");
        std::fs::write(&path, &shot.png).unwrap_or_else(|e| panic!("{e}"));
        println!("{:?} {}×{} → {}", focus.window, shot.width, shot.height, path.display());
    }
}
