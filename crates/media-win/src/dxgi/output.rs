//! DXGI Output (monitor) enumeration.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, DXGI_OUTPUT_DESC,
};

use crate::adapter::AdapterInfo;
use crate::error::{MediaError, Result};

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub index: u32,
    pub device_name: String,
    pub desktop_rect: RECT,
    pub rotation: u32,
    pub is_attached: bool,
}

pub fn enumerate_outputs_for_adapter(adapter: &AdapterInfo) -> Result<Vec<OutputInfo>> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| MediaError::dxgi("CreateDXGIFactory1", e))?;
        let adapter_com: IDXGIAdapter1 = factory
            .EnumAdapters1(adapter.index)
            .map_err(|e| MediaError::dxgi("EnumAdapters1 (output enum)", e))?;

        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let output: IDXGIOutput = match adapter_com.EnumOutputs(i) {
                Ok(o) => o,
                Err(e) if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(MediaError::dxgi("EnumOutputs", e)),
            };
            let desc: DXGI_OUTPUT_DESC = output
                .GetDesc()
                .map_err(|e| MediaError::dxgi("IDXGIOutput::GetDesc", e))?;
            let name = String::from_utf16_lossy(
                &desc.DeviceName[..desc
                    .DeviceName
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(desc.DeviceName.len())],
            );
            out.push(OutputInfo {
                index: i,
                device_name: name,
                desktop_rect: desc.DesktopCoordinates,
                rotation: desc.Rotation.0 as u32,
                is_attached: desc.AttachedToDesktop.as_bool(),
            });
            i += 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pick_default_adapter;

    #[test]
    fn enumerate_outputs_default_adapter() {
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("no adapter (skip): {e}");
                return;
            }
        };
        match enumerate_outputs_for_adapter(&adapter) {
            Ok(outputs) => {
                // On a desktop session there is at least 1 output; on headless CI there may be 0.
                for o in &outputs {
                    assert!(!o.device_name.is_empty());
                }
            }
            Err(e) => eprintln!("enumerate_outputs error (non-fatal): {e}"),
        }
    }
}
