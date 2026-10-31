//! 選択した要素への一括操作: コピー&ペースト、複製、グループ化、整列・分布、重なり順、ロック。
//!
//! どれもボードの通常の変更として書くので、Undo と同期はそのまま効く。

use std::collections::HashMap;

use glam::Vec2;

use crate::board::Board;
use crate::element::{Element, ElementId, Endpoint};
use crate::geom::Rect;
use crate::shape::ShapeKind;

/// クリップボードに入れる文字列の先頭。
const CLIP_PREFIX: &str = "colabboard/v1:";

// ---- 選択の広げ方 ----

/// グループの仲間を含めた選択。
pub fn expand_groups(board: &Board, ids: &[ElementId]) -> Vec<ElementId> {
    let groups: Vec<u64> = ids
        .iter()
        .filter_map(|id| board.get(*id))
        .map(|e| e.meta().group)
        .filter(|g| *g != 0)
        .collect();
    let mut out: Vec<ElementId> = ids.to_vec();
    if groups.is_empty() {
        return out;
    }
    for e in board.elements() {
        if groups.contains(&e.meta().group) && !out.contains(&e.id()) {
            out.push(e.id());
        }
    }
    out
}

/// 動かすときに一緒に動く要素(選択 + フレームの中身)。ロック中の要素は動かさない。
pub fn move_set(board: &Board, selection: &[ElementId]) -> Vec<ElementId> {
    // マインドマップの中心を動かすときは木全体を動かす。
    let selection = &crate::mindmap::expand_for_move(board, selection);
    let mut out: Vec<ElementId> = selection
        .iter()
        .copied()
        .filter(|id| board.get(*id).is_some_and(|e| !e.is_locked()))
        .collect();
    let frames: Vec<Rect> = out
        .iter()
        .filter_map(|id| board.get(*id)?.as_shape())
        .filter(|s| s.kind == ShapeKind::Frame)
        .map(|s| s.bounds())
        .collect();
    if frames.is_empty() {
        return out;
    }
    for e in board.elements() {
        if e.is_locked() || out.contains(&e.id()) {
            continue;
        }
        let b = e.bounds();
        if frames.iter().any(|f| f.contains_rect(&b)) {
            out.push(e.id());
        }
    }
    out
}

// ---- コピー&ペースト ----

/// 選択した要素をクリップボード用の文字列にする。
pub fn copy(board: &Board, ids: &[ElementId]) -> Option<String> {
    let mut tmp = Board::new();
    let mut picked: Vec<(f64, Element)> = ids
        .iter()
        .filter_map(|id| Some((board.z_of(*id)?, board.get(*id)?.clone())))
        .collect();
    if picked.is_empty() {
        return None;
    }
    // 重なり順を保つ。
    picked.sort_by(|a, b| a.0.total_cmp(&b.0));
    tmp.push_many(picked.into_iter().map(|(_, e)| e).collect());
    let bytes = tmp.encode_diff(None).ok()?;
    Some(format!("{CLIP_PREFIX}{}", base64_encode(&bytes)))
}

/// クリップボードの文字列から要素を読む(ID は元のまま、重なり順に並ぶ)。
pub fn parse_clipboard(text: &str) -> Option<Vec<Element>> {
    let data = text.trim().strip_prefix(CLIP_PREFIX)?;
    let bytes = base64_decode(data)?;
    let mut tmp = Board::new();
    tmp.apply_remote(&bytes).ok()?;
    Some(tmp.elements().to_vec())
}

/// 要素を新しい ID で貼り付ける。`offset` だけずらす。貼り付けた要素の ID を返す。
///
/// 一緒にコピーした要素どうしのコネクタはつながったまま、コピーしていない要素への
/// つながりは自由な端になる。グループは新しいグループとして作り直す。
pub fn paste(board: &mut Board, elements: Vec<Element>, offset: Vec2) -> Vec<ElementId> {
    let mut ids: HashMap<ElementId, ElementId> = HashMap::new();
    for e in &elements {
        ids.insert(e.id(), board.alloc_id());
    }
    let mut groups: HashMap<u64, u64> = HashMap::new();
    let mut out = Vec::with_capacity(elements.len());
    // マインドマップの枝は並べ直すときに作り直すので貼らない。
    let elements: Vec<Element> = elements
        .into_iter()
        .filter(|e| !matches!(e.meta().mind, Some(crate::mindmap::Mind::Branch)))
        .collect();
    for mut e in elements {
        let new_id = ids[&e.id()];
        let old_group = e.meta().group;
        if old_group != 0 {
            let g = *groups
                .entry(old_group)
                .or_insert_with(|| board.alloc_id().0);
            e.meta_mut().group = g;
        }
        e.meta_mut().locked = false;
        e.meta_mut().hidden = false;
        // 一緒にコピーした親につなぎ直す。親がなければ新しいマインドマップの中心にする。
        if let Some(crate::mindmap::Mind::Node(l)) = &mut e.meta_mut().mind {
            l.parent = l.parent.and_then(|p| ids.get(&p).copied());
        }
        match &mut e {
            Element::Shape(s) => s.id = new_id,
            Element::Stroke(s) => s.id = new_id,
            Element::Connector(c) => {
                c.id = new_id;
                let path = c.path.clone();
                for (end, at) in [
                    (&mut c.from, path.first().copied()),
                    (&mut c.to, path.last().copied()),
                ] {
                    if let Endpoint::Attached { id, .. } = end {
                        match ids.get(id) {
                            Some(n) => *id = *n,
                            None => *end = Endpoint::Free(at.unwrap_or(Vec2::ZERO)),
                        }
                    }
                }
            }
        }
        e.translate(offset);
        out.push(e);
    }
    let new_ids = out.iter().map(Element::id).collect();
    board.push_many(out);
    new_ids
}

/// 複製(コピーしてすぐ少しずらして貼る)。
pub fn duplicate(board: &mut Board, ids: &[ElementId]) -> Vec<ElementId> {
    let elements: Vec<Element> = {
        let mut v: Vec<(f64, Element)> = ids
            .iter()
            .filter_map(|id| Some((board.z_of(*id)?, board.get(*id)?.clone())))
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v.into_iter().map(|(_, e)| e).collect()
    };
    paste(board, elements, Vec2::splat(24.0))
}

// ---- グループ・ロック ----

/// グループにする。戻り値はグループ ID(要素が 2 つ未満なら `None`)。
pub fn group(board: &mut Board, ids: &[ElementId]) -> Option<u64> {
    if ids.len() < 2 {
        return None;
    }
    let g = board.alloc_id().0;
    for id in ids {
        board.update(*id, |e| e.meta_mut().group = g);
    }
    Some(g)
}

pub fn ungroup(board: &mut Board, ids: &[ElementId]) {
    for id in ids {
        board.update(*id, |e| e.meta_mut().group = 0);
    }
}

/// ロック状態を切り替える(1 つでもロックされていなければ全部ロックする)。
pub fn toggle_lock(board: &mut Board, ids: &[ElementId]) -> bool {
    let lock = ids
        .iter()
        .filter_map(|id| board.get(*id))
        .any(|e| !e.is_locked());
    for id in ids {
        board.update(*id, |e| e.meta_mut().locked = lock);
    }
    lock
}

/// すべてのロックを外す。外した要素を返す。
pub fn unlock_all(board: &mut Board) -> Vec<ElementId> {
    let locked: Vec<ElementId> = board
        .elements()
        .iter()
        .filter(|e| e.is_locked())
        .map(Element::id)
        .collect();
    for id in &locked {
        board.update(*id, |e| e.meta_mut().locked = false);
    }
    locked
}

// ---- 重なり順 ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// 最前面へ。
    Front,
    /// ひとつ前へ。
    Forward,
    /// ひとつ後ろへ。
    Backward,
    /// 最背面へ。
    Back,
}

pub fn reorder(board: &mut Board, ids: &[ElementId], order: Order) {
    let mut sel: Vec<(f64, ElementId)> = ids
        .iter()
        .filter_map(|id| Some((board.z_of(*id)?, *id)))
        .collect();
    if sel.is_empty() {
        return;
    }
    sel.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (lo, hi) = board.z_range();
    let others: Vec<f64> = board
        .elements()
        .iter()
        .filter(|e| !ids.contains(&e.id()))
        .filter_map(|e| board.z_of(e.id()))
        .collect();
    let n = sel.len() as f64;
    // 新しい z を [a, b) に等間隔で割り当てる。
    let spread = |a: f64, b: f64| -> Vec<(ElementId, f64)> {
        sel.iter()
            .enumerate()
            .map(|(i, (_, id))| (*id, a + (b - a) * (i as f64 + 1.0) / (n + 1.0)))
            .collect()
    };
    let changes = match order {
        Order::Front => spread(hi, hi + n + 1.0),
        Order::Back => spread(lo - n - 1.0, lo),
        Order::Forward => {
            let top = sel.last().unwrap().0;
            // 選択より手前にある最初の要素の、さらに 1 つ上まで。
            let mut above: Vec<f64> = others.iter().copied().filter(|z| *z > top).collect();
            above.sort_by(f64::total_cmp);
            match above.as_slice() {
                [] => return,
                [a] => spread(*a, *a + 1.0),
                [a, b, ..] => spread(*a, *b),
            }
        }
        Order::Backward => {
            let bottom = sel[0].0;
            let mut below: Vec<f64> = others.iter().copied().filter(|z| *z < bottom).collect();
            below.sort_by(|a, b| b.total_cmp(a));
            match below.as_slice() {
                [] => return,
                [a] => spread(*a - 1.0, *a),
                [a, b, ..] => spread(*b, *a),
            }
        }
    };
    board.set_z(&changes);
}

// ---- 整列・分布 ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    CenterX,
    Right,
    Top,
    CenterY,
    Bottom,
}

/// 整列・分布の単位(グループはまとめて 1 つ)。
fn units(board: &Board, ids: &[ElementId]) -> Vec<(Vec<ElementId>, Rect)> {
    let mut by_group: Vec<(u64, Vec<ElementId>)> = Vec::new();
    for id in ids {
        let Some(e) = board.get(*id) else { continue };
        if e.is_locked() || matches!(e, Element::Connector(_)) {
            continue;
        }
        let g = e.meta().group;
        match by_group.iter_mut().find(|(k, _)| g != 0 && *k == g) {
            Some((_, v)) => v.push(*id),
            None => by_group.push((g, vec![*id])),
        }
    }
    by_group
        .into_iter()
        .filter_map(|(_, v)| {
            let r = board.bounds_of(&v)?;
            Some((v, r))
        })
        .collect()
}

fn move_unit(board: &mut Board, ids: &[ElementId], d: Vec2) {
    if d == Vec2::ZERO {
        return;
    }
    for id in ids {
        board.update(*id, |e| e.translate(d));
    }
}

pub fn align(board: &mut Board, ids: &[ElementId], how: Align) {
    let us = units(board, ids);
    if us.len() < 2 {
        return;
    }
    let all = us.iter().skip(1).fold(us[0].1, |a, (_, r)| a.union(r));
    for (ids, r) in us {
        let d = match how {
            Align::Left => Vec2::new(all.min.x - r.min.x, 0.0),
            Align::Right => Vec2::new(all.max.x - r.max.x, 0.0),
            Align::CenterX => Vec2::new(all.center().x - r.center().x, 0.0),
            Align::Top => Vec2::new(0.0, all.min.y - r.min.y),
            Align::Bottom => Vec2::new(0.0, all.max.y - r.max.y),
            Align::CenterY => Vec2::new(0.0, all.center().y - r.center().y),
        };
        move_unit(board, &ids, d);
    }
}

/// 間隔を均等にする(`horizontal` なら横方向)。3 つ以上で効く。
pub fn distribute(board: &mut Board, ids: &[ElementId], horizontal: bool) {
    let mut us = units(board, ids);
    if us.len() < 3 {
        return;
    }
    let axis = |v: Vec2| if horizontal { v.x } else { v.y };
    us.sort_by(|a, b| axis(a.1.min).total_cmp(&axis(b.1.min)));
    let start = axis(us[0].1.min);
    let end = us.iter().map(|(_, r)| axis(r.max)).fold(f32::MIN, f32::max);
    let total: f32 = us.iter().map(|(_, r)| axis(r.size())).sum();
    let gap = (end - start - total) / (us.len() - 1) as f32;
    let mut cursor = start;
    for (ids, r) in us {
        let d = cursor - axis(r.min);
        let v = if horizontal {
            Vec2::new(d, 0.0)
        } else {
            Vec2::new(0.0, d)
        };
        move_unit(board, &ids, v);
        cursor += axis(r.size()) + gap;
    }
}

/// 並べて整理する(格子状に詰める)。FigJam の「整頓」に相当。
pub fn tidy(board: &mut Board, ids: &[ElementId]) {
    let mut us = units(board, ids);
    if us.len() < 2 {
        return;
    }
    let all = us.iter().skip(1).fold(us[0].1, |a, (_, r)| a.union(r));
    // 読む順(上から、左から)に並べ直す。
    us.sort_by(|a, b| {
        (a.1.min.y, a.1.min.x)
            .partial_cmp(&(b.1.min.y, b.1.min.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let cols = (us.len() as f32).sqrt().ceil() as usize;
    let cell = us.iter().fold(Vec2::ZERO, |m, (_, r)| m.max(r.size())) + Vec2::splat(24.0);
    for (i, (ids, r)) in us.into_iter().enumerate() {
        let target = all.min + Vec2::new((i % cols) as f32, (i / cols) as f32) * cell;
        move_unit(board, &ids, target - r.min);
    }
}

// ---- base64(依存を増やさないための小さな実装) ----

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgba8;
    use crate::connector::Connector;
    use crate::shape::Shape;

    fn rect(b: &mut Board, x: f32, y: f32, w: f32) -> ElementId {
        let id = b.alloc_id();
        b.push(Shape::rect(
            id,
            Vec2::new(x, y),
            Vec2::new(w, 50.0),
            Rgba8::WHITE,
        ))
    }

    #[test]
    fn base64_roundtrip() {
        for n in 0..10 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }

    #[test]
    fn copy_paste_remaps_ids_and_connectors() {
        let mut b = Board::new();
        let a = rect(&mut b, 0.0, 0.0, 100.0);
        let z = rect(&mut b, 300.0, 0.0, 100.0);
        let outside = rect(&mut b, 0.0, 300.0, 100.0);
        let c1 = b.alloc_id();
        b.push(Connector::new(
            c1,
            Endpoint::Attached {
                id: a,
                anchor: None,
            },
            Endpoint::Attached {
                id: z,
                anchor: None,
            },
        ));
        let c2 = b.alloc_id();
        b.push(Connector::new(
            c2,
            Endpoint::Attached {
                id: a,
                anchor: None,
            },
            Endpoint::Attached {
                id: outside,
                anchor: None,
            },
        ));
        group(&mut b, &[a, z]);

        let clip = copy(&b, &[a, z, c1, c2]).unwrap();
        let els = parse_clipboard(&clip).unwrap();
        assert_eq!(els.len(), 4);
        let new = paste(&mut b, els, Vec2::new(0.0, 1000.0));
        assert_eq!(new.len(), 4);
        assert_eq!(b.len(), 9);
        let shapes: Vec<&Element> = new.iter().filter_map(|id| b.get(*id)).collect();
        // コピー同士のコネクタは新しい図形につながる。
        let conns: Vec<_> = shapes
            .iter()
            .filter_map(|e| match e {
                Element::Connector(c) => Some(c),
                _ => None,
            })
            .collect();
        assert!(
            conns
                .iter()
                .any(|c| c.from.attached_to().is_some_and(|i| new.contains(&i))
                    && c.to.attached_to().is_some_and(|i| new.contains(&i)))
        );
        // コピーしていない相手へのつながりは外れる。
        assert!(conns.iter().any(|c| matches!(c.to, Endpoint::Free(_))));
        // グループは新しい ID になる。
        let g_old = b.get(a).unwrap().meta().group;
        let g_new = shapes[0].meta().group;
        assert!(g_new != 0 && g_new != g_old);
        assert!(parse_clipboard("hello").is_none());
    }

    #[test]
    fn groups_expand_and_move_with_frames() {
        let mut b = Board::new();
        let fid = b.alloc_id();
        b.push(Shape::frame(fid, Vec2::ZERO, Vec2::splat(500.0)));
        let inner = rect(&mut b, 10.0, 10.0, 50.0);
        let x = rect(&mut b, 1000.0, 0.0, 50.0);
        let y = rect(&mut b, 1200.0, 0.0, 50.0);
        group(&mut b, &[x, y]);
        assert_eq!(expand_groups(&b, &[x]).len(), 2);
        let set = move_set(&b, &[fid]);
        assert!(set.contains(&inner) && !set.contains(&x));
        toggle_lock(&mut b, &[inner]);
        assert!(!move_set(&b, &[fid]).contains(&inner));
        assert_eq!(unlock_all(&mut b), vec![inner]);
    }

    #[test]
    fn reorder_moves_z() {
        let mut b = Board::new();
        let a = rect(&mut b, 0.0, 0.0, 10.0);
        let m = rect(&mut b, 0.0, 0.0, 10.0);
        let z = rect(&mut b, 0.0, 0.0, 10.0);
        let order = |b: &Board| b.elements().iter().map(Element::id).collect::<Vec<_>>();
        reorder(&mut b, &[a], Order::Front);
        assert_eq!(order(&b), vec![m, z, a]);
        reorder(&mut b, &[a], Order::Back);
        assert_eq!(order(&b), vec![a, m, z]);
        reorder(&mut b, &[a], Order::Forward);
        assert_eq!(order(&b), vec![m, a, z]);
        reorder(&mut b, &[z], Order::Backward);
        assert_eq!(order(&b), vec![m, z, a]);
        reorder(&mut b, &[m], Order::Backward);
        assert_eq!(order(&b), vec![m, z, a]);
    }

    #[test]
    fn align_and_distribute() {
        let mut b = Board::new();
        let a = rect(&mut b, 0.0, 0.0, 100.0);
        let m = rect(&mut b, 150.0, 40.0, 50.0);
        let z = rect(&mut b, 500.0, 80.0, 100.0);
        align(&mut b, &[a, m, z], Align::Top);
        for id in [a, m, z] {
            assert_eq!(b.get(id).unwrap().bounds().min.y, 0.0);
        }
        distribute(&mut b, &[a, m, z], true);
        // 全幅 600、合計幅 250 → 間隔 175。
        assert_eq!(b.get(m).unwrap().bounds().min.x, 275.0);
        tidy(&mut b, &[a, m, z]);
        let ra = b.get(a).unwrap().bounds();
        let rm = b.get(m).unwrap().bounds();
        assert!(!ra.intersects(&rm));
    }

    #[test]
    fn duplicate_offsets() {
        let mut b = Board::new();
        let a = rect(&mut b, 0.0, 0.0, 100.0);
        let d = duplicate(&mut b, &[a]);
        assert_eq!(b.get(d[0]).unwrap().bounds().min, Vec2::splat(24.0));
    }
}
