//! The Wayland connection, the output layout (`xdg-output` logical geometry)
//! and the two Wayland capture backends: `ext-image-copy-capture-v1` and
//! `wlr-screencopy-unstable-v1`. Both copy into a `wl_shm` buffer we allocate
//! (memfd) and read back.

use std::fs::File;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use image::RgbaImage;
use memmap2::MmapMut;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1, ext_output_image_capture_source_manager_v1 as ext_src_mgr,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as ext_frame, ext_image_copy_capture_manager_v1 as ext_mgr,
    ext_image_copy_capture_session_v1 as ext_session,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as wlr_frame, zwlr_screencopy_manager_v1 as wlr_mgr,
};

use super::{CaptureError, MAX_FRAME_BYTES, OutputGeom, OutputShot, Rect, to_rgba};

const TIMEOUT: Duration = Duration::from_secs(5);

/// The Wayland connection, shared by capture, the overlay and the clipboard
/// check. Dropping it closes the socket.
pub struct Session {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    globals: GlobalList,
}

#[derive(Default)]
struct State {
    outputs: Vec<OutputData>,
    shm: Option<wl_shm::WlShm>,
    copy: CopyState,
}

struct OutputData {
    wl: wl_output::WlOutput,
    name: Option<String>,
    transform: wl_output::Transform,
    logical: Option<(i32, i32, i32, i32)>,
    xdg_pos: Option<(i32, i32)>,
    xdg_size: Option<(i32, i32)>,
    xdg_name: Option<String>,
}

/// Progress of the one capture in flight (we capture outputs one at a time).
#[derive(Default)]
struct CopyState {
    // wlr-screencopy
    shm_formats: Vec<(wl_shm::Format, u32, u32, u32)>,
    buffer_done: bool,
    y_invert: bool,
    // ext-image-copy-capture
    ext_size: Option<(u32, u32)>,
    ext_formats: Vec<wl_shm::Format>,
    ext_done: bool,
    ext_transform: Option<wl_output::Transform>,
    // both
    ready: bool,
    failed: Option<String>,
}

impl Session {
    pub fn connect() -> Result<Session, String> {
        let conn = Connection::connect_to_env().map_err(|e| {
            format!(
                "can't connect to the Wayland compositor ({e}); this tool needs a Wayland session"
            )
        })?;
        let (globals, queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| format!("can't read the compositor's interfaces: {e}"))?;
        let qh = queue.handle();
        let mut state = State::default();

        for g in globals.contents().clone_list() {
            if g.interface == wl_output::WlOutput::interface().name {
                let idx = state.outputs.len();
                let wl = globals.registry().bind::<wl_output::WlOutput, _, _>(
                    g.name,
                    g.version.min(4),
                    &qh,
                    idx,
                );
                state.outputs.push(OutputData {
                    wl,
                    name: None,
                    transform: wl_output::Transform::Normal,
                    logical: None,
                    xdg_pos: None,
                    xdg_size: None,
                    xdg_name: None,
                });
            }
        }
        state.shm = globals.bind(&qh, 1..=1, ()).ok();
        let xdg: zxdg_output_manager_v1::ZxdgOutputManagerV1 =
            globals.bind(&qh, 2..=3, ()).map_err(|_| {
                "the compositor doesn't describe its screen layout (no xdg-output)".to_string()
            })?;
        for (i, o) in state.outputs.iter().enumerate() {
            xdg.get_xdg_output(&o.wl, &qh, i);
        }
        let mut s = Session {
            conn,
            queue,
            state,
            globals,
        };
        s.roundtrip()?;
        s.roundtrip()?;
        for o in &mut s.state.outputs {
            if let (Some((x, y)), Some((w, h))) = (o.xdg_pos, o.xdg_size) {
                o.logical = Some((x, y, w, h));
            }
        }
        Ok(s)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    fn roundtrip(&mut self) -> Result<(), String> {
        self.queue
            .roundtrip(&mut self.state)
            .map(|_| ())
            .map_err(|e| format!("lost the Wayland connection: {e}"))
    }

    /// The outputs with a known layout, in compositor order.
    pub fn outputs(&self) -> Result<Vec<OutputGeom>, String> {
        Ok(self
            .state
            .outputs
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let (x, y, w, h) = o.logical?;
                (w > 0 && h > 0).then(|| OutputGeom {
                    name: o
                        .name
                        .clone()
                        .or_else(|| o.xdg_name.clone())
                        .unwrap_or_else(|| format!("output-{i}")),
                    logical: Rect::new(x, y, w, h),
                })
            })
            .collect())
    }

    /// Captures every output with ext-image-copy-capture, else wlr-screencopy.
    pub fn capture_outputs(
        &mut self,
        include_cursor: bool,
    ) -> Result<Vec<OutputShot>, CaptureError> {
        let qh = self.queue.handle();
        let shm = self
            .state
            .shm
            .clone()
            .ok_or_else(|| CaptureError::Unavailable("the compositor has no wl_shm".into()))?;
        let ext: Option<(
            ext_mgr::ExtImageCopyCaptureManagerV1,
            ext_src_mgr::ExtOutputImageCaptureSourceManagerV1,
        )> = match (
            self.globals.bind(&qh, 1..=1, ()),
            self.globals.bind(&qh, 1..=1, ()),
        ) {
            (Ok(m), Ok(s)) => Some((m, s)),
            _ => None,
        };
        let wlr: Option<wlr_mgr::ZwlrScreencopyManagerV1> = if ext.is_none() {
            self.globals.bind(&qh, 1..=3, ()).ok()
        } else {
            None
        };
        if ext.is_none() && wlr.is_none() {
            return Err(CaptureError::Unavailable(
                "no ext-image-copy-capture or wlr-screencopy".into(),
            ));
        }

        let geoms = self.outputs().map_err(CaptureError::Failed)?;
        let mut shots = Vec::new();
        for (i, geom) in geoms.into_iter().enumerate() {
            let Some(idx) = self.output_index(&geom.name, i) else {
                continue;
            };
            let wl = self.state.outputs[idx].wl.clone();
            let transform = self.state.outputs[idx].transform;
            let image = match (&ext, &wlr) {
                (Some((mgr, src)), _) => self.capture_ext(mgr, src, &wl, &shm, include_cursor)?,
                (None, Some(mgr)) => self.capture_wlr(mgr, &wl, &shm, include_cursor, transform)?,
                (None, None) => unreachable!(),
            };
            shots.push(OutputShot { geom, image });
        }
        if let Some((mgr, src)) = ext {
            mgr.destroy();
            src.destroy();
        }
        if let Some(mgr) = wlr {
            mgr.destroy();
        }
        Ok(shots)
    }

    fn output_index(&self, name: &str, fallback: usize) -> Option<usize> {
        self.state
            .outputs
            .iter()
            .position(|o| o.name.as_deref() == Some(name) || o.xdg_name.as_deref() == Some(name))
            .or_else(|| (fallback < self.state.outputs.len()).then_some(fallback))
    }

    fn capture_wlr(
        &mut self,
        mgr: &wlr_mgr::ZwlrScreencopyManagerV1,
        output: &wl_output::WlOutput,
        shm: &wl_shm::WlShm,
        include_cursor: bool,
        transform: wl_output::Transform,
    ) -> Result<RgbaImage, CaptureError> {
        let qh = self.queue.handle();
        self.state.copy = CopyState::default();
        let frame = mgr.capture_output(include_cursor as i32, output, &qh, ());
        let v3 = frame.version() >= 3;
        // v3 ends the buffer list with buffer_done; older versions send it
        // all at once, so one roundtrip is enough.
        self.dispatch_until(|c| {
            c.buffer_done || c.failed.is_some() || (!v3 && !c.shm_formats.is_empty())
        })?;
        let fmt = self
            .state
            .copy
            .shm_formats
            .iter()
            .copied()
            .find(|(f, ..)| pixel_layout(*f).is_some())
            .ok_or_else(|| {
                CaptureError::Failed(
                    "the compositor offers no screenshot pixel format this tool reads".into(),
                )
            })?;
        let (format, w, h, stride) = fmt;
        let buf = ShmBuffer::new(shm, &qh, w, h, stride, format).map_err(CaptureError::Failed)?;
        frame.copy(&buf.buffer);
        self.dispatch_until(|c| c.ready || c.failed.is_some())?;
        frame.destroy();
        let y_invert = self.state.copy.y_invert;
        let mut img = buf.read(format)?;
        if y_invert {
            image::imageops::flip_vertical_in_place(&mut img);
        }
        Ok(upright(img, transform))
    }

    fn capture_ext(
        &mut self,
        mgr: &ext_mgr::ExtImageCopyCaptureManagerV1,
        src_mgr: &ext_src_mgr::ExtOutputImageCaptureSourceManagerV1,
        output: &wl_output::WlOutput,
        shm: &wl_shm::WlShm,
        include_cursor: bool,
    ) -> Result<RgbaImage, CaptureError> {
        let qh = self.queue.handle();
        self.state.copy = CopyState::default();
        let source = src_mgr.create_source(output, &qh, ());
        let options = if include_cursor {
            ext_mgr::Options::PaintCursors
        } else {
            ext_mgr::Options::empty()
        };
        let session = mgr.create_session(&source, options, &qh, ());
        self.dispatch_until(|c| c.ext_done || c.failed.is_some())?;
        let (w, h) = self.state.copy.ext_size.ok_or_else(|| {
            CaptureError::Failed("the compositor didn't say the screen's size".into())
        })?;
        let format = self
            .state
            .copy
            .ext_formats
            .iter()
            .copied()
            .find(|f| pixel_layout(*f).is_some())
            .ok_or_else(|| {
                CaptureError::Failed(
                    "the compositor offers no screenshot pixel format this tool reads".into(),
                )
            })?;
        let buf = ShmBuffer::new(shm, &qh, w, h, w.saturating_mul(4), format)
            .map_err(CaptureError::Failed)?;
        let frame = session.create_frame(&qh, ());
        frame.attach_buffer(&buf.buffer);
        frame.damage_buffer(0, 0, w as i32, h as i32);
        frame.capture();
        let done = self.dispatch_until(|c| c.ready || c.failed.is_some());
        frame.destroy();
        session.destroy();
        source.destroy();
        done?;
        let transform = self
            .state
            .copy
            .ext_transform
            .unwrap_or(wl_output::Transform::Normal);
        Ok(upright(buf.read(format)?, transform))
    }

    /// Dispatches until `done` holds, the capture fails, or TIMEOUT passes.
    fn dispatch_until(&mut self, done: impl Fn(&CopyState) -> bool) -> Result<(), CaptureError> {
        let deadline = Instant::now() + TIMEOUT;
        let lost = |e: &dyn std::fmt::Display| {
            CaptureError::Failed(format!("lost the Wayland connection: {e}"))
        };
        loop {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| lost(&e))?;
            if let Some(why) = &self.state.copy.failed {
                return Err(CaptureError::Failed(format!(
                    "the compositor couldn't capture the screen ({why})"
                )));
            }
            if done(&self.state.copy) {
                return Ok(());
            }
            self.queue.flush().map_err(|e| lost(&e))?;
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(CaptureError::Failed(
                    "the compositor didn't capture the screen in time".into(),
                ));
            }
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let ts = Timespec::try_from(left).unwrap_or(Timespec {
                tv_sec: 5,
                tv_nsec: 0,
            });
            let ready = {
                let fd = guard.connection_fd();
                let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                match poll(&mut fds, Some(&ts)) {
                    Ok(n) => n > 0,
                    Err(rustix::io::Errno::INTR) => false,
                    Err(e) => return Err(lost(&e)),
                }
            };
            if ready {
                guard.read().map_err(|e| lost(&e))?;
            }
        }
    }
}

/// (bgra byte order, ignore alpha) for the shm formats we can read.
fn pixel_layout(f: wl_shm::Format) -> Option<(bool, bool)> {
    match f {
        wl_shm::Format::Argb8888 => Some((true, false)),
        wl_shm::Format::Xrgb8888 => Some((true, true)),
        wl_shm::Format::Abgr8888 => Some((false, false)),
        wl_shm::Format::Xbgr8888 => Some((false, true)),
        _ => None,
    }
}

/// Undoes the output transform, so the image is the way the user sees it.
/// Untested on rotated outputs (none on AtlasOS' test hardware).
fn upright(img: RgbaImage, t: wl_output::Transform) -> RgbaImage {
    use image::imageops::{flip_horizontal, rotate90, rotate180, rotate270};
    use wl_output::Transform as T;
    match t {
        T::_90 => rotate90(&img),
        T::_180 => rotate180(&img),
        T::_270 => rotate270(&img),
        T::Flipped => flip_horizontal(&img),
        T::Flipped90 => rotate90(&flip_horizontal(&img)),
        T::Flipped180 => rotate180(&flip_horizontal(&img)),
        T::Flipped270 => rotate270(&flip_horizontal(&img)),
        _ => img,
    }
}

/// A one-off wl_shm buffer over a sealed-size memfd.
struct ShmBuffer {
    buffer: wl_buffer::WlBuffer,
    map: MmapMut,
    width: u32,
    height: u32,
    stride: u32,
}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        width: u32,
        height: u32,
        stride: u32,
        format: wl_shm::Format,
    ) -> Result<ShmBuffer, String> {
        let len = stride as usize * height as usize;
        if width == 0
            || height == 0
            || width > 16384
            || height > 16384
            || stride < width * 4
            || len > MAX_FRAME_BYTES
        {
            return Err(format!(
                "the compositor asked for an impossible screenshot buffer ({width}x{height})"
            ));
        }
        let fd = rustix::fs::memfd_create("atlasos-screenshot", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| format!("can't allocate screenshot memory: {e}"))?;
        rustix::fs::ftruncate(&fd, len as u64)
            .map_err(|e| format!("can't allocate screenshot memory: {e}"))?;
        let file = File::from(fd);
        // SAFETY: the memfd is private to us and the compositor, and the
        // compositor only writes into it before `ready`, after which we read.
        let map = unsafe { MmapMut::map_mut(&file) }
            .map_err(|e| format!("can't map screenshot memory: {e}"))?;
        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            qh,
            (),
        );
        pool.destroy();
        Ok(ShmBuffer {
            buffer,
            map,
            width,
            height,
            stride,
        })
    }

    fn read(&self, format: wl_shm::Format) -> Result<RgbaImage, CaptureError> {
        let (bgra, opaque) = pixel_layout(format).unwrap_or((true, true));
        to_rgba(
            &self.map,
            self.width,
            self.height,
            self.stride as usize,
            bgra,
            opaque,
            !opaque,
        )
        .map_err(CaptureError::Failed)
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Outputs plugged in after start are ignored: the frame is already taken.
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.get_mut(*idx) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                transform: WEnum::Value(t),
                ..
            } => o.transform = t,
            wl_output::Event::Name { name } => o.name = Some(name),
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.get_mut(*idx) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => o.xdg_pos = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                o.xdg_size = Some((width, height))
            }
            zxdg_output_v1::Event::Name { name } => o.xdg_name = Some(name),
            _ => {}
        }
    }
}

impl Dispatch<wlr_frame::ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &wlr_frame::ZwlrScreencopyFrameV1,
        event: wlr_frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let c = &mut state.copy;
        match event {
            wlr_frame::Event::Buffer {
                format: WEnum::Value(f),
                width,
                height,
                stride,
            } => c.shm_formats.push((f, width, height, stride)),
            wlr_frame::Event::BufferDone => c.buffer_done = true,
            wlr_frame::Event::Flags {
                flags: WEnum::Value(f),
            } => c.y_invert = f.contains(wlr_frame::Flags::YInvert),
            wlr_frame::Event::Ready { .. } => c.ready = true,
            wlr_frame::Event::Failed => c.failed = Some("wlr-screencopy failed".into()),
            _ => {}
        }
    }
}

impl Dispatch<ext_session::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_session::ExtImageCopyCaptureSessionV1,
        event: ext_session::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let c = &mut state.copy;
        match event {
            ext_session::Event::BufferSize { width, height } => c.ext_size = Some((width, height)),
            ext_session::Event::ShmFormat {
                format: WEnum::Value(f),
            } => c.ext_formats.push(f),
            ext_session::Event::Done => c.ext_done = true,
            ext_session::Event::Stopped => c.failed = Some("the capture session stopped".into()),
            _ => {}
        }
    }
}

impl Dispatch<ext_frame::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_frame::ExtImageCopyCaptureFrameV1,
        event: ext_frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let c = &mut state.copy;
        match event {
            ext_frame::Event::Transform {
                transform: WEnum::Value(t),
            } => c.ext_transform = Some(t),
            ext_frame::Event::Ready => c.ready = true,
            ext_frame::Event::Failed { reason } => {
                c.failed = Some(format!("ext-image-copy-capture failed: {reason:?}"))
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: zxdg_output_manager_v1::ZxdgOutputManagerV1);
delegate_noop!(State: wlr_mgr::ZwlrScreencopyManagerV1);
delegate_noop!(State: ext_mgr::ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ext_src_mgr::ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ext_image_capture_source_v1::ExtImageCaptureSourceV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upright_rotates_and_flips() {
        let img = RgbaImage::from_fn(2, 1, |x, _| image::Rgba([x as u8, 0, 0, 255]));
        assert_eq!(
            upright(img.clone(), wl_output::Transform::Normal).dimensions(),
            (2, 1)
        );
        assert_eq!(
            upright(img.clone(), wl_output::Transform::_90).dimensions(),
            (1, 2)
        );
        assert_eq!(
            upright(img.clone(), wl_output::Transform::Flipped)
                .get_pixel(0, 0)
                .0[0],
            1
        );
    }

    #[test]
    fn readable_formats() {
        assert_eq!(pixel_layout(wl_shm::Format::Xrgb8888), Some((true, true)));
        assert_eq!(pixel_layout(wl_shm::Format::Abgr8888), Some((false, false)));
        assert_eq!(pixel_layout(wl_shm::Format::Rgb565), None);
    }
}
