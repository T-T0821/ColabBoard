//! CSV・TSV・スプレッドシートの取り込み。
//!
//! 行と列のデータを、列が 1 つなら付箋の並びに、2 つ以上なら表にする。
//! シートが複数あれば、シートごとにフレームに入れて横に並べる。
//! スプレッドシートのファイル(xlsx など)の読み取りはクライアント側で行い、ここには行の一覧を渡す。

use glam::Vec2;

use crate::board::Board;
use crate::color::Rgba8;
use crate::element::{Element, ElementId};
use crate::shape::{Shape, estimate_text_height};
use crate::table;

/// 取り込む行・列の上限(巨大なファイルでボードが固まらないように)。
pub const MAX_ROWS: usize = 500;
pub const MAX_COLS: usize = 30;
/// 1 つのセルの文字数の上限。
const MAX_CELL_CHARS: usize = 2000;

const STICKY: f32 = 200.0;
const GAP: f32 = 20.0;
const FRAME_PAD: f32 = 32.0;
const SHEET_GAP: f32 = 96.0;

/// 1 枚のシート(名前と行)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sheet {
    pub name: String,
    pub rows: Vec<Vec<String>>,
}

/// 区切り文字 `delim` の CSV を読む(RFC 4180: `"` で囲めば区切り・改行を含められ、`""` は `"`)。
pub fn parse_delimited(text: &str, delim: char) -> Vec<Vec<String>> {
    scan(text, delim, usize::MAX).0
}

/// 巨大な CSV 用。空でない行を先頭から `keep` 行まで読み、空でない行の総数も返す。
/// 上限を超えた部分は文字列を作らずに数えるだけなので、数百 MB でもメモリを食わない。
pub fn parse_limited(text: &str, keep: usize) -> (Vec<Vec<String>>, usize) {
    scan(text, sniff_delimiter(text), keep)
}

fn scan(text: &str, delim: char, keep: usize) -> (Vec<Vec<String>>, usize) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    // 今の欄がまだ始まっていない(`"` で囲み始められる)か。
    let mut at_start = true;
    // 今の行に空白以外の文字があるか。
    let mut filled = false;
    let mut kept = 0usize;
    let mut total = 0usize;
    // 行の終わり。空でない行を数え、上限までなら残す。
    let end_row = |row: &mut Vec<String>,
                   filled: &mut bool,
                   rows: &mut Vec<Vec<String>>,
                   kept: &mut usize,
                   total: &mut usize| {
        if *filled {
            *total += 1;
        }
        if *kept < keep {
            if *filled {
                *kept += 1;
            }
            rows.push(std::mem::take(row));
        } else {
            row.clear();
        }
        *filled = false;
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let store = kept < keep;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    if store {
                        field.push('"');
                    }
                    filled = true;
                    chars.next();
                }
                '"' => quoted = false,
                _ => {
                    if store {
                        field.push(c);
                    }
                    filled |= !c.is_whitespace();
                }
            }
            continue;
        }
        match c {
            '"' if at_start => {
                quoted = true;
                at_start = false;
            }
            c if c == delim => {
                if store {
                    row.push(std::mem::take(&mut field));
                }
                at_start = true;
            }
            '\r' => {}
            '\n' => {
                if store {
                    row.push(std::mem::take(&mut field));
                }
                at_start = true;
                end_row(&mut row, &mut filled, &mut rows, &mut kept, &mut total);
            }
            _ => {
                if store {
                    field.push(c);
                }
                filled |= !c.is_whitespace();
                at_start = false;
            }
        }
    }
    if !at_start || !row.is_empty() || filled {
        if kept < keep {
            row.push(field);
        }
        end_row(&mut row, &mut filled, &mut rows, &mut kept, &mut total);
    }
    (rows, total)
}

/// 区切り文字を推測する(1 行目で一番多いもの。タブ・カンマ・セミコロン)。
pub fn sniff_delimiter(text: &str) -> char {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut best = (',', 0);
    for d in ['\t', ',', ';'] {
        let mut n = 0;
        let mut quoted = false;
        for c in first.chars() {
            if c == '"' {
                quoted = !quoted;
            } else if c == d && !quoted {
                n += 1;
            }
        }
        if n > best.1 {
            best = (d, n);
        }
    }
    best.0
}

/// CSV / TSV を読む(区切り文字は推測)。
pub fn parse(text: &str) -> Vec<Vec<String>> {
    parse_delimited(text, sniff_delimiter(text))
}

/// 貼り付けられた文字列が表計算ソフトからコピーした表(タブ区切り)らしいか。
/// 2 行以上あり、どの行も同じ数(1 以上)のタブを含む。
pub fn looks_like_tsv(text: &str) -> bool {
    let lines: Vec<&str> = text
        .trim_end_matches(['\r', '\n'])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.len() < 2 {
        return false;
    }
    let tabs = |l: &str| l.matches('\t').count();
    let n = tabs(lines[0]);
    n > 0 && lines.iter().all(|l| tabs(l) == n)
}

/// 空の行・列を落とし、上限で切る。
pub fn normalize(rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|r| {
            r.into_iter()
                .take(MAX_COLS)
                .map(|c| c.trim().chars().take(MAX_CELL_CHARS).collect())
                .collect::<Vec<String>>()
        })
        .filter(|r| r.iter().any(|c| !c.is_empty()))
        .take(MAX_ROWS)
        .collect();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    // 全部の行で空の列を落とす。
    let keep: Vec<bool> = (0..cols)
        .map(|c| rows.iter().any(|r| r.get(c).is_some_and(|t| !t.is_empty())))
        .collect();
    for r in &mut rows {
        r.resize(cols, String::new());
        let mut i = 0;
        r.retain(|_| {
            let k = keep[i];
            i += 1;
            k
        });
    }
    rows
}

/// 1 列のデータを付箋にして、ほぼ正方形に並べる。
fn stickies(
    rows: &[Vec<String>],
    pos: Vec2,
    sticky: Rgba8,
    alloc: &mut dyn FnMut() -> ElementId,
) -> Vec<Shape> {
    let texts: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.first())
        .map(String::as_str)
        .filter(|t| !t.is_empty())
        .collect();
    let per_row = ((texts.len() as f32).sqrt().ceil() as usize).clamp(1, 10);
    let mut out = Vec::with_capacity(texts.len());
    let mut y = pos.y;
    for row in texts.chunks(per_row) {
        let mut notes: Vec<Shape> = row
            .iter()
            .enumerate()
            .map(|(c, t)| {
                let at = Vec2::new(pos.x + c as f32 * (STICKY + GAP), y);
                let mut s = Shape::sticky(alloc(), at, sticky);
                s.text = (*t).to_string();
                s.font_size = 18.0;
                let style = s.text_style();
                let inner = s.text_rect().size();
                let h = estimate_text_height(&s.text, inner.x, &style) + (s.size.y - inner.y);
                s.size.y = h.max(STICKY);
                s
            })
            .collect();
        let h = notes.iter().map(|s| s.size.y).fold(STICKY, f32::max);
        for s in &mut notes {
            s.size.y = h;
        }
        out.extend(notes);
        y += h + GAP;
    }
    out
}

/// 1 枚のシートを左上 `pos` に置く。
fn sheet_shapes(
    rows: &[Vec<String>],
    pos: Vec2,
    sticky: Rgba8,
    alloc: &mut dyn FnMut() -> ElementId,
) -> Vec<Shape> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols <= 1 {
        stickies(rows, pos, sticky, alloc)
    } else {
        table::build(rows, pos, alloc)
    }
}

/// シートを中心 `center` に並べてボードに足す。何もなければ空。
pub fn insert(
    board: &mut Board,
    sheets: Vec<Sheet>,
    center: Vec2,
    sticky: Rgba8,
) -> Vec<ElementId> {
    let sheets: Vec<Sheet> = sheets
        .into_iter()
        .map(|s| Sheet {
            name: s.name,
            rows: normalize(s.rows),
        })
        .filter(|s| !s.rows.is_empty())
        .collect();
    let framed = sheets.len() > 1;
    let mut alloc = || board.alloc_id();
    let mut frames: Vec<Element> = Vec::new();
    let mut content: Vec<Element> = Vec::new();
    let mut x = 0.0;
    for sheet in &sheets {
        let pad = if framed { FRAME_PAD } else { 0.0 };
        let shapes = sheet_shapes(&sheet.rows, Vec2::new(x + pad, pad), sticky, &mut alloc);
        let Some(r) = shapes.iter().map(Shape::bounds).reduce(|a, b| a.union(&b)) else {
            continue;
        };
        if framed {
            let size = r.max - Vec2::new(x, 0.0) + pad;
            let mut f = Shape::frame(alloc(), Vec2::new(x, 0.0), size);
            f.text = sheet.name.clone();
            frames.push(f.into());
            x += size.x + SHEET_GAP;
        }
        content.extend(shapes.into_iter().map(Element::from));
    }
    let mut elements = frames;
    elements.extend(content);
    let Some(bounds) = elements
        .iter()
        .map(Element::bounds)
        .reduce(|a, b| a.union(&b))
    else {
        return Vec::new();
    };
    let d = center - bounds.center();
    for e in &mut elements {
        e.translate(d);
    }
    let ids = elements.iter().map(Element::id).collect();
    board.push_many(elements);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::ShapeKind;

    #[test]
    fn parses_quoted_csv() {
        let rows =
            parse("\u{feff}名前,メモ\r\n\"山田, 太郎\",\"1行目\n2行目\"\n佐藤,\"\"\"引用\"\"\"\n");
        assert_eq!(
            rows,
            vec![
                vec!["名前", "メモ"],
                vec!["山田, 太郎", "1行目\n2行目"],
                vec!["佐藤", "\"引用\""],
            ]
        );
        assert_eq!(parse("a\tb,c\n1\t2,3")[1], vec!["1", "2,3"]);
        assert_eq!(sniff_delimiter("a;b;c"), ';');
    }

    #[test]
    fn huge_csv_keeps_the_head_and_counts_the_rest() {
        let mut text = String::from("名前,点\n\n");
        for i in 0..10_000 {
            text.push_str(&format!("人{i},\"{i}\n点\"\n"));
        }
        let (rows, total) = parse_limited(&text, 3);
        assert_eq!(total, 10_001);
        assert_eq!(
            rows,
            vec![
                vec!["名前", "点"],
                vec![""],
                vec!["人0", "0\n点"],
                vec!["人1", "1\n点"],
            ]
        );
        assert_eq!(parse_limited("a,b\n1,2", 10), (parse("a,b\n1,2"), 2));
    }

    #[test]
    fn detects_spreadsheet_paste() {
        assert!(looks_like_tsv("a\tb\n1\t2\n"));
        assert!(!looks_like_tsv("a\tb"));
        assert!(!looks_like_tsv("a\tb\n1\t2\t3"));
        assert!(!looks_like_tsv("ふつうの\n文章"));
    }

    #[test]
    fn normalize_drops_empty_rows_and_columns() {
        let rows = normalize(vec![
            vec!["a".into(), "".into(), "b".into()],
            vec!["".into(), "".into()],
            vec!["1".into()],
        ]);
        assert_eq!(rows, vec![vec!["a", "b"], vec!["1", ""]]);
    }

    #[test]
    fn single_column_becomes_stickies_and_many_columns_a_table() {
        let mut board = Board::new();
        let one = Sheet {
            name: String::new(),
            rows: parse("アイデア1\nアイデア2\nアイデア3\nアイデア4\nアイデア5"),
        };
        let ids = insert(&mut board, vec![one], Vec2::ZERO, Rgba8::WHITE);
        assert_eq!(ids.len(), 5);
        assert!(
            board
                .elements()
                .iter()
                .all(|e| e.as_shape().is_some_and(|s| s.kind == ShapeKind::Sticky))
        );

        let mut board = Board::new();
        let t = Sheet {
            name: "売上".into(),
            rows: parse("月,金額\n1月,100\n2月,200"),
        };
        let u = Sheet {
            name: "メモ".into(),
            rows: parse("a\nb"),
        };
        insert(&mut board, vec![t, u], Vec2::ZERO, Rgba8::WHITE);
        let frames: Vec<_> = board
            .elements()
            .iter()
            .filter_map(Element::as_shape)
            .filter(|s| s.kind == ShapeKind::Frame)
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].text, "売上");
        let cell = board
            .elements()
            .iter()
            .filter_map(Element::as_shape)
            .find(|s| s.text == "1月")
            .unwrap();
        let g = table::grid(&board, cell.meta.group).unwrap();
        assert_eq!((g.rows(), g.cols()), (3, 2));
        assert!(frames[0].bounds().contains(table::bounds(&g).center()));
        assert!(board.bounds().unwrap().center().length() < 1.0);
    }
}
