//! 画像のデコードと縮小(本体と Worker の両方で使う)。

/// 保存する画像の一辺の上限(ピクセル)。
pub const MAX_PIXELS: u32 = 1600;
/// これより小さい PNG / JPEG はそのまま保存する(バイト)。
const KEEP_BYTES: usize = 600 * 1024;

/// 保存用のバイト列と、画像の大きさ(ピクセル)。大きすぎれば縮めて保存し直す。
pub fn prepare_image(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("画像の大きさが 0 です".into());
    }
    let small = w.max(h) <= MAX_PIXELS && bytes.len() <= KEEP_BYTES;
    if small && matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Ok((bytes.to_vec(), w, h));
    }
    let img = if w.max(h) > MAX_PIXELS {
        img.resize(
            MAX_PIXELS,
            MAX_PIXELS,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let (w, h) = (img.width(), img.height());
    let mut out = std::io::Cursor::new(Vec::new());
    if img.color().has_alpha() {
        img.to_rgba8()
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
    } else {
        encode_jpeg(&img.to_rgb8(), 85, &mut out)?;
    }
    Ok((out.into_inner(), w, h))
}

/// RGB の画像を JPEG にする。
pub fn encode_jpeg(
    rgb: &image::RgbImage,
    quality: u8,
    out: &mut std::io::Cursor<Vec<u8>>,
) -> Result<(), String> {
    image::codecs::jpeg::JpegEncoder::new_with_quality(out, quality)
        .encode_image(rgb)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 128]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn small_images_are_kept_and_large_ones_shrunk() {
        let small = png(40, 20);
        let (bytes, w, h) = prepare_image(&small).unwrap();
        assert_eq!((bytes, w, h), (small, 40, 20));
        let (_, w, h) = prepare_image(&png(3200, 800)).unwrap();
        assert_eq!((w, h), (1600, 400));
        assert!(prepare_image(b"not an image").is_err());
    }
}
