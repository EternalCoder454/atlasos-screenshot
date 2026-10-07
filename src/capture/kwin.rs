//! KWin's `org.kde.KWin.ScreenShot2`: `CaptureWorkspace`, and `CaptureScreen`
//! for one output in its own pixels.
//!
//! We pass the write end of a pipe; KWin replies with the image's metadata
//! (`type` "raw", `width`, `height`, `stride`, `format` as a `QImage::Format`)
//! and then writes the pixels into the pipe from another thread. Everything
//! KWin sends is checked before it sizes an allocation.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use image::RgbaImage;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::pipe::{PipeFlags, pipe_with};
use zbus::zvariant::{Fd, OwnedValue, Value};

use super::{CaptureError, MAX_FRAME_BYTES, to_rgba};

const DEST: &str = "org.kde.KWin";
const PATH: &str = "/org/kde/KWin/ScreenShot2";
const IFACE: &str = "org.kde.KWin.ScreenShot2";
const TIMEOUT: Duration = Duration::from_secs(10);
/// KWin's picker waits for the user's click.
const PICK_TIMEOUT: Duration = Duration::from_secs(300);

/// Which of ScreenShot2's captures to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `CaptureWorkspace`: every screen, one image.
    Workspace,
    /// `CaptureActiveScreen`: the screen the pointer is on.
    ActiveScreen,
    /// `CaptureActiveWindow`: the window that has the focus.
    ActiveWindow,
    /// `CaptureInteractive` (window): KWin lets the user click one.
    PickWindow,
}

/// What a capture includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opts {
    pub cursor: bool,
    /// Window title bar and borders (windows only).
    pub frame: bool,
    /// Window shadow (windows only).
    pub shadow: bool,
}

pub fn capture_workspace(include_cursor: bool) -> Result<RgbaImage, CaptureError> {
    capture(
        Target::Workspace,
        Opts {
            cursor: include_cursor,
            frame: true,
            shadow: false,
        },
    )
}

/// One output (by its name) in its own pixels, as KWin put it on screen.
pub fn capture_screen(name: &str, include_cursor: bool) -> Result<RgbaImage, CaptureError> {
    run(
        Target::Workspace,
        Some(name),
        Opts {
            cursor: include_cursor,
            frame: true,
            shadow: false,
        },
    )
}

/// The `a{sv}` options of a call. Native resolution always: the picture is
/// the pixels the screen shows. `hide-caller-windows` keeps anything of ours
/// (the countdown, if it were still up) out of the picture.
fn options(target: Target, o: Opts) -> HashMap<&'static str, Value<'static>> {
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("include-cursor", Value::from(o.cursor));
    options.insert("native-resolution", Value::from(true));
    options.insert("hide-caller-windows", Value::from(true));
    if matches!(target, Target::ActiveWindow | Target::PickWindow) {
        options.insert("include-decoration", Value::from(o.frame));
        options.insert("include-shadow", Value::from(o.shadow));
    }
    options
}

pub fn capture(target: Target, opts: Opts) -> Result<RgbaImage, CaptureError> {
    run(target, None, opts)
}

/// `screen`: `CaptureScreen` of that output, instead of `target`.
fn run(target: Target, screen: Option<&str>, opts: Opts) -> Result<RgbaImage, CaptureError> {
    let timeout = if target == Target::PickWindow {
        PICK_TIMEOUT
    } else {
        TIMEOUT
    };
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(timeout).build())
        .map_err(|e| CaptureError::Unavailable(format!("no D-Bus session bus: {e}")))?;

    let (read_end, write_end) = pipe_with(PipeFlags::CLOEXEC)
        .map_err(|e| CaptureError::Failed(format!("can't create a pipe: {e}")))?;

    let options = options(target, opts);
    let fd = Fd::from(write_end.as_fd());
    let reply = if let Some(name) = screen {
        conn.call_method(
            Some(DEST),
            PATH,
            Some(IFACE),
            "CaptureScreen",
            &(name, options, fd),
        )
    } else {
        let member = match target {
            Target::Workspace => "CaptureWorkspace",
            Target::ActiveScreen => "CaptureActiveScreen",
            Target::ActiveWindow => "CaptureActiveWindow",
            Target::PickWindow => "CaptureInteractive",
        };
        match target {
            // kind 0: a window (1 would be a screen).
            Target::PickWindow => {
                conn.call_method(Some(DEST), PATH, Some(IFACE), member, &(0u32, options, fd))
            }
            _ => conn.call_method(Some(DEST), PATH, Some(IFACE), member, &(options, fd)),
        }
    };
    // KWin holds its own copy now; ours must close or we never see EOF.
    drop(write_end);
    let reply = reply.map_err(classify)?;
    let meta: HashMap<String, OwnedValue> = reply.body().deserialize().map_err(|e| {
        CaptureError::Failed(format!("KWin sent an unreadable screenshot reply: {e}"))
    })?;
    drop(conn);

    let image = Meta::parse(&meta).map_err(CaptureError::Failed)?;
    let data = read_all(read_end, image.byte_len(), TIMEOUT).map_err(CaptureError::Failed)?;
    image.decode(&data).map_err(CaptureError::Failed)
}

/// Missing service, object or method: not KWin (or too old). Anything else is
/// a real failure, explained in plain words.
fn classify(e: zbus::Error) -> CaptureError {
    if let zbus::Error::MethodError(name, msg, _) = &e {
        let name = name.as_str();
        if matches!(
            name,
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
                | "org.freedesktop.DBus.Error.UnknownObject"
                | "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.UnknownMethod"
        ) {
            return CaptureError::Unavailable(format!(
                "KWin's screenshot service isn't running ({name})"
            ));
        }
        if name.ends_with(".Error.Cancelled") {
            return CaptureError::Cancelled;
        }
        if name.ends_with(".Error.NoActiveWindow") {
            return CaptureError::Failed("there is no active window to capture".into());
        }
        if name.ends_with(".NoAuthorized") || name.ends_with(".AccessDenied") {
            return CaptureError::Failed(
                "KWin refused the screenshot: telamon-screenshot must run from /usr/bin, as installed by its package"
                    .into(),
            );
        }
        let msg = msg.as_deref().unwrap_or("");
        return CaptureError::Failed(format!("KWin couldn't take the screenshot: {msg} ({name})"));
    }
    CaptureError::Failed(format!("KWin couldn't take the screenshot: {e}"))
}

#[derive(Debug, PartialEq)]
struct Meta {
    width: u32,
    height: u32,
    stride: usize,
    /// `QImage::Format`
    format: u32,
}

impl Meta {
    fn parse(m: &HashMap<String, OwnedValue>) -> Result<Meta, String> {
        let get = |k: &str| -> Result<u32, String> {
            m.get(k)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| format!("KWin's screenshot reply has no valid {k:?}"))
        };
        if let Some(t) = m.get("type") {
            let t = <&str>::try_from(t).unwrap_or("");
            if t != "raw" {
                return Err(format!("KWin sent a {t:?} screenshot, not raw pixels"));
            }
        }
        let meta = Meta {
            width: get("width")?,
            height: get("height")?,
            stride: get("stride")? as usize,
            format: get("format")?,
        };
        if meta.width == 0 || meta.height == 0 || meta.width > 16384 || meta.height > 16384 {
            return Err(format!(
                "KWin's screenshot has an impossible size ({}x{})",
                meta.width, meta.height
            ));
        }
        if meta.stride < meta.width as usize * 4 || meta.byte_len() > MAX_FRAME_BYTES + 16384 * 64 {
            return Err(format!(
                "KWin's screenshot has an impossible row length ({})",
                meta.stride
            ));
        }
        Ok(meta)
    }

    fn byte_len(&self) -> usize {
        self.stride * self.height as usize
    }

    fn decode(&self, data: &[u8]) -> Result<RgbaImage, String> {
        // QImage formats in memory on little-endian. 4/5/6 are 0xAARRGGBB
        // words (bytes B, G, R, A); 16/17/18 are bytes R, G, B, A.
        let (bgra, opaque, premul) = match self.format {
            4 => (true, true, false),    // Format_RGB32
            5 => (true, false, false),   // Format_ARGB32
            6 => (true, false, true),    // Format_ARGB32_Premultiplied
            16 => (false, true, false),  // Format_RGBX8888
            17 => (false, false, false), // Format_RGBA8888
            18 => (false, false, true),  // Format_RGBA8888_Premultiplied
            f => {
                return Err(format!(
                    "KWin sent pixels in a format this tool can't read (QImage format {f})"
                ));
            }
        };
        to_rgba(
            data,
            self.width,
            self.height,
            self.stride,
            bgra,
            opaque,
            premul,
        )
    }
}

/// Reads exactly `len` bytes (KWin then closes its end), giving up after
/// `timeout` in total.
fn read_all(fd: OwnedFd, len: usize, timeout: Duration) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + timeout;
    let mut file = std::fs::File::from(fd);
    let mut data = vec![0u8; len];
    let mut got = 0;
    while got < len {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("KWin didn't send the screenshot in time".into());
        }
        let ts = Timespec::try_from(left).unwrap_or(Timespec {
            tv_sec: 10,
            tv_nsec: 0,
        });
        let mut fds = [PollFd::new(&file, PollFlags::IN)];
        match poll(&mut fds, Some(&ts)) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(format!("reading the screenshot failed: {e}")),
        }
        match file.read(&mut data[got..]) {
            Ok(0) => return Err(format!("KWin sent {got} of {len} screenshot bytes")),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("reading the screenshot failed: {e}")),
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.try_clone().unwrap()))
            .collect()
    }

    fn u(v: u32) -> OwnedValue {
        OwnedValue::from(v)
    }

    #[test]
    fn parses_and_validates_metadata() {
        let ok = meta(&[
            ("width", u(2)),
            ("height", u(1)),
            ("stride", u(8)),
            ("format", u(6)),
        ]);
        assert_eq!(
            Meta::parse(&ok).unwrap(),
            Meta {
                width: 2,
                height: 1,
                stride: 8,
                format: 6
            }
        );
        let short_stride = meta(&[
            ("width", u(2)),
            ("height", u(1)),
            ("stride", u(4)),
            ("format", u(6)),
        ]);
        assert!(Meta::parse(&short_stride).is_err());
        let huge = meta(&[
            ("width", u(100000)),
            ("height", u(1)),
            ("stride", u(400000)),
            ("format", u(6)),
        ]);
        assert!(Meta::parse(&huge).is_err());
        let missing = meta(&[("width", u(2)), ("height", u(1))]);
        assert!(Meta::parse(&missing).is_err());
        let wrong_type = meta(&[
            ("type", OwnedValue::try_from(Value::from("png")).unwrap()),
            ("width", u(2)),
            ("height", u(1)),
            ("stride", u(8)),
            ("format", u(6)),
        ]);
        assert!(Meta::parse(&wrong_type).is_err());
    }

    #[test]
    fn options_by_target() {
        let o = Opts {
            cursor: true,
            frame: false,
            shadow: true,
        };
        let ws = options(Target::Workspace, o);
        assert_eq!(bool::try_from(&ws["include-cursor"]), Ok(true));
        assert_eq!(bool::try_from(&ws["native-resolution"]), Ok(true));
        assert!(!ws.contains_key("include-decoration") && !ws.contains_key("include-shadow"));
        for t in [Target::ActiveWindow, Target::PickWindow] {
            let w = options(t, o);
            assert_eq!(bool::try_from(&w["include-decoration"]), Ok(false));
            assert_eq!(bool::try_from(&w["include-shadow"]), Ok(true));
            assert_eq!(bool::try_from(&w["hide-caller-windows"]), Ok(true));
        }
        assert!(!options(Target::ActiveScreen, o).contains_key("include-shadow"));
    }

    #[test]
    fn decodes_and_rejects_formats() {
        let m = Meta {
            width: 1,
            height: 1,
            stride: 4,
            format: 4,
        };
        assert_eq!(
            m.decode(&[3, 2, 1, 0]).unwrap().get_pixel(0, 0).0,
            [1, 2, 3, 255]
        );
        let m = Meta {
            width: 1,
            height: 1,
            stride: 4,
            format: 3,
        };
        assert!(m.decode(&[0; 4]).is_err());
    }

    #[test]
    fn pipe_reads_exact_length_and_times_out() {
        let (r, w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        rustix::io::write(&w, &[1, 2, 3]).unwrap();
        drop(w);
        assert_eq!(
            read_all(r, 3, Duration::from_secs(1)).unwrap(),
            vec![1, 2, 3]
        );

        let (r, w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        rustix::io::write(&w, &[1]).unwrap();
        drop(w);
        assert!(
            read_all(r, 3, Duration::from_secs(1))
                .unwrap_err()
                .contains("1 of 3")
        );

        let (r, _w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        assert!(
            read_all(r, 3, Duration::from_millis(100))
                .unwrap_err()
                .contains("in time")
        );
    }
}
