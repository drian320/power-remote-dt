//! Minimal RAII wrappers over the CUDA Driver API that Plan 2d's NVDEC
//! consumer needs. Scope is intentionally tight — just `cuInit`,
//! `cuDeviceGet`, `cuCtxCreate`, push/pop, and destroy. Anything fancier
//! (multi-device, streams, events) can land when we actually need it.

use super::ffi;
use crate::error::MediaError;

/// Check a CUDA `CUresult` and map errors into `MediaError::Other` with a
/// context tag, preserving the numeric code. bindgen omits Debug on the
/// auto-generated `cudaError_enum`, so we print the `as u32` discriminant
/// rather than relying on Debug.
pub(super) fn check(context: &'static str, r: ffi::CUresult) -> Result<(), MediaError> {
    if r == ffi::cudaError_enum::CUDA_SUCCESS {
        Ok(())
    } else {
        Err(MediaError::Other(format!(
            "CUDA {context}: CUresult={}",
            r as u32
        )))
    }
}

/// Owning handle over a `CUcontext`. Destroys the context on drop. Mutating
/// cuvid calls require the context to be current on the calling thread —
/// use [`CudaContext::push`] / [`CudaContext::pop`] for that.
pub struct CudaContext {
    raw: ffi::CUcontext,
}

// CUcontext handles are safely moveable between threads provided only one
// thread has the context current at a time; we enforce that via push/pop.
unsafe impl Send for CudaContext {}

static CU_INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();

fn ensure_cu_initialized() -> Result<(), MediaError> {
    let outcome = CU_INIT.get_or_init(|| unsafe {
        let r = ffi::cuInit(0);
        if r == ffi::cudaError_enum::CUDA_SUCCESS {
            Ok(())
        } else {
            Err(format!("cuInit failed: CUresult={}", r as u32))
        }
    });
    outcome.clone().map_err(MediaError::Other)
}

impl CudaContext {
    /// Create a primary-device CUDA context (ordinal 0). The returned
    /// context is NOT made current on the calling thread — `cuCtxCreate`
    /// does push it as a side effect, but we pop it immediately so callers
    /// get a clean handle. Use `push`/`pop` to activate.
    pub fn create_primary() -> Result<Self, MediaError> {
        ensure_cu_initialized()?;
        unsafe {
            let mut count = 0;
            check("cuDeviceGetCount", ffi::cuDeviceGetCount(&mut count))?;
            if count < 1 {
                return Err(MediaError::Other("no CUDA devices available".into()));
            }
            let mut device = 0;
            check("cuDeviceGet", ffi::cuDeviceGet(&mut device, 0))?;

            // cuCtxCreate_v4(pctx, ctxCreateParams, flags, dev).
            // Passing null for ctxCreateParams selects CUDA's defaults —
            // no exec affinity, no CUDA Graph Instantiation Global params.
            let mut ctx: ffi::CUcontext = std::ptr::null_mut();
            check(
                "cuCtxCreate_v4",
                ffi::cuCtxCreate_v4(
                    &mut ctx,
                    std::ptr::null_mut(), // ctxCreateParams
                    0,                    // flags (CU_CTX_SCHED_AUTO)
                    device,
                ),
            )?;
            // cuCtxCreate pushes the new context on the current thread;
            // pop it so we hand the caller a clean (non-current) context.
            let mut popped: ffi::CUcontext = std::ptr::null_mut();
            let _ = ffi::cuCtxPopCurrent_v2(&mut popped);
            Ok(Self { raw: ctx })
        }
    }

    pub fn raw(&self) -> ffi::CUcontext {
        self.raw
    }

    /// Push this context onto the calling thread's context stack. Every
    /// `push` must be matched by a `pop` (idiomatically inside a scope
    /// guard). The guard returned here does that automatically on drop.
    pub fn push(&self) -> Result<CudaCurrentGuard<'_>, MediaError> {
        unsafe {
            check("cuCtxPushCurrent_v2", ffi::cuCtxPushCurrent_v2(self.raw))?;
        }
        Ok(CudaCurrentGuard { _ctx: self })
    }
}

impl Drop for CudaContext {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                // Don't panic in Drop; log if destroy fails.
                let r = ffi::cuCtxDestroy_v2(self.raw);
                if r != ffi::cudaError_enum::CUDA_SUCCESS {
                    tracing::warn!(code = r as u32, "cuCtxDestroy_v2 failed");
                }
            }
        }
    }
}

/// RAII guard returned by `CudaContext::push`. Pops the context on drop.
pub struct CudaCurrentGuard<'a> {
    _ctx: &'a CudaContext,
}

impl Drop for CudaCurrentGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let mut popped: ffi::CUcontext = std::ptr::null_mut();
            let r = ffi::cuCtxPopCurrent_v2(&mut popped);
            if r != ffi::cudaError_enum::CUDA_SUCCESS {
                tracing::warn!(code = r as u32, "cuCtxPopCurrent_v2 failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_cu_initialized_is_idempotent() {
        // Must succeed on any box with the NVIDIA driver; the dev machine
        // has one. Skip gracefully if cuInit reports no devices (unlikely
        // in dev but possible on a headless CI VM).
        match ensure_cu_initialized() {
            Ok(()) => {
                // Call twice — the OnceLock path must not re-invoke cuInit.
                ensure_cu_initialized().expect("second call");
            }
            Err(e) => {
                eprintln!("cuInit unavailable (skipping test): {e}");
            }
        }
    }

    #[test]
    fn create_primary_context_round_trip() {
        let ctx = match CudaContext::create_primary() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("CUDA unavailable (skipping test): {e}");
                return;
            }
        };
        {
            let _guard = ctx.push().expect("push");
            unsafe {
                let mut current: ffi::CUcontext = std::ptr::null_mut();
                let r = ffi::cuCtxGetCurrent(&mut current);
                // cudaError_enum doesn't derive Debug (bindgen), so compare
                // via discriminant rather than assert_eq!.
                assert!(
                    r == ffi::cudaError_enum::CUDA_SUCCESS,
                    "cuCtxGetCurrent returned {}",
                    r as u32,
                );
                assert_eq!(current, ctx.raw(), "pushed context should be current");
            }
            // guard drop pops it
        }
        unsafe {
            let mut current: ffi::CUcontext = std::ptr::null_mut();
            let _ = ffi::cuCtxGetCurrent(&mut current);
            // after pop, either null or some other context — but not ours.
            assert_ne!(current, ctx.raw());
        }
    }
}
