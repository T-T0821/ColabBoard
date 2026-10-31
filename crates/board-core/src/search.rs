//! ボード内の文字の検索。

use crate::board::Board;
use crate::element::ElementId;

/// 文字に `query` を含む要素(大文字・小文字と全角・半角の英数字は区別しない)。
/// 読む順(上から下、同じ高さなら左から右)に並べる。
pub fn find(board: &Board, query: &str) -> Vec<ElementId> {
    let q = normalize(query.trim());
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(f32, f32, ElementId)> = board
        .elements()
        .iter()
        .filter(|e| normalize(e.text()).contains(&q))
        .map(|e| {
            let b = e.bounds();
            // 行の高さ程度の差は同じ行とみなす。
            ((b.min.y / 40.0).round(), b.min.x, e.id())
        })
        .collect();
    hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    hits.into_iter().map(|(_, _, id)| id).collect()
}

/// 全角英数字を半角に、英字を小文字にする。
fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            c => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Rgba8, Shape};
    use glam::Vec2;

    #[test]
    fn finds_in_reading_order() {
        let mut b = Board::local();
        let mut add = |text: &str, x: f32, y: f32| {
            let id = b.alloc_id();
            let mut s = Shape::sticky(id, Vec2::new(x, y), Rgba8::WHITE);
            s.text = text.into();
            b.push(s);
            id
        };
        let a = add("ログイン画面", 500.0, 0.0);
        let c = add("ＡＰＩ の設計", 0.0, 300.0);
        let d = add("api テスト", 0.0, 0.0);
        let _ = add("関係ない", 200.0, 0.0);
        assert_eq!(find(&b, "API"), vec![d, c]);
        assert_eq!(find(&b, "ログイン"), vec![a]);
        assert!(find(&b, "  ").is_empty());
    }
}
