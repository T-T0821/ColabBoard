//! 画面の言語(日本語 / 英語)の切り替え。
//!
//! UI の文字はコードと `index.html` に日本語で書いてあり、英語のときは
//! [`en.tsv`](en.tsv) の辞書で画面の文字を置き換える。各画面のコードを変えずに済むよう、
//! 起動時に DOM 全体を訳し、その後に増えた・変わった文字は `MutationObserver` で訳す。
//!
//! - 訳すのはテキストノードと `title`・`aria-label`・`placeholder`・`alt`・`data-tip` 属性。
//! - `translate="no"` の要素の中(ボード名・利用者の名前・スタンプの見本など)は訳さない。
//! - 辞書にない文字は日本語のまま出す(新しく足した文字は辞書に足せば英語になる)。
//! - 言語は localStorage に覚え、なければブラウザの言語(`ja` で始まれば日本語)で決める。
//!   切り替えるとページを読み直す。
//!
//! Rust のコードから直接出す文字(確認ダイアログなど)は [`tr`] を通す。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// 画面の言語。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ja,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ja => "ja",
            Lang::En => "en",
        }
    }

    pub fn from_code(s: &str) -> Option<Lang> {
        let s = s.trim().to_ascii_lowercase();
        if s.starts_with("ja") {
            Some(Lang::Ja)
        } else if s.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }
}

/// 覚えておく localStorage のキー。
pub const STORAGE_KEY: &str = "colabboard.lang";

const EN_TSV: &str = include_str!("en.tsv");

thread_local! {
    static LANG: Cell<Lang> = const { Cell::new(Lang::Ja) };
    static DICT: RefCell<Option<Dict>> = const { RefCell::new(None) };
}

/// 今の言語。
pub fn lang() -> Lang {
    LANG.with(|l| l.get())
}

/// 日本語の文字を今の言語にする(辞書になければそのまま)。
pub fn tr(ja: &str) -> String {
    if lang() == Lang::Ja {
        return ja.to_string();
    }
    translate(ja).unwrap_or_else(|| ja.to_string())
}

/// 英語の辞書で訳す。訳せなければ `None`。前後の空白は残す。
pub fn translate(text: &str) -> Option<String> {
    DICT.with(|d| {
        let mut d = d.borrow_mut();
        let dict = d.get_or_insert_with(|| Dict::parse(EN_TSV));
        dict.translate(text)
    })
}

/// 日本語→英語の辞書。
pub struct Dict {
    map: HashMap<String, String>,
}

impl Dict {
    /// `日本語<TAB>英語` の行を読む。`#` で始まる行と空行は飛ばす。
    pub fn parse(tsv: &str) -> Dict {
        let mut map = HashMap::new();
        for line in tsv.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if let Some((ja, en)) = line.split_once('\t') {
                map.insert(collapse(ja), en.trim().to_string());
            }
        }
        Dict { map }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 前後の空白を残して訳す。
    pub fn translate(&self, text: &str) -> Option<String> {
        let core = text.trim();
        if core.is_empty() || !has_japanese(core) {
            return None;
        }
        let en = self.lookup(core, 0)?;
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        Some(format!("{lead}{en}{trail}"))
    }

    fn lookup(&self, core: &str, depth: u8) -> Option<String> {
        let core = collapse(core);
        if let Some(en) = self.map.get(&core) {
            return Some(en.clone());
        }
        // 数字を {n} にして探し、訳に数字を戻す。
        let (pattern, numbers) = numbers_to_placeholders(&core);
        if !numbers.is_empty()
            && let Some(en) = self.map.get(&pattern)
        {
            return Some(fill_placeholders(en, &numbers));
        }
        if depth >= 2 {
            return None;
        }
        // 「本体(補足)」は別々に訳す。
        if let Some(stripped) = core.strip_suffix(')')
            && let Some(open) = stripped.rfind('(')
            && open > 0
        {
            let head = &stripped[..open];
            let inner = &stripped[open + 1..];
            let h = self.lookup(head, depth + 1);
            let i = self.lookup(inner, depth + 1);
            if h.is_some() || i.is_some() {
                let h = h.unwrap_or_else(|| head.to_string());
                let i = i.unwrap_or_else(|| inner.to_string());
                return Some(format!("{h} ({i})"));
            }
        }
        // 「リアクション 👍」のように、日本語のあとに記号などが続くものは前だけ訳す。
        if let Some((head, tail)) = core.rsplit_once(' ')
            && !has_japanese(tail)
            && let Some(h) = self.lookup(head, depth + 1)
        {
            return Some(format!("{h} {tail}"));
        }
        // 「見出し: 中身」も別々に訳す。
        if let Some((head, rest)) = core.split_once(": ") {
            let h = self.lookup(head, depth + 1);
            let r = self.lookup(rest, depth + 1);
            if h.is_some() || r.is_some() {
                let h = h.unwrap_or_else(|| head.to_string());
                let r = r.unwrap_or_else(|| rest.to_string());
                return Some(format!("{h}: {r}"));
            }
        }
        None
    }
}

/// 連続する空白を 1 つにして前後を落とす。
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// かな・漢字を含むか。
fn has_japanese(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c as u32, 0x3000..=0x30ff | 0x4e00..=0x9fff | 0xff00..=0xffef))
}

/// 数字の並びを `{n}` に置き換え、元の数字を順に返す。
fn numbers_to_placeholders(s: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(s.len());
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            if !cur.is_empty() {
                nums.push(std::mem::take(&mut cur));
                out.push_str("{n}");
            }
            out.push(c);
        }
    }
    if !cur.is_empty() {
        nums.push(cur);
        out.push_str("{n}");
    }
    (out, nums)
}

/// 訳の `{n}` に数字を順に入れる。語順が変わるときは `{1}`・`{2}` で何番目の数字かを書く。
fn fill_placeholders(en: &str, nums: &[String]) -> String {
    let mut out = String::with_capacity(en.len());
    let mut it = nums.iter();
    let mut rest = en;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let key = &after[..close];
        if key == "n" {
            out.push_str(it.next().map_or("", |s| s.as_str()));
        } else if let Ok(k) = key.parse::<usize>() {
            out.push_str(nums.get(k.wrapping_sub(1)).map_or("", |s| s.as_str()));
        } else {
            out.push('{');
            out.push_str(key);
            out.push('}');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// ブラウザの言語設定から決める(`ja…` なら日本語、それ以外は英語)。
pub fn detect(stored: Option<&str>, navigator: Option<&str>) -> Lang {
    stored
        .and_then(Lang::from_code)
        .or_else(|| navigator.map(|n| Lang::from_code(n).unwrap_or(Lang::En)))
        .unwrap_or(Lang::Ja)
}

#[cfg(target_arch = "wasm32")]
mod dom;
#[cfg(target_arch = "wasm32")]
pub use dom::init;

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Dict {
        Dict::parse(EN_TSV)
    }

    #[test]
    fn dictionary_is_well_formed() {
        let d = dict();
        assert!(d.len() > 600, "{}", d.len());
        for line in EN_TSV.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let (ja, en) = line.split_once('\t').expect(line);
            assert!(!en.trim().is_empty(), "訳が空: {ja}");
            assert!(!has_japanese(en), "訳に日本語が残っている: {line}");
            let n = ja.matches("{n}").count();
            assert!(en.matches("{n}").count() <= n, "{{n}} が多すぎる: {line}");
            for k in 1..=9 {
                if en.contains(&format!("{{{k}}}")) {
                    assert!(k <= n, "{{{k}}} に入る数字がない: {line}");
                }
            }
        }
    }

    #[test]
    fn exact_and_whitespace() {
        let d = dict();
        assert_eq!(d.translate("リンクをコピー").unwrap(), "Copy link");
        assert_eq!(d.translate("  コメント \n").unwrap(), "  Comments \n");
        assert_eq!(d.translate("Copy link"), None);
        assert_eq!(d.translate("知らない言葉"), None);
    }

    #[test]
    fn numbers() {
        let d = dict();
        assert_eq!(
            d.translate("オンライン · 3人").unwrap(),
            "Online · 3 people"
        );
        assert_eq!(d.translate("5 分前").unwrap(), "5 min ago");
        assert_eq!(d.translate("1万").unwrap(), "10K");
        assert_eq!(d.translate("100万").unwrap(), "1M");
        assert_eq!(
            d.translate(
                "付箋をクリックで投票(Shift+クリックで取り消し)。残り 2 / 3 票 · 4人が投票中"
            )
            .unwrap(),
            "Click sticky notes to vote (Shift+click to undo). 2 / 3 votes left · 4 voting"
        );
    }

    #[test]
    fn split_parenthetical_and_colon() {
        let d = dict();
        assert_eq!(
            d.translate("各自で付箋に書く(5 分)").unwrap(),
            "Everyone writes sticky notes (5 min)"
        );
        assert_eq!(
            d.translate("話したいものに投票(1 人 3 票)").unwrap(),
            "Vote on what to discuss (3 votes per person)"
        );
        assert_eq!(d.translate("進め方: KPT").unwrap(), "Guide: KPT");
        assert_eq!(d.translate("ルーム: abc").unwrap(), "Room: abc");
        assert_eq!(d.translate("リアクション 👍").unwrap(), "Reaction 👍");
        assert_eq!(
            d.translate("AWS: サーバーレス API").unwrap(),
            "AWS: Serverless API"
        );
        assert_eq!(
            d.translate("8 分で 8 案をスケッチする").unwrap(),
            "Sketch 8 ideas in 8 minutes"
        );
    }

    #[test]
    fn detect_language() {
        assert_eq!(detect(Some("en"), Some("ja-JP")), Lang::En);
        assert_eq!(detect(None, Some("ja-JP")), Lang::Ja);
        assert_eq!(detect(None, Some("en-US")), Lang::En);
        assert_eq!(detect(None, Some("fr")), Lang::En);
        assert_eq!(detect(Some("xx"), Some("ja")), Lang::Ja);
        assert_eq!(detect(None, None), Lang::Ja);
    }
}
