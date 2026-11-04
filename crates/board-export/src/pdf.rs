//! 画像を並べた PDF(1 枚 1 ページ)。画素は可逆圧縮(Flate)で埋め込むので、文字がにじまない。

use image::RgbaImage;

/// 1 ページ分の画像と、ページの大きさ(pt)。
pub struct Page<'a> {
    pub image: &'a RgbaImage,
    pub width_pt: f32,
    pub height_pt: f32,
}

/// 透明な部分は白にする(PDF の画像は不透明で埋め込む)。
fn to_rgb(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.width() as usize * img.height() as usize * 3);
    for p in img.pixels() {
        let a = p.0[3] as u32;
        for i in 0..3 {
            out.push(((p.0[i] as u32 * a + 255 * (255 - a)) / 255) as u8);
        }
    }
    out
}

/// PDF の文字列(UTF-16BE の 16 進)。
fn text_string(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        out.push_str(&format!("{u:04X}"));
    }
    out.push('>');
    out
}

pub fn build(pages: &[Page], title: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
    // オブジェクト番号: 1 = カタログ, 2 = ページ一覧, 3 = 情報, 以降ページごとに 3 つ(ページ・画像・内容)。
    let page_obj = |i: usize| 4 + i * 3;
    let push = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    push(&mut out, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", page_obj(i)))
        .collect();
    push(
        &mut out,
        &mut offsets,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .as_bytes(),
    );
    push(
        &mut out,
        &mut offsets,
        format!(
            "<< /Title {} /Producer {} >>",
            text_string(title),
            text_string("graboard")
        )
        .as_bytes(),
    );
    for (i, page) in pages.iter().enumerate() {
        let n = page_obj(i);
        let (pw, ph) = (page.width_pt, page.height_pt);
        let (w, h) = (page.image.width(), page.image.height());
        push(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] \
                 /Resources << /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>",
                n + 1,
                n + 2
            )
            .as_bytes(),
        );
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&to_rgb(page.image), 6);
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /FlateDecode /Interpolate true /Length {} >>\nstream\n",
            data.len()
        )
        .into_bytes();
        img.extend_from_slice(&data);
        img.extend_from_slice(b"\nendstream");
        push(&mut out, &mut offsets, &img);
        let content = format!("q {pw:.2} 0 0 {ph:.2} 0 0 cm /Im0 Do Q\n");
        push(
            &mut out,
            &mut offsets,
            format!(
                "<< /Length {} >>\nstream\n{content}endstream",
                content.len()
            )
            .as_bytes(),
        );
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n",
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
    fn xref_points_at_objects() {
        let a = RgbaImage::from_pixel(4, 3, image::Rgba([255, 0, 0, 255]));
        let b = RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let pdf = build(
            &[
                Page {
                    image: &a,
                    width_pt: 30.0,
                    height_pt: 20.0,
                },
                Page {
                    image: &b,
                    width_pt: 10.0,
                    height_pt: 10.0,
                },
            ],
            "企画",
        );
        let text = String::from_utf8_lossy(&pdf).to_string();
        assert!(text.contains("/Count 2"));
        assert!(text.contains("/MediaBox [0 0 30.00 20.00]"));
        let tail = &text[text.rfind("startxref\n").unwrap() + 10..];
        let start: usize = tail.lines().next().unwrap().parse().unwrap();
        assert!(pdf[start..].starts_with(b"xref"));
        let table = String::from_utf8_lossy(&pdf[start..]).to_string();
        for (i, line) in table.lines().skip(3).take(9).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(
                pdf[off..].starts_with(format!("{} 0 obj", i + 1).as_bytes()),
                "{i}"
            );
        }
        // 透明は白になる。
        assert_eq!(to_rgb(&b)[..3], [255, 255, 255]);
    }
}
