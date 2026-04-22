use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::error::TransportError;

/// Default FEC parameters for Phase 0. See spec §5.7.
pub const DEFAULT_K: usize = 8;
pub const DEFAULT_M: usize = 2;
pub const MAX_SHARDS: usize = 32 + 16; // defensive cap; 1 frame max 32 source + 16 parity

/// Wraps a ReedSolomon codec with per-frame shard encoding/decoding.
///
/// All shards MUST have the same length. Callers pad the last source
/// shard with zeros before passing here (the length is tracked separately
/// in the VideoPacket.payload_bytes field so the receiver knows the true
/// length of the final source chunk).
pub struct FecCodec {
    k: usize,
    m: usize,
    rs: ReedSolomon,
}

impl FecCodec {
    pub fn new(k: usize, m: usize) -> Result<Self, TransportError> {
        if k == 0 || m == 0 {
            return Err(TransportError::FecConfig(format!(
                "k={k}, m={m} must be > 0"
            )));
        }
        if k + m > MAX_SHARDS {
            return Err(TransportError::FecConfig(format!(
                "k+m={} exceeds MAX_SHARDS={}",
                k + m,
                MAX_SHARDS,
            )));
        }
        let rs = ReedSolomon::new(k, m)
            .map_err(|e| TransportError::FecConfig(format!("reed-solomon: {e}")))?;
        Ok(Self { k, m, rs })
    }

    pub fn k(&self) -> usize {
        self.k
    }
    pub fn m(&self) -> usize {
        self.m
    }

    /// Produce m parity shards given k source shards (all same length).
    pub fn encode_parity(&self, source: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, TransportError> {
        if source.len() != self.k {
            return Err(TransportError::FecConfig(format!(
                "expected {} source shards, got {}",
                self.k,
                source.len(),
            )));
        }
        let shard_len = source[0].len();
        for s in source {
            if s.len() != shard_len {
                return Err(TransportError::FecConfig(
                    "source shards must all be same length".into(),
                ));
            }
        }
        let mut all: Vec<Vec<u8>> = source.to_vec();
        for _ in 0..self.m {
            all.push(vec![0u8; shard_len]);
        }
        self.rs
            .encode(&mut all)
            .map_err(|e| TransportError::FecConfig(format!("rs encode: {e}")))?;
        Ok(all.split_off(self.k)) // only parity
    }

    /// Reconstruct missing source shards. `shards[i] = None` marks missing.
    /// Length of `shards` must be exactly k + m. Returns k source shards.
    pub fn reconstruct(
        &self,
        shards: Vec<Option<Vec<u8>>>,
    ) -> Result<Vec<Vec<u8>>, TransportError> {
        if shards.len() != self.k + self.m {
            return Err(TransportError::FecConfig(format!(
                "expected {} shards, got {}",
                self.k + self.m,
                shards.len(),
            )));
        }
        let have = shards.iter().filter(|s| s.is_some()).count();
        if have < self.k {
            return Err(TransportError::FecFailed {
                frame_seq: 0, // caller overrides if they have seq context
                have,
                need: self.k,
            });
        }
        let mut rs_shards = shards;
        self.rs
            .reconstruct(&mut rs_shards)
            .map_err(|e| TransportError::FecConfig(format!("rs reconstruct: {e}")))?;
        Ok(rs_shards
            .into_iter()
            .take(self.k)
            .map(|s| s.unwrap())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fec_round_trip_no_loss() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 100]).collect();
        let parity = codec.encode_parity(&source).unwrap();
        assert_eq!(parity.len(), 2);
        assert_eq!(parity[0].len(), 100);
    }

    #[test]
    fn fec_reconstruct_one_lost_source() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 50]).collect();
        let parity = codec.encode_parity(&source).unwrap();

        // Lose source shard index 1.
        let mut shards: Vec<Option<Vec<u8>>> = source.iter().cloned().map(Some).collect();
        shards[1] = None;
        shards.extend(parity.into_iter().map(Some));

        let recovered = codec.reconstruct(shards).unwrap();
        assert_eq!(recovered.len(), 4);
        for (i, s) in recovered.iter().enumerate() {
            assert_eq!(*s, vec![i as u8; 50], "shard {i} mismatch");
        }
    }

    #[test]
    fn fec_reconstruct_two_lost() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 32]).collect();
        let parity = codec.encode_parity(&source).unwrap();

        let mut shards: Vec<Option<Vec<u8>>> = source.iter().cloned().map(Some).collect();
        shards[0] = None;
        shards[3] = None;
        shards.extend(parity.into_iter().map(Some));

        let recovered = codec.reconstruct(shards).unwrap();
        assert_eq!(recovered.len(), 4);
        for (i, s) in recovered.iter().enumerate() {
            assert_eq!(*s, vec![i as u8; 32]);
        }
    }

    #[test]
    fn fec_fails_when_too_many_lost() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 16]).collect();
        let parity = codec.encode_parity(&source).unwrap();

        // Lose 3 shards; with k=4, m=2 we need 4 of 6. 3 lost = 3 have → fail.
        let mut shards: Vec<Option<Vec<u8>>> = source.iter().cloned().map(Some).collect();
        shards[0] = None;
        shards[1] = None;
        shards[2] = None;
        shards.extend(parity.into_iter().map(Some));

        match codec.reconstruct(shards) {
            Err(TransportError::FecFailed {
                have: 3, need: 4, ..
            }) => {}
            other => panic!("expected FecFailed, got {:?}", other),
        }
    }

    #[test]
    fn fec_bad_config() {
        assert!(matches!(
            FecCodec::new(0, 2),
            Err(TransportError::FecConfig(_))
        ));
        assert!(matches!(
            FecCodec::new(100, 100),
            Err(TransportError::FecConfig(_))
        ));
    }
}
