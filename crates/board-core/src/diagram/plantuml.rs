//! PlantUML の取り込み。
//!
//! 文を読んで [`DiagramSpec`] にし、レイアウトは図の自動配置に任せる。
//!
//! - アクティビティ図(新しい書き方): `start` `stop` `end` `:処理;`
//!   `if (条件) then (はい)` 〜 `elseif` 〜 `else (いいえ)` 〜 `endif`、
//!   `while (条件)` 〜 `endwhile`、`repeat` 〜 `repeat while (条件)`、`note`(読み飛ばす)
//! - そのほかの図(コンポーネント・ユースケース・配置・シーケンスなど):
//!   `A -> B : ラベル` の矢印(`-->` `..>` `-left->` `<-` なども)、
//!   `actor` `database` `component` `node` `rectangle` `usecase` などの宣言(`"名前" as ID`)、
//!   `[部品]` `(ユースケース)` `:アクター:` の書き方、`package` `node` `rectangle` などの `{ }` → 枠
//!
//! `skinparam` `title` 以外の見た目の指定は読み飛ばす。

use std::collections::HashMap;

use super::{DiagramSpec, Direction, EdgeSpec, GroupSpec, NodeSpec};

/// PlantUML らしい文字列か。
pub fn looks_like_plantuml(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.starts_with("@start"))
}

/// 宣言のキーワードと、ノードの種類。
const DECLS: &[(&str, &str)] = &[
    ("actor", "terminal"),
    ("participant", "process"),
    ("boundary", "process"),
    ("control", "process"),
    ("entity", "data"),
    ("database", "data"),
    ("collections", "data"),
    ("queue", "data"),
    ("storage", "data"),
    ("file", "data"),
    ("artifact", "data"),
    ("component", "process"),
    ("interface", "process"),
    ("usecase", "terminal"),
    ("agent", "process"),
    ("card", "process"),
    ("class", "process"),
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
            });
        }
        id
    }

    fn edge(&mut self, from: &str, to: &str, label: &str) {
        self.spec.edges.push(EdgeSpec {
            from: from.into(),
            to: to.into(),
            label: label.trim().into(),
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
        // 矢印は `-` `.` `=` と `<` `>` `*` `o` `|` などの並び(`-left->` のような向き指定も)。
        let Some((start, end)) = find_arrow(body) else {
            return false;
        };
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
        let (a, b) = (strip_mult(a, true), strip_mult(b, false));
        let (Some(x), Some(y)) = (self.operand(&a), self.operand(&b)) else {
            return false;
        };
        let reversed = arrow.starts_with('<') && !arrow.ends_with('>');
        let label = label.replace("\\n", "\n");
        if reversed {
            self.edge(&y, &x, &label);
        } else {
            self.edge(&x, &y, &label);
        }
        true
    }
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

/// PlantUML を図の定義にする。読めなければ理由を返す。
pub fn parse(text: &str) -> Result<DiagramSpec, String> {
    let mut b = Builder::default();
    let mut in_note = false;
    let mut activity = false;
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
    if lines
        .iter()
        .any(|l| *l == "start" || (l.starts_with(':') && l.ends_with(';')))
    {
        activity = true;
    }
    b.spec.direction = if activity {
        Direction::Down
    } else {
        Direction::Right
    };
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if in_note {
            if lower.starts_with("end note") || lower == "endnote" {
                in_note = false;
            }
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
        ) && !(activity && matches!(word, "end"))
        {
            continue;
        }
        if activity && b.activity_line(line) {
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
    Ok(b.spec)
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
    fn sequence_messages_become_edges() {
        let s = parse("@startuml\nAlice -> Bob: こんにちは\nBob --> Alice: やあ\n@enduml").unwrap();
        assert_eq!(
            edges(&s),
            vec![e("Alice", "Bob", "こんにちは"), e("Bob", "Alice", "やあ")]
        );
        assert!(looks_like_plantuml("\n@startuml\nA->B\n@enduml"));
        assert!(parse("@startuml\n@enduml").is_err());
    }
}
