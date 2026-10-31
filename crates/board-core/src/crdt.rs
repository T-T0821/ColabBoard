//! 要素と yrs の値の相互変換。
//!
//! 1 要素 = ルートマップ `elements` の 1 エントリ(キーは要素 ID の 16 進表記)で、
//! 中身はフィールドごとのネストしたマップ。フィールド単位で書き換えるので、
//! 別々の人が同じ要素の位置と色を同時に変えても両方残る。

use std::sync::Arc;

use glam::Vec2;
use yrs::{Any, Map, MapRef, Number, Out, ReadTxn};

use crate::color::Rgba8;
use crate::element::{Cap, Connector, Element, ElementId, Endpoint, Meta, PathStyle, Stroke};
use crate::shape::{Shape, ShapeKind, TextAlign};

pub fn key_of(id: ElementId) -> String {
    format!("{:x}", id.0)
}

pub fn id_of(key: &str) -> Option<ElementId> {
    u64::from_str_radix(key, 16).ok().map(ElementId)
}

fn color(c: Rgba8) -> Any {
    Any::Number(Number::Int(c.to_u32() as i64))
}

fn float(v: f64) -> Any {
    Any::Number(Number::Float(v))
}

fn endpoint(e: &Endpoint) -> Any {
    // "x,y" か "id:ax,ay" / "id:auto" の文字列にする。
    let s = match e {
        Endpoint::Free(p) => format!("{},{}", p.x, p.y),
        Endpoint::Attached { id, anchor: None } => format!("{}:auto", key_of(*id)),
        Endpoint::Attached {
            id,
            anchor: Some(a),
        } => format!("{}:{},{}", key_of(*id), a.x, a.y),
    };
    Any::String(s.into())
}

fn parse_endpoint(s: &str) -> Option<Endpoint> {
    let pair = |v: &str| -> Option<Vec2> {
        let (x, y) = v.split_once(',')?;
        Some(Vec2::new(x.parse().ok()?, y.parse().ok()?))
    };
    match s.split_once(':') {
        Some((id, rest)) => Some(Endpoint::Attached {
            id: id_of(id)?,
            anchor: if rest == "auto" { None } else { pair(rest) },
        }),
        None => pair(s).map(Endpoint::Free),
    }
}

/// 要素をフィールドの一覧にする。`z` は重なり順。
pub fn encode(e: &Element, z: f64) -> Vec<(&'static str, Any)> {
    let mut f = vec![("z", float(z))];
    let meta = e.meta();
    f.extend([
        ("g", Any::Number(Number::Int(meta.group as i64))),
        ("lock", Any::Bool(meta.locked)),
        ("link", Any::String(meta.link.as_str().into())),
    ]);
    match e {
        Element::Shape(s) => {
            f.extend([
                ("t", Any::String(s.kind.name().into())),
                ("x", float(s.pos.x as f64)),
                ("y", float(s.pos.y as f64)),
                ("w", float(s.size.x as f64)),
                ("h", float(s.size.y as f64)),
                ("fill", color(s.fill)),
                ("stroke", color(s.stroke)),
                ("sw", float(s.stroke_width as f64)),
                ("r", float(s.corner_radius as f64)),
                ("text", Any::String(s.text.as_str().into())),
                ("fs", float(s.font_size as f64)),
                ("al", Any::String(s.align.name().into())),
                ("tc", color(s.text_color)),
            ]);
            if let Some(img) = &s.image {
                f.push(("img", Any::Buffer(img.0.clone())));
            }
            if !s.icon.is_empty() {
                f.push(("icon", Any::String(s.icon.as_str().into())));
            }
        }
        Element::Connector(c) => {
            f.extend([
                ("t", Any::String("connector".into())),
                ("from", endpoint(&c.from)),
                ("to", endpoint(&c.to)),
                ("color", color(c.color)),
                ("width", float(c.width as f64)),
                ("style", Any::String(c.style.name().into())),
                ("cs", Any::String(c.start_cap.name().into())),
                ("ce", Any::String(c.end_cap.name().into())),
                ("text", Any::String(c.text.as_str().into())),
            ]);
        }
        Element::Stroke(s) => {
            let mut bytes = Vec::with_capacity(s.points.len() * 8);
            for p in &s.points {
                bytes.extend_from_slice(&p.x.to_le_bytes());
                bytes.extend_from_slice(&p.y.to_le_bytes());
            }
            f.extend([
                ("t", Any::String("stroke".into())),
                ("color", color(s.color)),
                ("width", float(s.width as f64)),
                ("pts", Any::Buffer(Arc::from(bytes))),
                ("hl", Any::Bool(s.highlighter)),
            ]);
        }
    }
    f
}

fn num<T: ReadTxn>(m: &MapRef, txn: &T, k: &str) -> f32 {
    match m.get(txn, k) {
        Some(Out::Any(Any::Number(Number::Float(n)))) => n as f32,
        Some(Out::Any(Any::Number(Number::Int(n)))) => n as f32,
        _ => 0.0,
    }
}

fn rgba<T: ReadTxn>(m: &MapRef, txn: &T, k: &str) -> Rgba8 {
    match m.get(txn, k) {
        Some(Out::Any(Any::Number(Number::Int(n)))) => Rgba8((n as u32).to_le_bytes()),
        Some(Out::Any(Any::Number(Number::Float(n)))) => Rgba8((n as u32).to_le_bytes()),
        _ => Rgba8::BLACK,
    }
}

fn boolean<T: ReadTxn>(m: &MapRef, txn: &T, k: &str) -> bool {
    matches!(m.get(txn, k), Some(Out::Any(Any::Bool(true))))
}

fn rgba_or<T: ReadTxn>(m: &MapRef, txn: &T, k: &str, default: Rgba8) -> Rgba8 {
    match m.get(txn, k) {
        Some(Out::Any(Any::Number(_))) => rgba(m, txn, k),
        _ => default,
    }
}

fn meta<T: ReadTxn>(m: &MapRef, txn: &T) -> Meta {
    let group = match m.get(txn, "g") {
        Some(Out::Any(Any::Number(Number::Int(n)))) => n as u64,
        Some(Out::Any(Any::Number(Number::Float(n)))) => n as u64,
        _ => 0,
    };
    Meta {
        group,
        locked: boolean(m, txn, "lock"),
        link: string(m, txn, "link"),
    }
}

fn string<T: ReadTxn>(m: &MapRef, txn: &T, k: &str) -> String {
    match m.get(txn, k) {
        Some(Out::Any(Any::String(s))) => s.to_string(),
        _ => String::new(),
    }
}

/// ネストしたマップから要素を読む。読めなければ `None`(未知の種類など)。
pub fn decode<T: ReadTxn>(id: ElementId, m: &MapRef, txn: &T) -> Option<(Element, f64)> {
    let z = match m.get(txn, "z") {
        Some(Out::Any(Any::Number(Number::Float(n)))) => n,
        Some(Out::Any(Any::Number(Number::Int(n)))) => n as f64,
        _ => 0.0,
    };
    let kind = string(m, txn, "t");
    if let Some(kind) = ShapeKind::from_name(&kind) {
        let shape = Shape {
            id,
            kind,
            pos: Vec2::new(num(m, txn, "x"), num(m, txn, "y")),
            size: Vec2::new(num(m, txn, "w"), num(m, txn, "h")),
            fill: rgba(m, txn, "fill"),
            stroke: rgba(m, txn, "stroke"),
            stroke_width: num(m, txn, "sw"),
            corner_radius: num(m, txn, "r"),
            text: string(m, txn, "text"),
            font_size: num(m, txn, "fs"),
            align: match m.get(txn, "al") {
                Some(Out::Any(Any::String(a))) => TextAlign::from_name(&a),
                // 古いデータには揃えがない。種類ごとの既定値にする。
                _ => Shape::new(id, kind, Vec2::ZERO, Vec2::ONE, Rgba8::WHITE).align,
            },
            text_color: rgba_or(m, txn, "tc", Rgba8::BLACK),
            meta: meta(m, txn),
            image: match m.get(txn, "img") {
                Some(Out::Any(Any::Buffer(b))) => Some(crate::shape::ImageData(b)),
                _ => None,
            },
            icon: string(m, txn, "icon"),
        };
        return Some((Element::Shape(shape), z));
    }
    if kind == "connector" {
        let end = |k: &str| parse_endpoint(&string(m, txn, k));
        let mut c = Connector::new(id, end("from")?, end("to")?);
        c.color = rgba(m, txn, "color");
        c.width = num(m, txn, "width");
        c.style = PathStyle::from_name(&string(m, txn, "style"));
        c.start_cap = Cap::from_name(&string(m, txn, "cs"));
        c.end_cap = Cap::from_name(&string(m, txn, "ce"));
        c.text = string(m, txn, "text");
        c.meta = meta(m, txn);
        return Some((Element::Connector(c), z));
    }
    if kind == "stroke" {
        let points = match m.get(txn, "pts") {
            Some(Out::Any(Any::Buffer(b))) => b
                .chunks_exact(8)
                .map(|c| {
                    Vec2::new(
                        f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                        f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        let stroke = Stroke {
            id,
            points,
            color: rgba(m, txn, "color"),
            width: num(m, txn, "width"),
            highlighter: boolean(m, txn, "hl"),
            meta: meta(m, txn),
        };
        return Some((Element::Stroke(stroke), z));
    }
    None
}
