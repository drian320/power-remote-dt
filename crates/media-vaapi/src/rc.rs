//! Rate-control parameter buffer builders.
//!
//! In P5C-1 we use CBR only. Per spec §2, CBR↔VBR switching needs
//! re-`create_config` on some drivers; the encoder treats RC mode as
//! init-time-only and exposes only `set_target_bitrate(bps)` for
//! dynamic updates within CBR mode.
//!
//! The rate buffer is built once per frame when the target bitrate
//! changes (encoder caches the last-sent value to avoid redundant
//! per-frame submits).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateControlParams {
    pub bits_per_second: u32,
    pub target_percentage: u32, // 100 = strict CBR target
    pub window_size_ms: u32,    // 1500 ms is typical
    pub initial_qp: u32,
    pub min_qp: u32,
    pub max_qp: u32,
}

impl RateControlParams {
    pub fn cbr_baseline(bitrate_bps: u32) -> Self {
        Self {
            bits_per_second: bitrate_bps,
            target_percentage: 100,
            window_size_ms: 1500,
            initial_qp: 0, // 0 = let encoder pick (Intel iHD honors)
            min_qp: 0,
            max_qp: 0, // 0 = no caps (defer to driver default)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbr_baseline_defaults() {
        let r = RateControlParams::cbr_baseline(5_000_000);
        assert_eq!(r.bits_per_second, 5_000_000);
        assert_eq!(r.target_percentage, 100);
        assert_eq!(r.window_size_ms, 1500);
    }
}
