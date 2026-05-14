//! CPU → GPU NV12 upload path for the OpenH264 software decoder.
//!
//! Plan §Phase 3 path (a): the SW decoder produces planar I420 on CPU
//! memory. We convert to NV12 layout via `prdt_media_sw::i420_to_nv12`,
//! map a `D3D11_USAGE_STAGING` NV12 texture, copy the bytes into it,
//! unmap, then issue a GPU `CopySubresourceRegion` into the renderer's
//! `D3D11_USAGE_DEFAULT` NV12 input texture. The shape that comes out
//! is identical to what `MfD3d11Consumer::take_latest_texture` returns,
//! so the existing `Nv12Renderer` path consumes it without modification.
//!
//! D3D11 NV12 texture layout (single subresource):
//!   * `mapped.pData[0 .. RowPitch * height]` — Y plane (one byte per pixel).
//!   * `mapped.pData[RowPitch * height ..]`   — interleaved UV plane,
//!     `height/2` rows, two bytes per UV pair, same `RowPitch` as Y.

use prdt_media_sw::I420Frame;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_CPU_ACCESS_WRITE, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
use crate::error::{MediaError, Result};

/// Upload an `I420Frame` from `prdt-media-sw` into a GPU NV12 texture
/// shaped like the MF / NVDEC consumer output. One uploader is held per
/// stream; `dst` is recreated when the negotiated dimensions change.
pub struct CpuI420Uploader {
    dev: D3d11Device,
    width: u32,
    height: u32,
    /// `D3D11_USAGE_STAGING` NV12 texture sized to `width × height`.
    /// Reused across frames; the upload path is map → memcpy → unmap →
    /// GPU CopySubresourceRegion.
    staging: ID3D11Texture2D,
    /// `D3D11_USAGE_DEFAULT` NV12 texture handed to `Nv12Renderer`.
    /// Wrapped in `D3d11Texture` so the renderer's input view code path
    /// matches what `MfD3d11Consumer::take_latest_texture` returns.
    dst: D3d11Texture,
}

impl CpuI420Uploader {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self> {
        if width % 2 != 0 || height % 2 != 0 || width == 0 || height == 0 {
            return Err(MediaError::Other(format!(
                "CpuI420Uploader: dims must be even and nonzero, got {width}x{height}"
            )));
        }

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        unsafe {
            dev.device()
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| MediaError::d3d11("CreateTexture2D NV12 staging", e))?;
        }
        let staging = staging.ok_or(MediaError::D3D11 {
            context: "CreateTexture2D returned null staging",
            hresult: 0,
        })?;

        // Must be a video-processor input texture (SHADER_RESOURCE only,
        // no RENDER_TARGET): `Nv12Renderer` feeds `dst` to
        // `CreateVideoProcessorInputView`, which rejects NV12 textures
        // carrying `D3D11_BIND_RENDER_TARGET` with `E_INVALIDARG` (issue
        // #19 Bug 4). `dst` is only ever a `CopyResource` destination
        // here, so it never needs render-target binding.
        let dst = D3d11Texture::new_for_video_processor(dev, width, height, TextureFormat::Nv12)?;

        Ok(Self {
            dev: dev.clone(),
            width,
            height,
            staging,
            dst,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Upload the given I420 frame into the GPU NV12 texture and return
    /// a handle to that texture. The same `D3d11Texture` is returned on
    /// every call; the caller keeps a clone (Arc-like ref-counted COM)
    /// for as long as it needs to render — subsequent uploads overwrite
    /// the GPU texture in place.
    pub fn upload(&self, frame: &I420Frame) -> Result<D3d11Texture> {
        if frame.width != self.width || frame.height != self.height {
            return Err(MediaError::Other(format!(
                "CpuI420Uploader: frame dims {}x{} do not match uploader dims {}x{}",
                frame.width, frame.height, self.width, self.height
            )));
        }

        let nv12 = prdt_media_sw::i420_to_nv12(frame)
            .map_err(|e| MediaError::Other(format!("i420_to_nv12: {e}")))?;
        let src_stride = frame.stride_y as usize;
        let h = self.height as usize;
        let w = self.width as usize;
        let uv_h = h / 2;

        // Map staging, memcpy plane-by-plane respecting RowPitch (which
        // may be > frame width on driver-padded textures), unmap, then
        // GPU-copy staging → default.
        self.dev.with_context(|ctx| -> Result<()> {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                ctx.Map(&self.staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))
                    .map_err(|e| MediaError::d3d11("Map NV12 staging", e))?;
            }
            let row_pitch = mapped.RowPitch as usize;
            unsafe {
                // Y plane: w bytes per row, h rows.
                for row in 0..h {
                    let src = nv12.as_ptr().add(row * src_stride);
                    let dst = (mapped.pData as *mut u8).add(row * row_pitch);
                    std::ptr::copy_nonoverlapping(src, dst, w);
                }
                // UV plane sits immediately after Y in the source NV12
                // buffer (length-wise: stride_y * h), and at offset
                // RowPitch * height in the mapped texture. The UV row is
                // also `w` bytes (w/2 UV pairs × 2 bytes).
                let src_uv_offset = src_stride * h;
                let dst_uv_offset = row_pitch * h;
                for row in 0..uv_h {
                    let src = nv12.as_ptr().add(src_uv_offset + row * src_stride);
                    let dst = (mapped.pData as *mut u8).add(dst_uv_offset + row * row_pitch);
                    std::ptr::copy_nonoverlapping(src, dst, w);
                }
                ctx.Unmap(&self.staging, 0);
                ctx.CopyResource(self.dst.raw(), &self.staging);
            }
            Ok(())
        })?;

        Ok(self.dst.clone())
    }
}

// CpuI420Uploader holds D3d11 COM objects via D3d11Device + raw
// ID3D11Texture2D. The viewer pipeline drives upload from a single
// tokio task at a time (the recv task), so Send is safe — same
// rationale as `MfD3d11Consumer`.
unsafe impl Send for CpuI420Uploader {}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_i420(width: u32, height: u32, y_val: u8, u_val: u8, v_val: u8) -> I420Frame {
        let mut f = I420Frame::new_packed(width, height).unwrap();
        for b in f.y.iter_mut() {
            *b = y_val;
        }
        for b in f.u.iter_mut() {
            *b = u_val;
        }
        for b in f.v.iter_mut() {
            *b = v_val;
        }
        f
    }

    #[test]
    fn uploader_roundtrip_solid_frame() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let up = CpuI420Uploader::new(&dev, 64, 64).expect("uploader");
        let frame = solid_i420(64, 64, 0xAB, 0x55, 0xCD);
        let tex = up.upload(&frame).expect("upload");
        assert_eq!(tex.width(), 64);
        assert_eq!(tex.height(), 64);
        assert_eq!(tex.format(), TextureFormat::Nv12);
    }

    #[test]
    fn uploader_rejects_dim_mismatch() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let up = CpuI420Uploader::new(&dev, 64, 64).expect("uploader");
        let frame = solid_i420(32, 32, 16, 128, 128);
        match up.upload(&frame) {
            Err(MediaError::Other(_)) => {}
            Ok(_) => panic!("expected dim-mismatch error"),
            Err(e) => panic!("expected MediaError::Other, got {e:?}"),
        }
    }

    #[test]
    fn uploader_rejects_odd_dims() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        match CpuI420Uploader::new(&dev, 65, 64) {
            Err(MediaError::Other(_)) => {}
            Ok(_) => panic!("expected odd-dims error"),
            Err(e) => panic!("expected MediaError::Other, got {e:?}"),
        }
    }
}
