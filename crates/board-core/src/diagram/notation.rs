//! Mermaid と PlantUML で共通の書き方(関係の矢印の記号・ER のカーディナリティ・列の書き方)。

use super::EdgeLine;
use crate::connector::Cap;

/// 線の本体の文字か。
fn is_line_char(c: char) -> bool {
    matches!(c, '-' | '.' | '=')
}

/// 端の記号に使う文字か。
fn is_head_char(c: char) -> bool {
    matches!(
        c,
        '<' | '>' | '|' | '*' | 'o' | '{' | '}' | '+' | '#' | '^' | '0' | 'x'
    )
}

/// 矢印らしい語か(線の本体の前後が端の記号だけ)。本体には `-left->` の向きや
/// `-[#red]->` の色も入りうる。
fn is_arrow_token(t: &str) -> bool {
    let (l, body, r) = arrow_parts(t);
    t.chars().count() >= 2
        && l.chars().all(is_head_char)
        && r.chars().all(is_head_char)
        && body.chars().any(is_line_char)
        && body
            .chars()
            .all(|c| is_line_char(c) || c.is_ascii_alphabetic() || matches!(c, '[' | ']' | '#'))
}

/// `A <|-- B` を「A」「<|--」「B」に分ける。多重度の `"1"` は落とす。
/// 矢印は空白で区切られた語。空白がなければ `--` `..` の前後の記号を矢印とみなす。
pub fn split_relation(body: &str) -> Option<(String, String, String)> {
    // 引用符の中の空白で分けないよう、引用符ごとに語にする。
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in body.chars() {
        if c == '"' {
            quoted = !quoted;
            cur.push(c);
        } else if c.is_whitespace() && !quoted {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    if let Some(i) = tokens.iter().position(|t| is_arrow_token(t))
        && i > 0
        && i + 1 < tokens.len()
    {
        let is_mult = |t: &String| t.starts_with('"') && t.ends_with('"') && t.len() >= 2;
        // 引用符だけの語は多重度(ただし名前がそれしかなければ名前)。
        fn side(v: &[String], is_mult: impl Fn(&String) -> bool) -> Vec<&String> {
            let named: Vec<&String> = v.iter().filter(|t| !is_mult(t)).collect();
            if named.is_empty() {
                v.iter().collect()
            } else {
                named
            }
        }
        let left = side(&tokens[..i], is_mult);
        let right = side(&tokens[i + 1..], is_mult);
        if !left.is_empty() && !right.is_empty() {
            let join = |v: Vec<&String>| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
            return Some((join(left), tokens[i].clone(), join(right)));
        }
    }
    // 空白のない `A-->B`。
    let start = body.find("--").or_else(|| body.find(".."))?;
    let mut a = start;
    while a > 0 {
        let c = body[..a].chars().last()?;
        if matches!(c, '<' | '|' | '*' | '}' | '{') {
            a -= c.len_utf8();
        } else {
            break;
        }
    }
    let mut b = start;
    for c in body[start..].chars() {
        if is_line_char(c) || matches!(c, '>' | '|' | '*' | '{' | '}') {
            b += c.len_utf8();
        } else {
            break;
        }
    }
    let (l, r) = (body[..a].trim(), body[b..].trim());
    (!l.is_empty() && !r.is_empty()).then(|| (l.to_string(), body[a..b].to_string(), r.to_string()))
}

/// 矢印の語を「左の記号」「本体」「右の記号」に分ける。
fn arrow_parts(arrow: &str) -> (&str, &str, &str) {
    let first = arrow.find(is_line_char).unwrap_or(0);
    let last = arrow
        .rfind(is_line_char)
        .map(|i| i + 1)
        .unwrap_or(arrow.len());
    (&arrow[..first], &arrow[first..last], &arrow[last..])
}

/// 左端の記号(右端は左右を入れ替えて渡す)を端の飾りにする。
fn head_cap(head: &str) -> Cap {
    match head {
        "" => Cap::None,
        "<" | "<<" | "^" | "+" | "#" => Cap::Arrow,
        "<|" => Cap::Triangle,
        "o" => Cap::Diamond,
        "*" => Cap::DiamondFilled,
        "0" | "()" => Cap::Circle,
        "||" | "|" => Cap::One,
        "|o" => Cap::ZeroOrOne,
        "}o" => Cap::ZeroOrMany,
        "}|" => Cap::OneOrMany,
        "}" => Cap::Many,
        "x" => Cap::None,
        _ => Cap::Arrow,
    }
}

/// 右端の記号を左端の向きに直す(`|>` → `<|`、`o{` → `}o`)。
fn mirror(head: &str) -> String {
    head.chars()
        .rev()
        .map(|c| match c {
            '>' => '<',
            '<' => '>',
            '{' => '}',
            '}' => '{',
            c => c,
        })
        .collect()
}

/// 関係の矢印を線の見た目にする。返り値の `bool` は向きを逆にするか
/// (`A <|-- B` は「B から A へ」の継承として読む)。
pub fn relation_line(arrow: &str) -> (EdgeLine, bool) {
    let (l, body, r) = arrow_parts(arrow);
    let start = head_cap(l);
    let end = head_cap(&mirror(r));
    let dashed = body.contains('.');
    if r.is_empty() && matches!(start, Cap::Arrow | Cap::Triangle) {
        return (
            EdgeLine {
                start: Cap::None,
                end: start,
                dashed,
            },
            true,
        );
    }
    (EdgeLine { start, end, dashed }, false)
}

/// 端の飾りを関係の記号にする(左端の向き。右端は [`right_head`])。
/// `er` なら ER 図のカーディナリティの記号(飾りがなければ `||`)。
pub fn left_head(cap: Cap, er: bool) -> &'static str {
    match cap {
        Cap::One => "||",
        Cap::ZeroOrOne => "|o",
        Cap::ZeroOrMany => "}o",
        Cap::OneOrMany | Cap::Many => "}|",
        _ if er => "||",
        Cap::None => "",
        Cap::Triangle => "<|",
        Cap::Diamond => "o",
        Cap::DiamondFilled => "*",
        Cap::Arrow | Cap::TriangleFilled | Cap::Circle => "<",
    }
}

pub fn right_head(cap: Cap, er: bool) -> &'static str {
    match cap {
        Cap::One => "||",
        Cap::ZeroOrOne => "o|",
        Cap::ZeroOrMany => "o{",
        Cap::OneOrMany | Cap::Many => "|{",
        _ if er => "||",
        Cap::None => "",
        Cap::Triangle => "|>",
        Cap::Diamond => "o",
        Cap::DiamondFilled => "*",
        Cap::Arrow | Cap::TriangleFilled | Cap::Circle => ">",
    }
}

/// ER 図のカーディナリティの飾りか。
pub fn is_er_cap(cap: Cap) -> bool {
    matches!(
        cap,
        Cap::One | Cap::ZeroOrOne | Cap::ZeroOrMany | Cap::OneOrMany | Cap::Many
    )
}

/// エンティティの列 1 行(`PK  id : int`)を「キー」「名前」「型」に分ける。
pub fn parse_column(line: &str) -> (String, String, String) {
    let t = line.trim();
    let (key, rest) = match t.split_once(char::is_whitespace) {
        Some((k, rest)) if matches!(k, "PK" | "FK" | "UK") => (k.to_string(), rest.trim()),
        _ => (String::new(), t),
    };
    match rest.split_once(':') {
        Some((name, ty)) => (key, name.trim().to_string(), ty.trim().to_string()),
        None => (key, rest.to_string(), String::new()),
    }
}

/// エンティティの列 1 行を作る(キーの幅をそろえる)。
pub fn format_column(key: &str, name: &str, ty: &str) -> String {
    let body = if ty.is_empty() {
        name.to_string()
    } else {
        format!("{name} : {ty}")
    };
    format!("{key:<4}{body}")
}

/// 行を並べた文字を、空行を除いた行の並びにする。
pub fn lines_of(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relations_split_and_read() {
        let (a, arrow, b) = split_relation("Animal <|-- Duck").unwrap();
        assert_eq!(
            (a.as_str(), arrow.as_str(), b.as_str()),
            ("Animal", "<|--", "Duck")
        );
        let (line, rev) = relation_line(&arrow);
        assert!(rev);
        assert_eq!(line.end, Cap::Triangle);
        let (a, arrow, b) = split_relation("A \"1\" *-- \"many\" B").unwrap();
        assert_eq!((a.as_str(), arrow.as_str(), b.as_str()), ("A", "*--", "B"));
        assert_eq!(relation_line(&arrow).0.start, Cap::DiamondFilled);
        let (_, arrow, _) = split_relation("users ||--o{ orders").unwrap();
        let (line, rev) = relation_line(&arrow);
        assert!(!rev);
        assert_eq!((line.start, line.end), (Cap::One, Cap::ZeroOrMany));
        let (_, arrow, _) = split_relation("A..|>B").unwrap();
        let (line, _) = relation_line(&arrow);
        assert!(line.dashed);
        assert_eq!(line.end, Cap::Triangle);
        assert!(split_relation("ただの文").is_none());
    }

    #[test]
    fn heads_round_trip() {
        for cap in [
            Cap::None,
            Cap::Arrow,
            Cap::Triangle,
            Cap::Diamond,
            Cap::DiamondFilled,
            Cap::One,
            Cap::ZeroOrOne,
            Cap::ZeroOrMany,
            Cap::OneOrMany,
        ] {
            let arrow = format!("{}--{}", left_head(cap, false), right_head(cap, false));
            let (line, rev) = relation_line(&arrow);
            if rev {
                assert_eq!(line.end, cap, "{arrow}");
            } else {
                assert_eq!((line.start, line.end), (cap, cap), "{arrow}");
            }
        }
    }

    #[test]
    fn columns() {
        assert_eq!(
            parse_column("FK  user_id : int"),
            ("FK".into(), "user_id".into(), "int".into())
        );
        assert_eq!(
            parse_column("     name : text"),
            (String::new(), "name".into(), "text".into())
        );
        assert_eq!(format_column("PK", "id", "int"), "PK  id : int");
        assert_eq!(format_column("", "name", "text"), "    name : text");
    }
}
