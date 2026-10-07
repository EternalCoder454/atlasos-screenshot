//! The overlay's pixels, kept apart from the Wayland code so they can be
//! tested and timed without a compositor.
//!
//! A screen's buffer is opaque XRGB8888 (B, G, R, X in memory) at frame
//! resolution: the frozen frame dimmed, and the selection undimmed inside a
//! rounded accent border. The dimmed frame is computed once per screen.
//! Buffers are reused, so a redraw repaints only the pixels that can differ
//! between what that buffer last showed and the new selection, and the
//! compositor is told (damaged) only about what differs from the last
//! commit. Dragging a full-screen selection on a 4K screen touches a few
//! strips along the moving edges per frame, not every pixel.

use crate::capture::{Frame, Rect};

/// Atlas.Ui's `AtlasStyle.radius` and control border width (logical px).
const RADIUS: f64 = 6.0;
const BORDER: f64 = 1.0;

/// What a buffer's pixels show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shows {
    /// Never drawn into.
    Garbage,
    /// The dimmed frame with this selection (or none), in logical coords.
    Selection(Option<Rect>),
}

/// Up to 16 rectangles in buffer pixels, clipped and non-empty, without
/// allocating.
#[derive(Debug, Clone, Copy)]
pub struct Rects {
    n: usize,
    r: [Rect; 16],
}

impl Rects {
    fn new() -> Rects {
        Rects {
            n: 0,
            r: [Rect::new(0, 0, 0, 0); 16],
        }
    }

    pub fn one(r: Rect) -> Rects {
        let mut out = Rects::new();
        out.r[0] = r;
        out.n = 1;
        out
    }

    fn push(&mut self, r: Rect, clip: &Rect) {
        if let Some(r) = r.intersect(clip) {
            self.r[self.n] = r;
            self.n += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &Rect> {
        self.r[..self.n].iter()
    }
}

/// One screen's part of the frame, prepared once.
pub struct Pixels {
    /// This screen's top-left in frame pixels.
    fx: u32,
    fy: u32,
    pub width: u32,
    pub height: u32,
    /// The frame, dimmed, as opaque XRGB8888 (gaps in the frame are black).
    dim: Vec<u8>,
    /// B, G, R.
    accent: [u8; 3],
}

impl Pixels {
    /// `None` when the screen lies outside the frame.
    pub fn new(frame: &Frame, geom: &Rect, dim: f32, accent: [u8; 3]) -> Option<Pixels> {
        let (fx, fy, fw, fh) = frame.pixel_rect(geom);
        if fw == 0 || fh == 0 {
            return None;
        }
        let keep = 1.0 - dim.clamp(0.0, 0.9);
        let k = (keep * 256.0) as u32;
        let row_len = fw as usize * 4;
        let mut out = vec![0u8; row_len * fh as usize];
        let iw = frame.image.width() as usize;
        let img = frame.image.as_raw();
        for (y, dst) in out.chunks_exact_mut(row_len).enumerate() {
            let start = ((fy as usize + y) * iw + fx as usize) * 4;
            let src = &img[start..start + row_len];
            for (d, s) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<4>().0)
            {
                let a = s[3] as u32;
                let c = |v: u8| {
                    if a == 255 {
                        ((v as u32 * k) >> 8) as u8
                    } else {
                        (((v as u32 * a / 255) * k) >> 8) as u8
                    }
                };
                *d = [c(s[2]), c(s[1]), c(s[0]), 255];
            }
        }
        Some(Pixels {
            fx,
            fy,
            width: fw,
            height: fh,
            dim: out,
            accent: [accent[2], accent[1], accent[0]],
        })
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    fn shape(&self, frame: &Frame, sel: &Rect) -> Shape {
        Shape::new(frame, sel, self.fx, self.fy)
    }

    /// The pixels that can differ between selection `prev` and `want`
    /// (logical), clipped to this screen. Empty when nothing on this screen
    /// changes.
    pub fn changed(&self, frame: &Frame, prev: Option<Rect>, want: Option<Rect>) -> Rects {
        let mut out = Rects::new();
        if prev == want {
            return out;
        }
        let bounds = self.bounds();
        let a = prev.map(|r| self.shape(frame, &r));
        let b = want.map(|r| self.shape(frame, &r));
        // Plain undimmed frame under both selections: unchanged.
        let keep = match (&a, &b) {
            (Some(a), Some(b)) => a.inner().zip(b.inner()).and_then(|(x, y)| x.intersect(&y)),
            _ => None,
        };
        for s in [a, b].iter().flatten() {
            let o = s.outer();
            match keep.and_then(|k| k.intersect(&o)) {
                None => out.push(o, &bounds),
                Some(k) => {
                    out.push(Rect::new(o.x, o.y, o.w, k.y - o.y), &bounds);
                    out.push(
                        Rect::new(o.x, k.bottom(), o.w, o.bottom() - k.bottom()),
                        &bounds,
                    );
                    out.push(Rect::new(o.x, k.y, k.x - o.x, k.h), &bounds);
                    out.push(
                        Rect::new(k.right(), k.y, o.right() - k.right(), k.h),
                        &bounds,
                    );
                }
            }
            for c in s.corners() {
                out.push(c, &bounds);
            }
        }
        out
    }

    /// Brings `buf`, which shows `had`, up to date with the selection `want`.
    pub fn update(&self, frame: &Frame, buf: &mut [u8], had: Shows, want: Option<Rect>) {
        let buf = &mut buf[..self.dim.len()];
        let shape = want.map(|r| self.shape(frame, &r));
        match had {
            Shows::Garbage => {
                buf.copy_from_slice(&self.dim);
                if let Some(s) = &shape {
                    self.paint(frame, buf, s.outer(), Some(s));
                }
            }
            Shows::Selection(prev) => {
                for r in self.changed(frame, prev, want).iter() {
                    self.paint(frame, buf, *r, shape.as_ref());
                }
            }
        }
    }

    /// Repaints `r` (buffer pixels) of `buf` for `shape`. Each row is
    /// dimmed frame outside the shape, plain frame well inside it (a copy
    /// with the channels swapped), and the exact antialiased mix only along
    /// the border.
    fn paint(&self, frame: &Frame, buf: &mut [u8], r: Rect, shape: Option<&Shape>) {
        let Some(r) = r.intersect(&self.bounds()) else {
            return;
        };
        let w = self.width as usize;
        let iw = frame.image.width() as usize;
        let img = frame.image.as_raw();
        let (rx0, rx1) = (r.x as usize, r.right() as usize);
        for y in r.y..r.bottom() {
            let row = y as usize * w * 4;
            let line = &mut buf[row..row + w * 4];
            let dim = &self.dim[row..row + w * 4];
            let src_at = ((self.fy as usize + y as usize) * iw + self.fx as usize) * 4;
            let src = &img[src_at..src_at + w * 4];
            let yc = y as f64 + 0.5;
            let Some(s) = shape.filter(|s| (yc - s.cy).abs() < s.hh + 0.5) else {
                line[rx0 * 4..rx1 * 4].copy_from_slice(&dim[rx0 * 4..rx1 * 4]);
                continue;
            };
            let clamp = |v: f64, lo: usize, hi: usize| (v.max(lo as f64).min(hi as f64)) as usize;
            let ox0 = clamp((s.cx - s.hw).floor(), rx0, rx1);
            let ox1 = clamp((s.cx + s.hw).ceil(), ox0, rx1);
            let (ia, ib) = s.interior_cols(yc);
            let ia = clamp(ia, ox0, ox1);
            let ib = clamp(ib, ia, ox1);

            line[rx0 * 4..ox0 * 4].copy_from_slice(&dim[rx0 * 4..ox0 * 4]);
            line[ox1 * 4..rx1 * 4].copy_from_slice(&dim[ox1 * 4..rx1 * 4]);
            for (d, p) in line[ia * 4..ib * 4]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src[ia * 4..ib * 4].as_chunks::<4>().0)
            {
                *d = bright(p);
            }
            for x in (ox0..ia).chain(ib..ox1) {
                let (inner, border) = s.coverage(x as f64 + 0.5, yc);
                let rest = (1.0 - inner - border).max(0.0);
                let b = bright(src[x * 4..x * 4 + 4].try_into().unwrap_or(&[0; 4]));
                let px = &mut line[x * 4..x * 4 + 4];
                for c in 0..3 {
                    let v = b[c] as f64 * inner
                        + self.accent[c] as f64 * border
                        + dim[x * 4 + c] as f64 * rest;
                    px[c] = v.round().min(255.0) as u8;
                }
                px[3] = 255;
            }
        }
    }
}

/// A frame pixel (straight RGBA) as opaque XRGB8888, over black.
#[inline]
fn bright(s: &[u8; 4]) -> [u8; 4] {
    let a = s[3] as u32;
    if a == 255 {
        [s[2], s[1], s[0], 255]
    } else {
        let p = |v: u8| ((v as u32 * a + 127) / 255) as u8;
        [p(s[2]), p(s[1]), p(s[0]), 255]
    }
}

/// The selection's rounded rectangle in one screen's buffer pixels, as a
/// signed distance field, for antialiased corners and border.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shape {
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    r: f64,
    bw: f64,
}

impl Shape {
    /// `sel` (logical) on a screen whose buffer starts at frame pixel
    /// (`fx`, `fy`).
    fn new(frame: &Frame, sel: &Rect, fx: u32, fy: u32) -> Shape {
        let s = frame.scale;
        let x0 = (sel.x - frame.bounds.x) as f64 * s - fx as f64;
        let y0 = (sel.y - frame.bounds.y) as f64 * s - fy as f64;
        let (w, h) = (sel.w as f64 * s, sel.h as f64 * s);
        Shape {
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

    /// Every pixel the shape covers at all, plus a pixel of margin. Outside
    /// it a buffer shows the plain dimmed frame.
    fn outer(&self) -> Rect {
        let x0 = (self.cx - self.hw).floor() as i32 - 1;
        let y0 = (self.cy - self.hh).floor() as i32 - 1;
        let x1 = (self.cx + self.hw).ceil() as i32 + 1;
        let y1 = (self.cy + self.hh).ceil() as i32 + 1;
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Pixels certainly inside the border (a pixel of margin), except in
    /// the four `corners`: they show the plain frame.
    fn inner(&self) -> Option<Rect> {
        let x0 = (self.cx - self.hw + self.bw).ceil() as i32 + 1;
        let y0 = (self.cy - self.hh + self.bw).ceil() as i32 + 1;
        let x1 = (self.cx + self.hw - self.bw).floor() as i32 - 1;
        let y1 = (self.cy + self.hh - self.bw).floor() as i32 - 1;
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    /// The rounded corners' squares (with margin), which `inner` may overlap.
    fn corners(&self) -> [Rect; 4] {
        let o = self.outer();
        let lx = (self.cx - self.hw + self.r).ceil() as i32 + 1;
        let rx = (self.cx + self.hw - self.r).floor() as i32 - 1;
        let ty = (self.cy - self.hh + self.r).ceil() as i32 + 1;
        let by = (self.cy + self.hh - self.r).floor() as i32 - 1;
        [
            Rect::new(o.x, o.y, lx - o.x, ty - o.y),
            Rect::new(rx, o.y, o.right() - rx, ty - o.y),
            Rect::new(o.x, by, lx - o.x, o.bottom() - by),
            Rect::new(rx, by, o.right() - rx, o.bottom() - by),
        ]
    }

    /// Columns (as f64 bounds of a half-open range) whose pixels on the
    /// row centred at `yc` are fully inside the border: exactly where
    /// `coverage` gives (1, 0).
    fn interior_cols(&self, yc: f64) -> (f64, f64) {
        let dy = (yc - self.cy).abs();
        let edge = self.bw + 0.5;
        if dy > self.hh - edge {
            return (0.0, 0.0);
        }
        // In the corner rows the curve eats into the row by up to `r`.
        let l = if dy <= self.hh - self.r {
            self.hw - edge
        } else {
            self.hw - edge.max(self.r)
        };
        if l < 0.0 {
            return (0.0, 0.0);
        }
        let a = (self.cx - l - 0.5).ceil();
        let b = (self.cx + l - 0.5).floor() + 1.0;
        (a, b.max(a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::OutputGeom;
    use std::time::Instant;

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
        let sh = Shape::new(&f, &Rect::new(10, 10, 40, 20), 0, 0);
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
        let sh = Shape::new(&f, &Rect::new(0, 0, 4, 4), 0, 0);
        assert_eq!(sh.r, 2.0);
        let (inner, border) = sh.coverage(2.0, 2.0);
        assert!(inner + border > 0.99);
    }

    /// Two screens side by side (the second is the one drawn, so its
    /// buffer starts inside the frame), with a strip of partly transparent
    /// pixels like the gaps between outputs.
    fn test_frame(scale: f64) -> (Frame, Rect) {
        let a = Rect::new(0, 0, 60, 90);
        let b = Rect::new(60, 0, 140, 90);
        let bounds = a.union(&b);
        let (w, h) = (
            (bounds.w as f64 * scale).ceil() as u32,
            (bounds.h as f64 * scale).ceil() as u32,
        );
        let image = image::RgbaImage::from_fn(w, h, |x, y| {
            let a = if (40..48).contains(&y) {
                (x * 7 % 256) as u8
            } else {
                255
            };
            image::Rgba([
                (x * 3 % 256) as u8,
                (y * 5 % 256) as u8,
                ((x ^ y) % 256) as u8,
                a,
            ])
        });
        let outputs = vec![
            OutputGeom {
                name: "A".into(),
                logical: a,
            },
            OutputGeom {
                name: "B".into(),
                logical: b,
            },
        ];
        (
            Frame {
                image,
                bounds,
                scale,
                outputs,
            },
            b,
        )
    }

    const DIM: f32 = 0.45;
    const ACCENT: [u8; 3] = [61, 174, 233];

    /// What the old overlay showed, recomputed from scratch with its exact
    /// arithmetic: the dimmed base, and over it (premultiplied "over", as
    /// the compositor blends) the selection subsurface. XRGB8888 out.
    fn old_composite(frame: &Frame, geom: &Rect, sel: Option<Rect>) -> Vec<u8> {
        let (fx, fy, fw, fh) = frame.pixel_rect(geom);
        let keep = 1.0 - DIM.clamp(0.0, 0.9);
        let k = (keep * 256.0) as u32;
        let iw = frame.image.width() as usize;
        let img = frame.image.as_raw();
        let mut out = vec![0u8; fw as usize * fh as usize * 4];
        for y in 0..fh as usize {
            for x in 0..fw as usize {
                let s = &img[((fy as usize + y) * iw + fx as usize + x) * 4..][..4];
                let a = s[3] as u32;
                let c = |v: u8| (((v as u32 * a / 255) * k) >> 8) as u8;
                out[(y * fw as usize + x) * 4..][..4].copy_from_slice(&[
                    c(s[2]),
                    c(s[1]),
                    c(s[0]),
                    255,
                ]);
            }
        }
        let Some(sel) = sel else { return out };
        let Some(part) = sel.intersect(geom) else {
            return out;
        };
        let (px, py, pw, ph) = frame.pixel_rect(&part);
        let shape = Shape::new(frame, &sel, 0, 0);
        for y in py..py + ph {
            for x in px..px + pw {
                let s = &img[(y as usize * iw + x as usize) * 4..][..4];
                let (inner, border) = shape.coverage(x as f64 + 0.5, y as f64 + 0.5);
                let a = s[3] as f64 / 255.0;
                let mix = |c: u8, acc: u8| {
                    (c as f64 * a * inner + acc as f64 * border)
                        .round()
                        .min(255.0) as u8
                };
                let sb = [
                    mix(s[2], ACCENT[2]),
                    mix(s[1], ACCENT[1]),
                    mix(s[0], ACCENT[0]),
                    ((inner + border) * 255.0).round().min(255.0) as u8,
                ];
                let o = ((y - fy) as usize * fw as usize + (x - fx) as usize) * 4;
                for c in 0..3 {
                    let base = out[o + c] as u32;
                    out[o + c] =
                        (sb[c] as u32 + (base * (255 - sb[3] as u32) + 127) / 255).min(255) as u8;
                }
            }
        }
        out
    }

    fn from_scratch(px: &Pixels, frame: &Frame, sel: Option<Rect>) -> Vec<u8> {
        let mut buf = vec![0xAB; px.dim.len()];
        px.update(frame, &mut buf, Shows::Garbage, sel);
        buf
    }

    /// Deterministic pseudo-random selections: small, large, inverted,
    /// off-screen, empty, crossing the screen edge.
    fn selections(n: usize) -> Vec<Option<Rect>> {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |m: i32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as i32
        };
        let mut out = Vec::new();
        let mut anchor = (100, 40);
        let mut pos = anchor;
        for i in 0..n {
            if i % 25 == 0 {
                anchor = (next(220) - 10, next(110) - 10);
                pos = anchor;
            }
            match next(10) {
                0 => out.push(None),
                1 => pos = (next(220) - 10, next(110) - 10),
                _ => pos = (pos.0 + next(9) - 4, pos.1 + next(9) - 4),
            }
            let r = Rect::new(
                anchor.0.min(pos.0),
                anchor.1.min(pos.1),
                (anchor.0 - pos.0).abs(),
                (anchor.1 - pos.1).abs(),
            );
            out.push((!r.is_empty()).then_some(r));
        }
        out
    }

    #[test]
    fn incremental_redraws_match_full_ones_and_damage_covers_changes() {
        for scale in [1.0, 1.5, 1.7, 2.0] {
            let (frame, geom) = test_frame(scale);
            let px = Pixels::new(&frame, &geom, DIM, ACCENT).unwrap();
            // Three buffers in turn, like the overlay when the compositor
            // holds two.
            let mut bufs = [(); 3].map(|_| (vec![0x5Au8; px.dim.len()], Shows::Garbage));
            let mut shown: Option<Rect> = None;
            let mut shown_px = from_scratch(&px, &frame, None);
            for (n, want) in selections(600).into_iter().enumerate() {
                let (buf, had) = &mut bufs[n % 3];
                px.update(&frame, buf, *had, want);
                *had = Shows::Selection(want);
                let full = from_scratch(&px, &frame, want);
                assert!(
                    *buf == full,
                    "scale {scale}, step {n}: incremental != full for {want:?}"
                );

                // Every pixel that changed since the last commit is damaged.
                let damage = px.changed(&frame, shown, want);
                for (i, (a, b)) in shown_px.chunks(4).zip(full.chunks(4)).enumerate() {
                    if a != b {
                        let (x, y) = (
                            (i % px.width as usize) as i32,
                            (i / px.width as usize) as i32,
                        );
                        assert!(
                            damage.iter().any(|r| r.contains(x, y)),
                            "scale {scale}, step {n}: ({x}, {y}) changed but isn't damaged ({shown:?} -> {want:?})"
                        );
                    }
                }
                shown = want;
                shown_px = full;
            }
        }
    }

    #[test]
    fn looks_like_the_old_overlay() {
        for scale in [1.0, 1.5, 1.7] {
            let (frame, geom) = test_frame(scale);
            let px = Pixels::new(&frame, &geom, DIM, ACCENT).unwrap();
            for want in selections(150) {
                let new = from_scratch(&px, &frame, want);
                let old = old_composite(&frame, &geom, want);
                for (i, (a, b)) in new.iter().zip(&old).enumerate() {
                    assert!(
                        a.abs_diff(*b) <= 2,
                        "scale {scale}, {want:?}: byte {i} is {a}, was {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_to_do_off_screen_or_unchanged() {
        let (frame, geom) = test_frame(1.7);
        let px = Pixels::new(&frame, &geom, DIM, ACCENT).unwrap();
        // Entirely on the other screen.
        let a = Some(Rect::new(5, 5, 30, 30));
        let b = Some(Rect::new(6, 5, 31, 40));
        assert!(px.changed(&frame, None, a).is_empty());
        assert!(px.changed(&frame, a, b).is_empty());
        let c = Some(Rect::new(70, 10, 50, 50));
        assert!(px.changed(&frame, c, c).is_empty());
        assert!(!px.changed(&frame, a, c).is_empty());
    }

    /// The redraw cost of dragging a full-screen selection on the owner's
    /// 4K screen at 1.7 (3840x2160 pixels, 2259x1271 logical): the old way
    /// (a new selection buffer per frame, every pixel through the distance
    /// field) against the new one (two reused buffers, changed strips only).
    /// `cargo test --release -- --ignored --nocapture redraw_bench`
    #[test]
    #[ignore = "benchmark; run with --release --ignored --nocapture"]
    fn redraw_bench() {
        let geom = Rect::new(0, 0, 2259, 1271);
        let frame = Frame {
            image: image::RgbaImage::from_fn(3840, 2160, |x, y| {
                image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255])
            }),
            bounds: geom,
            scale: 1.7,
            outputs: vec![OutputGeom {
                name: "DP-2".into(),
                logical: geom,
            }],
        };
        // A two-second drag (at 60 Hz) from the top-left corner to the
        // bottom-right one, then a second of small wobbles at full size.
        let anchor = (12, 10);
        let mut path: Vec<(i32, i32)> = (1..=120)
            .map(|i| (anchor.0 + 2235 * i / 120, anchor.1 + 1252 * i / 120))
            .collect();
        path.extend((0..60).map(|i| (2247 - (i % 7) * 3, 1262 - (i % 5) * 2)));
        let sels: Vec<Rect> = path
            .iter()
            .map(|p| Rect::new(anchor.0, anchor.1, p.0 - anchor.0, p.1 - anchor.1))
            .collect();

        let t = Instant::now();
        let px = Pixels::new(&frame, &geom, DIM, ACCENT).unwrap();
        let setup = t.elapsed();

        // Old: per frame, a fresh buffer the size of the selection, filled
        // pixel by pixel.
        let mut old_worst = 0f64;
        let t = Instant::now();
        let mut sink = 0u64;
        for sel in &sels {
            let t1 = Instant::now();
            let (fx, fy, fw, fh) = frame.pixel_rect(sel);
            let mut canvas = vec![0u8; fw as usize * fh as usize * 4];
            let shape = Shape::new(&frame, sel, 0, 0);
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
                    let mix = |c: u8, acc: u8| {
                        (c as f64 * a * inner + acc as f64 * border)
                            .round()
                            .min(255.0) as u8
                    };
                    d.copy_from_slice(&[
                        mix(s[2], ACCENT[2]),
                        mix(s[1], ACCENT[1]),
                        mix(s[0], ACCENT[0]),
                        ((inner + border) * 255.0).round().min(255.0) as u8,
                    ]);
                }
            }
            sink += canvas[canvas.len() / 2] as u64;
            old_worst = old_worst.max(t1.elapsed().as_secs_f64());
        }
        let old_total = t.elapsed().as_secs_f64();
        let old_uploaded: u64 = sels
            .iter()
            .map(|s| {
                let (_, _, w, h) = frame.pixel_rect(s);
                w as u64 * h as u64
            })
            .sum();

        // New: two buffers in turn, changed strips only. Both are filled
        // before the drag, as the overlay does (first commit, then the
        // spare); that one-off cost is reported apart.
        let mut bufs = [(); 2].map(|_| (vec![0u8; px.dim.len()], Shows::Garbage));
        let t = Instant::now();
        for (buf, had) in &mut bufs {
            px.update(&frame, buf, *had, None);
            *had = Shows::Selection(None);
        }
        let fill = t.elapsed().as_secs_f64() / 2.0;
        let mut shown = None;
        let mut new_worst = 0f64;
        let mut damaged = 0u64;
        let t = Instant::now();
        for (n, sel) in sels.iter().enumerate() {
            let t1 = Instant::now();
            let want = Some(*sel);
            let damage = px.changed(&frame, shown, want);
            let (buf, had) = &mut bufs[(n + 1) % 2];
            px.update(&frame, buf, *had, want);
            *had = Shows::Selection(want);
            shown = want;
            new_worst = new_worst.max(t1.elapsed().as_secs_f64());
            damaged += damage.iter().map(|r| r.w as u64 * r.h as u64).sum::<u64>();
            sink += buf[buf.len() / 2] as u64;
        }
        let new_total = t.elapsed().as_secs_f64();
        let n = sels.len() as f64;
        println!(
            "redraw_bench: 3840x2160 @1.7, {} frames, sink {sink}",
            sels.len()
        );
        println!(
            "  before the drag: dimmed copy {:.1} ms, first fill {:.1} ms per buffer",
            setup.as_secs_f64() * 1e3,
            fill * 1e3
        );
        println!(
            "  old: {:.2} ms/frame avg, {:.2} ms worst, {:.2} Mpx uploaded/frame",
            old_total / n * 1e3,
            old_worst * 1e3,
            old_uploaded as f64 / n / 1e6
        );
        println!(
            "  new: {:.3} ms/frame avg, {:.3} ms worst, {:.3} Mpx damaged/frame",
            new_total / n * 1e3,
            new_worst * 1e3,
            damaged as f64 / n / 1e6
        );
    }
}
