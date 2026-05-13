//! Encoder input discriminator. Only `CpuI420` is wired in P5C-1;
//! `VaSurface` / `Dmabuf` arms exist to lock the seam for P5C-2
//! (DMABUF zero-copy).

use prdt_media_sw::I420Frame;

#[allow(dead_code)] // VaSurface/Dmabuf placeholders unused in P5C-1
pub enum FrameInput<'a> {
    /// CPU-resident planar YUV. The host's bgra_to_i420 step produces this.
    CpuI420(&'a I420Frame),

    /// Reserved for P5C-2: already-mapped libva Surface (zero-copy from
    /// DMABUF). The encoder skips its internal upload step and binds the
    /// surface directly.
    VaSurface,

    /// Reserved for P5C-2: DMABUF FDs + plane descriptors. The encoder
    /// constructs a libva Surface via vaCreateSurfaceFromFds.
    Dmabuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_media_sw::I420Frame;

    #[test]
    fn cpu_i420_holds_borrow_lifetime() {
        let f = I420Frame::new_packed(2, 2).expect("valid dims");
        let _input = FrameInput::CpuI420(&f);
        // Smoke: the enum compiles + holds a borrow.
    }
}
