//! CPU → GPU P010 upload path for the HDR10 SW decode path (PR3).
//!
//! Mirrors `cpu_i420_upload.rs` but for P010LE 10-bit 4:2:0 frames.
//! `Nv12Frame16` from `prdt_media_core` carries u16 Y and u16 UV planes
//! (valid 10 bits in the high part of each 16-bit container, libavcodec
//! P010LE convention). We map a STAGING P010 texture, memcpy both planes,
//! unmap, then `CopySubresourceRegion` into a DEFAULT P010 texture suitable
//! for `Nv12ShaderRendererP010`.
//!
//! Only compiled when the `media-win-hdr10` feature is active.

use prdt_media_core::Nv12Frame16;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_WRITE, D3D11_MAP_WRITE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
use crate::error::{MediaError, Result};

/// Upload an `Nv12Frame16` (P010LE CPU frame) into a GPU P010 texture shaped
/// for `Nv12ShaderRendererP010`. One uploader is held per stream; `dst` is
/// recreated when the negotiated dimensions change.
pub struct CpuP010Uploader {
    dev: D3d11Device,
    width: u32,
    height: u32,
    /// `D3D11_USAGE_STAGING` P010 texture for CPU writes.
    staging: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    /// `D3D11_USAGE_DEFAULT` P010 texture handed to `Nv12ShaderRendererP010`.
    dst: D3d11Texture,
}

impl CpuP010Uploader {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self> {
        if width % 2 != 0 || height % 2 != 0 || width == 0 || height == 0 {
            return Err(MediaError::Other(format!(
                "CpuP010Uploader: dims must be even and nonzero, got {width}x{height}"
            )));
        }

        let staging = Self::make_staging(dev, width, height)?;
        let dst = D3d11Texture::new_for_nv12_shader_input(dev, width, height, TextureFormat::P010)?;

        Ok(Self {
            dev: dev.clone(),
            width,
            height,
            staging,
            dst,
        })
    }

    /// Upload `frame` into the GPU P010 texture. Recreates internal resources
    /// if the frame dimensions changed (mid-stream resolution switch).
    pub fn upload(&mut self, frame: &Nv12Frame16) -> Result<D3d11Texture> {
        let fw = frame.width;
        let fh = frame.height;
        if fw != self.width || fh != self.height {
            self.staging = Self::make_staging(&self.dev, fw, fh)?;
            self.dst =
                D3d11Texture::new_for_nv12_shader_input(&self.dev, fw, fh, TextureFormat::P010)?;
            self.width = fw;
            self.height = fh;
        }

        self.dev.with_context(|ctx| -> Result<()> {
            unsafe {
                // SAFETY: Map with WRITE on a STAGING texture blocks until
                // the GPU is done reading from it (D3D11 guarantees this).
                let mut mapped = std::mem::zeroed();
                ctx.Map(&self.staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))
                    .map_err(|e| MediaError::d3d11("CpuP010Uploader::Map", e))?;

                // P010 staging layout:
                //   Y plane:  RowPitch * height bytes (u16 per pixel, tightly packed per row)
                //   UV plane: RowPitch * (height/2) bytes (interleaved, u16 pairs)
                // RowPitch from D3D11 may be larger than width*2 (hardware alignment),
                // so we copy row-by-row.
                let row_pitch = mapped.RowPitch as usize;
                let base = mapped.pData as *mut u8;

                // Copy Y plane row by row.
                let src_stride_y = frame.stride_y as usize * 2; // stride in bytes
                for row in 0..(self.height as usize) {
                    let src = frame.y.as_ptr().add(row * frame.stride_y as usize) as *const u8;
                    let dst = base.add(row * row_pitch);
                    std::ptr::copy_nonoverlapping(src, dst, (self.width as usize) * 2);
                }

                // UV plane starts at RowPitch * height.
                let uv_base = base.add(row_pitch * self.height as usize);
                let uv_rows = (self.height as usize) / 2;
                let src_stride_uv = frame.stride_uv as usize * 2; // stride in bytes
                for row in 0..uv_rows {
                    let src = frame.uv.as_ptr().add(row * frame.stride_uv as usize) as *const u8;
                    let dst = uv_base.add(row * row_pitch);
                    // UV row width = width u16 pairs = width*2 bytes.
                    std::ptr::copy_nonoverlapping(src, dst, (self.width as usize) * 2);
                }

                ctx.Unmap(&self.staging, 0);

                // Copy STAGING → DEFAULT.
                ctx.CopySubresourceRegion(self.dst.raw(), 0, 0, 0, 0, &self.staging, 0, None);

                // Suppress unused-variable warnings for stride locals in the
                // copy loop (they are read via the raw pointer arithmetic above).
                let _ = src_stride_y;
                let _ = src_stride_uv;

                Ok(())
            }
        })?;

        Ok(self.dst.clone())
    }

    fn make_staging(
        dev: &D3d11Device,
        width: u32,
        height: u32,
    ) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> {
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_P010;

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_P010,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
        };

        let mut out: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> = None;
        unsafe {
            dev.device()
                .CreateTexture2D(&desc, None, Some(&mut out))
                .map_err(|e| MediaError::d3d11("CpuP010Uploader staging CreateTexture2D", e))?;
        }
        out.ok_or(MediaError::D3D11 {
            context: "CpuP010Uploader staging CreateTexture2D returned null",
            hresult: 0,
        })
    }
}
