//! Windows FFmpeg `hevc_cuvid` NVDEC Main10 decoder adapter.
//! Sibling of `nvdec_decoder.rs` (8-bit). Key differences:
//!   - Output pixel format: `AV_PIX_FMT_P010LE` (10-bit 4:2:0) after hw_download.
//!   - Emits `Nv12Frame16` (P010LE planes).
//!   - HDR10 SEI sidecar extracted via `prdt_media_ffmpeg::extract_hdr10_sidecar`
//!     (cross-platform parser from crates/media-ffmpeg/src/hdr10_sei.rs).
//!
//! Cargo cfg gate: `#[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]`.

#[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]
mod inner {
    use std::ptr;
    use std::ptr::NonNull;

    use prdt_media_core::{DecodeError, Nv12Frame16};
    use rusty_ffmpeg_win::ffi::{
        av_buffer_ref, av_buffer_unref, av_frame_alloc, av_frame_free, av_frame_unref,
        av_hwdevice_ctx_create, av_hwframe_transfer_data as hw_download, av_packet_alloc,
        av_packet_free, av_packet_unref, avcodec_alloc_context3, avcodec_find_decoder_by_name,
        avcodec_free_context, avcodec_open2, avcodec_receive_frame, avcodec_send_packet,
        AVBufferRef, AVCodecContext, AVFrame, AVPacket, AV_HWDEVICE_TYPE_CUDA,
    };

    use crate::error::MediaError;

    // AVERROR(EAGAIN) = -11 on Windows (same POSIX mapping as Linux).
    const AVERROR_EAGAIN: i32 = -11;
    // AVERROR_EOF = FFERRTAG('E','O','F',' ') negated.
    const AVERROR_EOF: i32 = -0x20464F45;

    pub struct HevcNvdecMain10FfmpegDecoderWindowsConfig {
        pub width: u32,
        pub height: u32,
        /// CUDA device index. Reserved for future multi-GPU selection; currently
        /// unused. Tracked as ADR follow-up F3.
        pub cuda_device_index: Option<u32>,
    }

    impl Default for HevcNvdecMain10FfmpegDecoderWindowsConfig {
        fn default() -> Self {
            Self {
                width: 1920,
                height: 1080,
                cuda_device_index: None,
            }
        }
    }

    pub struct HevcNvdecMain10FfmpegDecoderWindows {
        // Owns the top-level CUDA HW device buffer ref; freed in Drop.
        hw_device_buf: NonNull<AVBufferRef>,
        codec_ctx: NonNull<AVCodecContext>,
        hw_frame: NonNull<AVFrame>,
        sw_frame: NonNull<AVFrame>,
        packet: NonNull<AVPacket>,
    }

    // SAFETY: HevcNvdecMain10FfmpegDecoderWindows owns its libavcodec + CUDA resources
    // exclusively via NonNull pointers; never aliased; decode pipeline runs single-threaded.
    unsafe impl Send for HevcNvdecMain10FfmpegDecoderWindows {}

    impl HevcNvdecMain10FfmpegDecoderWindows {
        pub fn new(cfg: HevcNvdecMain10FfmpegDecoderWindowsConfig) -> Result<Self, MediaError> {
            // Probe hevc_cuvid availability first.
            // SAFETY: string literal is a valid nul-terminated C string.
            let codec = unsafe { avcodec_find_decoder_by_name(c"hevc_cuvid".as_ptr()) };
            if codec.is_null() {
                return Err(MediaError::DecoderNotAvailable {
                    codec: "hevc_cuvid".into(),
                    reason: "avcodec_find_decoder_by_name returned null — \
                             NVIDIA GPU or driver not present"
                        .into(),
                });
            }

            // Open CUDA HW device.
            let mut hw_device_ptr: *mut AVBufferRef = ptr::null_mut();
            // SAFETY: hw_device_ptr is a local out-param; null device path = CUDA default.
            let ret = unsafe {
                av_hwdevice_ctx_create(
                    &mut hw_device_ptr,
                    AV_HWDEVICE_TYPE_CUDA,
                    ptr::null(),
                    ptr::null_mut(),
                    0,
                )
            };
            if ret < 0 {
                return Err(MediaError::Other(format!(
                    "av_hwdevice_ctx_create(CUDA) returned {ret}"
                )));
            }
            // SAFETY: av_hwdevice_ctx_create succeeded; ptr is non-null.
            let hw_device_buf = unsafe { NonNull::new_unchecked(hw_device_ptr) };

            // Allocate codec context.
            // SAFETY: codec is non-null from avcodec_find_decoder_by_name.
            let codec_ctx_ptr = unsafe { avcodec_alloc_context3(codec) };
            if codec_ctx_ptr.is_null() {
                let mut d = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner.
                unsafe { av_buffer_unref(&mut d) };
                return Err(MediaError::Other(
                    "avcodec_alloc_context3 returned null".into(),
                ));
            }

            // Attach device context ref (bump refcount so codec_ctx + hw_device_buf each hold one).
            // SAFETY: hw_device_buf.as_ptr() is a valid AVBufferRef.
            let dev_ref = unsafe { av_buffer_ref(hw_device_buf.as_ptr()) };
            if dev_ref.is_null() {
                let mut c = codec_ctx_ptr;
                // SAFETY: codec_ctx_ptr is the unique owner.
                unsafe { avcodec_free_context(&mut c) };
                let mut d = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner.
                unsafe { av_buffer_unref(&mut d) };
                return Err(MediaError::Other("av_buffer_ref returned null".into()));
            }
            // SAFETY: codec_ctx_ptr is freshly allocated and unopened.
            unsafe {
                (*codec_ctx_ptr).hw_device_ctx = dev_ref;
                (*codec_ctx_ptr).width = cfg.width as i32;
                (*codec_ctx_ptr).height = cfg.height as i32;
            }

            // Open codec.
            // SAFETY: codec_ctx_ptr is valid; no priv_data dict needed for NVDEC.
            let ret = unsafe { avcodec_open2(codec_ctx_ptr, codec, ptr::null_mut()) };
            if ret < 0 {
                let mut c = codec_ctx_ptr;
                // SAFETY: codec_ctx_ptr is the unique owner (hw_device_ctx ref freed on context free).
                unsafe { avcodec_free_context(&mut c) };
                let mut d = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner of the top-level ref.
                unsafe { av_buffer_unref(&mut d) };
                return Err(MediaError::Other(format!("avcodec_open2 returned {ret}")));
            }
            // SAFETY: avcodec_alloc_context3 + avcodec_open2 succeeded; pointer is non-null.
            let codec_ctx = unsafe { NonNull::new_unchecked(codec_ctx_ptr) };

            // Allocate hw_frame.
            // SAFETY: av_frame_alloc returns non-null or null on OOM.
            let hw_frame_ptr = unsafe { av_frame_alloc() };
            if hw_frame_ptr.is_null() {
                let mut c = codec_ctx.as_ptr();
                // SAFETY: codec_ctx is the unique owner.
                unsafe { avcodec_free_context(&mut c) };
                let mut d = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner.
                unsafe { av_buffer_unref(&mut d) };
                return Err(MediaError::Other(
                    "av_frame_alloc (hw) returned null".into(),
                ));
            }
            // SAFETY: hw_frame_ptr is non-null.
            let hw_frame = unsafe { NonNull::new_unchecked(hw_frame_ptr) };

            // Allocate sw_frame.
            // SAFETY: av_frame_alloc returns non-null or null on OOM.
            let sw_frame_ptr = unsafe { av_frame_alloc() };
            if sw_frame_ptr.is_null() {
                let mut hp = hw_frame.as_ptr();
                // SAFETY: hw_frame is the unique owner.
                unsafe { av_frame_free(&mut hp) };
                let mut c = codec_ctx.as_ptr();
                // SAFETY: codec_ctx is the unique owner.
                unsafe { avcodec_free_context(&mut c) };
                let mut d = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner.
                unsafe { av_buffer_unref(&mut d) };
                return Err(MediaError::Other(
                    "av_frame_alloc (sw) returned null".into(),
                ));
            }
            // SAFETY: sw_frame_ptr is non-null.
            let sw_frame = unsafe { NonNull::new_unchecked(sw_frame_ptr) };

            // Allocate packet.
            // SAFETY: av_packet_alloc returns zeroed AVPacket or null.
            let packet_ptr = unsafe { av_packet_alloc() };
            if packet_ptr.is_null() {
                let mut sp = sw_frame.as_ptr();
                // SAFETY: sw_frame is the unique owner.
                unsafe { av_frame_free(&mut sp) };
                let mut hp = hw_frame.as_ptr();
                // SAFETY: hw_frame is the unique owner.
                unsafe { av_frame_free(&mut hp) };
                let mut c = codec_ctx.as_ptr();
                // SAFETY: codec_ctx is the unique owner.
                unsafe { avcodec_free_context(&mut c) };
                let mut d = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner.
                unsafe { av_buffer_unref(&mut d) };
                return Err(MediaError::Other("av_packet_alloc returned null".into()));
            }
            // SAFETY: packet_ptr is non-null.
            let packet = unsafe { NonNull::new_unchecked(packet_ptr) };

            tracing::info!(
                target: "video.pipeline",
                event = "decoder_ready",
                backend = "ffmpeg-nvdec-hevc-main10-win",
                codec = "h265",
                profile = "main10",
                bitdepth = 10,
            );

            Ok(Self {
                hw_device_buf,
                codec_ctx,
                hw_frame,
                sw_frame,
                packet,
            })
        }

        pub fn feed_packet(&mut self, data: &[u8], pts_us: u64) -> Result<(), DecodeError> {
            if data.is_empty() {
                return Ok(());
            }
            let pkt = self.packet.as_ptr();
            // SAFETY: pkt is the unique AVPacket owned by self; libavcodec copies bytes
            // synchronously inside avcodec_send_packet when buf is not set.
            unsafe {
                (*pkt).data = data.as_ptr() as *mut u8;
                (*pkt).size = data.len() as i32;
                (*pkt).pts = pts_us as i64;
                (*pkt).dts = pts_us as i64;
            }
            let ctx = self.codec_ctx.as_ptr();
            // SAFETY: ctx is a valid open AVCodecContext; pkt is valid.
            let ret = unsafe { avcodec_send_packet(ctx, pkt) };
            // SAFETY: pkt is the unique owner; clear out our borrowed-slice pointer.
            unsafe {
                (*pkt).data = ptr::null_mut();
                (*pkt).size = 0;
                av_packet_unref(pkt);
            }
            if ret < 0 && ret != AVERROR_EAGAIN {
                return Err(DecodeError::Bitstream(format!(
                    "avcodec_send_packet returned {ret}"
                )));
            }
            Ok(())
        }

        pub fn drain_frame(&mut self) -> Result<Option<Nv12Frame16>, DecodeError> {
            let ctx = self.codec_ctx.as_ptr();
            let hw = self.hw_frame.as_ptr();
            let sw = self.sw_frame.as_ptr();

            // SAFETY: ctx is open; hw is a valid AVFrame.
            let ret = unsafe { avcodec_receive_frame(ctx, hw) };
            if ret == AVERROR_EAGAIN || ret == AVERROR_EOF {
                return Ok(None);
            }
            if ret < 0 {
                return Err(DecodeError::Backend(format!(
                    "avcodec_receive_frame returned {ret}"
                )));
            }

            // CUDA → CPU readback. This is the per-file single hw_download call site.
            // SAFETY: hw is a valid CUDA surface; sw is an empty AVFrame; 0 flags = API contract.
            let xfer = unsafe { hw_download(sw, hw, 0) };
            if xfer < 0 {
                // SAFETY: hw is the unique owner.
                unsafe { av_frame_unref(hw) };
                return Err(DecodeError::Backend(format!(
                    "av_hwframe_transfer_data returned {xfer}"
                )));
            }

            // Extract HDR10 sidecar from the SW frame (side-data propagated by hw_download).
            // SAFETY: sw is a valid AVFrame after successful hw_download.
            let hdr10 = unsafe { prdt_media_ffmpeg::extract_hdr10_sidecar(sw as *const _) };

            // SAFETY: hw_download populated sw with CPU-side P010LE planes.
            let (y_ptr, uv_ptr, y_stride_bytes, uv_stride_bytes, w, h, pts) = unsafe {
                let f = &*sw;
                (
                    f.data[0] as *const u8,
                    f.data[1] as *const u8,
                    f.linesize[0] as usize,
                    f.linesize[1] as usize,
                    f.width as u32,
                    f.height as u32,
                    f.pts as u64,
                )
            };

            // SAFETY: copy_p010_planes copies bytes out of sw's P010LE planes into owned Vecs.
            let out = unsafe {
                copy_p010_planes(
                    y_ptr,
                    uv_ptr,
                    y_stride_bytes,
                    uv_stride_bytes,
                    w,
                    h,
                    pts,
                    hdr10,
                )
            };

            // SAFETY: hw and sw are the unique owners.
            unsafe {
                av_frame_unref(sw);
                av_frame_unref(hw);
            }

            Ok(Some(out))
        }

        pub fn backend_name(&self) -> &'static str {
            "ffmpeg-nvdec-hevc-main10-win"
        }
    }

    impl Drop for HevcNvdecMain10FfmpegDecoderWindows {
        fn drop(&mut self) {
            let mut pkt = self.packet.as_ptr();
            // SAFETY: packet is the unique owner.
            unsafe { av_packet_free(&mut pkt) };
            let mut sp = self.sw_frame.as_ptr();
            // SAFETY: sw_frame is the unique owner.
            unsafe { av_frame_free(&mut sp) };
            let mut hp = self.hw_frame.as_ptr();
            // SAFETY: hw_frame is the unique owner.
            unsafe { av_frame_free(&mut hp) };
            let mut ctx = self.codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner; the hw_device_ctx ref attached via
            // av_buffer_ref is freed by avcodec_free_context.
            unsafe { avcodec_free_context(&mut ctx) };
            let mut d = self.hw_device_buf.as_ptr();
            // SAFETY: hw_device_buf is the unique owner of the top-level CUDA device ref.
            unsafe { av_buffer_unref(&mut d) };
        }
    }

    /// Copy an `AVFrame`'s P010LE planes into an owned `Nv12Frame16`.
    /// P010LE stores each sample as a `u16` with valid 10 bits in the high part.
    /// `linesize` is in bytes; divide by 2 to get element counts.
    ///
    /// # Safety
    /// - `y_ptr` must be a valid readable pointer to `y_stride_bytes * height` bytes.
    /// - `uv_ptr` must be a valid readable pointer to `uv_stride_bytes * (height/2)` bytes.
    /// - Both pointers must outlive the function call.
    #[allow(clippy::too_many_arguments)]
    unsafe fn copy_p010_planes(
        y_ptr: *const u8,
        uv_ptr: *const u8,
        y_stride_bytes: usize,
        uv_stride_bytes: usize,
        width: u32,
        height: u32,
        pts_us: u64,
        hdr10: Option<prdt_media_core::Hdr10Metadata>,
    ) -> Nv12Frame16 {
        let h = height as usize;
        let y_elems = y_stride_bytes / 2;
        let uv_elems = uv_stride_bytes / 2;
        let mut y = vec![0u16; y_elems * h];
        let mut uv = vec![0u16; uv_elems * (h / 2)];
        // SAFETY: caller guarantees y_ptr/uv_ptr are readable; dst Vecs are freshly allocated.
        unsafe {
            std::ptr::copy_nonoverlapping(y_ptr as *const u16, y.as_mut_ptr(), y_elems * h);
            std::ptr::copy_nonoverlapping(
                uv_ptr as *const u16,
                uv.as_mut_ptr(),
                uv_elems * (h / 2),
            );
        }
        Nv12Frame16 {
            width,
            height,
            y,
            uv,
            stride_y: y_elems as u32,
            stride_uv: uv_elems as u32,
            pts_us,
            hdr10,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn context_creation_no_gpu_main10() {
            // On CI (windows-latest, no NVIDIA GPU), avcodec_find_decoder_by_name
            // returns null → expect DecoderNotAvailable, never panic.
            let codec = unsafe { avcodec_find_decoder_by_name(c"hevc_cuvid".as_ptr()) };
            if codec.is_null() {
                let result = HevcNvdecMain10FfmpegDecoderWindows::new(
                    HevcNvdecMain10FfmpegDecoderWindowsConfig::default(),
                );
                // The adapter holds raw FFmpeg pointers and does not derive Debug;
                // project to result.as_ref().err() (which IS Debug) for the message.
                assert!(
                    matches!(result, Err(MediaError::DecoderNotAvailable { .. })),
                    "expected DecoderNotAvailable when hevc_cuvid not found, got Err: {:?}",
                    result.as_ref().err()
                );
            }
        }

        #[test]
        fn default_config_fields_main10() {
            let cfg = HevcNvdecMain10FfmpegDecoderWindowsConfig::default();
            assert_eq!(cfg.width, 1920);
            assert_eq!(cfg.height, 1080);
            assert!(cfg.cuda_device_index.is_none());
        }
    }
}

#[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]
pub use inner::{HevcNvdecMain10FfmpegDecoderWindows, HevcNvdecMain10FfmpegDecoderWindowsConfig};
