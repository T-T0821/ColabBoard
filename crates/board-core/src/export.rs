//! 書き出し用の小さな道具(PDF の組み立てなど)。

/// JPEG 1 枚を 1 ページにした PDF を作る。`width`/`height` は画像のピクセル数で、
/// ページは 1 ピクセル = 0.5pt(2 倍密度)の大きさにする。
pub fn jpeg_pdf(jpeg: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (pw, ph) = (width as f32 * 0.5, height as f32 * 0.5);
    let content = format!("q {pw} 0 0 {ph} 0 0 cm /Im0 Do Q\n");
    let mut out: Vec<u8> = Vec::with_capacity(jpeg.len() + 1024);
    let mut offsets = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        let n = offsets.len();
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut out, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    obj(
        &mut out,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] \
             /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>"
        )
        .as_bytes(),
    );
    let mut image = format!(
        "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
        jpeg.len()
    )
    .into_bytes();
    image.extend_from_slice(jpeg);
    image.extend_from_slice(b"\nendstream");
    obj(&mut out, &image);
    obj(
        &mut out,
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        )
        .as_bytes(),
    );
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_offsets_point_at_objects() {
        let pdf = jpeg_pdf(b"\xff\xd8fakejpeg\xff\xd9", 200, 100);
        let at = |off: usize, s: &str| pdf[off..].starts_with(s.as_bytes());
        assert!(at(0, "%PDF-1.4"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/MediaBox [0 0 100 50]"));
        let tail = &text[text.rfind("startxref\n").unwrap() + 10..];
        let start: usize = tail.lines().next().unwrap().parse().unwrap();
        assert!(at(start, "xref"));
        // 各オブジェクトの位置が表のとおりか。
        let table = String::from_utf8_lossy(&pdf[start..]).to_string();
        for (i, line) in table.lines().skip(3).take(5).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(at(off, &format!("{} 0 obj", i + 1)), "{i}");
        }
    }
}
