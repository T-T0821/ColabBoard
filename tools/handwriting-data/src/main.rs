//! 端末内の手書き文字認識([`board_core::handwriting`])で使う文字の見本を作る。
//!
//! 見本は「1 文字 = 筆画(折れ線)の集まり」。次の 2 つから作る。
//! - ひらがな・カタカナ・漢字: [KanjiVG](https://kanjivg.tagaini.net/)(CC BY-SA 3.0)の SVG
//! - 英数字・記号: Hershey フォントの単線体(`futural.jhf`)
//!
//! ```sh
//! # KanjiVG の SVG(kanji/0xxxx.svg)を置いたフォルダと futural.jhf を渡す
//! cargo run -p handwriting-data --release -- --kanjivg kanjivg/kanji --hershey futural.jhf \
//!     [--chars crates/board-core/src/handwriting/chars.txt] [--out crates/board-core/src/handwriting/glyphs.bin]
//! ```
//!
//! 出力の形式(すべてリトルエンディアン):
//! `"HWG1"`、文字数(u16)、各文字に「UTF-8 の長さ(u8)・UTF-8・筆画数(u8)・各筆画に点の数(u8)と点(x, y の u8)」。
//! 座標は文字の枠(全角の正方形)を 0〜255 にしたもの。y は下向き。

use std::collections::BTreeMap;
use std::path::PathBuf;

type Pt = (f32, f32);
type Glyph = Vec<Vec<Pt>>;

/// SVG の path(d 属性)を折れ線にする。M L H V C S Q T Z(大文字・小文字)に対応。
fn parse_path(d: &str) -> Vec<Vec<Pt>> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in d.chars() {
        if c.is_ascii_alphabetic() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            tokens.push(c.to_string());
        } else if c == ',' || c.is_whitespace() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else if (c == '-' && !cur.is_empty() && !cur.ends_with('e'))
            || (c == '.' && cur.contains('.'))
        {
            // 区切りが省かれた数("20-2.12"、"1.5.5")。
            tokens.push(std::mem::take(&mut cur));
            cur.push(c);
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut out: Vec<Vec<Pt>> = Vec::new();
    let mut path: Vec<Pt> = Vec::new();
    let (mut x, mut y) = (0f32, 0f32);
    let (mut start_x, mut start_y) = (0f32, 0f32);
    let mut last_ctrl: Option<Pt> = None;
    let mut cmd = 'M';
    let mut i = 0;
    let num = |i: &mut usize| -> f32 {
        let v = tokens.get(*i).and_then(|t| t.parse().ok()).unwrap_or(0.0);
        *i += 1;
        v
    };
    let cubic = |path: &mut Vec<Pt>, p0: Pt, p1: Pt, p2: Pt, p3: Pt| {
        for k in 1..=10 {
            let t = k as f32 / 10.0;
            let u = 1.0 - t;
            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            path.push((
                w[0] * p0.0 + w[1] * p1.0 + w[2] * p2.0 + w[3] * p3.0,
                w[0] * p0.1 + w[1] * p1.1 + w[2] * p2.1 + w[3] * p3.1,
            ));
        }
    };
    while i < tokens.len() {
        if tokens[i].chars().all(|c| c.is_ascii_alphabetic()) {
            cmd = tokens[i].chars().next().unwrap_or('M');
            i += 1;
            if cmd == 'Z' || cmd == 'z' {
                path.push((start_x, start_y));
                x = start_x;
                y = start_y;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                if path.len() > 1 {
                    out.push(std::mem::take(&mut path));
                }
                path.clear();
                x = ox + num(&mut i);
                y = oy + num(&mut i);
                start_x = x;
                start_y = y;
                path.push((x, y));
                // M の後に続く座標は L として扱う。
                cmd = if rel { 'l' } else { 'L' };
                last_ctrl = None;
            }
            'L' => {
                x = ox + num(&mut i);
                y = oy + num(&mut i);
                path.push((x, y));
                last_ctrl = None;
            }
            'H' => {
                x = ox + num(&mut i);
                path.push((x, y));
                last_ctrl = None;
            }
            'V' => {
                y = oy + num(&mut i);
                path.push((x, y));
                last_ctrl = None;
            }
            'C' => {
                let p1 = (ox + num(&mut i), oy + num(&mut i));
                let p2 = (ox + num(&mut i), oy + num(&mut i));
                let p3 = (ox + num(&mut i), oy + num(&mut i));
                cubic(&mut path, (x, y), p1, p2, p3);
                last_ctrl = Some(p2);
                (x, y) = p3;
            }
            'S' => {
                let p1 = match last_ctrl {
                    Some((cx, cy)) => (2.0 * x - cx, 2.0 * y - cy),
                    None => (x, y),
                };
                let p2 = (ox + num(&mut i), oy + num(&mut i));
                let p3 = (ox + num(&mut i), oy + num(&mut i));
                cubic(&mut path, (x, y), p1, p2, p3);
                last_ctrl = Some(p2);
                (x, y) = p3;
            }
            'Q' | 'T' => {
                let q = if cmd.eq_ignore_ascii_case(&'Q') {
                    (ox + num(&mut i), oy + num(&mut i))
                } else {
                    match last_ctrl {
                        Some((cx, cy)) => (2.0 * x - cx, 2.0 * y - cy),
                        None => (x, y),
                    }
                };
                let p3 = (ox + num(&mut i), oy + num(&mut i));
                let p1 = (x + 2.0 / 3.0 * (q.0 - x), y + 2.0 / 3.0 * (q.1 - y));
                let p2 = (
                    p3.0 + 2.0 / 3.0 * (q.0 - p3.0),
                    p3.1 + 2.0 / 3.0 * (q.1 - p3.1),
                );
                cubic(&mut path, (x, y), p1, p2, p3);
                last_ctrl = Some(q);
                (x, y) = p3;
            }
            _ => i += 1,
        }
    }
    if path.len() > 1 {
        out.push(path);
    }
    out
}

/// KanjiVG の SVG から筆画を取り出す(`<path ... d="...">` を順に)。座標は 109 × 109。
fn kanjivg(svg: &str) -> Glyph {
    let mut strokes = Vec::new();
    for part in svg.split("<path").skip(1) {
        let Some(start) = part.find(" d=\"") else {
            continue;
        };
        let rest = &part[start + 4..];
        let Some(end) = rest.find('"') else {
            continue;
        };
        for s in parse_path(&rest[..end]) {
            strokes.push(s.into_iter().map(|(x, y)| (x / 109.0, y / 109.0)).collect());
        }
    }
    strokes
}

/// Hershey フォント(.jhf)を読む。ASCII 32 から順に並んでいる。
fn hershey(jhf: &str) -> BTreeMap<char, Glyph> {
    // 1 文字が複数行に折り返されていることがあるので、つなぎ直す。
    let mut records: Vec<String> = Vec::new();
    for line in jhf.lines() {
        let line = line.trim_end_matches('\r');
        if line.len() >= 8 && line[..5].trim().parse::<u32>().is_ok() {
            records.push(line.to_string());
        } else if let Some(last) = records.last_mut() {
            last.push_str(line);
        }
    }
    let mut out = BTreeMap::new();
    for (i, rec) in records.iter().enumerate() {
        let Some(ch) = char::from_u32(32 + i as u32) else {
            continue;
        };
        let body: Vec<u8> = rec.as_bytes()[8..].to_vec();
        if body.len() < 2 {
            continue;
        }
        let v = |b: u8| b as f32 - b'R' as f32;
        let (left, right) = (v(body[0]), v(body[1]));
        let cx = (left + right) / 2.0;
        let mut strokes: Glyph = Vec::new();
        let mut cur: Vec<Pt> = Vec::new();
        for pair in body[2..].chunks_exact(2) {
            if pair == b" R" {
                if cur.len() > 1 {
                    strokes.push(std::mem::take(&mut cur));
                }
                cur.clear();
                continue;
            }
            // 大文字は y = -12(上)〜 9(ベースライン)。全角の枠に収まるよう、
            // 縦 -14 〜 18 を 0〜1 にする(ベースラインは 0.72 あたり)。
            let x = (v(pair[0]) - cx) / 32.0 + 0.5;
            let y = (v(pair[1]) + 14.0) / 32.0;
            cur.push((x, y));
        }
        if cur.len() > 1 {
            strokes.push(cur);
        }
        if !strokes.is_empty() {
            out.insert(ch, strokes);
        }
    }
    out
}

/// KanjiVG にない記号を手で足す。
fn extra(ch: char) -> Option<Glyph> {
    let g: Glyph = match ch {
        '「' => vec![vec![(0.62, 0.12), (0.38, 0.12), (0.38, 0.55)]],
        '」' => vec![vec![(0.62, 0.45), (0.62, 0.88), (0.38, 0.88)]],
        '〜' => vec![vec![
            (0.12, 0.55),
            (0.25, 0.45),
            (0.38, 0.45),
            (0.62, 0.55),
            (0.75, 0.55),
            (0.88, 0.45),
        ]],
        _ => return None,
    };
    Some(g)
}

/// 点を間引く(Ramer–Douglas–Peucker)。
fn simplify(points: &[Pt], eps: f32) -> Vec<Pt> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (a, b) = (points[0], points[points.len() - 1]);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let (mut worst, mut at) = (0f32, 0);
    for (i, p) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        // 始点と終点が同じ(閉じた線)なら、始点からの距離で測る。
        let d = if len < 1e-6 {
            ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt()
        } else {
            ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len
        };
        if d > worst {
            worst = d;
            at = i;
        }
    }
    if worst <= eps {
        return vec![a, b];
    }
    let mut left = simplify(&points[..=at], eps);
    let right = simplify(&points[at..], eps);
    left.pop();
    left.extend(right);
    left
}

fn encode(out: &mut Vec<u8>, ch: char, glyph: &Glyph) {
    let q = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let mut utf8 = [0u8; 4];
    let s = ch.encode_utf8(&mut utf8);
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
    let strokes: Vec<Vec<(u8, u8)>> = glyph
        .iter()
        .map(|s| {
            let mut pts: Vec<(u8, u8)> = simplify(s, 0.004)
                .into_iter()
                .map(|(x, y)| (q(x), q(y)))
                .collect();
            pts.dedup();
            if pts.len() == 1 {
                pts.push(pts[0]);
            }
            pts.truncate(255);
            pts
        })
        .filter(|s| !s.is_empty())
        .take(255)
        .collect();
    out.push(strokes.len() as u8);
    for s in strokes {
        out.push(s.len() as u8);
        for (x, y) in s {
            out.push(x);
            out.push(y);
        }
    }
}

fn main() -> Result<(), String> {
    let mut kanjivg_dir: Option<PathBuf> = None;
    let mut hershey_file: Option<PathBuf> = None;
    let mut chars_file = PathBuf::from("crates/board-core/src/handwriting/chars.txt");
    let mut out_file = PathBuf::from("crates/board-core/src/handwriting/glyphs.bin");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || {
            args.next()
                .map(PathBuf::from)
                .ok_or(format!("{a} の値がありません"))
        };
        match a.as_str() {
            "--kanjivg" => kanjivg_dir = Some(val()?),
            "--hershey" => hershey_file = Some(val()?),
            "--chars" => chars_file = val()?,
            "--out" => out_file = val()?,
            _ => return Err(format!("知らない引数です: {a}")),
        }
    }
    let kanjivg_dir = kanjivg_dir.ok_or("--kanjivg を指定してください")?;
    let hershey_file = hershey_file.ok_or("--hershey を指定してください")?;
    let latin = hershey(&std::fs::read_to_string(&hershey_file).map_err(|e| e.to_string())?);
    let chars = std::fs::read_to_string(&chars_file).map_err(|e| e.to_string())?;
    let mut glyphs: Vec<(char, Glyph)> = Vec::new();
    let mut missing = Vec::new();
    // 「# 」で始まる行は説明。
    let body: String = chars.lines().filter(|l| !l.starts_with("# ")).collect();
    for ch in body.chars().filter(|c| !c.is_whitespace()) {
        if glyphs.iter().any(|(c, _)| *c == ch) {
            continue;
        }
        let glyph = if ch.is_ascii() {
            latin.get(&ch).cloned()
        } else {
            let path = kanjivg_dir.join(format!("{:05x}.svg", ch as u32));
            std::fs::read_to_string(&path)
                .ok()
                .map(|s| kanjivg(&s))
                .filter(|g| !g.is_empty())
                .or_else(|| extra(ch))
        };
        match glyph {
            Some(g) => glyphs.push((ch, g)),
            None => missing.push(ch),
        }
    }
    let mut out = b"HWG1".to_vec();
    out.extend_from_slice(&(glyphs.len() as u16).to_le_bytes());
    for (ch, g) in &glyphs {
        encode(&mut out, *ch, g);
    }
    std::fs::write(&out_file, &out).map_err(|e| e.to_string())?;
    eprintln!(
        "{} 文字({} バイト)を {} に書きました",
        glyphs.len(),
        out.len(),
        out_file.display()
    );
    if !missing.is_empty() {
        eprintln!("見本がない文字: {}", missing.iter().collect::<String>());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kanjivg_path() {
        let s = parse_path("M31.01,33c0.88,0.88,2.75,1.82,5.25,1.75");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0][0], (31.01, 33.0));
        let last = s[0].last().unwrap();
        assert!((last.0 - 36.26).abs() < 1e-3 && (last.1 - 34.75).abs() < 1e-3);
    }

    #[test]
    fn splits_packed_numbers() {
        // KanjiVG は "-0.25,20-2.12" のように区切りを省く。
        let s = parse_path("M0,0l1-2.5.5.5");
        assert_eq!(s[0], vec![(0.0, 0.0), (1.0, -2.5), (1.5, -2.0)]);
    }

    #[test]
    fn keeps_closed_curves() {
        let circle: Vec<Pt> = (0..=16)
            .map(|i| {
                let t = i as f32 / 16.0 * std::f32::consts::TAU;
                (t.cos(), t.sin())
            })
            .collect();
        assert!(simplify(&circle, 0.01).len() > 8);
    }

    #[test]
    fn reads_hershey() {
        let g = hershey("12345  9MWRFRT RRYQZR[SZRY\n");
        // 1 行目は ASCII 32(空白)だが、この行は点がある。
        assert_eq!(g[&' '].len(), 2);
    }
}
