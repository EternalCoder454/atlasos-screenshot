//! Freezing the screen: one capture of the whole workspace, taken before the
//! overlay appears. The overlay shows this frame and every mode crops from it,
//! so all backends behave the same.
//!
//! Backends, in order (the first one available wins):
//! 1. KWin's `org.kde.KWin.ScreenShot2` over D-Bus (Telamon OS). Restricted to
//!    binaries named by an installed `.desktop` file with
//!    `X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2`.
//! 2. `ext-image-copy-capture-v1`, one capture per output.
//! 3. `wlr-screencopy-unstable-v1`, one capture per output.
//!
//! Coordinates: `Rect` is in the compositor's logical (global) space. The
//! frame holds that space at `scale` pixels per logical unit, the highest
//! output scale, so nothing on the sharpest screen loses detail.

mod kwin;
mod wl;

use image::RgbaImage;

pub use wl::Session;

/// A rectangle in logical, global compositor coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }

    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    pub fn union(&self, o: &Rect) -> Rect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        Rect::new(
            x0,
            y0,
            self.right().max(o.right()) - x0,
            self.bottom().max(o.bottom()) - y0,
        )
    }
}

/// One output, as the compositor lays it out.
#[derive(Debug, Clone)]
pub struct OutputGeom {
    pub name: String,
    pub logical: Rect,
    /// The output's size in device pixels as the user sees it (turned by
    /// its transform); 0x0 when the compositor didn't say.
    pub pixels: (u32, u32),
}

/// One output as it was on screen, in its own pixels. Kept only for
/// outputs whose scale is lower than the frame's: the frame has them
/// resampled, and showing that resampled copy would not line up with what
/// the compositor drew there.
pub struct Native {
    pub name: String,
    pub image: RgbaImage,
}

/// The frozen workspace.
pub struct Frame {
    /// Straight (not premultiplied) RGBA. Gaps between outputs are transparent.
    pub image: RgbaImage,
    /// The workspace's logical bounds; `image` pixel (0, 0) is its top-left.
    pub bounds: Rect,
    /// Frame pixels per logical unit.
    pub scale: f64,
    pub outputs: Vec<OutputGeom>,
    pub natives: Vec<Native>,
}

impl Frame {
    /// Frame pixel range covering a logical rectangle, clamped to the frame.
    /// Rounds outwards, so a selection never loses its edge pixels.
    pub fn pixel_rect(&self, r: &Rect) -> (u32, u32, u32, u32) {
        let s = self.scale;
        let (iw, ih) = (self.image.width() as f64, self.image.height() as f64);
        let x0 = (((r.x - self.bounds.x) as f64) * s).floor().clamp(0.0, iw);
        let y0 = (((r.y - self.bounds.y) as f64) * s).floor().clamp(0.0, ih);
        let x1 = (((r.right() - self.bounds.x) as f64) * s)
            .ceil()
            .clamp(0.0, iw);
        let y1 = (((r.bottom() - self.bounds.y) as f64) * s)
            .ceil()
            .clamp(0.0, ih);
        (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    /// The frame pixels a screen occupies: its corner and size at the frame's
    /// scale, rounded to the nearest pixel (as compositors place outputs),
    /// not outwards like `pixel_rect`: a screen's buffer is shown 1:1, and
    /// an extra pixel would stretch it. Clamped to the frame.
    pub fn screen_rect(&self, r: &Rect) -> (u32, u32, u32, u32) {
        let s = self.scale;
        let (iw, ih) = (self.image.width() as f64, self.image.height() as f64);
        let x0 = (((r.x - self.bounds.x) as f64) * s).round().clamp(0.0, iw);
        let y0 = (((r.y - self.bounds.y) as f64) * s).round().clamp(0.0, ih);
        let x1 = (((r.right() - self.bounds.x) as f64) * s)
            .round()
            .clamp(0.0, iw);
        let y1 = (((r.bottom() - self.bounds.y) as f64) * s)
            .round()
            .clamp(0.0, ih);
        (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    /// The selection at full frame resolution. `None` when it misses the frame.
    pub fn crop(&self, r: &Rect) -> Option<RgbaImage> {
        let (x, y, w, h) = self.pixel_rect(r);
        (w > 0 && h > 0).then(|| image::imageops::crop_imm(&self.image, x, y, w, h).to_image())
    }
}

#[derive(Debug)]
pub enum CaptureError {
    /// This backend doesn't exist here: try the next one.
    Unavailable(String),
    /// It exists but failed: report this.
    Failed(String),
}

/// Captures the whole workspace with the first backend that's available.
pub fn capture_workspace(session: &mut Session, include_cursor: bool) -> Result<Frame, String> {
    let outputs = session.outputs()?;
    if outputs.is_empty() {
        return Err("the compositor reports no screens".into());
    }
    let bounds = outputs
        .iter()
        .skip(1)
        .fold(outputs[0].logical, |b, o| b.union(&o.logical));

    // A KWin refusal (say, a build not in /usr/bin) still tries the
    // Wayland protocols; KWin's reason is the one reported if all fail.
    let mut skipped = Vec::new();
    let kwin_error = match kwin::capture_workspace(include_cursor) {
        Ok(image) => {
            let mut frame = frame_from_workspace_image(image, bounds, outputs)?;
            frame.natives = kwin_natives(&frame, include_cursor);
            return Ok(frame);
        }
        Err(CaptureError::Failed(e)) => Some(e),
        Err(CaptureError::Unavailable(why)) => {
            skipped.push(why);
            None
        }
    };
    match session.capture_outputs(include_cursor) {
        Ok(shots) => compose(shots, bounds, outputs),
        Err(CaptureError::Failed(e)) => Err(kwin_error.unwrap_or(e)),
        Err(CaptureError::Unavailable(why)) => Err(kwin_error.unwrap_or_else(|| {
            skipped.push(why);
            format!(
                "this compositor offers no way to take screenshots ({})",
                skipped.join("; ")
            )
        })),
    }
}

/// The outputs drawn at a lower scale than the frame, as KWin shows them.
/// A failure only costs the exact match on that output: the frame's copy
/// is shown instead.
fn kwin_natives(frame: &Frame, include_cursor: bool) -> Vec<Native> {
    frame
        .outputs
        .iter()
        .filter(|o| {
            let (w, h) = o.pixels;
            w > 0 && h > 0 && (w as f64) < o.logical.w as f64 * frame.scale * 0.98
        })
        .filter_map(|o| {
            let image = kwin::capture_screen(&o.name, include_cursor).ok()?;
            (image.dimensions() == o.pixels).then(|| Native {
                name: o.name.clone(),
                image,
            })
        })
        .collect()
}

/// The pixels-per-logical-unit a compositor can mean: a multiple of 1/120
/// (wp-fractional-scale), when the measured value is within rounding of one.
/// KWin's image is `round(width * 1.7)` wide, which measures 1.69991.
fn snap_scale(scale: f64) -> f64 {
    let snapped = (scale * 120.0).round() / 120.0;
    if (snapped - scale).abs() < 0.002 * scale {
        snapped
    } else {
        scale
    }
}

/// KWin's workspace capture is already one image of the whole layout.
fn frame_from_workspace_image(
    image: RgbaImage,
    bounds: Rect,
    outputs: Vec<OutputGeom>,
) -> Result<Frame, String> {
    let scale = snap_scale(image.width() as f64 / bounds.w as f64);
    let expected_h = bounds.h as f64 * scale;
    if bounds.w <= 0
        || bounds.h <= 0
        || !scale.is_finite()
        || scale <= 0.0
        || (image.height() as f64 - expected_h).abs() > 2.0
    {
        return Err(format!(
            "KWin's screenshot is {}x{}, which doesn't match the {}x{} screen layout",
            image.width(),
            image.height(),
            bounds.w,
            bounds.h
        ));
    }
    Ok(Frame {
        image,
        bounds,
        scale,
        outputs,
        natives: Vec::new(),
    })
}

/// One output's capture, already turned upright.
pub struct OutputShot {
    pub geom: OutputGeom,
    pub image: RgbaImage,
}

/// Lays per-output captures out into one workspace frame.
fn compose(
    shots: Vec<OutputShot>,
    bounds: Rect,
    outputs: Vec<OutputGeom>,
) -> Result<Frame, String> {
    let scale = shots
        .iter()
        .map(|s| s.image.width() as f64 / s.geom.logical.w.max(1) as f64)
        .fold(1.0_f64, f64::max);
    let (fw, fh) = (
        (bounds.w as f64 * scale).round(),
        (bounds.h as f64 * scale).round(),
    );
    if fw * fh * 4.0 > MAX_FRAME_BYTES as f64 {
        return Err(format!(
            "the screens are too large to capture ({fw}x{fh} pixels)"
        ));
    }
    let mut image = RgbaImage::new(fw as u32, fh as u32);
    let mut natives = Vec::new();
    for shot in shots {
        let l = shot.geom.logical;
        let w = (l.w as f64 * scale).round() as u32;
        let h = (l.h as f64 * scale).round() as u32;
        let scaled = if shot.image.dimensions() == (w, h) {
            shot.image
        } else {
            let scaled = image::imageops::resize(
                &shot.image,
                w.max(1),
                h.max(1),
                image::imageops::FilterType::Triangle,
            );
            natives.push(Native {
                name: shot.geom.name.clone(),
                image: shot.image,
            });
            scaled
        };
        let x = ((l.x - bounds.x) as f64 * scale).round() as i64;
        let y = ((l.y - bounds.y) as f64 * scale).round() as i64;
        image::imageops::replace(&mut image, &scaled, x, y);
    }
    Ok(Frame {
        image,
        bounds,
        scale,
        outputs,
        natives,
    })
}

/// 16384 x 16384 RGBA: larger than any real desktop, small enough to never
/// let a confused compositor make us allocate gigabytes.
pub const MAX_FRAME_BYTES: usize = 16384 * 16384 * 4;

/// Converts tightly or loosely packed 32-bit pixels to straight RGBA.
/// `bgra` says the bytes in memory are B, G, R, A (Wayland ARGB8888, Qt ARGB32
/// on little-endian); otherwise R, G, B, A. `opaque` ignores the alpha byte
/// (XRGB formats). `premultiplied` un-premultiplies.
pub fn to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    bgra: bool,
    opaque: bool,
    premultiplied: bool,
) -> Result<RgbaImage, String> {
    let row = width as usize * 4;
    if stride < row || data.len() < stride * (height as usize).saturating_sub(1) + row {
        return Err("the screenshot data is shorter than its size says".into());
    }
    let mut out = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        for px in data[y * stride..y * stride + row].as_chunks::<4>().0 {
            let (r, g, b) = if bgra {
                (px[2], px[1], px[0])
            } else {
                (px[0], px[1], px[2])
            };
            let a = if opaque { 255 } else { px[3] };
            if premultiplied && a != 255 && a != 0 {
                let un = |c: u8| ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
                out.extend_from_slice(&[un(r), un(g), un(b), a]);
            } else {
                out.extend_from_slice(&[r, g, b, a]);
            }
        }
    }
    RgbaImage::from_raw(width, height, out).ok_or_else(|| "bad screenshot size".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, scale: f64) -> Frame {
        let bounds = Rect::new(
            -100,
            0,
            (w as f64 / scale) as i32,
            (h as f64 / scale) as i32,
        );
        Frame {
            image: RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 0, 255])),
            bounds,
            scale,
            outputs: vec![OutputGeom {
                name: "A".into(),
                logical: bounds,
                pixels: (0, 0),
            }],
            natives: Vec::new(),
        }
    }

    #[test]
    fn rect_ops() {
        let a = Rect::new(0, 5, 10, 15);
        assert_eq!(
            a.intersect(&Rect::new(5, 0, 100, 10)),
            Some(Rect::new(5, 5, 5, 5))
        );
        assert_eq!(a.intersect(&Rect::new(10, 0, 5, 5)), None);
        assert_eq!(a.union(&Rect::new(-5, 0, 1, 1)), Rect::new(-5, 0, 15, 20));
        assert!(a.contains(0, 5) && !a.contains(10, 5));
    }

    #[test]
    fn crop_scales_and_clamps() {
        let f = frame(300, 150, 1.5);
        // Logical (-100,0) is pixel (0,0); 10 logical = 15 px.
        let c = f.crop(&Rect::new(-90, 10, 10, 10)).unwrap();
        assert_eq!(c.dimensions(), (15, 15));
        assert_eq!(c.get_pixel(0, 0).0, [15, 15, 0, 255]);
        // Partly outside: clamped, not panicking.
        let c = f.crop(&Rect::new(-150, -50, 100, 100)).unwrap();
        assert_eq!(c.dimensions(), (75, 75));
        assert!(f.crop(&Rect::new(1000, 1000, 5, 5)).is_none());
    }

    #[test]
    fn pixel_conversion() {
        // One BGRA pixel, premultiplied half-alpha red, with 4 bytes of row padding.
        let data = [0, 0, 64, 128, 9, 9, 9, 9];
        let img = to_rgba(&data, 1, 1, 8, true, false, true).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [128, 0, 0, 128]);
        let img = to_rgba(&data, 1, 1, 8, false, true, false).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 64, 255]);
        assert!(to_rgba(&data[..3], 1, 1, 4, true, true, false).is_err());
        assert!(to_rgba(&data, 2, 1, 4, true, true, false).is_err());
    }

    #[test]
    fn compose_places_outputs() {
        let a = OutputGeom {
            name: "A".into(),
            logical: Rect::new(0, 0, 4, 2),
            pixels: (0, 0),
        };
        let b = OutputGeom {
            name: "B".into(),
            logical: Rect::new(4, 0, 2, 2),
            pixels: (0, 0),
        };
        let shots = vec![
            OutputShot {
                geom: a.clone(),
                image: RgbaImage::from_pixel(8, 4, image::Rgba([1, 0, 0, 255])),
            },
            OutputShot {
                geom: b.clone(),
                image: RgbaImage::from_pixel(2, 2, image::Rgba([2, 0, 0, 255])),
            },
        ];
        let f = compose(shots, Rect::new(0, 0, 6, 2), vec![a, b]).unwrap();
        assert_eq!(f.scale, 2.0);
        assert_eq!(f.image.dimensions(), (12, 4));
        assert_eq!(f.image.get_pixel(7, 3).0[0], 1);
        assert_eq!(f.image.get_pixel(8, 0).0[0], 2);
        assert_eq!(f.image.get_pixel(11, 3).0[0], 2);
    }

    #[test]
    fn workspace_image_must_match_layout() {
        let img = RgbaImage::new(300, 150);
        assert!(frame_from_workspace_image(img.clone(), Rect::new(0, 0, 200, 100), vec![]).is_ok());
        assert!(frame_from_workspace_image(img, Rect::new(0, 0, 200, 200), vec![]).is_err());
    }
}
