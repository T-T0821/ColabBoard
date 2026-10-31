//! スタンプ(ボードに押すバッジやハンコ)。
//!
//! スタンプは `ShapeKind::Stamp` の図形で、文字を図形の文字として保存するので、
//! 同期や保存は他の図形と同じ。見た目は 2 通り:
//!
//! - バッジ: 色付きの丸に白い記号(`+1`・`♥`・`OK` など)。
//! - ハンコ: 塗りのない朱色の丸枠に朱色の文字(`承認`・名前・日付印)。
//!   塗りが透明なスタンプをハンコとして描く(`Shape::is_hanko`)。

use std::borrow::Cow;

use glam::Vec2;

use crate::color::Rgba8;
use crate::element::ElementId;
use crate::shape::{Shape, ShapeKind};

/// スタンプの大きさ(ワールド単位、直径)。
pub const STAMP_SIZE: f32 = 56.0;
/// ハンコの大きさ。
pub const HANKO_SIZE: f32 = 72.0;
/// 日付印の大きさ(3 段に文字を入れるので大きめ)。
pub const DATE_HANKO_SIZE: f32 = 96.0;
/// ドット投票の点の大きさ。
pub const DOT_SIZE: f32 = 28.0;
/// 朱肉の色。
pub const SHUNIKU: Rgba8 = Rgba8::hex(0xd23a2f);

/// スタンプパレットの分類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampGroup {
    /// 反応(いいね・+1・ハート…)。
    Reaction,
    /// 評価・状態(OK・NG・済・要確認…)。
    Status,
    /// 記号(◎○△×・? ・!)。
    Mark,
    /// 番号。
    Number,
    /// ドット投票。
    Dot,
    /// ハンコ(承認・確認…)。
    Hanko,
}

impl StampGroup {
    pub const ALL: [StampGroup; 6] = [
        StampGroup::Reaction,
        StampGroup::Status,
        StampGroup::Mark,
        StampGroup::Number,
        StampGroup::Dot,
        StampGroup::Hanko,
    ];

    /// 識別名(UI の文言を引くのに使う)。
    pub fn key(self) -> &'static str {
        match self {
            StampGroup::Reaction => "reaction",
            StampGroup::Status => "status",
            StampGroup::Mark => "mark",
            StampGroup::Number => "number",
            StampGroup::Dot => "dot",
            StampGroup::Hanko => "hanko",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// 識別名(UI 用)。
    pub name: Cow<'static, str>,
    /// 載せる文字。
    pub glyph: Cow<'static, str>,
    pub color: Rgba8,
    /// 説明(日本語)。
    pub title: &'static str,
    /// 説明(英語)。
    pub title_en: &'static str,
    pub group: StampGroup,
}

const fn badge(
    name: &'static str,
    glyph: &'static str,
    color: u32,
    title: &'static str,
    title_en: &'static str,
    group: StampGroup,
) -> Stamp {
    Stamp {
        name: Cow::Borrowed(name),
        glyph: Cow::Borrowed(glyph),
        color: Rgba8::hex(color),
        title,
        title_en,
        group,
    }
}

const fn hanko(
    name: &'static str,
    glyph: &'static str,
    title: &'static str,
    title_en: &'static str,
) -> Stamp {
    Stamp {
        name: Cow::Borrowed(name),
        glyph: Cow::Borrowed(glyph),
        color: SHUNIKU,
        title,
        title_en,
        group: StampGroup::Hanko,
    }
}

const BLUE: u32 = 0x2f6fed;
const RED: u32 = 0xe5484d;
const AMBER: u32 = 0xf5a524;
const GREEN: u32 = 0x30a46c;
const PURPLE: u32 = 0x8e4ec6;
const ORANGE: u32 = 0xf76b15;
const GRAY: u32 = 0x6b7280;
const TEAL: u32 = 0x12a594;
const PINK: u32 = 0xd6409f;
const NAVY: u32 = 0x334155;

use StampGroup as G;

/// 選べるスタンプ。先頭 6 つは以前からある名前なので変えない。
pub const STAMPS: &[Stamp] = &[
    badge("plus1", "+1", BLUE, "賛成", "Agree", G::Reaction),
    badge("heart", "♥", RED, "いいね", "Love", G::Reaction),
    badge("star", "★", AMBER, "注目", "Star", G::Reaction),
    badge("check", "✓", GREEN, "完了", "Done", G::Status),
    badge("question", "?", PURPLE, "質問", "Question", G::Mark),
    badge("exclaim", "!", ORANGE, "重要", "Important", G::Mark),
    // 反応
    badge("like", "いいね", PINK, "いいね!", "Like", G::Reaction),
    badge("minus1", "-1", GRAY, "反対", "Disagree", G::Reaction),
    badge("idea", "案", AMBER, "アイデア", "Idea", G::Reaction),
    badge("thanks", "感謝", TEAL, "ありがとう", "Thanks", G::Reaction),
    badge("wow", "神", PURPLE, "すごい", "Awesome", G::Reaction),
    badge("heart-outline", "♡", PINK, "好き", "Like it", G::Reaction),
    // 評価・状態
    badge("ok", "OK", GREEN, "OK", "OK", G::Status),
    badge("ng", "NG", RED, "NG", "NG", G::Status),
    badge("done", "済", GREEN, "済み", "Finished", G::Status),
    badge(
        "review",
        "要確認",
        ORANGE,
        "要確認",
        "Needs review",
        G::Status,
    ),
    badge("important", "重要", RED, "重要", "Important", G::Status),
    badge("urgent", "急", RED, "急ぎ", "Urgent", G::Status),
    badge("wip", "対応中", BLUE, "対応中", "In progress", G::Status),
    badge("hold", "保留", GRAY, "保留", "On hold", G::Status),
    badge("new", "NEW", PINK, "新着", "New", G::Status),
    badge("todo", "TODO", NAVY, "やること", "To do", G::Status),
    // 記号
    badge(
        "double-circle",
        "◎",
        RED,
        "とても良い",
        "Excellent",
        G::Mark,
    ),
    badge("circle", "○", ORANGE, "良い", "Good", G::Mark),
    badge("triangle", "△", AMBER, "まあまあ", "Fair", G::Mark),
    badge("cross", "×", NAVY, "だめ", "Poor", G::Mark),
    badge("arrow", "→", BLUE, "次へ", "Next", G::Mark),
    badge("up", "↑", GREEN, "上げる", "Up", G::Mark),
    // 番号
    badge("n1", "1", BLUE, "1", "1", G::Number),
    badge("n2", "2", BLUE, "2", "2", G::Number),
    badge("n3", "3", BLUE, "3", "3", G::Number),
    badge("n4", "4", BLUE, "4", "4", G::Number),
    badge("n5", "5", BLUE, "5", "5", G::Number),
    badge("n6", "6", BLUE, "6", "6", G::Number),
    badge("n7", "7", BLUE, "7", "7", G::Number),
    badge("n8", "8", BLUE, "8", "8", G::Number),
    badge("n9", "9", BLUE, "9", "9", G::Number),
    badge("n10", "10", BLUE, "10", "10", G::Number),
    badge("rank1", "1位", AMBER, "1位", "1st", G::Number),
    badge("rank2", "2位", GRAY, "2位", "2nd", G::Number),
    badge("rank3", "3位", ORANGE, "3位", "3rd", G::Number),
    // ドット投票(記号なしの小さな点)
    badge("dot-red", "", RED, "投票(赤)", "Vote (red)", G::Dot),
    badge("dot-blue", "", BLUE, "投票(青)", "Vote (blue)", G::Dot),
    badge("dot-green", "", GREEN, "投票(緑)", "Vote (green)", G::Dot),
    badge("dot-amber", "", AMBER, "投票(黄)", "Vote (yellow)", G::Dot),
    badge(
        "dot-purple",
        "",
        PURPLE,
        "投票(紫)",
        "Vote (purple)",
        G::Dot,
    ),
    badge("dot-pink", "", PINK, "投票(桃)", "Vote (pink)", G::Dot),
    // ハンコ
    hanko("hanko-approved", "承認", "承認印", "Approved"),
    hanko("hanko-checked", "確認", "確認印", "Checked"),
    hanko("hanko-done", "済", "済印", "Done"),
    hanko("hanko-seen", "検印", "検印", "Inspected"),
    hanko("hanko-ok", "了", "了解", "Acknowledged"),
    hanko("hanko-urgent", "至急", "至急", "Urgent"),
    hanko("hanko-secret", "秘", "秘", "Confidential"),
    hanko("hanko-important", "重要", "重要", "Important"),
    hanko("hanko-rejected", "却下", "却下", "Rejected"),
    hanko("hanko-pending", "保留", "保留", "Pending"),
    hanko("hanko-circulate", "回覧", "回覧", "Circulate"),
    hanko(
        "hanko-good",
        "大変\nよく\nできました",
        "大変よくできました",
        "Excellent work",
    ),
];

impl Stamp {
    pub fn by_name(name: &str) -> Option<Stamp> {
        STAMPS.iter().find(|s| s.name == name).cloned()
    }

    /// 名前入りのハンコ(認印)。漢字・かなの名前は縦書きにする。
    pub fn name_hanko(name: &str) -> Stamp {
        Stamp {
            name: Cow::Borrowed("hanko-name"),
            glyph: Cow::Owned(seal_layout(name)),
            color: SHUNIKU,
            title: "名前の印鑑",
            title_en: "Name seal",
            group: StampGroup::Hanko,
        }
    }

    /// 日付印(上段に `label`、中段に日付、下段に名前)。
    pub fn date_hanko(label: &str, date: &str, name: &str) -> Stamp {
        let name: String = name.trim().chars().take(6).collect();
        Stamp {
            name: Cow::Borrowed("hanko-date"),
            glyph: Cow::Owned(format!("{}\n{}\n{}", label.trim(), date.trim(), name)),
            color: SHUNIKU,
            title: "日付印",
            title_en: "Date seal",
            group: StampGroup::Hanko,
        }
    }

    pub fn is_hanko(&self) -> bool {
        self.group == StampGroup::Hanko
    }

    /// 日付印か(3 段のハンコ)。
    pub fn is_date(&self) -> bool {
        self.name == "hanko-date"
    }

    /// 言語に合わせた説明。
    pub fn title_for(&self, english: bool) -> &'static str {
        if english { self.title_en } else { self.title }
    }

    fn diameter(&self) -> f32 {
        match self.group {
            StampGroup::Dot => DOT_SIZE,
            StampGroup::Hanko if self.is_date() => DATE_HANKO_SIZE,
            StampGroup::Hanko if self.glyph.lines().count() >= 3 => DATE_HANKO_SIZE,
            StampGroup::Hanko => HANKO_SIZE,
            _ => STAMP_SIZE,
        }
    }

    /// 中心 `center` に置くスタンプの図形を作る。
    pub fn shape(&self, id: ElementId, center: Vec2) -> Shape {
        let size = Vec2::splat(self.diameter());
        if self.is_hanko() {
            // 塗りなし・朱色の枠と文字。
            let mut s = Shape::new(
                id,
                ShapeKind::Stamp,
                center - size * 0.5,
                size,
                Rgba8::TRANSPARENT,
            );
            s.stroke = self.color;
            s.stroke_width = (size.y * 0.045).max(2.5);
            s.text = self.glyph.to_string();
            s.text_color = self.color;
            return s;
        }
        let mut s = Shape::new(id, ShapeKind::Stamp, center - size * 0.5, size, self.color);
        s.stroke = Rgba8::WHITE;
        s.stroke_width = if self.group == StampGroup::Dot {
            2.0
        } else {
            3.0
        };
        s.text = self.glyph.to_string();
        s.text_color = Rgba8::WHITE;
        s
    }
}

/// 全角の文字(漢字・かな)か。
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x30ff // 記号・かな
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff // 漢字
        | 0xf900..=0xfaff
        | 0xff00..=0xffef) // 全角英数
}

/// 文字の幅(全角を 1 とする)。
pub(crate) fn char_width(c: char) -> f32 {
    if is_wide(c) || matches!(c, '♥' | '♡' | '★' | '◎' | '○' | '△' | '×' | '→' | '↑')
    {
        1.0
    } else if c.is_ascii_uppercase() || c.is_ascii_digit() {
        0.62
    } else {
        0.55
    }
}

/// 印鑑の文字の並べ方。漢字・かなの名前は縦書き(1 文字ずつ改行)、
/// 4 文字なら右の列から読む 2 列にする。英字の名前はそのまま横書き。
pub fn seal_layout(name: &str) -> String {
    let chars: Vec<char> = name.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.is_empty() {
        return "印".to_string();
    }
    if !chars.iter().all(|&c| is_wide(c)) {
        return chars.iter().take(10).collect();
    }
    match chars.len() {
        1..=3 => chars
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        // 右の列が前半、左の列が後半(上から下へ、右から左へ読む)。
        4 => format!("{}{}\n{}{}", chars[2], chars[0], chars[3], chars[1]),
        _ => chars.iter().take(8).collect(),
    }
}

/// スタンプの文字の行の高さ(文字の大きさに対する比)。
pub const STAMP_LINE_HEIGHT: f32 = 1.15;
/// 日付印の行の高さ。段の間に線を引くので広めにする。
pub const DATE_LINE_HEIGHT: f32 = 1.5;

/// スタンプの文字の大きさ。丸に収まるよう、行数と最も長い行の幅で縮める。
pub fn stamp_font_size(text: &str, diameter: f32) -> f32 {
    stamp_font_size_with(text, diameter, STAMP_LINE_HEIGHT)
}

/// 行の高さ `line_height`(文字の大きさに対する比)を指定して文字の大きさを決める。
pub fn stamp_font_size_with(text: &str, diameter: f32, line_height: f32) -> f32 {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len().max(1) as f32;
    let widest = lines
        .iter()
        .map(|l| l.chars().map(char_width).sum::<f32>())
        .fold(0.0f32, f32::max)
        .max(0.5);
    // 丸に内接する正方形(直径の 0.7 倍)に収める。1 行なら少し広く使う。
    let avail = diameter * if n <= 1.0 { 0.78 } else { 0.68 };
    let by_width = avail / widest;
    let by_height = avail / (n * line_height);
    by_width.min(by_height).min(diameter * 0.46).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_shape() {
        let s = STAMPS[0].shape(ElementId(1), Vec2::new(100.0, 100.0));
        assert_eq!(s.kind, ShapeKind::Stamp);
        assert_eq!(s.bounds().center(), Vec2::new(100.0, 100.0));
        assert_eq!(s.text_style().color, Rgba8::WHITE);
        assert!(s.text_style().font_size > 20.0);
        assert!(s.contains(Vec2::new(100.0, 100.0)));
        assert!(!s.contains(Vec2::new(73.0, 73.0)), "丸の外の角");
        assert_eq!(Stamp::by_name("heart").unwrap().glyph, "♥");
        assert!(!s.is_hanko());
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = STAMPS.iter().map(|s| s.name.as_ref()).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len());
        assert!(STAMPS.len() >= 50);
    }

    #[test]
    fn hanko_shape() {
        let st = Stamp::by_name("hanko-approved").unwrap();
        let s = st.shape(ElementId(1), Vec2::ZERO);
        assert!(s.is_hanko());
        assert_eq!(s.fill.0[3], 0);
        assert_eq!(s.text_color, SHUNIKU);
        assert_eq!(s.size.x, HANKO_SIZE);
        // 2 文字は 1 文字より小さく、丸からはみ出さない。
        let one = stamp_font_size("済", HANKO_SIZE);
        let two = s.text_style().font_size;
        assert!(two < one);
        assert!(two * 2.0 <= HANKO_SIZE * 0.8);
    }

    #[test]
    fn dot_is_small_and_blank() {
        let s = Stamp::by_name("dot-red")
            .unwrap()
            .shape(ElementId(1), Vec2::ZERO);
        assert_eq!(s.size.x, DOT_SIZE);
        assert!(s.text.is_empty());
    }

    #[test]
    fn seal_layout_vertical() {
        assert_eq!(seal_layout("山田"), "山\n田");
        assert_eq!(seal_layout("佐々木"), "佐\n々\n木");
        assert_eq!(seal_layout("山田太郎"), "太山\n郎田");
        assert_eq!(seal_layout("Tsuno"), "Tsuno");
        assert_eq!(seal_layout(" "), "印");
    }

    #[test]
    fn date_hanko() {
        let st = Stamp::date_hanko("承認", "26.10.31", "山田");
        let s = st.shape(ElementId(1), Vec2::ZERO);
        assert!(s.is_date_hanko());
        assert_eq!(s.text, "承認\n26.10.31\n山田");
        // 3 段でも日付のないハンコは線を引かない。
        let good = Stamp::by_name("hanko-good")
            .unwrap()
            .shape(ElementId(2), Vec2::ZERO);
        assert!(!good.is_date_hanko());
        assert_eq!(s.size.x, DATE_HANKO_SIZE);
        let f = s.text_style().font_size;
        // 中段の日付が丸の幅に収まる。
        assert!(f * 8.0 * 0.62 <= DATE_HANKO_SIZE * 0.7);
        assert!(s.text_style().line_height * 3.0 <= DATE_HANKO_SIZE * 0.75);
    }

    #[test]
    fn old_stamp_font_unchanged() {
        // 以前からある 1〜2 文字のバッジは、これまでと同じ大きさ。
        for g in ["+1", "♥", "?"] {
            assert_eq!(stamp_font_size(g, STAMP_SIZE), STAMP_SIZE * 0.46);
        }
    }
}
