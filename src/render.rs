//! Native NV12/P010 plane imports. Like Switchblade, conversion and drawing stay on wgpu.
//! CVMetalTexture imports avoid a CPU pixel copy or CPU YUV-to-RGB conversion.
use crate::media::{Matrix, VideoFrame};
use anyhow::{Result, ensure};
use eframe::egui;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{MTLPixelFormat, MTLTexture, MTLTextureType};
use std::{collections::HashMap, ffi::c_void, ptr, sync::Arc};
use wgpu::util::DeviceExt;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVMetalTextureCacheCreate(
        allocator: *const c_void,
        attributes: *const c_void,
        device: *mut c_void,
        texture_attributes: *const c_void,
        cache: *mut *mut c_void,
    ) -> i32;
    fn CVMetalTextureCacheCreateTextureFromImage(
        allocator: *const c_void,
        cache: *mut c_void,
        image: *mut c_void,
        attributes: *const c_void,
        format: usize,
        width: usize,
        height: usize,
        plane: usize,
        texture: *mut *mut c_void,
    ) -> i32;
    fn CVMetalTextureGetTexture(texture: *mut c_void) -> *mut ProtocolObject<dyn MTLTexture>;
    fn CVMetalTextureCacheFlush(cache: *mut c_void, flags: u64);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}

struct CfObject(*mut c_void);
// These CoreVideo cache/texture objects support use and release across threads. The cache
// is used only during serialized render preparation; texture drop guards can run on wgpu threads.
unsafe impl Send for CfObject {}
unsafe impl Sync for CfObject {}
impl Drop for CfObject {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

struct Picture {
    frame: Arc<VideoFrame>,
    bind_group: wgpu::BindGroup,
}

pub struct VideoRenderer {
    cache: CfObject,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    pictures: HashMap<usize, Picture>,
    pub errors: HashMap<usize, String>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Conversion {
    levels: [f32; 4],
    matrix: [f32; 4],
}

impl VideoRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Result<Self> {
        // SAFETY: we require the Metal backend. The retained MTLDevice remains valid throughout
        // cache creation; CoreVideo retains it for the lifetime of the cache.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or_else(|| anyhow::anyhow!("CamCam requires the Metal backend"))?;
        let mut cache = ptr::null_mut();
        let result = unsafe {
            CVMetalTextureCacheCreate(
                ptr::null(),
                ptr::null(),
                Retained::as_ptr(hal.raw_device()).cast_mut().cast(),
                ptr::null(),
                &mut cache,
            )
        };
        ensure!(
            result == 0 && !cache.is_null(),
            "Could not create Metal video texture cache"
        );
        let cache = CfObject(cache);
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera YUV planes"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("camera video"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("camera YUV conversion"),
            source: wgpu::ShaderSource::Wgsl(include_str!("video.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("camera video"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("camera video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            cache,
            pipeline,
            layout,
            sampler,
            format,
            pictures: HashMap::new(),
            errors: HashMap::new(),
        })
    }

    fn import_plane(
        &self,
        device: &wgpu::Device,
        frame: &Arc<VideoFrame>,
        plane: usize,
    ) -> Result<wgpu::TextureView> {
        let (metal_format, format) = match (frame.ten_bit, plane) {
            (false, 0) => (MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm),
            (false, _) => (MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm),
            (true, 0) => (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
            (true, _) => (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
        };
        let (width, height) = if plane == 0 {
            (frame.width, frame.height)
        } else {
            (frame.width.div_ceil(2), frame.height.div_ceil(2))
        };
        let mut native = ptr::null_mut();
        // SAFETY: the frame owns its IOSurface. The cache uses the same MTLDevice as wgpu.
        let result = unsafe {
            CVMetalTextureCacheCreateTextureFromImage(
                ptr::null(),
                self.cache.0,
                frame.buffer(),
                ptr::null(),
                metal_format.0,
                width as usize,
                height as usize,
                plane,
                &mut native,
            )
        };
        ensure!(
            result == 0 && !native.is_null(),
            "Could not import camera plane into Metal ({result})"
        );
        let native = CfObject(native);
        // SAFETY: CoreVideo returns a borrowed MTLTexture. Retain it before passing to wgpu.
        let metal = unsafe { Retained::retain(CVMetalTextureGetTexture(native.0)) }
            .ok_or_else(|| anyhow::anyhow!("CoreVideo returned no Metal texture"))?;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let frame = frame.clone();
        // SAFETY: both APIs use the same device, format, plane extent, and initialized
        // IOSurface. The hal drop callback keeps the CV texture AND decoded pixel buffer
        // alive until wgpu is finished using this texture, including in-flight GPU work.
        let texture = unsafe {
            let hal = wgpu::hal::metal::Device::texture_from_raw(
                metal,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                extent.into(),
                Some(Box::new(move || {
                    drop(native);
                    drop(frame);
                })),
            );
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some("camera IOSurface plane"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        };
        Ok(texture.create_view(&Default::default()))
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        id: usize,
        frame: Arc<VideoFrame>,
    ) -> Result<()> {
        if self
            .pictures
            .get(&id)
            .is_some_and(|p| Arc::ptr_eq(&p.frame, &frame))
        {
            return Ok(());
        }
        let y = self.import_plane(device, &frame, 0)?;
        let uv = self.import_plane(device, &frame, 1)?;
        let (kr, kb) = match frame.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020 => (0.2627, 0.0593),
        };
        let (maximum, multiplier) = if frame.ten_bit {
            (65535.0, 256.0)
        } else {
            (255.0, 1.0)
        };
        let levels = if frame.full_range {
            [0.0, 1.0, 128.0 * multiplier / maximum, 1.0]
        } else {
            [
                16.0 * multiplier / maximum,
                maximum / (219.0 * multiplier),
                128.0 * multiplier / maximum,
                maximum / (224.0 * multiplier),
            ]
        };
        let conversion = Conversion {
            levels,
            matrix: [kr, kb, self.format.is_srgb() as u8 as f32, 0.0],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera color"),
            contents: bytemuck::bytes_of(&conversion),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera video"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        self.pictures.insert(id, Picture { frame, bind_group });
        self.errors.remove(&id);
        unsafe { CVMetalTextureCacheFlush(self.cache.0, 0) };
        Ok(())
    }

    pub fn retain(&mut self, active: &[usize]) {
        self.pictures.retain(|id, _| active.contains(id));
        self.errors.retain(|id, _| active.contains(id));
    }

    pub fn paint(&self, id: usize, pass: &mut wgpu::RenderPass<'static>) {
        if let Some(picture) = self.pictures.get(&id) {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &picture.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

pub struct VideoCallback {
    pub id: usize,
    pub frame: Arc<VideoFrame>,
}

impl CallbackTrait for VideoCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _: &wgpu::Queue,
        _: &ScreenDescriptor,
        _: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let renderer = resources
            .get_mut::<VideoRenderer>()
            .expect("video renderer");
        if let Err(error) = renderer.prepare(device, self.id, self.frame.clone()) {
            renderer.pictures.remove(&self.id);
            renderer.errors.insert(self.id, error.to_string());
        }
        Vec::new()
    }

    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        resources
            .get::<VideoRenderer>()
            .expect("video renderer")
            .paint(self.id, pass);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn gpu() -> egui_wgpu::RenderState {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
            ..Default::default()
        }))
        .unwrap();
        let target_format = wgpu::TextureFormat::Rgba8Unorm;
        let renderer = egui_wgpu::Renderer::new(&device, target_format, Default::default());
        egui_wgpu::RenderState {
            adapter,
            available_adapters: Vec::new(),
            instance,
            device,
            queue,
            target_format,
            renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
            surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
        }
    }

    pub(crate) fn readback(
        state: &egui_wgpu::RenderState,
        width: u32,
        height: u32,
        paint: impl FnOnce(&wgpu::TextureView, &mut wgpu::CommandEncoder),
    ) -> Vec<u8> {
        let texture = state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("verification image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: state.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let stride = (width * 4).div_ceil(256) * 256;
        let buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("verification readback"),
            size: stride as u64 * height as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = state.device.create_command_encoder(&Default::default());
        paint(&texture.create_view(&Default::default()), &mut encoder);
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        state.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap();
            });
        state
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        receiver.recv().unwrap().unwrap();
        let mapped = buffer.slice(..).get_mapped_range().unwrap();
        let data = mapped
            .chunks(stride as usize)
            .flat_map(|row| row[..width as usize * 4].to_vec())
            .collect();
        drop(mapped);
        buffer.unmap();
        data
    }

    #[test]
    #[ignore = "requires scripts/test-live.sh fixtures and macOS GPU access"]
    fn native_import_and_gpu_conversion_preserve_color_bars() {
        let state = gpu();
        let mut renderer = VideoRenderer::new(&state.device, state.target_format).unwrap();
        for (path, ten_bit) in [("sub", false), ("hevc10", true)] {
            let mut decoded = None;
            crate::media::decode(
                &url::Url::parse(&format!("rtsp://127.0.0.1:18554/{path}")).unwrap(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                |frame| {
                    decoded = Some(frame);
                    false
                },
            )
            .unwrap();
            let frame = decoded.unwrap();
            assert_eq!(frame.ten_bit, ten_bit);
            let (width, height) = (frame.width, frame.height);
            renderer.prepare(&state.device, 0, frame).unwrap();
            let pixels = readback(&state, width, height, |view, encoder| {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    })
                    .forget_lifetime();
                renderer.paint(0, &mut pass);
            });
            let expected = [
                [191, 191, 191],
                [191, 191, 0],
                [0, 191, 191],
                [0, 191, 0],
                [191, 0, 191],
                [191, 0, 0],
                [0, 0, 191],
            ];
            for (bar, expected) in expected.into_iter().enumerate() {
                let x = (bar as u32 * 2 + 1) * width / 14;
                let offset = ((height / 3 * width + x) * 4) as usize;
                for channel in 0..3 {
                    assert!(
                        (pixels[offset + channel] as i32 - expected[channel]).abs() < 24,
                        "bar {bar}: {:?}",
                        &pixels[offset..offset + 3]
                    );
                }
            }
        }
    }
}
