//! DXGI Desktop Duplication wrapper.

use std::time::Duration;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO,
};

use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
use crate::dxgi::output::OutputInfo;
use crate::error::{MediaError, Result};

/// Outcome of a single `acquire_next_frame` call.
pub enum AcquiredFrame {
    /// A new frame arrived. Caller must call `release_frame()` after processing
    /// (or drop the DesktopDuplication, which calls it automatically).
    Frame {
        texture: D3d11Texture,
        frame_info: DXGI_OUTDUPL_FRAME_INFO,
    },
    /// No new frame within the timeout window. The desktop didn't change.
    Timeout,
}

pub struct DesktopDuplication {
    dup: IDXGIOutputDuplication,
    width: u32,
    height: u32,
    frame_held: bool,
}

impl DesktopDuplication {
    pub fn new(dev: &D3d11Device, output: &OutputInfo) -> Result<Self> {
        unsafe {
            let dxgi_device: IDXGIDevice = dev
                .device()
                .cast()
                .map_err(|e| MediaError::dxgi("D3D11 device -> IDXGIDevice cast", e))?;

            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| MediaError::dxgi("CreateDXGIFactory1", e))?;
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapters1(dev.adapter().index)
                .map_err(|e| MediaError::dxgi("EnumAdapters1", e))?;
            let output_base: IDXGIOutput = adapter
                .EnumOutputs(output.index)
                .map_err(|e| MediaError::dxgi("EnumOutputs", e))?;
            let output1: IDXGIOutput1 = output_base
                .cast()
                .map_err(|e| MediaError::dxgi("IDXGIOutput -> IDXGIOutput1 cast", e))?;

            let dup: IDXGIOutputDuplication = output1
                .DuplicateOutput(&dxgi_device)
                .map_err(|e| MediaError::dxgi("DuplicateOutput", e))?;

            let width = (output.desktop_rect.right - output.desktop_rect.left) as u32;
            let height = (output.desktop_rect.bottom - output.desktop_rect.top) as u32;

            Ok(Self {
                dup,
                width,
                height,
                frame_held: false,
            })
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Acquire the next duplicated frame. `timeout` is clamped to u32 milliseconds.
    pub fn acquire_next_frame(&mut self, timeout: Duration) -> Result<AcquiredFrame> {
        if self.frame_held {
            unsafe {
                self.dup
                    .ReleaseFrame()
                    .map_err(|e| MediaError::dxgi("ReleaseFrame (before acquire)", e))?;
            }
            self.frame_held = false;
        }

        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;

        let ret = unsafe { self.dup.AcquireNextFrame(ms, &mut info, &mut resource) };
        match ret {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(AcquiredFrame::Timeout),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                return Err(MediaError::Dxgi {
                    context: "AcquireNextFrame (access lost)",
                    hresult: DXGI_ERROR_ACCESS_LOST.0 as u32,
                });
            }
            Err(e) => return Err(MediaError::dxgi("AcquireNextFrame", e)),
        }
        self.frame_held = true;

        let resource = resource.ok_or(MediaError::Dxgi {
            context: "AcquireNextFrame returned null IDXGIResource",
            hresult: 0,
        })?;

        let texture_com: ID3D11Texture2D = resource
            .cast()
            .map_err(|e| MediaError::d3d11("IDXGIResource -> ID3D11Texture2D cast", e))?;

        let tex =
            D3d11Texture::from_raw(texture_com, self.width, self.height, TextureFormat::Bgra8);
        Ok(AcquiredFrame::Frame {
            texture: tex,
            frame_info: info,
        })
    }

    pub fn release_frame(&mut self) -> Result<()> {
        if self.frame_held {
            unsafe {
                self.dup
                    .ReleaseFrame()
                    .map_err(|e| MediaError::dxgi("ReleaseFrame", e))?;
            }
            self.frame_held = false;
        }
        Ok(())
    }
}

impl Drop for DesktopDuplication {
    fn drop(&mut self) {
        let _ = self.release_frame();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pick_default_adapter;
    use crate::d3d11::D3d11Device;
    use crate::dxgi::output::enumerate_outputs_for_adapter;

    #[test]
    fn create_duplication_on_primary_output() {
        // Best-effort: requires a desktop session.
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("no adapter: {e}");
                return;
            }
        };
        let dev = match D3d11Device::create(&adapter) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("no device: {e}");
                return;
            }
        };
        let outputs = match enumerate_outputs_for_adapter(&adapter) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("no outputs: {e}");
                return;
            }
        };
        if outputs.is_empty() {
            eprintln!("no outputs available");
            return;
        }
        match DesktopDuplication::new(&dev, &outputs[0]) {
            Ok(dup) => {
                assert_eq!(
                    dup.width(),
                    (outputs[0].desktop_rect.right - outputs[0].desktop_rect.left) as u32
                );
            }
            Err(e) => eprintln!("duplication creation (non-fatal): {e}"),
        }
    }
}
