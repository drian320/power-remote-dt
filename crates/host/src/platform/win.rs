//! Windows host backend. Receives encoder dispatch + DXGI SW producer
//! contents (Task 3) + cross-platform factory wrappers (Task 4).
//! All items are `cfg(windows)`-gated at the file level.

#![cfg(windows)]

// === Moved from encoder_dispatch.rs ===

use std::time::Duration;

use anyhow::Context as _;
use prdt_media_sw::{bgra_to_i420, Openh264Encoder, Openh264EncoderConfig, SwH264Encoder};
use prdt_media_win::{
    AcquiredFrame, D3d11Device, D3d11Texture, DesktopDuplication, HwHevcEncoder, MediaError,
    OutputInfo, TextureFormat,
};
use prdt_protocol::{now_monotonic_us, EncodedFrame, ProducerError, VideoProducer};

/// Runtime-dispatched video encoder used to construct the right producer
/// in `run_host`. Phase 4 will use the `is_h264()` discriminator to fork
/// producer construction; Phase 2 just wires up the type.
pub enum VideoEncoderBackend {
    Hw(HwHevcEncoder),
    SwH264(Box<Openh264Encoder>),
}

impl VideoEncoderBackend {
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Hw(e) => e.backend_name(),
            Self::SwH264(_) => "openh264",
        }
    }

    #[allow(dead_code)]
    pub fn is_h264(&self) -> bool {
        matches!(self, Self::SwH264(_))
    }

    /// Best-effort target-bitrate update. For OpenH264 the new value is
    /// stashed in cfg and takes effect on encoder reinit (see media-sw
    /// `Openh264Encoder::set_target_bitrate` doc).
    #[allow(dead_code)]
    pub fn set_target_bitrate(&mut self, bps: u32) {
        match self {
            Self::Hw(e) => e.set_target_bitrate(bps),
            Self::SwH264(e) => {
                e.set_target_bitrate(bps);
            }
        }
    }
}

#[cfg(test)]
mod encoder_dispatch_tests {
    use super::*;

    #[test]
    fn sw_backend_name_is_openh264() {
        let cfg = Openh264EncoderConfig {
            width: 320,
            height: 240,
            target_bitrate_bps: 1_000_000,
            max_fps: 30.0,
        };
        let enc = Openh264Encoder::new(cfg).expect("encoder");
        let backend = VideoEncoderBackend::SwH264(Box::new(enc));
        assert_eq!(backend.backend_name(), "openh264");
        assert!(backend.is_h264());
    }
}

// === Moved from dxgi_sw_producer.rs ===

pub struct DxgiSwProducer {
    dev: D3d11Device,
    output: OutputInfo,
    dup: DesktopDuplication,
    /// Owned by the producer for the loop's lifetime. `take()` + restore
    /// pattern lets us move the encoder into `spawn_blocking` and back
    /// without an `Arc<Mutex<>>` around the hot path.
    encoder: Option<Openh264Encoder>,
    staging: D3d11Texture,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
}

impl DxgiSwProducer {
    /// Create a producer for the given monitor with a pre-built encoder.
    /// Mirrors `DxgiNvencProducer::with_encoder` so the host main fn can
    /// fork on `VideoEncoderBackend` without producer-vendor branching.
    pub fn with_encoder(
        dev: &D3d11Device,
        output: &OutputInfo,
        encoder: Openh264Encoder,
    ) -> anyhow::Result<Self> {
        let dup = DesktopDuplication::new(dev, output).context("DesktopDuplication::new")?;
        let width = dup.width();
        let height = dup.height();
        let staging = D3d11Texture::new_staging(dev, width, height, TextureFormat::Bgra8)
            .context("staging texture")?;
        Ok(Self {
            dev: dev.clone(),
            output: output.clone(),
            dup,
            encoder: Some(encoder),
            staging,
            seq: 0,
            idr_pending: true,
            width,
            height,
        })
    }

    #[allow(dead_code)]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Copy `tex` → cached staging tex, then map and tight-pack into the
    /// caller-provided `Vec<u8>` (length `width * height * 4`).
    fn readback_bgra(&self, tex: &D3d11Texture, out: &mut [u8]) -> Result<(), MediaError> {
        tex.read_back_bgra_into(&self.dev, &self.staging, out)
    }
}

// Same Send rationale as DxgiNvencProducer: DesktopDuplication holds a !Send
// IDXGIOutputDuplication, but we serialise access via &mut self in
// `next_frame`, never touching it concurrently. Openh264Encoder is Send.
unsafe impl Send for DxgiSwProducer {}

fn is_access_lost(e: &MediaError) -> bool {
    match e {
        MediaError::Dxgi { hresult, .. } => {
            const DXGI_ERROR_ACCESS_LOST: u32 = 0x887A_0026;
            const DXGI_ERROR_ACCESS_DENIED: u32 = 0x887A_0027;
            const DXGI_ERROR_INVALID_CALL: u32 = 0x887A_0001;
            *hresult == DXGI_ERROR_ACCESS_LOST
                || *hresult == DXGI_ERROR_ACCESS_DENIED
                || *hresult == DXGI_ERROR_INVALID_CALL
        }
        _ => false,
    }
}

#[async_trait::async_trait]
impl VideoProducer for DxgiSwProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        loop {
            let acquired = match self.dup.acquire_next_frame(Duration::from_millis(16)) {
                Ok(a) => a,
                Err(e) if e.is_device_removed() => {
                    tracing::error!(
                        error = %e,
                        "D3D11 device removed in sw producer — fatal; \
                         restart the host process to recover",
                    );
                    return Err(ProducerError::Capture(format!("device removed: {e}")));
                }
                Err(e) => {
                    if is_access_lost(&e) {
                        tracing::warn!(
                            error = %e,
                            "DXGI access lost (sw producer); re-acquiring duplication"
                        );
                        match DesktopDuplication::new(&self.dev, &self.output) {
                            Ok(new_dup) => {
                                self.dup = new_dup;
                                self.idr_pending = true;
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                continue;
                            }
                            Err(re_err) => {
                                tracing::warn!(
                                    error = %re_err,
                                    "re-acquiring DXGI duplication failed (sw producer); backing off"
                                );
                                tokio::time::sleep(Duration::from_millis(250)).await;
                                continue;
                            }
                        }
                    } else {
                        return Err(ProducerError::Capture(e.to_string()));
                    }
                }
            };
            let texture = match acquired {
                AcquiredFrame::Frame { texture, .. } => texture,
                AcquiredFrame::Timeout => continue,
            };

            // CPU readback (cached staging tex) — synchronous, sub-ms at 1080p.
            let row_bytes = (self.width as usize) * 4;
            let mut bgra = vec![0u8; row_bytes * (self.height as usize)];
            self.readback_bgra(&texture, &mut bgra)
                .map_err(|e| ProducerError::Capture(format!("readback: {e}")))?;

            let width = self.width;
            let height = self.height;
            let bgra_stride = width * 4;
            let i420 = bgra_to_i420(&bgra, width, height, bgra_stride)
                .map_err(|e| ProducerError::Other(format!("bgra_to_i420: {e}")))?;

            let ts_us = now_monotonic_us();
            let force_idr = std::mem::take(&mut self.idr_pending);

            // Move encoder into the blocking pool, run encode, move it back.
            // This keeps the single-threaded OpenH264 call off the tokio
            // reactor (pre-mortem #2 mitigation).
            let mut enc = self
                .encoder
                .take()
                .expect("encoder was taken twice; producer state corrupted");
            let join = tokio::task::spawn_blocking(move || {
                let result = enc.encode(&i420, force_idr, ts_us);
                (enc, result)
            })
            .await
            .map_err(|e| ProducerError::Other(format!("spawn_blocking join: {e}")))?;
            let (enc_back, encode_result) = join;
            self.encoder = Some(enc_back);

            let frame = encode_result.map_err(|e| ProducerError::Encode(e.to_string()))?;

            // Openh264Encoder already returns a fully-formed EncodedFrame
            // with codec=H264. Override seq with the producer-tracked
            // counter so the wire seq matches our producer ordering
            // (encoder's internal seq is independent and resets on
            // reinit).
            let seq = self.seq;
            self.seq += 1;
            return Ok(EncodedFrame { seq, ..frame });
        }
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(enc) = self.encoder.as_mut() {
            enc.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "openh264-sw"
    }
}

// === Migrated from lib.rs ===

use prdt_media_sw::{Openh264Encoder, Openh264EncoderConfig};
#[cfg(prdt_nvenc_bindings)]
use prdt_media_win::NvencEncoder;
use prdt_media_win::{HwHevcEncoder, MfH265Encoder, NvencEncoderConfig};
use prdt_protocol::Codec;

/// Resolve `--encoder` to a concrete backend. The `auto` selector picks
/// nvenc > mf > openh264. When the resolved backend is HW, the negotiated
/// codec must be H.265; when SW, must be H.264. The handshake layer rejects
/// mismatches before we get here, so a mismatch in this fn is a programmer
/// error and we bail.
pub(super) fn pick_encoder(
    args_encoder: &str,
    adapter: &prdt_media_win::AdapterInfo,
    dev: &D3d11Device,
    width: u32,
    height: u32,
    bitrate_bps: u32,
    negotiated_codec: Codec,
) -> anyhow::Result<VideoEncoderBackend> {
    let choice = resolve_encoder_choice(args_encoder, adapter, negotiated_codec);
    match choice {
        "nvenc" => {
            if negotiated_codec != Codec::H265 {
                anyhow::bail!(
                    "encoder=nvenc but negotiated codec={:?}; handshake layer should have rejected this",
                    negotiated_codec
                );
            }
            #[cfg(prdt_nvenc_bindings)]
            {
                let cfg = NvencEncoderConfig {
                    width,
                    height,
                    fps_numerator: 60,
                    fps_denominator: 1,
                    bitrate_bps,
                    gop_length: 60,
                };
                let enc = NvencEncoder::new(dev, &cfg).context("NvencEncoder::new")?;
                return Ok(VideoEncoderBackend::Hw(HwHevcEncoder::from(enc)));
            }
            #[cfg(not(prdt_nvenc_bindings))]
            {
                let _ = (dev, width, height, bitrate_bps);
                anyhow::bail!("nvenc backend not built (NV_CODEC_SDK_PATH unset at build time)")
            }
        }
        "mf" => {
            if negotiated_codec != Codec::H265 {
                anyhow::bail!(
                    "encoder=mf but negotiated codec={:?}; handshake layer should have rejected this",
                    negotiated_codec
                );
            }
            let cfg = NvencEncoderConfig {
                width,
                height,
                fps_numerator: 60,
                fps_denominator: 1,
                bitrate_bps,
                gop_length: 60,
            };
            let enc = MfH265Encoder::new(dev, &cfg).context("MfH265Encoder::new")?;
            Ok(VideoEncoderBackend::Hw(HwHevcEncoder::from(enc)))
        }
        "openh264" => {
            if negotiated_codec != Codec::H264 {
                anyhow::bail!(
                    "encoder=openh264 but negotiated codec={:?}; handshake layer should have rejected this",
                    negotiated_codec
                );
            }
            let cfg = Openh264EncoderConfig {
                width,
                height,
                target_bitrate_bps: bitrate_bps,
                max_fps: 60.0,
            };
            let enc = Openh264Encoder::new(cfg).context("Openh264Encoder::new")?;
            Ok(VideoEncoderBackend::SwH264(Box::new(enc)))
        }
        other => anyhow::bail!("unknown --encoder {other:?} (valid: auto, nvenc, mf, openh264)"),
    }
}

/// Apply the `auto` selection policy: nvenc > mf > openh264. Plan §Phase 2:
/// "auto selection order: nvenc > mf > openh264". The viewer-requested
/// codec narrows the choice — if the viewer wants H.264 we pick openh264
/// regardless of GPU vendor (HW H.264 encode is not implemented in this
/// repo and is out of scope for the software-codec tag).
fn resolve_encoder_choice<'a>(
    args_encoder: &'a str,
    adapter: &prdt_media_win::AdapterInfo,
    negotiated_codec: Codec,
) -> &'a str {
    if args_encoder == "auto" {
        match negotiated_codec {
            Codec::H264 => "openh264",
            Codec::H265 => {
                if adapter.is_nvidia() {
                    #[cfg(prdt_nvenc_bindings)]
                    return "nvenc";
                    #[cfg(not(prdt_nvenc_bindings))]
                    return "mf";
                } else {
                    "mf"
                }
            }
            // Any future codec falls through to nvenc/mf which will then
            // bail with a clear "encoder=X but negotiated codec=Y" error.
            _ => {
                if adapter.is_nvidia() {
                    #[cfg(prdt_nvenc_bindings)]
                    return "nvenc";
                    #[cfg(not(prdt_nvenc_bindings))]
                    return "mf";
                } else {
                    "mf"
                }
            }
        }
    } else {
        args_encoder
    }
}

/// What we advertise in HelloAck `host_supported_codecs` based on the
/// `--encoder` flag. An explicit choice locks us to a single codec; `auto`
/// advertises the full HW set so the viewer's preference wins.
pub(super) fn supported_codecs_for_encoder_arg(
    args_encoder: &str,
    adapter: &prdt_media_win::AdapterInfo,
) -> Vec<Codec> {
    match args_encoder {
        "openh264" => vec![Codec::H264],
        "nvenc" | "mf" => vec![Codec::H265],
        // "auto" or anything else — caller resolves to a HW backend
        // (nvenc/mf), both of which emit H.265. media-sw is built into
        // this binary, so SW H.264 is also reachable; advertise both
        // so a viewer that explicitly asks for H.264 (with `--codec
        // h264`) can still negotiate without forcing the operator to
        // pass `--encoder openh264` on the host side.
        _ => {
            let _ = adapter;
            vec![Codec::H265, Codec::H264]
        }
    }
}

// === Factory surface (cfg-transparent re-exports via platform/mod.rs) ===

use anyhow::Context as _;
use prdt_input_win::{
    clipboard_sequence_number as _input_win_clipboard_sequence_number,
    read_clipboard_text as _input_win_read_clipboard_text,
    virtual_desktop_rect as _input_win_virtual_desktop_rect,
    write_clipboard_text as _input_win_write_clipboard_text, SendInputInjector,
    MAX_CLIPBOARD_BYTES as _INPUT_WIN_MAX,
};
use prdt_media_win::{
    dxgi::enumerate_outputs_for_adapter, pick_default_adapter, DxgiNvencProducer, OutputInfo,
};
use prdt_protocol::{InputEvent, MonitorRect, VideoProducer};

/// Re-exported max clipboard bytes; identical value across OSes.
pub const MAX_CLIPBOARD_BYTES: usize = _INPUT_WIN_MAX;

/// Per-OS opaque output descriptor. Windows holds the real `OutputInfo`;
/// Linux holds a unit struct `OutputDescriptor`.
pub type OutputDescriptor = OutputInfo;

/// Human-readable name for the output; used in the "host starting" log.
pub fn output_display_name(d: &OutputDescriptor) -> &str {
    &d.device_name
}

/// Pick the default output (monitor) for capture. Internally enumerates
/// adapters + outputs and returns the first.
pub fn pick_default_output(_args: &crate::Args) -> anyhow::Result<OutputDescriptor> {
    let adapter = pick_default_adapter().context("pick_default_adapter")?;
    let outputs =
        enumerate_outputs_for_adapter(&adapter).context("enumerate_outputs_for_adapter")?;
    let primary = outputs
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no DXGI output found on adapter"))?;
    Ok(primary)
}

/// Build a boxed `VideoProducer` for the requested encoder backend.
/// `args_encoder` is the raw CLI string ("auto" | "nvenc" | "mf" | "openh264");
/// `negotiated_codec` is the codec the viewer agreed to in HelloAck.
pub fn build_video_producer(
    args_encoder: &str,
    output: &OutputDescriptor,
    bitrate_bps: u32,
    _fps: u32,
    negotiated_codec: prdt_protocol::Codec,
) -> anyhow::Result<Box<dyn VideoProducer>> {
    let adapter = pick_default_adapter().context("pick_default_adapter")?;
    let dev = D3d11Device::create(&adapter).context("D3D11 device")?;
    let width = (output.desktop_rect.right - output.desktop_rect.left) as u32;
    let height = (output.desktop_rect.bottom - output.desktop_rect.top) as u32;
    let backend = pick_encoder(
        args_encoder,
        &adapter,
        &dev,
        width,
        height,
        bitrate_bps,
        negotiated_codec,
    )
    .context("pick_encoder")?;
    let producer: Box<dyn VideoProducer> = match backend {
        VideoEncoderBackend::Hw(enc) => {
            Box::new(DxgiNvencProducer::with_encoder(&dev, output, enc).context("hw producer")?)
        }
        VideoEncoderBackend::SwH264(enc) => {
            Box::new(DxgiSwProducer::with_encoder(&dev, output, *enc).context("sw producer")?)
        }
    };
    Ok(producer)
}

/// Inject one input event into the kernel via SendInput.
pub fn dispatch_input(event: InputEvent) -> Result<(), super::DispatchError> {
    SendInputInjector::send(event).map_err(|e| super::DispatchError::Backend(e.to_string()))
}

/// Read the user's primary clipboard text channel.
pub fn read_clipboard_text() -> Result<String, super::ClipboardError> {
    _input_win_read_clipboard_text().map_err(|e| match e {
        prdt_input_win::ClipboardError::TooLarge(n) => super::ClipboardError::TooLarge(n),
        prdt_input_win::ClipboardError::NoText => super::ClipboardError::NoText,
        other => super::ClipboardError::Backend(other.to_string()),
    })
}

/// Set the user's primary clipboard text channel.
pub fn write_clipboard_text(text: &str) -> Result<(), super::ClipboardError> {
    _input_win_write_clipboard_text(text).map_err(|e| match e {
        prdt_input_win::ClipboardError::TooLarge(n) => super::ClipboardError::TooLarge(n),
        prdt_input_win::ClipboardError::NoText => super::ClipboardError::NoText,
        other => super::ClipboardError::Backend(other.to_string()),
    })
}

/// Cheap monotonic counter that bumps on any system clipboard change.
pub fn clipboard_sequence_number() -> u32 {
    _input_win_clipboard_sequence_number()
}

/// Return the host's combined virtual desktop rectangle in screen-space coords.
pub fn virtual_desktop_rect() -> MonitorRect {
    _input_win_virtual_desktop_rect()
}
