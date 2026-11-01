//! 端末内の手書き文字認識。手書きの線をサーバーに送らずに文字にする。
//!
//! 1. **行に分ける**: 縦の範囲が重なる線を 1 行にまとめる(横書き)。
//! 2. **文字に区切る**: 横に重なる線を「部品」にまとめ、隣り合う部品をいくつつなげて 1 文字にするかを
//!    動的計画法で選ぶ(「い」「川」のように離れた画がある文字も読める)。
//! 3. **文字を当てる**: 線を方向別(横・縦・右上がり・右下がり)の濃淡の地図(256 次元)にし、
//!    見本の文字と比べていちばん近いものを選ぶ(方向線素特徴 + 最近傍法)。
//!    見本は英数字・記号・ひらがな・カタカナ・小学校で習う漢字(約 1,300 字、[`glyphs.bin`])。
//! 4. **自分の字を覚える**: 正しい文字を教えると、その人の線も見本に加える([`Sample`])。
//!    見本は端末に保存し、次から優先して使う。
//!
//! 自信がない行は文字にしない(線のまま残す)。見本のデータは KanjiVG(CC BY-SA 3.0)と
//! Hershey フォントから `tools/handwriting-data` で作る。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::element::{ElementId, Stroke};
use crate::geom::Rect;
use crate::ink::{self, InkItem, InkKind};

/// 見本の文字の筆画データ(`tools/handwriting-data` で作る)。
static GLYPHS: &[u8] = include_bytes!("glyphs.bin");

/// 特徴を作るときの格子の一辺。
const GRID: usize = 32;
/// 特徴の格子(縮めたあと)の一辺。
const CELLS: usize = 8;
/// 方向の数(横・右下がり・縦・右上がり)。
const DIRS: usize = 4;
/// 特徴の次元。
pub const DIM: usize = DIRS * CELLS * CELLS;
/// 1 文字にまとめる部品の数の上限。
const MAX_PARTS: usize = 6;
/// 1 文字の幅の上限(行の高さに対する比)。
const MAX_CHAR_WIDTH: f32 = 1.6;
/// 文字を 1 つ増やすごとに足すコスト(細かく区切りすぎないように)。
/// 各文字の距離は幅(次の文字までの間隔)で重み付けして足すので、区切り方によらず
/// 「行全体の平均の距離」を比べることになる。
const CHAR_COST: f32 = 0.06;
/// 文字の候補にする図形の大きさ(ペンの太さの何倍まで)。
const SMALL_FOR_PEN: f32 = 25.0;
/// 行として受け入れる、1 文字あたりの平均の距離の上限。
pub const ACCEPT: f32 = 0.42;

/// 文字の種類(前後の文字とそろえるのに使う)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Script {
    Latin,
    Digit,
    Hiragana,
    Katakana,
    Kanji,
    Symbol,
}

fn script(c: char) -> Script {
    match c {
        '0'..='9' => Script::Digit,
        'a'..='z' | 'A'..='Z' => Script::Latin,
        '\u{3041}'..='\u{309F}' => Script::Hiragana,
        '\u{30A0}'..='\u{30FF}' if c != 'ー' && c != '・' => Script::Katakana,
        '\u{4E00}'..='\u{9FFF}' | '々' => Script::Kanji,
        _ => Script::Symbol,
    }
}

/// 1 文字ぶんの特徴。
#[derive(Clone, Debug)]
pub struct Feature {
    v: Vec<f32>,
    /// 幅 / 高さ の対数(-1.4〜1.4 に丸める)。
    aspect: f32,
    /// 字の高さ / 行の高さ(見本では、全角の枠に対する高さを文字の種類ごとにそろえたもの)。
    height: f32,
    /// 横に離れた部品の数(「い」は 2、「川」は 3)。
    parts: usize,
}

impl Feature {
    /// 線(ワールド座標)から作る。`line_height` は行の高さ(わからなければ 0)。
    pub fn from_strokes(strokes: &[&[Vec2]], line_height: f32) -> Option<Self> {
        let pts = strokes.iter().flat_map(|s| s.iter());
        let mut min = Vec2::splat(f32::MAX);
        let mut max = Vec2::splat(f32::MIN);
        for p in pts {
            min = min.min(*p);
            max = max.max(*p);
        }
        if min.x > max.x {
            return None;
        }
        let size = (max - min).max(Vec2::splat(1e-3));
        let side = size.max_element();
        let center = (min + max) * 0.5;
        let scale = (GRID as f32 - 6.0) / side;
        let to_grid = |p: Vec2| (p - center) * scale + Vec2::splat(GRID as f32 / 2.0);
        let mut planes = vec![0f32; DIRS * GRID * GRID];
        let mut splat = |p: Vec2, dir: usize, w: f32| {
            let x = p.x.clamp(0.0, GRID as f32 - 1.001);
            let y = p.y.clamp(0.0, GRID as f32 - 1.001);
            let (x0, y0) = (x.floor() as usize, y.floor() as usize);
            let (fx, fy) = (x - x0 as f32, y - y0 as f32);
            let plane = &mut planes[dir * GRID * GRID..(dir + 1) * GRID * GRID];
            plane[y0 * GRID + x0] += w * (1.0 - fx) * (1.0 - fy);
            plane[y0 * GRID + x0 + 1] += w * fx * (1.0 - fy);
            plane[(y0 + 1) * GRID + x0] += w * (1.0 - fx) * fy;
            plane[(y0 + 1) * GRID + x0 + 1] += w * fx * fy;
        };
        for s in strokes {
            if s.len() == 1 || s.windows(2).all(|w| (w[1] - w[0]).length() * scale < 0.5) {
                // 点(「、」や濁点)。どの方向にも少しずつ。
                let p = to_grid(s[0]);
                for d in 0..DIRS {
                    splat(p, d, 0.5);
                }
                continue;
            }
            for w in s.windows(2) {
                let (a, b) = (to_grid(w[0]), to_grid(w[1]));
                let d = b - a;
                let len = d.length();
                if len < 1e-4 {
                    continue;
                }
                // 向き(0〜π)を 4 方向に振り分ける(隣の 2 方向に按分)。
                let mut theta = d.y.atan2(d.x);
                if theta < 0.0 {
                    theta += std::f32::consts::PI;
                }
                let pos = theta / (std::f32::consts::PI / DIRS as f32);
                let i0 = pos.floor() as usize % DIRS;
                let i1 = (i0 + 1) % DIRS;
                let f = pos - pos.floor();
                let steps = (len / 0.5).ceil() as usize;
                let wstep = len / steps as f32;
                for k in 0..steps {
                    let p = a + d * ((k as f32 + 0.5) / steps as f32);
                    splat(p, i0, wstep * (1.0 - f));
                    splat(p, i1, wstep * f);
                }
            }
        }
        let v = pool(&planes);
        let aspect = (size.x / size.y).ln().clamp(-1.4, 1.4);
        let height = if line_height > 0.0 {
            (size.y / line_height).min(1.5)
        } else {
            0.0
        };
        // 小さな部品(濁点・点)は数えない。
        let parts = parts(strokes)
            .iter()
            .filter(|p| (p.right - p.left).max(p.bottom - p.top) > 0.35 * side)
            .count();
        Some(Self {
            v,
            aspect,
            height,
            parts,
        })
    }

    /// 見本(`self`)と書かれた字(`other`)の距離(0 に近いほど似ている)。
    fn distance(&self, other: &Feature) -> f32 {
        let dot: f32 = self.v.iter().zip(&other.v).map(|(a, b)| a * b).sum();
        let mut d = 1.0 - dot;
        d += 0.12 * (self.aspect - other.aspect).abs();
        if self.height > 0.0 && other.height > 0.0 {
            d += 0.4 * (self.height - other.height).abs();
        }
        // 見本より部品が多い(離れた線をまとめすぎた)なら不利にする。「ll」を「I」と読まないように。
        d += 0.1 * other.parts.saturating_sub(self.parts) as f32;
        d
    }
}

/// ぼかして 8 × 8 に縮め、平方根をとって長さ 1 にする。
fn pool(planes: &[f32]) -> Vec<f32> {
    const K: [f32; 7] = [0.03, 0.11, 0.22, 0.28, 0.22, 0.11, 0.03];
    let step = GRID / CELLS;
    let mut out = Vec::with_capacity(DIM);
    for d in 0..DIRS {
        let plane = &planes[d * GRID * GRID..(d + 1) * GRID * GRID];
        for cy in 0..CELLS {
            for cx in 0..CELLS {
                // 格子の中心のまわりを重み付きで足す。
                let (gx, gy) = (cx * step + step / 2, cy * step + step / 2);
                let mut s = 0f32;
                for (ky, wy) in K.iter().enumerate() {
                    for (kx, wx) in K.iter().enumerate() {
                        let x = gx as isize + kx as isize - 3;
                        let y = gy as isize + ky as isize - 3;
                        if (0..GRID as isize).contains(&x) && (0..GRID as isize).contains(&y) {
                            s += plane[y as usize * GRID + x as usize] * wx * wy;
                        }
                    }
                }
                out.push(s.sqrt());
            }
        }
    }
    let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    for x in &mut out {
        *x /= norm;
    }
    out
}

/// 部品をまとめた 1 文字ぶんの候補: (近い見本と距離, 線, 幅の重み)。
type Segment = (Vec<(usize, f32)>, Vec<usize>, f32);

/// 区切りで選んだ 1 文字。
struct Picked {
    /// 近い順の (見本, 距離)。
    alts: Vec<(usize, f32)>,
    chosen: usize,
    strokes: Vec<usize>,
}

/// 見本 1 つ。
#[derive(Clone, Debug)]
struct Proto {
    text: String,
    script: Script,
    feature: Feature,
    /// 利用者が教えた見本。
    user: bool,
}

/// 利用者が教えた文字の見本(端末に JSON で保存する)。座標は文字の枠の中で 0〜1。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub text: String,
    pub strokes: Vec<Vec<[f32; 2]>>,
    /// 字の高さ / 行の高さ。
    #[serde(default)]
    pub height: f32,
}

/// 認識した 1 文字。
#[derive(Clone, Debug, PartialEq)]
pub struct Char {
    pub text: String,
    /// この文字の線(入力の添字)。
    pub strokes: Vec<usize>,
    pub distance: f32,
}

/// 認識した 1 行。
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub chars: Vec<Char>,
    /// 1 文字あたりの平均の距離。
    pub distance: f32,
}

impl Line {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for c in &self.chars {
            out.push_str(&c.text);
        }
        out
    }

    pub fn strokes(&self) -> Vec<usize> {
        self.chars.iter().flat_map(|c| c.strokes.iter().copied()).collect()
    }
}

pub struct Recognizer {
    protos: Vec<Proto>,
}

/// 見本の文字の高さの基準(全角の枠に対する、その種類の文字のふつうの高さ)。
fn reference_height(c: char) -> f32 {
    match script(c) {
        // Hershey: 大文字の高さ 21 / 32。
        Script::Latin | Script::Digit => 21.0 / 32.0,
        Script::Symbol if c.is_ascii() => 21.0 / 32.0,
        // KanjiVG: 漢字はおよそ枠の 0.8。
        _ => 0.8,
    }
}

/// `glyphs.bin` を読む。
fn decode_glyphs(bytes: &[u8]) -> Vec<(char, Vec<Vec<Vec2>>)> {
    let mut out = Vec::new();
    if bytes.len() < 6 || &bytes[..4] != b"HWG1" {
        return out;
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut i = 6;
    let mut next = || {
        let b = bytes.get(i).copied();
        i += 1;
        b
    };
    for _ in 0..count {
        let Some(len) = next() else { break };
        let utf8: Option<Vec<u8>> = (0..len).map(|_| next()).collect();
        let Some(ch) = utf8
            .and_then(|u| String::from_utf8(u).ok())
            .and_then(|s| s.chars().next())
        else {
            break;
        };
        let Some(n) = next() else { break };
        let mut strokes = Vec::new();
        for _ in 0..n {
            let Some(np) = next() else { break };
            let pts: Option<Vec<Vec2>> = (0..np)
                .map(|_| Some(Vec2::new(next()? as f32 / 255.0, next()? as f32 / 255.0)))
                .collect();
            strokes.push(pts.unwrap_or_default());
        }
        out.push((ch, strokes));
    }
    out
}

impl Recognizer {
    /// 組み込みの見本だけの認識器(最初に呼んだときに作る)。
    pub fn builtin() -> &'static Recognizer {
        static R: OnceLock<Recognizer> = OnceLock::new();
        R.get_or_init(|| Recognizer {
            protos: decode_glyphs(GLYPHS)
                .into_iter()
                .filter_map(|(c, strokes)| {
                    let refs: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
                    let mut feature = Feature::from_strokes(&refs, 0.0)?;
                    let (min, max) = bounds(&refs)?;
                    feature.height = ((max.y - min.y) / reference_height(c)).min(1.5);
                    Some(Proto {
                        text: c.to_string(),
                        script: script(c),
                        feature,
                        user: false,
                    })
                })
                .collect(),
        })
    }

    /// 組み込みの見本に、利用者の見本を足した認識器。
    pub fn with_samples(samples: &[Sample]) -> Recognizer {
        let mut protos = Recognizer::builtin().protos.clone();
        for s in samples {
            let strokes: Vec<Vec<Vec2>> = s
                .strokes
                .iter()
                .map(|st| st.iter().map(|p| Vec2::new(p[0], p[1])).collect())
                .collect();
            let refs: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
            let Some(mut feature) = Feature::from_strokes(&refs, 0.0) else {
                continue;
            };
            feature.height = s.height;
            protos.push(Proto {
                script: s.text.chars().next().map(script).unwrap_or(Script::Symbol),
                text: s.text.clone(),
                feature,
                user: true,
            });
        }
        Recognizer { protos }
    }

    /// 見本の文字数。
    pub fn len(&self) -> usize {
        self.protos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.protos.is_empty()
    }

    /// 近い順に `k` 個の候補(文字, 距離, 種類)。同じ文字は 1 回だけ。
    fn candidates(&self, f: &Feature, k: usize) -> Vec<(usize, f32)> {
        let mut best: Vec<(usize, f32)> = Vec::with_capacity(k + 1);
        for (i, p) in self.protos.iter().enumerate() {
            let mut d = p.feature.distance(f);
            if p.user {
                // 本人の字は少し優先する。
                d *= 0.85;
            }
            if best.len() == k && d >= best[k - 1].1 {
                continue;
            }
            if let Some(j) = best.iter().position(|(b, _)| self.protos[*b].text == p.text) {
                if d < best[j].1 {
                    best.remove(j);
                } else {
                    continue;
                }
            }
            let at = best.partition_point(|(_, bd)| *bd <= d);
            best.insert(at, (i, d));
            best.truncate(k);
        }
        best
    }

    /// 1 文字として読む。近い順の (文字, 距離)。
    pub fn classify(&self, strokes: &[&[Vec2]], line_height: f32, k: usize) -> Vec<(String, f32)> {
        let Some(f) = Feature::from_strokes(strokes, line_height) else {
            return Vec::new();
        };
        self.candidates(&f, k)
            .into_iter()
            .map(|(i, d)| (self.protos[i].text.clone(), d))
            .collect()
    }

    /// 1 行の線を文字列として読む。`strokes` は 1 行ぶん(順不同)。
    pub fn read_line(&self, strokes: &[&[Vec2]]) -> Option<Line> {
        self.read_line_with(strokes, None)
    }

    /// `expect` があれば、その文字数・その文字になるように区切る(教えるとき)。
    fn read_line_with(&self, strokes: &[&[Vec2]], expect: Option<&[String]>) -> Option<Line> {
        let parts = parts(strokes);
        if parts.is_empty() {
            return None;
        }
        let line_h = line_height(strokes, &parts);
        let n = parts.len();
        // cand[i][len] = 部品 i から len 個をまとめたときの候補。
        let mut cand: Vec<Vec<Option<Segment>>> = vec![vec![None; MAX_PARTS + 1]; n];
        for i in 0..n {
            let mut ids: Vec<usize> = Vec::new();
            let mut left = f32::MAX;
            let mut right = f32::MIN;
            for len in 1..=MAX_PARTS.min(n - i) {
                let p = &parts[i + len - 1];
                // 部品の間が大きく空いていたら、そこから先はまとめない。
                if len > 1 && p.left - right > 0.8 * line_h {
                    break;
                }
                ids.extend(&p.strokes);
                left = left.min(p.left);
                right = right.max(p.right);
                if len > 1 && right - left > MAX_CHAR_WIDTH * line_h {
                    break;
                }
                let refs: Vec<&[Vec2]> = ids.iter().map(|&s| strokes[s]).collect();
                let Some(f) = Feature::from_strokes(&refs, line_h) else {
                    continue;
                };
                let width = right - left;
                let wide = (width / line_h - 1.1).max(0.0);
                let k = if expect.is_some() { self.protos.len() } else { 5 };
                let mut c = self.candidates(&f, k.min(self.protos.len()));
                if wide > 0.0 {
                    // 行の高さより幅の広い字はまれ(2 文字をまとめてしまうのを防ぐ)。
                    for (_, d) in &mut c {
                        *d += 0.6 * wide;
                    }
                }
                if len > 1 {
                    // まとめるほど少し不利にする(部品の間の空きに比例)。
                    let gaps: f32 = (i + 1..i + len)
                        .map(|j| (parts[j].left - parts[j - 1].right).max(0.0))
                        .sum();
                    let pen = 0.15 * gaps / line_h;
                    for (_, d) in &mut c {
                        *d += pen;
                    }
                }
                let next = parts.get(i + len).map_or(right, |p| p.left.max(right));
                let weight = ((next - left) / line_h).clamp(0.3, 2.0);
                cand[i][len] = Some((c, ids.clone(), weight));
            }
        }
        // 動的計画法。best[i] = 部品 i までの最小コスト。教えるときは文字数ごとに持つ。
        let chars_max = expect.map_or(n, <[String]>::len);
        let inf = f32::MAX / 4.0;
        // best[i][m] = 部品 i 個を m 文字にしたときの最小コストと、直前の (i, 候補)。
        let mut best = vec![vec![(inf, 0usize, 0usize); chars_max + 1]; n + 1];
        best[0][0].0 = 0.0;
        for i in 0..n {
            for m in 0..chars_max {
                let base = best[i][m].0;
                if base >= inf {
                    continue;
                }
                for len in 1..=MAX_PARTS.min(n - i) {
                    let Some((c, _, weight)) = &cand[i][len] else {
                        continue;
                    };
                    let pick = match expect {
                        Some(exp) => c
                            .iter()
                            .position(|(p, _)| self.protos[*p].text == exp[m])
                            .map(|j| (j, c[j].1)),
                        None => c.first().map(|(_, d)| (0, *d)),
                    };
                    let Some((j, d)) = pick else { continue };
                    let cost = base + d * weight + CHAR_COST;
                    if cost < best[i + len][m + 1].0 {
                        best[i + len][m + 1] = (cost, len, j);
                    }
                }
            }
        }
        let m = match expect {
            Some(exp) => exp.len(),
            None => (1..=chars_max).min_by(|&a, &b| best[n][a].0.total_cmp(&best[n][b].0))?,
        };
        if best[n][m].0 >= inf {
            return None;
        }
        // たどり直す。
        let mut chars = Vec::new();
        let (mut i, mut mm) = (n, m);
        while i > 0 {
            let (_, len, j) = best[i][mm];
            let start = i - len;
            let (c, ids, _) = cand[start][len].as_ref()?;
            chars.push(Picked {
                alts: c.clone(),
                chosen: j,
                strokes: ids.clone(),
            });
            i = start;
            mm -= 1;
        }
        chars.reverse();
        let mut out: Vec<Char> = chars
            .iter()
            .map(|p| Char {
                text: self.protos[p.alts[p.chosen].0].text.clone(),
                strokes: p.strokes.clone(),
                distance: p.alts[p.chosen].1,
            })
            .collect();
        if expect.is_none() {
            self.harmonize(&mut out, &chars);
            small_kana(&mut out, strokes, line_h);
            insert_spaces(&mut out, strokes, line_h);
        }
        let distance = out.iter().map(|c| c.distance).sum::<f32>() / out.len().max(1) as f32;
        Some(Line {
            chars: out,
            distance,
        })
    }

    /// 形がほぼ同じ文字(カ/力、ロ/口、ー/一 …)は、前後の文字の種類にそろえる。
    fn harmonize(&self, out: &mut [Char], chars: &[Picked]) {
        self.harmonize_scripts(out, chars);
        // 線 1 本の字や丸は、前後の文字で決める(直した結果を次の文字の判断に使う)。
        let kind = |t: &str| -> Option<char> {
            let c = t.chars().next()?;
            Some(match script(c) {
                Script::Hiragana | Script::Katakana => 'k',
                Script::Kanji => 'K',
                Script::Digit => 'd',
                Script::Latin if c.is_ascii_lowercase() => 'a',
                Script::Latin => 'A',
                Script::Symbol => return None,
            })
        };
        for k in 0..out.len() {
            let ctx: Vec<char> = [k.wrapping_sub(1), k + 1]
                .iter()
                .filter_map(|&i| out.get(i).and_then(|c| kind(&c.text)))
                .collect();
            let has = |c: char| ctx.contains(&c);
            let pick = match out[k].text.as_str() {
                "-" | "一" | "ー" | "_" => {
                    if has('k') {
                        "ー"
                    } else if has('K') {
                        "一"
                    } else if has('a') || has('A') || has('d') {
                        "-"
                    } else {
                        continue;
                    }
                }
                "I" | "l" | "1" | "|" => {
                    if has('d') {
                        "1"
                    } else if has('a') {
                        "l"
                    } else if has('A') {
                        "I"
                    } else {
                        continue;
                    }
                }
                "O" | "0" | "o" => {
                    if has('d') {
                        "0"
                    } else if has('a') {
                        "o"
                    } else if has('A') {
                        "O"
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            out[k].text = pick.to_string();
        }
    }

    fn harmonize_scripts(&self, out: &mut [Char], chars: &[Picked]) {
        let scripts: Vec<Script> = chars
            .iter()
            .map(|p| self.protos[p.alts[p.chosen].0].script)
            .collect();
        for (k, p) in chars.iter().enumerate() {
            let alts = &p.alts;
            let chosen = alts[p.chosen];
            let neighbors: Vec<Script> = [k.wrapping_sub(1), k + 1]
                .iter()
                .filter_map(|&i| scripts.get(i).copied())
                .filter(|s| *s != Script::Symbol)
                .collect();
            if neighbors.is_empty() || neighbors.contains(&scripts[k]) {
                continue;
            }
            if let Some(alt) = alts.iter().find(|(p, d)| {
                *d - chosen.1 < 0.04 && neighbors.contains(&self.protos[*p].script)
            }) {
                out[k].text = self.protos[alt.0].text.clone();
                out[k].distance = alt.1;
            }
        }
    }

    /// 線と正しい文字列から、文字ごとの見本を作る(自分の字を覚える)。
    /// 線を文字数ぶんに区切れなければ `None`。
    pub fn teach(&self, strokes: &[&[Vec2]], text: &str) -> Option<Vec<Sample>> {
        let expect: Vec<String> = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_string())
            .collect();
        if expect.is_empty() {
            return None;
        }
        // 知らない文字は、仮に組み込みの見本に足してから区切る。
        let unknown: Vec<&String> = expect
            .iter()
            .filter(|t| !self.protos.iter().any(|p| &p.text == *t))
            .collect();
        let line = if unknown.is_empty() {
            self.read_line_with(strokes, Some(&expect))?
        } else {
            // 知らない文字があるときは、区切りだけ形で決める(読み取りの文字数がそろえば使う)。
            let read = self.read_line(strokes)?;
            if read.chars.len() != expect.len() {
                return None;
            }
            Line {
                chars: read
                    .chars
                    .into_iter()
                    .zip(&expect)
                    .map(|(c, t)| Char {
                        text: t.clone(),
                        ..c
                    })
                    .collect(),
                distance: 0.0,
            }
        };
        let line_h = line_height(strokes, &parts(strokes));
        Some(
            line.chars
                .iter()
                .filter_map(|c| {
                    let refs: Vec<&[Vec2]> = c.strokes.iter().map(|&i| strokes[i]).collect();
                    let (min, max) = bounds(&refs)?;
                    let side = (max - min).max_element().max(1e-3);
                    let center = (min + max) * 0.5;
                    Some(Sample {
                        text: c.text.clone(),
                        strokes: refs
                            .iter()
                            .map(|s| {
                                s.iter()
                                    .map(|p| {
                                        let q = (*p - center) / side + Vec2::splat(0.5);
                                        [round3(q.x), round3(q.y)]
                                    })
                                    .collect()
                            })
                            .collect(),
                        height: ((max.y - min.y) / line_h).min(1.5),
                    })
                })
                .collect(),
        )
    }
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

fn bounds(strokes: &[&[Vec2]]) -> Option<(Vec2, Vec2)> {
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    for p in strokes.iter().flat_map(|s| s.iter()) {
        min = min.min(*p);
        max = max.max(*p);
    }
    (min.x <= max.x).then_some((min, max))
}

/// 横に重なる線をまとめた部品。
#[derive(Clone, Debug)]
struct Part {
    strokes: Vec<usize>,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

/// 線を左から並べ、横に大きく重なるものを部品にまとめる。
fn parts(strokes: &[&[Vec2]]) -> Vec<Part> {
    let mut items: Vec<Part> = strokes
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let (min, max) = bounds(&[s])?;
            Some(Part {
                strokes: vec![i],
                left: min.x,
                right: max.x,
                top: min.y,
                bottom: max.y,
            })
        })
        .collect();
    items.sort_by(|a, b| (a.left + a.right).total_cmp(&(b.left + b.right)));
    let mut out: Vec<Part> = Vec::new();
    for it in items {
        if let Some(last) = out.last_mut() {
            let overlap = last.right.min(it.right) - last.left.max(it.left);
            let narrow = (last.right - last.left).min(it.right - it.left).max(1e-3);
            if overlap > 0.4 * narrow {
                last.strokes.extend(it.strokes);
                last.left = last.left.min(it.left);
                last.right = last.right.max(it.right);
                last.top = last.top.min(it.top);
                last.bottom = last.bottom.max(it.bottom);
                continue;
            }
        }
        out.push(it);
    }
    out
}

/// 行の高さ(文字の大きさの目安)。部品の大きいほうから 1/4 の位置の値。
fn line_height(strokes: &[&[Vec2]], parts: &[Part]) -> f32 {
    // 仮名・漢字はほぼ正方形なので、縦横の長いほうで測る(「ー」「二」も行の高さに数える)。
    let mut hs: Vec<f32> = parts
        .iter()
        .map(|p| (p.bottom - p.top).max(p.right - p.left))
        .collect();
    hs.sort_by(|a, b| b.total_cmp(a));
    let h = hs.get(hs.len() / 4).copied().unwrap_or(0.0);
    if h > 1e-3 {
        return h;
    }
    bounds(strokes).map_or(1.0, |(a, b)| (b.y - a.y).max(1.0))
}

/// 小書きの仮名(ッ・ャ・ィ …)は形がふつうの仮名と同じなので、大きさで決める。
fn small_kana(chars: &mut [Char], strokes: &[&[Vec2]], line_h: f32) {
    const BIG: &str = "あいうえおつやゆよわアイウエオツヤユヨワ";
    const SMALL: &str = "ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮ";
    let boxes: Vec<Option<(Vec2, Vec2)>> = chars
        .iter()
        .map(|c| {
            let refs: Vec<&[Vec2]> = c.strokes.iter().map(|&i| strokes[i]).collect();
            bounds(&refs)
        })
        .collect();
    // 行の中心(各文字の中心の中央値)。小書きの仮名は小さく、下に寄っている。
    let mut mids: Vec<f32> = boxes.iter().flatten().map(|(a, b)| (a.y + b.y) / 2.0).collect();
    mids.sort_by(f32::total_cmp);
    let Some(&line_mid) = mids.get(mids.len() / 2) else {
        return;
    };
    for k in 0..chars.len() {
        let (Some(c), Some((min, max))) = (chars[k].text.chars().next(), boxes[k]) else {
            continue;
        };
        let ratio = (max - min).max_element() / line_h;
        let low = ((min.y + max.y) / 2.0 - line_mid) / line_h;
        let looks_small = ratio < 0.86 && low > 0.06;
        if let Some(i) = BIG.chars().position(|b| b == c)
            && looks_small
            && k > 0
        {
            chars[k].text = SMALL.chars().nth(i).unwrap_or(c).to_string();
        } else if let Some(i) = SMALL.chars().position(|b| b == c)
            && (!looks_small || k == 0)
        {
            chars[k].text = BIG.chars().nth(i).unwrap_or(c).to_string();
        }
    }
}

/// 英数字のあいだが大きく空いていたら空白を入れる。
fn insert_spaces(chars: &mut Vec<Char>, strokes: &[&[Vec2]], line_h: f32) {
    let edges: Vec<Option<(f32, f32)>> = chars
        .iter()
        .map(|c| {
            let refs: Vec<&[Vec2]> = c.strokes.iter().map(|&i| strokes[i]).collect();
            bounds(&refs).map(|(a, b)| (a.x, b.x))
        })
        .collect();
    let mut k = chars.len();
    while k > 1 {
        k -= 1;
        let ascii = |c: &Char| c.text.chars().all(|ch| ch.is_ascii_alphanumeric());
        if let (Some(prev), Some(cur)) = (edges[k - 1], edges[k])
            && ascii(&chars[k - 1])
            && ascii(&chars[k])
            && cur.0 - prev.1 > 0.5 * line_h
        {
            chars.insert(
                k,
                Char {
                    text: " ".into(),
                    strokes: Vec::new(),
                    distance: 0.0,
                },
            );
        }
    }
}

/// 線を行に分ける(横書き)。各行は線の添字。上の行から順。
pub fn lines(strokes: &[&[Vec2]]) -> Vec<Vec<usize>> {
    let mut items: Vec<(usize, Rect)> = strokes
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let (min, max) = bounds(&[s])?;
            Some((i, Rect::from_points(min, max)))
        })
        .collect();
    // 背の高い線から行を作り、小さな線(点など)は後で近い行に入れる。
    items.sort_by(|a, b| b.1.size().y.total_cmp(&a.1.size().y));
    let mut rows: Vec<(f32, f32, Vec<usize>)> = Vec::new();
    for (i, r) in items {
        let (top, bottom) = (r.min.y, r.max.y);
        let h = (bottom - top).max(1e-3);
        let best = rows
            .iter_mut()
            .map(|row| {
                let overlap = row.1.min(bottom) - row.0.max(top);
                let ratio = overlap / h.min(row.1 - row.0).max(1e-3);
                (ratio, row)
            })
            .filter(|(ratio, _)| *ratio > 0.35)
            .max_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((_, row)) => {
                // 小さな線(「、」など)で行の範囲を広げない。
                if h > 0.3 * (row.1 - row.0) {
                    row.0 = row.0.min(top);
                    row.1 = row.1.max(bottom);
                }
                row.2.push(i);
            }
            None => rows.push((top, bottom, vec![i])),
        }
    }
    // 小さな線だけの行(濁点・「、」が行の上下にはみ出したもの)は、近くの行に入れる。
    let tall = rows.iter().map(|r| r.1 - r.0).fold(0f32, f32::max);
    let mut i = 0;
    while i < rows.len() {
        let (top, bottom) = (rows[i].0, rows[i].1);
        if rows.len() > 1 && bottom - top < 0.45 * tall {
            let near = rows
                .iter()
                .enumerate()
                .filter(|(j, r)| *j != i && r.1 - r.0 >= 0.45 * tall)
                .map(|(j, r)| (j, (r.0 - bottom).max(top - r.1).max(0.0)))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((j, gap)) = near
                && gap < 0.5 * (rows[j].1 - rows[j].0)
            {
                let moved = std::mem::take(&mut rows[i].2);
                rows[j].2.extend(moved);
                rows.remove(i);
                continue;
            }
        }
        i += 1;
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|r| r.2).collect()
}

/// 認識した手書きの文字(1 行ぶん)。
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    pub text: String,
    pub strokes: Vec<ElementId>,
    pub distance: f32,
}

/// ストロークから文字を読む。自信のある行だけを返す(読めない線はそのまま残す)。
pub fn recognize(strokes: &[&Stroke], rec: &Recognizer) -> Vec<TextItem> {
    let pts: Vec<&[Vec2]> = strokes.iter().map(|s| s.points.as_slice()).collect();
    let mut out = Vec::new();
    for row in lines(&pts) {
        let refs: Vec<&[Vec2]> = row.iter().map(|&i| pts[i]).collect();
        let Some(line) = rec.read_line(&refs) else {
            continue;
        };
        let text = line.text().trim().to_string();
        if text.is_empty() || line.distance > ACCEPT {
            continue;
        }
        out.push(TextItem {
            text,
            strokes: line.strokes().into_iter().map(|k| strokes[row[k]].id).collect(),
            distance: line.distance,
        });
    }
    out
}

/// 手書きの線を図形と文字に分けて読む(端末だけの「図形に整形」)。
///
/// まず [`crate::ink::recognize`] で図形・線を認識し、残った線から文字の行を読む。
/// 文字の行の近くにある小さな「図形」(「O」「L」「口」など)は文字の候補に戻して一緒に読み、
/// 文字として読めなかったときは図形のままにする。
pub fn recognize_ink(strokes: &[&Stroke], rec: &Recognizer) -> Vec<InkItem> {
    let shapes = ink::recognize(strokes);
    let used: HashSet<ElementId> = shapes.iter().flat_map(|i| i.strokes.iter().copied()).collect();
    let leftover: Vec<&Stroke> = strokes
        .iter()
        .copied()
        .filter(|s| !s.highlighter && !used.contains(&s.id))
        .collect();
    let by_id: HashMap<ElementId, &Stroke> = strokes.iter().map(|s| (s.id, *s)).collect();
    let item_bounds = |it: &InkItem| -> Option<Rect> {
        it.strokes
            .iter()
            .filter_map(|id| by_id.get(id).map(|s| s.bounds()))
            .reduce(|a, b| a.union(&b))
    };
    // 残りの線の行(範囲と高さ)。
    let pts: Vec<&[Vec2]> = leftover.iter().map(|s| s.points.as_slice()).collect();
    let rows: Vec<(Rect, f32)> = lines(&pts)
        .into_iter()
        .filter_map(|row| {
            let refs: Vec<&[Vec2]> = row.iter().map(|&i| pts[i]).collect();
            let (min, max) = bounds(&refs)?;
            Some((Rect::from_points(min, max), line_height(&refs, &parts(&refs))))
        })
        .collect();
    let (mut keep, mut maybe): (Vec<InkItem>, Vec<InkItem>) = (Vec::new(), Vec::new());
    for it in shapes {
        let Some(b) = item_bounds(&it) else {
            keep.push(it);
            continue;
        };
        let near_text = rows.iter().any(|(r, h)| {
            let c = b.center();
            let dx = (r.min.x - b.max.x).max(b.min.x - r.max.x).max(0.0);
            b.size().max_element() <= 1.6 * h
                && c.y >= r.min.y - 0.5 * h
                && c.y <= r.max.y + 0.5 * h
                && dx <= 1.5 * h
        });
        // 文字の線が残っていなくても、ペンの太さに比べて小さな図形は文字の候補にする
        // (「OK」がすべて丸と線に読まれた場合など)。
        let width = it
            .strokes
            .iter()
            .filter_map(|id| by_id.get(id).map(|s| s.width))
            .fold(0f32, f32::max)
            .max(1.0);
        let small = b.size().max_element() <= SMALL_FOR_PEN * width;
        if near_text || small {
            maybe.push(it)
        } else {
            keep.push(it)
        }
    }
    if maybe.is_empty() && leftover.is_empty() {
        return keep;
    }
    let mut pool = leftover;
    pool.extend(
        maybe
            .iter()
            .flat_map(|it| it.strokes.iter().filter_map(|id| by_id.get(id).copied())),
    );
    let only_shapes: HashSet<ElementId> = maybe.iter().flat_map(|i| i.strokes.iter().copied()).collect();
    let texts: Vec<TextItem> = recognize(&pool, rec)
        .into_iter()
        // 図形だけでできた行は、2 文字以上のときだけ文字にする(丸 1 つを「O」にしない)。
        .filter(|t| t.text.chars().count() >= 2 || !t.strokes.iter().all(|id| only_shapes.contains(id)))
        .collect();
    let taken: HashSet<ElementId> = texts.iter().flat_map(|t| t.strokes.iter().copied()).collect();
    // 文字に使われなかった図形は図形のまま。
    keep.extend(
        maybe
            .into_iter()
            .filter(|it| !it.strokes.iter().any(|id| taken.contains(id))),
    );
    keep.extend(texts.into_iter().map(|t| InkItem {
        strokes: t.strokes,
        kind: InkKind::Text(t.text),
    }));
    keep
}

/// 利用者の見本を文字ごとに数える(設定画面の表示用)。
pub fn sample_counts(samples: &[Sample]) -> Vec<(String, usize)> {
    let mut m: HashMap<&str, usize> = HashMap::new();
    for s in samples {
        *m.entry(&s.text).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = m.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests;
