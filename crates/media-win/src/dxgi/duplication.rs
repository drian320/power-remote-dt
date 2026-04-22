//! DXGI Desktop Duplication wrapper. Acquires next frame and returns a D3d11Texture.
//! Full impl lands in Task 2.

use crate::d3d11::D3d11Device;
use crate::dxgi::output::OutputInfo;
use crate::error::{MediaError, Result};

pub struct DesktopDuplication {
    _placeholder: (),
}

impl DesktopDuplication {
    pub fn new(_dev: &D3d11Device, _output: &OutputInfo) -> Result<Self> {
        Err(MediaError::Other(
            "DesktopDuplication::new: not yet implemented (Task 2)".into(),
        ))
    }
}
