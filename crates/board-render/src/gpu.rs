use std::fmt;

use board_core::{Board, Camera, ElementId, Shape, Vec2};

use crate::cull::{VisibleBlocks, clip_ranges};
use crate::text::TextLayer;
use crate::{
    GRID_WGSL, Globals, Layer, ShapeInstance, build_layers_masked, changed_range, shapes_wgsl,
};

#[derive(Debug)]
pub enum RenderError {
    NoAdapter(String),
    Device(String),
    Surface(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter(e) => write!(f, "GPU アダプタが見つかりません: {e}"),
            Self::Device(e) => write!(f, "GPU デバイスを作れません: {e}"),
            Self::Surface(e) => write!(f, "描画先を用意できません: {e}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// 1 フレームの統計。
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub instances: u32,
    /// このフレームでインスタンスバッファを転送したか。
    pub uploaded: bool,
    /// 転送したインスタンス数(変わった範囲だけ送る)。
    pub uploaded_instances: usize,
    /// 文字の重なり順のために分けた段の数。
    pub layers: usize,
    /// カリング後に頂点シェーダへ渡したインスタンス数。
    pub drawn: u32,
    /// 図形の描画命令の数。
    pub draw_calls: u32,
    /// 描画先が使えず描画を飛ばしたか。
    pub skipped: bool,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    srgb_output: bool,
    dark: bool,
    backend: wgpu::Backend,
    adapter_name: String,

    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    grid_pipeline: wgpu::RenderPipeline,
    shape_pipeline: wgpu::RenderPipeline,

    instance_buf: wgpu::Buffer,
    instance_cap: u64,
    instance_count: u32,
    uploaded_revision: Option<u64>,
    hidden: std::collections::HashSet<board_core::ElementId>,
    /// GPU に送ってあるインスタンス列(差分転送のために覚えておく)。
    uploaded: Vec<ShapeInstance>,
    staging: Vec<ShapeInstance>,
    layers: Vec<Layer>,
    /// 直近の転送量(インスタンス数)。
    last_upload: usize,
    /// 画面外のブロックを描画命令から外すための外接矩形。
    blocks: VisibleBlocks,
    /// ブロック単位のカリングを使うか(比較計測用に切れる)。
    block_culling: bool,

    /// 選択枠など毎フレーム変わる少数の図形。
    overlay_buf: wgpu::Buffer,
    overlay_cap: u64,
    overlay_count: u32,

    text: TextLayer,
    images: crate::images::ImageLayer,
}

impl Renderer {
    /// `surface` は `instance` から作ったもの。`width`/`height` は物理ピクセル。
    pub async fn new(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::NoAdapter(e.to_string()))?;
        let info = adapter.get_info();

        // WebGL2 でも動くよう控えめな上限で要求する。
        let required_limits =
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("colabboard"),
                required_features: wgpu::Features::empty(),
                required_limits,
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::Device(e.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        // 色は sRGB の値をそのまま扱いたいので、非 sRGB のフォーマットを優先する。
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| RenderError::Surface("対応フォーマットがありません".into()))?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps
                .alpha_modes
                .iter()
                .copied()
                .find(|m| *m == wgpu::CompositeAlphaMode::Opaque)
                .unwrap_or(caps.alpha_modes[0]),
            view_formats: vec![],
            color_space: Default::default(),
        };
        surface.configure(&device, &config);

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("board"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });

        let grid_pipeline = make_pipeline(&device, &layout, format, "grid", GRID_WGSL, &[], None);
        let shape_pipeline = make_pipeline(
            &device,
            &layout,
            format,
            "shapes",
            &shapes_wgsl(),
            &[Some(ShapeInstance::layout())],
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );

        let instance_cap = 1024;
        let instance_buf = make_instance_buffer(&device, instance_cap);
        let overlay_buf = make_instance_buffer(&device, 64);
        let text = TextLayer::new(&device, &queue, format);
        let images = crate::images::ImageLayer::new(&device, &globals_layout, format);

        Ok(Self {
            device,
            queue,
            surface,
            srgb_output: format.is_srgb(),
            dark: false,
            config,
            backend: info.backend,
            adapter_name: info.name,
            globals_buf,
            globals_bind,
            grid_pipeline,
            shape_pipeline,
            instance_buf,
            instance_cap,
            instance_count: 0,
            uploaded_revision: None,
            hidden: Default::default(),
            uploaded: Vec::new(),
            staging: Vec::new(),
            layers: Vec::new(),
            last_upload: 0,
            blocks: VisibleBlocks::default(),
            block_culling: true,
            overlay_buf,
            overlay_cap: 64,
            overlay_count: 0,
            text,
            images,
        })
    }

    /// ブロック単位のカリングの有無(既定は有効)。無効にすると段ごとに全インスタンスを描く。
    pub fn set_block_culling(&mut self, on: bool) {
        self.block_culling = on;
    }

    /// これまでに提出した描画を GPU が終えたら `f` を呼ぶ(計測用)。
    pub fn on_submitted_work_done(&self, f: impl FnOnce() + Send + 'static) {
        self.queue.on_submitted_work_done(f);
    }

    /// `"WebGPU"` / `"WebGL2"` など。
    pub fn backend_name(&self) -> String {
        match self.backend {
            wgpu::Backend::BrowserWebGpu => "WebGPU".to_string(),
            wgpu::Backend::Gl => "WebGL2".to_string(),
            other => format!("{other:?}"),
        }
    }

    /// GPU アダプタ名(ブラウザが隠す場合は空)。
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// 文字描画用のフォント(TTF/OTF)を読み込む。
    /// ダークモードの切り替え。次の `render` から反映される。
    /// 付箋の作者名を出すか。
    pub fn set_show_authors(&mut self, on: bool) {
        self.text.set_show_authors(on);
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        self.text.set_dark(dark);
    }

    pub fn dark(&self) -> bool {
        self.dark
    }

    pub fn load_font(&mut self, data: Vec<u8>) {
        self.text.load_font(data);
    }

    /// 構成図のアイコン(`Shape::icon` の名前)の画像を登録する。読めなければ `false`。
    pub fn register_icon(&mut self, name: &str, bytes: &[u8]) -> bool {
        self.images
            .register_icon(&self.device, &self.queue, name, bytes)
    }

    /// 個人作業エリアで隠す要素を設定する(変わったらインスタンス列を作り直す)。
    pub fn set_hidden(&mut self, hidden: &std::collections::HashSet<board_core::ElementId>) {
        if *hidden != self.hidden {
            self.hidden = hidden.clone();
            self.uploaded_revision = None;
        }
    }

    pub fn has_font(&self) -> bool {
        self.text.has_font()
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == self.size() {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// ボードが前回から変わっていればインスタンス列を作り直し、変わった範囲だけ送る。
    fn sync_instances(&mut self, board: &Board) -> bool {
        if self.uploaded_revision == Some(board.revision()) {
            return false;
        }
        self.layers = build_layers_masked(board.elements(), &self.hidden, &mut self.staging);
        let needed = self.staging.len() as u64;
        let range = if needed > self.instance_cap {
            self.instance_cap = needed.next_power_of_two();
            self.instance_buf = make_instance_buffer(&self.device, self.instance_cap);
            Some(0..self.staging.len())
        } else {
            changed_range(&self.uploaded, &self.staging)
        };
        self.last_upload = 0;
        if let Some(r) = range.clone()
            && !r.is_empty()
        {
            let offset = (r.start * std::mem::size_of::<ShapeInstance>()) as u64;
            self.last_upload = r.len();
            self.queue.write_buffer(
                &self.instance_buf,
                offset,
                bytemuck::cast_slice(&self.staging[r]),
            );
        }
        std::mem::swap(&mut self.uploaded, &mut self.staging);
        if let Some(r) = range {
            self.blocks.update(&self.uploaded, r);
        }
        self.instance_count = self.uploaded.len() as u32;
        self.uploaded_revision = Some(board.revision());
        true
    }

    fn sync_overlay(&mut self, overlay: &[Shape]) {
        let data: Vec<ShapeInstance> = overlay.iter().map(ShapeInstance::from).collect();
        let needed = data.len() as u64;
        if needed > self.overlay_cap {
            self.overlay_cap = needed.next_power_of_two();
            self.overlay_buf = make_instance_buffer(&self.device, self.overlay_cap);
        }
        if !data.is_empty() {
            self.queue
                .write_buffer(&self.overlay_buf, 0, bytemuck::cast_slice(&data));
        }
        self.overlay_count = data.len() as u32;
    }

    /// `overlay` は選択枠など、ボードの上に重ねる図形。
    /// `text_skip` は文字を別の入力欄で表示している(編集中の)要素。
    pub fn render(
        &mut self,
        board: &Board,
        camera: &Camera,
        overlay: &[Shape],
        text_skip: Option<ElementId>,
    ) -> Result<FrameStats, RenderError> {
        let uploaded = self.sync_instances(board);
        self.sync_overlay(overlay);
        self.images.prepare(
            &self.device,
            &self.queue,
            board,
            camera,
            &self.layers,
            &self.hidden,
        );
        self.text.prepare(
            &self.device,
            &self.queue,
            board,
            camera,
            &self.layers,
            text_skip,
            &self.hidden,
        );

        self.write_globals(camera, self.dark);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(FrameStats {
                    skipped: true,
                    uploaded,
                    ..Default::default()
                });
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(FrameStats {
                    skipped: true,
                    uploaded,
                    ..Default::default()
                });
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RenderError::Surface("描画先の取得に失敗しました".into()));
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let (drawn, draw_calls);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("board"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_bind_group(0, &self.globals_bind, &[]);

            pass.set_pipeline(&self.grid_pipeline);
            pass.draw(0..3, 0..1);

            // 画面と重なるブロックだけを描く(描画順はそのまま)。
            let visible = if self.block_culling {
                self.blocks.visible_ranges(camera).to_vec()
            } else {
                std::iter::once(0..self.instance_count).collect()
            };
            let (d, c) = self.draw_layers(&mut pass, &visible);
            drawn = d;
            draw_calls = c;
            if self.overlay_count > 0 {
                pass.set_bind_group(0, &self.globals_bind, &[]);
                pass.set_pipeline(&self.shape_pipeline);
                pass.set_vertex_buffer(0, self.overlay_buf.slice(..));
                pass.draw(0..6, 0..self.overlay_count);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        self.text.trim();

        Ok(FrameStats {
            instances: self.instance_count,
            uploaded,
            uploaded_instances: if uploaded { self.last_upload } else { 0 },
            layers: self.layers.len(),
            drawn,
            draw_calls,
            skipped: false,
        })
    }
}

impl Renderer {
    /// 読み戻し待ちの処理を進める(書き出し中は毎フレーム呼ぶ)。
    pub fn poll(&self) {
        let _ = self.device.poll(wgpu::PollType::Poll);
    }

    fn write_globals(&self, camera: &Camera, dark: bool) {
        let globals = Globals {
            view_proj: camera.view_proj().to_cols_array_2d(),
            origin: camera.origin.to_array(),
            viewport: camera.viewport.to_array(),
            zoom: camera.zoom,
            srgb_output: self.srgb_output as u32,
            dark: dark as u32,
            _pad: 0,
        };
        self.queue
            .write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));
    }

    /// 段ごとに「図形 → 画像 → その段の文字」の順で描く(重なり順を保つ)。
    /// 段ごとに「図形 → その段の画像と文字」の順で描く(重なり順を保つ)。
    /// 図形は `visible`(昇順のインスタンス範囲)に入るものだけ描く。戻り値は(描いた数, 描画命令の数)。
    fn draw_layers(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        visible: &[std::ops::Range<u32>],
    ) -> (u32, u32) {
        let (mut drawn, mut calls) = (0, 0);
        for (i, layer) in self.layers.iter().enumerate() {
            let mut bound = false;
            for r in clip_ranges(visible, layer.instances.clone()) {
                if !bound {
                    // 文字描画が bind group 0 を差し替えるので毎回戻す。
                    pass.set_bind_group(0, &self.globals_bind, &[]);
                    pass.set_pipeline(&self.shape_pipeline);
                    pass.set_vertex_buffer(0, self.instance_buf.slice(..));
                    bound = true;
                }
                drawn += r.end - r.start;
                calls += 1;
                pass.draw(0..6, r);
            }
            self.images.render(i, &self.globals_bind, pass);
            self.text.render(i, pass);
        }
        (drawn, calls)
    }

    /// ボードのワールド座標の範囲 `area` を、長辺が `max_px` 以下の画像に描く(背景は白、
    /// 方眼・選択枠なし)。結果は [`Capture::read`] で受け取る。
    pub fn capture(&mut self, board: &Board, area: board_core::Rect, max_px: u32) -> Capture {
        let limit = self.device.limits().max_texture_dimension_2d;
        let max_px = max_px.min(limit).max(1) as f32;
        let size = area.size().max(Vec2::splat(1.0));
        // 拡大しすぎない(元の 2 倍まで)。
        let scale = (max_px / size.max_element()).min(2.0);
        let width = ((size.x * scale).ceil() as u32).clamp(1, max_px as u32);
        let height = ((size.y * scale).ceil() as u32).clamp(1, max_px as u32);
        let mut camera = Camera::new(Vec2::new(width as f32, height as f32));
        camera.zoom = scale;
        camera.origin = area.min;

        self.sync_instances(board);
        self.images.prepare(
            &self.device,
            &self.queue,
            board,
            &camera,
            &self.layers,
            &self.hidden,
        );
        self.text.set_dark(false);
        self.text.prepare(
            &self.device,
            &self.queue,
            board,
            &camera,
            &self.layers,
            None,
            &self.hidden,
        );
        self.text.set_dark(self.dark);
        // 書き出しはダークモードでも白背景の見た目にする。
        self.write_globals(&camera, false);

        let format = self.config.format;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let padded_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("capture"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("capture"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let all: Vec<_> = std::iter::once(0..self.instance_count).collect();
            self.draw_layers(&mut pass, &all);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        // 次の通常の描画で、画面用に文字などを準備し直させる。
        self.uploaded_revision = None;
        Capture {
            device: self.device.clone(),
            buffer,
            width,
            height,
            padded_row,
            bgra: matches!(
                format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            ),
        }
    }
}

/// [`Renderer::capture`] の結果(GPU からの読み戻し待ち)。
pub struct Capture {
    device: wgpu::Device,
    buffer: wgpu::Buffer,
    pub width: u32,
    pub height: u32,
    padded_row: u32,
    bgra: bool,
}

impl Capture {
    /// 読み戻しが終わったら、RGBA(行の詰め物なし)を `done` に渡す。
    pub fn read(self, done: impl FnOnce(Option<Vec<u8>>) + wgpu::WasmNotSend + 'static) {
        let Capture {
            device,
            buffer,
            width,
            height,
            padded_row,
            bgra,
        } = self;
        let buf = buffer.clone();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            if r.is_err() {
                done(None);
                return;
            }
            let mut out = Vec::with_capacity((width * height * 4) as usize);
            {
                let Ok(data) = buf.slice(..).get_mapped_range() else {
                    done(None);
                    return;
                };
                for row in data.chunks(padded_row as usize).take(height as usize) {
                    out.extend_from_slice(&row[..(width * 4) as usize]);
                }
            }
            buf.unmap();
            if bgra {
                for px in out.as_chunks_mut::<4>().0.iter_mut() {
                    px.swap(0, 2);
                }
            }
            done(Some(out));
        });
        // ネイティブではここで完了する。ブラウザでは待てないので、終わるまで
        // `Renderer::poll` を呼び続ける(WebGPU ではイベントループから呼ばれる)。
        #[cfg(not(target_arch = "wasm32"))]
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        #[cfg(target_arch = "wasm32")]
        let _ = device;
    }
}

fn make_instance_buffer(device: &wgpu::Device, cap: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("shape instances"),
        size: cap * std::mem::size_of::<ShapeInstance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(crate) fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    label: &str,
    source: &str,
    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers,
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
