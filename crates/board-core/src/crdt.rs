//! 要素と yrs の値の相互変換。
//!
//! 1 要素 = ルートマップ `elements` の 1 エントリ(キーは要素 ID の 16 進表記)で、
//! 中身はフィールドごとのネストしたマップ。フィールド単位で書き換えるので、
//! 別々の人が同じ要素の位置と色を同時に変えても両方残る。

use std::sync::Arc;

use glam::Vec2;
use yrs::{Any, Map, MapRef, Number, Out, ReadTxn};

use crate::color::Rgba8;
use crate::element::{Element, ElementId, Stroke};
use crate::shape::{Shape, ShapeKind};

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

/// 要素をフィールドの一覧にする。`z` は重なり順。
pub fn encode(e: &Element, z: f64) -> Vec<(&'static str, Any)> {
    let mut f = vec![("z", float(z))];
    match e {
        Element::Shape(s) => {
            let kind = match s.kind {
                ShapeKind::Rect => "rect",
                ShapeKind::Ellipse => "ellipse",
                ShapeKind::Sticky => "sticky",
            };
            f.extend([
                ("t", Any::String(kind.into())),
                ("x", float(s.pos.x as f64)),
                ("y", float(s.pos.y as f64)),
                ("w", float(s.size.x as f64)),
                ("h", float(s.size.y as f64)),
                ("fill", color(s.fill)),
                ("stroke", color(s.stroke)),
                ("sw", float(s.stroke_width as f64)),
                ("r", float(s.corner_radius as f64)),
                ("text", Any::String(s.text.as_str().into())),
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
    let shape_kind = match kind.as_str() {
        "rect" => Some(ShapeKind::Rect),
        "ellipse" => Some(ShapeKind::Ellipse),
        "sticky" => Some(ShapeKind::Sticky),
        _ => None,
    };
    if let Some(kind) = shape_kind {
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
        };
        return Some((Element::Shape(shape), z));
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
        };
        return Some((Element::Stroke(stroke), z));
    }
    None
}
