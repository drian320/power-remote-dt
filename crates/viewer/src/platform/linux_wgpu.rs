//! P3 — opt-in wgpu GPU presenter for the Linux viewer.
//!
//! Performs YUV→RGB color conversion in a fragment shader instead of the
//! per-pixel CPU loops in `linux.rs`. Selected at runtime via
//! `PRDT_LINUX_RENDERER=wgpu`; softbuffer stays the default and any init
//! error here makes `build_render` fall back to it.
//!
//! The shader math mirrors the CPU golden-reference converters
//! (`i420_to_bgra` / `nv12_to_bgra` BT.709, `p010_to_bgra_sdr_tonemap`
//! BT.2020→PQ→Reinhard(1000)→sRGB). Exact bit-match isn't required for the
//! GPU path; the golden tests only guard the untouched CPU path.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use winit::window::Window;

use super::linux::PlatformFrame;

/// Selects which fragment-shader conversion the pipeline runs. Pushed via
/// a small uniform so a single pipeline serves every PlatformFrame variant.
#[repr(u32)]
#[derive(Clone, Copy)]
enum ConvMode {
    /// I420 (three R8 planes) BT.709, matches `i420_to_bgra`.
    I420 = 0,
    /// NV12 (R8 Y + Rg8 UV) BT.709, matches `nv12_to_bgra`. Gated to match
    /// the `PlatformFrame::Nv12` construction site so the variant isn't
    /// dead code in feature-less builds.
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    Nv12 = 1,
    /// P010 (R16Uint Y + Rg16Uint UV) BT.2020→PQ→Reinhard, matches
    /// `p010_to_bgra_sdr_tonemap`.
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any"
    ))]
    P010 = 2,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mode: u32,
    _pad: [u32; 3],
}

/// Reusable per-frame plane textures, lazily (re)created when the stream
/// dimensions or sample layout change.
struct PlaneTextures {
    width: u32,
    height: u32,
    /// `true` while the textures are sized for an 8-bit (I420/NV12) frame,
    /// `false` for a 16-bit (P010) frame — the formats differ so a layout
    /// switch forces a rebuild.
    is_8bit: bool,
    y: wgpu::Texture,
    /// Second plane: U (I420) or interleaved UV (NV12/P010).
    uv: wgpu::Texture,
    /// Third plane: V (I420 only); a 1x1 dummy otherwise.
    v: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// wgpu GPU presenter state.
pub struct WgpuRender {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buf: wgpu::Buffer,
    /// 1x1 placeholder views so the YUV bind group can satisfy both the
    /// 8-bit (float) and 16-bit (uint) texture bindings regardless of the
    /// active conversion mode. The shader only samples the bindings its
    /// `mode` selects, so the unused placeholders are never read.
    dummy_f8_view: wgpu::TextureView,
    dummy_u16_view: wgpu::TextureView,
    dummy_u16_uv_view: wgpu::TextureView,
    planes: Option<PlaneTextures>,
    /// RGBA cursor overlay (premultiplied-over via alpha blending).
    cursor_pipeline: wgpu::RenderPipeline,
    cursor_bind_group_layout: wgpu::BindGroupLayout,
    cursor_sampler: wgpu::Sampler,
    cursor_rect_buf: wgpu::Buffer,
    cursor: Option<CursorTexture>,
}

struct CursorTexture {
    bind_group: wgpu::BindGroup,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CursorRect {
    /// Normalised device rect: x0, y0, x1, y1 in clip space.
    rect: [f32; 4],
}

impl WgpuRender {
    /// Create the GPU presenter from a winit window. Any failure returns
    /// `RenderError::Init` so `build_render` can fall back to softbuffer.
    pub fn new(window: Arc<Window>, width: u32, height: u32) -> Result<Self, super::RenderError> {
        let instance = wgpu::Instance::default();

        // SAFETY: `window` is an `Arc<Window>` kept alive for the whole
        // lifetime of this `WgpuRender` (stored in `self.window`), so the
        // surface created from its raw handles never outlives the window.
        // winit's `Window` implements `HasWindowHandle + HasDisplayHandle`
        // (raw-window-handle 0.6), satisfying `create_surface`.
        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|e| super::RenderError::Init(format!("wgpu create_surface: {e}")))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|e| super::RenderError::Init(format!("wgpu request_adapter: {e}")))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("prdt-viewer-wgpu"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| super::RenderError::Init(format!("wgpu request_device: {e}")))?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer a non-sRGB Bgra8Unorm to match the CPU BGRA output; fall
        // back to the surface's first preferred format otherwise.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| super::RenderError::Init("wgpu: no surface formats".to_string()))?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: caps
                .present_modes
                .iter()
                .copied()
                .find(|m| *m == wgpu::PresentMode::Fifo)
                .unwrap_or(caps.present_modes[0]),
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("prdt-yuv-shader"),
            source: wgpu::ShaderSource::Wgsl(YUV_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("prdt-yuv-bgl"),
            entries: &[
                plane_entry(0, false),
                plane_entry(1, false),
                plane_entry(2, false),
                // u16 (P010) Y plane sampled as a uint texture.
                plane_entry(3, true),
                plane_entry(4, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
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
            label: Some("prdt-yuv-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("prdt-yuv-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("prdt-yuv-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prdt-yuv-uniform"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Cursor overlay pipeline (RGBA texture, alpha blended) ──────────
        let cursor_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("prdt-cursor-shader"),
            source: wgpu::ShaderSource::Wgsl(CURSOR_SHADER.into()),
        });

        let cursor_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("prdt-cursor-bgl"),
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let cursor_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("prdt-cursor-pl"),
                bind_group_layouts: &[&cursor_bind_group_layout],
                push_constant_ranges: &[],
            });

        let cursor_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("prdt-cursor-pipeline"),
            layout: Some(&cursor_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &cursor_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &cursor_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        // Straight-alpha over operator: matches the CPU
                        // `alpha_blend_bgra` (dst = src*a + dst*(1-a)).
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::OVER,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let cursor_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("prdt-cursor-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let cursor_rect_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prdt-cursor-rect"),
            size: std::mem::size_of::<CursorRect>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mk_dummy = |fmt: wgpu::TextureFormat, label: &str| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: fmt,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let dummy_f8_view = mk_dummy(wgpu::TextureFormat::R8Unorm, "prdt-dummy-f8");
        let dummy_u16_view = mk_dummy(wgpu::TextureFormat::R16Uint, "prdt-dummy-u16");
        let dummy_u16_uv_view = mk_dummy(wgpu::TextureFormat::Rg16Uint, "prdt-dummy-u16uv");

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buf,
            dummy_f8_view,
            dummy_u16_view,
            dummy_u16_uv_view,
            planes: None,
            cursor_pipeline,
            cursor_bind_group_layout,
            cursor_sampler,
            cursor_rect_buf,
            cursor: None,
        })
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// Upload one decoded frame's planes and draw it (plus the cursor).
    pub fn present_frame(
        &mut self,
        f: &PlatformFrame,
        shared: &crate::ViewerShared,
    ) -> Result<(), super::RenderError> {
        let (stream_w, stream_h, mode) = match f {
            PlatformFrame::I420(i420) => {
                self.ensure_planes(i420.width, i420.height, true, false);
                self.upload_i420(i420);
                (i420.width, i420.height, ConvMode::I420)
            }
            #[cfg(any(
                feature = "ffmpeg-decode-hevc-sw-any",
                feature = "ffmpeg-decode-hevc-vaapi-any",
                feature = "ffmpeg-decode-hevc-nvdec-any"
            ))]
            PlatformFrame::Nv12(nv12) => {
                self.ensure_planes(nv12.width, nv12.height, true, true);
                self.upload_nv12(nv12);
                (nv12.width, nv12.height, ConvMode::Nv12)
            }
            #[cfg(any(
                feature = "ffmpeg-decode-hevc-sw-main10-any",
                feature = "ffmpeg-decode-hevc-vaapi-main10-any",
                feature = "ffmpeg-decode-hevc-nvdec-main10-any"
            ))]
            PlatformFrame::Nv12_10(nv12_10) => {
                self.ensure_planes(nv12_10.width, nv12_10.height, false, true);
                self.upload_p010(nv12_10);
                (nv12_10.width, nv12_10.height, ConvMode::P010)
            }
        };

        self.queue.write_buffer(
            &self.uniform_buf,
            0,
            bytemuck::bytes_of(&Uniforms {
                mode: mode as u32,
                _pad: [0; 3],
            }),
        );

        self.update_cursor(shared, stream_w, stream_h);

        let frame = self
            .surface
            .get_current_texture()
            .map_err(|e| super::RenderError::Present(format!("wgpu get_current_texture: {e}")))?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("prdt-present"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prdt-present-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(planes) = &self.planes {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &planes.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            if let Some(cursor) = &self.cursor {
                pass.set_pipeline(&self.cursor_pipeline);
                pass.set_bind_group(0, &cursor.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// (Re)create the plane textures + bind group when the stream size or
    /// 8-/16-bit layout changes.
    fn ensure_planes(&mut self, width: u32, height: u32, is_8bit: bool, interleaved_uv: bool) {
        if let Some(p) = &self.planes {
            if p.width == width && p.height == height && p.is_8bit == is_8bit {
                return;
            }
        }

        let cw = width.div_ceil(2);
        let ch = height.div_ceil(2);

        let (y_fmt, c_fmt, v_fmt) = if is_8bit {
            // I420: Y=R8, U=R8, V=R8. NV12: Y=R8, UV=Rg8, V=dummy.
            (
                wgpu::TextureFormat::R8Unorm,
                if interleaved_uv {
                    wgpu::TextureFormat::Rg8Unorm
                } else {
                    wgpu::TextureFormat::R8Unorm
                },
                wgpu::TextureFormat::R8Unorm,
            )
        } else {
            // P010: Y=R16Uint, UV=Rg16Uint, V=dummy.
            (
                wgpu::TextureFormat::R16Uint,
                wgpu::TextureFormat::Rg16Uint,
                wgpu::TextureFormat::R16Uint,
            )
        };

        let device = &self.device;
        let mk = |label: &str, w: u32, h: u32, fmt: wgpu::TextureFormat| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w.max(1),
                    height: h.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };

        let y = mk("prdt-plane-y", width, height, y_fmt);
        // Chroma planes are at half resolution. For NV12/P010 the UV plane
        // holds cw interleaved samples per row; for I420 the U/V planes hold
        // cw single samples per row. Either way the texture is cw x ch.
        let uv = mk("prdt-plane-uv", cw, ch, c_fmt);
        let v = mk("prdt-plane-v", cw, ch, v_fmt);

        let yv = y.create_view(&wgpu::TextureViewDescriptor::default());
        let uvv = uv.create_view(&wgpu::TextureViewDescriptor::default());
        let vv = v.create_view(&wgpu::TextureViewDescriptor::default());

        // The bind group must satisfy every binding for both 8-bit and
        // 16-bit pipeline modes. Bindings 0-2 are the 8-bit (float) views;
        // 3-4 are the 16-bit (uint) views. We bind the active layout's real
        // textures and reuse them as harmless placeholders for the inactive
        // bindings — only the bindings the shader reads for the current
        // `mode` are sampled, so this is sound.
        let (f8_y, f8_u, f8_v, u16_y, u16_uv);
        if is_8bit {
            f8_y = &yv;
            f8_u = &uvv;
            f8_v = &vv;
            u16_y = &self.dummy_u16_view;
            u16_uv = &self.dummy_u16_uv_view;
        } else {
            f8_y = &self.dummy_f8_view;
            f8_u = &self.dummy_f8_view;
            f8_v = &self.dummy_f8_view;
            u16_y = &yv;
            u16_uv = &uvv;
        }

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("prdt-yuv-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(f8_y),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(f8_u),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(f8_v),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(u16_y),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(u16_uv),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: self.uniform_buf.as_entire_binding(),
                },
            ],
        });

        self.planes = Some(PlaneTextures {
            width,
            height,
            is_8bit,
            y,
            uv,
            v,
            bind_group,
        });
    }

    fn upload_i420(&self, i420: &prdt_media_sw::I420Frame) {
        let p = self.planes.as_ref().expect("planes set in ensure_planes");
        let w = i420.width;
        let h = i420.height;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        write_plane_bytes(&self.queue, &p.y, &i420.y, i420.stride_y, w, h);
        write_plane_bytes(&self.queue, &p.uv, &i420.u, i420.stride_uv, cw, ch);
        write_plane_bytes(&self.queue, &p.v, &i420.v, i420.stride_uv, cw, ch);
    }

    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    fn upload_nv12(&self, nv12: &prdt_media_linux::Nv12Frame) {
        let p = self.planes.as_ref().expect("planes set in ensure_planes");
        let w = nv12.width;
        let h = nv12.height;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        write_plane_bytes(&self.queue, &p.y, &nv12.y, nv12.stride_y, w, h);
        // UV is Rg8 (2 bytes/sample); stride_uv is in bytes already.
        write_plane_bytes(&self.queue, &p.uv, &nv12.uv, nv12.stride_uv, cw * 2, ch);
    }

    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any"
    ))]
    fn upload_p010(&self, nv12_10: &prdt_media_linux::Nv12Frame16) {
        let p = self.planes.as_ref().expect("planes set in ensure_planes");
        let w = nv12_10.width;
        let h = nv12_10.height;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        // Y: R16Uint, 2 bytes/sample, stride_y in u16 elements → *2 bytes.
        let y_bytes: &[u8] = bytemuck::cast_slice(&nv12_10.y);
        write_plane_bytes(&self.queue, &p.y, y_bytes, nv12_10.stride_y * 2, w * 2, h);
        // UV: Rg16Uint, 4 bytes/sample, stride_uv in u16 elements → *2 bytes.
        let uv_bytes: &[u8] = bytemuck::cast_slice(&nv12_10.uv);
        write_plane_bytes(
            &self.queue,
            &p.uv,
            uv_bytes,
            nv12_10.stride_uv * 2,
            cw * 4,
            ch,
        );
    }

    /// Build (or clear) the cursor overlay from `shared.cursor`. Reads the
    /// same data the CPU `composite_cursor` reads: lock, visibility, BGRA
    /// bitmap, position/hotspot. The overlay quad is positioned in clip
    /// space relative to the stream dimensions.
    fn update_cursor(&mut self, shared: &crate::ViewerShared, stream_w: u32, stream_h: u32) {
        let snapshot = if let Ok(s) = shared.cursor.lock() {
            if s.visible() {
                s.bitmap().map(|bmp| {
                    (
                        bmp.width as u32,
                        bmp.height as u32,
                        bmp.bgra.clone(),
                        s.position_x - s.hotspot_x,
                        s.position_y - s.hotspot_y,
                    )
                })
            } else {
                None
            }
        } else {
            None
        };

        let Some((bw, bh, bgra, tlx, tly)) = snapshot else {
            self.cursor = None;
            return;
        };
        if bw == 0 || bh == 0 || stream_w == 0 || stream_h == 0 {
            self.cursor = None;
            return;
        }

        // Convert the BGRA cursor to RGBA for an Rgba8Unorm texture (the
        // CPU path keeps BGRA because softbuffer is BGRA; the GPU sampler
        // is happiest with RGBA + an Rgba8Unorm view).
        let mut rgba = vec![0u8; bgra.len()];
        for (dst, src) in rgba.chunks_exact_mut(4).zip(bgra.chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }

        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("prdt-cursor-tex"),
            size: wgpu::Extent3d {
                width: bw,
                height: bh,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_plane_bytes(&self.queue, &tex, &rgba, bw * 4, bw * 4, bh);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Map the stream-pixel rect [tlx, tly, tlx+bw, tly+bh] to clip
        // space. Clip x is [-1,1] left→right; clip y is [1,-1] top→bottom.
        let x0 = (tlx as f32 / stream_w as f32) * 2.0 - 1.0;
        let x1 = ((tlx + bw as i32) as f32 / stream_w as f32) * 2.0 - 1.0;
        let y0 = 1.0 - (tly as f32 / stream_h as f32) * 2.0;
        let y1 = 1.0 - ((tly + bh as i32) as f32 / stream_h as f32) * 2.0;
        self.queue.write_buffer(
            &self.cursor_rect_buf,
            0,
            bytemuck::bytes_of(&CursorRect {
                rect: [x0, y0, x1, y1],
            }),
        );

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("prdt-cursor-bg"),
            layout: &self.cursor_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.cursor_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.cursor_rect_buf.as_entire_binding(),
                },
            ],
        });

        self.cursor = Some(CursorTexture { bind_group });
    }
}

/// Texture binding-layout entry; `uint` selects an unfilterable Uint
/// sample type (P010 planes), otherwise a filterable Float type.
fn plane_entry(binding: u32, uint: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: if uint {
                wgpu::TextureSampleType::Uint
            } else {
                wgpu::TextureSampleType::Float { filterable: true }
            },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Upload `height` rows of `row_bytes` bytes each, picking the row pitch
/// from `src_stride` (bytes). Handles strided source buffers by issuing a
/// `write_texture` with the proper `bytes_per_row`.
fn write_plane_bytes(
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    src: &[u8],
    src_stride: u32,
    row_bytes: u32,
    height: u32,
) {
    debug_assert!(src.len() as u32 >= src_stride * (height.saturating_sub(1)) + row_bytes);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        src,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(src_stride),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width: row_bytes_to_texels(tex, row_bytes),
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Texture copy extent width must be in texels, not bytes. Recover the
/// texel count from the byte count and the format's block size.
fn row_bytes_to_texels(tex: &wgpu::Texture, row_bytes: u32) -> u32 {
    let bpp = match tex.format() {
        wgpu::TextureFormat::R8Unorm => 1,
        wgpu::TextureFormat::Rg8Unorm => 2,
        wgpu::TextureFormat::R16Uint => 2,
        wgpu::TextureFormat::Rg16Uint => 4,
        wgpu::TextureFormat::Rgba8Unorm => 4,
        _ => 1,
    };
    row_bytes / bpp
}

/// Fullscreen-triangle vertex shader + YUV→RGB fragment shader. The math
/// mirrors the CPU converters in `linux.rs`.
const YUV_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Oversized triangle covering the viewport.
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var t = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var o: VsOut;
    o.pos = vec4<f32>(p[vid], 0.0, 1.0);
    o.uv = t[vid];
    return o;
}

@group(0) @binding(0) var tex_y8: texture_2d<f32>;
@group(0) @binding(1) var tex_u8: texture_2d<f32>;
@group(0) @binding(2) var tex_v8: texture_2d<f32>;
@group(0) @binding(3) var tex_y16: texture_2d<u32>;
@group(0) @binding(4) var tex_uv16: texture_2d<u32>;
@group(0) @binding(5) var samp: sampler;

struct Uniforms { mode: u32, pad0: u32, pad1: u32, pad2: u32 };
@group(0) @binding(6) var<uniform> U: Uniforms;

fn pq_eotf(e: f32) -> f32 {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let ep = pow(e, 1.0 / m2);
    let num = max(ep - c1, 0.0);
    let den = c2 - c3 * ep;
    return pow(num / den, 1.0 / m1) * 10000.0;
}

fn srgb_gamma(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.0031308) {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (U.mode == 2u) {
        // P010 → BT.2020 NCL → inverse PQ → Reinhard(1000) → sRGB gamma.
        let ydims = vec2<f32>(textureDimensions(tex_y16));
        let cdims = vec2<f32>(textureDimensions(tex_uv16));
        let yc = vec2<i32>(in.uv * ydims);
        let cc = vec2<i32>(in.uv * cdims);
        let yraw = f32(textureLoad(tex_y16, yc, 0).r >> 6u) / 1023.0;
        let uvs = textureLoad(tex_uv16, cc, 0);
        let u = f32(uvs.r >> 6u) / 1023.0 - 0.5;
        let v = f32(uvs.g >> 6u) / 1023.0 - 0.5;
        let r_lin = clamp(yraw + 1.4746 * v, 0.0, 1.0);
        let g_lin = clamp(yraw - 0.1646 * u - 0.5714 * v, 0.0, 1.0);
        let b_lin = clamp(yraw + 1.8814 * u, 0.0, 1.0);
        let scale = 1.0 / 1000.0;
        let r_tm = (pq_eotf(r_lin) * scale) / (1.0 + pq_eotf(r_lin) * scale);
        let g_tm = (pq_eotf(g_lin) * scale) / (1.0 + pq_eotf(g_lin) * scale);
        let b_tm = (pq_eotf(b_lin) * scale) / (1.0 + pq_eotf(b_lin) * scale);
        return vec4<f32>(srgb_gamma(r_tm), srgb_gamma(g_tm), srgb_gamma(b_tm), 1.0);
    }

    // I420 (mode 0) / NV12 (mode 1): BT.709 full-coefficient conversion.
    let y = textureSample(tex_y8, samp, in.uv).r;
    var u: f32;
    var v: f32;
    if (U.mode == 1u) {
        let uv = textureSample(tex_u8, samp, in.uv).rg;
        u = uv.r - 0.5;
        v = uv.g - 0.5;
    } else {
        u = textureSample(tex_u8, samp, in.uv).r - 0.5;
        v = textureSample(tex_v8, samp, in.uv).r - 0.5;
    }
    // CPU intent: r = y + (1793*V')>>10 with V' in [-128,127]. In float,
    // 1793/1024 = 1.7510, 534/1024 = 0.5215, 213/1024 = 0.2080,
    // 2115/1024 = 2.0654; (u,v) here are normalised to [-0.5,0.5] so
    // multiply by 255 to recover the integer chroma offset.
    let uo = u * 255.0;
    let vo = v * 255.0;
    let r = y + (1793.0 * vo / 1024.0) / 255.0;
    let g = y - (534.0 * uo / 1024.0 + 213.0 * vo / 1024.0) / 255.0;
    let b = y + (2115.0 * uo / 1024.0) / 255.0;
    return vec4<f32>(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}
"#;

/// Cursor overlay: a textured quad placed by a clip-space rect uniform.
const CURSOR_SHADER: &str = r#"
struct Rect { rect: vec4<f32> };
@group(0) @binding(2) var<uniform> R: Rect;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Two triangles for the quad [x0,y0]..[x1,y1].
    let x0 = R.rect.x;
    let y0 = R.rect.y;
    let x1 = R.rect.z;
    let y1 = R.rect.w;
    var corners = array<vec4<f32>, 6>(
        vec4<f32>(x0, y0, 0.0, 0.0),
        vec4<f32>(x1, y0, 1.0, 0.0),
        vec4<f32>(x0, y1, 0.0, 1.0),
        vec4<f32>(x1, y0, 1.0, 0.0),
        vec4<f32>(x1, y1, 1.0, 1.0),
        vec4<f32>(x0, y1, 0.0, 1.0),
    );
    let c = corners[vid];
    var o: VsOut;
    o.pos = vec4<f32>(c.x, c.y, 0.0, 1.0);
    o.uv = vec2<f32>(c.z, c.w);
    return o;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;
