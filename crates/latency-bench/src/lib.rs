//! Plan 4 latency bench library — shared between the single-config bin
//! (`prdt-latency-bench`) and the matrix bin (`prdt-bench-matrix`).

#[cfg(windows)]
pub mod full_pipeline;

#[cfg(windows)]
pub use full_pipeline::{ConsumerBackend, FullPipelineConfig, RunStats, StageTimes};

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

#[cfg(windows)]
mod matrix {
    use super::{percentiles, ConsumerBackend, FullPipelineConfig, RunStats};

    /// CLI-supplied axes for the matrix bin.
    pub struct MatrixAxes {
        pub resolutions: Vec<(u32, u32)>,
        pub bitrates_mbps: Vec<u32>,
        pub decoders: Vec<ConsumerBackend>,
        pub fps: Vec<u32>,
        pub duration: std::time::Duration,
    }

    /// One row of summary.csv.
    pub struct ConfigStats {
        pub config_id: String,
        pub resolution: (u32, u32),
        pub bitrate_mbps: u32,
        pub decoder: ConsumerBackend,
        pub fps: u32,
        pub sent: u64,
        pub received: u64,
        pub loss_ppm: u64,
        pub arrival_p50_us: u64,
        pub arrival_p95_us: u64,
        pub arrival_p99_us: u64,
        pub decode_p50_us: u64,
        pub decode_p95_us: u64,
        pub decode_p99_us: u64,
        pub e2e_p50_us: u64,
        pub e2e_p95_us: u64,
        pub e2e_p99_us: u64,
    }

    /// Stable, filesystem-safe identifier for a config:
    /// `{height}p{fps}-{bitrate}mbps-{decoder}`.
    pub fn config_id(
        resolution: (u32, u32),
        fps: u32,
        bitrate_mbps: u32,
        decoder: ConsumerBackend,
    ) -> String {
        let dec = match decoder {
            ConsumerBackend::Mf => "mf",
            ConsumerBackend::Nvdec => "nvdec",
        };
        format!("{}p{}-{}mbps-{}", resolution.1, fps, bitrate_mbps, dec)
    }

    /// Expand axes into a `Vec<FullPipelineConfig>`. Order:
    /// resolution outer → bitrate → decoder → fps inner.
    pub fn expand_matrix(axes: &MatrixAxes) -> Vec<FullPipelineConfig> {
        let mut out = Vec::with_capacity(
            axes.resolutions.len()
                * axes.bitrates_mbps.len()
                * axes.decoders.len()
                * axes.fps.len(),
        );
        for &res in &axes.resolutions {
            for &bitrate_mbps in &axes.bitrates_mbps {
                for &decoder in &axes.decoders {
                    for &fps in &axes.fps {
                        out.push(FullPipelineConfig {
                            width: res.0,
                            height: res.1,
                            fps,
                            duration: axes.duration,
                            bitrate_bps: bitrate_mbps.saturating_mul(1_000_000),
                            drop_ppm: 0,
                            latency_ms: 0,
                            csv: None,
                            consumer: decoder,
                        });
                    }
                }
            }
        }
        out
    }

    /// Aggregate per-frame raw into the summary row. Empty `frames` produces
    /// a "skip row" with `loss_ppm = 1_000_000` and all percentiles = 0.
    pub fn aggregate(cfg: &FullPipelineConfig, run: &RunStats) -> ConfigStats {
        let id = config_id(
            (cfg.width, cfg.height),
            cfg.fps,
            cfg.bitrate_bps / 1_000_000,
            cfg.consumer,
        );
        if run.frames.is_empty() {
            return ConfigStats {
                config_id: id,
                resolution: (cfg.width, cfg.height),
                bitrate_mbps: cfg.bitrate_bps / 1_000_000,
                decoder: cfg.consumer,
                fps: cfg.fps,
                sent: run.sent,
                received: run.received,
                loss_ppm: 1_000_000,
                arrival_p50_us: 0,
                arrival_p95_us: 0,
                arrival_p99_us: 0,
                decode_p50_us: 0,
                decode_p95_us: 0,
                decode_p99_us: 0,
                e2e_p50_us: 0,
                e2e_p95_us: 0,
                e2e_p99_us: 0,
            };
        }
        let mut arrival: Vec<u64> = run
            .frames
            .iter()
            .map(|s| s.recv_us.saturating_sub(s.capture_us))
            .collect();
        let mut decode: Vec<u64> = run
            .frames
            .iter()
            .map(|s| s.decode_done_us.saturating_sub(s.recv_us))
            .collect();
        let mut e2e: Vec<u64> = run
            .frames
            .iter()
            .map(|s| s.decode_done_us.saturating_sub(s.capture_us))
            .collect();
        let (a50, _, a95, a99, _) = percentiles(&mut arrival);
        let (d50, _, d95, d99, _) = percentiles(&mut decode);
        let (e50, _, e95, e99, _) = percentiles(&mut e2e);
        let loss_ppm = if run.sent > 0 {
            ((run.sent.saturating_sub(run.received)) as f64 / run.sent as f64
                * 1_000_000.0) as u64
        } else {
            0
        };
        ConfigStats {
            config_id: id,
            resolution: (cfg.width, cfg.height),
            bitrate_mbps: cfg.bitrate_bps / 1_000_000,
            decoder: cfg.consumer,
            fps: cfg.fps,
            sent: run.sent,
            received: run.received,
            loss_ppm,
            arrival_p50_us: a50,
            arrival_p95_us: a95,
            arrival_p99_us: a99,
            decode_p50_us: d50,
            decode_p95_us: d95,
            decode_p99_us: d99,
            e2e_p50_us: e50,
            e2e_p95_us: e95,
            e2e_p99_us: e99,
        }
    }
}

#[cfg(windows)]
pub use matrix::{aggregate, config_id, expand_matrix, ConfigStats, MatrixAxes};

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

    #[cfg(windows)]
    #[test]
    fn config_id_format_canonical() {
        let id = config_id((1920, 1080), 60, 30, ConsumerBackend::Mf);
        assert_eq!(id, "1080p60-30mbps-mf");

        let id = config_id((3840, 2160), 120, 50, ConsumerBackend::Nvdec);
        assert_eq!(id, "2160p120-50mbps-nvdec");
    }

    #[cfg(windows)]
    #[test]
    fn expand_matrix_produces_cartesian_product() {
        let axes = MatrixAxes {
            resolutions: vec![(1920, 1080), (2560, 1440)],
            bitrates_mbps: vec![10, 30],
            decoders: vec![ConsumerBackend::Mf],
            fps: vec![60],
            duration: std::time::Duration::from_secs(10),
        };
        let configs = expand_matrix(&axes);
        // 2 * 2 * 1 * 1 = 4 configs
        assert_eq!(configs.len(), 4);
        // Order: outermost = resolution, then bitrate, then decoder, then fps
        assert_eq!((configs[0].width, configs[0].height), (1920, 1080));
        assert_eq!(configs[0].bitrate_bps, 10_000_000);
        assert_eq!((configs[1].width, configs[1].height), (1920, 1080));
        assert_eq!(configs[1].bitrate_bps, 30_000_000);
        assert_eq!((configs[2].width, configs[2].height), (2560, 1440));
        assert_eq!(configs[2].bitrate_bps, 10_000_000);
        assert_eq!((configs[3].width, configs[3].height), (2560, 1440));
        assert_eq!(configs[3].bitrate_bps, 30_000_000);
    }

    #[cfg(windows)]
    #[test]
    fn aggregate_empty_run_emits_skip_row() {
        let cfg = FullPipelineConfig {
            width: 1920, height: 1080, fps: 60,
            duration: std::time::Duration::from_secs(10),
            bitrate_bps: 30_000_000, drop_ppm: 0, latency_ms: 0,
            csv: None, consumer: ConsumerBackend::Mf,
        };
        let run = RunStats { sent: 0, received: 0, frames: vec![] };
        let stats = aggregate(&cfg, &run);
        assert_eq!(stats.config_id, "1080p60-30mbps-mf");
        assert_eq!(stats.loss_ppm, 1_000_000);
        assert_eq!(stats.arrival_p50_us, 0);
        assert_eq!(stats.e2e_p99_us, 0);
    }

    #[cfg(windows)]
    #[test]
    fn aggregate_full_run_computes_percentiles() {
        let cfg = FullPipelineConfig {
            width: 1920, height: 1080, fps: 60,
            duration: std::time::Duration::from_secs(10),
            bitrate_bps: 30_000_000, drop_ppm: 0, latency_ms: 0,
            csv: None, consumer: ConsumerBackend::Mf,
        };
        // 100 frames with arrival_lag_us = i, decode_lag_us = 2*i, e2e = 3*i.
        let frames: Vec<StageTimes> = (1..=100u64)
            .map(|i| StageTimes {
                seq: i,
                capture_us: 0,
                encode_done_us: 0,
                recv_us: i,
                decode_done_us: 3 * i,
            })
            .collect();
        let run = RunStats { sent: 100, received: 100, frames };
        let stats = aggregate(&cfg, &run);
        assert_eq!(stats.sent, 100);
        assert_eq!(stats.received, 100);
        assert_eq!(stats.loss_ppm, 0);
        // arrival_lag = recv - capture = i (1..=100). With round-style
        // percentile picking: p50 = round(99*0.5)=50 → v[50]=51,
        // p95 = round(94.05)=94 → v[94]=95, p99 = round(98.01)=98 → v[98]=99.
        assert_eq!(stats.arrival_p50_us, 51);
        assert_eq!(stats.arrival_p95_us, 95);
        assert_eq!(stats.arrival_p99_us, 99);
        // e2e_lag = decode_done - capture = 3i. Same indices times 3:
        // p50 = 3*51 = 153, p99 = 3*99 = 297.
        assert_eq!(stats.e2e_p50_us, 153);
        assert_eq!(stats.e2e_p99_us, 297);
    }
}
