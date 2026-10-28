use crate::*;
use std::borrow::Cow;
use wgpu::util::DeviceExt;
mod admission;
mod execute;
use admission::{Admission, Layout, Passes};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const CHUNK: usize = 65536;
const QUAD: [f32; 12] = [-1., -1., 1., -1., -1., 1., -1., 1., 1., -1., 1., 1.];

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    identity: Identity,
    limits: Limits,
    pipelines: Pipelines,
    sampler: wgpu::Sampler,
    nearest: wgpu::Sampler,
    vertices: wgpu::Buffer,
    targets: Option<Targets>,
    failed: bool,
}
struct Targets {
    layout: Layout,
    textures: [wgpu::Texture; 3],
    masks: Vec<wgpu::Texture>,
    effects: Vec<wgpu::Texture>,
    colors: std::collections::BTreeMap<[u32; 2], [wgpu::Texture; 2]>,
    readback: wgpu::Buffer,
    aligned: u32,
}
struct Pipelines {
    layer: wgpu::RenderPipeline,
    blend: wgpu::RenderPipeline,
    mask: wgpu::RenderPipeline,
    init: wgpu::RenderPipeline,
    step: wgpu::RenderPipeline,
    distance: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    color: wgpu::RenderPipeline,
}

fn gpu_error(error: impl std::fmt::Display) -> Error {
    Error::new("VISUAL_GPU_FAILED", error.to_string())
}
fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn blend_index(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Darken => 1,
        BlendMode::Multiply => 2,
        BlendMode::ColorBurn => 3,
        BlendMode::Lighten => 4,
        BlendMode::Screen => 5,
        BlendMode::PlusLighter => 6,
        BlendMode::ColorDodge => 7,
        BlendMode::Overlay => 8,
        BlendMode::SoftLight => 9,
        BlendMode::HardLight => 10,
        BlendMode::Difference => 11,
        BlendMode::Exclusion => 12,
        BlendMode::Hue => 13,
        BlendMode::Saturation => 14,
        BlendMode::Color => 15,
        BlendMode::Luminosity => 16,
    }
}
fn texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("prepared frame texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
fn pipeline(
    device: &wgpu::Device,
    vertex: &wgpu::ShaderModule,
    code: &'static str,
) -> wgpu::RenderPipeline {
    let fragment = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("captured compositor shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(code)),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("prepared frame pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: vertex,
            entry_point: Some("vertex_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                }],
            })],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &fragment,
            entry_point: Some("fragment_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl Renderer {
    /// The selected backend is exact. No software device or alternative API
    /// is silently substituted if this host cannot provide it.
    pub fn new(backend: Backend, limits: Limits) -> Result<Self> {
        if limits.max_frame_pixels == 0
            || limits.max_gpu_bytes == 0
            || limits.max_cpu_bytes < CHUNK as u64
            || limits.max_layers == 0
            || limits.max_layers > 3900
        {
            return Err(budget("Invalid native rendering admission"));
        }
        let backends = match backend {
            Backend::Metal => wgpu::Backends::METAL,
            Backend::Vulkan => wgpu::Backends::VULKAN,
            Backend::DirectX => wgpu::Backends::DX12,
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| Error::new("CAPABILITY_UNAVAILABLE", e.to_string()))?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err(Error::new(
                "CAPABILITY_UNAVAILABLE",
                "A native GPU is required by this execution",
            ));
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("OpenCut native compositor"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(gpu_error)?;
        let memory_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("captured fullscreen shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(shaders::VERTEX)),
        });
        let pipelines = Pipelines {
            layer: pipeline(&device, &vertex, shaders::LAYER),
            blend: pipeline(&device, &vertex, shaders::BLEND),
            mask: pipeline(&device, &vertex, shaders::MASK),
            init: pipeline(&device, &vertex, shaders::INIT),
            step: pipeline(&device, &vertex, shaders::STEP),
            distance: pipeline(&device, &vertex, shaders::DISTANCE),
            blur: pipeline(&device, &vertex, shaders::BLUR),
            color: pipeline(&device, &vertex, shaders::color()),
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear clamped source"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let nearest = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fullscreen vertices"),
            contents: &floats(&QUAD),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let validation = pollster::block_on(validation_scope.pop());
        let memory = pollster::block_on(memory_scope.pop());
        if let Some(error) = validation.or(memory) {
            device.destroy();
            return Err(gpu_error(error));
        }
        Ok(Self {
            device,
            queue,
            identity: Identity {
                backend: format!("{:?}", info.backend),
                adapter: info.name,
                driver: format!("{} {}", info.driver, info.driver_info),
                device: info.device,
                vendor: info.vendor,
                wgpu: "30.0.1",
                shaders_sha256: shaders::identity(),
            },
            limits,
            pipelines,
            sampler,
            nearest,
            vertices,
            targets: None,
            failed: false,
        })
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    fn texture_group(
        &self,
        pipeline: &wgpu::RenderPipeline,
        index: u32,
        texture: &wgpu::Texture,
    ) -> wgpu::BindGroup {
        self.sampled_group(pipeline, index, texture, &self.sampler)
    }
    fn sampled_group(
        &self,
        pipeline: &wgpu::RenderPipeline,
        index: u32,
        texture: &wgpu::Texture,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fixed texture binding"),
            layout: &pipeline.get_bind_group_layout(index),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &texture.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }
    fn uniform(
        &self,
        pipeline: &wgpu::RenderPipeline,
        index: u32,
        bytes: &[u8],
    ) -> wgpu::BindGroup {
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fixed uniform"),
                contents: bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fixed uniform binding"),
            layout: &pipeline.get_bind_group_layout(index),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        })
    }
    fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        clear: [f32; 4],
        pipeline: Option<&wgpu::RenderPipeline>,
        groups: &[&wgpu::BindGroup],
    ) {
        let view = target.create_view(&Default::default());
        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: clear[0] as f64,
                    g: clear[1] as f64,
                    b: clear[2] as f64,
                    a: clear[3] as f64,
                }),
                store: wgpu::StoreOp::Store,
            },
        })];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fixed composite pass"),
            color_attachments: &attachments,
            ..Default::default()
        });
        if let Some(pipeline) = pipeline {
            pass.set_pipeline(pipeline);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            for (index, group) in groups.iter().enumerate() {
                pass.set_bind_group(index as u32, *group, &[]);
            }
            pass.draw(0..6, 0..1);
        }
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        self.targets = None;
        self.device.destroy();
    }
}

// The authored background originally crossed an 8-bit premultiplied Canvas.
// Preserve that quantization explicitly; source pixels are already straight.
fn background_bytes(color: [f32; 4]) -> [u8; 4] {
    let alpha = (color[3] * 255.0).round() as u8;
    let component = |value: f32| {
        if alpha == 0 {
            0
        } else {
            (((value * alpha as f32).round() * 255.0 / alpha as f32).round()).clamp(0.0, 255.0)
                as u8
        }
    };
    [
        component(color[0]),
        component(color[1]),
        component(color[2]),
        alpha,
    ]
}
