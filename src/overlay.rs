//! The selection overlay: one wlr-layer-shell surface per output, showing the
//! frozen frame dimmed, with the selection drawn undimmed on a subsurface
//! inside a rounded border in the Atlas accent. No toolkit; SHM buffers drawn
//! by hand.
//!
//! - Drag with the left button to select; release to finish. The modifiers
//!   held at release pick the mode (Ctrl: text, Alt: redact).
//! - A click without a drag selects the whole screen under the pointer.
//! - Escape or the right button cancels.
//!
//! Buffers hold frame pixels 1:1 and `wp_viewporter` maps them onto the
//! output's logical size, so the frozen image is pixel-exact at any scale.
//! Redraws follow frame callbacks, so a fast mouse never queues stale frames.

use std::rc::Rc;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{
    wl_keyboard, wl_output, wl_pointer, wl_seat, wl_subsurface, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle, delegate_noop};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_subcompositor, registry_handlers,
};
use wayland_client::protocol::wl_shm;

use crate::capture::{Frame, Rect};
use crate::watchdog::Watchdog;

/// Setup and teardown waits on the compositor (not the user).
const SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Atlas.Ui's `AtlasStyle.radius` and control border width (logical px).
const RADIUS: f64 = 6.0;
const BORDER: f64 = 1.0;

#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// 0 = no dimming, 0.9 = nearly black.
    pub dim: f32,
    pub accent: [u8; 3],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
}

/// Shows the overlay until the user selects or cancels. `Ok(None)` is a cancel.
pub fn select(
    conn: &Connection,
    frame: Rc<Frame>,
    style: Style,
) -> Result<Option<(Rect, Mods)>, String> {
    let (globals, mut queue) = registry_queue_init::<Overlay>(conn)
        .map_err(|e| format!("can't read the compositor's interfaces: {e}"))?;
    let qh = queue.handle();
    let missing = |what: &str| {
        format!("the compositor doesn't support {what}, which the selection overlay needs")
    };
    let compositor = CompositorState::bind(&globals, &qh).map_err(|_| missing("wl_compositor"))?;
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .map_err(|_| missing("subsurfaces"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| missing("wlr-layer-shell"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| missing("wl_shm"))?;
    let viewporter: wp_viewporter::WpViewporter = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| missing("wp_viewporter"))?;
    let pool =
        SlotPool::new(4096, &shm).map_err(|e| format!("can't allocate overlay memory: {e}"))?;

    let mut ov = Overlay {
        registry: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        cursor_shape: CursorShapeManager::bind(&globals, &qh).ok(),
        compositor,
        subcompositor,
        layer_shell,
        shm,
        viewporter,
        pool,
        frame,
        style,
        screens: Vec::new(),
        keyboard: None,
        pointer: None,
        shape_device: None,
        mods: Mods::default(),
        pos: None,
        anchor: None,
        result: None,
    };
    let lost = |e: &dyn std::fmt::Display| format!("lost the Wayland connection: {e}");
    // Bounded: a compositor that stops answering must not leave a hung
    // process behind the hotkey. Disarmed once a screen is configured.
    let mut setup = Some(Watchdog::arm(
        SETUP_TIMEOUT,
        "the selection overlay didn't appear",
    ));
    // Two rounds: output names and positions arrive after the globals.
    queue.roundtrip(&mut ov).map_err(|e| lost(&e))?;
    queue.roundtrip(&mut ov).map_err(|e| lost(&e))?;
    ov.create_screens(&qh)?;

    while ov.result.is_none() {
        queue.blocking_dispatch(&mut ov).map_err(|e| lost(&e))?;
        if setup.is_some() && ov.screens.iter().any(|s| s.configured) {
            setup = None;
        }
        ov.redraw(&qh);
    }
    drop(setup);
    // Tear the overlay down and make sure it's gone from the screen before
    // the caller spends time on OCR or the clipboard.
    let result = ov.result.take().unwrap_or(Ok(None));
    ov.destroy();
    let _teardown = Watchdog::arm(SETUP_TIMEOUT, "the selection overlay didn't close");
    queue.roundtrip(&mut ov).map_err(|e| lost(&e))?;
    result
}

struct Screen {
    /// Logical geometry, from the frame (what the image shows).
    geom: Rect,
    layer: LayerSurface,
    viewport: wp_viewport::WpViewport,
    sel_surface: wl_surface::WlSurface,
    sel_sub: wl_subsurface::WlSubsurface,
    sel_viewport: wp_viewport::WpViewport,
    base: Option<Buffer>,
    sel: Option<Buffer>,
    configured: bool,
    waiting_frame: bool,
    dirty: bool,
}

struct Overlay {
    registry: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    cursor_shape: Option<CursorShapeManager>,
    compositor: CompositorState,
    subcompositor: SubcompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    viewporter: wp_viewporter::WpViewporter,
    pool: SlotPool,
    frame: Rc<Frame>,
    style: Style,
    screens: Vec<Screen>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    shape_device: Option<WpCursorShapeDeviceV1>,
    mods: Mods,
    /// Pointer position, logical global.
    pos: Option<(f64, f64)>,
    /// Where the drag started, logical global.
    anchor: Option<(f64, f64)>,
    result: Option<Result<Option<(Rect, Mods)>, String>>,
}

impl Overlay {
    /// One layer surface per output the frame covers, matched by name (or,
    /// failing that, by position).
    fn create_screens(&mut self, qh: &QueueHandle<Self>) -> Result<(), String> {
        let outputs: Vec<wl_output::WlOutput> = self.output_state.outputs().collect();
        for output in outputs {
            let Some(info) = self.output_state.info(&output) else {
                continue;
            };
            let geom = self
                .frame
                .outputs
                .iter()
                .find(|o| info.name.as_deref() == Some(o.name.as_str()))
                .or_else(|| {
                    self.frame
                        .outputs
                        .iter()
                        .find(|o| Some((o.logical.x, o.logical.y)) == info.logical_position)
                })
                .map(|o| o.logical);
            let Some(geom) = geom else { continue };

            let surface = self.compositor.create_surface(qh);
            let layer = self.layer_shell.create_layer_surface(
                qh,
                surface,
                Layer::Overlay,
                Some("atlasos-screenshot"),
                Some(&output),
            );
            layer.set_anchor(Anchor::all());
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.set_size(0, 0);
            let viewport = self.viewporter.get_viewport(layer.wl_surface(), qh, ());
            let (sel_sub, sel_surface) = self
                .subcompositor
                .create_subsurface(layer.wl_surface().clone(), qh);
            let sel_viewport = self.viewporter.get_viewport(&sel_surface, qh, ());
            // Input goes to the layer surface underneath, never the selection.
            if let Ok(region) = Region::new(&self.compositor) {
                sel_surface.set_input_region(Some(region.wl_region()));
            }
            layer.commit();
            self.screens.push(Screen {
                geom,
                layer,
                viewport,
                sel_surface,
                sel_sub,
                sel_viewport,
                base: None,
                sel: None,
                configured: false,
                waiting_frame: false,
                dirty: false,
            });
        }
        if self.screens.is_empty() {
            return Err("none of the screens could show the selection overlay".into());
        }
        Ok(())
    }

    fn screen_of(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.screens
            .iter()
            .position(|s| s.layer.wl_surface() == surface)
    }

    /// The current selection, rounded outwards to whole logical units.
    fn selection(&self) -> Option<Rect> {
        let (a, p) = (self.anchor?, self.pos?);
        let x0 = a.0.min(p.0).floor() as i32;
        let y0 = a.1.min(p.1).floor() as i32;
        let x1 = a.0.max(p.0).ceil() as i32;
        let y1 = a.1.max(p.1).ceil() as i32;
        Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    fn finish(&mut self) {
        let Some(sel) = self.selection() else { return };
        // A click (no real drag) takes the whole screen under the pointer.
        let rect = if sel.w < 3 && sel.h < 3 {
            match self.screens.iter().find(|s| s.geom.contains(sel.x, sel.y)) {
                Some(s) => s.geom,
                None => return,
            }
        } else if sel.is_empty() {
            // A perfectly straight drag has no area: start over.
            self.anchor = None;
            self.mark_all_dirty();
            return;
        } else {
            sel
        };
        // The first result wins (an Escape in the same batch stays a cancel).
        self.result.get_or_insert(Ok(Some((rect, self.mods))));
    }

    fn cancel(&mut self) {
        self.result.get_or_insert(Ok(None));
    }

    fn fail(&mut self, e: String) {
        self.result = Some(Err(e));
    }

    fn mark_all_dirty(&mut self) {
        for s in &mut self.screens {
            s.dirty = true;
        }
    }

    fn redraw(&mut self, qh: &QueueHandle<Self>) {
        for i in 0..self.screens.len() {
            let s = &self.screens[i];
            if s.configured
                && s.dirty
                && !s.waiting_frame
                && self.result.is_none()
                && let Err(e) = self.draw_selection(i, qh)
            {
                self.fail(e);
            }
        }
    }

    /// The frozen, dimmed screen. Drawn once per configure.
    fn draw_base(&mut self, i: usize, size: (u32, u32)) -> Result<(), String> {
        let frame = Rc::clone(&self.frame);
        let geom = self.screens[i].geom;
        let (fx, fy, fw, fh) = frame.pixel_rect(&geom);
        if fw == 0 || fh == 0 {
            return Err("a screen lies outside the captured frame".into());
        }
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                fw as i32,
                fh as i32,
                fw as i32 * 4,
                wl_shm::Format::Argb8888,
            )
            .map_err(|e| format!("can't allocate overlay memory: {e}"))?;
        let keep = 1.0 - self.style.dim.clamp(0.0, 0.9);
        let k = (keep * 256.0) as u32;
        for y in 0..fh {
            let src = &frame.image.as_raw()
                [((fy + y) as usize * frame.image.width() as usize + fx as usize) * 4..]
                [..fw as usize * 4];
            let dst = &mut canvas[(y * fw * 4) as usize..][..fw as usize * 4];
            for (d, s) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<4>().0)
            {
                // Opaque: gaps in the frame show as black.
                let a = s[3] as u32;
                let c = |v: u8| (((v as u32 * a / 255) * k) >> 8) as u8;
                d.copy_from_slice(&[c(s[2]), c(s[1]), c(s[0]), 255]);
            }
        }
        let s = &mut self.screens[i];
        let surface = s.layer.wl_surface();
        buffer
            .attach_to(surface)
            .map_err(|e| format!("overlay buffer error: {e}"))?;
        s.viewport.set_destination(size.0 as i32, size.1 as i32);
        surface.damage_buffer(0, 0, fw as i32, fh as i32);
        s.base = Some(buffer);
        s.dirty = true;
        Ok(())
    }

    /// The selection: undimmed frame pixels inside a rounded accent border.
    fn draw_selection(&mut self, i: usize, qh: &QueueHandle<Self>) -> Result<(), String> {
        let frame = Rc::clone(&self.frame);
        let geom = self.screens[i].geom;
        let sel = self.selection().filter(|r| !r.is_empty());
        let part = sel.and_then(|r| r.intersect(&geom));

        match (sel, part) {
            (Some(sel), Some(part)) => {
                let (fx, fy, fw, fh) = frame.pixel_rect(&part);
                if fw == 0 || fh == 0 {
                    return Ok(());
                }
                let (buffer, canvas) = self
                    .pool
                    .create_buffer(
                        fw as i32,
                        fh as i32,
                        fw as i32 * 4,
                        wl_shm::Format::Argb8888,
                    )
                    .map_err(|e| format!("can't allocate overlay memory: {e}"))?;
                let shape = RoundRect::new(&frame, &sel);
                let accent = self.style.accent;
                let img = frame.image.as_raw();
                let iw = frame.image.width() as usize;
                for y in 0..fh {
                    let gy = (fy + y) as f64 + 0.5;
                    let row = &img[((fy + y) as usize * iw + fx as usize) * 4..][..fw as usize * 4];
                    let dst = &mut canvas[(y * fw * 4) as usize..][..fw as usize * 4];
                    for (x, (d, s)) in dst
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(row.as_chunks::<4>().0)
                        .enumerate()
                    {
                        let gx = (fx as usize + x) as f64 + 0.5;
                        let (inner, border) = shape.coverage(gx, gy);
                        let a = s[3] as f64 / 255.0;
                        // Premultiplied ARGB, little-endian: B, G, R, A.
                        let mix = |c: u8, acc: u8| {
                            (c as f64 * a * inner + acc as f64 * border)
                                .round()
                                .min(255.0) as u8
                        };
                        d.copy_from_slice(&[
                            mix(s[2], accent[2]),
                            mix(s[1], accent[1]),
                            mix(s[0], accent[0]),
                            ((inner + border) * 255.0).round().min(255.0) as u8,
                        ]);
                    }
                }
                let s = &mut self.screens[i];
                s.sel_sub.set_position(part.x - geom.x, part.y - geom.y);
                s.sel_viewport.set_destination(part.w, part.h);
                buffer
                    .attach_to(&s.sel_surface)
                    .map_err(|e| format!("overlay buffer error: {e}"))?;
                s.sel_surface.damage_buffer(0, 0, fw as i32, fh as i32);
                s.sel = Some(buffer);
            }
            _ => {
                let s = &mut self.screens[i];
                s.sel_surface.attach(None, 0, 0);
                s.sel = None;
            }
        }
        let s = &mut self.screens[i];
        // The subsurface is synchronised: its state lands with the parent's
        // commit, together with the new position.
        s.sel_surface.commit();
        let surface = s.layer.wl_surface();
        surface.frame(qh, surface.clone());
        s.layer.commit();
        s.dirty = false;
        s.waiting_frame = true;
        Ok(())
    }

    fn destroy(&mut self) {
        for s in self.screens.drain(..) {
            s.sel_viewport.destroy();
            s.sel_sub.destroy();
            s.sel_surface.destroy();
            s.viewport.destroy();
            // LayerSurface destroys itself and its wl_surface on drop.
        }
        if let Some(d) = self.shape_device.take() {
            d.destroy();
        }
        if let Some(k) = self.keyboard.take() {
            k.release();
        }
        if let Some(p) = self.pointer.take() {
            p.release();
        }
    }
}

/// The selection's rounded rectangle in frame pixels, as a signed distance
/// field, for antialiased corners and border.
struct RoundRect {
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    r: f64,
    bw: f64,
}

impl RoundRect {
    fn new(frame: &Frame, sel: &Rect) -> RoundRect {
        let s = frame.scale;
        let x0 = (sel.x - frame.bounds.x) as f64 * s;
        let y0 = (sel.y - frame.bounds.y) as f64 * s;
        let (w, h) = (sel.w as f64 * s, sel.h as f64 * s);
        RoundRect {
            cx: x0 + w / 2.0,
            cy: y0 + h / 2.0,
            hw: w / 2.0,
            hh: h / 2.0,
            r: (RADIUS * s).min(w / 2.0).min(h / 2.0),
            bw: (BORDER * s).round().max(1.0),
        }
    }

    /// (coverage of the inside, coverage of the border) at a pixel centre.
    fn coverage(&self, x: f64, y: f64) -> (f64, f64) {
        let qx = (x - self.cx).abs() - (self.hw - self.r);
        let qy = (y - self.cy).abs() - (self.hh - self.r);
        let outside = qx.max(0.0).hypot(qy.max(0.0));
        let d = outside + qx.max(qy).min(0.0) - self.r;
        let outer = (0.5 - d).clamp(0.0, 1.0);
        let inner = (0.5 - (d + self.bw)).clamp(0.0, 1.0);
        (inner, outer - inner)
    }
}

impl CompositorHandler for Overlay {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(i) = self.screen_of(surface) {
            self.screens[i].waiting_frame = false;
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

impl OutputHandler for Overlay {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // The compositor took the overlay away (output unplugged, session
        // locked): give up quietly, as if cancelled.
        self.cancel();
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        cfg: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.screens.iter().position(|s| &s.layer == layer) else {
            return;
        };
        let geom = self.screens[i].geom;
        let size = match cfg.new_size {
            (0, 0) => (geom.w as u32, geom.h as u32),
            s => s,
        };
        if self.screens[i].base.is_none() {
            if let Err(e) = self.draw_base(i, size) {
                self.fail(e);
                return;
            }
        } else {
            self.screens[i]
                .viewport
                .set_destination(size.0 as i32, size.1 as i32);
        }
        self.screens[i].configured = true;
        self.screens[i].waiting_frame = false;
        self.screens[i].dirty = true;
    }
}

impl SeatHandler for Overlay {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if cap == Capability::Pointer
            && self.pointer.is_none()
            && let Ok(p) = self.seat_state.get_pointer(qh, &seat)
        {
            self.shape_device = self
                .cursor_shape
                .as_ref()
                .map(|m| m.get_shape_device(&p, qh));
            self.pointer = Some(p);
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
        // Dropped, not released: release needs wl_seat v3+.
        if cap == Capability::Keyboard {
            self.keyboard = None;
        }
        if cap == Capability::Pointer {
            if let Some(d) = self.shape_device.take() {
                d.destroy();
            }
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Overlay {
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
            self.cancel();
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        m: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.mods = Mods {
            ctrl: m.ctrl,
            alt: m.alt,
        };
    }
}

impl PointerHandler for Overlay {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for ev in events {
            let Some(i) = self.screen_of(&ev.surface) else {
                continue;
            };
            let g = self.screens[i].geom;
            let global = (g.x as f64 + ev.position.0, g.y as f64 + ev.position.1);
            match ev.kind {
                PointerEventKind::Enter { serial } => {
                    self.pos = Some(global);
                    if let Some(d) = &self.shape_device {
                        d.set_shape(serial, Shape::Crosshair);
                    }
                }
                PointerEventKind::Motion { .. } => {
                    self.pos = Some(global);
                    if self.anchor.is_some() {
                        self.mark_all_dirty();
                    }
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => {
                    self.pos = Some(global);
                    self.anchor = Some(global);
                    self.mark_all_dirty();
                }
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } => {
                    self.pos = Some(global);
                    self.finish();
                }
                PointerEventKind::Press {
                    button: BTN_RIGHT, ..
                } => self.cancel(),
                _ => {}
            }
        }
    }
}

impl ShmHandler for Overlay {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Overlay {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Overlay);
delegate_subcompositor!(Overlay);
delegate_output!(Overlay);
delegate_seat!(Overlay);
delegate_keyboard!(Overlay);
delegate_pointer!(Overlay);
delegate_layer!(Overlay);
delegate_shm!(Overlay);
delegate_registry!(Overlay);
delegate_noop!(Overlay: wp_viewporter::WpViewporter);
delegate_noop!(Overlay: wp_viewport::WpViewport);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::OutputGeom;

    fn frame(scale: f64) -> Frame {
        let bounds = Rect::new(0, 0, 100, 100);
        Frame {
            image: image::RgbaImage::new((100.0 * scale) as u32, (100.0 * scale) as u32),
            bounds,
            scale,
            outputs: vec![OutputGeom {
                name: "A".into(),
                logical: bounds,
            }],
        }
    }

    #[test]
    fn shape_inside_border_and_corners() {
        let f = frame(1.5);
        let sh = RoundRect::new(&f, &Rect::new(10, 10, 40, 20));
        // Centre: fully inside, no border.
        assert_eq!(sh.coverage(45.0, 30.0), (1.0, 0.0));
        // The first pixel row along the top edge is border (1.5 px -> 2 px).
        let (inner, border) = sh.coverage(45.0, 15.5);
        assert_eq!((inner, border), (0.0, 1.0));
        // The very corner pixel is outside the rounded corner.
        let (inner, border) = sh.coverage(15.5, 15.5);
        assert_eq!(inner + border, 0.0);
        // Far outside.
        assert_eq!(sh.coverage(0.5, 0.5), (0.0, 0.0));
    }

    #[test]
    fn tiny_selections_clamp_the_radius() {
        let f = frame(1.0);
        let sh = RoundRect::new(&f, &Rect::new(0, 0, 4, 4));
        assert_eq!(sh.r, 2.0);
        let (inner, border) = sh.coverage(2.0, 2.0);
        assert!(inner + border > 0.99);
    }
}
