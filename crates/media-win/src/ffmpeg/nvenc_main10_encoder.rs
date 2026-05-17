//! Windows `hevc_nvenc` FFmpeg Main10 + HDR10 encoder adapter. Sibling of
//! `nvenc_encoder.rs` (8-bit). Key differences from the 8-bit sibling:
//!   - HW frames pool uses `sw_format = AV_PIX_FMT_P010LE` (10-bit 4:2:0).
//!   - CPU-side conversion: BGRA8→P010LE via `sws_scale`.
//!   - Profile: `AV_PROFILE_HEVC_MAIN_10` (2); HDR10 VUI/SEI color metadata
//!     set on the codec context.
//!   - No BSF chain — `hevc_nvenc` emits Annex-B natively (same as 8-bit).
//!
//! `HevcNvencMain10FfmpegEncoderWindowsAdapter` implements `Hevc265Encoder`.
//! Cargo cfg gate: `#[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]`.

#[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
mod inner {
    use std::ffi::CString;
    use std::ptr;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use rusty_ffmpeg_win::ffi::{
        av_buffer_ref, av_buffer_unref, av_dict_set, av_frame_alloc, av_frame_free,
        av_frame_get_buffer, av_hwdevice_ctx_create, av_hwframe_ctx_alloc, av_hwframe_ctx_init,
        av_hwframe_get_buffer, av_hwframe_transfer_data as hw_upload, av_opt_set_int,
        av_packet_alloc, av_packet_free, av_packet_unref, avcodec_alloc_context3,
        avcodec_find_encoder_by_name, avcodec_free_context, avcodec_open2, avcodec_receive_packet,
        avcodec_send_frame, sws_freeContext, sws_getContext, sws_scale, AVBufferRef,
        AVCodecContext, AVDictionary, AVFrame, AVHWFramesContext, AVRational, AVCOL_PRI_BT2020,
        AVCOL_RANGE_MPEG, AVCOL_SPC_BT2020_NCL, AVCOL_TRC_SMPTE2084, AV_CODEC_FLAG_GLOBAL_HEADER,
        AV_HWDEVICE_TYPE_CUDA, AV_OPT_SEARCH_CHILDREN, AV_PICTURE_TYPE_I, AV_PIX_FMT_BGRA,
        AV_PIX_FMT_CUDA, AV_PIX_FMT_P010LE, AV_PKT_FLAG_KEY, SWS_BILINEAR,
    };

    use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
    use crate::encoder_trait::{EncodedH265Frame, Hevc265Encoder};
    use crate::error::MediaError;

    // HEVC Main10 profile = 2 (AV_PROFILE_HEVC_MAIN_10, same across FFmpeg 5/6/7).
    const AV_PROFILE_HEVC_MAIN_10: i32 = 2;
    // AVERROR(EAGAIN) = -11 on Windows (same POSIX mapping as Linux).
    const AVERROR_EAGAIN: i32 = -11;

    pub struct HevcNvencMain10FfmpegEncoderWindowsAdapterConfig {
        pub width: u32,
        pub height: u32,
        pub fps: u32,
        pub initial_bitrate_bps: u32,
        pub gop_size: u32,
    }

    impl Default for HevcNvencMain10FfmpegEncoderWindowsAdapterConfig {
        fn default() -> Self {
            Self {
                width: 1920,
                height: 1080,
                fps: 60,
                initial_bitrate_bps: 8_000_000,
                gop_size: 60,
            }
        }
    }

    pub struct HevcNvencMain10FfmpegEncoderWindowsAdapter {
        // D3D11 state for GPU→CPU texture readback.
        device: D3d11Device,
        staging: D3d11Texture,
        bgra_buf: Vec<u8>,

        // FFmpeg CUDA encoder state.
        hw_device_buf: NonNull<AVBufferRef>,
        // P010LE hw frames pool buffer ref; kept alive for hw_frame lifetime.
        frames_buf: NonNull<AVBufferRef>,
        codec_ctx: NonNull<AVCodecContext>,
        // CPU-side P010LE frame written by sws_scale, then uploaded via hw_upload.
        cpu_frame: NonNull<AVFrame>,
        hw_frame: NonNull<AVFrame>,
        // sws_scale context for BGRA8→P010LE conversion.
        sws_ctx: *mut rusty_ffmpeg_win::ffi::SwsContext,

        width: u32,
        height: u32,
        fps: u32,
        seq: u64,
        first_frame_logged: AtomicBool,
        last_bitrate_warn_secs: AtomicU64,
    }

    impl HevcNvencMain10FfmpegEncoderWindowsAdapter {
        pub fn new(
            device: D3d11Device,
            cfg: HevcNvencMain10FfmpegEncoderWindowsAdapterConfig,
        ) -> Result<Self, MediaError> {
            // Fail-fast: probe hevc_nvenc availability before allocating anything.
            // SAFETY: string literal is a valid nul-terminated C string.
            let codec = unsafe { avcodec_find_encoder_by_name(c"hevc_nvenc".as_ptr()) };
            if codec.is_null() {
                return Err(MediaError::EncoderNotAvailable {
                    codec: "hevc_nvenc".into(),
                    reason: "avcodec_find_encoder_by_name returned null — \
                             NVIDIA GPU or driver not present"
                        .into(),
                });
            }

            // 1. Open CUDA HW device.
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

            // 2. Allocate CUDA HW frames pool with P010LE sw_format.
            // SAFETY: hw_device_buf.as_ptr() is a valid AVHWDeviceContext buffer ref.
            let mut frames_raw_ptr = unsafe { av_hwframe_ctx_alloc(hw_device_buf.as_ptr()) };
            if frames_raw_ptr.is_null() {
                let mut p = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner.
                unsafe { av_buffer_unref(&mut p) };
                return Err(MediaError::Other(
                    "av_hwframe_ctx_alloc returned null".into(),
                ));
            }
            // SAFETY: frames_raw_ptr is non-null; data points to AVHWFramesContext.
            unsafe {
                let ctx = (*frames_raw_ptr).data as *mut AVHWFramesContext;
                (*ctx).format = AV_PIX_FMT_CUDA;
                (*ctx).sw_format = AV_PIX_FMT_P010LE;
                (*ctx).width = cfg.width as i32;
                (*ctx).height = cfg.height as i32;
                (*ctx).initial_pool_size = 4;
            }
            // SAFETY: frames_raw_ptr is a valid uninitialised AVHWFramesContext buf ref.
            let init_ret = unsafe { av_hwframe_ctx_init(frames_raw_ptr) };
            if init_ret < 0 {
                // SAFETY: frames_raw_ptr is the unique owner.
                unsafe { av_buffer_unref(&mut frames_raw_ptr) };
                let mut p = hw_device_buf.as_ptr();
                // SAFETY: hw_device_buf is the unique owner.
                unsafe { av_buffer_unref(&mut p) };
                return Err(MediaError::Other(format!(
                    "av_hwframe_ctx_init returned {init_ret}"
                )));
            }
            // SAFETY: init succeeded; frames_raw_ptr is non-null.
            let frames_buf = unsafe { NonNull::new_unchecked(frames_raw_ptr) };

            // 3. Open codec context for Main10.
            // SAFETY: av_buffer_ref bumps refcount so open_nvenc_main10_codec_ctx can consume.
            let frames_ref_ptr = unsafe { av_buffer_ref(frames_buf.as_ptr()) };
            let frames_ref = match NonNull::new(frames_ref_ptr) {
                Some(r) => r,
                None => {
                    let mut f = frames_buf.as_ptr();
                    unsafe { av_buffer_unref(&mut f) };
                    let mut p = hw_device_buf.as_ptr();
                    unsafe { av_buffer_unref(&mut p) };
                    return Err(MediaError::Other("av_buffer_ref returned null".into()));
                }
            };
            let codec_ctx = match open_nvenc_main10_codec_ctx(
                codec,
                cfg.width,
                cfg.height,
                cfg.fps,
                cfg.initial_bitrate_bps,
                cfg.gop_size,
                frames_ref,
            ) {
                Ok(c) => c,
                Err(e) => {
                    // frames_ref was consumed by open_nvenc_main10_codec_ctx on error.
                    let mut f = frames_buf.as_ptr();
                    unsafe { av_buffer_unref(&mut f) };
                    let mut p = hw_device_buf.as_ptr();
                    unsafe { av_buffer_unref(&mut p) };
                    return Err(e);
                }
            };

            // 4a. Allocate cpu_frame (P010LE, software side for sws_scale output).
            // SAFETY: av_frame_alloc returns zeroed AVFrame or null on OOM.
            let cpu_ptr = unsafe {
                let f = av_frame_alloc();
                if f.is_null() {
                    let mut c = codec_ctx.as_ptr();
                    avcodec_free_context(&mut c);
                    let mut fb = frames_buf.as_ptr();
                    av_buffer_unref(&mut fb);
                    let mut p = hw_device_buf.as_ptr();
                    av_buffer_unref(&mut p);
                    return Err(MediaError::Other(
                        "av_frame_alloc (cpu) returned null".into(),
                    ));
                }
                (*f).format = AV_PIX_FMT_P010LE;
                (*f).width = cfg.width as i32;
                (*f).height = cfg.height as i32;
                // SAFETY: frame fields are set; 32-byte alignment is safe for P010LE.
                let ret = av_frame_get_buffer(f, 32);
                if ret < 0 {
                    av_frame_free(&mut { f });
                    let mut c = codec_ctx.as_ptr();
                    avcodec_free_context(&mut c);
                    let mut fb = frames_buf.as_ptr();
                    av_buffer_unref(&mut fb);
                    let mut p = hw_device_buf.as_ptr();
                    av_buffer_unref(&mut p);
                    return Err(MediaError::Other(format!(
                        "av_frame_get_buffer returned {ret}"
                    )));
                }
                f
            };
            // SAFETY: cpu_ptr is non-null after successful av_frame_get_buffer.
            let cpu_frame = unsafe { NonNull::new_unchecked(cpu_ptr) };

            // 4b. Allocate hw_frame (CUDA surface from P010LE pool).
            // SAFETY: frames_buf.as_ptr() is the valid frames buffer.
            let hw_ptr = unsafe {
                let f = av_frame_alloc();
                if f.is_null() {
                    let mut cp = cpu_frame.as_ptr();
                    av_frame_free(&mut cp);
                    let mut c = codec_ctx.as_ptr();
                    avcodec_free_context(&mut c);
                    let mut fb = frames_buf.as_ptr();
                    av_buffer_unref(&mut fb);
                    let mut p = hw_device_buf.as_ptr();
                    av_buffer_unref(&mut p);
                    return Err(MediaError::Other(
                        "av_frame_alloc (hw) returned null".into(),
                    ));
                }
                let ret = av_hwframe_get_buffer(frames_buf.as_ptr(), f, 0);
                if ret < 0 {
                    av_frame_free(&mut { f });
                    let mut cp = cpu_frame.as_ptr();
                    av_frame_free(&mut cp);
                    let mut c = codec_ctx.as_ptr();
                    avcodec_free_context(&mut c);
                    let mut fb = frames_buf.as_ptr();
                    av_buffer_unref(&mut fb);
                    let mut p = hw_device_buf.as_ptr();
                    av_buffer_unref(&mut p);
                    return Err(MediaError::Other(format!(
                        "av_hwframe_get_buffer returned {ret}"
                    )));
                }
                f
            };
            // SAFETY: hw_ptr is non-null after successful av_hwframe_get_buffer.
            let hw_frame = unsafe { NonNull::new_unchecked(hw_ptr) };

            // 5. Allocate sws_scale context: BGRA8→P010LE, SWS_BILINEAR.
            // SAFETY: all integer args are positive; null filter/param selects defaults.
            let sws_ctx = unsafe {
                sws_getContext(
                    cfg.width as i32,
                    cfg.height as i32,
                    AV_PIX_FMT_BGRA,
                    cfg.width as i32,
                    cfg.height as i32,
                    AV_PIX_FMT_P010LE,
                    SWS_BILINEAR as i32,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
            if sws_ctx.is_null() {
                let mut hw = hw_frame.as_ptr();
                unsafe { av_frame_free(&mut hw) };
                let mut cp = cpu_frame.as_ptr();
                unsafe { av_frame_free(&mut cp) };
                let mut c = codec_ctx.as_ptr();
                unsafe { avcodec_free_context(&mut c) };
                let mut fb = frames_buf.as_ptr();
                unsafe { av_buffer_unref(&mut fb) };
                let mut p = hw_device_buf.as_ptr();
                unsafe { av_buffer_unref(&mut p) };
                return Err(MediaError::Other("sws_getContext returned null".into()));
            }

            // 6. Allocate staging texture and BGRA CPU buffer.
            let staging =
                D3d11Texture::new_staging(&device, cfg.width, cfg.height, TextureFormat::Bgra8)
                    .map_err(|e| {
                        unsafe { sws_freeContext(sws_ctx) };
                        let mut hw = hw_frame.as_ptr();
                        unsafe { av_frame_free(&mut hw) };
                        let mut cp = cpu_frame.as_ptr();
                        unsafe { av_frame_free(&mut cp) };
                        let mut c = codec_ctx.as_ptr();
                        unsafe { avcodec_free_context(&mut c) };
                        let mut fb = frames_buf.as_ptr();
                        unsafe { av_buffer_unref(&mut fb) };
                        let mut p = hw_device_buf.as_ptr();
                        unsafe { av_buffer_unref(&mut p) };
                        e
                    })?;

            let bgra_buf = vec![0u8; (cfg.width * cfg.height * 4) as usize];

            tracing::info!(
                target: "video.pipeline",
                event = "encoder_ready",
                backend = "ffmpeg-nvenc-hevc-main10-win",
                codec = "h265",
                profile = "main10",
                bitdepth = 10,
                gop = cfg.gop_size,
            );

            Ok(Self {
                device,
                staging,
                bgra_buf,
                hw_device_buf,
                frames_buf,
                codec_ctx,
                cpu_frame,
                hw_frame,
                sws_ctx,
                width: cfg.width,
                height: cfg.height,
                fps: cfg.fps,
                seq: 0,
                first_frame_logged: AtomicBool::new(false),
                last_bitrate_warn_secs: AtomicU64::new(0),
            })
        }
    }

    impl Hevc265Encoder for HevcNvencMain10FfmpegEncoderWindowsAdapter {
        fn encode(
            &mut self,
            texture: &D3d11Texture,
            force_idr: bool,
            timestamp_us: u64,
        ) -> Result<EncodedH265Frame, MediaError> {
            // 1. GPU→CPU readback: copy D3D11 BGRA texture into self.bgra_buf.
            texture
                .read_back_bgra_into(&self.device, &self.staging, &mut self.bgra_buf)
                .map_err(|e| MediaError::Other(format!("D3D11 readback: {e}")))?;

            let cpu = self.cpu_frame.as_ptr();
            let hw = self.hw_frame.as_ptr();
            let width = self.width;
            let height = self.height;

            // 2. BGRA8→P010LE via sws_scale (CPU; sole format-convert site in this file).
            // SAFETY: bgra_buf has width*height*4 bytes; cpu frame buffers are allocated.
            unsafe {
                let src_ptr = self.bgra_buf.as_ptr();
                let src_stride = (width as i32) * 4;
                let src_planes: [*const u8; 4] = [src_ptr, ptr::null(), ptr::null(), ptr::null()];
                let src_strides: [i32; 4] = [src_stride, 0, 0, 0];
                let dst_planes: [*mut u8; 4] = [
                    (*cpu).data[0],
                    (*cpu).data[1],
                    ptr::null_mut(),
                    ptr::null_mut(),
                ];
                let dst_strides: [i32; 4] = [(*cpu).linesize[0], (*cpu).linesize[1], 0, 0];
                sws_scale(
                    self.sws_ctx,
                    src_planes.as_ptr(),
                    src_strides.as_ptr(),
                    0,
                    height as i32,
                    dst_planes.as_ptr(),
                    dst_strides.as_ptr(),
                );
            }

            // 3. Upload: CPU P010LE → GPU CUDA surface. Sole CPU→GPU transfer in this file
            // (per-backend A9b invariant enforced by CI grep guard).
            // SAFETY: hw and cpu are valid non-null AVFrames; 0 flags required by API.
            let ret = unsafe { hw_upload(hw, cpu, 0) };
            if ret < 0 {
                return Err(MediaError::Other(format!(
                    "av_hwframe_transfer_data returned {ret}"
                )));
            }

            // 4. Set picture type for IDR forcing.
            // SAFETY: hw is a valid AVFrame owned by self.
            unsafe {
                if force_idr {
                    (*hw).pict_type = AV_PICTURE_TYPE_I;
                    (*hw).key_frame = 1;
                } else {
                    (*hw).pict_type = 0;
                    (*hw).key_frame = 0;
                }
            }

            // 5. PTS rescale: ts_us (microseconds) → 1/fps time_base units.
            // SAFETY: hw is a valid AVFrame.
            unsafe {
                (*hw).pts = (timestamp_us as i64 * self.fps as i64) / 1_000_000;
            }

            let ctx = self.codec_ctx.as_ptr();

            // 6. Send frame; drain one packet on EAGAIN then retry.
            // SAFETY: ctx is a valid open AVCodecContext; hw is a valid AVFrame.
            let mut send_ret = unsafe { avcodec_send_frame(ctx, hw) };
            if send_ret == AVERROR_EAGAIN {
                // SAFETY: av_packet_alloc returns zeroed packet or null.
                let drain_pkt = unsafe { av_packet_alloc() };
                if !drain_pkt.is_null() {
                    // SAFETY: ctx is open; drain_pkt is freshly allocated.
                    unsafe { avcodec_receive_packet(ctx, drain_pkt) };
                    // SAFETY: drain_pkt is the unique owner.
                    unsafe {
                        av_packet_unref(drain_pkt);
                        av_packet_free(&mut { drain_pkt });
                    }
                }
                // SAFETY: retry send after drain.
                send_ret = unsafe { avcodec_send_frame(ctx, hw) };
            }
            if send_ret < 0 {
                return Err(MediaError::Other(format!(
                    "avcodec_send_frame returned {send_ret}"
                )));
            }

            // 7. Receive encoded packet (Annex-B directly — no BSF).
            // SAFETY: av_packet_alloc returns zeroed packet or null.
            let pkt = unsafe { av_packet_alloc() };
            if pkt.is_null() {
                return Err(MediaError::Other("av_packet_alloc failed".into()));
            }
            // SAFETY: ctx is open; pkt is freshly allocated.
            let recv_ret = unsafe { avcodec_receive_packet(ctx, pkt) };
            if recv_ret < 0 {
                // SAFETY: pkt is still the unique owner.
                unsafe {
                    av_packet_unref(pkt);
                    av_packet_free(&mut { pkt });
                }
                return Err(MediaError::Other(format!(
                    "avcodec_receive_packet returned {recv_ret}"
                )));
            }

            // 8. Copy bytes and detect keyframe.
            let (nal_bytes, is_keyframe) = {
                // SAFETY: pkt.data/size are valid after successful avcodec_receive_packet.
                let (data_ptr, size, flags) =
                    unsafe { ((*pkt).data, (*pkt).size as usize, (*pkt).flags) };
                // SAFETY: data_ptr is valid for `size` bytes for the duration of pkt.
                let slice = unsafe { std::slice::from_raw_parts(data_ptr, size) };
                let bytes = slice.to_vec();
                let key = (flags & AV_PKT_FLAG_KEY as i32) != 0;
                // SAFETY: pkt is the unique owner; unref before free.
                unsafe {
                    av_packet_unref(pkt);
                    av_packet_free(&mut { pkt });
                }
                (bytes, key)
            };

            // 9. First-frame log.
            if !self.first_frame_logged.swap(true, Ordering::SeqCst) {
                tracing::info!(
                    target: "video.pipeline",
                    event = "first_frame_emitted",
                    seq = self.seq,
                    codec = "hevc_nvenc",
                    hw_path = "cuda",
                    convert_path = "sws_scale_p010",
                    width = self.width,
                    height = self.height,
                    "first encoded frame delivered"
                );
            }

            self.seq += 1;
            Ok(EncodedH265Frame {
                nal_bytes,
                is_keyframe,
                timestamp: timestamp_us,
            })
        }

        fn set_target_bitrate(&mut self, bps: u32) {
            let ctx = self.codec_ctx.as_ptr();
            // SAFETY: ctx is a valid open AVCodecContext; "b" is the standard bitrate option.
            let ret = unsafe {
                av_opt_set_int(
                    ctx.cast(),
                    c"b".as_ptr(),
                    bps as i64,
                    AV_OPT_SEARCH_CHILDREN as i32,
                )
            };
            if ret < 0 {
                use std::time::{SystemTime, UNIX_EPOCH};
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let last = self.last_bitrate_warn_secs.load(Ordering::Relaxed);
                if now_secs.saturating_sub(last) >= 60 {
                    self.last_bitrate_warn_secs
                        .store(now_secs, Ordering::Relaxed);
                    tracing::warn!(
                        target: "video.pipeline",
                        ret,
                        bps,
                        "set_target_bitrate av_opt_set_int failed (rate-limited warn)"
                    );
                }
            }
        }

        fn backend_name(&self) -> &'static str {
            "ffmpeg-nvenc-hevc-main10-win"
        }
    }

    impl Drop for HevcNvencMain10FfmpegEncoderWindowsAdapter {
        fn drop(&mut self) {
            // Reverse-creation order: sws → hw_frame → cpu_frame → codec_ctx → frames → hw_device.
            // SAFETY: sws_ctx is the unique owner of the SwsContext.
            unsafe { sws_freeContext(self.sws_ctx) };

            let mut hw = self.hw_frame.as_ptr();
            // SAFETY: hw_frame is the unique owner of the CUDA surface ref.
            unsafe { av_frame_free(&mut hw) };

            let mut cp = self.cpu_frame.as_ptr();
            // SAFETY: cpu_frame is the unique owner of the P010LE CPU frame.
            unsafe { av_frame_free(&mut cp) };

            let mut c = self.codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner (hw_frames_ctx consumed by avcodec_open2).
            unsafe { avcodec_free_context(&mut c) };

            let mut fb = self.frames_buf.as_ptr();
            // SAFETY: frames_buf is the unique owner of the P010LE HW frames pool.
            unsafe { av_buffer_unref(&mut fb) };

            let mut p = self.hw_device_buf.as_ptr();
            // SAFETY: hw_device_buf is the unique owner of the CUDA HW device.
            unsafe { av_buffer_unref(&mut p) };

            // device, staging, bgra_buf, and atomics drop via their own impls.
        }
    }

    // SAFETY: all raw FFmpeg pointers are owned exclusively by this struct and
    // accessed only from the thread that calls encode() / drop(). The struct is
    // never aliased across threads; spawn_blocking moves sole ownership in and out.
    unsafe impl Send for HevcNvencMain10FfmpegEncoderWindowsAdapter {}

    /// Open and configure an NVENC codec context for HEVC Main10 (preset=p1, tune=ull,
    /// rc=cbr, zerolatency=1, bf=0, forced-idr=1; HDR10 BT.2020 PQ color metadata).
    ///
    /// OWNERSHIP: `hw_frames_ctx` ref is consumed on both success and failure.
    fn open_nvenc_main10_codec_ctx(
        codec: *const rusty_ffmpeg_win::ffi::AVCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u32,
        gop_size: u32,
        hw_frames_ctx: NonNull<AVBufferRef>,
    ) -> Result<NonNull<AVCodecContext>, MediaError> {
        // SAFETY: codec is a valid non-null AVCodec pointer.
        let ctx_ptr = unsafe { avcodec_alloc_context3(codec) };
        if ctx_ptr.is_null() {
            let mut p = hw_frames_ctx.as_ptr();
            // SAFETY: hw_frames_ctx is the unique owner.
            unsafe { av_buffer_unref(&mut p) };
            return Err(MediaError::Other(
                "avcodec_alloc_context3 returned null".into(),
            ));
        }

        // SAFETY: ctx_ptr is a freshly allocated, unopened AVCodecContext.
        unsafe {
            (*ctx_ptr).bit_rate = bitrate_bps as i64;
            (*ctx_ptr).rc_max_rate = bitrate_bps as i64;
            (*ctx_ptr).rc_buffer_size = (bitrate_bps / fps.max(1)) as i32;
            (*ctx_ptr).gop_size = gop_size as i32;
            (*ctx_ptr).max_b_frames = 0;
            (*ctx_ptr).time_base = AVRational {
                num: 1,
                den: fps as i32,
            };
            (*ctx_ptr).framerate = AVRational {
                num: fps as i32,
                den: 1,
            };
            (*ctx_ptr).profile = AV_PROFILE_HEVC_MAIN_10;
            (*ctx_ptr).flags &= !(AV_CODEC_FLAG_GLOBAL_HEADER as i32);
            (*ctx_ptr).pix_fmt = AV_PIX_FMT_CUDA;
            (*ctx_ptr).width = width as i32;
            (*ctx_ptr).height = height as i32;
            // HDR10 BT.2020 PQ color metadata. Constants are free u32 items in
            // rusty_ffmpeg_win::ffi — not enum variants.
            (*ctx_ptr).color_primaries = AVCOL_PRI_BT2020;
            (*ctx_ptr).color_trc = AVCOL_TRC_SMPTE2084;
            (*ctx_ptr).colorspace = AVCOL_SPC_BT2020_NCL;
            (*ctx_ptr).color_range = AVCOL_RANGE_MPEG;
        }

        // Attach hw_frames_ctx — avcodec_open2 will take ownership of the ref.
        // SAFETY: ctx_ptr is valid and not yet opened.
        unsafe { (*ctx_ptr).hw_frames_ctx = hw_frames_ctx.as_ptr() };

        // Build private-data dict (same keys as 8-bit NVENC) and open codec.
        let dict = match build_nvenc_main10_dict(gop_size) {
            Ok(d) => d,
            Err(e) => {
                let mut p = ctx_ptr;
                // SAFETY: ctx_ptr owns hw_frames_ctx ref via the field assignment above.
                unsafe { avcodec_free_context(&mut p) };
                return Err(e);
            }
        };
        // SAFETY: ctx_ptr, codec, and dict are all valid; avcodec_open2 frees dict.
        let open_ret = unsafe { avcodec_open2(ctx_ptr, codec, &mut dict.as_ptr()) };
        if open_ret < 0 {
            let mut p = ctx_ptr;
            // SAFETY: ctx_ptr is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(MediaError::Other(format!(
                "avcodec_open2 returned {open_ret}"
            )));
        }

        // SAFETY: avcodec_alloc_context3 succeeded; pointer is non-null.
        Ok(unsafe { NonNull::new_unchecked(ctx_ptr) })
    }

    fn build_nvenc_main10_dict(gop_size: u32) -> Result<NonNull<AVDictionary>, MediaError> {
        let mut dict: *mut AVDictionary = ptr::null_mut();
        dict_set(&mut dict, "preset", "p1")?;
        dict_set(&mut dict, "tune", "ull")?;
        dict_set(&mut dict, "rc", "cbr")?;
        dict_set(&mut dict, "zerolatency", "1")?;
        dict_set(&mut dict, "rc-lookahead", "0")?;
        dict_set(&mut dict, "bf", "0")?;
        dict_set(&mut dict, "g", &gop_size.to_string())?;
        dict_set(&mut dict, "forced-idr", "1")?;
        dict_set(&mut dict, "delay", "0")?;
        NonNull::new(dict).ok_or_else(|| MediaError::Other("av_dict_set produced null dict".into()))
    }

    fn dict_set(dict: &mut *mut AVDictionary, key: &str, value: &str) -> Result<(), MediaError> {
        let k = CString::new(key).expect("key has no interior nul");
        let v = CString::new(value).expect("value has no interior nul");
        // SAFETY: dict is a valid *mut *mut AVDictionary; k/v lifetimes cover the call.
        let ret = unsafe { av_dict_set(dict, k.as_ptr(), v.as_ptr(), 0) };
        if ret < 0 {
            Err(MediaError::Other(format!(
                "av_dict_set({key}={value}) returned {ret}"
            )))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn context_creation_succeeds_main10() {
            // On CI (windows-latest, no NVIDIA GPU), avcodec_find_encoder_by_name
            // returns null → expect EncoderNotAvailable, never panic.
            let codec = unsafe { avcodec_find_encoder_by_name(c"hevc_nvenc".as_ptr()) };
            if codec.is_null() {
                let dev = crate::d3d11::D3d11Device::create_default()
                    .expect("D3D11 device (software fallback should always work)");
                let result = HevcNvencMain10FfmpegEncoderWindowsAdapter::new(
                    dev,
                    HevcNvencMain10FfmpegEncoderWindowsAdapterConfig::default(),
                );
                assert!(
                    matches!(result, Err(MediaError::EncoderNotAvailable { .. })),
                    "expected EncoderNotAvailable when hevc_nvenc not found, got: {result:?}"
                );
            }
        }

        #[test]
        fn default_config_fields_main10() {
            let cfg = HevcNvencMain10FfmpegEncoderWindowsAdapterConfig::default();
            assert_eq!(cfg.width, 1920);
            assert_eq!(cfg.height, 1080);
            assert_eq!(cfg.fps, 60);
            assert_eq!(cfg.initial_bitrate_bps, 8_000_000);
            assert_eq!(cfg.gop_size, 60);
        }
    }
}

#[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
pub use inner::{
    HevcNvencMain10FfmpegEncoderWindowsAdapter, HevcNvencMain10FfmpegEncoderWindowsAdapterConfig,
};
