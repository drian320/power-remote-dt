//! Opus encoder + decoder wrapping audiopus.

use std::convert::TryFrom;

use audiopus::{
    coder::{Decoder as OpusDec, Encoder as OpusEnc},
    packet::Packet,
    Application, Channels, MutSignals, SampleRate,
};

use crate::error::AudioError;

pub const OPUS_SAMPLE_RATE: u32 = 48000;
pub const OPUS_CHANNELS: u16 = 2;
pub const OPUS_FRAME_MS: u32 = 20;
/// Samples per channel in a 20 ms frame.
pub const OPUS_FRAME_SAMPLES: usize = (OPUS_SAMPLE_RATE as usize * OPUS_FRAME_MS as usize) / 1000; // 960
pub const OPUS_BITRATE: i32 = 64_000;

pub struct OpusEncoder {
    enc: OpusEnc,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, AudioError> {
        let mut enc = OpusEnc::new(SampleRate::Hz48000, Channels::Stereo, Application::LowDelay)?;
        enc.set_bitrate(audiopus::Bitrate::BitsPerSecond(OPUS_BITRATE))?;
        Ok(Self { enc })
    }

    /// Encode a 20 ms stereo f32 frame (interleaved L,R,L,R...).
    /// Returns Opus bytes.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, AudioError> {
        let expected = OPUS_FRAME_SAMPLES * OPUS_CHANNELS as usize;
        if pcm.len() != expected {
            return Err(AudioError::Other(format!(
                "encode: expected {expected} samples, got {}",
                pcm.len()
            )));
        }
        let mut out = vec![0u8; 4000];
        let n = self.enc.encode_float(pcm, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}

pub struct OpusDecoder {
    dec: OpusDec,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, AudioError> {
        let dec = OpusDec::new(SampleRate::Hz48000, Channels::Stereo)?;
        Ok(Self { dec })
    }

    /// Decode Opus bytes into a 20 ms stereo f32 frame.
    pub fn decode(&mut self, opus_bytes: &[u8]) -> Result<Vec<f32>, AudioError> {
        let mut out = vec![0.0f32; OPUS_FRAME_SAMPLES * OPUS_CHANNELS as usize];
        let pkt = Packet::try_from(opus_bytes)?;
        let sig = MutSignals::try_from(&mut out[..])?;
        let n = self.dec.decode_float(Some(pkt), sig, false)?;
        out.truncate(n * OPUS_CHANNELS as usize);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        // 20 ms of silence.
        let pcm = vec![0.0f32; OPUS_FRAME_SAMPLES * OPUS_CHANNELS as usize];
        let encoded = enc.encode(&pcm).unwrap();
        assert!(!encoded.is_empty());
        let decoded = dec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), pcm.len());
    }
}
