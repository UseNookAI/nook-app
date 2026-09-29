//! Reading text in pixels with Windows' own text recognition (Windows.Media.Ocr): nothing to
//! download, and it reads the languages Windows has recognition for (those of the person's
//! language list that come with it; Settings adds more).
//!
//! On a Mac, Apple's Vision does the same (`VNRecognizeTextRequest`, accurate, the language told
//! from the text): each line it reads comes with its words' boxes (`boundingBoxForRange`), turned
//! from Vision's normalized, bottom-up rectangles into the picture's pixels.

use anyhow::Result;

use super::raster::Word;

/// The lines of text in a BGRA picture (rows from the top), each its words from the left, their
/// boxes in the picture's pixels.
#[cfg(windows)]
pub fn read(bgra: &[u8], width: usize, height: usize) -> Result<Vec<Vec<Word>>> {
    use anyhow::{anyhow, Context};
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;

    let most = OcrEngine::MaxImageDimension().unwrap_or(2600) as usize;
    if width > most || height > most {
        return Err(anyhow!(
            "That part of the page is too large to read at once: drag a smaller box."
        ));
    }
    // The person's own languages first, else any Windows can read.
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .ok()
        .or_else(|| {
            let all = OcrEngine::AvailableRecognizerLanguages().ok()?;
            (0..all.Size().ok()?).find_map(|i| {
                let language = all.GetAt(i).ok()?;
                OcrEngine::TryCreateFromLanguage(&language).ok()
            })
        })
        .ok_or_else(|| {
            anyhow!("Windows cannot read text in pictures on this computer yet: add a language with its text recognition in Settings, Time & language, Language & region.")
        })?;
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bgra[..width * height * 4])?;
    let buffer = writer.DetachBuffer()?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
    )?;
    let result = engine
        .RecognizeAsync(&bitmap)?
        .get()
        .context("Windows could not read the text")?;
    let lines = result.Lines()?;
    let mut out = Vec::new();
    for i in 0..lines.Size()? {
        let words = lines.GetAt(i)?.Words()?;
        let mut line = Vec::new();
        for j in 0..words.Size()? {
            let w = words.GetAt(j)?;
            let r = w.BoundingRect()?;
            line.push(Word {
                text: w.Text()?.to_string_lossy(),
                left: r.X,
                top: r.Y,
                right: r.X + r.Width,
                bottom: r.Y + r.Height,
            });
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    Ok(out)
}

/// Whether Windows can read text in pictures here: it has recognition for some language.
#[cfg(windows)]
pub fn available() -> bool {
    use windows::Media::Ocr::OcrEngine;
    OcrEngine::AvailableRecognizerLanguages()
        .and_then(|all| all.Size())
        .is_ok_and(|n| n > 0)
}

/// Vision is in every macOS Nook runs on (13.3 and later).
#[cfg(target_os = "macos")]
pub fn available() -> bool {
    true
}

/// The lines of text in a BGRA picture, read by Vision, top line first.
#[cfg(target_os = "macos")]
pub fn read(bgra: &[u8], width: usize, height: usize) -> Result<Vec<Vec<Word>>> {
    use anyhow::{anyhow, bail};
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::AnyObject;
    use objc2::AllocAnyThread;
    use objc2_core_foundation::CFData;
    use objc2_core_graphics::{
        CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
        CGImageAlphaInfo, CGImageByteOrderInfo,
    };
    use objc2_foundation::{NSArray, NSDictionary, NSRange};
    use objc2_vision::{
        VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
        VNRequestTextRecognitionLevel,
    };

    // As large a picture as Windows' reader takes; the page is drawn at most this size.
    const MOST: usize = 10000;
    if width == 0 || height == 0 || bgra.len() < width * height * 4 {
        bail!("There is no picture to read");
    }
    if width > MOST || height > MOST {
        bail!("That part of the page is too large to read at once: drag a smaller box.");
    }
    autoreleasepool(|_| {
        let data = CFData::from_bytes(&bgra[..width * height * 4]);
        let provider =
            CGDataProvider::with_cf_data(Some(&data)).ok_or_else(|| anyhow!("no image data"))?;
        let space = CGColorSpace::new_device_rgb().ok_or_else(|| anyhow!("no colour space"))?;
        // BGRA in memory: 32-bit little-endian words with the alpha first.
        let info = CGBitmapInfo(
            CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0,
        );
        // SAFETY: the provider holds width * height * 4 bytes, rows of width * 4; no decode array.
        let image = unsafe {
            CGImage::new(
                width,
                height,
                8,
                32,
                width * 4,
                Some(&space),
                info,
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
        .ok_or_else(|| anyhow!("The page could not be handed to Vision"))?;

        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        request.setAutomaticallyDetectsLanguage(true);
        let options: Retained<NSDictionary<VNImageOption, AnyObject>> = NSDictionary::new();
        // SAFETY: an empty options dictionary of the declared types.
        let handler = unsafe {
            VNImageRequestHandler::initWithCGImage_options(
                VNImageRequestHandler::alloc(),
                &image,
                &options,
            )
        };
        let as_request: &VNRequest = &request;
        let requests = NSArray::from_slice(&[as_request]);
        handler.performRequests_error(&requests).map_err(|e| {
            anyhow!(
                "Vision could not read the text: {}",
                e.localizedDescription()
            )
        })?;

        let mut lines: Vec<Vec<Word>> = Vec::new();
        for observation in request.results().map(|r| r.to_vec()).unwrap_or_default() {
            let Some(text) = observation.topCandidates(1).firstObject() else {
                continue;
            };
            let string = text.string().to_string();
            let mut line = Vec::new();
            for (start, len, word) in words_utf16(&string) {
                // SAFETY: the range lies inside the candidate's own string.
                let Ok(rect) =
                    (unsafe { text.boundingBoxForRange_error(NSRange::new(start, len)) })
                else {
                    continue;
                };
                // SAFETY: a plain property of the rectangle Vision returned.
                let b = unsafe { rect.boundingBox() };
                let (w, h) = (width as f64, height as f64);
                line.push(Word {
                    text: word,
                    left: (b.origin.x * w) as f32,
                    right: ((b.origin.x + b.size.width) * w) as f32,
                    top: ((1.0 - (b.origin.y + b.size.height)) * h) as f32,
                    bottom: ((1.0 - b.origin.y) * h) as f32,
                });
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }
        // Reading order, as Windows gives it: top to bottom, then left to right.
        lines.sort_by(|a, b| {
            let top = |l: &Vec<Word>| l.iter().map(|w| w.top).fold(f32::MAX, f32::min);
            let left = |l: &Vec<Word>| l.first().map_or(0.0, |w| w.left);
            top(a).total_cmp(&top(b)).then(left(a).total_cmp(&left(b)))
        });
        Ok(lines)
    })
}

/// The words of `s` (runs between spaces) with where each starts and how long it is in UTF-16
/// code units, the units an `NSRange` counts.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn words_utf16(s: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut current: Option<(usize, String)> = None;
    for c in s.chars() {
        let units = c.len_utf16();
        if c.is_whitespace() {
            if let Some((start, word)) = current.take() {
                out.push((start, at - start, word));
            }
        } else {
            current.get_or_insert_with(|| (at, String::new())).1.push(c);
        }
        at += units;
    }
    if let Some((start, word)) = current {
        out.push((start, at - start, word));
    }
    out
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn available() -> bool {
    false
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn read(_bgra: &[u8], _width: usize, _height: usize) -> Result<Vec<Vec<Word>>> {
    anyhow::bail!("Reading text in pictures needs Windows or macOS.")
}

#[cfg(test)]
mod tests {
    use super::words_utf16;

    #[test]
    fn words_are_counted_in_utf16_units() {
        assert_eq!(
            words_utf16("Total  €5,200 due 𝟙4 Oct"),
            vec![
                (0, 5, "Total".to_string()),
                (7, 6, "€5,200".to_string()),
                (14, 3, "due".to_string()),
                (18, 3, "𝟙4".to_string()),
                (22, 3, "Oct".to_string()),
            ]
        );
        assert!(words_utf16("   ").is_empty());
    }
}
