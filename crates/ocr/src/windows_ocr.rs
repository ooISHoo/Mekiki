//! Windows: text recognition via `Windows.Media.Ocr`.
//!
//! # Things worth knowing
//!
//! - **Which languages work depends on the installed language packs.** Anything
//!   missing from `OcrEngine::AvailableRecognizerLanguages` is unusable.
//! - **There is a minimum input size.** Anything below 40x40 is rejected.
//!   To OCR a small region, the caller has to scale it up first.
//! - **BGRA8, premultiplied.** Captured BGRA can be handed over as-is.

use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::core::HSTRING;

use crate::{Line, OcrError, Recognition, TextRecognizer, Word};

/// The minimum size `Windows.Media.Ocr` accepts.
const MIN_DIMENSION: u32 = 40;
/// And the maximum.
const MAX_DIMENSION: u32 = 10000;

fn os_err(e: windows::core::Error) -> OcrError {
    OcrError::Os(format!("{e}"))
}

pub struct WindowsOcr {
    engine: OcrEngine,
    language: String,
}

// OcrEngine is a WinRT agile object and can be used across threads.
unsafe impl Send for WindowsOcr {}

impl WindowsOcr {
    /// Build an engine following the user's language settings.
    pub fn from_user_profile() -> Result<Self, OcrError> {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(os_err)?;
        let language = engine
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .map(|t| t.to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        Ok(Self { engine, language })
    }

    /// Build an engine for a specific language.
    pub fn from_language(tag: &str) -> Result<Self, OcrError> {
        let language = Language::CreateLanguage(&HSTRING::from(tag)).map_err(os_err)?;
        let engine = OcrEngine::TryCreateFromLanguage(&language)
            .map_err(|_| OcrError::LanguageUnavailable(tag.to_string()))?;
        Ok(Self {
            engine,
            language: tag.to_string(),
        })
    }
}

/// Build a `SoftwareBitmap` from a BGRA byte slice.
///
/// Going through `DataWriter` is the most straightforward safe way to construct
/// an `IBuffer`. Touching `IBufferByteAccess` from a raw pointer is possible,
/// but the cost of this path is small next to the OCR itself.
fn software_bitmap(bgra: &[u8], width: u32, height: u32) -> Result<SoftwareBitmap, OcrError> {
    let expected = (width as usize) * (height as usize) * 4;
    if bgra.len() != expected {
        return Err(OcrError::Os(format!(
            "pixel count mismatch: {} bytes, expected {expected}",
            bgra.len()
        )));
    }

    let writer = DataWriter::new().map_err(os_err)?;
    writer.WriteBytes(bgra).map_err(os_err)?;
    let buffer = writer.DetachBuffer().map_err(os_err)?;

    SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
    )
    .or_else(|_| {
        // Alpha handling sometimes gets it rejected, so retry with it stated explicitly.
        SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width as i32,
            height as i32,
            BitmapAlphaMode::Premultiplied,
        )
    })
    .map_err(os_err)
}

impl TextRecognizer for WindowsOcr {
    fn recognize_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Recognition, OcrError> {
        if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&width)
            || !(MIN_DIMENSION..=MAX_DIMENSION).contains(&height)
        {
            return Err(OcrError::ImageSize { width, height });
        }

        let bitmap = software_bitmap(bgra, width, height)?;
        // join() waits for the async operation. OCR takes tens to hundreds of
        // milliseconds, so this is used on the premise that it blocks the
        // caller (the engine's worker thread).
        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .map_err(os_err)?
            .join()
            .map_err(os_err)?;

        let mut lines = Vec::new();
        for line in result.Lines().map_err(os_err)? {
            let text = line.Text().map_err(os_err)?.to_string();

            let mut words = Vec::new();
            for w in line.Words().map_err(os_err)? {
                let rect = w.BoundingRect().map_err(os_err)?;
                words.push(Word {
                    text: w.Text().map_err(os_err)?.to_string(),
                    x: rect.X.round() as i32,
                    y: rect.Y.round() as i32,
                    width: rect.Width.round().max(0.0) as u32,
                    height: rect.Height.round().max(0.0) as u32,
                });
            }

            lines.push(Line { text, words });
        }

        Ok(Recognition {
            lines,
            text_angle: result.TextAngle().ok().and_then(|a| a.Value().ok()),
        })
    }

    fn language(&self) -> String {
        self.language.clone()
    }

    fn backend_name(&self) -> &'static str {
        "windows/media-ocr"
    }
}

/// The list of usable OCR language tags.
pub fn available_languages() -> Result<Vec<String>, OcrError> {
    let languages = OcrEngine::AvailableRecognizerLanguages().map_err(os_err)?;
    let mut tags = Vec::new();
    for l in languages {
        if let Ok(tag) = l.LanguageTag() {
            tags.push(tag.to_string());
        }
    }
    if tags.is_empty() {
        return Err(OcrError::NoEngine);
    }
    Ok(tags)
}
