//! Safe wrapper around ID3D11Texture2D with helpers for common operations:
//! staging-buffer readback, creation by explicit desc, format enum.

use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_RESOURCE_MISC_SHARED, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC,
};

use crate::d3d11::device::D3d11Device;
use crate::error::{MediaError, Result};

/// Pixel formats supported by the media-win pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    /// 8-bit BGRA, post-DXGI-capture default.
    Bgra8,
    /// 8-bit RGBA (used by some tooling paths).
    Rgba8,
    /// NV12 (Y plane + interleaved UV half-res) — the NVDEC default output.
    Nv12,
    /// 8-bit single-channel red. Used as the Y plane carrier for the
    /// dual-plane CUDA-D3D11 interop path (Plan 2d zero-copy).
    R8,
    /// 8-bit two-channel red+green. Used as the UV plane carrier for the
    /// dual-plane CUDA-D3D11 interop path (Plan 2d zero-copy). Half-resolution
    /// in both dimensions vs the Y plane; each element holds (Cb, Cr).
    R8G8,
}

impl TextureFormat {
    pub fn to_dxgi(self) -> windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT {
        match self {
            Self::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
            Self::Rgba8 => DXGI_FORMAT_R8G8B8A8_UNORM,
            Self::Nv12 => DXGI_FORMAT_NV12,
            Self::R8 => DXGI_FORMAT_R8_UNORM,
            Self::R8G8 => DXGI_FORMAT_R8G8_UNORM,
        }
    }

    /// Bytes per pixel when the format is interleaved (YUV planar formats
    /// like NV12 return the Y-plane byte rate; callers that need full size
    /// must account for the UV plane separately).
    pub fn bytes_per_pixel_y(self) -> usize {
        match self {
            Self::Bgra8 | Self::Rgba8 => 4,
            Self::Nv12 | Self::R8 => 1,
            Self::R8G8 => 2,
        }
    }
}

/// A 2D texture on the GPU.
#[derive(Clone)]
pub struct D3d11Texture {
    inner: ID3D11Texture2D,
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl D3d11Texture {
    /// Create a fresh DEFAULT-usage texture ready for render target / shader
    /// resource binding.
    pub fn new_default(
        dev: &D3d11Device,
        width: u32,
        height: u32,
        fmt: TextureFormat,
    ) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a SHARED texture (for NVENC input, Phase 2b).
    pub fn new_shared_for_encoder(
        dev: &D3d11Device,
        width: u32,
        height: u32,
        fmt: TextureFormat,
    ) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a texture intended as a CUDA-D3D11 interop target.
    /// `SHADER_RESOURCE` only (no RENDER_TARGET), no CPU access, no misc
    /// flags — CUDA's `cuGraphicsD3D11RegisterResource` is pickier about
    /// extra BindFlags than general-purpose textures and has been
    /// observed to refuse NV12 textures that also carry
    /// `D3D11_BIND_RENDER_TARGET`.
    pub fn new_for_cuda_interop(
        dev: &D3d11Device,
        width: u32,
        height: u32,
        fmt: TextureFormat,
    ) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a DEFAULT-usage texture intended as a Direct3D11 *video
    /// processor input* (the texture handed to `CreateVideoProcessorInputView`).
    ///
    /// `BindFlags: D3D11_BIND_RENDER_TARGET`. Per the
    /// `CreateVideoProcessorInputView` documentation, a video-processor
    /// input resource must use either `BindFlags = 0`, or a combination
    /// that includes one of `D3D11_BIND_DECODER`,
    /// `D3D11_BIND_VIDEO_ENCODER`, `D3D11_BIND_RENDER_TARGET`, or
    /// `D3D11_BIND_UNORDERED_ACCESS_VIEW`. The 2026-05-15 smoke proved
    /// the spec-allowed `BindFlags = 0` does **not** work on the user's
    /// Intel iGPU driver — it returned `E_INVALIDARG` even with `0x0` —
    /// so we use `RENDER_TARGET` (the most general included-flag, used
    /// by NV12 video pipelines elsewhere). This still rules out the
    /// pre-PR-#22 `new_default` choice (`RT | SR = 0x28`) — though `0x28`
    /// is documented-valid (it includes RT), it is unnecessary to keep
    /// `SHADER_RESOURCE` for this texture's only uses (`CopyResource`
    /// destination from `CpuI420Uploader`'s staging, and video-processor
    /// input). Keeping `BindFlags` minimal reduces driver-quirk surface.
    pub fn new_for_video_processor(
        dev: &D3d11Device,
        width: u32,
        height: u32,
        fmt: TextureFormat,
    ) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a STAGING texture for CPU readback.
    pub fn new_staging(
        dev: &D3d11Device,
        width: u32,
        height: u32,
        fmt: TextureFormat,
    ) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a texture from an arbitrary desc. Internal.
    fn new_with_desc(
        dev: &D3d11Device,
        desc: D3D11_TEXTURE2D_DESC,
        fmt: TextureFormat,
        init: Option<&D3D11_SUBRESOURCE_DATA>,
    ) -> Result<Self> {
        let mut out: Option<ID3D11Texture2D> = None;
        unsafe {
            dev.device()
                .CreateTexture2D(&desc, init.map(|d| d as *const _), Some(&mut out))
                .map_err(|e| MediaError::d3d11("CreateTexture2D", e))?;
        }
        let inner = out.ok_or(MediaError::D3D11 {
            context: "CreateTexture2D returned null",
            hresult: 0,
        })?;
        Ok(Self {
            inner,
            width: desc.Width,
            height: desc.Height,
            format: fmt,
        })
    }

    /// Package-private helper for constructing from a raw ID3D11Texture2D.
    /// Used by `synthetic::make_counter_texture` (Task 6) and planned
    /// Plan 2b DXGI capture wrapper. Not public API.
    #[allow(dead_code)]
    pub(crate) fn from_raw(
        inner: ID3D11Texture2D,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        Self {
            inner,
            width,
            height,
            format,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    pub fn raw(&self) -> &ID3D11Texture2D {
        &self.inner
    }

    /// Tight-packed CPU readback of this texture's BGRA/RGBA bytes into a
    /// caller-provided buffer using a caller-provided staging texture
    /// (allocated once per producer lifetime so the hot path doesn't pay
    /// for a fresh staging allocation per frame). `out` must already be
    /// `width * height * 4` bytes long.
    ///
    /// Used by the SW codec path (`DxgiSwProducer` in the host crate) where
    /// readback is the dominant cost and we can't afford the allocate-per-
    /// call pattern used by `read_back_bgra_or_rgba`.
    pub fn read_back_bgra_into(
        &self,
        dev: &D3d11Device,
        staging: &D3d11Texture,
        out: &mut [u8],
    ) -> Result<()> {
        if !matches!(self.format, TextureFormat::Bgra8 | TextureFormat::Rgba8) {
            return Err(MediaError::UnsupportedFormat {
                fmt: "read_back_bgra_into requires BGRA8 or RGBA8 source",
            });
        }
        if staging.format != self.format
            || staging.width != self.width
            || staging.height != self.height
        {
            return Err(MediaError::UnsupportedFormat {
                fmt: "staging tex format/dims mismatch source",
            });
        }
        let row_bytes = (self.width as usize) * 4;
        let expected = row_bytes * (self.height as usize);
        if out.len() != expected {
            return Err(MediaError::UnsupportedFormat {
                fmt: "read_back_bgra_into out buffer wrong size",
            });
        }

        // Copy GPU → staging.
        dev.with_context(|ctx| unsafe {
            ctx.CopyResource(&staging.inner, &self.inner);
        });

        // Map and tight-pack.
        dev.with_context(|ctx| -> Result<()> {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                ctx.Map(&staging.inner, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(|e| {
                        MediaError::d3d11("Map staging texture (read_back_bgra_into)", e)
                    })?;
            }
            let row_pitch = mapped.RowPitch as usize;
            unsafe {
                for y in 0..self.height as usize {
                    let src_row = (mapped.pData as *const u8).add(y * row_pitch);
                    let dst_row = out.as_mut_ptr().add(y * row_bytes);
                    std::ptr::copy_nonoverlapping(src_row, dst_row, row_bytes);
                }
                ctx.Unmap(&staging.inner, 0);
            }
            Ok(())
        })?;

        Ok(())
    }

    /// Copy this texture to a staging texture, map it, and return a CPU-side
    /// Vec<u8> containing the pixel bytes (row-major, tight packing for BGRA/RGBA).
    ///
    /// For NV12 this returns the Y plane only; UV plane read is a separate
    /// helper in NVDEC-oriented code in Plan 2c.
    pub fn read_back_bgra_or_rgba(&self, dev: &D3d11Device) -> Result<Vec<u8>> {
        if !matches!(self.format, TextureFormat::Bgra8 | TextureFormat::Rgba8) {
            return Err(MediaError::UnsupportedFormat {
                fmt: "read_back_bgra_or_rgba requires BGRA8 or RGBA8",
            });
        }
        let staging = Self::new_staging(dev, self.width, self.height, self.format)?;

        // Copy GPU -> staging. Needs immediate context.
        dev.with_context(|ctx| unsafe {
            ctx.CopyResource(&staging.inner, &self.inner);
        });

        // Map and copy to Vec<u8>.
        let bytes_per_pixel = self.format.bytes_per_pixel_y();
        let target_row_bytes = (self.width as usize) * bytes_per_pixel;
        let mut out = vec![0u8; target_row_bytes * self.height as usize];

        dev.with_context(|ctx| -> Result<()> {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                ctx.Map(&staging.inner, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(|e| MediaError::d3d11("Map staging texture", e))?;
            }
            let row_pitch = mapped.RowPitch as usize;
            unsafe {
                for y in 0..self.height as usize {
                    let src_row = (mapped.pData as *const u8).add(y * row_pitch);
                    let dst_row = out.as_mut_ptr().add(y * target_row_bytes);
                    std::ptr::copy_nonoverlapping(src_row, dst_row, target_row_bytes);
                }
                ctx.Unmap(&staging.inner, 0);
            }
            Ok(())
        })?;

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_conversions() {
        assert_eq!(TextureFormat::Bgra8.bytes_per_pixel_y(), 4);
        assert_eq!(TextureFormat::Rgba8.bytes_per_pixel_y(), 4);
        assert_eq!(TextureFormat::Nv12.bytes_per_pixel_y(), 1);

        assert_eq!(TextureFormat::Bgra8.to_dxgi(), DXGI_FORMAT_B8G8R8A8_UNORM);
    }

    #[test]
    fn create_default_texture() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let tex = D3d11Texture::new_default(&dev, 256, 256, TextureFormat::Bgra8)
            .expect("create texture");
        assert_eq!(tex.width(), 256);
        assert_eq!(tex.height(), 256);
        assert_eq!(tex.format(), TextureFormat::Bgra8);
    }

    #[test]
    fn create_staging_texture() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let tex = D3d11Texture::new_staging(&dev, 64, 64, TextureFormat::Bgra8)
            .expect("create staging texture");
        assert_eq!(tex.width(), 64);
    }

    #[test]
    fn create_video_processor_input_texture() {
        // issue #19 Bug 4: NV12 video-processor input textures must be
        // created with BindFlags = 0 (or a combination including
        // RENDER_TARGET / DECODER / VIDEO_ENCODER / UAV); SHADER_RESOURCE
        // alone makes CreateVideoProcessorInputView fail with E_INVALIDARG.
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let tex = D3d11Texture::new_for_video_processor(&dev, 128, 64, TextureFormat::Nv12)
            .expect("create video-processor input texture");
        assert_eq!(tex.width(), 128);
        assert_eq!(tex.height(), 64);
        assert_eq!(tex.format(), TextureFormat::Nv12);
    }

    #[test]
    fn r8_format_dxgi_mapping() {
        assert_eq!(
            TextureFormat::R8.to_dxgi(),
            windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8_UNORM
        );
        assert_eq!(TextureFormat::R8.bytes_per_pixel_y(), 1);
    }

    #[test]
    fn r8g8_format_dxgi_mapping() {
        assert_eq!(
            TextureFormat::R8G8.to_dxgi(),
            windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8_UNORM
        );
        assert_eq!(TextureFormat::R8G8.bytes_per_pixel_y(), 2);
    }
}
