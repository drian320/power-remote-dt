//! Error surface for media-win. Wraps both windows HRESULT and higher-level
//! semantic failures (e.g. "no adapter found").

use windows::core::HRESULT;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("D3D11: {context}: HRESULT 0x{hresult:08x}")]
    D3D11 { context: &'static str, hresult: u32 },

    #[error("DXGI: {context}: HRESULT 0x{hresult:08x}")]
    Dxgi { context: &'static str, hresult: u32 },

    /// The D3D11 device was removed — TDR (timeout detection + recovery),
    /// driver crash, hybrid-GPU switch, hot-unplug. The current device and
    /// every resource bound to it are unusable; recovery needs a fresh
    /// `D3D11Device` plus re-created swapchain / encoder / decoder.
    /// `reason` is the HRESULT returned by `GetDeviceRemovedReason`.
    #[error("D3D11 device removed ({context}): reason 0x{reason:08x}")]
    DeviceRemoved { context: &'static str, reason: u32 },

    #[error("no suitable adapter found (requested: {requested})")]
    NoAdapter { requested: String },

    #[error("adapter index out of range: {index} (have {count})")]
    AdapterOutOfRange { index: u32, count: u32 },

    #[error("unsupported pixel format: {fmt}")]
    UnsupportedFormat { fmt: &'static str },

    #[error("MMCSS task registration failed: {reason}")]
    MmcssFailed { reason: String },

    /// HDR10 swapchain construction failed because no HDR-capable display was
    /// detected or the driver/compositor does not support HDR10 presentation.
    /// Callers may surface this to the user; opt-in SDR-fallback is via the
    /// `media-win-hdr-to-sdr-fallback` feature (not the default).
    #[error("HDR10 unavailable: {reason}")]
    HdrUnavailable { reason: String },

    /// HEVC Main10 (or another codec-profile combination) decoder MFT is not
    /// available on this host — typically because the OS SKU lacks the HEVC
    /// Video Extensions UWP codec package, or because the Windows 10 build
    /// predates 1709 (the first SKU shipping the Microsoft Hybrid HEVC decoder
    /// MFT). Callers MUST surface this to the user; no silent fallback to a
    /// different codec profile (the 8-bit MFT cannot parse a 10-bit bitstream).
    /// `reason` includes a remediation pointer to the Microsoft Store package
    /// (ProductId 9NMZLZ57R3T7 — paid; or 9N4WGH0Z6VHQ — free OEM variant).
    #[error("decoder not available: {codec}: {reason}")]
    DecoderNotAvailable { codec: String, reason: String },

    /// A hardware encoder (e.g. `hevc_nvenc`) is not available on this host —
    /// typically because no compatible GPU or driver is present. Callers may
    /// fall back to a software or MF encoder path.
    #[error("encoder not available: {codec}: {reason}")]
    EncoderNotAvailable { codec: String, reason: String },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("other: {0}")]
    Other(String),
}

impl MediaError {
    /// Helper for wrapping a windows::core::Error from a D3D11 call.
    pub fn d3d11(context: &'static str, err: windows::core::Error) -> Self {
        Self::D3D11 {
            context,
            hresult: err.code().0 as u32,
        }
    }

    /// Helper for wrapping a windows::core::Error from a DXGI call.
    pub fn dxgi(context: &'static str, err: windows::core::Error) -> Self {
        Self::Dxgi {
            context,
            hresult: err.code().0 as u32,
        }
    }

    /// Return the HRESULT if this is an HRESULT-bearing variant.
    pub fn hresult(&self) -> Option<HRESULT> {
        match self {
            Self::D3D11 { hresult, .. } | Self::Dxgi { hresult, .. } => {
                Some(HRESULT(*hresult as i32))
            }
            Self::DeviceRemoved { reason, .. } => Some(HRESULT(*reason as i32)),
            _ => None,
        }
    }

    /// True if this error indicates the D3D11 device is gone and every
    /// resource bound to it must be recreated.
    pub fn is_device_removed(&self) -> bool {
        matches!(self, Self::DeviceRemoved { .. })
    }
}

pub type Result<T> = std::result::Result<T, MediaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable() {
        let e = MediaError::NoAdapter {
            requested: "nvidia".into(),
        };
        assert_eq!(
            e.to_string(),
            "no suitable adapter found (requested: nvidia)"
        );

        let e = MediaError::D3D11 {
            context: "CreateDevice",
            hresult: 0x887A0005,
        };
        assert_eq!(e.to_string(), "D3D11: CreateDevice: HRESULT 0x887a0005");
    }

    #[test]
    fn hresult_roundtrip() {
        let e = MediaError::Dxgi {
            context: "EnumAdapters",
            hresult: 0x887A0002,
        };
        assert_eq!(e.hresult().unwrap().0 as u32, 0x887A0002);

        let e = MediaError::NoAdapter {
            requested: "any".into(),
        };
        assert!(e.hresult().is_none());
    }

    #[test]
    fn device_removed_flag_distinguishes_variants() {
        let removed = MediaError::DeviceRemoved {
            context: "Present",
            reason: 0x887A0020, // DXGI_ERROR_INVALID_CALL as a stand-in
        };
        assert!(removed.is_device_removed());
        assert_eq!(removed.hresult().unwrap().0 as u32, 0x887A0020);

        let dxgi = MediaError::Dxgi {
            context: "Present",
            hresult: 0x887A0005,
        };
        assert!(!dxgi.is_device_removed());
    }

    #[test]
    fn device_removed_display_is_readable() {
        let e = MediaError::DeviceRemoved {
            context: "Present",
            reason: 0x887A0005,
        };
        let s = e.to_string();
        assert!(s.contains("device removed"));
        assert!(s.contains("Present"));
        assert!(s.contains("887a0005"));
    }

    #[test]
    fn decoder_not_available_display_is_readable() {
        let e = MediaError::DecoderNotAvailable {
            codec: "HEVC Main10".into(),
            reason: "no MFT registered. Install HEVC Video Extensions (ProductId 9NMZLZ57R3T7)"
                .into(),
        };
        let s = e.to_string();
        assert!(s.contains("HEVC Main10"));
        assert!(s.contains("9NMZLZ57R3T7"));
    }
}
