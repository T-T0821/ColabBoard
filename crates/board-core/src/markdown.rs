//! マークダウンの取り込みと書き出し。
//!
//! 取り込みは pulldown-cmark(Pure Rust)で読み、ボードの要素に置き換えて自動で並べる。
//!
//! - 見出し → フレーム(セクション)。いちばん上の階層の見出しが 1 つだけで文書の先頭にあれば、
//!   それは文書の題として大きな文字にし、その次の階層の見出しをフレームにする。
//! - それより深い見出し → 大きめの文字
//! - 段落 → 文字
//! - 箇条書き → 付箋(入れ子の項目は親の付箋に「・」付きでまとめる。チェックボックスは □ / ✓(描画用のフォントにある記号))
//! - 表 → 表(セルの格子)
//! - コードブロック・引用 → 色付きの四角形
//! - 画像 → 仮の四角形(読み込めたら画像に差し替える。差し替えは呼び出し側)
//!
//! フレームは横に最大 4 つ並べ、あふれたら次の段に送る。
//!
//! 書き出しはその逆で、フレームを見出し、付箋を箇条書き、表を表、文字を段落にする。

use glam::Vec2;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::board::Board;
use crate::color::Rgba8;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind, TextAlign, estimate_text_height};
use crate::table;

/// 付箋の一辺。
const STICKY: f32 = 200.0;
/// 付箋どうしの隙間。
const GAP: f32 = 20.0;
/// ブロックどうしの縦の隙間。
const BLOCK_GAP: f32 = 24.0;
/// 段の最小の幅(付箋 3 枚分)。
const COLUMN_MIN: f32 = STICKY * 3.0 + GAP * 2.0;
/// フレームの内側の余白。
const FRAME_PAD: f32 = 32.0;
/// フレームどうしの隙間(上の隙間にはフレームの見出しが入る)。
const SECTION_GAP: f32 = 96.0;
/// 横に並べるフレームの数。
const SECTIONS_PER_ROW: usize = 4;
/// 取り込む文字数の上限(巨大なファイルでボードが固まらないように)。
pub const MAX_CHARS: usize = 200_000;

const CODE_FILL: Rgba8 = Rgba8::hex(0xf1f3f4);
const QUOTE_FILL: Rgba8 = Rgba8::hex(0xe8f0fe);
const PLACEHOLDER_STROKE: Rgba8 = Rgba8::hex(0xb8bcc2);

// ---- 読み取り ----

#[derive(Clone, Debug, PartialEq)]
enum Block {
    Heading(u8, String),
    Paragraph {
        text: String,
        link: Option<String>,
    },
    List(Vec<Item>),
    /// 1 行目が見出し。
    Table(Vec<Vec<String>>),
    Code(String),
    Quote(String),
    Image {
        alt: String,
        url: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct Item {
    /// 入れ子の深さ(0 が一番外)。
    depth: usize,
    /// 「1. 」「□ 」など。
    prefix: String,
    text: String,
    link: Option<String>,
}

impl Item {
    fn line(&self) -> String {
        format!("{}{}", self.prefix, self.text)
    }
}

#[derive(Default)]
struct Reader {
    blocks: Vec<Block>,
    buf: String,
    link: Option<String>,
    /// 入れ子のリスト。番号付きなら次の番号。
    lists: Vec<Option<u64>>,
    items: Vec<Item>,
    table: Option<Vec<Vec<String>>>,
    quote_depth: usize,
    quote: String,
    /// 読み途中の画像(URL と代替テキスト)。
    image: Option<(String, String)>,
    /// ブロックの後ろに置く画像。
    images: Vec<Block>,
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn push_line(dst: &mut String, line: &str) {
    if !dst.is_empty() {
        dst.push('\n');
    }
    dst.push_str(line);
}

impl Reader {
    fn in_list(&self) -> bool {
        !self.lists.is_empty()
    }

    fn take_buf(&mut self) -> String {
        std::mem::take(&mut self.buf).trim().to_string()
    }

    /// たまった文字を今の項目に足す。
    fn flush_item(&mut self) {
        let text = self.take_buf();
        let link = self.link.take();
        if text.is_empty() {
            return;
        }
        if let Some(it) = self.items.last_mut() {
            push_line(&mut it.text, &text);
            if it.link.is_none() {
                it.link = link;
            }
        }
    }

    fn flush_images(&mut self) {
        if !self.in_list() && self.table.is_none() {
            self.blocks.append(&mut self.images);
        }
    }

    fn push(&mut self, b: Block) {
        self.blocks.push(b);
        self.flush_images();
    }

    fn text(&mut self, t: &str) {
        match &mut self.image {
            Some((_, alt)) => alt.push_str(t),
            None => self.buf.push_str(t),
        }
    }

    fn soft_break(&mut self) {
        // 英文は空白でつなぎ、日本語はそのままつなぐ。
        let dst = match &mut self.image {
            Some((_, alt)) => alt,
            None => &mut self.buf,
        };
        if dst.chars().last().is_some_and(|c| c.is_ascii()) {
            dst.push(' ');
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph | Tag::Heading { .. } | Tag::TableCell | Tag::CodeBlock(_) => {
                if self.in_list() {
                    self.flush_item();
                } else {
                    self.buf.clear();
                }
            }
            Tag::BlockQuote(_) => self.quote_depth += 1,
            Tag::List(start) => {
                if self.in_list() {
                    self.flush_item();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush_item();
                let depth = self.lists.len().saturating_sub(1);
                let prefix = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let p = format!("{n}. ");
                        *n += 1;
                        p
                    }
                    _ => String::new(),
                };
                self.items.push(Item {
                    depth,
                    prefix,
                    text: String::new(),
                    link: None,
                });
            }
            Tag::Table(_) => self.table = Some(Vec::new()),
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.push(Vec::new());
                }
            }
            Tag::Link { dest_url, .. } => {
                if self.link.is_none() && self.image.is_none() {
                    self.link = Some(dest_url.to_string());
                }
            }
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.to_string(), String::new())),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(level) => {
                if self.in_list() {
                    self.flush_item();
                    return;
                }
                let text = self.take_buf();
                self.link = None;
                if !text.is_empty() {
                    self.push(Block::Heading(heading_level(level), text));
                }
            }
            TagEnd::Paragraph => {
                if self.in_list() {
                    self.flush_item();
                } else if self.quote_depth > 0 {
                    let text = self.take_buf();
                    push_line(&mut self.quote, &text);
                } else {
                    let text = self.take_buf();
                    let link = self.link.take();
                    if text.is_empty() {
                        self.flush_images();
                    } else {
                        self.push(Block::Paragraph { text, link });
                    }
                }
            }
            TagEnd::CodeBlock => {
                let code = std::mem::take(&mut self.buf).trim_end().to_string();
                if self.in_list() {
                    if let Some(it) = self.items.last_mut() {
                        push_line(&mut it.text, &code);
                    }
                } else if self.quote_depth > 0 {
                    push_line(&mut self.quote, &code);
                } else if !code.is_empty() {
                    self.push(Block::Code(code));
                }
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                if self.quote_depth == 0 {
                    let q = std::mem::take(&mut self.quote);
                    if !q.trim().is_empty() {
                        self.push(Block::Quote(q.trim().to_string()));
                    }
                }
            }
            TagEnd::Item => self.flush_item(),
            TagEnd::List(_) => {
                self.flush_item();
                self.lists.pop();
                if !self.in_list() {
                    let items = std::mem::take(&mut self.items);
                    if items.is_empty() {
                        self.flush_images();
                    } else {
                        self.push(Block::List(items));
                    }
                }
            }
            TagEnd::TableCell => {
                let text = self.take_buf();
                self.link = None;
                if let Some(row) = self.table.as_mut().and_then(|t| t.last_mut()) {
                    row.push(text);
                }
            }
            TagEnd::Table => {
                let rows = self.table.take().unwrap_or_default();
                if rows.iter().any(|r| !r.is_empty()) {
                    self.push(Block::Table(rows));
                }
            }
            TagEnd::Image => {
                if let Some((url, alt)) = self.image.take() {
                    self.images.push(Block::Image {
                        alt: alt.trim().to_string(),
                        url,
                    });
                }
            }
            _ => {}
        }
    }
}

fn read(md: &str) -> Vec<Block> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut r = Reader::default();
    for ev in Parser::new_ext(md, opts) {
        match ev {
            Event::Start(tag) => r.start(tag),
            Event::End(tag) => r.end(tag),
            Event::Text(t) | Event::Code(t) => r.text(&t),
            Event::SoftBreak => r.soft_break(),
            Event::HardBreak => r.text("\n"),
            Event::TaskListMarker(done) => {
                if let Some(it) = r.items.last_mut() {
                    it.prefix = if done { "✓ " } else { "□ " }.into();
                }
            }
            _ => {}
        }
    }
    r.flush_images();
    r.blocks
}

/// 貼り付けられた文字列がマークダウンらしいか(見出し・箇条書き 2 行以上・表・コードブロック)。
pub fn looks_like_markdown(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let heading = lines.iter().any(|l| {
        let hashes = l.chars().take_while(|&c| c == '#').count();
        (1..=6).contains(&hashes) && l[hashes..].starts_with(' ')
    });
    let bullet = |l: &&str| {
        ["- ", "* ", "+ "].iter().any(|p| l.starts_with(p)) || {
            let digits = l.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && l[digits..].starts_with(". ")
        }
    };
    let list = lines.iter().filter(|l| bullet(l)).count() >= 2;
    let table = lines.windows(2).any(|w| {
        w[0].contains('|')
            && w[1].contains("---")
            && w[1].chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    });
    let fence = lines.iter().any(|l| l.starts_with("```"));
    heading || list || table || fence
}

// ---- 配置 ----

/// 読み込みを待つ画像。`placeholder` を画像に差し替える。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    pub placeholder: ElementId,
    pub url: String,
}

/// 取り込んだ結果。
#[derive(Clone, Debug, Default)]
pub struct Imported {
    pub ids: Vec<ElementId>,
    pub images: Vec<ImageRef>,
}

struct Section {
    title: Option<String>,
    blocks: Vec<Block>,
}

/// 文書の題と、見出しで区切ったセクション。
fn split_sections(mut blocks: Vec<Block>) -> (Option<String>, Vec<Section>) {
    let levels: Vec<u8> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Heading(l, _) => Some(*l),
            _ => None,
        })
        .collect();
    let mut title = None;
    if let Some(Block::Heading(l, t)) = blocks.first()
        && levels.iter().filter(|x| **x <= *l).count() == 1
    {
        title = Some(t.clone());
        blocks.remove(0);
    }
    let level = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Heading(l, _) => Some(*l),
            _ => None,
        })
        .min();
    let mut sections: Vec<Section> = Vec::new();
    for b in blocks {
        match b {
            Block::Heading(l, t) if Some(l) == level => sections.push(Section {
                title: Some(t),
                blocks: Vec::new(),
            }),
            b => {
                if sections.is_empty() {
                    sections.push(Section {
                        title: None,
                        blocks: Vec::new(),
                    });
                }
                if let Some(s) = sections.last_mut() {
                    s.blocks.push(b);
                }
            }
        }
    }
    // 題だけで見出しがなければ、題をフレームの見出しにする。
    if level.is_none()
        && let [only] = sections.as_mut_slice()
    {
        only.title = title.take();
    }
    (title, sections)
}

/// 中身に合わせて高さを伸ばす(最小 `min_h`)。
fn fit_height(s: &mut Shape, min_h: f32) {
    if s.kind == ShapeKind::Text {
        s.fit_text_height();
        return;
    }
    let style = s.text_style();
    let inner = s.text_rect().size();
    let pad = s.size.y - inner.y;
    let h = estimate_text_height(&s.text, inner.x, &style) + pad;
    s.size.y = h.max(min_h);
}

fn text_shape(id: ElementId, pos: Vec2, width: f32, text: String, font: f32) -> Shape {
    let mut s = Shape::text(id, pos, Vec2::new(width, 40.0));
    s.text = text;
    s.font_size = font;
    fit_height(&mut s, 0.0);
    s
}

fn boxed(id: ElementId, pos: Vec2, width: f32, text: String, fill: Rgba8, font: f32) -> Shape {
    let mut s = Shape::rect(id, pos, Vec2::new(width, 48.0), fill);
    s.stroke = Rgba8::TRANSPARENT;
    s.stroke_width = 0.0;
    s.corner_radius = 6.0;
    s.align = TextAlign::Left;
    s.font_size = font;
    s.text = text;
    fit_height(&mut s, 48.0);
    s
}

/// 付箋 1 枚の本文(入れ子の項目は「・」付きの行にする)。
fn sticky_text(items: &[Item]) -> String {
    let mut out = String::new();
    for it in items {
        if it.depth == 0 {
            push_line(&mut out, &it.line());
        } else {
            let indent = "  ".repeat(it.depth - 1);
            let mark = if it.prefix.is_empty() { "・" } else { "" };
            push_line(&mut out, &format!("{indent}{mark}{}", it.line()));
        }
    }
    out
}

struct Layout<'a> {
    board: &'a mut Board,
    sticky: Rgba8,
    frames: Vec<Element>,
    content: Vec<Element>,
    images: Vec<ImageRef>,
}

impl Layout<'_> {
    fn id(&mut self) -> ElementId {
        self.board.alloc_id()
    }

    fn add(&mut self, s: Shape) -> f32 {
        let h = s.size.y;
        self.content.push(s.into());
        h
    }

    /// 段の幅(表が入る幅、最小は付箋 3 枚分)。
    fn column_width(blocks: &[Block]) -> f32 {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Table(rows) => Some(table::measure(rows).0.iter().sum::<f32>()),
                _ => None,
            })
            .fold(COLUMN_MIN, f32::max)
    }

    /// ブロックを左上 `at`、幅 `width` で縦に積む。積んだ高さを返す。
    fn column(&mut self, blocks: Vec<Block>, at: Vec2, width: f32) -> f32 {
        let mut y = at.y;
        for (i, b) in blocks.into_iter().enumerate() {
            if i > 0 {
                y += BLOCK_GAP;
            }
            let pos = Vec2::new(at.x, y);
            y += self.block(b, pos, width);
        }
        y - at.y
    }

    fn block(&mut self, b: Block, pos: Vec2, width: f32) -> f32 {
        match b {
            Block::Heading(level, text) => {
                let font = match level {
                    1 => 36.0,
                    2 => 32.0,
                    3 => 28.0,
                    _ => 26.0,
                };
                let id = self.id();
                self.add(text_shape(id, pos, width, text, font))
            }
            Block::Paragraph { text, link } => {
                let id = self.id();
                let mut s = text_shape(id, pos, width, text, 18.0);
                s.meta.link = link.unwrap_or_default();
                self.add(s)
            }
            Block::Code(text) => {
                let id = self.id();
                self.add(boxed(id, pos, width, text, CODE_FILL, 14.0))
            }
            Block::Quote(text) => {
                let id = self.id();
                self.add(boxed(id, pos, width, text, QUOTE_FILL, 18.0))
            }
            Block::Image { alt, url } => {
                let id = self.id();
                let size = Vec2::new(width.min(480.0), 270.0);
                let mut s = Shape::rect(id, pos, size, CODE_FILL);
                s.stroke = PLACEHOLDER_STROKE;
                s.stroke_width = 1.0;
                s.corner_radius = 0.0;
                s.font_size = 16.0;
                s.text = if alt.is_empty() { "画像".into() } else { alt };
                s.meta.link = url.clone();
                self.images.push(ImageRef {
                    placeholder: id,
                    url,
                });
                self.add(s)
            }
            Block::List(items) => self.stickies(items, pos, width),
            Block::Table(rows) => self.table(rows, pos),
        }
    }

    fn stickies(&mut self, items: Vec<Item>, pos: Vec2, width: f32) -> f32 {
        // 外側の項目ごとに 1 枚。
        let mut groups: Vec<&[Item]> = Vec::new();
        let mut start = 0;
        for i in 1..=items.len() {
            if i == items.len() || items[i].depth == 0 {
                if i > start {
                    groups.push(&items[start..i]);
                }
                start = i;
            }
        }
        let per_row = (((width + GAP) / (STICKY + GAP)).floor() as usize).max(1);
        let mut y = pos.y;
        for row in groups.chunks(per_row) {
            let mut notes: Vec<Shape> = Vec::new();
            for (c, group) in row.iter().enumerate() {
                let id = self.id();
                let at = Vec2::new(pos.x + c as f32 * (STICKY + GAP), y);
                let mut s = Shape::sticky(id, at, self.sticky);
                s.text = sticky_text(group);
                s.font_size = 18.0;
                s.meta.link = group
                    .iter()
                    .find_map(|it| it.link.clone())
                    .unwrap_or_default();
                fit_height(&mut s, STICKY);
                notes.push(s);
            }
            // 段の中は高さをそろえる。
            let h = notes.iter().map(|s| s.size.y).fold(STICKY, f32::max);
            for mut s in notes {
                s.size.y = h;
                self.add(s);
            }
            y += h + GAP;
        }
        (y - GAP - pos.y).max(0.0)
    }

    fn table(&mut self, rows: Vec<Vec<String>>, pos: Vec2) -> f32 {
        let board = &mut *self.board;
        let cells = table::build(&rows, pos, &mut || board.alloc_id());
        let h = cells.iter().map(|c| c.max().y - pos.y).fold(0.0, f32::max);
        self.content.extend(cells.into_iter().map(Element::from));
        h
    }
}

/// マークダウンを読み、中心 `center` に並べてボードに足す。何も読めなければ `None`。
///
/// 付箋の色は `sticky`。画像は仮の四角形で置き、`Imported::images` で差し替え先を返す。
pub fn insert(board: &mut Board, md: &str, center: Vec2, sticky: Rgba8) -> Option<Imported> {
    let md: String = md.chars().take(MAX_CHARS).collect();
    let blocks = read(&md);
    if blocks.is_empty() {
        return None;
    }
    let (title, mut sections) = split_sections(blocks);
    let mut l = Layout {
        board,
        sticky,
        frames: Vec::new(),
        content: Vec::new(),
        images: Vec::new(),
    };
    let mut y = 0.0;
    if let Some(t) = title {
        let id = l.id();
        let s = text_shape(id, Vec2::ZERO, COLUMN_MIN * 1.5, t, 48.0);
        y = s.size.y + BLOCK_GAP + crate::shape::FRAME_TITLE_HEIGHT;
        l.add(s);
    }
    for row in sections.chunks_mut(SECTIONS_PER_ROW) {
        let mut x = 0.0;
        let mut row_h: f32 = 0.0;
        for sec in row {
            let width = Layout::column_width(&sec.blocks);
            let blocks = std::mem::take(&mut sec.blocks);
            let at = Vec2::new(x + FRAME_PAD, y + FRAME_PAD);
            let h = l.column(blocks, at, width);
            let size = Vec2::new(width, h) + FRAME_PAD * 2.0;
            if let Some(t) = sec.title.take() {
                let id = l.id();
                let mut f = Shape::frame(id, Vec2::new(x, y), size);
                f.text = t;
                l.frames.push(f.into());
            }
            x += size.x + SECTION_GAP;
            row_h = row_h.max(size.y);
        }
        y += row_h + SECTION_GAP;
    }
    let Layout {
        frames,
        content,
        images,
        ..
    } = l;
    // フレームは中身より下に置く。
    let mut elements = frames;
    elements.extend(content);
    let bounds = elements
        .iter()
        .map(Element::bounds)
        .reduce(|a, b| a.union(&b))?;
    let d = center - bounds.center();
    for e in &mut elements {
        e.translate(d);
    }
    let ids = elements.iter().map(Element::id).collect();
    board.push_many(elements);
    Some(Imported { ids, images })
}

// ---- 書き出し ----

/// 書き出しの単位。
enum Unit<'a> {
    Shape(&'a Shape),
    Table(table::Grid),
}

impl Unit<'_> {
    fn bounds(&self) -> Rect {
        match self {
            Unit::Shape(s) => s.bounds(),
            Unit::Table(g) => table::bounds(g),
        }
    }
}

/// 表のセルの文字(改行は `<br>`、`|` はエスケープ)。
fn cell_text(board: &Board, id: ElementId) -> String {
    board
        .get(id)
        .map(|e| e.text().trim().replace('|', "\\|").replace('\n', "<br>"))
        .unwrap_or_default()
}

fn table_md(board: &Board, g: &table::Grid) -> String {
    let mut out = String::new();
    for (r, row) in g.cells.iter().enumerate() {
        let cells: Vec<String> = row.iter().map(|id| cell_text(board, *id)).collect();
        push_line(&mut out, &format!("| {} |", cells.join(" | ")));
        if r == 0 {
            let sep = vec!["---"; row.len()].join(" | ");
            push_line(&mut out, &format!("| {sep} |"));
        }
    }
    out
}

fn with_link(text: &str, link: &str) -> String {
    if link.is_empty() {
        text.to_string()
    } else {
        format!("[{text}]({link})")
    }
}

/// 1 つの単位をマークダウンにする。付箋は箇条書きの 1 項目(`true`)として返す。
fn unit_md(board: &Board, u: &Unit) -> Option<(String, bool)> {
    let s = match u {
        Unit::Table(g) => return Some((table_md(board, g), false)),
        Unit::Shape(s) => s,
    };
    let text = s.text.trim();
    match s.kind {
        ShapeKind::Image if s.meta.link.is_empty() => None,
        ShapeKind::Image => Some((format!("![]({})", s.meta.link), false)),
        ShapeKind::Video if s.media.is_empty() => None,
        ShapeKind::Video => Some((format!("[動画メモ]({})", s.media), false)),
        // 取り込んだときの画像の仮置き。
        ShapeKind::Rect
            if s.stroke == PLACEHOLDER_STROKE && !s.meta.link.is_empty() && s.meta.group == 0 =>
        {
            Some((format!("![{text}]({})", s.meta.link), false))
        }
        _ if text.is_empty() => None,
        ShapeKind::Sticky => {
            // 取り込んだチェックボックスは元の書き方に戻す。
            let (mark, text) = if let Some(t) = text.strip_prefix("□ ") {
                ("[ ] ", t)
            } else if let Some(t) = text.strip_prefix("✓ ") {
                ("[x] ", t)
            } else {
                ("", text)
            };
            let body = with_link(text, &s.meta.link).replace('\n', "\n  ");
            Some((format!("- {mark}{body}"), true))
        }
        ShapeKind::Text if s.font_size >= 26.0 => {
            let level = if s.font_size >= 40.0 {
                1
            } else if s.font_size >= 30.0 {
                3
            } else {
                4
            };
            let line = text.replace('\n', " ");
            Some((format!("{} {line}", "#".repeat(level)), false))
        }
        _ if s.fill == CODE_FILL && s.font_size == 14.0 => {
            Some((format!("```\n{text}\n```"), false))
        }
        _ if s.fill == QUOTE_FILL => Some((format!("> {}", text.replace('\n', "\n> ")), false)),
        _ => Some((with_link(text, &s.meta.link), false)),
    }
}

fn units_md(board: &Board, mut units: Vec<Unit>, out: &mut Vec<String>) {
    units.sort_by(|a, b| {
        let (a, b) = (a.bounds().min, b.bounds().min);
        (a.y, a.x)
            .partial_cmp(&(b.y, b.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut list = String::new();
    for u in &units {
        let Some((md, item)) = unit_md(board, u) else {
            continue;
        };
        if item {
            push_line(&mut list, &md);
        } else {
            if !list.is_empty() {
                out.push(std::mem::take(&mut list));
            }
            out.push(md);
        }
    }
    if !list.is_empty() {
        out.push(list);
    }
}

/// 要素 `ids`(空ならボード全体)をマークダウンにする。
///
/// フレームは見出し(`##`)にし、中の要素を上から順に並べる。選んだフレームの中身も含める。
pub fn export(board: &Board, ids: &[ElementId]) -> String {
    let all = ids.is_empty();
    let frames: Vec<&Shape> = board
        .elements()
        .iter()
        .filter_map(Element::as_shape)
        .filter(|s| s.kind == ShapeKind::Frame && (all || ids.contains(&s.id)))
        .collect();
    let in_frame = |s: &Shape| {
        let c = s.bounds().center();
        frames
            .iter()
            .filter(|f| f.id != s.id && f.bounds().contains(c))
            .min_by(|a, b| {
                let (a, b) = (a.size.x * a.size.y, b.size.x * b.size.y);
                a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|f| f.id)
    };
    // 対象の図形と、それぞれが入っているフレーム。
    let mut targets: Vec<(&Shape, Option<ElementId>)> = Vec::new();
    for s in board.elements().iter().filter_map(Element::as_shape) {
        if s.kind == ShapeKind::Frame {
            continue;
        }
        let frame = in_frame(s);
        if all || ids.contains(&s.id) || frame.is_some() {
            targets.push((s, frame));
        }
    }
    // 表はまとめて 1 単位にする。
    let mut tables: Vec<u64> = Vec::new();
    let mut loose: Vec<Unit> = Vec::new();
    let mut framed: Vec<(ElementId, Unit)> = Vec::new();
    for (s, frame) in targets {
        let g = s.meta.group;
        let unit = if g != 0 && s.kind == ShapeKind::Rect {
            if tables.contains(&g) {
                continue;
            }
            match table::grid(board, g) {
                Some(grid) => {
                    tables.push(g);
                    Unit::Table(grid)
                }
                None => Unit::Shape(s),
            }
        } else {
            Unit::Shape(s)
        };
        match frame {
            Some(f) => framed.push((f, unit)),
            None => loose.push(unit),
        }
    }
    let mut out = Vec::new();
    units_md(board, loose, &mut out);
    let mut frames = frames;
    frames.sort_by(|a, b| {
        (a.pos.y, a.pos.x)
            .partial_cmp(&(b.pos.y, b.pos.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for f in frames {
        out.push(format!("## {}", f.text.trim().replace('\n', " ")));
        let mut units = Vec::new();
        let mut rest = Vec::new();
        for (fid, u) in framed {
            if fid == f.id {
                units.push(u);
            } else {
                rest.push((fid, u));
            }
        }
        framed = rest;
        units_md(board, units, &mut out);
    }
    let mut md = out.join("\n\n");
    md.push('\n');
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# 企画書

## 目的
新しいボードを作る。

- 速い
- 共同編集
  - CRDT
- [ ] オフライン

## 予定
| 項目 | 期限 |
| --- | --- |
| 設計 | 11月 |
| 実装 | 12月 |

```
cargo build
```

> 引用です

![図](https://example.com/a.png)
";

    fn shapes(board: &Board) -> Vec<&Shape> {
        board
            .elements()
            .iter()
            .filter_map(Element::as_shape)
            .collect()
    }

    #[test]
    fn reads_blocks() {
        let blocks = read(SAMPLE);
        assert_eq!(blocks[0], Block::Heading(1, "企画書".into()));
        let Block::List(items) = &blocks[3] else {
            panic!("{:?}", blocks[3]);
        };
        assert_eq!(items.len(), 4);
        assert_eq!(items[2].depth, 1);
        assert_eq!(items[3].line(), "□ オフライン");
        assert!(blocks.contains(&Block::Code("cargo build".into())));
        assert!(blocks.contains(&Block::Quote("引用です".into())));
        assert!(blocks.contains(&Block::Image {
            alt: "図".into(),
            url: "https://example.com/a.png".into()
        }));
    }

    #[test]
    fn imports_sections_as_frames() {
        let mut board = Board::new();
        let r = insert(&mut board, SAMPLE, Vec2::ZERO, Rgba8::WHITE).unwrap();
        assert_eq!(r.ids.len(), board.len());
        let all = shapes(&board);
        let frames: Vec<_> = all.iter().filter(|s| s.kind == ShapeKind::Frame).collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].text, "目的");
        // 題は大きな文字。
        assert!(
            all.iter()
                .any(|s| s.kind == ShapeKind::Text && s.text == "企画書")
        );
        // 付箋は 3 枚(入れ子はまとめる)で、すべてフレームの中。
        let notes: Vec<_> = all.iter().filter(|s| s.kind == ShapeKind::Sticky).collect();
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[1].text, "共同編集\n・CRDT");
        for n in &notes {
            assert!(frames[0].bounds().contains(n.bounds().center()));
        }
        // 表は格子として読める。
        let cell = all.iter().find(|s| s.text == "設計").unwrap();
        let g = table::grid(&board, cell.meta.group).unwrap();
        assert_eq!((g.rows(), g.cols()), (3, 2));
        assert!(frames[1].bounds().contains(table::bounds(&g).center()));
        // 画像は仮の四角形。
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[0].url, "https://example.com/a.png");
        // 中心がそろう。
        assert!(board.bounds().unwrap().center().length() < 1.0);
    }

    #[test]
    fn plain_list_becomes_stickies() {
        let mut board = Board::new();
        insert(&mut board, "- a\n- b\n- c\n- d", Vec2::ZERO, Rgba8::WHITE).unwrap();
        let all = shapes(&board);
        assert_eq!(all.len(), 4);
        assert!(all.iter().all(|s| s.kind == ShapeKind::Sticky));
        // 3 枚で折り返す。
        assert!(all[3].pos.y > all[0].pos.y);
        assert!(insert(&mut board, "   \n", Vec2::ZERO, Rgba8::WHITE).is_none());
    }

    #[test]
    fn detects_markdown() {
        assert!(looks_like_markdown("# 見出し\n本文"));
        assert!(looks_like_markdown("- a\n- b"));
        assert!(looks_like_markdown("1. a\n2. b"));
        assert!(looks_like_markdown("|a|b|\n|---|---|\n|1|2|"));
        assert!(!looks_like_markdown("ただの文章です"));
        assert!(!looks_like_markdown("#hashtag\n- 1つだけ"));
    }

    #[test]
    fn round_trip() {
        let mut board = Board::new();
        insert(&mut board, SAMPLE, Vec2::ZERO, Rgba8::WHITE).unwrap();
        let md = export(&board, &[]);
        assert!(md.starts_with("# 企画書\n\n## 目的\n\n新しいボードを作る。\n\n- 速い\n- 共同編集\n  ・CRDT\n- [ ] オフライン"), "{md}");
        assert!(
            md.contains("## 予定\n\n| 項目 | 期限 |\n| --- | --- |\n| 設計 | 11月 |"),
            "{md}"
        );
        assert!(md.contains("```\ncargo build\n```"), "{md}");
        assert!(md.contains("> 引用です"), "{md}");
        // 読み直しても同じ数のフレーム。
        let mut again = Board::new();
        insert(&mut again, &md, Vec2::ZERO, Rgba8::WHITE).unwrap();
        let frames = shapes(&again)
            .iter()
            .filter(|s| s.kind == ShapeKind::Frame)
            .count();
        assert_eq!(frames, 2);
    }
}
