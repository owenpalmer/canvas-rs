//! The paper background and the main view's vignette, drawn on the GPU.
//!
//! Paper: a mid-gray texture (paper.webp) blended onto the base color, as the old
//! CSS did it: light mode multiplies it in and lays a wash of the base color on top (less wash =
//! stronger texture); dark mode soft-lights it with no wash. The shader does the blend per pixel,
//! so switching themes just changes a few uniforms (and cross-fades them).
//!
//! Vignette: radial-gradient(ellipse 90% 80% at 50% 40%, transparent 55%, var(--vignette)).


use egui::{Color32, Rect};
use egui_wgpu::wgpu;

const PAPER_WEBP: &[u8] = include_bytes!("../../../assets/paper.webp");
/// CSS px per tile (the image is twice that).
pub const TILE: f32 = 720.0;
const SLOTS: usize = 8;

const SHADER: &str = r#"
struct U {
    base: vec4<f32>,
    params: vec4<f32>,   // wash, soft, tile size in px, mode (0 paper, 1 vignette)
    rect: vec4<f32>,     // x, y, w, h in px (vignette)
    vig: vec4<f32>,      // vignette color (premultiplied gamma rgba)
    extra: vec4<f32>,    // srgb target (x), opacity (y)
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn linear_from_gamma(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c < vec3<f32>(0.04045);
    let lower = c / vec3<f32>(12.92);
    let higher = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

fn soft_light(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let d = select(sqrt(cb), ((16.0 * cb - 12.0) * cb + 4.0) * cb, cb <= vec3<f32>(0.25));
    let low = cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    let high = cb + (2.0 * cs - 1.0) * (d - cb);
    return select(high, low, cs <= vec3<f32>(0.5));
}

fn out(c: vec4<f32>) -> vec4<f32> {
    if (u.extra.x > 0.5) {
        // premultiplied: convert the unpremultiplied color, keep alpha
        if (c.a <= 0.0) { return vec4<f32>(0.0); }
        let rgb = linear_from_gamma(c.rgb / c.a) * c.a;
        return vec4<f32>(rgb, c.a);
    }
    return c;
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let o = u.extra.y;
    if (u.params.w > 0.5) {
        let p = (pos.xy - u.rect.xy) / u.rect.zw;
        let d = vec2<f32>((p.x - 0.5) / 0.9, (p.y - 0.4) / 0.8);
        let t = length(d);
        let k = clamp((t - 0.55) / 0.45, 0.0, 1.0);
        return out(u.vig * k * o);
    }
    let g = textureSample(tex, samp, pos.xy / u.params.z).r;
    let base = u.base.rgb;
    let multiplied = mix(base * g, base, u.params.x);
    let soft = mix(soft_light(base, vec3<f32>(g)), base, u.params.x);
    let c = mix(multiplied, soft, u.params.y);
    return out(vec4<f32>(c * o, o));
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    base: [f32; 4],
    params: [f32; 4],
    rect: [f32; 4],
    vig: [f32; 4],
    extra: [f32; 4],
}

struct Resources {
    pipeline: wgpu::RenderPipeline,
    slots: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    srgb: bool,
}

/// Build the pipeline and upload the texture (once, when the renderer starts).
pub fn install(rs: &egui_wgpu::RenderState) {
    let device = &rs.device;
    let img = image::load_from_memory(PAPER_WEBP).map(|i| i.to_luma8()).unwrap_or_else(|_| image::GrayImage::from_pixel(4, 4, image::Luma([128])));
    let (w, h) = img.dimensions();
    let levels = (w.min(h) as f32).log2().floor() as u32 + 1;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("paper"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // Mip levels by 2x2 averaging, so the paper stays calm when drawn at 1x.
    let mut level = img;
    for mip in 0..levels {
        let (lw, lh) = level.dimensions();
        rs.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            level.as_raw(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw), rows_per_image: Some(lh) },
            wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
        );
        if lw <= 1 || lh <= 1 {
            break;
        }
        let (nw, nh) = ((lw / 2).max(1), (lh / 2).max(1));
        level = image::GrayImage::from_fn(nw, nh, |x, y| {
            let p = |dx: u32, dy: u32| level.get_pixel((x * 2 + dx).min(lw - 1), (y * 2 + dy).min(lh - 1)).0[0] as u32;
            image::Luma([((p(0, 0) + p(1, 0) + p(0, 1) + p(1, 1) + 2) / 4) as u8])
        });
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("paper"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("paper"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { multisampled: false, sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2 },
                count: None,
            },
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("paper"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("paper"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("paper"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), buffers: &[], compilation_options: Default::default() },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: rs.target_format,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
                    alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::OneMinusDstAlpha, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    let slots = (0..SLOTS)
        .map(|_| {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("paper uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("paper"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
                ],
            });
            (buf, bg)
        })
        .collect();
    rs.renderer.write().callback_resources.insert(Resources { pipeline, slots, srgb: rs.target_format.is_srgb() });
}

struct Draw {
    slot: usize,
    u: Uniforms,
}

impl egui_wgpu::CallbackTrait for Draw {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        res: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(r) = res.get::<Resources>() {
            let mut u = self.u;
            u.extra[0] = if r.srgb { 1.0 } else { 0.0 };
            queue.write_buffer(&r.slots[self.slot].0, 0, bytemuck::bytes_of(&u));
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, res: &egui_wgpu::CallbackResources) {
        if let Some(r) = res.get::<Resources>() {
            pass.set_pipeline(&r.pipeline);
            pass.set_bind_group(0, &r.slots[self.slot].1, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

fn gamma(c: Color32) -> [f32; 4] {
    let [r, g, b, a] = c.to_array();
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
}

/// Painter for the frame's paper draws (each draw uses its own uniform slot).
pub struct Paper {
    next: usize,
    pub ppp: f32,
}

impl Paper {
    pub fn new(ppp: f32) -> Paper {
        Paper { next: 0, ppp }
    }

    fn slot(&mut self) -> Option<usize> {
        (self.next < SLOTS).then(|| {
            self.next += 1;
            self.next - 1
        })
    }

    /// The paper over `rect`, tinted with `base` (the texture tiles from the window's origin).
    pub fn paper(&mut self, painter: &egui::Painter, rect: Rect, base: Color32, tokens: &crate::theme::Tokens, opacity: f32) {
        let Some(slot) = self.slot() else { return };
        let u = Uniforms {
            base: gamma(base),
            params: [tokens.paper_wash, tokens.paper_soft, TILE * self.ppp, 0.0],
            rect: [0.0; 4],
            vig: [0.0; 4],
            extra: [0.0, opacity, 0.0, 0.0],
        };
        painter.add(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect, Draw { slot, u })));
    }

    /// The vignette over `rect` (the gradient is sized to it).
    pub fn vignette(&mut self, painter: &egui::Painter, rect: Rect, color: Color32, opacity: f32) {
        let Some(slot) = self.slot() else { return };
        let p = self.ppp;
        let u = Uniforms {
            base: [0.0; 4],
            params: [0.0, 0.0, 1.0, 1.0],
            rect: [rect.min.x * p, rect.min.y * p, rect.width() * p, rect.height() * p],
            vig: gamma(color),
            extra: [0.0, opacity, 0.0, 0.0],
        };
        painter.add(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect, Draw { slot, u })));
    }
}

/// Whether the paper can be drawn (the wgpu renderer is there).
pub fn available(frame: &eframe::Frame) -> bool {
    frame.wgpu_render_state().is_some()
}
