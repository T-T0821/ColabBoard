//! PlantUML の取り込みと書き出し。
//!
//! 文を読んで [`Diagram`] にし、レイアウトは図の自動配置に任せる。
//! 書き出し([`write`])は同じ書き方で出すので、取り込み → 書き出し → 取り込みで図が変わらない。
//!
//! - アクティビティ図(新しい書き方): `start` `stop` `end` `:処理;`
//!   `if (条件) then (はい)` 〜 `elseif` 〜 `else (いいえ)` 〜 `endif`、
//!   `while (条件)` 〜 `endwhile`、`repeat` 〜 `repeat while (条件)`、`note`(読み飛ばす)
//! - そのほかの図(コンポーネント・ユースケース・配置・シーケンスなど):
//!   `A -> B : ラベル` の矢印(`-->` `..>` `-left->` `<-` なども)、
//!   `actor` `database` `component` `node` `rectangle` `usecase` などの宣言(`"名前" as ID`)、
//!   `[部品]` `(ユースケース)` `:アクター:` の書き方、`package` `node` `rectangle` などの `{ }` → 枠、
//!   `note "文字" as ID` → メモ、`hexagon` → 判断
//! - クラス図・ER 図: `class` `interface` `abstract` `enum` `entity` の `{ }` の中身(`--` で区画を分ける)
//!   → UML のクラス・エンティティ。関係 `<|--` `..|>` `*--` `o--` `||--o{` などの端の記号も写す
//! - シーケンス図: `participant` などの宣言と `A -> B : 文字` だけの図(`-->` は戻り)
//!
//! `skinparam` `title` 以外の見た目の指定は読み飛ばす。

use std::collections::HashMap;

use super::notation::{self, relation_line, split_relation};
use super::{
    Diagram, DiagramSpec, Direction, EdgeLine, EdgeSpec, GroupSpec, NodeSpec, NodeStyle,
    SequenceSpec, catalog,
};
use crate::connector::Cap;

/// PlantUML らしい文字列か。
pub fn looks_like_plantuml(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.starts_with("@start"))
}

/// 宣言のキーワードと、ノードの種類。
const DECLS: &[(&str, &str)] = &[
    ("actor", "actor"),
    ("hexagon", "decision"),
    ("participant", "process"),
    ("boundary", "process"),
    ("control", "process"),
    ("entity", "entity"),
    ("database", "data"),
    ("collections", "data"),
    ("queue", "data"),
    ("storage", "data"),
    ("file", "data"),
    ("artifact", "data"),
    ("component", "process"),
    ("interface", "interface"),
    ("usecase", "terminal"),
    ("agent", "process"),
    ("card", "process"),
    ("class", "class"),
    ("abstract", "class"),
    ("enum", "class"),
    ("object", "process"),
    ("cloud", "process"),
    ("node", "process"),
    ("rectangle", "process"),
    ("package", "process"),
    ("frame", "process"),
    ("folder", "data"),
    ("person", "terminal"),
    ("state", "process"),
];

/// 枠にできるキーワード(`{` で始まれば枠)。
const CONTAINERS: &[&str] = &[
    "package",
    "node",
    "rectangle",
    "folder",
    "frame",
    "cloud",
    "database",
    "component",
    "namespace",
    "together",
    "box",
    "state",
];

#[derive(Default)]
struct Builder {
    spec: DiagramSpec,
    index: HashMap<String, usize>,
    /// 名前 → ID(`"名前" as ID` の別名)。
    alias: HashMap<String, String>,
    groups: Vec<String>,
    auto: usize,
    // ---- アクティビティ図 ----
    /// 次のノードにつなぐ元(ノード ID とラベル)。
    tails: Vec<(String, String)>,
    /// 開いている if / while / repeat。
    blocks: Vec<Block>,
}

enum Block {
    If {
        decision: String,
        /// 終わった分岐の末尾。
        ends: Vec<(String, String)>,
        has_else: bool,
    },
    While {
        decision: String,
    },
    Repeat {
        /// 繰り返しの先頭につなぐ目印(最初のノード)。
        first: Option<String>,
        start_tails: Vec<(String, String)>,
    },
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s);
    s.replace("\\n", "\n").trim().to_string()
}

/// `(文字)` の中身。
fn paren(s: &str) -> Option<String> {
    let start = s.find('(')?;
    let end = s[start..].find(')')? + start;
    Some(s[start + 1..end].trim().to_string())
}

impl Builder {
    fn node(&mut self, id: &str, kind: &str, label: &str) -> String {
        let id = self
            .alias
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string());
        if !self.index.contains_key(&id) {
            self.index.insert(id.clone(), self.spec.nodes.len());
            self.spec.nodes.push(NodeSpec {
                id: id.clone(),
                kind: kind.into(),
                label: label.into(),
                group: self.groups.last().cloned(),
                ..Default::default()
            });
        }
        id
    }

    fn edge(&mut self, from: &str, to: &str, label: &str) {
        self.edge_with(from, to, label, None);
    }

    fn edge_with(&mut self, from: &str, to: &str, label: &str, line: Option<EdgeLine>) {
        self.spec.edges.push(EdgeSpec {
            from: from.into(),
            to: to.into(),
            label: label.trim().into(),
            line,
        });
    }

    fn fresh(&mut self) -> String {
        self.auto += 1;
        format!("__n{}", self.auto)
    }

    // ---- アクティビティ図 ----

    /// 新しいノードを今の末尾につなぐ。
    fn step(&mut self, kind: &str, label: &str) -> String {
        let id = self.fresh();
        self.node(&id, kind, label);
        for (from, l) in std::mem::take(&mut self.tails) {
            self.edge(&from, &id, &l);
        }
        if let Some(Block::Repeat { first, .. }) = self.blocks.last_mut()
            && first.is_none()
        {
            *first = Some(id.clone());
        }
        self.tails = vec![(id.clone(), String::new())];
        id
    }

    fn activity_line(&mut self, line: &str) -> bool {
        let lower = line.to_ascii_lowercase();
        let word = lower.split([' ', '(']).next().unwrap_or("");
        match word {
            "start" => {
                self.step("terminal", "開始");
            }
            "stop" | "end" | "kill" | "detach" => {
                self.step("terminal", "終了");
                self.tails.clear();
            }
            "if" => {
                let cond = paren(line).unwrap_or_default();
                let decision = self.step("decision", &cond);
                let yes = line
                    .split_once(" then")
                    .and_then(|(_, t)| paren(t))
                    .unwrap_or_default();
                self.tails = vec![(decision.clone(), yes)];
                self.blocks.push(Block::If {
                    decision,
                    ends: Vec::new(),
                    has_else: false,
                });
            }
            "elseif" | "else" => {
                let label = if word == "elseif" {
                    line.split_once(" then")
                        .and_then(|(_, t)| paren(t))
                        .unwrap_or_default()
                } else {
                    paren(line).unwrap_or_default()
                };
                let cond = paren(line).unwrap_or_default();
                let tails = std::mem::take(&mut self.tails);
                let Some(Block::If {
                    decision,
                    ends,
                    has_else,
                }) = self.blocks.last_mut()
                else {
                    return true;
                };
                ends.extend(tails);
                let from = decision.clone();
                if word == "elseif" {
                    // 次の判断を前の判断の「いいえ」につなぐ。
                    let id = self.fresh();
                    self.node(&id, "decision", &cond);
                    self.edge(&from, &id, "");
                    if let Some(Block::If { decision, .. }) = self.blocks.last_mut() {
                        *decision = id.clone();
                    }
                    self.tails = vec![(id, label)];
                } else {
                    *has_else = true;
                    self.tails = vec![(from, label)];
                }
            }
            "endif" => {
                if let Some(Block::If {
                    decision,
                    ends,
                    has_else,
                }) = self.blocks.pop()
                {
                    let mut tails = ends;
                    tails.append(&mut self.tails);
                    if !has_else {
                        tails.push((decision, String::new()));
                    }
                    self.tails = tails;
                }
            }
            "while" => {
                let cond = paren(line).unwrap_or_default();
                let decision = self.step("decision", &cond);
                let yes = line
                    .split_once(" is")
                    .and_then(|(_, t)| paren(t))
                    .unwrap_or_default();
                self.tails = vec![(decision.clone(), yes)];
                self.blocks.push(Block::While { decision });
            }
            "endwhile" => {
                if let Some(Block::While { decision }) = self.blocks.pop() {
                    for (from, l) in std::mem::take(&mut self.tails) {
                        self.edge(&from, &decision, &l);
                    }
                    self.tails = vec![(decision, paren(line).unwrap_or_default())];
                }
            }
            "repeat" if lower.starts_with("repeat while") => {
                if let Some(Block::Repeat { first, start_tails }) = self.blocks.pop() {
                    let cond = paren(line).unwrap_or_default();
                    let decision = self.step("decision", &cond);
                    if let Some(first) = first {
                        self.edge(&decision, &first, "");
                    }
                    let _ = start_tails;
                    let no = line
                        .split_once(" not")
                        .and_then(|(_, t)| paren(t))
                        .unwrap_or_default();
                    self.tails = vec![(decision, no)];
                }
            }
            "repeat" => {
                let start_tails = self.tails.clone();
                self.blocks.push(Block::Repeat {
                    first: None,
                    start_tails,
                });
            }
            _ => {
                if let Some(body) = line.strip_prefix(':') {
                    let text = body
                        .trim_end_matches([';', '|', '<', '>', '/', ']', '}'])
                        .trim();
                    self.step("process", &text.replace("\\n", "\n"));
                } else {
                    return false;
                }
            }
        }
        true
    }

    // ---- そのほかの図 ----

    /// `[部品]` `(ユースケース)` `:アクター:` `"名前"` `ID` のどれかを読んでノードにする。
    fn operand(&mut self, s: &str) -> Option<String> {
        let s = s.trim();
        let (kind, label) = if let Some(t) = s.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            ("process", t)
        } else if let Some(t) = s.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            ("terminal", t)
        } else if let Some(t) = s.strip_prefix(':').and_then(|t| t.strip_suffix(':')) {
            ("terminal", t)
        } else {
            ("process", s)
        };
        let label = unquote(label);
        if label.is_empty() {
            return None;
        }
        // 名前に空白などがあれば、名前そのものを ID にする。
        Some(self.node(&label, kind, &label))
    }

    fn declaration(&mut self, line: &str) -> bool {
        // `[部品] as ID` `(ユースケース) as ID` のようにキーワードのない宣言。
        if let Some((name, id)) = line.split_once(" as ")
            && name.starts_with(['[', '(', ':'])
        {
            let kind = if name.starts_with('[') {
                "process"
            } else {
                "terminal"
            };
            let label = unquote(name.trim_matches(['[', ']', '(', ')', ':']));
            let id = unquote(id);
            self.alias.insert(label.clone(), id.clone());
            self.node(&id, kind, &label);
            return true;
        }
        let word = line.split_whitespace().next().unwrap_or("");
        let Some((_, kind)) = DECLS.iter().find(|(k, _)| *k == word) else {
            return false;
        };
        let opens = line.trim_end().ends_with('{');
        let rest = line[word.len()..].trim().trim_end_matches('{').trim();
        // 色やステレオタイプ(`#red` `<<x>>`)を落とす。
        let rest = rest
            .split(" #")
            .next()
            .unwrap_or(rest)
            .split(" <<")
            .next()
            .unwrap_or(rest)
            .trim();
        let (label, id) = match rest.split_once(" as ") {
            Some((a, b)) => (unquote(a), unquote(b)),
            None => {
                let n = unquote(rest);
                (n.clone(), n)
            }
        };
        let label = label
            .trim_start_matches(['[', '(', ':'])
            .trim_end_matches([']', ')', ':'])
            .to_string();
        if id.is_empty() {
            return true;
        }
        if opens && CONTAINERS.contains(&word) {
            self.spec.groups.push(GroupSpec {
                id: id.clone(),
                kind: "generic".into(),
                label,
                parent: self.groups.last().cloned(),
            });
            self.groups.push(id);
        } else {
            if label != id {
                self.alias.insert(label.clone(), id.clone());
            }
            self.node(&id, kind, &label);
        }
        true
    }

    fn relation(&mut self, line: &str) -> bool {
        let (body, label) = match line.split_once(" : ") {
            Some((a, b)) => (a, b.trim()),
            None => match line.split_once(':') {
                Some((a, b)) if !a.trim_end().ends_with(':') && !a.trim().is_empty() => {
                    (a, b.trim())
                }
                _ => (line, ""),
            },
        };
        let Some((a, arrow, b)) = split_arrow(body) else {
            return false;
        };
        let (Some(x), Some(y)) = (self.operand(&a), self.operand(&b)) else {
            return false;
        };
        let (line, reversed) = relation_line(&arrow);
        let label = label.replace("\\n", "\n");
        if reversed {
            self.edge_with(&y, &x, &label, line.normalized());
        } else {
            self.edge_with(&x, &y, &label, line.normalized());
        }
        true
    }
}

/// 関係の文を「左」「矢印」「右」に分ける。多重度の `"1"` は落とす。
fn split_arrow(body: &str) -> Option<(String, String, String)> {
    if let Some(parts) = split_relation(body) {
        return Some(parts);
    }
    // 矢印は `-` `.` `=` と `<` `>` `*` `o` `|` などの並び(`-left->` のような向き指定も)。
    let (start, end) = find_arrow(body)?;
    let arrow = &body[start..end];
    let (a, b) = (body[..start].trim(), body[end..].trim());
    // 多重度の `"1"` などを落とす。
    let strip_mult = |s: &str, tail: bool| -> String {
        let s = s.trim();
        if tail {
            match s.rfind(" \"") {
                Some(i) if s.ends_with('"') => s[..i].trim().to_string(),
                _ => s.to_string(),
            }
        } else {
            match s.strip_prefix('"').and_then(|t| t.split_once("\" ")) {
                Some((_, rest)) => rest.trim().to_string(),
                None => s.to_string(),
            }
        }
    };
    Some((strip_mult(a, true), arrow.to_string(), strip_mult(b, false)))
}

/// 矢印の位置(開始と終了)。空白で区切られているか、`[` `)` などに接している。
fn find_arrow(s: &str) -> Option<(usize, usize)> {
    let bytes: Vec<(usize, char)> = s.char_indices().collect();
    let is_arrow_char = |c: char| matches!(c, '-' | '.' | '=' | '<' | '>' | '*' | '|' | '#' | '^');
    let mut i = 0;
    let mut quoted = false;
    while i < bytes.len() {
        let (pos, c) = bytes[i];
        if c == '"' {
            quoted = !quoted;
        }
        if !quoted && (c == '-' || c == '.' || c == '<' || c == '=') {
            let mut j = i;
            let mut dashes = 0;
            while j < bytes.len() {
                let (_, d) = bytes[j];
                if is_arrow_char(d) {
                    if matches!(d, '-' | '.' | '=') {
                        dashes += 1;
                    }
                    j += 1;
                } else if d.is_ascii_alphabetic() && dashes > 0 {
                    // `-left->` `-[#red]->` などの向き・色の指定。
                    let rest: String = bytes[j..].iter().map(|(_, c)| *c).collect();
                    let word_len = rest
                        .find(|c: char| !c.is_ascii_alphabetic())
                        .unwrap_or(rest.len());
                    let word = &rest[..word_len];
                    if matches!(
                        word,
                        "left"
                            | "right"
                            | "up"
                            | "down"
                            | "l"
                            | "r"
                            | "u"
                            | "d"
                            | "le"
                            | "ri"
                            | "do"
                    ) && rest[word_len..].starts_with(['-', '.'])
                    {
                        j += word_len;
                    } else {
                        break;
                    }
                } else if d == '[' && dashes > 0 {
                    // `-[#red]->`
                    match bytes[j..].iter().position(|(_, c)| *c == ']') {
                        Some(k) => j += k + 1,
                        None => break,
                    }
                } else {
                    break;
                }
            }
            let end = bytes.get(j).map(|(p, _)| *p).unwrap_or(s.len());
            if dashes >= 1 && j - i >= 2 {
                return Some((pos, end));
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    None
}

/// クラス・エンティティの中身を読んでいる途中。
struct ClassBody {
    id: String,
    kind: &'static str,
    lines: Vec<String>,
}

/// 中身(`{ }`)を持てるクラス図・ER 図の宣言のキーワードと種類。
const CLASS_WORDS: &[(&str, &str)] = &[
    ("class", "class"),
    ("abstract", "class"),
    ("enum", "class"),
    ("annotation", "class"),
    ("interface", "interface"),
    ("entity", "entity"),
];

/// 区画の区切りの行(`--` `..` `==` `__`、`-- 見出し --` も)か。
fn is_separator(line: &str) -> bool {
    ["--", "..", "==", "__"]
        .iter()
        .any(|s| line.starts_with(s) && line.ends_with(s))
}

impl Builder {
    /// `class "名前" as ID {` なら中身を読み始める。
    fn open_class(&mut self, line: &str) -> Option<ClassBody> {
        if !line.ends_with('{') {
            return None;
        }
        let mut words = line.split_whitespace();
        let mut word = words.next()?;
        if word == "abstract" && line.split_whitespace().nth(1) == Some("class") {
            word = "class";
        }
        let (_, kind) = CLASS_WORDS.iter().find(|(k, _)| *k == word)?;
        let rest = line.trim_end_matches('{').trim();
        let rest = rest.strip_prefix("abstract").map(str::trim).unwrap_or(rest);
        let rest = rest.strip_prefix(word)?.trim();
        // 色やステレオタイプ(`#red` `<<x>>`)を落とす。
        let rest = rest.split(" #").next().unwrap_or(rest);
        let rest = rest.split(" <<").next().unwrap_or(rest).trim();
        let (label, id) = match rest.split_once(" as ") {
            Some((a, b)) => (unquote(a), unquote(b)),
            None => (unquote(rest), unquote(rest)),
        };
        if id.is_empty() {
            return None;
        }
        if label != id {
            self.alias.insert(label.clone(), id.clone());
        }
        let id = self.node(&id, kind, &label);
        Some(ClassBody {
            id,
            kind,
            lines: Vec::new(),
        })
    }

    /// 読んだ中身を区画にする。
    fn close_class(&mut self, body: ClassBody) {
        let Some(&i) = self.index.get(&body.id) else {
            return;
        };
        let lines: Vec<&str> = body.lines.iter().map(String::as_str).collect();
        let sections = if body.kind == "entity" {
            let cols: Vec<String> = lines
                .iter()
                .filter(|l| !is_separator(l))
                .map(|l| {
                    let (key, name, ty) = notation::parse_column(l);
                    notation::format_column(&key, &name, &ty)
                })
                .collect();
            vec![cols.join("\n")]
        } else if lines.iter().any(|l| is_separator(l)) {
            lines
                .split(|l| is_separator(l))
                .map(|part| part.join("\n"))
                .collect()
        } else {
            let (ops, attrs): (Vec<&str>, Vec<&str>) = lines.iter().partition(|l| l.contains('('));
            if body.kind == "interface" && attrs.is_empty() {
                vec![ops.join("\n")]
            } else {
                vec![attrs.join("\n"), ops.join("\n")]
            }
        };
        let n = &mut self.spec.nodes[i];
        n.kind = body.kind.into();
        n.sections = sections;
    }

    /// `note "文字" as ID`(ほかとつなげるメモ)。
    fn floating_note(&mut self, line: &str) -> bool {
        let Some(rest) = line.strip_prefix("note ") else {
            return false;
        };
        let rest = rest.trim();
        let Some((text, id)) = rest.split_once(" as ") else {
            return false;
        };
        if !text.starts_with('"') {
            return false;
        }
        let id = unquote(id);
        self.node(&id, "note", &unquote(text));
        true
    }
}

/// 宣言だけでシーケンス図とみなせるキーワード。
const PARTICIPANTS: &[&str] = &[
    "participant",
    "actor",
    "boundary",
    "control",
    "entity",
    "database",
    "collections",
    "queue",
];

/// シーケンス図だけで使う語。
const SEQUENCE_WORDS: &[&str] = &[
    "participant",
    "boundary",
    "control",
    "collections",
    "activate",
    "deactivate",
    "autonumber",
    "return",
    "alt",
    "loop",
    "opt",
    "par",
    "critical",
    "ref",
    "...",
];

/// シーケンス図か。宣言と矢印だけで、矢印がメッセージの形(`->` `-->`)のもの。
fn is_sequence(lines: &[&str]) -> bool {
    let mut arrows = 0;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        let word = lower.split_whitespace().next().unwrap_or("");
        if SEQUENCE_WORDS.contains(&word) || line.starts_with("==") {
            return true;
        }
        if line.ends_with('{')
            || word.ends_with("direction")
            || (lower.starts_with("left to right") || lower.starts_with("top to bottom"))
        {
            return false;
        }
        if PARTICIPANTS.contains(&word) {
            continue;
        }
        let body = line.split_once(':').map_or(*line, |(a, _)| a);
        if let Some((a, arrow, b)) = split_arrow(body) {
            let operand_ok = |s: &str| !s.starts_with(['[', '(', ':']);
            let message = arrow
                .chars()
                .all(|c| matches!(c, '-' | '<' | '>' | 'x' | 'o' | '/' | '\\'))
                && arrow.contains('-');
            if !(message && operand_ok(&a) && operand_ok(&b)) {
                return false;
            }
            arrows += 1;
        }
    }
    arrows > 0
}

fn parse_sequence(lines: &[&str]) -> Result<SequenceSpec, String> {
    let mut spec = SequenceSpec::default();
    let mut alias: HashMap<String, String> = HashMap::new();
    let mut in_note = false;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if in_note {
            in_note = !(lower.starts_with("end note") || lower == "endnote");
            continue;
        }
        if lower.starts_with("note ") || lower.starts_with("hnote") || lower.starts_with("rnote") {
            in_note = !line.contains(':');
            continue;
        }
        if lower.starts_with("title ") {
            spec.title = line[6..].trim().to_string();
            continue;
        }
        let word = lower.split_whitespace().next().unwrap_or("");
        if PARTICIPANTS.contains(&word) {
            let rest = line[word.len()..].trim();
            let rest = rest.split(" #").next().unwrap_or(rest);
            let rest = rest.split(" <<").next().unwrap_or(rest);
            let rest = rest.split(" order ").next().unwrap_or(rest).trim();
            let (label, id) = match rest.split_once(" as ") {
                Some((a, b)) if a.trim().starts_with('"') => (unquote(a), unquote(b)),
                Some((a, b)) => (unquote(b), unquote(a)),
                None => (unquote(rest), unquote(rest)),
            };
            if label != id {
                alias.insert(label.clone(), id.clone());
            }
            spec.participant(&id, Some(&label));
            continue;
        }
        let (body, text) = match line.split_once(':') {
            Some((a, b)) => (a, b.trim()),
            None => (*line, ""),
        };
        let Some((a, arrow, b)) = split_arrow(body) else {
            continue;
        };
        let id = |s: &str| {
            let s = unquote(s.trim_end_matches(['+', '-', '*', '!']).trim());
            alias.get(&s).cloned().unwrap_or(s)
        };
        let (a, b) = (id(&a), id(&b));
        if a.is_empty() || b.is_empty() {
            continue;
        }
        let reply = arrow.contains("--");
        let reversed = arrow.starts_with('<') && !arrow.ends_with('>');
        let text = text.replace("\\n", "\n");
        if reversed {
            spec.message(&b, &a, &text, reply);
        } else {
            spec.message(&a, &b, &text, reply);
        }
    }
    if spec.participants.is_empty() {
        return Err("PlantUML のシーケンス図に参加者がありません".into());
    }
    Ok(spec)
}

/// PlantUML のノードと矢印の図を図の定義にする。シーケンス図は [`parse_diagram`] で読む。
pub fn parse(text: &str) -> Result<DiagramSpec, String> {
    match parse_diagram(text)? {
        Diagram::Flow(spec) => Ok(spec),
        Diagram::Sequence(_) => Err("シーケンス図はノードと矢印の図として読めません".into()),
    }
}

/// PlantUML を図にする。読めなければ理由を返す。
pub fn parse_diagram(text: &str) -> Result<Diagram, String> {
    let mut b = Builder::default();
    let mut in_note = false;
    let mut lines = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('\'') || line.starts_with("@start") {
            continue;
        }
        if line.starts_with("@end") {
            break;
        }
        lines.push(line);
    }
    // `start` か `:処理;` があればアクティビティ図とみなす。
    let activity = lines
        .iter()
        .any(|l| *l == "start" || (l.starts_with(':') && l.ends_with(';')));
    if !activity && is_sequence(&lines) {
        return parse_sequence(&lines).map(Diagram::Sequence);
    }
    b.spec.direction = if activity {
        Direction::Down
    } else {
        Direction::Right
    };
    let mut body: Option<ClassBody> = None;
    for line in lines {
        if let Some(cb) = body.as_mut() {
            if line == "}" {
                let cb = body.take().unwrap();
                b.close_class(cb);
            } else {
                cb.lines.push(line.to_string());
            }
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if in_note {
            if lower.starts_with("end note") || lower == "endnote" {
                in_note = false;
            }
            continue;
        }
        if b.floating_note(line) {
            continue;
        }
        if lower.starts_with("note ") || lower.starts_with("hnote") || lower.starts_with("rnote") {
            // 1 行の `note left: 文字` 以外は `end note` まで読み飛ばす。
            in_note = !line.contains(':');
            continue;
        }
        if lower.starts_with("title ") {
            b.spec.title = line[6..].trim().to_string();
            continue;
        }
        if lower == "left to right direction" {
            b.spec.direction = Direction::Right;
            continue;
        }
        if lower == "top to bottom direction" {
            b.spec.direction = Direction::Down;
            continue;
        }
        if line == "}" {
            b.groups.pop();
            continue;
        }
        let word = lower.split_whitespace().next().unwrap_or("");
        if matches!(
            word,
            "skinparam"
                | "hide"
                | "show"
                | "scale"
                | "autonumber"
                | "!include"
                | "!theme"
                | "header"
                | "footer"
                | "legend"
                | "caption"
                | "activate"
                | "deactivate"
                | "destroy"
                | "return"
                | "group"
                | "alt"
                | "opt"
                | "loop"
                | "par"
                | "break"
                | "critical"
                | "fork"
                | "end"
                | "endlegend"
                | "allowmixing"
        ) && !(activity && matches!(word, "end"))
        {
            continue;
        }
        if activity && b.activity_line(line) {
            continue;
        }
        if let Some(cb) = b.open_class(line) {
            body = Some(cb);
            continue;
        }
        if b.relation(line) {
            continue;
        }
        b.declaration(line);
    }
    let used: Vec<String> = b
        .spec
        .nodes
        .iter()
        .filter_map(|n| n.group.clone())
        .chain(b.spec.groups.iter().filter_map(|g| g.parent.clone()))
        .collect();
    b.spec.groups.retain(|g| used.contains(&g.id));
    if b.spec.nodes.is_empty() {
        return Err("PlantUML の図にノードがありません".into());
    }
    Ok(Diagram::Flow(b.spec))
}

// ---- 書き出し ----

/// `"..."` で囲んだ名前(改行は `\n`、引用符は `'`)。
fn quote(s: &str) -> String {
    format!("\"{}\"", inline(s))
}

fn inline(s: &str) -> String {
    s.trim()
        .replace("\r\n", "\n")
        .replace('"', "'")
        .replace('\n', "\\n")
}

/// ノードの種類 → 宣言のキーワード。
fn keyword(kind: &str) -> &'static str {
    match kind {
        "class" => "class",
        "interface" => "interface",
        "entity" => "entity",
        "actor" => "actor",
        _ => match catalog::node_kind(kind).style {
            NodeStyle::Decision => "hexagon",
            NodeStyle::Terminal => "usecase",
            NodeStyle::Data => "database",
            NodeStyle::Note => "note",
            NodeStyle::Process | NodeStyle::Service => "rectangle",
        },
    }
}

/// 始点だけに矢じりのある線は、向きを入れ替えて終点の矢じりにする。
fn canonical(e: &EdgeSpec) -> (&str, &str, EdgeLine) {
    let l = e.line.unwrap_or(EdgeLine::ARROW);
    if l.end == Cap::None && matches!(l.start, Cap::Arrow | Cap::Triangle | Cap::TriangleFilled) {
        (
            &e.to,
            &e.from,
            EdgeLine {
                start: Cap::None,
                end: l.start,
                dashed: l.dashed,
            },
        )
    } else {
        (&e.from, &e.to, l)
    }
}

/// 図を PlantUML にする。
pub fn write(d: &Diagram) -> String {
    let mut out = String::from("@startuml\n");
    match d {
        Diagram::Flow(spec) => write_flow(spec, &mut out),
        Diagram::Sequence(spec) => {
            if !spec.title.is_empty() {
                out.push_str(&format!("title {}\n", inline(&spec.title)));
            }
            for p in &spec.participants {
                if p.label == p.id {
                    out.push_str(&format!("participant {}\n", p.id));
                } else {
                    out.push_str(&format!("participant {} as {}\n", quote(&p.label), p.id));
                }
            }
            for m in &spec.messages {
                let arrow = if m.reply { "-->" } else { "->" };
                if m.label.is_empty() {
                    out.push_str(&format!("{} {arrow} {}\n", m.from, m.to));
                } else {
                    out.push_str(&format!(
                        "{} {arrow} {} : {}\n",
                        m.from,
                        m.to,
                        inline(&m.label)
                    ));
                }
            }
        }
    }
    out.push_str("@enduml\n");
    out
}

fn write_flow(spec: &DiagramSpec, out: &mut String) {
    if !spec.title.is_empty() {
        out.push_str(&format!("title {}\n", inline(&spec.title)));
    }
    let uml = spec
        .nodes
        .iter()
        .any(|n| matches!(n.kind.as_str(), "class" | "interface" | "entity"));
    let other = spec
        .nodes
        .iter()
        .any(|n| !matches!(n.kind.as_str(), "class" | "interface" | "entity"));
    if uml && other {
        out.push_str("allowmixing\n");
    }
    out.push_str(match spec.direction {
        Direction::Right => "left to right direction\n",
        Direction::Down => "top to bottom direction\n",
    });
    fn node(n: &NodeSpec, indent: usize, out: &mut String) {
        let pad = "  ".repeat(indent);
        let kw = keyword(&n.kind);
        let text = match kw {
            "class" | "interface" | "entity" | "actor" => n.label.clone(),
            _ => super::node_text(catalog::node_kind(&n.kind), &n.label),
        };
        let decl = if text == n.id && kw != "note" {
            format!("{pad}{kw} {}", n.id)
        } else {
            format!("{pad}{kw} {} as {}", quote(&text), n.id)
        };
        let sections: Vec<Vec<&str>> = n
            .sections
            .iter()
            .map(|s| notation::lines_of(s).collect())
            .collect();
        match kw {
            "class" | "interface" | "entity" if sections.iter().any(|s| !s.is_empty()) => {
                out.push_str(&format!("{decl} {{\n"));
                for (k, sec) in sections.iter().enumerate() {
                    if k > 0 && kw != "entity" {
                        out.push_str(&format!("{pad}  --\n"));
                    }
                    for l in sec {
                        out.push_str(&format!("{pad}  {l}\n"));
                    }
                }
                out.push_str(&format!("{pad}}}\n"));
            }
            _ => out.push_str(&format!("{decl}\n")),
        }
    }
    fn group(spec: &DiagramSpec, g: &GroupSpec, indent: usize, out: &mut String) {
        let pad = "  ".repeat(indent);
        out.push_str(&format!(
            "{pad}rectangle {} as {} {{\n",
            quote(&g.label),
            g.id
        ));
        for c in spec
            .groups
            .iter()
            .filter(|c| c.parent.as_ref() == Some(&g.id))
        {
            group(spec, c, indent + 1, out);
        }
        for n in spec
            .nodes
            .iter()
            .filter(|n| n.group.as_ref() == Some(&g.id))
        {
            node(n, indent + 1, out);
        }
        out.push_str(&format!("{pad}}}\n"));
    }
    for g in spec.groups.iter().filter(|g| g.parent.is_none()) {
        group(spec, g, 0, out);
    }
    for n in spec.nodes.iter().filter(|n| n.group.is_none()) {
        node(n, 0, out);
    }
    for e in &spec.edges {
        let (from, to, l) = canonical(e);
        let body = if l.dashed { ".." } else { "--" };
        let arrow = format!(
            "{}{body}{}",
            notation::left_head(l.start, false),
            notation::right_head(l.end, false)
        );
        if e.label.trim().is_empty() {
            out.push_str(&format!("{from} {arrow} {to}\n"));
        } else {
            out.push_str(&format!("{from} {arrow} {to} : {}\n", inline(&e.label)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(s: &DiagramSpec) -> Vec<(String, String, String)> {
        let label = |id: &str| {
            s.nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.label.clone())
                .unwrap_or_default()
        };
        s.edges
            .iter()
            .map(|e| (label(&e.from), label(&e.to), e.label.clone()))
            .collect()
    }

    fn e(a: &str, b: &str, l: &str) -> (String, String, String) {
        (a.into(), b.into(), l.into())
    }

    #[test]
    fn activity_with_if_else() {
        let s = parse(
            "@startuml
start
:注文を受ける;
if (在庫あり?) then (はい)
  :発送する;
else (いいえ)
  :取り寄せる;
endif
:完了を通知;
stop
@enduml",
        )
        .unwrap();
        assert_eq!(s.direction, Direction::Down);
        assert_eq!(
            edges(&s),
            vec![
                e("開始", "注文を受ける", ""),
                e("注文を受ける", "在庫あり?", ""),
                e("在庫あり?", "発送する", "はい"),
                e("在庫あり?", "取り寄せる", "いいえ"),
                e("発送する", "完了を通知", ""),
                e("取り寄せる", "完了を通知", ""),
                e("完了を通知", "終了", ""),
            ]
        );
        assert_eq!(s.nodes[2].kind, "decision");
    }

    #[test]
    fn activity_while_loop() {
        let s = parse(
            "@startuml
start
while (データあり?) is (はい)
  :読む;
endwhile (いいえ)
stop
@enduml",
        )
        .unwrap();
        assert!(edges(&s).contains(&e("読む", "データあり?", "")));
        assert!(edges(&s).contains(&e("データあり?", "終了", "いいえ")));
    }

    #[test]
    fn components_and_packages() {
        let s = parse(
            "@startuml
actor 利用者 as U
package \"Web\" {
  [フロント] as F
  component API
}
database \"顧客DB\" as DB
U -> F : 操作
F --> API
API ..> DB : SQL
DB <-- [バッチ]
API -left-> [ログ]
@enduml",
        )
        .unwrap();
        assert_eq!(s.direction, Direction::Right);
        assert_eq!(
            edges(&s),
            vec![
                e("利用者", "フロント", "操作"),
                e("フロント", "API", ""),
                e("API", "顧客DB", "SQL"),
                e("バッチ", "顧客DB", ""),
                e("API", "ログ", ""),
            ]
        );
        let api = s.nodes.iter().find(|n| n.id == "API").unwrap();
        assert_eq!(api.group.as_deref(), Some("Web"));
        assert_eq!(s.groups.len(), 1);
        let db = s.nodes.iter().find(|n| n.id == "DB").unwrap();
        assert_eq!(db.kind, "data");
    }

    #[test]
    fn sequence_messages_become_a_sequence_diagram() {
        let Diagram::Sequence(s) = parse_diagram(
            "@startuml\nparticipant \"利用者\" as U\nU -> Bob: こんにちは\nBob --> U: やあ\n@enduml",
        )
        .unwrap() else {
            panic!("シーケンス図ではない");
        };
        assert_eq!(s.participants.len(), 2);
        assert_eq!(s.participants[0].label, "利用者");
        assert_eq!(s.messages.len(), 2);
        assert!(!s.messages[0].reply && s.messages[1].reply);
        assert_eq!(s.messages[1].from, "Bob");
        assert!(looks_like_plantuml("\n@startuml\nA->B\n@enduml"));
        assert!(parse("@startuml\n@enduml").is_err());
    }

    #[test]
    fn class_bodies_and_relations() {
        let s = parse(
            "@startuml
class \"顧客\" as C {
  - id: int
  --
  + order(): 注文
}
interface Payment {
  + pay(): bool
}
class Card
entity users {
  PK  id : int
  name : text
}
Payment <|.. Card
C \"1\" *-- \"many\" Order : 持つ
users ||--o{ orders
@enduml",
        )
        .unwrap();
        let node = |id: &str| s.nodes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(node("C").kind, "class");
        assert_eq!(node("C").label, "顧客");
        assert_eq!(node("C").sections, ["- id: int", "+ order(): 注文"]);
        assert_eq!(node("Payment").kind, "interface");
        assert_eq!(node("Payment").sections, ["+ pay(): bool"]);
        assert_eq!(node("users").kind, "entity");
        assert_eq!(node("users").sections, ["PK  id : int\n    name : text"]);
        let realize = &s.edges[0];
        assert_eq!(
            (realize.from.as_str(), realize.to.as_str()),
            ("Card", "Payment")
        );
        assert_eq!(
            realize.line,
            Some(EdgeLine {
                start: Cap::None,
                end: Cap::Triangle,
                dashed: true
            })
        );
        assert_eq!(s.edges[1].line.unwrap().start, Cap::DiamondFilled);
        assert_eq!(s.edges[1].to, "Order");
        assert_eq!(s.edges[1].label, "持つ");
        let er = s.edges[2].line.unwrap();
        assert_eq!((er.start, er.end), (Cap::One, Cap::ZeroOrMany));
    }
}
