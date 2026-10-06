//! Text from pixels with Windows' built-in OCR (Windows.Media.Ocr), for apps whose accessibility
//! tree lacks it (games, canvases, video). Runs on the capture worker; pixels never leave it.

use windows::Foundation::Rect;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use winwright_contracts::capture::{ScreenText, TextLine, TextWord};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::com::platform;
use crate::raster::Bgra;

fn unavailable(reason: impl Into<String>) -> WinwrightError {
    WinwrightError::BackendUnavailable {
        backend: "Windows OCR".into(),
        reason: reason.into(),
    }
}

/// A word's box (image pixels, fractional) on the desktop.
fn on_desktop(r: Rect, origin: PhysicalPoint) -> PhysicalRect {
    let left = origin.x + r.X.floor() as i32;
    let top = origin.y + r.Y.floor() as i32;
    let right = origin.x + (r.X + r.Width).ceil() as i32;
    let bottom = origin.y + (r.Y + r.Height).ceil() as i32;
    PhysicalRect::new(left, top, right.max(left + 1), bottom.max(top + 1))
}

fn union(a: PhysicalRect, b: PhysicalRect) -> PhysicalRect {
    PhysicalRect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

/// Reads the text in `image`, whose top-left pixel is `origin` on the desktop.
pub fn recognize(image: &Bgra, origin: PhysicalPoint) -> WinwrightResult<ScreenText> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| {
        unavailable("no OCR language is installed (Settings > Time & language > Language)")
    })?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(0);
    if max > 0 && (image.width > max || image.height > max) {
        return Err(WinwrightError::invalid(format!(
            "{}x{} is larger than OCR reads ({max} px a side): read a window or region",
            image.width, image.height
        )));
    }
    let buffer = CryptographicBuffer::CreateFromByteArray(&image.pixels)
        .map_err(|e| platform("CryptographicBuffer::CreateFromByteArray", &e))?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        image.width as i32,
        image.height as i32,
        BitmapAlphaMode::Ignore,
    )
    .map_err(|e| platform("SoftwareBitmap::CreateCopyWithAlphaFromBuffer", &e))?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .and_then(|op| op.join())
        .map_err(|e| platform("OcrEngine::RecognizeAsync", &e))?;
    let language = engine
        .RecognizerLanguage()
        .and_then(|l| l.LanguageTag())
        .map(|t| t.to_string())
        .unwrap_or_default();
    let mut lines = Vec::new();
    for line in result
        .Lines()
        .map_err(|e| platform("OcrResult::Lines", &e))?
    {
        let mut words = Vec::new();
        for word in line.Words().map_err(|e| platform("OcrLine::Words", &e))? {
            let (Ok(text), Ok(rect)) = (word.Text(), word.BoundingRect()) else {
                continue;
            };
            words.push(TextWord {
                text: text.to_string(),
                bounds: on_desktop(rect, origin),
            });
        }
        let Some(bounds) = words.iter().map(|w| w.bounds).reduce(union) else {
            continue;
        };
        lines.push(TextLine {
            text: line.Text().map(|t| t.to_string()).unwrap_or_default(),
            bounds,
            words,
        });
    }
    Ok(ScreenText { language, lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boxes_cover_their_fractional_pixels_on_the_desktop() {
        let r = Rect {
            X: 10.4,
            Y: 2.6,
            Width: 20.2,
            Height: 0.1,
        };
        let b = on_desktop(r, PhysicalPoint { x: -100, y: 50 });
        assert_eq!((b.left, b.top, b.right, b.bottom), (-90, 52, -69, 53));
    }
}
