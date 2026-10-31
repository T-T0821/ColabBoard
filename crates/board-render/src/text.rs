//! 図形内の文字の GPU 描画(glyphon / cosmic-text)。
//!
//! 文字はワールド単位でレイアウトし、表示倍率は glyphon の `scale` で掛ける。
//! レイアウト結果は要素ごとにキャッシュし、文字か幅が変わったときだけ組み直す。

use std::collections::HashMap;

use board_core::{Board, Camera, ElementId, Rect, Shape};

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
    /// 前回の prepare で使ったか(使われなくなったものを捨てる)。
    used: bool,
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
    buffers: HashMap<ElementId, Cached>,
    has_font: bool,
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

    /// 見えている図形の文字を準備する。`skip` は編集中で別の欄に表示している要素。
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        board: &Board,
        camera: &Camera,
        layers: &[Layer],
        skip: Option<ElementId>,
    ) -> usize {
        self.prepared.clear();
        self.prepared.resize(layers.len(), false);
        if !self.has_font {
            return 0;
        }
        let (view_min, view_max) = camera.visible_world_rect();
        let view = Rect::new(view_min, view_max);

        // 見えていて、文字があり、十分な大きさで表示される図形を集める(要素の並び順)。
        let visible: Vec<(usize, &Shape)> = board
            .elements()
            .iter()
            .enumerate()
            .filter_map(|(i, e)| Some((i, e.as_shape()?)))
            .filter(|(_, s)| !s.text.is_empty() && Some(s.id) != skip)
            .filter(|(_, s)| s.text_style().font_size * camera.zoom >= MIN_SCREEN_FONT_PX)
            .filter(|(_, s)| s.text_clip().intersects(&view))
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
            ..
        } = self;

        for (_, s) in &visible {
            let style = s.text_style();
            let width = s.text_rect().size().x.max(1.0);
            let entry = buffers.entry(s.id).or_insert_with(|| Cached {
                text: String::new(),
                width: -1.0,
                font_size: 0.0,
                align: style.align,
                buffer: Buffer::new(
                    font_system,
                    Metrics::new(style.font_size, style.line_height),
                ),
                height: 0.0,
                used: false,
            });
            entry.used = true;
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
                    &s.text,
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
                entry.text.clone_from(&s.text);
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

        let area = |s: &Shape| {
            let c = buffers.get(&s.id)?;
            let rect = s.text_rect();
            // 縦方向も中央に寄せる(はみ出す場合は上詰め)。
            let top = rect.min.y + ((rect.size().y - c.height) * 0.5).max(0.0);
            let origin = camera.world_to_screen(glam::Vec2::new(rect.min.x, top));
            let clip = s.text_clip();
            let clip_min = camera.world_to_screen(clip.min);
            let clip_max = camera.world_to_screen(clip.max);
            let [r, g, b, a] = s.text_style().color.0;
            Some(TextArea {
                buffer: &c.buffer,
                left: origin.x,
                top: origin.y,
                scale: camera.zoom,
                bounds: TextBounds {
                    left: clip_min.x.floor() as i32,
                    top: clip_min.y.floor() as i32,
                    right: clip_max.x.ceil() as i32,
                    bottom: clip_max.y.ceil() as i32,
                },
                default_color: Color::rgba(r, g, b, a),
                custom_glyphs: &[],
            })
        };

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
            let areas = mine.iter().filter_map(|(_, s)| area(s));
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
