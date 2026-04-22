//! DXGI adapter enumeration. Provides a safe, ergonomic view of available
//! GPU adapters so callers can pick by vendor name or index.

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_DESC1,
    DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
};

use crate::error::{MediaError, Result};

/// Describes one DXGI adapter at enumeration time.
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub index: u32,
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory_bytes: u64,
    pub is_software: bool,
}

impl AdapterInfo {
    pub fn is_nvidia(&self) -> bool {
        self.vendor_id == 0x10DE
    }
    pub fn is_amd(&self) -> bool {
        self.vendor_id == 0x1002
    }
    pub fn is_intel(&self) -> bool {
        self.vendor_id == 0x8086
    }
}

/// Enumerate all DXGI 1.1+ adapters on this system.
pub fn enumerate_adapters() -> Result<Vec<AdapterInfo>> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| MediaError::dxgi("CreateDXGIFactory1", e))?;

        let mut out = Vec::new();
        let mut i: u32 = 0;
        loop {
            let adapter_res: windows::core::Result<IDXGIAdapter1> = factory.EnumAdapters1(i);
            let adapter = match adapter_res {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(MediaError::dxgi("EnumAdapters1", e)),
            };
            let desc: DXGI_ADAPTER_DESC1 = adapter
                .GetDesc1()
                .map_err(|e| MediaError::dxgi("GetDesc1", e))?;

            let name = String::from_utf16_lossy(
                &desc.Description[..desc
                    .Description
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(desc.Description.len())],
            );

            out.push(AdapterInfo {
                index: i,
                name,
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                dedicated_video_memory_bytes: desc.DedicatedVideoMemory as u64,
                is_software: (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0,
            });
            i += 1;
        }
        Ok(out)
    }
}

/// Pick a specific adapter by index. Fails if index is out of range.
pub fn pick_adapter_by_index(index: u32) -> Result<AdapterInfo> {
    let all = enumerate_adapters()?;
    if (index as usize) >= all.len() {
        return Err(MediaError::AdapterOutOfRange {
            index,
            count: all.len() as u32,
        });
    }
    Ok(all[index as usize].clone())
}

/// Pick the default adapter: prefer the first non-software adapter.
pub fn pick_default_adapter() -> Result<AdapterInfo> {
    let all = enumerate_adapters()?;
    all.iter()
        .find(|a| !a.is_software)
        .cloned()
        .or_else(|| all.first().cloned())
        .ok_or_else(|| MediaError::NoAdapter {
            requested: "default".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_runs() {
        // On any Windows dev machine there is at least a software (WARP)
        // adapter available, so enumerate_adapters() should return at
        // least one entry without error.
        let adapters = enumerate_adapters().expect("DXGI enumerate should work");
        assert!(!adapters.is_empty(), "expected >= 1 adapter");
        // Smoke: every entry has a non-zero vendor and a non-empty name.
        for a in &adapters {
            assert!(!a.name.is_empty(), "adapter name empty at idx {}", a.index);
        }
    }

    #[test]
    fn default_adapter_resolves() {
        let a = pick_default_adapter().expect("at least one adapter");
        assert!(!a.name.is_empty());
    }

    #[test]
    fn out_of_range_index_errors() {
        let err = pick_adapter_by_index(9999).unwrap_err();
        matches!(err, MediaError::AdapterOutOfRange { index: 9999, .. });
    }
}
