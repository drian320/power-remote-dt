//! System audio capture. On Windows uses WASAPI loopback (captures what
//! goes to the speaker). On other platforms this is a stub that errors.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::error::AudioError;
use crate::opus_codec::{OPUS_CHANNELS, OPUS_FRAME_SAMPLES, OPUS_SAMPLE_RATE};

/// Capture the host's system audio via WASAPI loopback. Emits 20 ms PCM
/// frames (stereo f32 interleaved) via the returned mpsc receiver.
/// Drop the returned `LoopbackCapture` to stop.
pub struct LoopbackCapture {
    _stream: cpal::Stream,
}

impl LoopbackCapture {
    pub fn start() -> Result<(Self, UnboundedReceiver<Vec<f32>>), AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;
        let config = device
            .default_output_config()
            .map_err(|e| AudioError::CpalBuild(format!("default_output_config: {e}")))?;

        let src_rate = config.sample_rate().0;
        let src_channels = config.channels();
        let sample_format = config.sample_format();

        // For simplicity: if src_rate != 48000 or src_channels != 2, error out.
        // A full implementation would resample and downmix.
        if src_rate != OPUS_SAMPLE_RATE || src_channels != OPUS_CHANNELS {
            return Err(AudioError::UnsupportedFormat(format!(
                "need {}Hz {}ch, device is {}Hz {}ch",
                OPUS_SAMPLE_RATE, OPUS_CHANNELS, src_rate, src_channels
            )));
        }

        let stream_config: cpal::StreamConfig = config.into();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let frame_stereo = OPUS_FRAME_SAMPLES * OPUS_CHANNELS as usize;

        let mut buf: Vec<f32> = Vec::with_capacity(frame_stereo * 2);
        let tx_cb: UnboundedSender<Vec<f32>> = tx;

        let err_fn = |e| tracing::warn!(?e, "cpal input stream error");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device
                .build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        buf.extend_from_slice(data);
                        while buf.len() >= frame_stereo {
                            let frame: Vec<f32> = buf.drain(..frame_stereo).collect();
                            let _ = tx_cb.send(frame);
                        }
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::CpalBuild(format!("build_input_stream: {e}")))?,
            other => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "only F32 supported, device is {:?}",
                    other
                )));
            }
        };
        stream
            .play()
            .map_err(|e| AudioError::CpalBuild(format!("stream.play: {e}")))?;

        Ok((Self { _stream: stream }, rx))
    }
}
