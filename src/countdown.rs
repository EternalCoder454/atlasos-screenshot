//! The wait before a delayed capture, with a small countdown on screen.
//!
//! A layer-shell surface at the top of the screen shows the seconds left. It
//! is destroyed, and the compositor has answered the destruction, before the
//! capture is taken, so it is never in the picture (KWin is also asked to
//! hide the caller's windows). Where the compositor has no layer-shell the
//! wait is silent: the delay still holds.

use std::time::{Duration, Instant};

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_surface};
use smithay_client_toolkit::reexports::client::{
    Connection, EventQueue, QueueHandle, delegate_noop,
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{
    wp_viewport, wp_viewporter,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    registry_handlers,
};
use wayland_client::protocol::wl_shm;

/// Logical size of the badge, and its distance from the top edge.
const SIZE: u32 = 96;
const MARGIN: i32 = 40;
/// The buffer is drawn at this multiple of the logical size, so the digits
/// stay sharp up to 2x screens.
const DENSITY: u32 = 2;
/// Time for the compositor to repaint without the badge, on compositors that
/// capture what was last painted (the Wayland capture protocols).
const SETTLE: Duration = Duration::from_millis(120);

/// Waits `secs` seconds, showing the seconds left. Never fails: a
/// compositor without layer-shell just gets a quiet wait.
pub fn wait(secs: u32, accent: [u8; 3]) {
    if secs == 0 {
        return;
    }
    let end = Instant::now() + Duration::from_secs(u64::from(secs));
    match Badge::run(end, accent) {
        Ok(()) => std::thread::sleep(SETTLE),
        Err(_) => {
            std::thread::sleep(end.saturating_duration_since(Instant::now()));
        }
    }
}

struct Badge {
    registry: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: Option<LayerSurface>,
    viewport: Option<wp_viewport::WpViewport>,
    configured: bool,
    closed: bool,
}

impl Badge {
    fn run(end: Instant, accent: [u8; 3]) -> Result<(), String> {
        let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
        let (globals, mut queue) =
            registry_queue_init::<Badge>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| e.to_string())?;
        let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| e.to_string())?;
        let shm = Shm::bind(&globals, &qh).map_err(|e| e.to_string())?;
        let viewporter: wp_viewporter::WpViewporter =
            globals.bind(&qh, 1..=1, ()).map_err(|e| e.to_string())?;
        let pool = SlotPool::new((SIZE * DENSITY * SIZE * DENSITY * 4) as usize, &shm)
            .map_err(|e| e.to_string())?;

        let surface = compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some("telamon-screenshot-countdown"),
            None,
        );
        layer.set_anchor(Anchor::TOP);
        layer.set_size(SIZE, SIZE);
        layer.set_margin(MARGIN, 0, 0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let viewport = viewporter.get_viewport(layer.wl_surface(), &qh, ());
        viewport.set_destination(SIZE as i32, SIZE as i32);
        layer.commit();

        let mut b = Badge {
            registry: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            shm,
            pool,
            layer: Some(layer),
            viewport: Some(viewport),
            configured: false,
            closed: false,
        };
        let result = b.count_down(&conn, &mut queue, end, accent);
        // Gone from the screen, and the compositor knows it, before the
        // capture: the roundtrip returns after it handled the destruction.
        b.destroy();
        let _ = queue.roundtrip(&mut b);
        result
    }

    fn count_down(
        &mut self,
        conn: &Connection,
        queue: &mut EventQueue<Badge>,
        end: Instant,
        accent: [u8; 3],
    ) -> Result<(), String> {
        let qh = queue.handle();
        let lost = |e: &dyn std::fmt::Display| format!("lost the Wayland connection: {e}");
        let start = Instant::now();
        let mut shown = 0u32;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            if self.closed {
                // The compositor took the badge away: keep waiting, silently.
                std::thread::sleep(left);
                return Ok(());
            }
            let n = left.as_secs_f64().ceil() as u32;
            if self.configured && n != shown {
                self.draw(n, accent, &qh)?;
                shown = n;
            } else if !self.configured && start.elapsed() > Duration::from_secs(2) {
                return Err("the countdown didn't appear".into());
            }
            queue.flush().map_err(|e| lost(&e))?;
            queue.dispatch_pending(self).map_err(|e| lost(&e))?;
            // Sleep until the next second flips, or an event arrives.
            let next = left - Duration::from_secs(u64::from(n.saturating_sub(1)));
            let wait = next
                .min(Duration::from_millis(250))
                .max(Duration::from_millis(5));
            if let Some(guard) = queue.prepare_read() {
                let ts = rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: wait.subsec_nanos() as _,
                };
                let ready = {
                    let fd = guard.connection_fd();
                    let mut fds = [rustix::event::PollFd::new(
                        &fd,
                        rustix::event::PollFlags::IN,
                    )];
                    matches!(rustix::event::poll(&mut fds, Some(&ts)), Ok(n) if n > 0)
                };
                if ready {
                    guard.read().map_err(|e| lost(&e))?;
                } else {
                    drop(guard);
                }
            }
            queue.dispatch_pending(self).map_err(|e| lost(&e))?;
            let _ = conn;
        }
    }

    fn draw(&mut self, n: u32, accent: [u8; 3], qh: &QueueHandle<Self>) -> Result<(), String> {
        let Some(layer) = &self.layer else {
            return Ok(());
        };
        let side = (SIZE * DENSITY) as i32;
        let (buffer, canvas) = self
            .pool
            .create_buffer(side, side, side * 4, wl_shm::Format::Argb8888)
            .map_err(|e| e.to_string())?;
        paint_badge(canvas, side as usize, n, accent);
        let surface = layer.wl_surface();
        buffer.attach_to(surface).map_err(|e| e.to_string())?;
        surface.damage_buffer(0, 0, side, side);
        surface.commit();
        let _ = qh;
        Ok(())
    }

    fn destroy(&mut self) {
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        // The layer surface and its wl_surface are destroyed on drop.
        self.layer = None;
    }
}

/// 5x7 digits.
const DIGITS: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ],
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ],
    [
        0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
    ],
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ],
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ],
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ],
];

/// A rounded dark badge with the number in the accent colour, premultiplied
/// ARGB8888 (B, G, R, A in memory) in a square `side` pixels wide.
pub fn paint_badge(canvas: &mut [u8], side: usize, n: u32, accent: [u8; 3]) {
    let radius = side as f32 * 0.24;
    let bg = [0x1c_u8, 0x1c, 0x1e];
    let alpha = 0.92_f32;
    for y in 0..side {
        for x in 0..side {
            let cov = corner_coverage(x, y, side, radius);
            let a = (alpha * cov * 255.0).round() as u8;
            let px = [
                (bg[2] as f32 * alpha * cov).round() as u8,
                (bg[1] as f32 * alpha * cov).round() as u8,
                (bg[0] as f32 * alpha * cov).round() as u8,
                a,
            ];
            canvas[(y * side + x) * 4..][..4].copy_from_slice(&px);
        }
    }
    let text = n.min(999).to_string();
    let digits = text.len();
    // A cell is 5 columns by 7 rows, with one column between digits.
    let units = digits * 6 - 1;
    let cell = (side as f32 * 0.50 / 7.0)
        .min(side as f32 * 0.80 / units as f32)
        .floor()
        .max(1.0) as usize;
    let width = units * cell;
    let (x0, y0) = ((side - width) / 2, (side - 7 * cell) / 2);
    for (i, ch) in text.bytes().enumerate() {
        let glyph = &DIGITS[(ch - b'0') as usize];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits >> (4 - col) & 1 == 0 {
                    continue;
                }
                let (gx, gy) = (x0 + (i * 6 + col) * cell, y0 + row * cell);
                for dy in 0..cell {
                    for dx in 0..cell {
                        let o = ((gy + dy) * side + gx + dx) * 4;
                        canvas[o..o + 4].copy_from_slice(&[accent[2], accent[1], accent[0], 255]);
                    }
                }
            }
        }
    }
}

/// How much of pixel (x, y) is inside a `side` square with rounded corners.
fn corner_coverage(x: usize, y: usize, side: usize, r: f32) -> f32 {
    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
    let s = side as f32;
    let cx = if fx < r {
        r
    } else if fx > s - r {
        s - r
    } else {
        return 1.0;
    };
    let cy = if fy < r {
        r
    } else if fy > s - r {
        s - r
    } else {
        return 1.0;
    };
    let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

impl CompositorHandler for Badge {
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
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

impl OutputHandler for Badge {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Badge {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.closed = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.configured = true;
    }
}

impl ShmHandler for Badge {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Badge {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_compositor!(Badge);
delegate_output!(Badge);
delegate_layer!(Badge);
delegate_shm!(Badge);
delegate_registry!(Badge);
delegate_noop!(Badge: wp_viewporter::WpViewporter);
delegate_noop!(Badge: wp_viewport::WpViewport);

#[cfg(test)]
mod tests {
    use super::*;

    fn px(canvas: &[u8], side: usize, x: usize, y: usize) -> [u8; 4] {
        canvas[(y * side + x) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn badge_is_round_and_shows_the_digit() {
        let side = 96;
        let mut c = vec![0u8; side * side * 4];
        paint_badge(&mut c, side, 3, [0x8A, 0x7A, 0xF4]);
        // The corner is see-through, the middle of the edge is not.
        assert_eq!(px(&c, side, 0, 0)[3], 0);
        assert!(px(&c, side, side / 2, 1)[3] > 200);
        // Some pixels are the accent (B, G, R, A in memory).
        let accent = [0xF4, 0x7A, 0x8A, 255];
        assert!(c.as_chunks::<4>().0.contains(&accent));
        // Premultiplied: no channel exceeds alpha.
        assert!(
            c.as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
        );
        // Two and three digits fit inside the badge (no panic, still painted).
        for n in [10, 59, 600] {
            let mut c = vec![0u8; side * side * 4];
            paint_badge(&mut c, side, n, [1, 2, 3]);
            assert!(c.as_chunks::<4>().0.contains(&[3, 2, 1, 255]));
        }
    }

    #[test]
    fn zero_seconds_is_no_wait() {
        let t = Instant::now();
        wait(0, [0, 0, 0]);
        assert!(t.elapsed() < Duration::from_millis(50));
    }
}
