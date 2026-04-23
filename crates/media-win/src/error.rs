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
}
