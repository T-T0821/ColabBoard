use std::fmt;

use board_core::{Board, Camera, ElementId, Shape};

use crate::text::TextLayer;
use crate::{GRID_WGSL, Globals, Layer, SHAPES_WGSL, ShapeInstance, build_layers, changed_range};

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
    /// 描画先が使えず描画を飛ばしたか。
    pub skipped: bool,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    srgb_output: bool,
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
    /// GPU に送ってあるインスタンス列(差分転送のために覚えておく)。
    uploaded: Vec<ShapeInstance>,
    staging: Vec<ShapeInstance>,
    layers: Vec<Layer>,
    /// 直近の転送量(インスタンス数)。
    last_upload: usize,

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
            SHAPES_WGSL,
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
            uploaded: Vec::new(),
            staging: Vec::new(),
            layers: Vec::new(),
            last_upload: 0,
            overlay_buf,
            overlay_cap: 64,
            overlay_count: 0,
            text,
            images,
        })
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
    pub fn load_font(&mut self, data: Vec<u8>) {
        self.text.load_font(data);
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
        self.layers = build_layers(board.elements(), &mut self.staging);
        let needed = self.staging.len() as u64;
        let range = if needed > self.instance_cap {
            self.instance_cap = needed.next_power_of_two();
            self.instance_buf = make_instance_buffer(&self.device, self.instance_cap);
            Some(0..self.staging.len())
        } else {
            changed_range(&self.uploaded, &self.staging)
        };
        self.last_upload = 0;
        if let Some(r) = range
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
        self.images
            .prepare(&self.device, &self.queue, board, camera, &self.layers);
        self.text.prepare(
            &self.device,
            &self.queue,
            board,
            camera,
            &self.layers,
            text_skip,
        );

        let globals = Globals {
            view_proj: camera.view_proj().to_cols_array_2d(),
            origin: camera.origin.to_array(),
            viewport: camera.viewport.to_array(),
            zoom: camera.zoom,
            srgb_output: self.srgb_output as u32,
            _pad: [0; 2],
        };
        self.queue
            .write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));

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

            // 段ごとに「図形 → その段の文字」の順で描く(重なり順を保つ)。
            for (i, layer) in self.layers.iter().enumerate() {
                if !layer.instances.is_empty() {
                    // 文字描画が bind group 0 を差し替えるので毎回戻す。
                    pass.set_bind_group(0, &self.globals_bind, &[]);
                    pass.set_pipeline(&self.shape_pipeline);
                    pass.set_vertex_buffer(0, self.instance_buf.slice(..));
                    pass.draw(0..6, layer.instances.clone());
                }
                self.images.render(i, &self.globals_bind, &mut pass);
                self.text.render(i, &mut pass);
            }
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
            skipped: false,
        })
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
