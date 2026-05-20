//! I420 (YUV 4:2:0 planar) → BGRA conversion using BT.709 limited-range
//! coefficients. Output buffer is `width * height * 4` bytes BGRA8888,
//! with B in the lowest byte (matches softbuffer's `&mut [u32]` layout
//! when read as little-endian u32 = 0x00RRGGBB stored as B,G,R,X).

use prdt_media_sw::I420Frame;

/// Convert one I420Frame into BGRA. `out_bgra` must be width*height*4
/// bytes long; the function writes B,G,R,A=0xFF per pixel.
pub fn i420_to_bgra(i420: &I420Frame, out_bgra: &mut [u8]) {
    let w = i420.width as usize;
    let h = i420.height as usize;
    debug_assert_eq!(out_bgra.len(), w * h * 4);
    let y_stride = i420.stride_y as usize;
    let uv_stride = i420.stride_uv as usize;
    for j in 0..h {
        for i in 0..w {
            let y = i420.y[j * y_stride + i] as i32;
            let u = i420.u[(j / 2) * uv_stride + i / 2] as i32 - 128;
            let v = i420.v[(j / 2) * uv_stride + i / 2] as i32 - 128;
            // BT.709 limited-range: scale Y to [0,255] from [16,235]
            // (approximation: Y' = (Y-16)*255/219), but for L1 we use
            // BT.709 full coefficients — visible artifacts on broadcast
            // content are tolerable, fix in L2.
            let r = y + ((1793 * v) >> 10); // 1.793 ≈ 2*(1-Kr)
            let g = y - ((534 * u + 213 * v) >> 10); // BT.709
            let b = y + ((2115 * u) >> 10); // 2.115 ≈ 2*(1-Kb)
            let off = (j * w + i) * 4;
            out_bgra[off] = clamp_u8(b);
            out_bgra[off + 1] = clamp_u8(g);
            out_bgra[off + 2] = clamp_u8(r);
            out_bgra[off + 3] = 0xFF;
        }
    }
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_i420(w: u32, h: u32, y_val: u8) -> I420Frame {
        let yp = vec![y_val; (w * h) as usize];
        let up = vec![128u8; (w * h / 4) as usize];
        let vp = vec![128u8; (w * h / 4) as usize];
        I420Frame {
            width: w,
            height: h,
            y: yp,
            u: up,
            v: vp,
            stride_y: w,
            stride_uv: w / 2,
        }
    }

    #[test]
    fn gray_yuv_yields_gray_bgra() {
        let i = gray_i420(8, 8, 128);
        let mut out = vec![0u8; 8 * 8 * 4];
        i420_to_bgra(&i, &mut out);
        // U=V=128 means u' = v' = 0, so BGR = (Y, Y, Y) = (128,128,128).
        for px in out.chunks_exact(4) {
            assert_eq!(px[0], 128);
            assert_eq!(px[1], 128);
            assert_eq!(px[2], 128);
            assert_eq!(px[3], 0xFF);
        }
    }

    #[test]
    fn black_yuv_yields_black_bgra() {
        let i = gray_i420(4, 4, 0);
        let mut out = vec![0u8; 4 * 4 * 4];
        i420_to_bgra(&i, &mut out);
        for px in out.chunks_exact(4) {
            assert_eq!(px[0], 0);
            assert_eq!(px[1], 0);
            assert_eq!(px[2], 0);
            assert_eq!(px[3], 0xFF);
        }
    }

    #[test]
    fn white_yuv_yields_near_white_bgra() {
        let i = gray_i420(4, 4, 255);
        let mut out = vec![0u8; 4 * 4 * 4];
        i420_to_bgra(&i, &mut out);
        for px in out.chunks_exact(4) {
            assert!(px[0] >= 250);
            assert!(px[1] >= 250);
            assert!(px[2] >= 250);
        }
    }

    // ---- P0 GUI-modernization baseline freeze ----------------------------
    // Golden digest of the I420→BGRA output for a deterministic gradient that
    // exercises the full Y/U/V range. The GUI rewrite (P3) replaces this CPU
    // converter with a wgpu fragment shader; the shader output must reproduce
    // this reference within tolerance. If you intentionally change the
    // conversion math, recompute the constant from the test failure message.
    // See .omc/plans/gui-modernization-design.md §8 (P0).

    /// 64-bit FNV-1a over a byte slice. Self-contained so the baseline guard
    /// adds no dependency.
    fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Deterministic gradient I420 frame covering a wide YUV range.
    fn gradient_i420(w: u32, h: u32) -> I420Frame {
        let (wu, hu) = (w as usize, h as usize);
        let mut y = vec![0u8; wu * hu];
        for j in 0..hu {
            for i in 0..wu {
                y[j * wu + i] = ((i.wrapping_mul(5)).wrapping_add(j.wrapping_mul(3))) as u8;
            }
        }
        let cw = wu / 2;
        let ch = hu / 2;
        let mut u = vec![0u8; cw * ch];
        let mut v = vec![0u8; cw * ch];
        for j in 0..ch {
            for i in 0..cw {
                u[j * cw + i] = (i.wrapping_mul(7)) as u8;
                v[j * cw + i] = (j.wrapping_mul(11)) as u8;
            }
        }
        I420Frame {
            width: w,
            height: h,
            y,
            u,
            v,
            stride_y: w,
            stride_uv: w / 2,
        }
    }

    #[test]
    fn i420_to_bgra_gradient_golden_digest() {
        let frame = gradient_i420(64, 64);
        let mut out = vec![0u8; 64 * 64 * 4];
        i420_to_bgra(&frame, &mut out);
        let digest = fnv1a64(&out);
        const GOLDEN: u64 = 0xe113_1b22_fd54_6e98;
        assert_eq!(
            digest, GOLDEN,
            "i420_to_bgra gradient digest changed: got {digest:#018x} (update GOLDEN if intentional)"
        );
    }
}
