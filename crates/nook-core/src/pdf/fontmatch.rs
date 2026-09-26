//! Which installed face letters drawn as pixels are set in. The words read from them are drawn
//! in each face Windows commonly has, rising as far above their baseline as the originals, and
//! compared with them: by shape (the share of ink both have, the drawn words fitted to the
//! originals' width) and by width (a face much narrower or wider is not it).

use std::path::PathBuf;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

use super::fonts::SystemFonts;
use super::raster::Mask;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sans,
    Serif,
    Mono,
}

/// A face to try: its key among the installed fonts (see [`super::fonts::key`]) and what it is.
#[derive(Debug, PartialEq, Eq)]
pub struct Face {
    pub key: &'static str,
    pub family: &'static str,
    pub bold: bool,
    pub italic: bool,
    pub kind: Kind,
}

const fn face(
    key: &'static str,
    family: &'static str,
    bold: bool,
    italic: bool,
    kind: Kind,
) -> Face {
    Face {
        key,
        family,
        bold,
        italic,
        kind,
    }
}

/// The faces tried: those Windows and Office put on most computers, and the ones documents
/// are mostly set in (Arial stands in for Helvetica, whose widths it copies).
pub const FACES: &[Face] = &[
    face("arial", "Arial", false, false, Kind::Sans),
    face("arialbold", "Arial", true, false, Kind::Sans),
    face("arialitalic", "Arial", false, true, Kind::Sans),
    face("arialbolditalic", "Arial", true, true, Kind::Sans),
    face("arialnarrow", "Arial Narrow", false, false, Kind::Sans),
    face("arialnarrowbold", "Arial Narrow", true, false, Kind::Sans),
    face("calibri", "Calibri", false, false, Kind::Sans),
    face("calibribold", "Calibri", true, false, Kind::Sans),
    face("calibriitalic", "Calibri", false, true, Kind::Sans),
    face("aptos", "Aptos", false, false, Kind::Sans),
    face("aptosbold", "Aptos", true, false, Kind::Sans),
    face("segoeui", "Segoe UI", false, false, Kind::Sans),
    face("segoeuibold", "Segoe UI", true, false, Kind::Sans),
    face("verdana", "Verdana", false, false, Kind::Sans),
    face("verdanabold", "Verdana", true, false, Kind::Sans),
    face("tahoma", "Tahoma", false, false, Kind::Sans),
    face("tahomabold", "Tahoma", true, false, Kind::Sans),
    face("trebuchetms", "Trebuchet MS", false, false, Kind::Sans),
    face("trebuchetmsbold", "Trebuchet MS", true, false, Kind::Sans),
    face("centurygothic", "Century Gothic", false, false, Kind::Sans),
    face(
        "centurygothicbold",
        "Century Gothic",
        true,
        false,
        Kind::Sans,
    ),
    face(
        "timesnewroman",
        "Times New Roman",
        false,
        false,
        Kind::Serif,
    ),
    face(
        "timesnewromanbold",
        "Times New Roman",
        true,
        false,
        Kind::Serif,
    ),
    face(
        "timesnewromanitalic",
        "Times New Roman",
        false,
        true,
        Kind::Serif,
    ),
    face(
        "timesnewromanbolditalic",
        "Times New Roman",
        true,
        true,
        Kind::Serif,
    ),
    face("georgia", "Georgia", false, false, Kind::Serif),
    face("georgiabold", "Georgia", true, false, Kind::Serif),
    face("cambria", "Cambria", false, false, Kind::Serif),
    face("cambriabold", "Cambria", true, false, Kind::Serif),
    face("garamond", "Garamond", false, false, Kind::Serif),
    face("garamondbold", "Garamond", true, false, Kind::Serif),
    face("bookantiqua", "Book Antiqua", false, false, Kind::Serif),
    face("bookantiquabold", "Book Antiqua", true, false, Kind::Serif),
    face(
        "palatinolinotype",
        "Palatino Linotype",
        false,
        false,
        Kind::Serif,
    ),
    face(
        "palatinolinotypebold",
        "Palatino Linotype",
        true,
        false,
        Kind::Serif,
    ),
    face("couriernew", "Courier New", false, false, Kind::Mono),
    face("couriernewbold", "Courier New", true, false, Kind::Mono),
    face("consolas", "Consolas", false, false, Kind::Mono),
    face("consolasbold", "Consolas", true, false, Kind::Mono),
    face("lucidaconsole", "Lucida Console", false, false, Kind::Mono),
];

/// A face by its key.
pub fn face_of(key: &str) -> Option<&'static Face> {
    FACES.iter().find(|f| f.key == key)
}

/// The face the letters matched: its file, the font size (the em, as a PDF sizes fonts) in the
/// pixels' scale, and how much wider the letters are than the face at that size.
#[derive(Clone, Debug)]
pub struct Matched {
    pub face: &'static Face,
    pub path: PathBuf,
    pub size_px: f32,
    pub stretch: f32,
    pub score: f32,
}

/// The installed face closest to `letters` (their ink from its top, `baseline` the row edge
/// they stand on) reading `text`; None when no face is installed or the text is empty.
pub fn closest(
    fonts: &SystemFonts,
    text: &str,
    letters: &Mask,
    baseline: usize,
) -> Option<Matched> {
    let text = text.trim();
    if text.is_empty() || baseline < 4 || letters.width < 4 {
        return None;
    }
    let mut best: Option<Matched> = None;
    for face in FACES {
        let Some((path, _)) = fonts.file(face.key) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(font) = FontRef::try_from_slice_and_index(&bytes, 0) else {
            continue;
        };
        // Drawn once to learn how far the text rises at a size, then at the letters' size.
        let Some(probe) = draw(&font, text, 64.0) else {
            continue;
        };
        let scale = 64.0 * baseline as f32 / probe.rise.max(1) as f32;
        let Some(drawn) = draw(&font, text, scale) else {
            continue;
        };
        let stretch = letters.width as f32 / drawn.mask.width as f32;
        if !(0.7..=1.45).contains(&stretch) {
            continue;
        }
        let score = overlap(letters, baseline, &drawn.mask, drawn.rise) - 1.2 * stretch.ln().abs();
        if best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(Matched {
                face,
                path,
                size_px: scale / em_ratio(&font),
                stretch,
                score,
            });
        }
    }
    best
}

/// How many ems ab_glyph's scale is: it sizes a face by its whole height (ascent to descent),
/// a PDF by its em.
pub(crate) fn em_ratio(font: &FontRef) -> f32 {
    font.units_per_em()
        .map_or(1.0, |em| font.height_unscaled() / em)
}

/// Text drawn in a face: its ink, cropped, and how many rows of it are above the baseline.
pub(crate) struct Rendered {
    pub mask: Mask,
    pub rise: usize,
}

/// `text` in `font` at ab_glyph's scale `px` (see [`em_ratio`]), one line, kerned.
pub(crate) fn draw(font: &FontRef, text: &str, px: f32) -> Option<Rendered> {
    let scale = PxScale::from(px);
    let scaled = font.as_scaled(scale);
    let pad = 2.0;
    let base = pad + scaled.ascent();
    let mut caret = pad;
    let mut prev = None;
    let mut glyphs = Vec::new();
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(p) = prev {
            caret += scaled.kern(p, id);
        }
        glyphs.push(id.with_scale_and_position(scale, ab_glyph::point(caret, base)));
        caret += scaled.h_advance(id);
        prev = Some(id);
    }
    let width = (caret + pad).ceil() as usize + 1;
    let height = (base - scaled.descent() + pad).ceil() as usize + 1;
    if width * height > 40_000_000 {
        return None;
    }
    let mut cover = vec![0f32; width * height];
    for g in glyphs {
        if let Some(o) = font.outline_glyph(g) {
            let b = o.px_bounds();
            o.draw(|x, y, c| {
                let (x, y) = (b.min.x as i64 + x as i64, b.min.y as i64 + y as i64);
                if x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < height {
                    let i = y as usize * width + x as usize;
                    cover[i] = (cover[i] + c).min(1.0);
                }
            });
        }
    }
    let mut mask = Mask::new(width, height);
    for (i, &c) in cover.iter().enumerate() {
        mask.ink[i] = c >= 0.5;
    }
    let b = mask.bounds()?;
    let rise = (base.round() as usize).checked_sub(b.top)?;
    Some(Rendered {
        mask: mask.crop(b),
        rise,
    })
}

/// The share of ink two masks have in common (intersection over union), `b` fitted to `a`'s
/// width, both on the same baseline.
fn overlap(a: &Mask, a_base: usize, b: &Mask, b_base: usize) -> f32 {
    let top = -(a_base.max(b_base) as i64);
    let bottom = (a.height as i64 - a_base as i64).max(b.height as i64 - b_base as i64);
    let (mut both, mut either) = (0usize, 0usize);
    for r in top..bottom {
        let (ay, by) = (r + a_base as i64, r + b_base as i64);
        for x in 0..a.width {
            let bx = x * b.width / a.width;
            let ai = ay >= 0 && a.get(x, ay as usize);
            let bi = by >= 0 && b.get(bx, by as usize);
            both += (ai && bi) as usize;
            either += (ai || bi) as usize;
        }
    }
    if either == 0 {
        0.0
    } else {
        both as f32 / either as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_key_is_a_windows_name_key() {
        for f in FACES {
            assert_eq!(super::super::fonts::key(f.key), f.key);
            assert!(
                f.key.starts_with(&super::super::fonts::key(f.family)),
                "{}",
                f.key
            );
        }
        assert_eq!(face_of("georgiabold").unwrap().family, "Georgia");
    }

    #[test]
    fn the_same_ink_overlaps_fully_and_a_shift_less() {
        let mut a = Mask::new(10, 10);
        for y in 2..8 {
            a.set(3, y, true);
            a.set(4, y, true);
        }
        assert_eq!(overlap(&a, 8, &a, 8), 1.0);
        let mut b = Mask::new(10, 10);
        for y in 2..8 {
            b.set(4, y, true);
            b.set(5, y, true);
        }
        let half = overlap(&a, 8, &b, 8);
        assert!((0.3..0.4).contains(&half), "{half}");
    }

    /// Words drawn in an installed face are matched to it, at its size and width.
    #[cfg(windows)]
    #[test]
    fn letters_drawn_in_a_face_match_it() {
        let fonts = SystemFonts::installed();
        let mut tried = 0;
        for (key, text) in [
            ("timesnewromanbold", "Invoice 2026-10-14"),
            ("arial", "Date of birth 6-23-1990"),
            ("couriernew", "Account 000123"),
            ("calibri", "Istanbul, Turkey"),
            ("georgia", "Bill to: Northwind"),
        ] {
            let Some((path, _)) = fonts.file(key) else {
                continue;
            };
            let bytes = std::fs::read(&path).unwrap();
            let font = FontRef::try_from_slice_and_index(&bytes, 0).unwrap();
            // 42 pixels to the em.
            let drawn = draw(&font, text, 42.0 * em_ratio(&font)).unwrap();
            let got = closest(&fonts, text, &drawn.mask, drawn.rise).unwrap();
            assert_eq!(got.face.key, key, "{text}");
            assert!((got.stretch - 1.0).abs() < 0.03, "{key}: {}", got.stretch);
            assert!((got.size_px - 42.0).abs() < 1.5, "{key}: {}", got.size_px);
            tried += 1;
        }
        assert!(tried >= 3, "the usual Windows fonts are installed");
    }
}
