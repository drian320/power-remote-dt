//! CPU-resident BGRA frame produced by the X11 capture path. Width and
//! height are in pixels; `stride` is bytes-per-row (= width * 4 in
//! L1; we don't pad). `bgra` is `width * height * 4` bytes long when
//! stride == width * 4.

#[derive(Debug, Clone)]
pub struct BgraFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bgra: Vec<u8>,
    /// Monotonic capture timestamp (microseconds). Used by latency
    /// instrumentation downstream; the producer fills this with
    /// `prdt_protocol::now_monotonic_us()` at the moment the X server
    /// returns the pixel data.
    pub capture_ts_us: u64,
}

impl BgraFrame {
    pub fn new_zeroed(width: u32, height: u32) -> Self {
        let stride = width * 4;
        Self {
            width,
            height,
            stride,
            bgra: vec![0u8; (stride as usize) * (height as usize)],
            capture_ts_us: 0,
        }
    }

    pub fn expected_len(&self) -> usize {
        (self.stride as usize) * (self.height as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_zeroed_has_expected_buffer_size() {
        let f = BgraFrame::new_zeroed(1920, 1080);
        assert_eq!(f.stride, 1920 * 4);
        assert_eq!(f.bgra.len(), f.expected_len());
        assert_eq!(f.bgra.len(), 1920 * 1080 * 4);
    }

    #[test]
    fn new_zeroed_buffer_is_all_zero() {
        let f = BgraFrame::new_zeroed(64, 64);
        assert!(f.bgra.iter().all(|b| *b == 0));
    }
}
