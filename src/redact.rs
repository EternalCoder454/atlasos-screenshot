//! Alt+drag: cover e-mail addresses, IP addresses and MAC addresses with
//! opaque boxes.
//!
//! The boxes are solid fills, never blurs: a blur of short text can be read
//! back or brute-forced. This is best-effort: it only covers what OCR read,
//! and read correctly, one line at a time.

use std::net::Ipv6Addr;
use std::sync::LazyLock;

use image::{Rgba, RgbaImage};
use regex::Regex;

use crate::config::RedactConfig;

/// A pixel rectangle in the cropped image. Right and bottom are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixRect {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
}

impl PixRect {
    fn union(&self, o: &PixRect) -> PixRect {
        PixRect {
            left: self.left.min(o.left),
            top: self.top.min(o.top),
            right: self.right.max(o.right),
            bottom: self.bottom.max(o.bottom),
        }
    }
}

/// One line of OCR output, with a box per character.
#[derive(Debug, Clone)]
pub struct Line {
    pub chars: Vec<(char, PixRect)>,
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}").unwrap()
});
static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^0-9.])((?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9]))(?:/[0-9]{1,2})?(?:$|[^0-9.])").unwrap()
});
// One separator throughout (the regex crate has no backreferences).
static MAC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{2}(?::[0-9a-f]{2}){5}\b|\b[0-9a-f]{2}(?:-[0-9a-f]{2}){5}\b")
        .unwrap()
});
/// Candidates only; each is confirmed by `Ipv6Addr`'s parser.
static IPV6_CANDIDATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[0-9a-f:]*:[0-9a-f:]*:[0-9a-f:.]*(?:%[0-9a-z]+)?").unwrap());

/// Byte ranges (into `text`) of everything the config asks to redact.
pub fn find_sensitive(text: &str, cfg: &RedactConfig) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    if cfg.emails {
        found.extend(EMAIL.find_iter(text).map(|m| (m.start(), m.end())));
    }
    if cfg.ipv4 {
        // OCR often reads 0 as O and 1 as l or I; a same-length copy with
        // those swapped back finds addresses the raw text hides.
        let digits: String = text
            .chars()
            .map(|c| match c {
                'O' | 'o' => '0',
                'l' | 'I' | '|' => '1',
                c => c,
            })
            .collect();
        // OCR also drops a dot now and then ("192.168.1 20"). A third copy
        // reads a space or comma between digits as a dot; its matches only
        // count with at least two real dots, so ordinary numbers stay.
        let b = digits.as_bytes();
        let gaps: String = digits
            .char_indices()
            .map(|(i, c)| {
                let between = i > 0
                    && b[i - 1].is_ascii_digit()
                    && b.get(i + 1).is_some_and(u8::is_ascii_digit);
                if between && (c == ' ' || c == ',') {
                    '.'
                } else {
                    c
                }
            })
            .collect();
        for (t, need_dots) in [(text, 3), (digits.as_str(), 3), (gaps.as_str(), 2)] {
            // Resume right after each address, not after the separator the
            // pattern consumed, so "1.1.1.1 2.2.2.2" finds both.
            let mut at = 0;
            while let Some(m) = IPV4.captures_at(t, at).and_then(|c| c.get(1)) {
                if text[m.start()..m.end()].matches('.').count() >= need_dots {
                    found.push((m.start(), m.end()));
                }
                at = m.end();
            }
        }
    }
    if cfg.mac {
        found.extend(MAC.find_iter(text).map(|m| (m.start(), m.end())));
    }
    if cfg.ipv6 {
        for m in IPV6_CANDIDATE.find_iter(text) {
            // Strip a trailing ':' or '.' (end of a sentence) and a zone id.
            let s = m.as_str().trim_end_matches(['.', ':']);
            let addr = s.split('%').next().unwrap_or(s);
            if addr.parse::<Ipv6Addr>().is_ok() {
                found.push((m.start(), m.start() + s.len()));
            }
        }
    }
    // The digit-fixed copy finds the same addresses again.
    found.sort_unstable();
    found.dedup();
    found
}

/// Boxes covering every sensitive match in `lines`, padded by `pad` pixels.
pub fn boxes(lines: &[Line], cfg: &RedactConfig) -> Vec<PixRect> {
    let mut out = Vec::new();
    for line in lines {
        let mut text = String::new();
        // Byte offset in `text` -> index in `line.chars`.
        let mut char_at = Vec::new();
        for (i, (c, _)) in line.chars.iter().enumerate() {
            for _ in 0..c.len_utf8() {
                char_at.push(i);
            }
            text.push(*c);
        }
        for (start, end) in find_sensitive(&text, cfg) {
            if start >= end {
                continue;
            }
            let (a, b) = (char_at[start], char_at[end - 1]);
            let mut r = line.chars[a].1;
            for (_, cr) in &line.chars[a..=b] {
                r = r.union(cr);
            }
            let pad = cfg.padding as i64;
            out.push(PixRect {
                left: r.left - pad,
                top: r.top - pad,
                right: r.right + pad,
                bottom: r.bottom + pad,
            });
        }
    }
    out
}

/// Paints the boxes opaque, clamped to the image.
pub fn apply(img: &mut RgbaImage, boxes: &[PixRect], rgb: [u8; 3]) {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let fill = Rgba([rgb[0], rgb[1], rgb[2], 255]);
    for b in boxes {
        let (x0, x1) = (b.left.clamp(0, w), b.right.clamp(0, w));
        let (y0, y1) = (b.top.clamp(0, h), b.bottom.clamp(0, h));
        for y in y0..y1 {
            for x in x0..x1 {
                img.put_pixel(x as u32, y as u32, fill);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> RedactConfig {
        RedactConfig::default()
    }

    fn found<'a>(text: &'a str, cfg: &RedactConfig) -> Vec<&'a str> {
        let mut v: Vec<_> = find_sensitive(text, cfg)
            .into_iter()
            .map(|(a, b)| &text[a..b])
            .collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn finds_each_kind() {
        assert_eq!(
            found("mail zach@example.co.uk now", &all()),
            ["zach@example.co.uk"]
        );
        assert_eq!(found("host 192.168.1.20:22 up", &all()), ["192.168.1.20"]);
        assert_eq!(found("net 10.0.0.0/8", &all()), ["10.0.0.0"]);
        assert_eq!(
            found("ether 3c:22:fb:0a:1B:ff", &all()),
            ["3c:22:fb:0a:1B:ff"]
        );
        assert_eq!(
            found("ether 3C-22-FB-0A-1B-FF", &all()),
            ["3C-22-FB-0A-1B-FF"]
        );
        assert_eq!(
            found("inet6 fe80::1c2b:3aff:fe4d:5e6f%eth0 x", &all()),
            ["fe80::1c2b:3aff:fe4d:5e6f%eth0"]
        );
        assert_eq!(
            found("dns 2001:4860:4860::8888.", &all()),
            ["2001:4860:4860::8888"]
        );
        assert_eq!(found("lo ::1", &all()), ["::1"]);
    }

    #[test]
    fn ignores_lookalikes() {
        assert!(found("version 1.2.3.4.5 and 999.1.1.1 and 12:30:45", &all()).is_empty());
        assert!(found("ratio 3:2 at 10:15, not@an-address", &all()).is_empty());
        // A MAC with mixed separators isn't one.
        assert!(found("3c:22-fb:0a:1b:ff", &all()).is_empty());
    }

    #[test]
    fn adjacent_addresses() {
        assert_eq!(
            found("1.1.1.1 2.2.2.2\t3.3.3.3", &all()),
            ["1.1.1.1", "2.2.2.2", "3.3.3.3"]
        );
    }

    #[test]
    fn ocr_digit_confusions() {
        assert_eq!(found("ip 1O.0.l.5 end", &all()), ["1O.0.l.5"]);
        assert_eq!(found("from 192.168.1 20 now", &all()), ["192.168.1 20"]);
        assert_eq!(found("at 10.0,0.7", &all()), ["10.0,0.7"]);
        // One dot is not enough to call it an address.
        assert!(found("pages 1.2 3 4 and 12 34 56 78", &all()).is_empty());
    }

    #[test]
    fn respects_switches() {
        let mut cfg = all();
        cfg.emails = false;
        cfg.ipv4 = false;
        assert!(found("a@b.io 1.2.3.4", &cfg).is_empty());
    }

    fn line(text: &str) -> Line {
        Line {
            chars: text
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    (
                        c,
                        PixRect {
                            left: i as i64 * 10,
                            top: 5,
                            right: i as i64 * 10 + 8,
                            bottom: 20,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn boxes_cover_exactly_the_match_plus_padding() {
        let b = boxes(&[line("ip 1.2.3.4 ok")], &all());
        assert_eq!(
            b,
            [PixRect {
                left: 30 - 2,
                top: 3,
                right: 98 + 2,
                bottom: 22
            }]
        );
        // Non-ASCII before the match must not shift the box.
        let b = boxes(&[line("é→ a@b.io")], &all());
        assert_eq!(b[0].left, 30 - 2);
    }

    #[test]
    fn fill_is_opaque_and_clamped() {
        let mut img = RgbaImage::from_pixel(10, 10, Rgba([200, 200, 200, 255]));
        apply(
            &mut img,
            &[PixRect {
                left: -5,
                top: 8,
                right: 3,
                bottom: 50,
            }],
            [1, 2, 3],
        );
        assert_eq!(img.get_pixel(0, 9).0, [1, 2, 3, 255]);
        assert_eq!(img.get_pixel(2, 8).0, [1, 2, 3, 255]);
        assert_eq!(img.get_pixel(3, 9).0, [200, 200, 200, 255]);
        assert_eq!(img.get_pixel(0, 7).0, [200, 200, 200, 255]);
    }
}
