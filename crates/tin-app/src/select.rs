//! Picking a window or an area on frozen monitors (Linux, Wayland layer shell).
//!
//! One overlay surface covers each monitor and shows what it showed when the picker opened,
//! dimmed. The element under the pointer lights up on whichever monitor the pointer is on (see
//! `elements.rs`); the scroll wheel grows or shrinks it, a click picks it, a drag picks an area,
//! Escape cancels. Rune's own overlays can't take pointer input on Wayland, so these surfaces are
//! drawn here in software.

use std::sync::Arc;

use ab_glyph::FontVec;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers};
use smithay_client_toolkit::seat::pointer::{
    CursorIcon, PointerData, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, registry_handlers,
};
use tin_core::capture::Window;
use tin_core::screenshot::Rect;
use tin_core::snapshot::{SelectionKind, Snapshot};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_callback, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::canvas::{Backdrop, Canvas, Rgba};
use crate::elements::{Chain, Finder, Item};

/// A drag shorter than this (points) is a click.
const CLICK_SLOP: f64 = 6.0;
const BTN_LEFT: u32 = 0x110;
const ACCENT: Rgba = Rgba(64, 196, 120, 255);
const HINT: &str = "Click to pick  ·  Scroll to grow or shrink  ·  Drag an area  ·  Esc to cancel";

/// What the user picked.
pub struct Picked {
    pub snap: Arc<Snapshot>,
    /// Points from the monitor's top-left.
    pub rect: Rect,
    pub kind: SelectionKind,
    pub window: Option<Window>,
}

/// Shows the picker over every monitor and waits for a pick. `Ok(None)` is a cancel.
pub fn pick(snaps: &[Arc<Snapshot>]) -> Result<Option<Picked>, String> {
    let fail = |what: &str, e: &dyn std::fmt::Display| format!("couldn't show the picker ({what}: {e})");
    let conn = Connection::connect_to_env().map_err(|e| fail("connect", &e))?;
    let (globals, mut queue) = registry_queue_init::<Picker>(&conn).map_err(|e| fail("registry", &e))?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| fail("wl_compositor", &e))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| fail("layer shell", &e))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| fail("wl_shm", &e))?;
    let pool = SlotPool::new(1920 * 1080 * 4, &shm).map_err(|e| fail("shared memory", &e))?;
    // The element worker wakes this loop with a sync callback when it has an answer.
    let (wake_conn, wake_qh) = (conn.clone(), qh.clone());
    let finder = Finder::spawn(move || {
        wake_conn.display().sync(&wake_qh, ());
        let _ = wake_conn.flush();
    });
    let mut picker = Picker {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        compositor,
        pool,
        surfaces: Vec::new(),
        keyboard: None,
        pointer: None,
        font: crate::canvas::system_font(),
        at: None,
        press: None,
        result: None,
        finder,
        chain: None,
        level: 0,
    };
    // Outputs' names, to match each snapshot to its monitor.
    queue.roundtrip(&mut picker).map_err(|e| fail("outputs", &e))?;
    for snap in snaps {
        let output = picker
            .outputs
            .outputs()
            .find(|o| picker.outputs.info(o).and_then(|i| i.name).is_some_and(|n| n == snap.output));
        let Some(output) = output else { continue };
        let scale = picker.outputs.info(&output).map_or(1, |i| i.scale_factor.max(1));
        let surface = picker.compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("tin-picker"), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.wl_surface().set_buffer_scale(scale);
        layer.commit();
        picker.surfaces.push(Surface {
            layer,
            snap: snap.clone(),
            scale,
            size: None,
            backdrop: None,
            shown: None,
            in_flight: false,
        });
    }
    if picker.surfaces.is_empty() {
        return Err("none of the frozen monitors is still connected".into());
    }
    while picker.result.is_none() {
        queue.blocking_dispatch(&mut picker).map_err(|e| fail("events", &e))?;
    }
    let result = picker.result.take().unwrap_or(None);
    // Take the surfaces down before anything else opens.
    picker.surfaces.clear();
    let _ = queue.roundtrip(&mut picker);
    Ok(result)
}

struct Surface {
    layer: LayerSurface,
    snap: Arc<Snapshot>,
    scale: i32,
    /// Points, once configured.
    size: Option<(u32, u32)>,
    backdrop: Option<Backdrop>,
    /// What the last frame showed, to skip redrawing the same thing.
    shown: Option<Frame>,
    /// A frame was committed and the compositor hasn't asked for the next one yet. Drawing
    /// waits for that, so a fast mouse costs one redraw per display refresh, not one per event.
    in_flight: bool,
}

/// What one surface shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    highlight: Option<(Rect, String)>,
    hint: bool,
}

struct Picker {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    compositor: CompositorState,
    pool: SlotPool,
    surfaces: Vec<Surface>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer<PointerData>>,
    font: Option<FontVec>,
    /// The pointer: which surface, and where on it in points.
    at: Option<(usize, (f64, f64))>,
    /// Where a press started: surface and point.
    press: Option<(usize, (f64, f64))>,
    /// Set once the user picks or cancels.
    result: Option<Option<Picked>>,
    finder: Finder,
    /// The newest answer about what's under the pointer.
    chain: Option<Chain>,
    /// Scroll steps out (positive) or in (negative) from the default element.
    level: i32,
}

impl Picker {
    fn surface_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.layer.wl_surface() == surface)
    }

    fn dragged(&self) -> Option<(usize, Rect)> {
        let ((i, start), (j, now)) = (self.press?, self.at?);
        let moved = (now.0 - start.0).hypot(now.1 - start.1) > CLICK_SLOP;
        (i == j && moved).then(|| (i, Rect::between(round(start), round(now))))
    }

    /// What's lit under the pointer on surface `i`: the chosen element when the worker has
    /// answered for this window, else the window itself.
    fn hovered(&self, i: usize) -> Option<(usize, Item)> {
        let (p, point) = self.at?;
        if p != i {
            return None;
        }
        let snap = &self.surfaces[i].snap;
        let (x, y) = round(point);
        let w = snap.window_index_at(x, y)?;
        match &self.chain {
            Some(chain) if chain.output == snap.output && chain.window == w => {
                chain.items.get(chain.index(self.level)).map(|item| (w, item.clone()))
            }
            _ => {
                let target = &snap.windows[w];
                Some((w, Item { rect: target.rect, label: label(&target.window), kind: SelectionKind::Window }))
            }
        }
    }

    fn frame_for(&self, i: usize) -> Frame {
        let highlight = match self.dragged() {
            Some((d, rect)) => (d == i).then(|| (rect, format!("{} × {}", rect.width, rect.height))),
            None => self.hovered(i).map(|(_, item)| (item.rect, item.label)),
        };
        let hint = self.at.map_or(i == 0, |(p, _)| p == i);
        Frame { highlight, hint }
    }

    /// Asks the worker what's under the pointer.
    fn look(&self) {
        if self.press.is_some() {
            return;
        }
        let Some((i, point)) = self.at else { return };
        let snap = &self.surfaces[i].snap;
        let (x, y) = round(point);
        if let Some(w) = snap.window_index_at(x, y) {
            self.finder.ask(snap.clone(), w, (x, y));
        }
    }

    fn redraw_all(&mut self, qh: &QueueHandle<Self>) {
        for i in 0..self.surfaces.len() {
            self.draw(i, qh);
        }
    }

    fn draw(&mut self, i: usize, qh: &QueueHandle<Self>) {
        let frame = self.frame_for(i);
        let font = self.font.as_ref();
        let s = &mut self.surfaces[i];
        let Some((w, h)) = s.size else { return };
        if s.in_flight || s.shown.as_ref() == Some(&frame) {
            return;
        }
        let (bw, bh) = (w * s.scale as u32, h * s.scale as u32);
        let backdrop = s.backdrop.get_or_insert_with(|| Backdrop::new(&s.snap.image, bw, bh));
        let Ok((buffer, canvas)) =
            self.pool.create_buffer(bw as i32, bh as i32, bw as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let k = s.scale;
        let px = |r: Rect| Rect { x: r.x * k, y: r.y * k, width: r.width * k, height: r.height * k };
        backdrop.compose(canvas, frame.highlight.as_ref().map(|(r, _)| px(*r)));
        let mut c = Canvas { buf: canvas, width: bw, height: bh };
        let size = 13.0 * k as f32;
        if let Some((r, text)) = &frame.highlight {
            let r = px(*r);
            c.outline(r, 2 * k, ACCENT);
            let label_y = if r.y >= 30 * k { r.y - 28 * k } else { r.y + 6 * k };
            c.pill(font, r.x + 6 * k, label_y, text, size, (r.width - 12 * k).max(80 * k));
        }
        if frame.hint {
            let width =
                font.map_or(0.0, |f| crate::canvas::measure(f, HINT, size)).ceil() as i32 + (size * 1.4) as i32 + 4;
            c.pill(font, (bw as i32 - width) / 2, 14 * k, HINT, size, width);
        }
        let surface = s.layer.wl_surface();
        surface.damage_buffer(0, 0, bw as i32, bh as i32);
        if buffer.attach_to(surface).is_ok() {
            surface.frame(qh, surface.clone());
            s.layer.commit();
            s.shown = Some(frame);
            s.in_flight = true;
        }
    }

    fn finish(&mut self, release: (usize, (f64, f64))) {
        let picked = match self.dragged() {
            Some((i, rect)) if rect.width >= 4 && rect.height >= 4 => {
                Some(Picked { snap: self.surfaces[i].snap.clone(), rect, kind: SelectionKind::Region, window: None })
            }
            _ => {
                let (i, _) = release;
                let snap = self.surfaces[i].snap.clone();
                self.hovered(i).map(|(w, item)| Picked {
                    window: Some(snap.windows[w].window.clone()),
                    snap,
                    rect: item.rect,
                    kind: item.kind,
                })
            }
        };
        self.press = None;
        if picked.is_some() {
            self.result = Some(picked);
        }
    }
}

fn round((x, y): (f64, f64)) -> (i32, i32) {
    (x.round() as i32, y.round() as i32)
}

fn label(w: &Window) -> String {
    w.title.clone().or_else(|| w.app.clone()).unwrap_or_else(|| "Window".into())
}

/// The element worker's wake-up: take its answer and redraw.
impl Dispatch<wl_callback::WlCallback, ()> for Picker {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(chain) = state.finder.take() else { return };
        let innermost = |c: &Chain| (c.output.clone(), c.window, c.items.last().map(|i| i.rect));
        if state.chain.as_ref().map(innermost) != Some(innermost(&chain)) {
            state.level = 0;
        }
        state.chain = Some(chain);
        state.redraw_all(qh);
    }
}

impl LayerShellHandler for Picker {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.result = Some(None);
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.surfaces.iter().position(|s| s.layer == *layer) else { return };
        let s = &mut self.surfaces[i];
        let (w, h) = configure.new_size;
        let size = if w == 0 || h == 0 {
            (s.snap.bounds.width.max(1) as u32, s.snap.bounds.height.max(1) as u32)
        } else {
            (w, h)
        };
        if s.size != Some(size) {
            s.size = Some(size);
            s.backdrop = None;
            s.shown = None;
        }
        self.draw(i, qh);
    }
}

impl PointerHandler for Picker {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(i) = self.surface_index(&event.surface) else { continue };
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    if let Some(p) = &self.pointer {
                        let _ = p.set_cursor(conn, CursorIcon::Crosshair);
                    }
                    self.at = Some((i, event.position));
                    self.look();
                }
                PointerEventKind::Leave { .. } => {
                    if self.at.is_some_and(|(p, _)| p == i) {
                        self.at = None;
                    }
                }
                PointerEventKind::Motion { .. } => {
                    self.at = Some((i, event.position));
                    self.look();
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let step = if vertical.discrete != 0 {
                        vertical.discrete.signum()
                    } else {
                        vertical.absolute.signum() as i32
                    };
                    // Scrolling up grows the highlight to what's around it; down shrinks it.
                    self.level -= step;
                    let len = self.chain.as_ref().map_or(1, |c| c.items.len()) as i32;
                    self.level = self.level.clamp(-len, len);
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => self.press = Some((i, event.position)),
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    self.at = Some((i, event.position));
                    if self.press.is_some() {
                        self.finish((i, event.position));
                    }
                }
                _ => {}
            }
        }
        if self.result.is_none() {
            self.redraw_all(qh);
        }
    }
}

impl KeyboardHandler for Picker {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if event.keysym == Keysym::Escape {
            self.result = Some(None);
        }
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl SeatHandler for Picker {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seats.get_keyboard(qh, &seat, None).ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let surface = self.compositor.create_surface(qh);
            self.pointer =
                self.seats.get_pointer_with_theme(qh, &seat, self.shm.wl_shm(), surface, ThemeSpec::default()).ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if capability == Capability::Pointer {
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for Picker {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    /// The compositor is ready for the next frame: draw it if anything changed meanwhile.
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(i) = self.surface_index(surface) {
            self.surfaces[i].in_flight = false;
            self.draw(i, qh);
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Picker {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Picker {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Picker {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Picker);
delegate_output!(Picker);
delegate_shm!(Picker);
delegate_seat!(Picker);
delegate_keyboard!(Picker);
delegate_pointer!(Picker);
delegate_layer!(Picker);
delegate_registry!(Picker);
