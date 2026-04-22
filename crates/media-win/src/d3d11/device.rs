//! Safe wrapper around ID3D11Device + ID3D11DeviceContext.
//!
//! The device handle itself is free-threaded. The immediate context is
//! single-threaded-only; we gate access behind a Mutex. For the hot path
//! (encode/decode), production code can use deferred contexts or bypass
//! this wrapper.

use std::sync::{Arc, Mutex};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

use crate::adapter::AdapterInfo;
use crate::error::{MediaError, Result};

/// Safe handle to an ID3D11Device + its immediate context.
///
/// Clone-cheap (Arc-wrapped). The immediate context is guarded by a Mutex
/// because D3D11 does NOT allow concurrent use of the same immediate context.
#[derive(Clone)]
pub struct D3d11Device {
    inner: Arc<D3d11Inner>,
}

struct D3d11Inner {
    #[allow(dead_code)]
    device: ID3D11Device,
    context: Mutex<ID3D11DeviceContext>,
    feature_level: D3D_FEATURE_LEVEL,
    adapter: AdapterInfo,
}

impl D3d11Device {
    /// Create a D3D11 device on the specified DXGI adapter.
    pub fn create(adapter: &AdapterInfo) -> Result<Self> {
        // Re-enumerate to obtain IDXGIAdapter (we don't cache COM pointers).
        let factory: windows::Win32::Graphics::Dxgi::IDXGIFactory1 = unsafe {
            windows::Win32::Graphics::Dxgi::CreateDXGIFactory1()
                .map_err(|e| MediaError::dxgi("CreateDXGIFactory1", e))?
        };
        let dxgi_adapter: IDXGIAdapter = unsafe {
            factory
                .EnumAdapters1(adapter.index)
                .map_err(|e| MediaError::dxgi("EnumAdapters1", e))?
                .cast()
                .map_err(|e| MediaError::dxgi("IDXGIAdapter cast", e))?
        };

        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut out_device: Option<ID3D11Device> = None;
        let mut out_context: Option<ID3D11DeviceContext> = None;
        let mut out_feature_level = D3D_FEATURE_LEVEL::default();

        let flags: D3D11_CREATE_DEVICE_FLAG = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

        unsafe {
            D3D11CreateDevice(
                &dxgi_adapter,
                D3D_DRIVER_TYPE_UNKNOWN, // required when passing a specific adapter
                windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut out_device),
                Some(&mut out_feature_level),
                Some(&mut out_context),
            )
            .map_err(|e| MediaError::d3d11("D3D11CreateDevice", e))?;
        }

        let device = out_device.ok_or(MediaError::D3D11 {
            context: "D3D11CreateDevice returned null device",
            hresult: 0,
        })?;
        let context = out_context.ok_or(MediaError::D3D11 {
            context: "D3D11CreateDevice returned null context",
            hresult: 0,
        })?;

        Ok(Self {
            inner: Arc::new(D3d11Inner {
                device,
                context: Mutex::new(context),
                feature_level: out_feature_level,
                adapter: adapter.clone(),
            }),
        })
    }

    /// Create on the default adapter (first non-software).
    pub fn create_default() -> Result<Self> {
        let adapter = crate::adapter::pick_default_adapter()?;
        Self::create(&adapter)
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.inner.device
    }

    pub fn adapter(&self) -> &AdapterInfo {
        &self.inner.adapter
    }

    pub fn feature_level(&self) -> D3D_FEATURE_LEVEL {
        self.inner.feature_level
    }

    /// Run a closure with the locked immediate context. Releases the mutex
    /// when the closure returns.
    pub fn with_context<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&ID3D11DeviceContext) -> R,
    {
        let guard = self
            .inner
            .context
            .lock()
            .expect("D3D11 context mutex poisoned");
        f(&guard)
    }

    /// Expose Arc for internal strong-count checks in tests.
    #[cfg(test)]
    pub(crate) fn arc_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_default_device() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        assert!(!dev.adapter().name.is_empty());
        // Non-zero feature level.
        assert!(dev.feature_level().0 > 0);
    }

    #[test]
    fn device_is_cloneable_and_shares_inner() {
        let a = D3d11Device::create_default().expect("D3D11 device");
        let b = a.clone();
        assert_eq!(a.arc_count(), 2);
        drop(b);
        assert_eq!(a.arc_count(), 1);
    }
}
