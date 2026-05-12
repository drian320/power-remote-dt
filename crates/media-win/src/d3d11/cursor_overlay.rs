//! Windows D3D11 cursor overlay — composite cursor on top of the
//! VideoProcessorBlt'd swapchain backbuffer before IDXGISwapChain1::Present.
//!
//! P5B-2b ships this as a stub; full implementation is deferred to the
//! Windows follow-up branch because the Debian bookworm dev container
//! cannot compile media-win (Windows SDK + D3D11 headers absent).

#![cfg(target_os = "windows")]

use prdt_protocol::CursorBitmap;

pub struct CursorOverlay {
    // … D3D11 texture cache + pixel shader handles (TODO follow-up) …
}

impl CursorOverlay {
    pub fn new(/* device: &ID3D11Device */) -> windows::core::Result<Self> {
        // TODO(P5B-2b-windows-follow-up): create cursor texture + shader
        Ok(Self {})
    }

    /// Update the cached cursor bitmap. Call once per new bitmap-carrying
    /// `ControlMessage::CursorUpdate`.
    pub fn update_bitmap(&mut self, _bitmap: &CursorBitmap) -> windows::core::Result<()> {
        // TODO(P5B-2b-windows-follow-up): upload to ID3D11Texture2D
        Ok(())
    }

    /// Draw the cursor at (x, y) on the current backbuffer. Called after
    /// the video Blt + before IDXGISwapChain1::Present.
    pub fn draw(
        &self,
        _x: i32,
        _y: i32,
        // … swapchain backbuffer RTV …
    ) -> windows::core::Result<()> {
        // TODO(P5B-2b-windows-follow-up): pixel-shader draw
        Ok(())
    }
}
