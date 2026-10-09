//! The PNG the helpers take on stdin (`--save-png`, `--copy-png`), checked.

use std::io::Read;

/// The biggest PNG taken from stdin.
pub const MAX_STDIN_PNG: u64 = 256 * 1024 * 1024;

/// A PNG from `input`, checked: the signature, and a size that is a screen's
/// and not a bomb's. The bytes are kept as they are.
pub fn read_png(input: impl Read) -> Result<Vec<u8>, String> {
    use image::ImageDecoder;

    let mut png = Vec::new();
    input
        .take(MAX_STDIN_PNG + 1)
        .read_to_end(&mut png)
        .map_err(|e| format!("can't read the picture: {e}"))?;
    if png.len() as u64 > MAX_STDIN_PNG {
        return Err("the picture is too large".into());
    }
    let dec = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&png))
        .map_err(|_| "that is not a PNG picture".to_string())?;
    let (w, h) = dec.dimensions();
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(format!("a {w}x{h} picture is too large"));
    }
    Ok(png)
}
