//! I420 (YUV 4:2:0 planar) → BGRA conversion using BT.709 limited-range
//! coefficients. Output buffer is `width * height * 4` bytes BGRA8888,
//! with B in the lowest byte (matches softbuffer's `&mut [u32]` layout
//! when read as little-endian u32 = 0x00RRGGBB stored as B,G,R,X).

use prdt_media_sw::I420Frame;

/// Convert one I420Frame into BGRA. `out_bgra` must be width*height*4
/// bytes long; the function writes B,G,R,A=0xFF per pixel.
///
/// Returns `Err` (instead of panicking) when the frame's plane lengths or the
/// output buffer are inconsistent with its `width`/`height`/`stride` geometry —
/// a decoded frame whose size disagrees with the render buffer must be skipped
/// by the caller, never allowed to abort the viewer. The `debug_assert` that
/// used to guard this is compiled out of the release viewer, so the bounds are
/// checked explicitly here.
pub fn i420_to_bgra(i420: &I420Frame, out_bgra: &mut [u8]) -> Result<(), String> {
    let w = i420.width as usize;
    let h = i420.height as usize;
    let y_stride = i420.stride_y as usize;
    let uv_stride = i420.stride_uv as usize;
    let expect_out = w.saturating_mul(h).saturating_mul(4);
    if out_bgra.len() != expect_out {
        return Err(format!(
            "i420_to_bgra: out buffer {} bytes but geometry {w}x{h} needs {expect_out}",
            out_bgra.len()
        ));
    }
    if w != 0 && h != 0 {
        // Worst-case indices reached on the final pixel (w-1, h-1):
        // luma (h-1)*stride_y + (w-1); chroma ((h-1)/2)*stride_uv + (w-1)/2.
        let y_need = (h - 1) * y_stride + w;
        let c_need = ((h - 1) / 2) * uv_stride + (w - 1) / 2 + 1;
        if i420.y.len() < y_need {
            return Err(format!(
                "i420_to_bgra: Y plane {} < {y_need} needed for {w}x{h} stride_y={y_stride}",
                i420.y.len()
            ));
        }
        if i420.u.len() < c_need || i420.v.len() < c_need {
            return Err(format!(
                "i420_to_bgra: U/V plane {}/{} < {c_need} needed for {w}x{h} stride_uv={uv_stride}",
                i420.u.len(),
                i420.v.len()
            ));
        }
    }
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
    Ok(())
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
        i420_to_bgra(&i, &mut out).expect("consistent frame converts");
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
        i420_to_bgra(&i, &mut out).expect("consistent frame converts");
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
        i420_to_bgra(&i, &mut out).expect("consistent frame converts");
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
        i420_to_bgra(&frame, &mut out).expect("consistent frame converts");
        let digest = fnv1a64(&out);
        const GOLDEN: u64 = 0xe113_1b22_fd54_6e98;
        assert_eq!(
            digest, GOLDEN,
            "i420_to_bgra gradient digest changed: got {digest:#018x} (update GOLDEN if intentional)"
        );
    }

    /// A frame header claiming 3840x2160 but carrying only 1920x1080-worth of
    /// chroma must be rejected, not panic. Guards the same class of bug as the
    /// NV12 viewer crash: a decoded size that disagrees with the plane buffers.
    #[test]
    fn i420_to_bgra_rejects_short_planes() {
        let (w, h) = (3840u32, 2160u32);
        // Y sized correctly for 3840x2160, but U/V hold only a 1920x1080 frame.
        let y = vec![16u8; (w * h) as usize];
        let u = vec![128u8; (1920 * 1080 / 4) as usize];
        let v = vec![128u8; (1920 * 1080 / 4) as usize];
        let frame = I420Frame {
            width: w,
            height: h,
            y,
            u,
            v,
            stride_y: w,
            stride_uv: w / 2,
        };
        // out buffer is the header-consistent size, so only the short planes
        // can trip the guard.
        let mut out = vec![0u8; (w * h * 4) as usize];
        let err = i420_to_bgra(&frame, &mut out).expect_err("short U/V plane must be rejected");
        assert!(
            err.contains("U/V plane"),
            "error should name the short chroma plane, got: {err}"
        );
    }

    /// A small, fully consistent frame with stride padding converts Ok and
    /// fills the entire output buffer (no pixel left at the sentinel).
    #[test]
    fn i420_to_bgra_ok_with_padded_stride() {
        let (w, h) = (64u32, 36u32);
        let (stride_y, stride_uv) = (80u32, 40u32); // padded beyond w / w/2
        let y = vec![120u8; (stride_y * h) as usize];
        let u = vec![128u8; (stride_uv * (h / 2)) as usize];
        let v = vec![128u8; (stride_uv * (h / 2)) as usize];
        let frame = I420Frame {
            width: w,
            height: h,
            y,
            u,
            v,
            stride_y,
            stride_uv,
        };
        let mut out = vec![7u8; (w * h * 4) as usize];
        i420_to_bgra(&frame, &mut out).expect("consistent padded frame converts");
        // Every alpha byte written => whole buffer covered.
        assert!(
            out.chunks_exact(4).all(|px| px[3] == 0xFF),
            "converter must write every output pixel"
        );
    }
}
