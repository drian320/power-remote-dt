//! Plan 4 latency bench library — shared between the single-config bin
//! (`prdt-latency-bench`) and the matrix bin (`prdt-bench-matrix`).

#[cfg(windows)]
pub mod full_pipeline;

/// Compute (p50, p90, p95, p99, p100) by sorting in place. Sorts the input.
pub fn percentiles(lags_us: &mut [u64]) -> (u64, u64, u64, u64, u64) {
    lags_us.sort_unstable();
    let pick = |p: f64| -> u64 {
        let idx = ((lags_us.len() as f64 - 1.0) * p).round() as usize;
        lags_us[idx]
    };
    (
        pick(0.50),
        pick(0.90),
        pick(0.95),
        pick(0.99),
        *lags_us.last().unwrap_or(&0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_monotonic() {
        let mut v: Vec<u64> = (1..=100).collect();
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert!(p50 <= p90);
        assert!(p90 <= p95);
        assert!(p95 <= p99);
        assert!(p99 <= p100);
        assert_eq!(p100, 100);
    }

    #[test]
    fn percentiles_single_sample() {
        let mut v = vec![42u64];
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert_eq!((p50, p90, p95, p99, p100), (42, 42, 42, 42, 42));
    }
}
