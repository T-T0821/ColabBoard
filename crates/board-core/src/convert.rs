//! ファイルとボードの変換(CLI・サーバー・Web で共用する読み書き)。
//!
//! - 読み込み: グラボードのファイル(`.graboard`)・JSON(要素の一覧か一括操作)・マークダウン・
//!   Mermaid・PlantUML・draw.io・Miro・CSV / TSV(機能 `spreadsheet` があれば Excel なども)。
//! - 書き出し(文字): `.graboard`・JSON(要素の一覧)・マークダウン。画像と PDF は `board-export`。
//!
//! `.graboard` はボードの yrs 更新(v1)そのもので、画像も含めて欠けずに保存できる。
//! サーバーのボードと同じ形なので、そのまま読み書き・取り込みできる。

use glam::Vec2;

use crate::board::Board;
use crate::color::STICKY_COLORS;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::ops::{
    ElementBody, ElementData, EndpointData, MAX_OPS, MetaPatch, NewElement, Op, OpsRequest,
    ShapeStyle,
};

/// 読み込める形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    /// グラボードのファイル(yrs 更新)。
    Graboard,
    /// 要素の一覧(`[ElementData]`)か一括操作(`{"ops": [...]}`)。
    Json,
    Markdown,
    Mermaid,
    PlantUml,
    DrawIo,
    Miro,
    Csv,
    /// Excel・LibreOffice の表(機能 `spreadsheet` が必要)。
    Spreadsheet,
}

impl InputFormat {
    pub const ALL: [InputFormat; 9] = [
        InputFormat::Graboard,
        InputFormat::Json,
        InputFormat::Markdown,
        InputFormat::Mermaid,
        InputFormat::PlantUml,
        InputFormat::DrawIo,
        InputFormat::Miro,
        InputFormat::Csv,
        InputFormat::Spreadsheet,
    ];

    pub fn name(self) -> &'static str {
        match self {
            InputFormat::Graboard => "graboard",
            InputFormat::Json => "json",
            InputFormat::Markdown => "markdown",
            InputFormat::Mermaid => "mermaid",
            InputFormat::PlantUml => "plantuml",
            InputFormat::DrawIo => "drawio",
            InputFormat::Miro => "miro",
            InputFormat::Csv => "csv",
            InputFormat::Spreadsheet => "xlsx",
        }
    }

    /// 名前か拡張子から。
    pub fn from_name(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase()
                .as_str()
            {
                "graboard" | "board" => InputFormat::Graboard,
                "json" => InputFormat::Json,
                "md" | "markdown" | "txt" => InputFormat::Markdown,
                "mmd" | "mermaid" => InputFormat::Mermaid,
                "puml" | "plantuml" | "pu" | "iuml" | "wsd" => InputFormat::PlantUml,
                "drawio" | "dio" | "xml" => InputFormat::DrawIo,
                "miro" => InputFormat::Miro,
                "csv" | "tsv" => InputFormat::Csv,
                "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" | "excel" => InputFormat::Spreadsheet,
                _ => return None,
            },
        )
    }

    /// ファイル名(拡張子)と中身から推測する。
    pub fn detect(name: Option<&str>, bytes: &[u8]) -> Option<Self> {
        let by_ext = name
            .and_then(|n| n.rsplit_once('.'))
            .and_then(|(_, ext)| Self::from_name(ext));
        match by_ext {
            // JSON は Miro の書き出しかもしれない。
            Some(InputFormat::Json) => {
                let text = String::from_utf8_lossy(bytes);
                if crate::miro::looks_like_miro(&text) && !looks_like_ops_json(&text) {
                    Some(InputFormat::Miro)
                } else {
                    Some(InputFormat::Json)
                }
            }
            Some(f) => Some(f),
            None => Self::sniff(bytes),
        }
    }

    /// 中身だけから推測する。
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        // xlsx / ods は zip。
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"\xd0\xcf\x11\xe0") {
            return Some(InputFormat::Spreadsheet);
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let t = text.trim_start();
        if t.starts_with('{') || t.starts_with('[') {
            if looks_like_ops_json(t) {
                return Some(InputFormat::Json);
            }
            if crate::miro::looks_like_miro(t) {
                return Some(InputFormat::Miro);
            }
            return Some(InputFormat::Json);
        }
        if crate::drawio::looks_like_drawio(t) {
            return Some(InputFormat::DrawIo);
        }
        if crate::diagram::mermaid::looks_like_mermaid(t) {
            return Some(InputFormat::Mermaid);
        }
        if crate::diagram::plantuml::looks_like_plantuml(t) {
            return Some(InputFormat::PlantUml);
        }
        if crate::tabular::looks_like_tsv(t) {
            return Some(InputFormat::Csv);
        }
        if !t.is_empty() {
            return Some(InputFormat::Markdown);
        }
        // 文字でなければ yrs 更新とみなす。
        None
    }
}

/// 文字で書き出せる形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFormat {
    Graboard,
    Json,
    Markdown,
}

impl TextFormat {
    pub fn from_name(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase()
                .as_str()
            {
                "graboard" | "board" => TextFormat::Graboard,
                "json" => TextFormat::Json,
                "md" | "markdown" => TextFormat::Markdown,
                _ => return None,
            },
        )
    }

    pub fn extension(self) -> &'static str {
        match self {
            TextFormat::Graboard => "graboard",
            TextFormat::Json => "json",
            TextFormat::Markdown => "md",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            TextFormat::Graboard => "application/vnd.graboard",
            TextFormat::Json => "application/json",
            TextFormat::Markdown => "text/markdown; charset=utf-8",
        }
    }
}

fn looks_like_ops_json(text: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with('{') && t.contains("\"ops\""))
        || (t.starts_with('[') && (t.contains("\"op\"") || t.contains("\"type\"")))
}

fn utf8(bytes: &[u8]) -> Result<String, String> {
    #[cfg(feature = "spreadsheet")]
    {
        Ok(crate::sheets::decode_text(bytes))
    }
    #[cfg(not(feature = "spreadsheet"))]
    {
        String::from_utf8(bytes.to_vec()).map_err(|_| "UTF-8 の文字として読めません".to_string())
    }
}

/// `.graboard` を読んでボードにする。
pub fn load(bytes: &[u8]) -> Result<Board, String> {
    let mut b = Board::new();
    b.apply_remote(bytes)
        .map_err(|e| format!("グラボードのファイルとして読めません: {e}"))?;
    Ok(b)
}

/// ボードを文字の形式で書き出す。
pub fn save(board: &Board, format: TextFormat) -> Result<Vec<u8>, String> {
    match format {
        TextFormat::Graboard => board.encode_diff(None),
        TextFormat::Json => {
            let list = elements_json(board);
            serde_json::to_vec_pretty(&list).map_err(|e| e.to_string())
        }
        TextFormat::Markdown => Ok(crate::markdown::export(board, &[]).into_bytes()),
    }
}

/// 全要素の JSON 表現(重なり順)。
pub fn elements_json(board: &Board) -> Vec<ElementData> {
    crate::ops::query(board, &Default::default())
}

/// ボードの中身を、既存の要素と重ならない場所に読み込む。読み込んだ要素の ID を返す。
///
/// 空のボードには元の位置のまま(位置を持たない形式は原点を中心に)置く。
/// 中身があれば、既存の要素の右に並べる。
pub fn import(
    board: &mut Board,
    format: InputFormat,
    bytes: &[u8],
) -> Result<Vec<ElementId>, String> {
    let before = board.bounds();
    let ids = import_at(board, format, bytes, Vec2::ZERO)?;
    if ids.is_empty() {
        return Err("読み込める内容がありませんでした".into());
    }
    if let (Some(old), Some(new)) = (before, board.bounds_of(&ids)) {
        let d = Vec2::new(
            old.max.x + 160.0 - new.min.x,
            old.center().y - new.center().y,
        );
        for id in &ids {
            board.update(*id, |e| e.translate(d));
        }
    }
    Ok(ids)
}

/// `center` を中心に読み込む(draw.io・Miro・JSON・`.graboard` は元の位置関係のまま)。
pub fn import_at(
    board: &mut Board,
    format: InputFormat,
    bytes: &[u8],
    center: Vec2,
) -> Result<Vec<ElementId>, String> {
    let sticky = STICKY_COLORS[0];
    match format {
        InputFormat::Graboard => {
            let src = load(bytes)?;
            let elements: Vec<Element> = src.elements().to_vec();
            if board.is_empty() {
                // 空のボードには ID と位置をそのまま写す。
                let ids = elements.iter().map(Element::id).collect();
                board.push_many(elements);
                return Ok(ids);
            }
            let offset = src
                .bounds()
                .map(|b| center - b.center())
                .unwrap_or(Vec2::ZERO);
            Ok(crate::arrange::paste(board, elements, offset))
        }
        InputFormat::Json => import_json(board, &utf8(bytes)?),
        InputFormat::Markdown => {
            let text = utf8(bytes)?;
            match crate::markdown::insert(board, &text, center, sticky) {
                Some(r) => Ok(r.ids),
                None => Ok(Vec::new()),
            }
        }
        InputFormat::Mermaid | InputFormat::PlantUml => {
            let text = utf8(bytes)?;
            let spec = if format == InputFormat::Mermaid {
                crate::diagram::mermaid::parse(&text)?
            } else {
                crate::diagram::plantuml::parse(&text)?
            };
            let elements = crate::diagram::build(&spec, center, &mut || board.alloc_id());
            let ids = elements.iter().map(Element::id).collect();
            board.push_many(elements);
            Ok(ids)
        }
        InputFormat::DrawIo => crate::drawio::import(board, &utf8(bytes)?, center),
        InputFormat::Miro => crate::miro::import(board, &utf8(bytes)?, center),
        InputFormat::Csv => {
            let sheet = crate::tabular::Sheet {
                name: String::new(),
                rows: crate::tabular::parse(&utf8(bytes)?),
            };
            Ok(crate::tabular::insert(board, vec![sheet], center, sticky))
        }
        InputFormat::Spreadsheet => {
            #[cfg(feature = "spreadsheet")]
            {
                let sheets = crate::sheets::read_spreadsheet(bytes.to_vec())?;
                Ok(crate::tabular::insert(board, sheets, center, sticky))
            }
            #[cfg(not(feature = "spreadsheet"))]
            {
                Err("このビルドでは表計算ファイルを読めません".into())
            }
        }
    }
}

/// JSON を読み込む。次のどれか:
///
/// - 一括操作 `{"ops": [...]}` か操作の配列(REST API の `/ops` と同じ)
/// - 要素の一覧 `[ElementData]`(`--to json` や REST API の `/elements` の出力)か `{"elements": [...]}`
/// - 新しい要素の配列 `[NewElement]`
pub fn import_json(board: &mut Board, text: &str) -> Result<Vec<ElementId>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("JSON を読めません: {e}"))?;
    let list = match &value {
        serde_json::Value::Object(o) if o.contains_key("ops") => {
            let req: OpsRequest = serde_json::from_value(value)
                .map_err(|e| format!("一括操作として読めません: {e}"))?;
            return apply_ops(board, &req.ops);
        }
        serde_json::Value::Object(o) if o.contains_key("elements") => o["elements"].clone(),
        serde_json::Value::Array(_) => value,
        _ => return Err("JSON は配列か {\"ops\": [...]} にしてください".into()),
    };
    let items = list.as_array().cloned().unwrap_or_default();
    let Some(first) = items.first() else {
        return Ok(Vec::new());
    };
    if first.get("op").is_some() {
        let ops: Vec<Op> =
            serde_json::from_value(list).map_err(|e| format!("操作として読めません: {e}"))?;
        return apply_ops(board, &ops);
    }
    if first.get("id").is_some() && first.get("z").is_some() {
        let data: Vec<ElementData> =
            serde_json::from_value(list).map_err(|e| format!("要素として読めません: {e}"))?;
        return import_element_data(board, &data);
    }
    let elements: Vec<NewElement> =
        serde_json::from_value(list).map_err(|e| format!("要素として読めません: {e}"))?;
    let ops: Vec<Op> = elements
        .into_iter()
        .map(|element| Op::Create { element })
        .collect();
    apply_ops(board, &ops)
}

/// 操作を適用して、作った要素の ID を返す。失敗したらボードを元に戻す。
fn apply_ops(board: &mut Board, ops: &[Op]) -> Result<Vec<ElementId>, String> {
    let mut tmp = board_copy(board)?;
    let r = crate::ops::apply(&mut tmp, ops).map_err(|e| e.to_string())?;
    let created = r
        .created
        .iter()
        .filter_map(|s| crate::ops::parse_id(s))
        .collect();
    adopt(board, &tmp)?;
    Ok(created)
}

/// 失敗しても元のボードを壊さないための写し。
fn board_copy(board: &Board) -> Result<Board, String> {
    let mut b = Board::new();
    b.apply_remote(&board.encode_diff(None)?)?;
    Ok(b)
}

/// 写しで行った変更を元のボードに取り込む。
fn adopt(board: &mut Board, tmp: &Board) -> Result<(), String> {
    let diff = tmp.encode_diff(Some(&board.state_vector()))?;
    board.apply_remote(&diff)
}

/// 要素の一覧(JSON)を新しい ID で作り直す。画像の中身は JSON にないので空の画像枠になる。
fn import_element_data(board: &mut Board, data: &[ElementData]) -> Result<Vec<ElementId>, String> {
    let mut sorted: Vec<&ElementData> = data.iter().collect();
    sorted.sort_by(|a, b| a.z.total_cmp(&b.z));
    let mut tmp = board_copy(board)?;
    let mut map: std::collections::HashMap<String, String> = Default::default();
    let mut created: Vec<(ElementId, usize)> = Vec::new();
    let meta = |d: &ElementData| MetaPatch {
        group: (d.group != 0).then_some(d.group),
        locked: d.locked.then_some(true),
        link: (!d.link.is_empty()).then(|| d.link.clone()),
    };
    // 先に図形と手書き、あとでコネクタ(つなぐ先の新しい ID が要る)。
    for pass in [false, true] {
        let mut ops = Vec::new();
        let mut rank = Vec::new();
        for (i, d) in sorted.iter().enumerate() {
            let is_conn = matches!(d.body, ElementBody::Connector { .. });
            if is_conn != pass {
                continue;
            }
            let element = match &d.body {
                ElementBody::Shape {
                    kind,
                    x,
                    y,
                    w,
                    h,
                    text,
                    fill,
                    stroke,
                    stroke_width,
                    text_color,
                    font_size,
                    align,
                    icon,
                    ..
                } => NewElement::Shape {
                    kind: Some(kind.clone()),
                    x: *x,
                    y: *y,
                    w: Some(*w),
                    h: Some(*h),
                    style: ShapeStyle {
                        text: Some(text.clone()),
                        fill: Some(fill.clone()),
                        stroke: Some(stroke.clone()),
                        stroke_width: Some(*stroke_width),
                        text_color: Some(text_color.clone()),
                        font_size: (*font_size > 0.0).then_some(*font_size),
                        align: Some(align.clone()),
                        icon: (!icon.is_empty()).then(|| icon.clone()),
                    },
                    meta: meta(d),
                },
                ElementBody::Stroke {
                    points,
                    color,
                    width,
                    highlighter,
                } => NewElement::Stroke {
                    points: points.clone(),
                    color: Some(color.clone()),
                    width: Some(*width),
                    highlighter: Some(*highlighter),
                    meta: meta(d),
                },
                ElementBody::Connector {
                    from,
                    to,
                    text,
                    color,
                    width,
                    style,
                    start_cap,
                    end_cap,
                    dashed,
                } => {
                    let end = |e: &EndpointData| -> Option<EndpointData> {
                        match e {
                            EndpointData::Element { element, anchor } => {
                                Some(EndpointData::Element {
                                    element: map.get(element)?.clone(),
                                    anchor: *anchor,
                                })
                            }
                            p => Some(p.clone()),
                        }
                    };
                    // つなぐ先が一覧にないコネクタは捨てる。
                    let (Some(from), Some(to)) = (end(from), end(to)) else {
                        continue;
                    };
                    NewElement::Connector {
                        from,
                        to,
                        style: crate::ops::ConnectorStyle {
                            text: Some(text.clone()),
                            color: Some(color.clone()),
                            width: Some(*width),
                            style: Some(style.clone()),
                            start_cap: Some(start_cap.clone()),
                            end_cap: Some(end_cap.clone()),
                            dashed: Some(*dashed),
                        },
                        meta: meta(d),
                    }
                }
            };
            ops.push(Op::Create { element });
            rank.push(i);
        }
        for (chunk, ranks) in ops.chunks(MAX_OPS).zip(rank.chunks(MAX_OPS)) {
            let r = crate::ops::apply(&mut tmp, chunk).map_err(|e| e.to_string())?;
            for (new, &i) in r.created.iter().zip(ranks) {
                map.insert(sorted[i].id.clone(), new.clone());
                if let Some(id) = crate::ops::parse_id(new) {
                    created.push((id, i));
                }
            }
        }
    }
    // 重なり順を元の順に戻す(既存の要素より手前)。
    let top = tmp.z_range().1;
    let z: Vec<(ElementId, f64)> = created
        .iter()
        .map(|&(id, i)| (id, top + 1.0 + i as f64))
        .collect();
    tmp.set_z(&z);
    adopt(board, &tmp)?;
    let mut ids: Vec<(ElementId, usize)> = created;
    ids.sort_by_key(|&(_, i)| i);
    Ok(ids.into_iter().map(|(id, _)| id).collect())
}

/// テンプレートの名前(`kpt` など)か題名から探す。
pub fn find_template(name: &str) -> Option<crate::templates::Template> {
    use crate::templates::Template;
    let key = name.trim();
    Template::from_name(key)
        .or_else(|| Template::ALL.into_iter().find(|t| t.title() == key))
        .or_else(|| {
            let k = key.to_lowercase();
            Template::ALL
                .into_iter()
                .find(|t| t.title().to_lowercase().contains(&k) || t.name().contains(&k))
        })
}

/// テンプレートから新しいボードを作る。
pub fn from_template(t: crate::templates::Template) -> Board {
    let mut b = Board::new();
    crate::templates::insert(&mut b, t, Vec2::ZERO);
    b
}

/// ボードのあらまし(CLI の `info` 用)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub elements: usize,
    /// 種類ごとの数(多い順)。
    pub kinds: Vec<(String, usize)>,
    pub bounds: Option<Rect>,
    /// フレームの名前(スライドの順)。
    pub frames: Vec<String>,
    pub images: usize,
}

pub fn summary(board: &Board) -> Summary {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    let mut images = 0;
    for e in board.elements() {
        let kind = match e {
            Element::Shape(s) => {
                if s.image.is_some() {
                    images += 1;
                }
                s.kind.name().to_string()
            }
            Element::Connector(_) => "connector".into(),
            Element::Stroke(_) => "stroke".into(),
        };
        *counts.entry(kind).or_default() += 1;
    }
    let mut kinds: Vec<(String, usize)> = counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Summary {
        elements: board.len(),
        kinds,
        bounds: board.bounds(),
        frames: crate::slides::slides(board)
            .into_iter()
            .map(|s| s.title)
            .collect(),
        images,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShapeKind;

    #[test]
    fn graboard_round_trip_keeps_everything() {
        let b = Board::welcome();
        let bytes = save(&b, TextFormat::Graboard).unwrap();
        let back = load(&bytes).unwrap();
        assert_eq!(back.len(), b.len());
        assert_eq!(back.elements(), b.elements());
    }

    #[test]
    fn detects_formats() {
        assert_eq!(
            InputFormat::detect(Some("a.mmd"), b"graph TD; A-->B"),
            Some(InputFormat::Mermaid)
        );
        assert_eq!(
            InputFormat::sniff(b"graph TD\n A-->B"),
            Some(InputFormat::Mermaid)
        );
        assert_eq!(
            InputFormat::sniff(b"@startuml\nA -> B\n@enduml"),
            Some(InputFormat::PlantUml)
        );
        assert_eq!(
            InputFormat::sniff(b"# Title\n- a"),
            Some(InputFormat::Markdown)
        );
        assert_eq!(
            InputFormat::sniff(br#"{"ops": []}"#),
            Some(InputFormat::Json)
        );
        assert_eq!(
            InputFormat::from_name("XLSX"),
            Some(InputFormat::Spreadsheet)
        );
    }

    #[test]
    fn imports_mermaid_and_places_beside_existing() {
        let mut b = Board::welcome();
        let old = b.bounds().unwrap();
        let ids = import(
            &mut b,
            InputFormat::Mermaid,
            "graph LR\n A[開始] --> B[終了]".as_bytes(),
        )
        .unwrap();
        assert!(ids.len() >= 3);
        let new = b.bounds_of(&ids).unwrap();
        assert!(new.min.x > old.max.x);
    }

    #[test]
    fn ops_json_builds_a_board() {
        let mut b = Board::new();
        let json = r##"{"ops": [
            {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 0, "y": 0, "text": "A"}},
            {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 300, "y": 0, "text": "B"}},
            {"op": "create", "element": {"type": "connector", "from": {"element": "$0"}, "to": {"element": "$1"}}}
        ]}"##;
        let ids = import(&mut b, InputFormat::Json, json.as_bytes()).unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(b.len(), 3);
        // 失敗したら何も変わらない。
        let bad = r#"{"ops": [{"op": "delete", "id": "ffff"}]}"#;
        assert!(import(&mut b, InputFormat::Json, bad.as_bytes()).is_err());
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn element_json_round_trip() {
        let mut src = Board::new();
        let json = r##"[
            {"type": "shape", "kind": "rect", "x": 0, "y": 0, "w": 100, "h": 60, "text": "A", "fill": "#ff0000"},
            {"type": "shape", "kind": "ellipse", "x": 300, "y": 0, "text": "B"}
        ]"##;
        import(&mut src, InputFormat::Json, json.as_bytes()).unwrap();
        let ids: Vec<_> = src.elements().iter().map(Element::id).collect();
        let conn = crate::Connector::new(
            src.alloc_id(),
            crate::Endpoint::Attached {
                id: ids[0],
                anchor: None,
            },
            crate::Endpoint::Attached {
                id: ids[1],
                anchor: None,
            },
        );
        src.push(conn);
        let out = save(&src, TextFormat::Json).unwrap();
        let mut dst = Board::new();
        let created = import(&mut dst, InputFormat::Json, &out).unwrap();
        assert_eq!(created.len(), 3);
        let kinds: Vec<String> = elements_json(&dst)
            .iter()
            .map(|d| d.kind_name().to_string())
            .collect();
        assert_eq!(kinds, ["rect", "ellipse", "connector"]);
        let Element::Shape(s) = &dst.elements()[0] else {
            panic!()
        };
        assert_eq!(s.kind, ShapeKind::Rect);
        assert_eq!(s.text, "A");
    }

    #[test]
    fn templates_by_name_or_title() {
        let t = find_template("kpt").expect("kpt");
        assert!(!from_template(t).is_empty());
        assert_eq!(find_template(t.title()), Some(t));
        assert!(find_template("存在しないテンプレート").is_none());
    }

    #[test]
    fn summary_counts_kinds() {
        let s = summary(&Board::welcome());
        assert_eq!(s.elements, Board::welcome().len());
        assert!(!s.kinds.is_empty());
    }
}
