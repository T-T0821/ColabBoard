//! PDF のページを JPEG にする(hayro、Pure Rust のラスタライザ)。
//!
//! 数百ページの PDF を一度に描くと最初のページが出るまで長く待つので、
//! 「開く(ページ数と大きさだけ読む)」と「1 ページ描く」に分けてある。
//! 本体はページの大きさで先に枠を並べ、ページごとの仕事を複数の Worker に配って並行に描く。

use hayro::hayro_interpret::InterpreterSettings;
pub use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;

/// PDF を開く(中身の解析はページを描くときに必要な分だけ行う)。
pub fn open(bytes: Vec<u8>) -> Result<Pdf, String> {
    Pdf::new(bytes).map_err(|e| format!("PDF を読めません: {e:?}"))
}

/// 取り込むページ([`crate::MAX_PAGES`] まで)の大きさ(ポイント)と、PDF の全ページ数。
pub fn page_sizes(pdf: &Pdf) -> Result<(Vec<(f32, f32)>, usize), String> {
    let pages = pdf.pages();
    if pages.is_empty() {
        return Err("PDF にページがありません".into());
    }
    let sizes = pages
        .iter()
        .take(crate::MAX_PAGES)
        .map(|p| {
            let (w, h) = p.render_dimensions();
            (w.max(1.0), h.max(1.0))
        })
        .collect();
    Ok((sizes, pages.len()))
}

/// `index` 番目(0 から)のページを、長辺 `pixels` ピクセルの JPEG にする。
/// 返すのは JPEG と、その大きさ(ピクセル)。
pub fn render_page(pdf: &Pdf, index: usize, pixels: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let page = pdf
        .pages()
        .get(index)
        .ok_or_else(|| format!("{} ページ目がありません", index + 1))?;
    let (w, h) = page.render_dimensions();
    let scale = (pixels as f32 / w.max(h).max(1.0)).min(4.0);
    // フォントなどのキャッシュは PDF を借りるので、ページごとに作る(Worker ごとにページを分けて描くため)。
    let cache = hayro::RenderCache::new();
    let pixmap = hayro::render(
        page,
        &cache,
        &InterpreterSettings::default(),
        &hayro::RenderSettings::default(),
        &hayro::PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (pw, ph) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    let rgba = pixmap.take_rgba8(hayro::vello_cpu::peniko::ImageAlphaType::Alpha);
    let img = image::RgbaImage::from_raw(pw, ph, rgba).ok_or("ページを画像にできません")?;
    let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
    let quality = if pixels >= 1400 { 85 } else { 80 };
    let mut buf = std::io::Cursor::new(Vec::new());
    crate::picture::encode_jpeg(&rgb, quality, &mut buf)?;
    Ok((buf.into_inner(), pw, ph))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `pages` ページ(1 ページ目は横長)の小さな PDF を作る。
    pub fn sample_pdf(pages: usize) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ));
        for i in 0..pages {
            let size = if i == 0 { "0 0 400 200" } else { "0 0 200 300" };
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [{size}] /Contents {} 0 R >>",
                4 + i * 2
            ));
            let content = format!("0 0 1 rg 10 10 {} 50 re f", 20 + i);
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn opens_and_renders_pages_one_by_one() {
        let pdf = open(sample_pdf(3)).unwrap();
        let (sizes, total) = page_sizes(&pdf).unwrap();
        assert_eq!(total, 3);
        assert_eq!(sizes, vec![(400.0, 200.0), (200.0, 300.0), (200.0, 300.0)]);
        let (jpeg, w, h) = render_page(&pdf, 0, 800).unwrap();
        assert_eq!((w, h), (800, 400));
        assert_eq!(
            image::guess_format(&jpeg).unwrap(),
            image::ImageFormat::Jpeg
        );
        let (_, w, h) = render_page(&pdf, 2, 900).unwrap();
        assert_eq!((w, h), (600, 900));
        assert!(render_page(&pdf, 3, 900).is_err());
    }

    #[test]
    fn only_the_first_pages_are_taken_from_huge_pdfs() {
        let pdf = open(sample_pdf(crate::MAX_PAGES + 5)).unwrap();
        let (sizes, total) = page_sizes(&pdf).unwrap();
        assert_eq!(sizes.len(), crate::MAX_PAGES);
        assert_eq!(total, crate::MAX_PAGES + 5);
        assert!(open(b"not a pdf".to_vec()).is_err());
    }
}
