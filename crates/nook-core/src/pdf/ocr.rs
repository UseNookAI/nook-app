//! Reading text in pixels with Windows' own text recognition (Windows.Media.Ocr): nothing to
//! download, and it reads the languages Windows has recognition for (those of the person's
//! language list that come with it; Settings adds more).

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

#[cfg(not(windows))]
pub fn read(_bgra: &[u8], _width: usize, _height: usize) -> Result<Vec<Vec<Word>>> {
    anyhow::bail!("Reading text in pictures needs Windows.")
}
