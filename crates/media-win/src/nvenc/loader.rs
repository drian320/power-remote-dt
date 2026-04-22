//! Runtime loader for `nvEncodeAPI64.dll`. The NVIDIA docs mandate using
//! LoadLibrary + GetProcAddress rather than static linking (the DLL is
//! shipped with the NVIDIA display driver, not the SDK).

use std::sync::OnceLock;

use libloading::{Library, Symbol};

use crate::error::{MediaError, Result};

/// Wraps the `nvEncodeAPI64.dll` runtime and the `NvEncodeAPICreateInstance`
/// entry point.
pub struct NvEncLibrary {
    _lib: Library,
    create_instance: unsafe extern "C" fn(*mut std::ffi::c_void) -> u32,
    get_max_version: unsafe extern "C" fn(*mut u32) -> u32,
}

static LIB: OnceLock<std::result::Result<NvEncLibrary, String>> = OnceLock::new();

impl NvEncLibrary {
    /// Load `nvEncodeAPI64.dll`. The library is cached in a OnceLock so
    /// subsequent calls return the same handle. Returns error if the DLL
    /// is not present (e.g. running on a machine without an NVIDIA driver).
    pub fn load() -> Result<&'static NvEncLibrary> {
        let res = LIB.get_or_init(|| unsafe {
            let lib = Library::new("nvEncodeAPI64.dll")
                .map_err(|e| format!("LoadLibrary nvEncodeAPI64.dll: {e}"))?;

            // Fetch function pointers. We transmute through the owned Symbol
            // to extract the raw fn pointer, then let Symbol drop (the Library
            // itself keeps the DLL loaded).
            let create_sym: Symbol<unsafe extern "C" fn(*mut std::ffi::c_void) -> u32> = lib
                .get(b"NvEncodeAPICreateInstance\0")
                .map_err(|e| format!("GetProcAddress(NvEncodeAPICreateInstance): {e}"))?;
            let create_instance = *create_sym.into_raw();

            let ver_sym: Symbol<unsafe extern "C" fn(*mut u32) -> u32> = lib
                .get(b"NvEncodeAPIGetMaxSupportedVersion\0")
                .map_err(|e| format!("GetProcAddress(NvEncodeAPIGetMaxSupportedVersion): {e}"))?;
            let get_max_version = *ver_sym.into_raw();

            Ok(NvEncLibrary {
                _lib: lib,
                create_instance,
                get_max_version,
            })
        });

        match res {
            Ok(l) => Ok(l),
            Err(msg) => Err(MediaError::Other(msg.clone())),
        }
    }

    /// Call NvEncodeAPICreateInstance with the given function-table pointer.
    /// Returns the raw NVENCSTATUS code (0 == success).
    /// # Safety
    /// The caller must ensure `fn_table_ptr` points to a properly-sized and
    /// version-initialized NV_ENCODE_API_FUNCTION_LIST.
    pub unsafe fn create_instance(&self, fn_table_ptr: *mut std::ffi::c_void) -> u32 {
        (self.create_instance)(fn_table_ptr)
    }

    /// Read the maximum supported NVENC API version from the driver.
    pub fn get_max_supported_version(&self) -> Result<u32> {
        let mut version: u32 = 0;
        let status = unsafe { (self.get_max_version)(&mut version) };
        if status != 0 {
            return Err(MediaError::Other(format!(
                "NvEncodeAPIGetMaxSupportedVersion failed: status={status}"
            )));
        }
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_library() {
        // Best-effort: on a machine without NVIDIA driver this fails. On
        // this dev machine (RTX 3070 Ti / GTX 1080), load should succeed.
        match NvEncLibrary::load() {
            Ok(lib) => {
                let version = lib
                    .get_max_supported_version()
                    .expect("get_max_supported_version");
                eprintln!("NVENC max supported version = 0x{version:08x}");
                // NVENC API version encodes (major << 4 | minor). Version
                // is non-zero on any driver that supports NVENC.
                assert!(version > 0, "version should be non-zero");
            }
            Err(e) => {
                eprintln!("NVENC DLL not available (non-fatal in CI): {e}");
            }
        }
    }
}
