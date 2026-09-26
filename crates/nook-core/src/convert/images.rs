//! Pictures, converted by Nook itself: read in any format it knows, written in another. JPEG
//! has no see-through parts, so those go on white; an icon is at most 256 pixels a side.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

/// The picture at `input` written as `to` (a format id) at `out`.
pub fn convert(input: &Path, to: &str, out: &Path) -> Result<()> {
    let img = open(input)?;
    save(&img, to, out)
}

pub fn open(input: &Path) -> Result<DynamicImage> {
    Ok(open_upright(input)?.0)
}

/// The picture turned upright as the camera recorded it (its EXIF orientation), and whether it
/// had to be turned.
pub fn open_upright(input: &Path) -> Result<(DynamicImage, bool)> {
    use image::metadata::Orientation;
    use image::ImageDecoder;
    let bad = |e: image::ImageError| anyhow!("Could not read the picture {}: {e}", input.display());
    let mut decoder = image::ImageReader::open(input)
        .with_context(|| format!("Could not read {}", input.display()))?
        .with_guessed_format()
        .with_context(|| format!("Could not read {}", input.display()))?
        .into_decoder()
        .map_err(bad)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(bad)?;
    img.apply_orientation(orientation);
    Ok((img, orientation != Orientation::NoTransforms))
}

/// Writes a picture as `to`.
pub fn save(img: &DynamicImage, to: &str, out: &Path) -> Result<()> {
    let (format, img) = match to {
        "png" => (ImageFormat::Png, img.clone()),
        "jpg" => (ImageFormat::Jpeg, DynamicImage::ImageRgb8(on_white(img))),
        "webp" => (ImageFormat::WebP, DynamicImage::ImageRgba8(img.to_rgba8())),
        "bmp" => (ImageFormat::Bmp, DynamicImage::ImageRgba8(img.to_rgba8())),
        "tiff" => (ImageFormat::Tiff, DynamicImage::ImageRgba8(img.to_rgba8())),
        "gif" => (ImageFormat::Gif, DynamicImage::ImageRgba8(img.to_rgba8())),
        "ico" => {
            let small = if img.width() > 256 || img.height() > 256 {
                img.resize(256, 256, image::imageops::FilterType::Lanczos3)
            } else {
                img.clone()
            };
            (ImageFormat::Ico, DynamicImage::ImageRgba8(small.to_rgba8()))
        }
        other => bail!("{other} is not a picture format Nook writes"),
    };
    img.save_with_format(out, format)
        .map_err(|e| anyhow!("Could not write {}: {e}", out.display()))
}

/// The picture laid on white, for formats with no see-through parts.
fn on_white(img: &DynamicImage) -> image::RgbImage {
    let rgba = img.to_rgba8();
    let mut white = RgbaImage::from_pixel(rgba.width(), rgba.height(), Rgba([255, 255, 255, 255]));
    image::imageops::overlay(&mut white, &rgba, 0, 0);
    DynamicImage::ImageRgba8(white).to_rgb8()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_see_through_png_becomes_every_picture_format() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("dot.png");
        let mut img = RgbaImage::from_pixel(300, 200, Rgba([0, 0, 0, 0]));
        for x in 100..200 {
            for y in 50..150 {
                img.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
        }
        img.save(&png).unwrap();
        for to in ["jpg", "webp", "bmp", "tiff", "gif", "ico", "png"] {
            let out = dir.path().join(format!("out.{to}"));
            convert(&png, to, &out).unwrap();
            let back = open(&out).unwrap();
            if to == "ico" {
                assert_eq!(
                    (back.width(), back.height()),
                    (256, 171),
                    "at most 256 a side"
                );
            } else {
                assert_eq!((back.width(), back.height()), (300, 200), "{to}");
            }
            if to == "jpg" {
                let corner = back.to_rgb8().get_pixel(0, 0).0;
                assert!(
                    corner.iter().all(|&c| c > 245),
                    "see-through went white: {corner:?}"
                );
            }
        }
    }
}
