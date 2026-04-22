//! Synthetic texture generators for tests and the Phase 0 latency-bench.
//! Produces CPU-side pixel buffers that can be uploaded to a D3d11Texture.

use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_SUBRESOURCE_DATA};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
use crate::error::{MediaError, Result};

/// Generate a BGRA solid-color buffer.
pub fn solid_bgra(width: u32, height: u32, b: u8, g: u8, r: u8, a: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
        buf.extend_from_slice(&[b, g, r, a]);
    }
    buf
}

/// Generate a BGRA texture with a "counter" encoded as a small 16x16 pixel
/// block at top-left. The counter value is spread across 4 channels of 16
/// pixels each — useful for measuring end-to-end frame delivery in tests.
///
/// Layout:
/// ```text
/// pixels[0..16]    = frame_seq byte 0 (low)
/// pixels[16..32]   = frame_seq byte 1
/// pixels[32..48]   = frame_seq byte 2
/// pixels[48..64]   = frame_seq byte 3
/// (frame_seq is truncated to u32; rest of image is solid background)
/// ```
pub fn bgra_with_counter(width: u32, height: u32, frame_seq: u32, bg: (u8, u8, u8)) -> Vec<u8> {
    let mut buf = solid_bgra(width, height, bg.0, bg.1, bg.2, 0xFF);
    // First 64 pixels encode the frame counter in 4 bytes of 16 pixels each.
    let bytes = frame_seq.to_le_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        for p in 0..16 {
            let pixel_index = i * 16 + p;
            let offset = pixel_index * 4;
            buf[offset] = byte; // B
            buf[offset + 1] = byte; // G
            buf[offset + 2] = byte; // R
            buf[offset + 3] = 0xFF; // A
        }
    }
    buf
}

/// Decode a frame counter from a BGRA buffer produced by
/// `bgra_with_counter`. Returns `None` if the counter pixels disagree
/// (indicating the texture got corrupted).
pub fn decode_counter_bgra(buf: &[u8]) -> Option<u32> {
    if buf.len() < 64 * 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for i in 0..4 {
        // Take the B channel of the first pixel in each 16-pixel group as
        // the canonical byte; verify the rest of the group agrees.
        let canonical = buf[i * 16 * 4]; // B of first pixel in group
        for p in 1..16 {
            let offset = (i * 16 + p) * 4;
            if buf[offset] != canonical {
                return None;
            }
        }
        bytes[i] = canonical;
    }
    Some(u32::from_le_bytes(bytes))
}

/// Create a D3D11 texture populated with a BGRA counter pattern.
///
/// Uploads the CPU-side buffer to a new DEFAULT-usage D3D11 texture using
/// `CreateTexture2D` with initial data.
pub fn make_counter_texture(
    dev: &D3d11Device,
    width: u32,
    height: u32,
    frame_seq: u32,
) -> Result<D3d11Texture> {
    let pixels = bgra_with_counter(width, height, frame_seq, (0x33, 0x66, 0x99));
    let row_pitch = width * 4;

    // We have to construct the texture manually because `new_default`
    // doesn't support initial data. Mirror new_default's behavior.
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const _,
        SysMemPitch: row_pitch,
        SysMemSlicePitch: 0,
    };

    let mut out: Option<ID3D11Texture2D> = None;
    unsafe {
        dev.device()
            .CreateTexture2D(&desc, Some(&initial), Some(&mut out))
            .map_err(|e| MediaError::d3d11("CreateTexture2D with initial data", e))?;
    }
    let inner = out.ok_or(MediaError::D3D11 {
        context: "CreateTexture2D returned null (synthetic)",
        hresult: 0,
    })?;

    Ok(D3d11Texture::from_raw(
        inner,
        width,
        height,
        TextureFormat::Bgra8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_round_trip_through_buffer() {
        for seq in [0u32, 1, 256, 65536, u32::MAX] {
            let buf = bgra_with_counter(64, 64, seq, (10, 20, 30));
            let back = decode_counter_bgra(&buf).expect("decode");
            assert_eq!(back, seq);
        }
    }

    #[test]
    fn solid_bgra_size_correct() {
        let buf = solid_bgra(10, 5, 1, 2, 3, 255);
        assert_eq!(buf.len(), 10 * 5 * 4);
        assert_eq!(buf[0..4], [1, 2, 3, 255]);
    }

    #[test]
    fn counter_texture_survives_gpu_round_trip() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let tex = make_counter_texture(&dev, 128, 128, 42).expect("create counter tex");
        assert_eq!(tex.width(), 128);
        let buf = tex.read_back_bgra_or_rgba(&dev).expect("readback");
        let decoded = decode_counter_bgra(&buf).expect("decode counter");
        assert_eq!(decoded, 42);
    }
}
