//! The selection overlay: one wlr-layer-shell surface per output, showing the
//! frozen frame dimmed, with the selection undimmed inside a rounded border
//! in the Telamon accent. No toolkit; SHM buffers drawn by hand (`paint`).
//!
//! - Drag with the left button to select; release to finish. The modifiers
//!   held at release pick the mode (Ctrl: text, Alt: redact).
//! - A click without a drag selects the whole screen under the pointer.
//! - Escape or the right button cancels.
//!
//! Buffers hold frame pixels 1:1 and `wp_viewporter` maps them onto the
//! output's logical size, so the frozen image is pixel-exact at any scale.
//! Redraws follow frame callbacks, so a fast mouse never queues stale frames:
//! all the motion since the last frame becomes one redraw. Each screen
//! reuses two (at most three) buffers, repaints only the strips the
//! selection's edges moved across, and damages only those.

mod paint;

use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{
    wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, QueueHandle, delegate_noop};
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
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, registry_handlers,
};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::protocol::wl_shm;

use crate::capture::{Frame, Rect};
use crate::watchdog::Watchdog;
use paint::{Pixels, Rects, Shows};

/// Setup and teardown waits on the compositor (not the user).
const SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
/// Buffers per screen: one on screen, one being drawn, and a spare for a
/// compositor slow to release.
const MAX_BUFFERS: usize = 3;

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
        layer_shell,
        shm,
        viewporter,
        pool,
        frame,
        style,
        hold: entry_hold(),
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
        let until = ov.next_reveal();
        dispatch(&mut queue, &mut ov, until).map_err(|e| lost(&e))?;
        if setup.is_some() && ov.screens.iter().any(|s| s.configured) {
            setup = None;
        }
        ov.redraw(&qh);
        if ov.result.is_none() && ov.screens.iter().any(|s| s.slots.len() == 1) {
            // Send the first frames on their way before the extra work.
            queue.flush().map_err(|e| lost(&e))?;
            if let Err(e) = ov.fill_spares() {
                ov.fail(e);
            }
        }
    }
    drop(setup);
    // Tear the overlay down and make sure it's gone from the screen before
    // the caller spends time on OCR or the clipboard.
    let result = ov.result.take().unwrap_or(Ok(None));
    ov.blank_all();
    ov.destroy();
    let _teardown = Watchdog::arm(SETUP_TIMEOUT, "the selection overlay didn't close");
    queue.roundtrip(&mut ov).map_err(|e| lost(&e))?;
    result
}

/// How long KWin's open animation (the Scale effect: a fade and a zoom, 160
/// ms at the default animation speed) has to play out, from the first frame
/// the compositor shows of the window, before the window can be replaced
/// without the animation showing; see `show_blank`.
const KWIN_OPEN_ANIMATION: Duration = Duration::from_millis(180);

/// The longest a screen waits for the compositor's first frame callback
/// (the sign that the transparent pixel is up and the animation running)
/// before the hold starts anyway.
const FIRST_FRAME_WAIT: Duration = Duration::from_millis(250);

/// Zero on any compositor but KWin, or when KWin has no such effect loaded
/// or animations are off. `TELAMON_HOLD_MS` overrides it (for tests).
fn entry_hold() -> Duration {
    if let Some(ms) = std::env::var("TELAMON_HOLD_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_millis(ms.min(1000));
    }
    if !kwin_animates_new_windows() {
        return Duration::ZERO;
    }
    KWIN_OPEN_ANIMATION.mul_f64(animation_factor())
}

/// Asks KWin which effects are loaded. Anything but "KWin isn't there, or
/// has neither effect" counts as yes: a pause is cheaper than the glitch.
fn kwin_animates_new_windows() -> bool {
    let call = || -> zbus::Result<Vec<String>> {
        let conn = zbus::blocking::connection::Builder::session()?
            .method_timeout(Duration::from_millis(500))
            .build()?;
        let reply = conn.call_method(
            Some("org.kde.KWin"),
            "/Effects",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.kde.kwin.Effects", "loadedEffects"),
        )?;
        let v: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
        Vec::<String>::try_from(v).map_err(zbus::Error::from)
    };
    match call() {
        Ok(effects) => effects.iter().any(|e| e == "scale" || e == "fade"),
        Err(zbus::Error::MethodError(name, ..)) => !matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ),
        // No session bus: no KWin to talk to either.
        Err(zbus::Error::InputOutput(_)) => false,
        Err(_) => true,
    }
}

/// Plasma's "animation speed" (`AnimationDurationFactor` in kdeglobals):
/// 1 is normal, larger is slower, 0 turns animations off.
fn animation_factor() -> f64 {
    use std::io::Read;
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")));
    let Some(path) = dir.map(|d| d.join("kdeglobals")) else {
        return 1.0;
    };
    let mut text = String::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(65536).read_to_string(&mut text));
    if read.is_err() {
        return 1.0;
    }
    let mut in_kde = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_kde = line == "[KDE]";
        } else if in_kde && let Some(v) = line.strip_prefix("AnimationDurationFactor=") {
            return v
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .map_or(1.0, |f| f.clamp(0.0, 4.0));
        }
    }
    1.0
}

/// Reads and dispatches events, waiting at most until `until`.
fn dispatch(
    queue: &mut EventQueue<Overlay>,
    ov: &mut Overlay,
    until: Option<Instant>,
) -> Result<(), String> {
    queue.dispatch_pending(ov).map_err(|e| e.to_string())?;
    queue.flush().map_err(|e| e.to_string())?;
    if let Some(guard) = queue.prepare_read() {
        let left = until.map(|t| t.saturating_duration_since(Instant::now()));
        let ts = left.and_then(|d| Timespec::try_from(d).ok());
        let fd = guard.connection_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let polled = poll(&mut fds, ts.as_ref());
        match polled {
            Ok(n) if n > 0 => {
                guard.read().map_err(|e| e.to_string())?;
            }
            Ok(_) | Err(rustix::io::Errno::INTR) => drop(guard),
            Err(e) => return Err(e.to_string()),
        }
        queue.dispatch_pending(ov).map_err(|e| e.to_string())?;
    }
    Ok(())
}

struct Screen {
    /// Logical geometry, from the frame (what the image shows).
    geom: Rect,
    layer: LayerSurface,
    viewport: wp_viewport::WpViewport,
    /// This screen's dimmed frame, made at the first configure.
    pixels: Option<Pixels>,
    slots: Vec<Slot>,
    /// The selection the surface shows; `None` until the next commit must
    /// damage everything (first commit, new size).
    shown: Option<Option<Rect>>,
    configured: bool,
    waiting_frame: bool,
    dirty: bool,
    /// Until then the screen shows a transparent pixel (see `Overlay::hold`):
    /// `hold` after the compositor's first frame callback for it, or a
    /// fixed time after the configure if none comes.
    reveal: Option<Instant>,
    /// The transparent pixel's first frame callback is still to come.
    blank_frame_due: bool,
    /// The transparent pixel, once attached.
    blank: Option<Buffer>,
}

fn new_buffer<'p>(pool: &'p mut SlotPool, px: &Pixels) -> Result<(Buffer, &'p mut [u8]), String> {
    let (w, h) = (px.width as i32, px.height as i32);
    pool.create_buffer(w, h, w * 4, wl_shm::Format::Xrgb8888)
        .map_err(|e| format!("can't allocate overlay memory: {e}"))
}

/// A reusable SHM buffer and what it shows.
struct Slot {
    buffer: Buffer,
    shows: Shows,
}

struct Overlay {
    registry: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    cursor_shape: Option<CursorShapeManager>,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    viewporter: wp_viewporter::WpViewporter,
    pool: SlotPool,
    frame: Rc<Frame>,
    style: Style,
    /// How long a new screen shows nothing, for the compositor's open
    /// animation to play out unseen.
    hold: Duration,
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
                Some("telamon-screenshot"),
                Some(&output),
            );
            layer.set_anchor(Anchor::all());
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.set_size(0, 0);
            let viewport = self.viewporter.get_viewport(layer.wl_surface(), qh, ());
            layer.commit();
            self.screens.push(Screen {
                geom,
                layer,
                viewport,
                pixels: None,
                slots: Vec::new(),
                shown: None,
                configured: false,
                waiting_frame: false,
                dirty: false,
                reveal: None,
                blank_frame_due: false,
                blank: None,
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

    /// When the first screen still on hold is due to show its frame.
    fn next_reveal(&self) -> Option<Instant> {
        self.screens.iter().filter_map(|s| s.reveal).min()
    }

    /// Shows screen `i` as one transparent pixel stretched over it. KWin
    /// plays its open and close animations (a fade and a zoom) on whatever a
    /// new window shows; on the frozen frame that reads as a second, smaller
    /// copy of the screen fading in over the live one. On a transparent
    /// window nothing of it can be seen.
    fn show_blank(&mut self, i: usize, qh: Option<&QueueHandle<Self>>) -> Result<(), String> {
        let Overlay { pool, screens, .. } = self;
        let s = &mut screens[i];
        if s.blank.is_some() && s.shown.is_none() {
            return Ok(());
        }
        let (buffer, canvas) = pool
            .create_buffer(1, 1, 4, wl_shm::Format::Argb8888)
            .map_err(|e| format!("can't allocate overlay memory: {e}"))?;
        canvas[..4].fill(0);
        let surface = s.layer.wl_surface();
        buffer
            .attach_to(surface)
            .map_err(|e| format!("overlay buffer error: {e}"))?;
        surface.damage_buffer(0, 0, 1, 1);
        if let Some(qh) = qh {
            surface.frame(qh, surface.clone());
            s.blank_frame_due = s.reveal.is_some();
        }
        s.layer.commit();
        s.blank = Some(buffer);
        s.shown = None;
        Ok(())
    }

    /// Blanks every screen that has shown its frame, so that the compositor's
    /// close animation has nothing to show either.
    fn blank_all(&mut self) {
        for i in 0..self.screens.len() {
            let s = &self.screens[i];
            if s.configured && s.pixels.is_some() && s.reveal.is_none() {
                let _ = self.show_blank(i, None);
            }
        }
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
                && let Err(e) = self.draw(i, qh)
            {
                self.fail(e);
            }
        }
    }

    /// Brings screen `i` up to date with the selection, in a buffer the
    /// compositor has released: repaints only what that buffer shows
    /// differently, and damages only what changed since the last commit.
    /// Commits nothing when the change doesn't touch this screen.
    fn draw(&mut self, i: usize, qh: &QueueHandle<Self>) -> Result<(), String> {
        if let Some(t) = self.screens[i].reveal {
            if Instant::now() < t {
                return self.show_blank(i, Some(qh));
            }
            self.screens[i].reveal = None;
            self.screens[i].blank_frame_due = false;
        }
        let want = self.selection().filter(|r| !r.is_empty());
        let Overlay {
            pool,
            screens,
            frame,
            ..
        } = self;
        let s = &mut screens[i];
        let Some(px) = &s.pixels else {
            return Ok(());
        };
        let damage = match s.shown {
            Some(prev) => px.changed(prev, want),
            None => Rects::one(px.bounds()),
        };
        if damage.is_empty() {
            s.shown = Some(want);
            s.dirty = false;
            return Ok(());
        }
        // Usually the buffer before last: the compositor releases a buffer
        // once the next one replaces it.
        let free = s.slots.iter().position(|b| b.buffer.canvas(pool).is_some());
        let n = match free {
            Some(n) => n,
            None if s.slots.len() < MAX_BUFFERS => {
                let (buffer, _) = new_buffer(pool, px)?;
                s.slots.push(Slot {
                    buffer,
                    shows: Shows::Garbage,
                });
                s.slots.len() - 1
            }
            // All in use: the release that frees one wakes the loop, which
            // tries again (the screen stays dirty).
            None => return Ok(()),
        };
        let slot = &mut s.slots[n];
        let canvas = slot
            .buffer
            .canvas(pool)
            .ok_or("overlay buffer error: busy")?;
        px.update(frame, canvas, slot.shows, want);
        slot.shows = Shows::Selection(want);
        let surface = s.layer.wl_surface();
        slot.buffer
            .attach_to(surface)
            .map_err(|e| format!("overlay buffer error: {e}"))?;
        for r in damage.iter() {
            surface.damage_buffer(r.x, r.y, r.w, r.h);
        }
        surface.frame(qh, surface.clone());
        s.layer.commit();
        s.shown = Some(want);
        s.dirty = false;
        s.waiting_frame = true;
        Ok(())
    }

    /// Gives each screen its second buffer, filled, once the first one is
    /// on screen and before the user starts dragging, so the first frames
    /// of a drag don't pay for a whole-screen copy.
    fn fill_spares(&mut self) -> Result<(), String> {
        let Overlay {
            pool,
            screens,
            frame,
            ..
        } = self;
        for s in screens {
            let (Some(px), 1, Some(shown)) = (&s.pixels, s.slots.len(), s.shown) else {
                continue;
            };
            let (buffer, canvas) = new_buffer(pool, px)?;
            px.update(frame, canvas, Shows::Garbage, shown);
            s.slots.push(Slot {
                buffer,
                shows: Shows::Selection(shown),
            });
        }
        Ok(())
    }

    fn destroy(&mut self) {
        for s in self.screens.drain(..) {
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
            let hold = self.hold;
            let s = &mut self.screens[i];
            s.waiting_frame = false;
            if s.blank_frame_due {
                // The compositor has shown the transparent pixel: its open
                // animation started about now.
                s.blank_frame_due = false;
                s.reveal = Some(Instant::now() + hold);
            }
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
        let s = &mut self.screens[i];
        let size = match cfg.new_size {
            (0, 0) => (s.geom.w as u32, s.geom.h as u32),
            n => n,
        };
        if s.pixels.is_none() {
            // The frozen frame, dimmed, once: redraws copy from it.
            s.pixels = Pixels::new(&self.frame, &s.geom, self.style.dim, self.style.accent);
            if !self.hold.is_zero() {
                s.reveal = Some(Instant::now() + self.hold + FIRST_FRAME_WAIT);
            }
            if s.pixels.is_none() {
                self.fail("a screen lies outside the captured frame".into());
                return;
            }
        }
        s.viewport.set_destination(size.0 as i32, size.1 as i32);
        // The next commit (from `redraw`, right after this event) carries
        // the size, with everything damaged.
        s.shown = None;
        s.configured = true;
        s.waiting_frame = false;
        s.dirty = true;
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
delegate_output!(Overlay);
delegate_seat!(Overlay);
delegate_keyboard!(Overlay);
delegate_pointer!(Overlay);
delegate_layer!(Overlay);
delegate_shm!(Overlay);
delegate_registry!(Overlay);
delegate_noop!(Overlay: wp_viewporter::WpViewporter);
delegate_noop!(Overlay: wp_viewport::WpViewport);
