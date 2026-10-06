//! Text recognition with ocrs (pure Rust, on the CPU).
//!
//! Ctrl+drag wants the text, laid out in lines like Tesseract's `--psm 6`
//! (one uniform block). Alt+drag wants a box per character, to cover matches.

use image::RgbaImage;

use crate::redact::Line;

#[cfg(feature = "ocr")]
pub struct Ocr {
    engine: ocrs::OcrEngine,
}

#[cfg(feature = "ocr")]
impl Ocr {
    /// Builds the engine from verified model bytes (see `models`).
    pub fn new(models: crate::models::Models) -> Result<Ocr, String> {
        let load = |bytes, what| {
            rten::Model::load(bytes).map_err(|e| format!("the {what} model can't be loaded: {e}"))
        };
        let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
            detection_model: Some(load(models.detection, "text detection")?),
            recognition_model: Some(load(models.recognition, "text recognition")?),
            ..Default::default()
        })
        .map_err(|e| format!("the OCR engine didn't start: {e}"))?;
        Ok(Ocr { engine })
    }

    fn input(&self, img: &RgbaImage) -> Result<ocrs::OcrInput, String> {
        let source = ocrs::ImageSource::from_bytes(img.as_raw(), img.dimensions())
            .map_err(|e| format!("the selection can't be read for OCR: {e}"))?;
        self.engine
            .prepare_input(source)
            .map_err(|e| format!("OCR failed: {e}"))
    }

    /// The recognised text, one line per text line, trimmed.
    pub fn text(&self, img: &RgbaImage) -> Result<String, String> {
        let input = self.input(img)?;
        let text = self
            .engine
            .get_text(&input)
            .map_err(|e| format!("OCR failed: {e}"))?;
        Ok(text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string())
    }

    /// Every recognised line with a pixel box per character.
    pub fn lines(&self, img: &RgbaImage) -> Result<Vec<Line>, String> {
        use ocrs::TextItem;

        let input = self.input(img)?;
        let words = self
            .engine
            .detect_words(&input)
            .map_err(|e| format!("OCR failed: {e}"))?;
        let rows = self.engine.find_text_lines(&input, &words);
        let lines = self
            .engine
            .recognize_text(&input, &rows)
            .map_err(|e| format!("OCR failed: {e}"))?;
        Ok(lines
            .into_iter()
            .flatten()
            .map(|line| Line {
                chars: line
                    .chars()
                    .iter()
                    .map(|c| {
                        let r = c.rect;
                        let rect = crate::redact::PixRect {
                            left: r.left() as i64,
                            top: r.top() as i64,
                            right: r.right() as i64,
                            bottom: r.bottom() as i64,
                        };
                        (c.char, rect)
                    })
                    .collect(),
            })
            .collect())
    }
}

/// Builds without the `ocr` feature: Ctrl and Alt drags explain why they
/// can't work.
#[cfg(not(feature = "ocr"))]
pub struct Ocr;

#[cfg(not(feature = "ocr"))]
impl Ocr {
    pub fn text(&self, _: &RgbaImage) -> Result<String, String> {
        Err(NO_OCR.into())
    }

    pub fn lines(&self, _: &RgbaImage) -> Result<Vec<Line>, String> {
        Err(NO_OCR.into())
    }
}

#[cfg(not(feature = "ocr"))]
pub const NO_OCR: &str = "this build of atlasos-screenshot has no text recognition";
