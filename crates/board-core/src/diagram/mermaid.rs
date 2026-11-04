//! Mermaid の取り込みと書き出し。
//!
//! 文を読んで [`Diagram`] にし、レイアウトは図の自動配置に任せる。
//! 書き出し([`write`])は同じ書き方で出すので、取り込み → 書き出し → 取り込みで図が変わらない。
//!
//! - `flowchart` / `graph`(向き `TD` `TB` `LR` `RL` `BT`)
//!   - ノードの形: `A[四角]` `A(角丸)` → 処理、`A{ひし形}` `A{{六角}}` → 判断、
//!     `A([スタジアム])` `A((円))` → 開始・終了、`A[(円柱)]` `A[/平行四辺形/]` → データ、`A>旗]` → メモ
//!   - 矢印: `-->` `---` `-.->` `==>` `--o` `--x`、ラベルは `-->|文字|` と `-- 文字 -->`。
//!     `A --> B --> C` のような連続や `A & B --> C` もよい
//!   - `subgraph ID [見出し]` 〜 `end` → 枠(入れ子可)
//! - `stateDiagram` / `stateDiagram-v2`: `A --> B : ラベル`、`[*]` は開始・終了
//! - `classDiagram`: `class 名前["表示名"] { 属性・操作 }`、`<<interface>>`、
//!   関係 `<|--` `..|>` `*--` `o--` `-->` `..>` `--` `..`(ラベルは `: 文字`)→ UML のクラス
//! - `erDiagram`: `名前["表示名"] { 型 名前 PK }`、`||--o{` などのカーディナリティ → ER のエンティティ
//! - `sequenceDiagram`: `participant` `actor`(`as` で表示名)、`->>` `-->>` `->` `-->` `-x` `-)` → シーケンス図
//!
//! `classDef` `style` `click` `linkStyle` `%%` などの見た目・注記は読み飛ばす。

use std::collections::HashMap;

use super::{
    Diagram, DiagramSpec, Direction, EdgeLine, EdgeSpec, GroupSpec, NodeSpec, SequenceSpec, catalog,
};
use crate::connector::Cap;

/// Mermaid らしい文字列か(先頭の意味のある行が図の種類の宣言)。
pub fn looks_like_mermaid(text: &str) -> bool {
    let Some(first) = meaningful_lines(text).next() else {
        return false;
    };
    let word = first.split_whitespace().next().unwrap_or("");
    matches!(
        word,
        "flowchart"
            | "graph"
            | "stateDiagram"
            | "stateDiagram-v2"
            | "classDiagram"
            | "classDiagram-v2"
            | "erDiagram"
            | "sequenceDiagram"
    )
}

/// コメント・空行・``` の囲みを除いた行。
fn meaningful_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("%%") && !l.starts_with("```") && *l != "---")
}

#[derive(Default)]
struct Builder {
    spec: DiagramSpec,
    /// ノード ID → `spec.nodes` の位置。
    index: HashMap<String, usize>,
    /// 開いている枠(内側が最後)。
    groups: Vec<String>,
    /// `[*]` の開始・終了ノードの数。
    terminals: usize,
}

impl Builder {
    /// ノードを足す(すでにあれば、形と文字が書いてあるときだけ上書き)。
    fn node(&mut self, id: &str, kind: Option<&str>, label: Option<String>) -> String {
        let group = self.groups.last().cloned();
        match self.index.get(id) {
            Some(&i) => {
                let n = &mut self.spec.nodes[i];
                if let Some(k) = kind {
                    n.kind = k.into();
                }
                if let Some(l) = label {
                    n.label = l;
                }
                // 枠の中で初めて形を書いたら、その枠に入れる。
                if n.group.is_none() {
                    n.group = group;
                }
            }
            None => {
                self.index.insert(id.to_string(), self.spec.nodes.len());
                self.spec.nodes.push(NodeSpec {
                    id: id.to_string(),
                    kind: kind.unwrap_or("process").into(),
                    label: label.unwrap_or_else(|| id.to_string()),
                    group,
                    ..Default::default()
                });
            }
        }
        id.to_string()
    }

    fn edge(&mut self, from: &str, to: &str, label: &str, line: Option<EdgeLine>) {
        self.spec.edges.push(EdgeSpec {
            from: from.into(),
            to: to.into(),
            label: label.trim().to_string(),
            line,
        });
    }

    fn open_group(&mut self, id: String, label: String) {
        self.spec.groups.push(GroupSpec {
            id: id.clone(),
            kind: "generic".into(),
            label,
            parent: self.groups.last().cloned(),
        });
        self.groups.push(id);
    }

    /// `[*]` の開始(`start`)・終了ノード。
    fn terminal(&mut self, start: bool) -> String {
        self.terminals += 1;
        let id = format!("__terminal{}", self.terminals);
        let label = if start { "開始" } else { "終了" };
        self.node(&id, Some("terminal"), Some(label.into()))
    }
}

/// ラベルの引用符・HTML の改行を整える。
fn clean_label(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s);
    s.replace("<br/>", "\n")
        .replace("<br>", "\n")
        .replace("<br />", "\n")
        .replace("#quot;", "\"")
        .replace("#124;", "|")
        .trim()
        .to_string()
}

/// ノードの形の囲み(開き、閉じ、種類)。長いものから試す。
const SHAPES: &[(&str, &str, &str)] = &[
    ("(((", ")))", "terminal"),
    ("([", "])", "terminal"),
    ("((", "))", "terminal"),
    ("[(", ")]", "data"),
    ("[[", "]]", "process"),
    ("{{", "}}", "decision"),
    ("[/", "/]", "data"),
    ("[\\", "\\]", "data"),
    ("[/", "\\]", "data"),
    ("[\\", "/]", "data"),
    ("[", "]", "process"),
    ("(", ")", "process"),
    ("{", "}", "decision"),
    (">", "]", "note"),
];

/// `A[文字]` のようなノードの書き方を読む。読んだ分の長さも返す。
fn parse_node(s: &str) -> Option<(String, Option<&'static str>, Option<String>, usize)> {
    let mut id_len = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    // `-` `.` は矢印の始まりにもなるので、`--` `-.` の手前で切る。
    for arrow in ["--", "-.", ".-"] {
        if let Some(i) = s[..id_len].find(arrow) {
            id_len = id_len.min(i);
        }
    }
    let id = s[..id_len].trim_end_matches(['-', '.']);
    if id.is_empty() {
        return None;
    }
    let rest = &s[id.len()..];
    for (open, close, kind) in SHAPES {
        if let Some(body) = rest.strip_prefix(open)
            && let Some(end) = find_close(body, close)
        {
            let label = clean_label(&body[..end]);
            return Some((
                id.to_string(),
                Some(kind),
                Some(label),
                id.len() + open.len() + end + close.len(),
            ));
        }
    }
    Some((id.to_string(), None, None, id.len()))
}

/// 引用符の外で `close` が現れる位置。
fn find_close(body: &str, close: &str) -> Option<usize> {
    let mut quoted = false;
    for (i, c) in body.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && body[i..].starts_with(close) {
            return Some(i);
        }
    }
    None
}

/// 矢印の書き方から線の見た目を決める(`<-->` は両端、`-.->` は破線、`---` は矢じりなし)。
fn arrow_line(arrow: &str) -> Option<EdgeLine> {
    let head = |c: Option<char>, arrow_char: char| match c {
        Some(c) if c == arrow_char => Cap::Arrow,
        Some('o') => Cap::Circle,
        Some('x') => Cap::Arrow,
        _ => Cap::None,
    };
    EdgeLine {
        start: head(arrow.chars().next(), '<'),
        end: head(arrow.chars().last(), '>'),
        dashed: arrow.contains('.'),
    }
    .normalized()
}

/// 矢印を読む。読んだ長さ、ラベル(なければ空)、線の見た目。
fn parse_arrow(s: &str) -> Option<(usize, String, Option<EdgeLine>)> {
    let t = s.trim_start();
    let lead = s.len() - t.len();
    let first = t.chars().next()?;
    if !matches!(first, '-' | '=' | '.' | '<' | 'o' | 'x') {
        return None;
    }
    // `-- 文字 -->` の形。
    for (open, closes) in [
        ("--", &["-->", "---", "--o", "--x"][..]),
        ("==", &["==>", "==="][..]),
        ("-.", &[".->", ".-"][..]),
    ] {
        if let Some(after) = t.strip_prefix(open)
            && after.starts_with(' ')
        {
            for close in closes {
                if let Some(end) = after.find(close) {
                    let label = clean_label(&after[..end]);
                    let line = arrow_line(&format!("{open}{close}"));
                    return Some((lead + open.len() + end + close.len(), label, line));
                }
            }
        }
    }
    let len = t
        .char_indices()
        .find(|(i, c)| {
            !matches!(c, '-' | '=' | '.' | '>' | '<')
                && !(matches!(c, 'o' | 'x')
                    && *i > 0
                    && t[i + 1..].chars().next().is_none_or(char::is_whitespace))
        })
        .map(|(i, _)| i)
        .unwrap_or(t.len());
    let arrow = &t[..len];
    if arrow.len() < 2 || !(arrow.contains("--") || arrow.contains("==") || arrow.contains(".-")) {
        return None;
    }
    let line = arrow_line(arrow);
    let mut used = lead + len;
    let mut label = String::new();
    // `-->|文字|` の形。
    let after = &s[used..];
    let after_trim = after.trim_start();
    if let Some(body) = after_trim.strip_prefix('|')
        && let Some(end) = body.find('|')
    {
        label = clean_label(&body[..end]);
        used += after.len() - after_trim.len() + 1 + end + 1;
    }
    Some((used, label, line))
}

/// `A & B` のようなノードの並びを読む。
fn parse_group_of_nodes(b: &mut Builder, s: &str) -> Option<(Vec<String>, usize)> {
    let mut ids = Vec::new();
    let mut pos = 0;
    loop {
        let rest = &s[pos..];
        let trimmed = rest.trim_start();
        pos += rest.len() - trimmed.len();
        let (id, kind, label, used) = parse_node(trimmed)?;
        ids.push(b.node(&id, kind, label));
        pos += used;
        let rest = &s[pos..];
        let trimmed = rest.trim_start();
        if let Some(after) = trimmed.strip_prefix('&') {
            pos += rest.len() - after.len();
            continue;
        }
        return Some((ids, pos));
    }
}

fn flowchart_line(b: &mut Builder, line: &str) {
    let line = line.trim_end_matches(';').trim();
    let Some((mut prev, mut pos)) = parse_group_of_nodes(b, line) else {
        return;
    };
    while let Some((used, label, style)) = parse_arrow(&line[pos..]) {
        pos += used;
        let Some((next, used)) = parse_group_of_nodes(b, &line[pos..]) else {
            return;
        };
        pos += used;
        for from in &prev {
            for to in &next {
                b.edge(from, to, &label, style);
            }
        }
        prev = next;
    }
}

fn state_line(b: &mut Builder, line: &str) {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("state ") {
        // `state "説明" as ID` と `state ID {`。
        let rest = rest.trim_end_matches('{').trim();
        if let Some((label, id)) = rest.split_once(" as ") {
            b.node(id.trim(), None, Some(clean_label(label)));
        } else if line.ends_with('{') {
            b.open_group(rest.to_string(), rest.to_string());
        }
        return;
    }
    let (body, label) = match line.split_once(" : ").or_else(|| line.split_once(": ")) {
        Some((a, l)) => (a.trim(), l.trim()),
        None => (line, ""),
    };
    let Some((from, to)) = body.split_once("-->") else {
        // `ID : 説明`
        if !label.is_empty() && !body.contains(' ') {
            b.node(body, None, Some(clean_label(label)));
        }
        return;
    };
    let (from, to) = (from.trim(), to.trim());
    let from = if from == "[*]" {
        b.terminal(true)
    } else {
        b.node(from, None, None)
    };
    let to = if to == "[*]" {
        b.terminal(false)
    } else {
        b.node(to, None, None)
    };
    b.edge(&from, &to, label, None);
}

/// Mermaid のノードと矢印の図(フローチャート・状態遷移図・クラス図・ER 図)を図の定義にする。
/// シーケンス図は [`parse_diagram`] で読む。
pub fn parse(text: &str) -> Result<DiagramSpec, String> {
    match parse_diagram(text)? {
        Diagram::Flow(spec) => Ok(spec),
        Diagram::Sequence(_) => Err("シーケンス図はノードと矢印の図として読めません".into()),
    }
}

/// Mermaid を図にする。読めなければ理由を返す。
pub fn parse_diagram(text: &str) -> Result<Diagram, String> {
    let mut lines = meaningful_lines(text);
    let header = lines.next().ok_or("Mermaid の図がありません")?;
    let kind = header.split_whitespace().next().unwrap_or("");
    let rest: Vec<&str> = lines.collect();
    match kind {
        "flowchart" | "graph" | "stateDiagram" | "stateDiagram-v2" => {
            parse_flow(header, rest).map(Diagram::Flow)
        }
        "classDiagram" | "classDiagram-v2" => parse_class(rest, false).map(Diagram::Flow),
        "erDiagram" => parse_class(rest, true).map(Diagram::Flow),
        "sequenceDiagram" => parse_sequence(rest).map(Diagram::Sequence),
        _ => Err(format!(
            "この種類の Mermaid の図には対応していません: {kind}(flowchart / graph / stateDiagram / classDiagram / erDiagram / sequenceDiagram に対応)"
        )),
    }
}

fn parse_flow(header: &str, lines: Vec<&str>) -> Result<DiagramSpec, String> {
    let mut words = header.split_whitespace();
    let kind = words.next().unwrap_or("");
    let state = kind.starts_with("stateDiagram");
    let mut b = Builder::default();
    b.spec.direction = match words.next().unwrap_or(if state { "TB" } else { "TD" }) {
        "LR" | "RL" => Direction::Right,
        _ => Direction::Down,
    };
    let mut anon = 0;
    for line in lines {
        let first = line.split_whitespace().next().unwrap_or("");
        match first {
            "classDef" | "class" | "style" | "linkStyle" | "click" | "direction" | "note"
            | "accTitle" | "accDescr" => continue,
            "end" | "}" => {
                b.groups.pop();
                continue;
            }
            "subgraph" => {
                let rest = line["subgraph".len()..].trim();
                let (id, label) = match rest.find('[') {
                    Some(i) => (
                        rest[..i].trim().to_string(),
                        clean_label(rest[i + 1..].trim_end_matches(']')),
                    ),
                    None => {
                        anon += 1;
                        let label = clean_label(rest);
                        let id = if label.contains(' ') || label.is_empty() {
                            format!("__group{anon}")
                        } else {
                            label.clone()
                        };
                        (id, label)
                    }
                };
                b.open_group(id, label);
                continue;
            }
            _ => {}
        }
        if let Some(t) = line.strip_prefix("title ") {
            b.spec.title = t.trim().to_string();
            continue;
        }
        if state {
            state_line(&mut b, line);
        } else {
            flowchart_line(&mut b, line);
        }
    }
    // 中身のない枠は、同じ ID のノードとして書かれていることがあるので落とす。
    let used: Vec<String> = b
        .spec
        .nodes
        .iter()
        .filter_map(|n| n.group.clone())
        .chain(b.spec.groups.iter().filter_map(|g| g.parent.clone()))
        .collect();
    b.spec.groups.retain(|g| used.contains(&g.id));
    if b.spec.nodes.is_empty() {
        return Err("Mermaid の図にノードがありません".into());
    }
    Ok(b.spec)
}

// ---- クラス図・ER 図 ----

#[derive(Default)]
struct ClassBuilder {
    spec: DiagramSpec,
    index: HashMap<String, usize>,
    members: Vec<Vec<String>>,
    annotations: Vec<Option<String>>,
    groups: Vec<String>,
    er: bool,
}

/// `名前["表示名"]` を ID と表示名に分ける(`名前~T~` の型引数は落とす)。
fn split_name(s: &str) -> (String, Option<String>) {
    let s = s.trim().trim_end_matches('{').trim();
    let (id, label) = match s.find('[') {
        Some(i) => (&s[..i], Some(clean_label(s[i + 1..].trim_end_matches(']')))),
        None => (s, None),
    };
    let id = id.split('~').next().unwrap_or(id).trim();
    (unquote(id), label)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s)
        .to_string()
}

impl ClassBuilder {
    fn node(&mut self, id: &str, label: Option<String>) -> usize {
        if let Some(&i) = self.index.get(id) {
            if let Some(l) = label {
                self.spec.nodes[i].label = l;
            }
            return i;
        }
        let i = self.spec.nodes.len();
        self.index.insert(id.to_string(), i);
        self.spec.nodes.push(NodeSpec {
            id: id.to_string(),
            kind: if self.er { "entity" } else { "class" }.into(),
            label: label.unwrap_or_else(|| id.to_string()),
            group: self.groups.last().cloned(),
            ..NodeSpec::default()
        });
        self.members.push(Vec::new());
        self.annotations.push(None);
        i
    }

    fn member(&mut self, i: usize, line: &str) {
        let line = line.trim();
        if let Some(a) = line.strip_prefix("<<").and_then(|t| t.strip_suffix(">>")) {
            self.annotations[i] = Some(a.trim().to_ascii_lowercase());
        } else if !line.is_empty() {
            self.members[i].push(line.to_string());
        }
    }

    fn relation(&mut self, line: &str) -> bool {
        let (body, label) = match line.split_once(':') {
            Some((a, b)) => (a, clean_label(b)),
            None => (line, String::new()),
        };
        let Some((a, arrow, b)) = super::notation::split_relation(body) else {
            return false;
        };
        let (style, reversed) = super::notation::relation_line(&arrow);
        let (a, _) = split_name(&a);
        let (b, _) = split_name(&b);
        let ia = self.node(&a, None);
        let ib = self.node(&b, None);
        let (from, to) = if reversed { (ib, ia) } else { (ia, ib) };
        self.spec.edges.push(EdgeSpec {
            from: self.spec.nodes[from].id.clone(),
            to: self.spec.nodes[to].id.clone(),
            label,
            line: style.normalized(),
        });
        true
    }

    /// 集めた属性・注記から区画を作る。
    fn finish(mut self) -> DiagramSpec {
        for (i, n) in self.spec.nodes.iter_mut().enumerate() {
            let members = &self.members[i];
            if self.er {
                let cols: Vec<String> = members.iter().map(|m| er_column(m)).collect();
                n.sections = vec![cols.join("\n")];
                continue;
            }
            match self.annotations[i].as_deref() {
                Some("entity") => {
                    n.kind = "entity".into();
                    n.sections = vec![members.join("\n")];
                }
                Some(k) if k != "interface" && (k == "actor" || catalog::node_kind(k).id == k) => {
                    n.kind = k.into();
                }
                annotation => {
                    let interface = annotation == Some("interface");
                    if let Some(a) = annotation.filter(|_| !interface) {
                        n.label = format!("«{a}»\n{}", n.label);
                    }
                    let (ops, attrs): (Vec<&String>, Vec<&String>) =
                        members.iter().partition(|m| m.contains('('));
                    let join = |v: Vec<&String>| {
                        v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
                    };
                    let (attrs, ops) = (join(attrs), join(ops));
                    n.kind = if interface { "interface" } else { "class" }.into();
                    n.sections = if interface && attrs.is_empty() {
                        vec![ops]
                    } else {
                        vec![attrs, ops]
                    };
                }
            }
        }
        self.spec
    }
}

/// ER 図の属性 `型 名前 PK "説明"` を列 1 行にする。
fn er_column(line: &str) -> String {
    let mut words = line.split_whitespace();
    let ty = words.next().unwrap_or("");
    let name = words.next().unwrap_or("");
    let key = words
        .flat_map(|w| w.split(','))
        .map(str::trim)
        .find(|w| matches!(*w, "PK" | "FK" | "UK"))
        .unwrap_or("");
    if name.is_empty() {
        return super::notation::format_column(key, ty, "");
    }
    super::notation::format_column(key, name, ty)
}

fn parse_class(lines: Vec<&str>, er: bool) -> Result<DiagramSpec, String> {
    let mut b = ClassBuilder {
        er,
        ..ClassBuilder::default()
    };
    b.spec.direction = Direction::Down;
    let mut body: Option<usize> = None;
    let mut anon = 0;
    for line in lines {
        if let Some(i) = body {
            if line == "}" {
                body = None;
            } else {
                b.member(i, line);
            }
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        match first {
            "direction" => {
                b.spec.direction = match line.split_whitespace().nth(1) {
                    Some("LR" | "RL") => Direction::Right,
                    _ => Direction::Down,
                };
                continue;
            }
            "note" | "classDef" | "style" | "cssClass" | "click" | "link" | "callback"
            | "accTitle" | "accDescr" => continue,
            "title" => {
                b.spec.title = line[5..].trim().to_string();
                continue;
            }
            "}" => {
                b.groups.pop();
                continue;
            }
            "namespace" => {
                anon += 1;
                let label = line["namespace".len()..]
                    .trim_end_matches('{')
                    .trim()
                    .to_string();
                let id = format!("__ns{anon}");
                b.spec.groups.push(GroupSpec {
                    id: id.clone(),
                    kind: "generic".into(),
                    label,
                    parent: b.groups.last().cloned(),
                });
                b.groups.push(id);
                continue;
            }
            _ => {}
        }
        // `<<interface>> 名前`
        if let Some(rest) = line.strip_prefix("<<")
            && let Some((a, name)) = rest.split_once(">>")
        {
            let (id, _) = split_name(name);
            let i = b.node(&id, None);
            b.member(i, &format!("<<{a}>>"));
            continue;
        }
        if b.relation(line) {
            continue;
        }
        let decl = if !er && first == "class" {
            Some(line["class".len()..].trim())
        } else if er && !line.contains(':') {
            Some(line)
        } else {
            None
        };
        if let Some(decl) = decl {
            let opens = decl.ends_with('{');
            let (id, label) = split_name(decl);
            if id.is_empty() {
                continue;
            }
            let i = b.node(&id, label);
            if opens {
                body = Some(i);
            }
            continue;
        }
        // `名前 : +属性`
        if let Some((name, member)) = line.split_once(':') {
            let (id, _) = split_name(name);
            if !id.is_empty() && !id.contains(char::is_whitespace) {
                let i = b.node(&id, None);
                b.member(i, member);
            }
        }
    }
    let used: Vec<String> = b
        .spec
        .nodes
        .iter()
        .filter_map(|n| n.group.clone())
        .collect();
    b.spec.groups.retain(|g| used.contains(&g.id));
    if b.spec.nodes.is_empty() {
        return Err("Mermaid の図にクラス・エンティティがありません".into());
    }
    Ok(b.finish())
}

// ---- シーケンス図 ----

/// メッセージの矢印(長いものから)。`--` で始まるものは戻り(破線)。
const SEQ_ARROWS: &[&str] = &[
    "<<-->>", "<<->>", "-->>", "->>", "--x", "-x", "--)", "-)", "-->", "->",
];

fn parse_sequence(lines: Vec<&str>) -> Result<SequenceSpec, String> {
    let mut spec = SequenceSpec::default();
    for line in lines {
        let line = line.strip_prefix("create ").unwrap_or(line).trim();
        let first = line.split_whitespace().next().unwrap_or("");
        match first {
            "participant" | "actor" => {
                let rest = line[first.len()..].trim();
                match rest.split_once(" as ") {
                    Some((id, label)) => {
                        spec.participant(id.trim(), Some(&clean_label(label)));
                    }
                    None => {
                        spec.participant(rest, None);
                    }
                }
                continue;
            }
            "title" => {
                spec.title = line[5..].trim().trim_start_matches(':').trim().to_string();
                continue;
            }
            "note" | "Note" | "loop" | "alt" | "else" | "opt" | "par" | "and" | "end" | "rect"
            | "activate" | "deactivate" | "autonumber" | "critical" | "break" | "box"
            | "destroy" | "link" | "links" | "option" => continue,
            _ => {}
        }
        let (body, text) = match line.split_once(':') {
            Some((a, b)) => (a, b.trim()),
            None => (line, ""),
        };
        let found = SEQ_ARROWS
            .iter()
            .filter_map(|a| body.find(a).map(|i| (i, *a)))
            .min_by_key(|(i, a)| (*i, std::cmp::Reverse(a.len())));
        let Some((i, arrow)) = found else { continue };
        let from = body[..i].trim();
        let to = body[i + arrow.len()..]
            .trim()
            .trim_start_matches(['+', '-'])
            .trim();
        if from.is_empty() || to.is_empty() {
            continue;
        }
        let text = clean_label(text).replace("#59;", ";");
        spec.message(
            from,
            to,
            &text,
            arrow.starts_with("--") || arrow.starts_with("<<--"),
        );
    }
    if spec.participants.is_empty() {
        return Err("Mermaid のシーケンス図に参加者がありません".into());
    }
    Ok(spec)
}

// ---- 書き出し ----

/// 表示名を `"..."` で囲む(引用符と改行は Mermaid の書き方に直す)。
fn quote(s: &str) -> String {
    format!("\"{}\"", inline(s))
}

/// 1 行の文字にする(改行は `<br>`)。
fn inline(s: &str) -> String {
    s.trim()
        .replace('"', "#quot;")
        .replace("\r\n", "\n")
        .replace('\n', "<br>")
}

/// 図を Mermaid にする。
pub fn write(d: &Diagram) -> String {
    match d {
        Diagram::Flow(spec) => {
            let has = |k: &str| spec.nodes.iter().any(|n| n.kind == k);
            if has("class") || has("interface") {
                write_class(spec, false)
            } else if has("entity") {
                write_class(spec, true)
            } else {
                write_flowchart(spec)
            }
        }
        Diagram::Sequence(spec) => write_sequence(spec),
    }
}

/// ノードの形の囲み。
fn node_brackets(kind: &str) -> (&'static str, &'static str) {
    use super::NodeStyle;
    if kind == "actor" {
        return ("([", "])");
    }
    match catalog::node_kind(kind).style {
        NodeStyle::Decision => ("{", "}"),
        NodeStyle::Terminal => ("([", "])"),
        NodeStyle::Data => ("[(", ")]"),
        NodeStyle::Note => (">", "]"),
        NodeStyle::Process | NodeStyle::Service => ("[", "]"),
    }
}

/// フローチャートの矢印。
fn flow_arrow(line: Option<EdgeLine>) -> &'static str {
    let Some(l) = line else { return "-->" };
    let head = |c: Cap| match c {
        Cap::None => 0,
        Cap::Circle => 2,
        _ => 1,
    };
    match (head(l.start), head(l.end), l.dashed) {
        (0, 0, false) => "---",
        (0, 0, true) => "-.-",
        (0, 2, false) => "--o",
        (2, _, false) => "o--o",
        (0, _, false) => "-->",
        (0, _, true) => "-.->",
        (_, _, false) => "<-->",
        (_, _, true) => "<-.->",
    }
}

fn write_flowchart(spec: &DiagramSpec) -> String {
    let mut out = String::new();
    let dir = match spec.direction {
        Direction::Right => "LR",
        Direction::Down => "TD",
    };
    out.push_str(&format!("flowchart {dir}\n"));
    let node_line = |n: &NodeSpec, indent: usize| {
        let (open, close) = node_brackets(&n.kind);
        let text = super::node_text(catalog::node_kind(&n.kind), &n.label);
        format!(
            "{}{}{open}{}{close}\n",
            "    ".repeat(indent),
            n.id,
            quote(&text)
        )
    };
    fn group(
        spec: &DiagramSpec,
        g: &GroupSpec,
        indent: usize,
        out: &mut String,
        node_line: &dyn Fn(&NodeSpec, usize) -> String,
    ) {
        let pad = "    ".repeat(indent);
        out.push_str(&format!("{pad}subgraph {} [{}]\n", g.id, quote(&g.label)));
        for c in spec
            .groups
            .iter()
            .filter(|c| c.parent.as_ref() == Some(&g.id))
        {
            group(spec, c, indent + 1, out, node_line);
        }
        for n in spec
            .nodes
            .iter()
            .filter(|n| n.group.as_ref() == Some(&g.id))
        {
            out.push_str(&node_line(n, indent + 1));
        }
        out.push_str(&format!("{pad}end\n"));
    }
    for g in spec.groups.iter().filter(|g| g.parent.is_none()) {
        group(spec, g, 1, &mut out, &node_line);
    }
    for n in spec.nodes.iter().filter(|n| n.group.is_none()) {
        out.push_str(&node_line(n, 1));
    }
    for e in &spec.edges {
        let (from, to, line) = match e.line {
            // 始点だけの矢印は向きを入れ替える。
            Some(l) if l.end == Cap::None && l.start != Cap::None => (
                &e.to,
                &e.from,
                Some(EdgeLine {
                    start: l.end,
                    end: l.start,
                    dashed: l.dashed,
                }),
            ),
            l => (&e.from, &e.to, l),
        };
        let arrow = flow_arrow(line);
        if e.label.trim().is_empty() {
            out.push_str(&format!("    {from} {arrow} {to}\n"));
        } else {
            let label = quote(&e.label).replace('|', "#124;");
            out.push_str(&format!("    {from} {arrow}|{label}| {to}\n"));
        }
    }
    out
}

/// ER 図の属性に使える語(空白は `_`)。
fn er_word(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join("_")
}

fn write_class(spec: &DiagramSpec, er: bool) -> String {
    let mut out = String::from(if er { "erDiagram\n" } else { "classDiagram\n" });
    let dir = match spec.direction {
        Direction::Right => "LR",
        Direction::Down => "TB",
    };
    out.push_str(&format!("    direction {dir}\n"));
    for n in &spec.nodes {
        let name = if n.label == n.id {
            n.id.clone()
        } else {
            format!("{}[{}]", n.id, quote(&n.label))
        };
        let decl = if er {
            format!("    {name}")
        } else {
            format!("    class {name}")
        };
        let mut body: Vec<String> = Vec::new();
        match n.kind.as_str() {
            "entity" if er => {
                for line in n.sections.iter().flat_map(|s| super::notation::lines_of(s)) {
                    let (key, name, ty) = super::notation::parse_column(line);
                    let ty = if ty.is_empty() {
                        "string".into()
                    } else {
                        er_word(&ty)
                    };
                    let key = if key.is_empty() {
                        String::new()
                    } else {
                        format!(" {key}")
                    };
                    body.push(format!("{ty} {}{key}", er_word(&name)));
                }
            }
            "class" | "interface" if !er => {
                if n.kind == "interface" {
                    body.push("<<interface>>".into());
                }
                body.extend(
                    n.sections
                        .iter()
                        .flat_map(|s| super::notation::lines_of(s))
                        .map(str::to_string),
                );
            }
            _ if er => {}
            k => {
                body.push(format!("<<{k}>>"));
                if k == "entity" {
                    body.extend(
                        n.sections
                            .iter()
                            .flat_map(|s| super::notation::lines_of(s))
                            .map(str::to_string),
                    );
                }
            }
        }
        if body.is_empty() {
            out.push_str(&format!("{decl}\n"));
        } else {
            out.push_str(&format!("{decl} {{\n"));
            for l in body {
                out.push_str(&format!("        {l}\n"));
            }
            out.push_str("    }\n");
        }
    }
    for e in &spec.edges {
        let l = e.line.unwrap_or(EdgeLine::ARROW);
        let body = if l.dashed { ".." } else { "--" };
        let arrow = format!(
            "{}{body}{}",
            super::notation::left_head(l.start, er),
            super::notation::right_head(l.end, er)
        );
        let label = e.label.split_whitespace().collect::<Vec<_>>().join(" ");
        if er {
            out.push_str(&format!(
                "    {} {arrow} {} : {}\n",
                e.from,
                e.to,
                quote(&label)
            ));
        } else if label.is_empty() {
            out.push_str(&format!("    {} {arrow} {}\n", e.from, e.to));
        } else {
            out.push_str(&format!("    {} {arrow} {} : {}\n", e.from, e.to, label));
        }
    }
    out
}

fn write_sequence(spec: &SequenceSpec) -> String {
    let mut out = String::from("sequenceDiagram\n");
    if !spec.title.is_empty() {
        out.push_str(&format!("    title {}\n", inline(&spec.title)));
    }
    for p in &spec.participants {
        if p.label == p.id {
            out.push_str(&format!("    participant {}\n", p.id));
        } else {
            out.push_str(&format!(
                "    participant {} as {}\n",
                p.id,
                inline(&p.label)
            ));
        }
    }
    for m in &spec.messages {
        let arrow = if m.reply { "-->>" } else { "->>" };
        let text = inline(&m.label).replace(';', "#59;");
        out.push_str(&format!("    {}{arrow}{}: {text}\n", m.from, m.to));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node<'a>(s: &'a DiagramSpec, id: &str) -> &'a NodeSpec {
        s.nodes.iter().find(|n| n.id == id).unwrap()
    }

    #[test]
    fn flowchart_shapes_edges_and_subgraphs() {
        let s = parse(
            "```mermaid
flowchart LR
    %% コメント
    A([開始]) --> B{ログイン済み?}
    B -->|はい| C[ダッシュボード]
    B -- いいえ --> D[(ユーザーDB)]
    subgraph auth [認証]
        D -.-> E(\"ログイン<br>画面\")
    end
    C & E ==> F((終了));
    classDef red fill:#f00
```",
        )
        .unwrap();
        assert_eq!(s.direction, Direction::Right);
        assert_eq!(node(&s, "A").kind, "terminal");
        assert_eq!(node(&s, "B").kind, "decision");
        assert_eq!(node(&s, "B").label, "ログイン済み?");
        assert_eq!(node(&s, "D").kind, "data");
        assert_eq!(node(&s, "E").label, "ログイン\n画面");
        assert_eq!(node(&s, "E").group.as_deref(), Some("auth"));
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].label, "認証");
        let labels: Vec<(&str, &str, &str)> = s
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("A", "B", ""),
                ("B", "C", "はい"),
                ("B", "D", "いいえ"),
                ("D", "E", ""),
                ("C", "F", ""),
                ("E", "F", ""),
            ]
        );
    }

    #[test]
    fn chains_and_plain_ids() {
        let s = parse("graph TD\nA --> B --> C\nC---A").unwrap();
        assert_eq!(s.direction, Direction::Down);
        assert_eq!(s.nodes.len(), 3);
        assert_eq!(s.edges.len(), 3);
        assert_eq!(node(&s, "A").label, "A");
    }

    #[test]
    fn state_diagram() {
        let s = parse(
            "stateDiagram-v2
    [*] --> 待機
    待機 --> 実行中 : 開始
    実行中 --> 待機 : 停止
    実行中 --> [*]",
        )
        .unwrap();
        assert_eq!(s.nodes.len(), 4);
        assert_eq!(s.nodes[0].kind, "terminal");
        assert_eq!(s.edges[1].label, "開始");
    }

    #[test]
    fn detection_and_errors() {
        assert!(looks_like_mermaid("\n```mermaid\ngraph LR\nA-->B\n```"));
        assert!(!looks_like_mermaid("# 見出し"));
        assert!(parse("sequenceDiagram\nA->>B: hi").is_err());
    }
}
