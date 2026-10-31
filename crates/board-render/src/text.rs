//! 図形内の文字の GPU 描画(glyphon / cosmic-text)。
//!
//! 文字はワールド単位でレイアウトし、表示倍率は glyphon の `scale` で掛ける。
//! レイアウト結果は要素ごとにキャッシュし、文字か幅が変わったときだけ組み直す。

use std::collections::HashMap;

use board_core::{Board, Camera, Element, ElementId, Rect, ShapeKind, TextBox};

use crate::Layer;
use glyphon::{
    Attrs, Buffer, Cache, Color, ColorMode, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};

/// 画面上でこれより小さい文字は描かない(ズームアウト時の LOD)。
const MIN_SCREEN_FONT_PX: f32 = 4.0;
/// 一度に描く文字領域の上限。これを超える場合は文字を省略する。
const MAX_TEXT_AREAS: usize = 4000;

struct Cached {
    text: String,
    width: f32,
    font_size: f32,
    align: board_core::TextAlign,
    buffer: Buffer,
    height: f32,
    /// 縦書きの列(右から順)。縦書きでなければ空。
    columns: Vec<Column>,
    /// 縦書きのときの枠の高さ(列の折り返しに使う)。横書きなら負。
    column_height: f32,
    /// 前回の prepare で使ったか(使われなくなったものを捨てる)。
    used: bool,
}

/// 縦書きの 1 列。1 文字ずつ改行した横書きのバッファで描く。
struct Column {
    buffer: Buffer,
    /// 文字数。
    len: usize,
}

/// 縦書きの 1 文字の送り(文字の大きさに対する比)。
const VERTICAL_ADVANCE: f32 = 1.1;

/// 縦書き用の字形に置き換える OpenType の機能。
fn vertical_features() -> glyphon::cosmic_text::FontFeatures {
    let mut f = glyphon::cosmic_text::FontFeatures::new();
    f.enable(glyphon::cosmic_text::FeatureTag::new(b"vert"));
    f
}

/// 縦書きで向きを変える約物のうち、フォントの vert が効かないもの(1 文字だけの行は
/// 用字系が決まらず置き換えられない)を、形の近い文字にする。
fn vertical_form(c: char) -> char {
    match c {
        '「' | '『' => '┐',
        '」' | '』' => '└',
        '—' | '―' => '│',
        c => c,
    }
}

/// 縦書きの列に分ける。段落(改行)ごとに新しい列にし、`per_column` 文字で折り返す。
fn split_columns(text: &str, per_column: usize) -> Vec<Vec<char>> {
    let per_column = per_column.max(1);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let chars: Vec<char> = para.chars().map(vertical_form).collect();
        if chars.is_empty() {
            out.push(Vec::new());
            continue;
        }
        for chunk in chars.chunks(per_column) {
            out.push(chunk.to_vec());
        }
    }
    out
}

/// 文字を描く 1 要素分。
struct Item<'a> {
    id: ElementId,
    /// 0 は本文、1 は付箋の作者名。
    slot: u8,
    text: &'a str,
    tb: TextBox,
    /// 塗りのない要素(文字だけ・枠だけの図形)。背景がキャンバスそのものになる。
    on_canvas: bool,
}

pub struct TextLayer {
    font_system: FontSystem,
    swash: SwashCache,
    _cache: Cache,
    atlas: TextAtlas,
    /// 段ごとの描画器(段の数だけ増やし、使い回す)。
    renderers: Vec<TextRenderer>,
    /// 段ごとに準備できたか。
    prepared: Vec<bool>,
    viewport: Viewport,
    buffers: HashMap<(ElementId, u8), Cached>,
    has_font: bool,
    dark: bool,
    /// 付箋の作者名を出すか。
    show_authors: bool,
}

impl TextLayer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // ブラウザにはシステムフォントがないので、空のデータベースから始めて後で読み込む。
        let font_system =
            FontSystem::new_with_locale_and_db("ja-JP".into(), glyphon::fontdb::Database::new());
        let cache = Cache::new(device);
        let color_mode = if format.is_srgb() {
            ColorMode::Accurate
        } else {
            ColorMode::Web
        };
        let atlas = TextAtlas::with_color_mode(device, queue, &cache, format, color_mode);
        let viewport = Viewport::new(device, &cache);
        Self {
            font_system,
            swash: SwashCache::new(),
            _cache: cache,
            atlas,
            renderers: Vec::new(),
            prepared: Vec::new(),
            viewport,
            buffers: HashMap::new(),
            has_font: false,
            dark: false,
            show_authors: true,
        }
    }

    /// TTF/OTF のフォントデータを追加する。
    pub fn load_font(&mut self, data: Vec<u8>) {
        self.font_system.db_mut().load_font_data(data);
        self.has_font = self.font_system.db().faces().next().is_some();
        // 既存のレイアウトは新しいフォントで組み直す。
        self.buffers.clear();
    }

    pub fn has_font(&self) -> bool {
        self.has_font
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
    }

    pub fn set_show_authors(&mut self, on: bool) {
        self.show_authors = on;
    }

    /// 見えている図形の文字を準備する。`skip` は編集中で別の欄に表示している要素。
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        board: &Board,
        camera: &Camera,
        layers: &[Layer],
        skip: Option<ElementId>,
        hidden: &std::collections::HashSet<ElementId>,
    ) -> usize {
        self.prepared.clear();
        self.prepared.resize(layers.len(), false);
        if !self.has_font {
            return 0;
        }
        let (view_min, view_max) = camera.visible_world_rect();
        let view = Rect::new(view_min, view_max);

        // 見えていて、文字があり、十分な大きさで表示される図形を集める(要素の並び順)。
        // 空間インデックスで画面付近の要素だけに絞る(広すぎる範囲なら全要素を見る)。
        let elements = board.elements();
        let candidates = board.query_candidates(&view);
        let near: Box<dyn Iterator<Item = (usize, &Element)>> = match &candidates {
            Some(c) => Box::new(c.iter().map(|&i| (i, &elements[i]))),
            None => Box::new(elements.iter().enumerate()),
        };
        let show_authors = self.show_authors;
        let visible: Vec<(usize, Item<'_>)> = near
            .flat_map(|(i, e)| {
                let masked = hidden.contains(&e.id());
                // 伏せたカードは中身の代わりに「作業中」とだけ出す(描かない種類は何も出さない)。
                let text = if masked {
                    board_core::privacy::face_down(e).map(|_| board_core::privacy::FACE_DOWN_TEXT)
                } else {
                    Some(e.text())
                };
                let body = text
                    .filter(|t| !t.is_empty() && Some(e.id()) != skip)
                    .and_then(|t| Some((t, e.text_box()?)))
                    .map(|(text, tb)| {
                        (
                            i,
                            Item {
                                id: e.id(),
                                slot: 0,
                                text,
                                tb,
                                on_canvas: e.as_shape().is_none_or(|s| {
                                    s.fill.0[3] < 128 || s.kind == ShapeKind::Frame
                                }),
                            },
                        )
                    });
                let author = e
                    .as_shape()
                    .filter(|_| show_authors && !masked)
                    .and_then(|s| Some((s, s.author_box()?)))
                    .map(|(s, tb)| {
                        (
                            i,
                            Item {
                                id: e.id(),
                                slot: 1,
                                text: s.meta.author.as_str(),
                                tb,
                                on_canvas: false,
                            },
                        )
                    });
                body.into_iter().chain(author)
            })
            .filter(|(_, s)| s.tb.style.font_size * camera.zoom >= MIN_SCREEN_FONT_PX)
            .filter(|(_, s)| s.tb.clip.intersects(&view))
            .take(MAX_TEXT_AREAS + 1)
            .collect();
        if visible.is_empty() || visible.len() > MAX_TEXT_AREAS {
            return 0;
        }

        for c in self.buffers.values_mut() {
            c.used = false;
        }

        let Self {
            font_system,
            swash,
            atlas,
            renderers,
            prepared,
            viewport,
            buffers,
            dark,
            ..
        } = self;
        let dark = *dark;

        for (_, s) in &visible {
            let style = s.tb.style;
            let width = s.tb.rect.size().x.max(1.0);
            let entry = buffers.entry((s.id, s.slot)).or_insert_with(|| Cached {
                text: String::new(),
                width: -1.0,
                font_size: 0.0,
                align: style.align,
                buffer: Buffer::new(
                    font_system,
                    Metrics::new(style.font_size, style.line_height),
                ),
                height: 0.0,
                columns: Vec::new(),
                column_height: -1.0,
                used: false,
            });
            entry.used = true;
            let column_height = if s.tb.vertical {
                s.tb.rect.size().y.max(1.0)
            } else {
                -1.0
            };
            if s.tb.vertical
                && (entry.text != s.text
                    || entry.column_height != column_height
                    || entry.font_size != style.font_size)
            {
                // 縦書き: 列ごとに 1 文字ずつ改行したバッファを作る。
                let advance = style.font_size * VERTICAL_ADVANCE;
                let per_column = (column_height / advance).floor() as usize;
                let cols = split_columns(s.text, per_column);
                entry.columns = cols
                    .into_iter()
                    .map(|chars| {
                        let mut b =
                            Buffer::new(font_system, Metrics::new(style.font_size, advance));
                        b.set_wrap(Wrap::None);
                        b.set_size(Some(style.line_height), None);
                        let text: String = chars
                            .iter()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join("\n");
                        b.set_text(
                            &text,
                            // 約物(「」ー など)はフォントの縦書き用の字形(OpenType の vert)にする。
                            &Attrs::new()
                                .family(Family::SansSerif)
                                .font_features(vertical_features()),
                            Shaping::Advanced,
                            Some(glyphon::cosmic_text::Align::Center),
                        );
                        b.shape_until_scroll(font_system, false);
                        Column {
                            buffer: b,
                            len: chars.len(),
                        }
                    })
                    .collect();
                entry.text = s.text.to_string();
                entry.column_height = column_height;
                entry.font_size = style.font_size;
                entry.width = -1.0;
                continue;
            }
            if s.tb.vertical {
                continue;
            }
            if !entry.columns.is_empty() {
                entry.columns.clear();
                entry.column_height = -1.0;
                entry.width = -1.0;
            }
            if entry.text != s.text
                || entry.width != width
                || entry.font_size != style.font_size
                || entry.align != style.align
            {
                let b = &mut entry.buffer;
                b.set_metrics(Metrics::new(style.font_size, style.line_height));
                b.set_wrap(Wrap::WordOrGlyph);
                b.set_size(Some(width), None);
                b.set_text(
                    s.text,
                    &Attrs::new().family(Family::SansSerif),
                    Shaping::Advanced,
                    Some(match style.align {
                        board_core::TextAlign::Left => glyphon::cosmic_text::Align::Left,
                        board_core::TextAlign::Center => glyphon::cosmic_text::Align::Center,
                        board_core::TextAlign::Right => glyphon::cosmic_text::Align::Right,
                    }),
                );
                b.shape_until_scroll(font_system, false);
                entry.height = b
                    .layout_runs()
                    .last()
                    .map(|r| r.line_top + r.line_height)
                    .unwrap_or(0.0);
                entry.text = s.text.to_string();
                entry.width = width;
                entry.font_size = style.font_size;
                entry.align = style.align;
            }
        }

        let (w, h) = (camera.viewport.x as u32, camera.viewport.y as u32);
        viewport.update(
            queue,
            Resolution {
                width: w,
                height: h,
            },
        );

        let buffers: &HashMap<(ElementId, u8), Cached> = buffers;

        // 段ごとに、その段の要素の文字だけを準備する。
        let mut rest = visible.as_slice();
        for (li, layer) in layers.iter().enumerate() {
            let n = rest
                .iter()
                .take_while(|(i, _)| *i < layer.elements.end)
                .count();
            let (mine, tail) = rest.split_at(n);
            rest = tail;
            if mine.is_empty() {
                continue;
            }
            while renderers.len() <= li {
                renderers.push(TextRenderer::new(
                    atlas,
                    device,
                    wgpu::MultisampleState::default(),
                    None,
                ));
            }
            let mut areas = Vec::with_capacity(mine.len());
            for (_, s) in mine {
                areas_of(buffers, s, camera, dark, &mut areas);
            }
            match renderers[li].prepare(device, queue, font_system, atlas, viewport, areas, swash) {
                Ok(()) => prepared[li] = true,
                Err(e) => log::warn!("文字の準備に失敗しました: {e}"),
            }
        }
        // 使われなかった(画面外・削除済み)レイアウトを捨てる。
        let count = visible.len();
        self.buffers.retain(|_, c| c.used);
        count
    }

    /// `layer` 段目の文字を描く。
    pub fn render(&self, layer: usize, pass: &mut wgpu::RenderPass<'_>) {
        if !self.prepared.get(layer).copied().unwrap_or(false) {
            return;
        }
        if let Err(e) = self.renderers[layer].render(&self.atlas, &self.viewport, pass) {
            log::warn!("文字の描画に失敗しました: {e}");
        }
    }

    /// フレームの終わりに呼ぶ。使われなくなったグリフをアトラスから外す。
    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}

/// 1 要素分の文字領域(縦書きは列ごとに 1 つ)を `out` に足す。
fn areas_of<'a>(
    buffers: &'a HashMap<(ElementId, u8), Cached>,
    s: &Item<'_>,
    camera: &Camera,
    dark: bool,
    out: &mut Vec<TextArea<'a>>,
) {
    let Some(c) = buffers.get(&(s.id, s.slot)) else {
        return;
    };
    let rect = s.tb.rect;
    let clip = s.tb.clip;
    let clip_min = camera.world_to_screen(clip.min);
    let clip_max = camera.world_to_screen(clip.max);
    let bounds = TextBounds {
        left: clip_min.x.floor() as i32,
        top: clip_min.y.floor() as i32,
        right: clip_max.x.ceil() as i32,
        bottom: clip_max.y.ceil() as i32,
    };
    let [r, g, b, a] = crate::dark_ink(s.tb.style.color.0, dark && s.on_canvas);
    let color = Color::rgba(r, g, b, a);
    let mut push = |buffer, at: glam::Vec2| {
        let origin = camera.world_to_screen(at);
        out.push(TextArea {
            buffer,
            left: origin.x,
            top: origin.y,
            scale: camera.zoom,
            bounds,
            default_color: color,
            custom_glyphs: &[],
        });
    };
    if s.tb.vertical {
        let style = s.tb.style;
        let col_w = style.line_height;
        let total_w = col_w * c.columns.len() as f32;
        let free = rect.size().x - total_w;
        // はみ出すときは先頭(右端)の列を見せる。
        let left = if free < 0.0 {
            rect.max.x - total_w
        } else {
            rect.min.x
                + match style.align {
                    board_core::TextAlign::Left => 0.0,
                    board_core::TextAlign::Center => free * 0.5,
                    board_core::TextAlign::Right => free,
                }
        };
        let advance = style.font_size * VERTICAL_ADVANCE;
        let longest = c.columns.iter().map(|k| k.len).max().unwrap_or(0) as f32 * advance;
        let top = rect.min.y + s.tb.valign.offset(rect.size().y, longest);
        for (i, col) in c.columns.iter().enumerate() {
            let x = left + total_w - col_w * (i + 1) as f32;
            push(&col.buffer, glam::Vec2::new(x, top));
        }
    } else {
        let top = rect.min.y + s.tb.valign.offset(rect.size().y, c.height);
        push(&c.buffer, glam::Vec2::new(rect.min.x, top));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_wrap_by_paragraph_and_length() {
        let cols = split_columns("あいうえお\n「ー」", 2);
        let cols: Vec<String> = cols.iter().map(|c| c.iter().collect()).collect();
        assert_eq!(cols, ["あい", "うえ", "お", "┐ー", "└"]);
        assert_eq!(split_columns("", 3).len(), 1);
    }
}
