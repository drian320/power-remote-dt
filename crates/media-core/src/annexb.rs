//! Annex-B bitstream normalizer (codec-neutral).
//!
//! VAAPI coded-buffer output is driver-dependent: start codes may be
//! 3-byte (00 00 01) or 4-byte (00 00 00 01); SPS/PPS may be inline or
//! absent. This module produces a consistent 4-byte-prefixed Annex-B
//! stream and prepends a cached SPS+PPS blob on IDR frames so that the
//! downstream consumer sees a uniform format regardless of which driver
//! wrote the coded buffer.

use crate::error::AnnexBError;

const ANNEXB_4: &[u8] = &[0x00, 0x00, 0x00, 0x01];

/// Walk a coded buffer's contents and re-emit as 4-byte Annex-B into
/// `out`. If `is_idr`, the `sps_pps` blob is prepended before the first
/// NAL emitted from `raw`.
///
/// The input `raw` may contain:
/// - Multiple NAL units separated by either `00 00 01` or `00 00 00 01`
/// - A trailing non-NAL byte stream segment (driver padding) — flagged.
/// - An empty buffer (encoder failed) — returns Err.
pub fn normalize_to_annexb(
    raw: &[u8],
    sps_pps: &[u8],
    is_idr: bool,
    out: &mut Vec<u8>,
) -> Result<(), AnnexBError> {
    if raw.is_empty() {
        return Err(AnnexBError::Empty);
    }
    if is_idr && !sps_pps.is_empty() {
        out.extend_from_slice(sps_pps);
    }
    // Scan for start codes; copy NAL bodies + 4-byte start code.
    let mut i = 0;
    let mut found_any = false;
    while i < raw.len() {
        let three = i + 2 < raw.len() && raw[i] == 0 && raw[i + 1] == 0 && raw[i + 2] == 1;
        let four = i + 3 < raw.len()
            && raw[i] == 0
            && raw[i + 1] == 0
            && raw[i + 2] == 0
            && raw[i + 3] == 1;
        if three || four {
            // Find the next start code (or end of buffer) to delimit this NAL.
            let nal_start = if four { i + 4 } else { i + 3 };
            let mut nal_end = raw.len();
            let mut j = nal_start;
            while j + 2 < raw.len() {
                if raw[j] == 0
                    && raw[j + 1] == 0
                    && (raw[j + 2] == 1
                        || (j + 3 < raw.len() && raw[j + 2] == 0 && raw[j + 3] == 1))
                {
                    nal_end = j;
                    break;
                }
                j += 1;
            }
            out.extend_from_slice(ANNEXB_4);
            out.extend_from_slice(&raw[nal_start..nal_end]);
            found_any = true;
            i = nal_end;
        } else {
            i += 1;
        }
    }
    if !found_any {
        return Err(AnnexBError::NoStartCode { len: raw.len() });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_4byte_input_passes_through_unchanged() {
        let raw = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0xaa, 0xbb, // SPS-like
            0x00, 0x00, 0x00, 0x01, 0x68, 0xcc, // PPS-like
        ];
        let mut out = Vec::new();
        normalize_to_annexb(&raw, &[], false, &mut out).expect("ok");
        assert_eq!(out, raw);
    }

    #[test]
    fn normalize_collapses_3byte_to_4byte() {
        let raw = vec![0x00, 0x00, 0x01, 0x67, 0xaa, 0x00, 0x00, 0x01, 0x68, 0xcc];
        let mut out = Vec::new();
        normalize_to_annexb(&raw, &[], false, &mut out).expect("ok");
        assert_eq!(
            out,
            vec![0x00, 0x00, 0x00, 0x01, 0x67, 0xaa, 0x00, 0x00, 0x00, 0x01, 0x68, 0xcc,]
        );
    }

    #[test]
    fn normalize_prepends_sps_pps_on_idr() {
        let sps_pps = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce,
            0x06, 0xe2,
        ];
        let raw = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84]; // IDR slice
        let mut out = Vec::new();
        normalize_to_annexb(&raw, &sps_pps, true, &mut out).expect("ok");
        // SPS+PPS first, then IDR.
        assert!(out.starts_with(&sps_pps));
        assert!(out.ends_with(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84]));
    }

    #[test]
    fn normalize_rejects_empty_input() {
        let mut out = Vec::new();
        let e = normalize_to_annexb(&[], &[], false, &mut out).unwrap_err();
        assert!(matches!(e, AnnexBError::Empty));
    }

    #[test]
    fn normalize_rejects_input_without_start_code() {
        let raw = vec![0xff, 0xee, 0xdd, 0xcc];
        let mut out = Vec::new();
        let e = normalize_to_annexb(&raw, &[], false, &mut out).unwrap_err();
        assert!(matches!(e, AnnexBError::NoStartCode { len: 4 }));
    }
}
