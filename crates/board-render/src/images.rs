//! 画像の GPU 描画。
//!
//! 画像要素のバイト列(PNG / JPEG)を初めて見えたときにデコードしてテクスチャにし、
//! 要素ごとにキャッシュする。描画は段(`Layer`)ごとに、図形の後・文字の前に行う。

use std::collections::HashMap;

use board_core::{Board, Camera, ElementId, Rect, ShapeKind};
use bytemuck::{Pod, Zeroable};

use crate::Layer;

/// テクスチャの一辺の上限(WebGL2 でも確実に使える大きさ)。
pub const MAX_TEXTURE_SIZE: u32 = 2048;

pub const IMAGES_WGSL: &str = include_str!("images.wgsl");

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct ImageInstance {
    pos: [f32; 2],
    size: [f32; 2],
}

struct Entry {
    key: (usize, usize),
    /// デコードに失敗した画像は `None`(何度も試さない)。
    bind: Option<wgpu::BindGroup>,
    used: bool,
}

pub struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    cache: HashMap<ElementId, Entry>,
    buf: wgpu::Buffer,
    cap: u64,
    /// 段ごとの描画(インスタンス番号と要素 ID)。
    draws: Vec<Vec<(u32, ElementId)>>,
}

/// RGBA 画像を、一辺が `MAX_TEXTURE_SIZE` 以下になるよう縮める。
pub fn fit_texture(img: image::RgbaImage) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    let m = w.max(h);
    if m <= MAX_TEXTURE_SIZE {
        return img;
    }
    let s = MAX_TEXTURE_SIZE as f32 / m as f32;
    let nw = ((w as f32 * s).round() as u32).max(1);
    let nh = ((h as f32 * s).round() as u32).max(1);
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

/// ミップマップ(半分ずつ縮めた画像の列)。
fn mip_chain(base: image::RgbaImage) -> Vec<image::RgbaImage> {
    let mut levels = vec![base];
    loop {
        let (w, h) = levels.last().unwrap().dimensions();
        if w == 1 && h == 1 {
            break;
        }
        let next = image::imageops::resize(
            levels.last().unwrap(),
            (w / 2).max(1),
            (h / 2).max(1),
            image::imageops::FilterType::Triangle,
        );
        levels.push(next);
    }
    levels
}

impl ImageLayer {
    pub fn new(
        device: &wgpu::Device,
        globals_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("images"),
            bind_group_layouts: &[Some(globals_layout), Some(&layout)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
        let pipeline = crate::gpu::make_pipeline(
            device,
            &pipeline_layout,
            format,
            "images",
            IMAGES_WGSL,
            &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ImageInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &attrs,
            })],
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let cap = 16;
        Self {
            pipeline,
            layout,
            sampler,
            cache: HashMap::new(),
            buf: make_buffer(device, cap),
            cap,
            draws: Vec::new(),
        }
    }

    fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bytes: &[u8],
    ) -> Option<wgpu::BindGroup> {
        let img = match image::load_from_memory(bytes) {
            Ok(i) => fit_texture(i.to_rgba8()),
            Err(e) => {
                log::warn!("画像を読めません: {e}");
                return None;
            }
        };
        let levels = mip_chain(img);
        let (w, h) = levels[0].dimensions();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (i, level) in levels.iter().enumerate() {
            let (lw, lh) = level.dimensions();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: i as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                level.as_raw(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(lw * 4),
                    rows_per_image: Some(lh),
                },
                wgpu::Extent3d {
                    width: lw,
                    height: lh,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        }))
    }

    /// 見えている画像を準備する(未デコードのものはここでテクスチャにする)。
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        board: &Board,
        camera: &Camera,
        layers: &[Layer],
    ) -> usize {
        self.draws.clear();
        self.draws.resize_with(layers.len(), Vec::new);
        for e in self.cache.values_mut() {
            e.used = false;
        }
        let (vmin, vmax) = camera.visible_world_rect();
        let view = Rect::new(vmin, vmax);
        let elements = board.elements();
        let mut instances = Vec::new();
        for (li, layer) in layers.iter().enumerate() {
            for e in &elements[layer.elements.clone()] {
                let Some(s) = e.as_shape().filter(|s| s.kind == ShapeKind::Image) else {
                    continue;
                };
                let Some(data) = &s.image else { continue };
                if !s.bounds().intersects(&view) {
                    continue;
                }
                let key = data.key();
                let stale = self.cache.get(&s.id).is_none_or(|c| c.key != key);
                if stale {
                    let bind = self.upload(device, queue, data.bytes());
                    self.cache.insert(
                        s.id,
                        Entry {
                            key,
                            bind,
                            used: true,
                        },
                    );
                }
                let entry = self.cache.get_mut(&s.id).unwrap();
                entry.used = true;
                if entry.bind.is_none() {
                    continue;
                }
                self.draws[li].push((instances.len() as u32, s.id));
                instances.push(ImageInstance {
                    pos: s.pos.to_array(),
                    size: s.size.to_array(),
                });
            }
        }
        self.cache.retain(|_, e| e.used);
        if instances.len() as u64 > self.cap {
            self.cap = (instances.len() as u64).next_power_of_two();
            self.buf = make_buffer(device, self.cap);
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.buf, 0, bytemuck::cast_slice(&instances));
        }
        instances.len()
    }

    /// `layer` 段目の画像を描く。呼んだ後は bind group 0 を戻すこと。
    pub fn render(&self, layer: usize, globals: &wgpu::BindGroup, pass: &mut wgpu::RenderPass<'_>) {
        let Some(draws) = self.draws.get(layer) else {
            return;
        };
        if draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, globals, &[]);
        pass.set_vertex_buffer(0, self.buf.slice(..));
        for (i, id) in draws {
            if let Some(bind) = self.cache.get(id).and_then(|e| e.bind.as_ref()) {
                pass.set_bind_group(1, bind, &[]);
                pass.draw(0..6, *i..*i + 1);
            }
        }
    }
}

fn make_buffer(device: &wgpu::Device, cap: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image instances"),
        size: cap * std::mem::size_of::<ImageInstance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_images_are_shrunk_and_mipped() {
        let img = image::RgbaImage::new(4096, 1024);
        let fit = fit_texture(img);
        assert_eq!(fit.dimensions(), (2048, 512));
        let chain = mip_chain(image::RgbaImage::new(8, 2));
        let dims: Vec<_> = chain.iter().map(|i| i.dimensions()).collect();
        assert_eq!(dims, vec![(8, 2), (4, 1), (2, 1), (1, 1)]);
    }
}
