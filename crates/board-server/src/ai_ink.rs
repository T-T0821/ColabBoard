//! 手書きの図を Claude に読んでもらい、整った図形にする。
//!
//! `POST /api/boards/{id}/ai/ink?key=` に `{ "ids": ["ストロークID", …] }` を送ると、
//! `{ "items": [{ "kind": "rect" | "line" | "arrow" | "text" | …, "strokes": [ID…], "text": "…" }] }` を返す。
//!
//! ストロークはサーバーがルームの状態から読み、白地に黒で PNG に描いて Claude に見せる
//! (手書きの文字も読める)。各ストロークの ID と画像上の位置も一緒に渡し、どのストロークが
//! どの図形かを答えてもらう。要素の組み立ては端末だけの認識と同じ [`board_core::ink::build`] が
//! クライアントで行う(自分の操作として Undo でき、そのまま同期される)。

use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use base64::Engine as _;
use board_core::ink::{AI_SHAPE_KINDS, InkKind};
use board_core::{Board, Element, Rect, Stroke, Vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ai::{AiError, AiRequest, id_hex};
use crate::{AppState, api};

/// 1 回に送るストロークの上限。
pub const MAX_STROKES: usize = 400;
/// 1 回に送る点の合計の上限。
pub const MAX_POINTS: usize = 60_000;
/// 画像の長い辺(ピクセル)。
const IMAGE_SIZE: f32 = 1024.0;
/// 画像の余白(ピクセル)。
const IMAGE_PAD: f32 = 24.0;

const SYSTEM: &str = "\
You clean up hand-drawn whiteboard sketches. The image shows the selected pen strokes \
(black on white). The list gives every stroke's id with its bounding box, start and end \
point in image pixels, so you can tell which strokes make up which figure.

Group the strokes into items and classify each item:
- a shape kind from the enum for closed figures (rect, ellipse, triangle, diamond, cylinder, cloud…). \
Pick the kind that matches what the person drew, e.g. a box with rounded corners is round-rect, \
a circle is ellipse, a database drum is cylinder.
- line, arrow or double_arrow for open connecting lines. Put the arrowhead strokes in the same item \
as the line they belong to.
- text for handwritten letters, words or numbers. Put all strokes of one word or one line of \
writing in a single item and write the transcription in `text` (in the language written; \
Japanese is common). Writing inside a shape stays a separate text item.

`text` is empty for everything except text items. Use each stroke id at most once. Leave out \
strokes you cannot read with confidence (scribbles, shading, drawings that are not shapes): \
they stay as they are.";

/// ストロークを集める(`ids` の中の、蛍光ペンでないもの)。
pub fn collect_strokes(board: &Board, ids: &[String]) -> Result<Vec<Stroke>, AiError> {
    let wanted: HashSet<u64> = ids
        .iter()
        .filter_map(|h| u64::from_str_radix(h, 16).ok())
        .collect();
    let strokes: Vec<Stroke> = board
        .elements()
        .iter()
        .filter_map(|e| match e {
            Element::Stroke(s) if !s.highlighter && wanted.contains(&s.id.0) => Some(s.clone()),
            _ => None,
        })
        .filter(|s| s.points.len() >= 2)
        .collect();
    if strokes.is_empty() {
        return Err(AiError::Api(api::Error::bad_request(
            "整形する手書きの線を選んでください",
        )));
    }
    if strokes.len() > MAX_STROKES {
        return Err(AiError::TooMany(format!(
            "線が多すぎます({} 本)。{MAX_STROKES} 本以下を選んでください",
            strokes.len()
        )));
    }
    let points: usize = strokes.iter().map(|s| s.points.len()).sum();
    if points > MAX_POINTS {
        return Err(AiError::TooMany(
            "手書きが細かすぎます。範囲を分けて選んでください".into(),
        ));
    }
    Ok(strokes)
}

/// ワールド座標から画像のピクセル座標への変換。
struct Frame {
    origin: Vec2,
    scale: f32,
    size: (u32, u32),
}

impl Frame {
    fn new(strokes: &[Stroke]) -> Self {
        let mut b: Option<Rect> = None;
        for p in strokes.iter().flat_map(|s| &s.points) {
            let r = Rect::new(*p, *p);
            b = Some(b.map_or(r, |b| b.union(&r)));
        }
        let b = b.unwrap_or(Rect::new(Vec2::ZERO, Vec2::ONE));
        let long = b.size().max_element().max(1.0);
        // 小さな走り書きは拡大しすぎない。
        let scale = ((IMAGE_SIZE - IMAGE_PAD * 2.0) / long).min(4.0);
        let size = b.size() * scale + Vec2::splat(IMAGE_PAD * 2.0);
        Self {
            origin: b.min,
            scale,
            size: (size.x.ceil() as u32, size.y.ceil() as u32),
        }
    }

    fn map(&self, p: Vec2) -> Vec2 {
        (p - self.origin) * self.scale + Vec2::splat(IMAGE_PAD)
    }
}

/// ストロークを白地に黒で描いた PNG。
pub fn render_png(strokes: &[Stroke]) -> Vec<u8> {
    let f = Frame::new(strokes);
    let (w, h) = f.size;
    let mut img = image::GrayImage::from_pixel(w, h, image::Luma([255]));
    for s in strokes {
        let r = (s.width * f.scale * 0.5).clamp(1.5, 8.0);
        let pts: Vec<Vec2> = s.points.iter().map(|p| f.map(*p)).collect();
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let steps = (a.distance(b) / (r * 0.5).max(0.5)).ceil().max(1.0) as usize;
            for i in 0..=steps {
                stamp(&mut img, a.lerp(b, i as f32 / steps as f32), r);
            }
        }
    }
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("PNG に書けません");
    out
}

/// 円を塗る。
fn stamp(img: &mut image::GrayImage, c: Vec2, r: f32) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let (x0, x1) = ((c.x - r).floor() as i32, (c.x + r).ceil() as i32);
    let (y0, y1) = ((c.y - r).floor() as i32, (c.y + r).ceil() as i32);
    for y in y0.max(0)..=y1.min(h - 1) {
        for x in x0.max(0)..=x1.min(w - 1) {
            if Vec2::new(x as f32 + 0.5, y as f32 + 0.5).distance(c) <= r {
                img.put_pixel(x as u32, y as u32, image::Luma([0]));
            }
        }
    }
}

/// 各ストロークの位置(画像のピクセル座標)の一覧。
pub fn stroke_list(strokes: &[Stroke]) -> String {
    let f = Frame::new(strokes);
    let mut s = String::from("<strokes>\n");
    for st in strokes {
        let pts: Vec<Vec2> = st.points.iter().map(|p| f.map(*p)).collect();
        let (mut min, mut max) = (pts[0], pts[0]);
        for p in &pts {
            min = min.min(*p);
            max = max.max(*p);
        }
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        s.push_str(&format!(
            "<stroke id=\"{}\" bbox=\"{:.0},{:.0},{:.0},{:.0}\" start=\"{:.0},{:.0}\" end=\"{:.0},{:.0}\"/>\n",
            id_hex(st.id),
            min.x,
            min.y,
            max.x,
            max.y,
            a.x,
            a.y,
            b.x,
            b.y
        ));
    }
    s.push_str("</strokes>");
    s
}

pub fn schema() -> Value {
    let mut kinds: Vec<&str> = AI_SHAPE_KINDS.iter().map(|k| k.name()).collect();
    kinds.extend(InkKind::LINE_KINDS);
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["items"],
        "properties": {
            "items": {"type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "strokes", "text"],
                "properties": {
                    "kind": {"type": "string", "enum": kinds},
                    "strokes": {"type": "array", "items": {"type": "string"}},
                    "text": {"type": "string"}
                }
            }}
        }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub kind: String,
    pub strokes: Vec<String>,
    #[serde(default)]
    pub text: String,
}

/// Claude の答えを検証する: 知らない種類・ID と重複を除き、空になった項目を捨てる。
pub fn normalize(raw: Vec<Item>, strokes: &[Stroke]) -> Vec<Item> {
    let known: HashSet<String> = strokes.iter().map(|s| id_hex(s.id)).collect();
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter_map(|it| {
            let kind = InkKind::from_name(&it.kind, &it.text)?;
            let ids: Vec<String> = it
                .strokes
                .into_iter()
                .map(|i| i.trim().to_lowercase())
                .filter(|i| known.contains(i) && seen.insert(i.clone()))
                .collect();
            (!ids.is_empty()).then(|| Item {
                kind: kind.name().to_string(),
                strokes: ids,
                text: match kind {
                    InkKind::Text(t) => t.chars().take(500).collect(),
                    _ => String::new(),
                },
            })
        })
        .collect()
}

pub async fn recognize(
    State(state): State<Arc<AppState>>,
    Path(room): Path<String>,
    Query(q): Query<api::KeyQuery>,
    headers: HeaderMap,
    Json(req): Json<AiRequest>,
) -> Result<Json<Value>, AiError> {
    // `Board` は await をまたいで持てない(Send でない)ので、先に読み終える。
    let (ai, strokes) = {
        let (ai, board) = crate::ai::authorize(&state, &headers, &room, q.key.as_deref())?;
        (ai, collect_strokes(&board, &req.ids)?)
    };
    if !ai.try_acquire(&room) {
        return Err(AiError::Busy);
    }
    crate::billing::charge_ai(&state, &headers, &room).map_err(AiError::Api)?;
    let png = render_png(&strokes);
    let content = json!([
        {
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(&png),
            },
        },
        { "type": "text", "text": stroke_list(&strokes) },
    ]);
    let out = ai.call_content(SYSTEM, content, schema()).await?;
    let raw: Vec<Item> = serde_json::from_value(out.get("items").cloned().unwrap_or_default())
        .map_err(|e| AiError::Upstream(format!("認識結果の形式が違います: {e}")))?;
    let items = normalize(raw, &strokes);
    tracing::info!(room = %room, strokes = strokes.len(), items = items.len(), "AI で手書きを整形");
    Ok(Json(json!({ "items": items })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::{ElementId, Rgba8};

    fn stroke(id: u64, pts: &[(f32, f32)]) -> Stroke {
        let mut s = Stroke::new(ElementId(id), Vec2::ZERO, Rgba8::BLACK, 4.0);
        s.points = pts.iter().map(|(x, y)| Vec2::new(*x, *y)).collect();
        s
    }

    #[test]
    fn renders_and_lists_strokes() {
        let s = vec![
            stroke(0xa, &[(0.0, 0.0), (500.0, 0.0), (500.0, 250.0)]),
            stroke(0xb, &[(100.0, 100.0), (200.0, 120.0)]),
        ];
        let png = render_png(&s);
        let img = image::load_from_memory(&png).unwrap().to_luma8();
        assert_eq!(img.width(), 1024);
        // 線の上は黒、余白は白。
        assert_eq!(img.get_pixel(512, 24).0[0], 0);
        assert_eq!(img.get_pixel(5, 5).0[0], 255);
        let list = stroke_list(&s);
        assert!(list.contains("<stroke id=\"a\" bbox=\"24,24,1000,512\""));
        assert!(list.contains("id=\"b\""));
    }

    #[test]
    fn items_are_normalized() {
        let s = vec![
            stroke(1, &[(0.0, 0.0), (1.0, 1.0)]),
            stroke(2, &[(0.0, 0.0), (1.0, 1.0)]),
        ];
        let raw = vec![
            Item {
                kind: "rect".into(),
                strokes: vec!["1".into(), "zz".into()],
                text: "x".into(),
            },
            Item {
                kind: "arrow".into(),
                strokes: vec!["1".into(), "2".into()],
                text: "".into(),
            },
            Item {
                kind: "sticky".into(),
                strokes: vec!["2".into()],
                text: "".into(),
            },
            Item {
                kind: "text".into(),
                strokes: vec!["2".into()],
                text: " ".into(),
            },
        ];
        assert_eq!(
            normalize(raw, &s),
            vec![
                Item {
                    kind: "rect".into(),
                    strokes: vec!["1".into()],
                    text: "".into()
                },
                Item {
                    kind: "arrow".into(),
                    strokes: vec!["2".into()],
                    text: "".into()
                },
            ]
        );
    }
}
