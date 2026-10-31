//! Mermaid の取り込み(フローチャートと状態遷移図)。
//!
//! 文を読んで [`DiagramSpec`] にし、レイアウトは図の自動配置に任せる。
//!
//! - `flowchart` / `graph`(向き `TD` `TB` `LR` `RL` `BT`)
//!   - ノードの形: `A[四角]` `A(角丸)` → 処理、`A{ひし形}` `A{{六角}}` → 判断、
//!     `A([スタジアム])` `A((円))` → 開始・終了、`A[(円柱)]` `A[/平行四辺形/]` → データ、`A>旗]` → メモ
//!   - 矢印: `-->` `---` `-.->` `==>` `--o` `--x`、ラベルは `-->|文字|` と `-- 文字 -->`。
//!     `A --> B --> C` のような連続や `A & B --> C` もよい
//!   - `subgraph ID [見出し]` 〜 `end` → 枠(入れ子可)
//! - `stateDiagram` / `stateDiagram-v2`: `A --> B : ラベル`、`[*]` は開始・終了
//!
//! `classDef` `style` `click` `linkStyle` `%%` などの見た目・注記は読み飛ばす。

use std::collections::HashMap;

use super::{DiagramSpec, Direction, EdgeSpec, GroupSpec, NodeSpec};

/// Mermaid らしい文字列か(先頭の意味のある行が図の種類の宣言)。
pub fn looks_like_mermaid(text: &str) -> bool {
    let Some(first) = meaningful_lines(text).next() else {
        return false;
    };
    let word = first.split_whitespace().next().unwrap_or("");
    matches!(
        word,
        "flowchart" | "graph" | "stateDiagram" | "stateDiagram-v2"
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
                });
            }
        }
        id.to_string()
    }

    fn edge(&mut self, from: &str, to: &str, label: &str) {
        self.spec.edges.push(EdgeSpec {
            from: from.into(),
            to: to.into(),
            label: label.trim().to_string(),
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

/// 矢印を読む。読んだ長さとラベル(なければ空)。
fn parse_arrow(s: &str) -> Option<(usize, String)> {
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
                    return Some((lead + open.len() + end + close.len(), label));
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
    Some((used, label))
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
    while let Some((used, label)) = parse_arrow(&line[pos..]) {
        pos += used;
        let Some((next, used)) = parse_group_of_nodes(b, &line[pos..]) else {
            return;
        };
        pos += used;
        for from in &prev {
            for to in &next {
                b.edge(from, to, &label);
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
    b.edge(&from, &to, label);
}

/// Mermaid を図の定義にする。読めなければ理由を返す。
pub fn parse(text: &str) -> Result<DiagramSpec, String> {
    let mut lines = meaningful_lines(text);
    let header = lines.next().ok_or("Mermaid の図がありません")?;
    let mut words = header.split_whitespace();
    let kind = words.next().unwrap_or("");
    let state = kind.starts_with("stateDiagram");
    if !state && kind != "flowchart" && kind != "graph" {
        return Err(format!(
            "この種類の Mermaid の図には対応していません: {kind}(flowchart / graph / stateDiagram に対応)"
        ));
    }
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
