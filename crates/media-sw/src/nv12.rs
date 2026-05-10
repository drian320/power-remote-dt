//! CPU-side pixel-format helpers used on the SW codec path.
//!
//! - `I420Frame`: planar YUV 4:2:0 with separate U and V planes. This is
//!   the format OpenH264 produces and consumes natively.
//! - `bgra_to_i420`: BGRA8 (DXGI desktop-duplication staging readback
//!   layout) → I420. Used by the SW *encoder* path to feed OpenH264.
//! - `i420_to_nv12`: I420 → NV12 (interleaved UV). Used by the SW
//!   *decoder* path so the existing `DualPlaneYuvRenderer` (which
//!   expects NV12) can present without a shader change.
//!
//! BT.601 limited-range coefficients are used for BGRA→I420. This
//! matches the historical assumption of the project's HW path
//! (`Nv12Renderer` and `DualPlaneYuvRenderer` both decode using
//! BT.601 limited range). The conversion is intentionally simple
//! scalar Rust — adequate at 1080p where SW encode is the dominant
//! cost; a SIMD/GPU optimisation is tracked as a follow-up in the ADR.

use crate::error::{MediaSwError, Result};

/// Planar I420 (YUV 4:2:0) frame on CPU memory.
#[derive(Debug, Clone)]
pub struct I420Frame {
    pub width: u32,
    pub height: u32,
    /// Y plane, length == `stride_y * height`.
    pub y: Vec<u8>,
    /// U plane, length == `stride_uv * (height / 2)`.
    pub u: Vec<u8>,
    /// V plane, length == `stride_uv * (height / 2)`.
    pub v: Vec<u8>,
    pub stride_y: u32,
    pub stride_uv: u32,
}

impl I420Frame {
    /// Allocate a tightly-packed I420 frame (strides == width / width-half).
    pub fn new_packed(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err(MediaSwError::InvalidFrame {
                reason: format!("dims must be even and nonzero, got {width}x{height}"),
            });
        }
        let stride_y = width;
        let stride_uv = width / 2;
        let y = vec![0u8; (stride_y as usize) * (height as usize)];
        let u = vec![0u8; (stride_uv as usize) * (height as usize / 2)];
        let v = vec![0u8; (stride_uv as usize) * (height as usize / 2)];
        Ok(Self {
            width,
            height,
            y,
            u,
            v,
            stride_y,
            stride_uv,
        })
    }

    /// Implements `openh264::formats::YUVSource` so the encoder can
    /// consume an `I420Frame` directly.
    pub(crate) fn as_yuv_source(&self) -> AsYuv<'_> {
        AsYuv(self)
    }
}

pub(crate) struct AsYuv<'a>(&'a I420Frame);

impl openh264::formats::YUVSource for AsYuv<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.0.width as usize, self.0.height as usize)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (
            self.0.stride_y as usize,
            self.0.stride_uv as usize,
            self.0.stride_uv as usize,
        )
    }
    fn y(&self) -> &[u8] {
        &self.0.y
    }
    fn u(&self) -> &[u8] {
        &self.0.u
    }
    fn v(&self) -> &[u8] {
        &self.0.v
    }
}

/// Convert BGRA8 (`bgra_stride` bytes per row) to packed I420 using
/// BT.601 limited-range coefficients. Width and height must be even.
pub fn bgra_to_i420(bgra: &[u8], width: u32, height: u32, bgra_stride: u32) -> Result<I420Frame> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err(MediaSwError::InvalidFrame {
            reason: format!("dims must be even and nonzero, got {width}x{height}"),
        });
    }
    let bgra_stride = bgra_stride as usize;
    if bgra_stride < (width as usize) * 4 {
        return Err(MediaSwError::InvalidFrame {
            reason: format!(
                "bgra_stride {bgra_stride} too small for width {width} (need >= {})",
                width as usize * 4
            ),
        });
    }
    let needed = bgra_stride * height as usize;
    if bgra.len() < needed {
        return Err(MediaSwError::InvalidFrame {
            reason: format!(
                "bgra buffer {} bytes < required {needed} (stride {bgra_stride}, h {height})",
                bgra.len()
            ),
        });
    }

    let mut frame = I420Frame::new_packed(width, height)?;
    let w = width as usize;
    let h = height as usize;
    let stride_y = frame.stride_y as usize;
    let stride_uv = frame.stride_uv as usize;

    // Y plane: per-pixel BT.601 limited-range
    //   Y = ((66*R + 129*G + 25*B + 128) >> 8) + 16
    for y in 0..h {
        let src_row = &bgra[y * bgra_stride..y * bgra_stride + w * 4];
        let dst_row = &mut frame.y[y * stride_y..y * stride_y + w];
        for x in 0..w {
            let b = src_row[x * 4] as i32;
            let g = src_row[x * 4 + 1] as i32;
            let r = src_row[x * 4 + 2] as i32;
            let yv = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            dst_row[x] = yv.clamp(0, 255) as u8;
        }
    }

    // U/V planes: 2x2 box subsampling, BT.601 limited-range
    //   U = ((-38*R - 74*G + 112*B + 128) >> 8) + 128
    //   V = ((112*R - 94*G - 18*B + 128) >> 8) + 128
    for j in 0..(h / 2) {
        let r0 = j * 2;
        let r1 = r0 + 1;
        let row0 = &bgra[r0 * bgra_stride..r0 * bgra_stride + w * 4];
        let row1 = &bgra[r1 * bgra_stride..r1 * bgra_stride + w * 4];
        let u_row = &mut frame.u[j * stride_uv..j * stride_uv + w / 2];
        let v_row = &mut frame.v[j * stride_uv..j * stride_uv + w / 2];
        for i in 0..(w / 2) {
            let c0 = i * 2;
            let c1 = c0 + 1;
            // average four BGRA pixels
            let mut sb = 0i32;
            let mut sg = 0i32;
            let mut sr = 0i32;
            for (row, c) in [(row0, c0), (row0, c1), (row1, c0), (row1, c1)] {
                sb += row[c * 4] as i32;
                sg += row[c * 4 + 1] as i32;
                sr += row[c * 4 + 2] as i32;
            }
            let r = sr / 4;
            let g = sg / 4;
            let b = sb / 4;
            let uv_u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let uv_v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            u_row[i] = uv_u.clamp(0, 255) as u8;
            v_row[i] = uv_v.clamp(0, 255) as u8;
        }
    }

    Ok(frame)
}

/// Convert I420 to NV12 (Y plane unchanged, UV interleaved).
/// Output layout: `[Y plane (height*stride_y) | UV plane (height/2 * stride_y)]`.
/// Output stride for both planes equals `stride_y` (the typical NV12 layout
/// expected by D3D11 NV12 textures).
pub fn i420_to_nv12(i420: &I420Frame) -> Result<Vec<u8>> {
    let w = i420.width as usize;
    let h = i420.height as usize;
    let stride_y = i420.stride_y as usize;
    let stride_uv = i420.stride_uv as usize;

    if i420.y.len() < stride_y * h {
        return Err(MediaSwError::InvalidFrame {
            reason: format!(
                "y plane {} bytes < required {} (stride {stride_y}, h {h})",
                i420.y.len(),
                stride_y * h
            ),
        });
    }
    let uv_h = h / 2;
    if i420.u.len() < stride_uv * uv_h || i420.v.len() < stride_uv * uv_h {
        return Err(MediaSwError::InvalidFrame {
            reason: format!(
                "u/v planes too short: u={}, v={}, need {} each",
                i420.u.len(),
                i420.v.len(),
                stride_uv * uv_h,
            ),
        });
    }

    let nv12_stride = stride_y;
    let mut out = vec![0u8; nv12_stride * h + nv12_stride * uv_h];

    // Copy Y row-by-row (handles stride_y > w)
    for row in 0..h {
        let src = &i420.y[row * stride_y..row * stride_y + w];
        let dst = &mut out[row * nv12_stride..row * nv12_stride + w];
        dst.copy_from_slice(src);
    }

    // Interleave U,V into UV plane at offset stride_y * h
    let uv_offset = nv12_stride * h;
    for row in 0..uv_h {
        let u_src = &i420.u[row * stride_uv..row * stride_uv + w / 2];
        let v_src = &i420.v[row * stride_uv..row * stride_uv + w / 2];
        let dst_row_start = uv_offset + row * nv12_stride;
        let dst = &mut out[dst_row_start..dst_row_start + w];
        for i in 0..(w / 2) {
            dst[i * 2] = u_src[i];
            dst[i * 2 + 1] = v_src[i];
        }
    }

    Ok(out)
}

/// Build a synthetic counter I420 frame matching the BGRA-counter pattern
/// produced by `prdt_media_win::synthetic::bgra_with_counter` (BT.601
/// limited range). The first 64 luma columns of row 0 carry the four bytes
/// of `frame_seq` little-endian, 16 cols per byte; the remainder of the
/// frame is mid-grey. Used by the SW codec full-pipeline bench so the
/// encoder has structured-but-cheap content to compress without needing a
/// D3D11 device.
pub fn make_counter_i420(width: u32, height: u32, frame_seq: u32) -> Result<I420Frame> {
    if width < 64 {
        return Err(MediaSwError::InvalidFrame {
            reason: format!("width {width} < 64; cannot embed 4-byte counter"),
        });
    }
    let mut frame = I420Frame::new_packed(width, height)?;
    // BT.601 limited Y for the (0x33, 0x66, 0x99) BGR background used by
    // bgra_with_counter: Y = ((66*0x99 + 129*0x66 + 25*0x33 + 128) >> 8) + 16
    //                     = ((66*153 + 129*102 + 25*51 + 128) >> 8) + 16
    //                     = ((10098 + 13158 + 1275 + 128) >> 8) + 16
    //                     = (24659 >> 8) + 16 = 96 + 16 = 112.
    let bg_y = 112u8;
    for byte in frame.y.iter_mut() {
        *byte = bg_y;
    }
    // Counter: 4 bytes × 16 luma columns each, in row 0.
    let bytes = frame_seq.to_le_bytes();
    let stride_y = frame.stride_y as usize;
    for (i, &byte) in bytes.iter().enumerate() {
        for p in 0..16 {
            let col = i * 16 + p;
            frame.y[col] = byte;
            // Also copy the same byte into rows 1..=15 so the encoder sees
            // a 16x16 solid block per byte rather than a 1px-tall slit
            // (avoids the encoder smearing the byte against background).
            for row in 1..16 {
                frame.y[row * stride_y + col] = byte;
            }
        }
    }
    // U/V planes: leave at 0 (interpreted as -128, i.e. neutral chroma)
    // for the counter region; full grey body. The decoded counter is read
    // off Y so chroma fidelity isn't load-bearing here.
    for byte in frame.u.iter_mut() {
        *byte = 128;
    }
    for byte in frame.v.iter_mut() {
        *byte = 128;
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_counter_i420_dimensions_and_seed() {
        let f = make_counter_i420(1920, 1080, 0xDEAD_BEEF).expect("counter frame");
        assert_eq!(f.width, 1920);
        assert_eq!(f.height, 1080);
        // Bytes are LE: 0xEF, 0xBE, 0xAD, 0xDE — first 16 luma cols of row 0 = 0xEF.
        assert_eq!(f.y[0], 0xEF);
        assert_eq!(f.y[15], 0xEF);
        assert_eq!(f.y[16], 0xBE);
        assert_eq!(f.y[31], 0xBE);
        assert_eq!(f.y[32], 0xAD);
        assert_eq!(f.y[47], 0xAD);
        assert_eq!(f.y[48], 0xDE);
        assert_eq!(f.y[63], 0xDE);
        // Beyond the counter, background luma = 112.
        assert_eq!(f.y[64], 112);
    }

    #[test]
    fn make_counter_i420_rejects_too_narrow() {
        let err = make_counter_i420(60, 480, 0).unwrap_err();
        assert!(matches!(err, MediaSwError::InvalidFrame { .. }));
    }

    #[test]
    fn bgra_to_i420_round_trip_dimensions() {
        // 1920x1080 BGRA → I420 with the expected plane sizes.
        let w = 1920u32;
        let h = 1080u32;
        let bgra_stride = w * 4;
        let bgra = vec![128u8; (bgra_stride as usize) * (h as usize)];
        let frame = bgra_to_i420(&bgra, w, h, bgra_stride).expect("convert");
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.stride_y, 1920);
        assert_eq!(frame.stride_uv, 960);
        assert_eq!(frame.y.len(), 1920 * 1080);
        assert_eq!(frame.u.len(), 960 * 540);
        assert_eq!(frame.v.len(), 960 * 540);
        // A solid grey input maps to a constant Y around 126 (BT.601 limited).
        // Spot-check a single pixel rather than asserting the exact value to
        // avoid over-constraining the rounding behavior.
        let y0 = frame.y[0];
        assert!(
            (110..=140).contains(&y0),
            "expected mid-grey luma, got {y0}"
        );
    }

    #[test]
    fn bgra_to_i420_rejects_odd_dims() {
        let bgra = vec![0u8; 4 * 3 * 3];
        let err = bgra_to_i420(&bgra, 3, 2, 12).unwrap_err();
        assert!(matches!(err, MediaSwError::InvalidFrame { .. }));
    }

    #[test]
    fn i420_to_nv12_dimensions() {
        let mut frame = I420Frame::new_packed(640, 480).unwrap();
        // Fill U with 0x55 and V with 0xAA so we can assert interleave order.
        for byte in frame.u.iter_mut() {
            *byte = 0x55;
        }
        for byte in frame.v.iter_mut() {
            *byte = 0xAA;
        }
        let nv12 = i420_to_nv12(&frame).unwrap();
        // Y bytes (640*480) plus UV bytes (640 * 240, since stride matches stride_y).
        assert_eq!(nv12.len(), 640 * 480 + 640 * 240);
        // First UV byte should be 0x55 (U), second 0xAA (V).
        let uv_offset = 640 * 480;
        assert_eq!(nv12[uv_offset], 0x55);
        assert_eq!(nv12[uv_offset + 1], 0xAA);
        // Interleaved length per row in bytes equals U.len() + V.len() per row.
        // For one UV row: w/2 U samples + w/2 V samples = w bytes.
        let row_bytes = nv12.len() - uv_offset;
        assert_eq!(row_bytes, frame.u.len() + frame.v.len());
    }
}
