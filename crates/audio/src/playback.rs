//! Audio playback on the viewer's default output device.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::error::AudioError;
use crate::opus_codec::{OPUS_CHANNELS, OPUS_SAMPLE_RATE};

/// Play incoming PCM frames. Drop to stop.
pub struct AudioPlayback {
    _stream: cpal::Stream,
    buffer: Arc<Mutex<std::collections::VecDeque<f32>>>,
}

impl AudioPlayback {
    pub fn start() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;
        let config = device
            .default_output_config()
            .map_err(|e| AudioError::CpalBuild(format!("default_output_config: {e}")))?;
        let dev_rate = config.sample_rate().0;
        let dev_channels = config.channels();
        if dev_rate != OPUS_SAMPLE_RATE || dev_channels != OPUS_CHANNELS {
            return Err(AudioError::UnsupportedFormat(format!(
                "need {}Hz {}ch, device is {}Hz {}ch",
                OPUS_SAMPLE_RATE, OPUS_CHANNELS, dev_rate, dev_channels
            )));
        }
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();

        let buffer = Arc::new(Mutex::new(std::collections::VecDeque::<f32>::new()));
        let buf_cb = Arc::clone(&buffer);

        let err_fn = |e| tracing::warn!(?e, "cpal output stream error");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device
                .build_output_stream(
                    &stream_config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let mut q = buf_cb.lock().unwrap();
                        for s in data.iter_mut() {
                            *s = q.pop_front().unwrap_or(0.0);
                        }
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::CpalBuild(format!("build_output_stream: {e}")))?,
            other => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "only F32 output supported, device is {:?}",
                    other
                )));
            }
        };
        stream
            .play()
            .map_err(|e| AudioError::CpalBuild(format!("stream.play: {e}")))?;

        Ok(Self {
            _stream: stream,
            buffer,
        })
    }

    /// Enqueue PCM samples (stereo interleaved f32). They'll be consumed by
    /// the output callback at the device's rate.
    pub fn enqueue(&self, pcm: &[f32]) {
        let mut q = self.buffer.lock().unwrap();
        // Cap backlog at ~1 second of audio to avoid unbounded growth on slow consumers.
        const MAX: usize = OPUS_SAMPLE_RATE as usize * OPUS_CHANNELS as usize;
        if q.len() > MAX {
            let excess = q.len() - MAX;
            q.drain(..excess);
        }
        q.extend(pcm);
    }
}
