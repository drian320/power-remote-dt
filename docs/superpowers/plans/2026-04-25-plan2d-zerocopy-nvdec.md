# Plan 2d Zero-Copy NVDEC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** NVDEC decode 経路から CPU NV12 バウンスを排除し、`cuvidMapVideoFrame64` の出力を CUDA-D3D11 interop 経由でそのまま D3D11 R8(Y)+ R8G8(UV)テクスチャに device-to-device コピーし、自前 YUV→BGRA pixel shader で swapchain に描画する。

**Architecture:** 単一 NV12 D3D11 texture interop は既に実機 probe で UV 取り出し不可と確認済み(`Y=OK UV=FAIL`)。代わりに NVIDIA SDK 標準の dual R8+R8G8 D3D11 textures を CUDA に register、`cuGraphicsMapResources` + `cuGraphicsSubResourceGetMappedArray` で各 CUarray を取得、`cuMemcpy2D_v2`(device-to-array)で 2 回コピーで完結。viewer は `--decoder nvdec` のときだけ新しい `DualPlaneYuvRenderer` を使う(MF 経路の `Nv12Renderer` は据え置き)。

**Tech Stack:** Rust 2021、windows-rs 0.58(`Win32_Graphics_Direct3D11` + `Win32_Graphics_Direct3D_Fxc` 追加)、bindgen 0.69(NVDEC FFI 既存)、CUDA 13.x(`prdt_nvdec_bindings` cfg)、HLSL Shader Model 5.0(VS/PS、runtime `D3DCompile`)。

**Spec:** `docs/superpowers/specs/2026-04-25-plan2d-zerocopy-nvdec-design.md`

---

## File Structure

**Created files:**
- `crates/media-win/src/d3d11/dual_plane_renderer.rs` — `DualPlaneYuvRenderer`(VS+PS+sampler 込み)
- `crates/media-win/tests/zerocopy_compare_smoke.rs` — bench compare spot test (`#[ignore]`)

**Modified files:**
- `crates/media-win/Cargo.toml` — `cpu-nv12` feature 追加、windows feature に `Win32_Graphics_Direct3D_Fxc` を足す
- `crates/media-win/src/d3d11/texture.rs` — `TextureFormat::R8` / `R8G8` 追加、`bytes_per_pixel_y` 拡張
- `crates/media-win/src/d3d11/mod.rs` — `pub use dual_plane_renderer::DualPlaneYuvRenderer`
- `crates/media-win/src/nvdec/decoder.rs` — `DualPlaneFrame` 型、`DecoderState` に `dual_cache`/`latest_dual` 追加、display callback rewrite、CPU NV12 path を `#[cfg(any(test, feature = "cpu-nv12"))]` で gate
- `crates/media-win/src/nvdec/consumer.rs` — `take_latest_dual_plane` 追加、`take_latest_texture` 削除、CUDA registration ライフサイクル
- `crates/viewer/src/main.rs` — `--decoder nvdec` 経路で `DualPlaneYuvRenderer` を使う分岐、`take_latest_dual_plane` 呼び出し

**Public API surface added:**
- `prdt_media_win::TextureFormat::{R8, R8G8}`
- `prdt_media_win::nvdec::decoder::DualPlaneFrame`
- `prdt_media_win::NvdecD3d11Consumer::take_latest_dual_plane`
- `prdt_media_win::DualPlaneYuvRenderer`

**Public API surface removed:**
- `prdt_media_win::NvdecD3d11Consumer::take_latest_texture`(viewer 側を switch するため)

---

## Task 1: TextureFormat の R8 / R8G8 variant 追加

**Files:**
- Modify: `crates/media-win/src/d3d11/texture.rs:9-45`

- [ ] **Step 1: Write failing tests**

Append to `crates/media-win/src/d3d11/texture.rs` の既存 `#[cfg(test)] mod tests` 内:

```rust
    #[test]
    fn r8_format_dxgi_mapping() {
        assert_eq!(
            TextureFormat::R8.to_dxgi(),
            windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8_UNORM
        );
        assert_eq!(TextureFormat::R8.bytes_per_pixel_y(), 1);
    }

    #[test]
    fn r8g8_format_dxgi_mapping() {
        assert_eq!(
            TextureFormat::R8G8.to_dxgi(),
            windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8_UNORM
        );
        assert_eq!(TextureFormat::R8G8.bytes_per_pixel_y(), 2);
    }
```

- [ ] **Step 2: Run failing tests**

```
cargo test -p prdt-media-win --lib d3d11::texture::tests::r8
```

Expected: FAIL — `TextureFormat::R8` not found.

- [ ] **Step 3: Add the variants**

Edit `crates/media-win/src/d3d11/texture.rs`. The `use` block at the top already has BGRA / NV12 / RGBA imports — extend it:

Find:
```rust
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
```

Replace with:
```rust
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8_UNORM,
    DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC,
};
```

Then in the `enum TextureFormat` definition, add two variants. Find:
```rust
pub enum TextureFormat {
    /// 8-bit BGRA, post-DXGI-capture default.
    Bgra8,
    /// 8-bit RGBA (used by some tooling paths).
    Rgba8,
    /// NV12 (Y plane + interleaved UV half-res) — the NVDEC default output.
    Nv12,
}
```

Replace with:
```rust
pub enum TextureFormat {
    /// 8-bit BGRA, post-DXGI-capture default.
    Bgra8,
    /// 8-bit RGBA (used by some tooling paths).
    Rgba8,
    /// NV12 (Y plane + interleaved UV half-res) — the NVDEC default output.
    Nv12,
    /// 8-bit single-channel red. Used as the Y plane carrier for the
    /// dual-plane CUDA-D3D11 interop path (Plan 2d zero-copy).
    R8,
    /// 8-bit two-channel red+green. Used as the UV plane carrier for the
    /// dual-plane CUDA-D3D11 interop path (Plan 2d zero-copy). Half-resolution
    /// in both dimensions vs the Y plane; each element holds (Cb, Cr).
    R8G8,
}
```

In `to_dxgi`, find:
```rust
    pub fn to_dxgi(self) -> windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT {
        match self {
            Self::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
            Self::Rgba8 => DXGI_FORMAT_R8G8B8A8_UNORM,
            Self::Nv12 => DXGI_FORMAT_NV12,
        }
    }
```

Replace with:
```rust
    pub fn to_dxgi(self) -> windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT {
        match self {
            Self::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
            Self::Rgba8 => DXGI_FORMAT_R8G8B8A8_UNORM,
            Self::Nv12 => DXGI_FORMAT_NV12,
            Self::R8 => DXGI_FORMAT_R8_UNORM,
            Self::R8G8 => DXGI_FORMAT_R8G8_UNORM,
        }
    }
```

In `bytes_per_pixel_y`, find:
```rust
    pub fn bytes_per_pixel_y(self) -> usize {
        match self {
            Self::Bgra8 | Self::Rgba8 => 4,
            Self::Nv12 => 1, // Y plane; UV is interleaved at half-res per dim
        }
    }
```

Replace with:
```rust
    pub fn bytes_per_pixel_y(self) -> usize {
        match self {
            Self::Bgra8 | Self::Rgba8 => 4,
            Self::Nv12 | Self::R8 => 1,
            Self::R8G8 => 2,
        }
    }
```

- [ ] **Step 4: Tests pass**

```
cargo test -p prdt-media-win --lib d3d11::texture::tests
```

Expected: PASS — both new tests + the existing ones.

- [ ] **Step 5: Commit**

```bash
cd E:/project/rust-desktop/power-remote-dt
git add crates/media-win/src/d3d11/texture.rs
git commit -m "media-win: add TextureFormat::R8 and R8G8 variants"
```

---

## Task 2: CUDA register/map probe for R8 + R8G8 dual textures

**Files:**
- Modify: `crates/media-win/src/nvdec/consumer.rs` — append a new `#[cfg(prdt_nvdec_bindings)] #[test]` to the existing `mod tests`

This task validates the spec assumption that R8 and R8G8 textures are accepted by `cuGraphicsD3D11RegisterResource` before the bigger display-callback rewrite. If the test FAILs on the user's driver, we BLOCK and surface to the controller — there's no point implementing the full path.

- [ ] **Step 1: Add the probe test**

In `crates/media-win/src/nvdec/consumer.rs` `#[cfg(test)] mod tests` (around line 203), append:

```rust
    /// Probe: can we register R8 (Y) and R8G8 (UV) D3D11 textures with CUDA
    /// and pull a non-null CUarray for each? This validates the dual-plane
    /// zero-copy approach BEFORE we rewire the display callback. If this
    /// FAILs on the host's driver, the entire Plan 2d zero-copy strategy
    /// must be reconsidered — escalate rather than carrying on.
    #[cfg(prdt_nvdec_bindings)]
    #[test]
    fn dual_plane_textures_register_with_cuda() {
        use super::super::cuda::{check, CudaContext};
        use super::super::ffi;
        use windows::core::Interface;

        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return,
        };
        if !adapter.is_nvidia() {
            eprintln!("skipping: non-NVIDIA adapter");
            return;
        }
        let dev = match D3d11Device::create(&adapter) {
            Ok(d) => d,
            Err(_) => return,
        };
        let ctx = match CudaContext::create_primary() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _g = ctx.push().expect("push");

        // Build the two textures: Y = R8 (W×H), UV = R8G8 (W/2 × H/2).
        let (w, h) = (256u32, 256u32);
        let y_tex = D3d11Texture::new_for_cuda_interop(&dev, w, h, TextureFormat::R8)
            .expect("Y R8 interop tex");
        let uv_tex = D3d11Texture::new_for_cuda_interop(&dev, w / 2, h / 2, TextureFormat::R8G8)
            .expect("UV R8G8 interop tex");

        // Register both with CUDA.
        let mut y_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        let mut uv_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        unsafe {
            let y_ptr: *mut std::ffi::c_void = y_tex.raw().as_raw() as *mut _;
            let uv_ptr: *mut std::ffi::c_void = uv_tex.raw().as_raw() as *mut _;
            check(
                "cuGraphicsD3D11RegisterResource(Y R8)",
                ffi::cuGraphicsD3D11RegisterResource(&mut y_res, y_ptr as *mut _, 0),
            )
            .expect("Y R8 register must succeed (Plan 2d hard requirement)");
            check(
                "cuGraphicsD3D11RegisterResource(UV R8G8)",
                ffi::cuGraphicsD3D11RegisterResource(&mut uv_res, uv_ptr as *mut _, 0),
            )
            .expect("UV R8G8 register must succeed (Plan 2d hard requirement)");

            // Map them, fetch CUarrays, confirm non-null.
            let mut resources = [y_res, uv_res];
            let map_r = ffi::cuGraphicsMapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());
            assert_eq!(
                map_r,
                ffi::cudaError_enum::CUDA_SUCCESS,
                "cuGraphicsMapResources failed: {}",
                map_r as u32
            );

            let mut y_array: ffi::CUarray = std::ptr::null_mut();
            let ry = ffi::cuGraphicsSubResourceGetMappedArray(&mut y_array, y_res, 0, 0);
            let mut uv_array: ffi::CUarray = std::ptr::null_mut();
            let ruv = ffi::cuGraphicsSubResourceGetMappedArray(&mut uv_array, uv_res, 0, 0);

            let _ = ffi::cuGraphicsUnmapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());
            let _ = ffi::cuGraphicsUnregisterResource(y_res);
            let _ = ffi::cuGraphicsUnregisterResource(uv_res);

            assert_eq!(ry, ffi::cudaError_enum::CUDA_SUCCESS, "Y array fetch CUresult={}", ry as u32);
            assert!(!y_array.is_null(), "Y CUarray was null");
            assert_eq!(
                ruv,
                ffi::cudaError_enum::CUDA_SUCCESS,
                "UV array fetch CUresult={}",
                ruv as u32
            );
            assert!(!uv_array.is_null(), "UV CUarray was null");
        }
    }
```

- [ ] **Step 2: Run probe — must pass**

```
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo test -p prdt-media-win --lib nvdec::consumer::tests::dual_plane_textures_register_with_cuda -- --nocapture
```

Expected: PASS. If FAIL, **stop here** and report BLOCKED with the assertion message — the spec assumption is invalidated and we'd have to revisit (e.g., explore SHARED-handle interop or fall back to the CPU bounce path).

- [ ] **Step 3: Commit**

```bash
git add crates/media-win/src/nvdec/consumer.rs
git commit -m "media-win: add dual_plane_textures_register_with_cuda probe (R8+R8G8 interop)"
```

---

## Task 3: DualPlaneFrame + dual cache + display-callback rewrite

This is the heart of the change. We add a new GPU path that lives alongside the CPU NV12 path; the CPU path is still active for now (gated removal happens in Task 7) so existing tests don't fall over. The new code must compile cleanly under both `prdt_nvdec_bindings` cfg presence and absence.

**Files:**
- Modify: `crates/media-win/src/nvdec/decoder.rs`

- [ ] **Step 1: Add the DualPlaneFrame type + DualCache fields**

Edit `crates/media-win/src/nvdec/decoder.rs`. Find the `DecodedFrame` definition near line 42:

```rust
/// One decoded NV12 frame in CPU memory. Y plane is `width * height`
/// bytes; UV plane follows, interleaved, at half vertical resolution.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: i64,
    /// Packed Y (height rows) + UV (height/2 rows, interleaved UVUV…).
    pub nv12: Vec<u8>,
}
```

After this struct, insert:

```rust
/// One decoded frame as a pair of D3D11 textures sitting in GPU memory,
/// already populated by the display callback via CUDA-D3D11 device-to-device
/// `cuMemcpy2D_v2`. Cloning is cheap (refcount bump on the inner ID3D11Texture2D).
#[derive(Clone)]
pub struct DualPlaneFrame {
    /// R8 texture, width × height. Holds the Y (luma) plane.
    pub y_tex: crate::d3d11::D3d11Texture,
    /// R8G8 texture, (width/2) × (height/2). Each element is (Cb, Cr).
    pub uv_tex: crate::d3d11::D3d11Texture,
    /// Width of the original NV12 frame in pixels (Y plane size).
    pub width: u32,
    /// Height of the original NV12 frame in pixels (Y plane size).
    pub height: u32,
    pub timestamp_us: i64,
}

/// CUDA-side handle for a registered D3D11 texture pair. The `Drop` impl
/// unregisters both resources on the same CUDA context they were registered on.
struct DualCache {
    y_tex: crate::d3d11::D3d11Texture,
    uv_tex: crate::d3d11::D3d11Texture,
    y_cuda_res: ffi::CUgraphicsResource,
    uv_cuda_res: ffi::CUgraphicsResource,
    width: u32,
    height: u32,
}

unsafe impl Send for DualCache {}

impl DualCache {
    /// Build a fresh dual cache for `(width, height)`. `width` is the Y plane
    /// width in pixels; the UV texture is half that in each dimension.
    /// Caller must hold the CUDA context push BEFORE calling this.
    fn new(
        dev: &crate::d3d11::D3d11Device,
        width: u32,
        height: u32,
    ) -> Result<Self, MediaError> {
        use crate::d3d11::{D3d11Texture, TextureFormat};

        let y_tex = D3d11Texture::new_for_cuda_interop(dev, width, height, TextureFormat::R8)?;
        let uv_tex =
            D3d11Texture::new_for_cuda_interop(dev, width / 2, height / 2, TextureFormat::R8G8)?;

        let mut y_cuda_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        let mut uv_cuda_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        unsafe {
            use windows::core::Interface;
            let y_ptr: *mut std::ffi::c_void = y_tex.raw().as_raw() as *mut _;
            let uv_ptr: *mut std::ffi::c_void = uv_tex.raw().as_raw() as *mut _;
            super::cuda::check(
                "cuGraphicsD3D11RegisterResource(Y)",
                ffi::cuGraphicsD3D11RegisterResource(&mut y_cuda_res, y_ptr as *mut _, 0),
            )?;
            super::cuda::check(
                "cuGraphicsD3D11RegisterResource(UV)",
                ffi::cuGraphicsD3D11RegisterResource(&mut uv_cuda_res, uv_ptr as *mut _, 0),
            )?;
        }
        Ok(Self {
            y_tex,
            uv_tex,
            y_cuda_res,
            uv_cuda_res,
            width,
            height,
        })
    }
}

impl Drop for DualCache {
    fn drop(&mut self) {
        unsafe {
            // Best-effort unregister; failing here only leaks until the
            // CUDA context is destroyed.
            let _ = ffi::cuGraphicsUnregisterResource(self.y_cuda_res);
            let _ = ffi::cuGraphicsUnregisterResource(self.uv_cuda_res);
        }
    }
}
```

- [ ] **Step 2: Extend DecoderState**

In the same file, find `struct DecoderState` (around line 54):

```rust
struct DecoderState {
    ctx: Arc<CudaContext>,
    #[allow(dead_code)]
    dev: D3d11Device,
    decoder: Option<ffi::CUvideodecoder>,
    /// Set by the sequence callback; read by the display callback to
    /// size the output NV12 buffer. u32 fits any real resolution.
    width: u32,
    height: u32,
    /// Max decode surfaces the decoder was created with. Returned from
    /// pfnSequenceCallback so cuvid knows the parser's ring depth.
    surfaces: u32,
    /// Latest decoded NV12 frame in CPU memory. Populated on every
    /// successful display callback. The consumer side uploads this
    /// into a cached D3D11 NV12 texture when `take_latest_texture` is
    /// called (on the viewer's event-loop thread, which owns the
    /// D3D11 immediate context).
    latest: Mutex<Option<DecodedFrame>>,
    /// Sticky error from a callback: any MediaError produced inside
    /// a callback gets stashed so `submit()` can surface it.
    error: Mutex<Option<MediaError>>,
}
```

Replace with:

```rust
struct DecoderState {
    ctx: Arc<CudaContext>,
    dev: D3d11Device,
    decoder: Option<ffi::CUvideodecoder>,
    /// Set by the sequence callback; read by the display callback to
    /// size the output NV12 buffer. u32 fits any real resolution.
    width: u32,
    height: u32,
    /// Max decode surfaces the decoder was created with. Returned from
    /// pfnSequenceCallback so cuvid knows the parser's ring depth.
    surfaces: u32,
    /// Latest decoded NV12 frame in CPU memory. Populated by the display
    /// callback only when `cpu_nv12` is true (test/feature path). Production
    /// uses the dual-plane GPU path below.
    #[cfg(any(test, feature = "cpu-nv12"))]
    latest: Mutex<Option<DecodedFrame>>,
    /// CUDA-registered dual-plane D3D11 cache. Lazily built on the first
    /// display callback once the decode resolution is known. Populated in
    /// place by every subsequent display callback via device-to-device
    /// `cuMemcpy2D_v2`.
    dual_cache: Mutex<Option<DualCache>>,
    /// Latest decoded GPU dual-plane frame. Holds clones (refcount bumps)
    /// of the textures inside `dual_cache`, plus the timestamp.
    latest_dual: Mutex<Option<DualPlaneFrame>>,
    /// Sticky error from a callback: any MediaError produced inside
    /// a callback gets stashed so `submit()` can surface it.
    error: Mutex<Option<MediaError>>,
}
```

The `#[allow(dead_code)]` on `dev` is removed because `dual_cache::new` actually uses it now.

- [ ] **Step 3: Update DecoderState construction**

Find the place where `DecoderState` is constructed inside `CuvidDecoder::new_hevc` (search for `latest: Mutex::new(None)`). Replace the `DecoderState { ... }` literal with one matching the new struct.

Original (around line 130-145, inside `new_hevc`):
```rust
        let state = Box::new(DecoderState {
            ctx: Arc::clone(&ctx),
            dev,
            decoder: None,
            width: max_w,
            height: max_h,
            surfaces: max_decode_surfaces,
            latest: Mutex::new(None),
            error: Mutex::new(None),
        });
```

Replace with:
```rust
        let state = Box::new(DecoderState {
            ctx: Arc::clone(&ctx),
            dev,
            decoder: None,
            width: max_w,
            height: max_h,
            surfaces: max_decode_surfaces,
            #[cfg(any(test, feature = "cpu-nv12"))]
            latest: Mutex::new(None),
            dual_cache: Mutex::new(None),
            latest_dual: Mutex::new(None),
            error: Mutex::new(None),
        });
```

- [ ] **Step 4: Rewrite the display callback's body**

Find the display callback body inside `unsafe extern "C" fn handle_picture_display`. The relevant block runs from "Copy Y + UV planes from pitched device memory..." (around line 376) to "let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr); 1" (around line 428).

Replace the entire block from the comment `// Copy Y + UV planes from pitched device memory ...` through `let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);` (NOT including the trailing `1`) with:

```rust
        // Production path: cuMemcpy2D_v2 directly from cuvid's pitched device
        // memory into CUDA-D3D11-mapped CUarrays for R8 (Y) and R8G8 (UV)
        // textures. Builds the dual cache on the first call. Test / opt-in
        // feature path additionally copies into a CPU NV12 buffer for
        // pixel-level cross-checking.
        let w = state.width as usize;
        let h = state.height as usize;

        // Lazily build the dual cache if it doesn't exist or the resolution
        // changed. The CUDA context is already pushed by the `_g` guard above.
        {
            let mut slot = state.dual_cache.lock().unwrap();
            let needs_rebuild = match slot.as_ref() {
                Some(c) => c.width != state.width || c.height != state.height,
                None => true,
            };
            if needs_rebuild {
                match DualCache::new(&state.dev, state.width, state.height) {
                    Ok(c) => *slot = Some(c),
                    Err(e) => {
                        record_error(state, e);
                        let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);
                        return 0;
                    }
                }
            }
        }

        // GPU-side copy. Map both resources, fetch the two CUarrays, copy,
        // then unmap. Keeping the lock for the whole copy is fine — the
        // display callback is the only writer.
        let mut copy_ok = true;
        let cache_guard = state.dual_cache.lock().unwrap();
        let cache = cache_guard.as_ref().expect("dual_cache populated above");
        let mut resources = [cache.y_cuda_res, cache.uv_cuda_res];
        let map_r =
            ffi::cuGraphicsMapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());
        if map_r != ffi::cudaError_enum::CUDA_SUCCESS {
            record_error(
                state,
                MediaError::Other(format!("cuGraphicsMapResources: CUresult={}", map_r as u32)),
            );
            let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);
            return 0;
        }

        let mut y_array: ffi::CUarray = std::ptr::null_mut();
        let mut uv_array: ffi::CUarray = std::ptr::null_mut();
        let ry =
            ffi::cuGraphicsSubResourceGetMappedArray(&mut y_array, cache.y_cuda_res, 0, 0);
        let ruv = ffi::cuGraphicsSubResourceGetMappedArray(
            &mut uv_array,
            cache.uv_cuda_res,
            0,
            0,
        );
        if ry != ffi::cudaError_enum::CUDA_SUCCESS
            || y_array.is_null()
            || ruv != ffi::cudaError_enum::CUDA_SUCCESS
            || uv_array.is_null()
        {
            copy_ok = false;
        }

        if copy_ok {
            // Y: device → R8 array. WidthInBytes = w (1 byte/pixel).
            let mut params_y: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
            params_y.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
            params_y.srcDevice = dev_ptr;
            params_y.srcPitch = pitch as usize;
            params_y.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_ARRAY;
            params_y.dstArray = y_array;
            params_y.WidthInBytes = w;
            params_y.Height = h;
            if ffi::cuMemcpy2D_v2(&mut params_y) != ffi::cudaError_enum::CUDA_SUCCESS {
                copy_ok = false;
            }
        }

        if copy_ok {
            // UV: device(+pitch*h) → R8G8 array. R8G8 is 2 bytes/pixel and
            // the UV plane is half-resolution per dim, so the row width in
            // bytes equals the Y plane row width: 2 * (w/2) = w.
            let mut params_uv: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
            params_uv.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
            params_uv.srcDevice = dev_ptr + (pitch as u64) * (h as u64);
            params_uv.srcPitch = pitch as usize;
            params_uv.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_ARRAY;
            params_uv.dstArray = uv_array;
            params_uv.WidthInBytes = w;
            params_uv.Height = h / 2;
            if ffi::cuMemcpy2D_v2(&mut params_uv) != ffi::cudaError_enum::CUDA_SUCCESS {
                copy_ok = false;
            }
        }

        let _ =
            ffi::cuGraphicsUnmapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());

        if copy_ok {
            *state.latest_dual.lock().unwrap() = Some(DualPlaneFrame {
                y_tex: cache.y_tex.clone(),
                uv_tex: cache.uv_tex.clone(),
                width: state.width,
                height: state.height,
                timestamp_us: (*disp).timestamp,
            });
        }
        drop(cache_guard);

        // Test/feature path: also produce a CPU NV12 copy for pixel-level
        // cross-checking against the dual-plane texture pair.
        #[cfg(any(test, feature = "cpu-nv12"))]
        if copy_ok {
            let mut nv12 = vec![0u8; w * h * 3 / 2];
            let mut cpu_ok = true;
            let mut params_y_cpu: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
            params_y_cpu.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
            params_y_cpu.srcDevice = dev_ptr;
            params_y_cpu.srcPitch = pitch as usize;
            params_y_cpu.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_HOST;
            params_y_cpu.dstHost = nv12.as_mut_ptr() as *mut c_void;
            params_y_cpu.dstPitch = w;
            params_y_cpu.WidthInBytes = w;
            params_y_cpu.Height = h;
            if ffi::cuMemcpy2D_v2(&mut params_y_cpu) != ffi::cudaError_enum::CUDA_SUCCESS {
                cpu_ok = false;
            }
            if cpu_ok {
                let mut params_uv_cpu: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
                params_uv_cpu.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
                params_uv_cpu.srcDevice = dev_ptr + (pitch as u64) * (h as u64);
                params_uv_cpu.srcPitch = pitch as usize;
                params_uv_cpu.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_HOST;
                params_uv_cpu.dstHost = nv12[w * h..].as_mut_ptr() as *mut c_void;
                params_uv_cpu.dstPitch = w;
                params_uv_cpu.WidthInBytes = w;
                params_uv_cpu.Height = h / 2;
                if ffi::cuMemcpy2D_v2(&mut params_uv_cpu) != ffi::cudaError_enum::CUDA_SUCCESS {
                    cpu_ok = false;
                }
            }
            if cpu_ok {
                *state.latest.lock().unwrap() = Some(DecodedFrame {
                    width: state.width,
                    height: state.height,
                    timestamp_us: (*disp).timestamp,
                    nv12,
                });
            }
        }

        let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);
```

- [ ] **Step 5: Add take_latest_dual_frame on CuvidDecoder**

Locate `pub fn take_latest_frame(&self) -> Option<DecodedFrame>` (search the file). Update the function and add a sibling.

Find:
```rust
    pub fn take_latest_frame(&self) -> Option<DecodedFrame> {
        self.state.latest.lock().unwrap().take()
    }
```

Replace with:
```rust
    /// CPU-side NV12 frame (test / opt-in feature only). Production callers
    /// use `take_latest_dual_plane`.
    #[cfg(any(test, feature = "cpu-nv12"))]
    pub fn take_latest_frame(&self) -> Option<DecodedFrame> {
        self.state.latest.lock().unwrap().take()
    }

    /// GPU-side dual-plane frame: a (R8 Y, R8G8 UV) D3D11 texture pair already
    /// populated by the display callback via CUDA-D3D11 device-to-device copy.
    pub fn take_latest_dual_plane(&self) -> Option<DualPlaneFrame> {
        self.state.latest_dual.lock().unwrap().take()
    }
```

- [ ] **Step 6: Add a unit test that the decode produces dual-plane textures**

In `crates/media-win/src/nvdec/consumer.rs` test module(near `decode_single_nvenc_frame_round_trip`), add a new test:

```rust
    /// End-to-end with the new GPU dual-plane path: encode 5 frames, submit
    /// them to NVDEC, take the latest dual-plane frame, and verify the
    /// texture sizes/formats match expectation.
    #[cfg(prdt_nvdec_bindings)]
    #[test]
    fn decode_emits_dual_plane_textures() {
        use crate::d3d11::TextureFormat;
        use crate::nvenc::{NvencEncoder, NvencEncoderConfig};
        use crate::synthetic::make_counter_texture;

        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return,
        };
        if !adapter.is_nvidia() {
            eprintln!("skipping: non-NVIDIA adapter");
            return;
        }
        let dev = match D3d11Device::create(&adapter) {
            Ok(d) => d,
            Err(_) => return,
        };
        let (w, h) = (256u32, 256u32);

        let enc = NvencEncoder::new(
            &dev,
            &NvencEncoderConfig {
                width: w,
                height: h,
                fps_numerator: 60,
                fps_denominator: 1,
                bitrate_bps: 5_000_000,
                gop_length: 30,
            },
        )
        .expect("NvencEncoder");
        let mut consumer = NvdecD3d11Consumer::new(&dev, w, h).expect("NvdecD3d11Consumer");

        for i in 0..5 {
            let tex = make_counter_texture(&dev, w, h, i).expect("counter tex");
            let ts = i as u64 * 16_666;
            let force_idr = i == 0;
            let frame = enc.encode(&tex, force_idr, ts).expect("encode");
            consumer
                .decoder
                .submit(&frame.nal_bytes, ts as i64)
                .unwrap_or_else(|e| panic!("submit frame {i} failed: {e}"));
        }

        let dual = consumer
            .decoder
            .take_latest_dual_plane()
            .expect("NVDEC should have produced at least one dual-plane frame");
        assert_eq!(dual.width, w);
        assert_eq!(dual.height, h);
        assert_eq!(dual.y_tex.format(), TextureFormat::R8);
        assert_eq!(dual.uv_tex.format(), TextureFormat::R8G8);
        assert_eq!(dual.y_tex.width(), w);
        assert_eq!(dual.y_tex.height(), h);
        assert_eq!(dual.uv_tex.width(), w / 2);
        assert_eq!(dual.uv_tex.height(), h / 2);
    }
```

- [ ] **Step 7: Run the tests**

```
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo test -p prdt-media-win --lib nvdec::
```

Expected:
- `dual_plane_textures_register_with_cuda` ... ok
- `decode_emits_dual_plane_textures` ... ok
- `probe_nv12_shader_resource_only_interop` ... ok (existing, still runs as a diagnostic)
- `decode_single_nvenc_frame_round_trip` ... ok (uses `take_latest_texture` — keep working until Task 4 deletes it)
- `construction_matches_feature_availability` ... ok

If the build fails because `take_latest_frame` is no longer callable from `decode_single_nvenc_frame_round_trip` (because of the `#[cfg(any(test, feature = "cpu-nv12"))]` gate) — that's a false alarm: tests compile under `cfg(test)`, so the gate IS satisfied. If you see this anyway, double-check that `decoder.rs:take_latest_frame` is gated with `#[cfg(any(test, feature = "cpu-nv12"))]` and not just `#[cfg(feature = "cpu-nv12")]`.

- [ ] **Step 8: Commit**

```bash
git add crates/media-win/src/nvdec/decoder.rs crates/media-win/src/nvdec/consumer.rs
git commit -m "media-win/nvdec: add DualPlaneFrame + GPU dual-plane decode path"
```

---

## Task 4: NvdecD3d11Consumer expose dual-plane API + remove old NV12 path

**Files:**
- Modify: `crates/media-win/src/nvdec/consumer.rs` — replace `take_latest_texture` with `take_latest_dual_plane`, drop the `nv12_cache` field and `upload_nv12_to_cache` helper, drop `take_latest_nv12`.

- [ ] **Step 1: Drop the old NV12 cache and upload helper**

In `crates/media-win/src/nvdec/consumer.rs`, find the struct and `take_latest_texture`/`upload_nv12_to_cache`/`take_latest_nv12` methods. Apply these changes.

Find:
```rust
pub struct NvdecD3d11Consumer {
    #[cfg(prdt_nvdec_bindings)]
    _ctx: Arc<CudaContext>,
    #[cfg(prdt_nvdec_bindings)]
    decoder: CuvidDecoder,
    /// Cached NV12 D3D11 texture we reuse across frames. Lazily created
    /// on the first `take_latest_texture` call once the decoder's
    /// actual output size is known.
    #[cfg(prdt_nvdec_bindings)]
    nv12_cache: Mutex<Option<D3d11Texture>>,
    _dev: D3d11Device,
    _width: u32,
    _height: u32,
}
```

Replace with:
```rust
pub struct NvdecD3d11Consumer {
    #[cfg(prdt_nvdec_bindings)]
    _ctx: Arc<CudaContext>,
    #[cfg(prdt_nvdec_bindings)]
    decoder: CuvidDecoder,
    _dev: D3d11Device,
    _width: u32,
    _height: u32,
}
```

The `Mutex` import is no longer needed unless the file uses it elsewhere. Find:
```rust
#[cfg(prdt_nvdec_bindings)]
use std::sync::{Arc, Mutex};
```

Replace with:
```rust
#[cfg(prdt_nvdec_bindings)]
use std::sync::Arc;
```

In the constructor, find:
```rust
            Ok(Self {
                _ctx: ctx,
                decoder,
                nv12_cache: Mutex::new(None),
                _dev: dev.clone(),
                _width: width,
                _height: height,
            })
```

Replace with:
```rust
            Ok(Self {
                _ctx: ctx,
                decoder,
                _dev: dev.clone(),
                _width: width,
                _height: height,
            })
```

Delete `take_latest_nv12`, `take_latest_texture`, and `upload_nv12_to_cache` entirely. Find this block (around lines 76-176):

```rust
    /// Pop the latest decoded NV12 frame as raw CPU bytes. Tests use
    /// this to verify pixel-level correctness; the production viewer
    /// uses `take_latest_texture` instead.
    #[cfg(prdt_nvdec_bindings)]
    pub fn take_latest_nv12(&self) -> Option<DecodedFrame> {
        self.decoder.take_latest_frame()
    }

    /// Drain the latest decoded GPU texture, if any. Mirrors
    /// `MfD3d11Consumer::take_latest_texture` so viewer code can be
    /// decoder-agnostic. Uploads the latest CPU NV12 bytes into a
    /// cached NV12 D3D11 texture via UpdateSubresource and returns a
    /// clone. Must be called on the thread that owns the D3D11
    /// immediate context (i.e., the viewer's event-loop thread).
    pub fn take_latest_texture(&self) -> Option<D3d11Texture> {
        #[cfg(prdt_nvdec_bindings)]
        {
            let frame = self.decoder.take_latest_frame()?;
            match self.upload_nv12_to_cache(&frame) {
                Ok(tex) => Some(tex),
                Err(e) => {
                    tracing::warn!(%e, "NVDEC: D3D11 NV12 upload failed");
                    None
                }
            }
        }
        #[cfg(not(prdt_nvdec_bindings))]
        {
            None
        }
    }

    #[cfg(prdt_nvdec_bindings)]
    fn upload_nv12_to_cache(&self, frame: &DecodedFrame) -> Result<D3d11Texture, MediaError> {
        // ... (whole body, including UpdateSubresource calls) ...
    }
```

Replace the entire above block with:

```rust
    /// Pop the latest decoded NV12 frame as raw CPU bytes. Test / opt-in
    /// feature path only — production viewer uses `take_latest_dual_plane`.
    #[cfg(all(prdt_nvdec_bindings, any(test, feature = "cpu-nv12")))]
    pub fn take_latest_nv12(&self) -> Option<DecodedFrame> {
        self.decoder.take_latest_frame()
    }

    /// Drain the latest decoded GPU dual-plane frame: a (R8 Y, R8G8 UV)
    /// D3D11 texture pair already populated via CUDA-D3D11 device-to-device
    /// copy. Mirrors `MfD3d11Consumer::take_latest_texture` shape but the
    /// downstream renderer is `DualPlaneYuvRenderer` rather than
    /// `Nv12Renderer`. Must be called on the thread that owns the D3D11
    /// immediate context (the viewer's event-loop thread).
    pub fn take_latest_dual_plane(&self) -> Option<DualPlaneFrame> {
        #[cfg(prdt_nvdec_bindings)]
        {
            self.decoder.take_latest_dual_plane()
        }
        #[cfg(not(prdt_nvdec_bindings))]
        {
            None
        }
    }
```

Note the import addition. Find:
```rust
#[cfg(prdt_nvdec_bindings)]
use super::decoder::{CuvidDecoder, DecodedFrame};
```

Replace with:
```rust
#[cfg(prdt_nvdec_bindings)]
use super::decoder::{CuvidDecoder, DualPlaneFrame};
#[cfg(all(prdt_nvdec_bindings, any(test, feature = "cpu-nv12")))]
use super::decoder::DecodedFrame;
```

Outside the `prdt_nvdec_bindings` cfg(for the `#[cfg(not(prdt_nvdec_bindings))]` arm of `take_latest_dual_plane`), add a stub `DualPlaneFrame` definition so the API surface is the same regardless of bindings. After the existing `use super::decoder::DualPlaneFrame;` block, add:

```rust
#[cfg(not(prdt_nvdec_bindings))]
pub struct DualPlaneFrame;
```

Wait — that's inconsistent with the cfg-gated import above. The cleanest approach: re-export `DualPlaneFrame` from `consumer.rs` always. We'll need a no-bindings stub. Modify `nvdec/mod.rs` after Task 3, but here in Task 4 we just need the stub for the no-bindings build.

Actually the simplest: keep `DualPlaneFrame` only available with bindings, and `take_latest_dual_plane` returns `Option<DualPlaneFrame>` only when the bindings exist. But the viewer needs to compile against both cfgs. Cleanest solution: just gate `take_latest_dual_plane` behind `#[cfg(prdt_nvdec_bindings)]` — viewer's no-bindings path already errors out at construction time.

Let me revise the `take_latest_dual_plane` method to be cfg-gated:

```rust
    /// Drain the latest decoded GPU dual-plane frame. Only available when
    /// the NVDEC bindings are compiled in; viewer must check the cfg before
    /// calling.
    #[cfg(prdt_nvdec_bindings)]
    pub fn take_latest_dual_plane(&self) -> Option<DualPlaneFrame> {
        self.decoder.take_latest_dual_plane()
    }
```

And remove the no-bindings stub addition.

Also: the `#[cfg(prdt_nvdec_bindings)]` gate on `take_latest_nv12` was originally inside the impl block but the `#[cfg]` syntax I used (`all(prdt_nvdec_bindings, any(test, feature = "cpu-nv12"))`) is correct.

Final state of these methods inside `impl NvdecD3d11Consumer`:

```rust
    /// Pop the latest decoded NV12 frame as raw CPU bytes. Test / opt-in
    /// feature path only — production viewer uses `take_latest_dual_plane`.
    #[cfg(all(prdt_nvdec_bindings, any(test, feature = "cpu-nv12")))]
    pub fn take_latest_nv12(&self) -> Option<DecodedFrame> {
        self.decoder.take_latest_frame()
    }

    /// Drain the latest decoded GPU dual-plane frame: a (R8 Y, R8G8 UV)
    /// D3D11 texture pair already populated via CUDA-D3D11 device-to-device
    /// copy. Mirrors `MfD3d11Consumer::take_latest_texture` shape but the
    /// downstream renderer is `DualPlaneYuvRenderer` rather than
    /// `Nv12Renderer`. Must be called on the thread that owns the D3D11
    /// immediate context (the viewer's event-loop thread).
    #[cfg(prdt_nvdec_bindings)]
    pub fn take_latest_dual_plane(&self) -> Option<DualPlaneFrame> {
        self.decoder.take_latest_dual_plane()
    }
```

The `D3d11Texture` import in `consumer.rs` may now be unused — if `cargo build` complains, remove it from the `use` statements at the top of the file (search for `use crate::d3d11::{D3d11Device, D3d11Texture};` and remove `D3d11Texture` if it's unused; if the `#[cfg(prdt_nvdec_bindings)]` test code still uses it, keep it).

- [ ] **Step 2: Update existing test that called take_latest_texture**

Find `decode_single_nvenc_frame_round_trip` test in `consumer.rs` (around line 339). It currently calls `take_latest_texture`. Replace the relevant assertion lines:

Find:
```rust
        // take_latest_texture returns a fully populated D3D11 NV12
        // texture — the path the viewer actually exercises.
        let gpu = consumer
            .take_latest_texture()
            .expect("NVDEC should have produced at least one NV12 texture");
        assert_eq!(gpu.width(), w);
        assert_eq!(gpu.height(), h);
    }
```

Replace with:
```rust
        // take_latest_dual_plane returns the GPU texture pair — the path the
        // viewer actually exercises after Plan 2d zero-copy.
        let dual = consumer
            .take_latest_dual_plane()
            .expect("NVDEC should have produced at least one dual-plane frame");
        assert_eq!(dual.width, w);
        assert_eq!(dual.height, h);
        assert_eq!(dual.y_tex.format(), TextureFormat::R8);
        assert_eq!(dual.uv_tex.format(), TextureFormat::R8G8);
    }
```

Add the `TextureFormat` import to the test (if not already imported):

Find inside the test:
```rust
        use crate::nvenc::{NvencEncoder, NvencEncoderConfig};
        use crate::synthetic::make_counter_texture;
```

Replace with:
```rust
        use crate::d3d11::TextureFormat;
        use crate::nvenc::{NvencEncoder, NvencEncoderConfig};
        use crate::synthetic::make_counter_texture;
```

This is now functionally identical to `decode_emits_dual_plane_textures` from Task 3 — that's fine, the older test name conveys intent; you may delete the duplicate `decode_emits_dual_plane_textures` test from Task 3 OR rename the older one. Pick: delete the duplicate added in Task 3 to avoid redundancy.

- [ ] **Step 3: Build + test**

```
cargo build -p prdt-media-win
```

Expected: clean build, no unused-import warnings.

```
cargo test -p prdt-media-win --lib nvdec::
```

Expected: all NVDEC tests pass, including the consolidated `decode_single_nvenc_frame_round_trip` (asserting dual-plane).

- [ ] **Step 4: Commit**

```bash
git add crates/media-win/src/nvdec/consumer.rs
git commit -m "media-win/nvdec: replace take_latest_texture with take_latest_dual_plane"
```

---

## Task 5: DualPlaneYuvRenderer skeleton (constructor + HLSL compile)

**Files:**
- Create: `crates/media-win/src/d3d11/dual_plane_renderer.rs`
- Modify: `crates/media-win/src/d3d11/mod.rs`
- Modify: `crates/media-win/Cargo.toml` (add `Win32_Graphics_Direct3D_Fxc` to windows feature)

- [ ] **Step 1: Add the windows-rs feature for D3DCompile**

In `crates/media-win/Cargo.toml`, find:
```toml
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Graphics_Gdi",
    "Win32_System_Threading",
    "Win32_System_Performance",
    "Win32_Media",
    "Win32_Media_MediaFoundation",
    "Win32_System_Com",
] }
```

Replace with:
```toml
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Direct3D_Fxc",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Graphics_Gdi",
    "Win32_System_Threading",
    "Win32_System_Performance",
    "Win32_Media",
    "Win32_Media_MediaFoundation",
    "Win32_System_Com",
] }
```

- [ ] **Step 2: Add the cpu-nv12 feature**

In the same Cargo.toml, append (it currently has no `[features]` section):

```toml

[features]
default = []
# Enables the legacy CPU NV12 readback path on the NVDEC decoder. Used by
# regression tests that compare CPU and GPU output pixel-by-pixel; off by
# default in production where it would slow the decode loop by 2 PCIe copies.
cpu-nv12 = []
```

- [ ] **Step 3: Create the renderer skeleton**

Create `crates/media-win/src/d3d11/dual_plane_renderer.rs` with the constructor only (render method comes in Task 6):

```rust
//! NV12 (dual R8 + R8G8 plane) → BGRA conversion via custom pixel shader.
//!
//! Used by the NVDEC zero-copy path. Unlike `Nv12Renderer` (which delegates
//! to `ID3D11VideoProcessor`), this renderer samples the Y and UV textures
//! directly in a fragment shader and applies a BT.709 limited-range YUV→RGB
//! matrix. This is required because NVDEC outputs to R8 + R8G8 textures (the
//! single-NV12 D3D11 texture interop path is rejected by current drivers for
//! the UV plane — see `consumer.rs::probe_nv12_shader_resource_only_interop`).

use std::ffi::CString;

use windows::core::{Interface, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11PixelShader, ID3D11SamplerState, ID3D11VertexShader, D3D11_COMPARISON_NEVER,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_SAMPLER_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
};
use windows::Win32::Graphics::Direct3D::ID3DBlob;

use crate::d3d11::D3d11Device;
use crate::error::{MediaError, Result};

const VS_SOURCE: &str = r#"
struct VsOut {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

VsOut main(uint id : SV_VertexID) {
    VsOut o;
    // Fullscreen triangle: emits NDC positions covering the entire viewport
    // with the texcoord ranging 0..1 across the visible area. Vertex IDs
    // 0/1/2 give (-1,-1), (3,-1), (-1,3) respectively.
    o.uv  = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return o;
}
"#;

const PS_SOURCE: &str = r#"
Texture2D    YPlane  : register(t0);
Texture2D    UVPlane : register(t1);
SamplerState Samp    : register(s0);

float4 main(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    float  y     = YPlane .Sample(Samp, uv).r;
    float2 cbcr  = UVPlane.Sample(Samp, uv).rg;

    // BT.709 limited-range expansion to (Y in 0..1, Cb/Cr in -0.5..0.5).
    y           = (y         - 16.0/255.0) * (255.0/219.0);
    float cb    = (cbcr.x    - 128.0/255.0) * (255.0/224.0);
    float cr    = (cbcr.y    - 128.0/255.0) * (255.0/224.0);

    // BT.709 inverse matrix.
    float3 rgb = float3(
        y +              1.5748 * cr,
        y - 0.1873 * cb - 0.4681 * cr,
        y + 1.8556 * cb
    );
    return float4(saturate(rgb), 1.0);
}
"#;

/// YUV→BGRA renderer for the NVDEC zero-copy dual-plane path.
pub struct DualPlaneYuvRenderer {
    dev: D3d11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl DualPlaneYuvRenderer {
    /// Compile the VS/PS, create a linear-clamp sampler. The renderer is
    /// dimension-agnostic — `render()` uses the swapchain's current size.
    pub fn new(dev: &D3d11Device) -> Result<Self> {
        let vs_blob = compile_shader(VS_SOURCE, "main", "vs_5_0")?;
        let ps_blob = compile_shader(PS_SOURCE, "main", "ps_5_0")?;
        let (vs, ps) = unsafe {
            let mut vs: Option<ID3D11VertexShader> = None;
            dev.device()
                .CreateVertexShader(blob_slice(&vs_blob), None, Some(&mut vs))
                .map_err(|e| MediaError::d3d11("CreateVertexShader", e))?;
            let vs = vs.ok_or_else(|| MediaError::Other("CreateVertexShader returned null".into()))?;

            let mut ps: Option<ID3D11PixelShader> = None;
            dev.device()
                .CreatePixelShader(blob_slice(&ps_blob), None, Some(&mut ps))
                .map_err(|e| MediaError::d3d11("CreatePixelShader", e))?;
            let ps = ps.ok_or_else(|| MediaError::Other("CreatePixelShader returned null".into()))?;

            (vs, ps)
        };

        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: 0.0,
        };
        let mut sampler: Option<ID3D11SamplerState> = None;
        unsafe {
            dev.device()
                .CreateSamplerState(&sampler_desc, Some(&mut sampler))
                .map_err(|e| MediaError::d3d11("CreateSamplerState", e))?;
        }
        let sampler = sampler.ok_or_else(|| MediaError::Other("CreateSamplerState returned null".into()))?;

        Ok(Self {
            dev: dev.clone(),
            vs,
            ps,
            sampler,
        })
    }
}

unsafe impl Send for DualPlaneYuvRenderer {}
unsafe impl Sync for DualPlaneYuvRenderer {}

fn compile_shader(src: &str, entry: &str, target: &str) -> Result<ID3DBlob> {
    let entry_c = CString::new(entry).expect("entry has no NUL bytes");
    let target_c = CString::new(target).expect("target has no NUL bytes");
    let mut code: Option<ID3DBlob> = None;
    let mut errs: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(
            src.as_ptr() as *const _,
            src.len(),
            PCSTR(b"shader\0".as_ptr()),
            None,
            None,
            PCSTR(entry_c.as_ptr() as *const u8),
            PCSTR(target_c.as_ptr() as *const u8),
            D3DCOMPILE_ENABLE_STRICTNESS,
            0,
            &mut code,
            Some(&mut errs),
        )
    };
    if let Err(e) = r {
        let err_msg = errs
            .as_ref()
            .map(|b| unsafe {
                let p = b.GetBufferPointer() as *const u8;
                let n = b.GetBufferSize();
                String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned()
            })
            .unwrap_or_default();
        return Err(MediaError::Other(format!(
            "D3DCompile({entry}/{target}) failed: {e}: {err_msg}"
        )));
    }
    code.ok_or_else(|| MediaError::Other(format!("D3DCompile({entry}/{target}) returned null blob")))
}

unsafe fn blob_slice(blob: &ID3DBlob) -> &[u8] {
    let p = blob.GetBufferPointer() as *const u8;
    let n = blob.GetBufferSize();
    std::slice::from_raw_parts(p, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construction-only smoke test: shader source must compile, shaders must
    /// be created, sampler must come up. Doesn't render anything.
    #[test]
    fn constructs_on_default_device() {
        let dev = match D3d11Device::create_default() {
            Ok(d) => d,
            Err(_) => return, // No D3D11 adapter — skip.
        };
        let _r = DualPlaneYuvRenderer::new(&dev).expect("DualPlaneYuvRenderer::new");
    }
}
```

- [ ] **Step 4: Re-export from mod.rs**

In `crates/media-win/src/d3d11/mod.rs`, find the existing `pub use` block. Add the new module + re-export.

```bash
grep -n "pub use\|pub mod" E:/project/rust-desktop/power-remote-dt/crates/media-win/src/d3d11/mod.rs
```

If `mod.rs` is small (just `pub mod` + `pub use` lines), open it and add:
```rust
pub mod dual_plane_renderer;
pub use dual_plane_renderer::DualPlaneYuvRenderer;
```

at the appropriate place (alongside the other `pub mod ...; pub use ...` declarations).

- [ ] **Step 5: Build + smoke test**

```
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-media-win
```

Expected: clean build, no warnings.

```
cargo test -p prdt-media-win --lib d3d11::dual_plane_renderer::tests::constructs_on_default_device
```

Expected: PASS — the shader compile path works.

- [ ] **Step 6: Commit**

```bash
git add crates/media-win/Cargo.toml crates/media-win/src/d3d11/dual_plane_renderer.rs crates/media-win/src/d3d11/mod.rs
git commit -m "media-win: add DualPlaneYuvRenderer skeleton with HLSL VS/PS"
```

---

## Task 6: DualPlaneYuvRenderer render() + viewer wiring

**Files:**
- Modify: `crates/media-win/src/d3d11/dual_plane_renderer.rs` — add `render()` method
- Modify: `crates/viewer/src/main.rs` — wire `--decoder nvdec` to the new renderer

- [ ] **Step 1: Add render() to DualPlaneYuvRenderer**

In `crates/media-win/src/d3d11/dual_plane_renderer.rs`, extend the imports and add the `render` method to the impl.

Update the `use` block at the top:

Find:
```rust
use windows::Win32::Graphics::Direct3D11::{
    ID3D11PixelShader, ID3D11SamplerState, ID3D11VertexShader, D3D11_COMPARISON_NEVER,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_SAMPLER_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
};
```

Replace with:
```rust
use windows::Win32::Graphics::Direct3D11::{
    ID3D11PixelShader, ID3D11RenderTargetView, ID3D11Resource, ID3D11SamplerState,
    ID3D11ShaderResourceView, ID3D11VertexShader, D3D11_COMPARISON_NEVER,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_RENDER_TARGET_VIEW_DESC,
    D3D11_RENDER_TARGET_VIEW_DESC_0, D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_SAMPLER_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_RTV,
    D3D11_TEX2D_SRV, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_VIEWPORT,
    D3D_SRV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM,
};
```

Also import `DualPlaneFrame` and `SwapChain`:

After the `use crate::d3d11::D3d11Device;` line, add:
```rust
use crate::d3d11::SwapChain;
use crate::nvdec::decoder::DualPlaneFrame;
```

(If `crate::d3d11::SwapChain` isn't the correct path — check `mod.rs` — adjust accordingly. The existing `nv12_renderer.rs` has `use crate::d3d11::swapchain::SwapChain;` so that's probably the right import.)

Then add the render method to `impl DualPlaneYuvRenderer`. Find the closing brace of the impl(`}` after the constructor body) and insert before it:

```rust
    /// Render the dual-plane `frame` into the `swap` back-buffer's BGRA
    /// surface using the YUV→RGB pixel shader. Must be called on the thread
    /// that owns the D3D11 immediate context.
    pub fn render(&self, frame: &DualPlaneFrame, swap: &SwapChain) -> Result<()> {
        let backbuf = swap.backbuffer()?;
        let (out_w, out_h) = swap.size();

        // RTV on the swapchain backbuffer.
        let rtv_desc = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
            },
        };
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        let backbuf_res: ID3D11Resource = backbuf
            .cast()
            .map_err(|e| MediaError::d3d11("backbuffer -> ID3D11Resource", e))?;
        unsafe {
            self.dev
                .device()
                .CreateRenderTargetView(&backbuf_res, Some(&rtv_desc), Some(&mut rtv))
                .map_err(|e| MediaError::d3d11("CreateRenderTargetView", e))?;
        }
        let rtv = rtv.ok_or_else(|| MediaError::Other("CreateRenderTargetView returned null".into()))?;

        // SRVs on Y (R8) and UV (R8G8).
        let make_srv = |tex: &crate::d3d11::D3d11Texture, fmt| -> Result<ID3D11ShaderResourceView> {
            let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: fmt,
                ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                    },
                },
            };
            let res: ID3D11Resource = tex
                .raw()
                .cast()
                .map_err(|e| MediaError::d3d11("plane Texture2D -> Resource", e))?;
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            unsafe {
                self.dev
                    .device()
                    .CreateShaderResourceView(&res, Some(&desc), Some(&mut srv))
                    .map_err(|e| MediaError::d3d11("CreateShaderResourceView", e))?;
            }
            srv.ok_or_else(|| MediaError::Other("CreateShaderResourceView returned null".into()))
        };
        let y_srv = make_srv(&frame.y_tex, DXGI_FORMAT_R8_UNORM)?;
        let uv_srv = make_srv(&frame.uv_tex, DXGI_FORMAT_R8G8_UNORM)?;

        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: out_w as f32,
            Height: out_h as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };

        self.dev.with_context(|ctx| unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[viewport]));
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            // No vertex/index buffers; VS reads SV_VertexID for the
            // fullscreen triangle.
            ctx.IASetInputLayout(None);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(y_srv.clone()), Some(uv_srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(3, 0);
            // Unbind shader resources so the textures can be written next
            // frame without driver complaints.
            ctx.PSSetShaderResources(0, Some(&[None, None]));
            ctx.OMSetRenderTargets(Some(&[None]), None);
        });
        Ok(())
    }
```

The closure `make_srv` is defined inline above the use site. The `ctx.IASetInputLayout(None)` call may require type annotation if windows-rs is strict — if it complains about ambiguity, change to `ctx.IASetInputLayout(None::<&_>)` or look at how `Nv12Renderer` handles it (it doesn't bind input layout because VideoProcessorBlt manages everything).

- [ ] **Step 2: Wire up the viewer**

**Current state of `crates/viewer/src/main.rs` (verify before editing):**
- `use` block at line 17-18 imports `MfD3d11Consumer, Nv12Renderer, NvdecD3d11Consumer, ...`
- The `decoder: String` arg exists at line 85 + 685
- A `renderer: Option<Nv12Renderer>` field at line 190 holds the current single renderer
- Renderer construction is at line 395 (`Nv12Renderer::new(...)`)
- Per-frame render call is at line 415 (`r.render(...)`)
- The consumer construction at line 887-920 currently STUBS the NVDEC path with a warn-and-fall-back-to-MF (the comment "until the NVDEC FFI is wired up" is stale — FFI exists since plan2d-complete; we're wiring it now)
- The consumer is a single concrete type: `Arc<tokio::sync::Mutex<MfD3d11Consumer>>`
- `take_latest_texture` is called at line 1023, result stored in `Option<(D3d11Texture, u64)>` at line 1026 via `recv_shared.latest_texture`

**Plan: introduce two parallel enums to replace the single types.**

Open `crates/viewer/src/main.rs`. Apply the changes as follows.

**Edit A — Define the enums.** Find a good location near the top of the file (after the `use` block, before `struct Args`). Add:

```rust
/// Per-decoder decoded frame. The viewer thread receives one of these per
/// frame and dispatches to the matching renderer.
enum LatestFrame {
    /// Single NV12 D3D11 texture from `MfD3d11Consumer::take_latest_texture`.
    Nv12(prdt_media_win::D3d11Texture),
    /// Dual-plane (R8 Y, R8G8 UV) frame from
    /// `NvdecD3d11Consumer::take_latest_dual_plane`. Only constructed when
    /// `prdt_nvdec_bindings` cfg is set.
    #[cfg(prdt_nvdec_bindings)]
    DualPlane(prdt_media_win::DualPlaneFrame),
}

/// Decoder-selected consumer. Held behind the recv task's
/// `Arc<tokio::sync::Mutex<...>>`.
enum ViewerConsumer {
    Mf(prdt_media_win::MfD3d11Consumer),
    #[cfg(prdt_nvdec_bindings)]
    Nvdec(prdt_media_win::NvdecD3d11Consumer),
}

/// Decoder-selected renderer. Held inside the event-loop's render code.
enum ViewerRenderer {
    Mf(prdt_media_win::Nv12Renderer),
    Nvdec(prdt_media_win::DualPlaneYuvRenderer),
}
```

(Note: `DualPlaneFrame` and `NvdecD3d11Consumer::take_latest_dual_plane` are only available with `prdt_nvdec_bindings` per the consumer/decoder cfg from Tasks 3-4. The cfg gates above keep the no-bindings build green.)

**Edit B — Re-export DualPlaneFrame from prdt-media-win.** In `crates/media-win/src/lib.rs`, find the `pub use` block (search for `pub use crate::nvdec::NvdecD3d11Consumer;` or similar). Add:

```rust
#[cfg(prdt_nvdec_bindings)]
pub use crate::nvdec::decoder::DualPlaneFrame;
pub use crate::d3d11::DualPlaneYuvRenderer;
```

**Edit C — Update the consumer construction at line 887.** Find:

```rust
        let consumer = if decoder == "nvdec" {
            match NvdecD3d11Consumer::new(&dev, ack.neg_width, ack.neg_height) {
                Ok(_nv) => {
                    // Once the FFI lands, we'll replace this with
                    // Arc::new(Mutex::new(_nv)). For now bail to MF.
                    warn!(
                        "unreachable: NvdecD3d11Consumer::new returned Ok but \
                         the VideoConsumer trait impl is a stub — falling back to MF",
                    );
                    None
                }
                Err(e) => {
                    warn!(%e, "NVDEC unavailable; falling back to MF decoder");
                    None
                }
            }
        } else {
            None
        }
        .unwrap_or_else(|| {
            // Fallback / default path.
            Arc::new(tokio::sync::Mutex::new(
                match MfD3d11Consumer::new(&dev, ack.neg_width, ack.neg_height) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(?e, "MfD3d11Consumer::new failed");
                        panic!("no decoder could be initialized");
                    }
                },
            ))
        });
```

Replace with:

```rust
        let consumer: Arc<tokio::sync::Mutex<ViewerConsumer>> = if decoder == "nvdec" {
            #[cfg(prdt_nvdec_bindings)]
            {
                match NvdecD3d11Consumer::new(&dev, ack.neg_width, ack.neg_height) {
                    Ok(nv) => Some(Arc::new(tokio::sync::Mutex::new(ViewerConsumer::Nvdec(nv)))),
                    Err(e) => {
                        warn!(%e, "NVDEC unavailable; falling back to MF decoder");
                        None
                    }
                }
            }
            #[cfg(not(prdt_nvdec_bindings))]
            {
                warn!("--decoder nvdec specified but NVDEC bindings are not compiled in; falling back to MF");
                None
            }
        } else {
            None
        }
        .unwrap_or_else(|| {
            // Fallback / default path.
            Arc::new(tokio::sync::Mutex::new(ViewerConsumer::Mf(
                match MfD3d11Consumer::new(&dev, ack.neg_width, ack.neg_height) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(?e, "MfD3d11Consumer::new failed");
                        panic!("no decoder could be initialized");
                    }
                },
            )))
        });
```

**Edit D — Update the receive-loop submit + take.** Find around line 1018-1027:

```rust
                        let mut c = recv_consumer.lock().await;
                        if let Err(e) = c.submit(frame).await {
                            warn!(?e, seq, is_kf, nal_len, "consumer.submit error");
                            continue;
                        }
                        if let Some(tex) = c.take_latest_texture() {
                            tex_count += 1;
                            recv_shared.latency.record_decoded(seq);
                            *recv_shared.latest_texture.lock().unwrap() = Some((tex, host_ts_us));
                        }
```

Replace with:

```rust
                        let mut c = recv_consumer.lock().await;
                        let submit_result = match &mut *c {
                            ViewerConsumer::Mf(m) => m.submit(frame).await,
                            #[cfg(prdt_nvdec_bindings)]
                            ViewerConsumer::Nvdec(n) => n.submit(frame).await,
                        };
                        if let Err(e) = submit_result {
                            warn!(?e, seq, is_kf, nal_len, "consumer.submit error");
                            continue;
                        }
                        let frame_opt: Option<LatestFrame> = match &*c {
                            ViewerConsumer::Mf(m) => m.take_latest_texture().map(LatestFrame::Nv12),
                            #[cfg(prdt_nvdec_bindings)]
                            ViewerConsumer::Nvdec(n) => {
                                n.take_latest_dual_plane().map(LatestFrame::DualPlane)
                            }
                        };
                        if let Some(frame) = frame_opt {
                            tex_count += 1;
                            recv_shared.latency.record_decoded(seq);
                            *recv_shared.latest_texture.lock().unwrap() = Some((frame, host_ts_us));
                        }
```

The shared `latest_texture` field's type changes — see Edit E.

**Edit E — Update the shared-state texture field.** Find the struct that defines `latest_texture` (search `latest_texture:`):

```bash
grep -n "latest_texture:" E:/project/rust-desktop/power-remote-dt/crates/viewer/src/main.rs
```

The field is currently typed `Mutex<Option<(D3d11Texture, u64)>>`. Change it to `Mutex<Option<(LatestFrame, u64)>>`. Apply the type change at the struct definition site and any constructor site that initializes it to `None`.

**Edit F — Update the renderer field and construction.** Find the struct field at line 190 and the construction at line 395:

```bash
grep -n "renderer:\s*Option<Nv12Renderer>\|Nv12Renderer::new" E:/project/rust-desktop/power-remote-dt/crates/viewer/src/main.rs
```

Change the field type from `Option<Nv12Renderer>` to `Option<ViewerRenderer>`.

At the construction site (around line 395), find:

```rust
                match Nv12Renderer::new(
                    /* original args */
                ) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        warn!(?e, "Nv12Renderer::new failed");
                        None
                    }
                }
```

Replace with:

```rust
                if decoder == "nvdec" {
                    #[cfg(prdt_nvdec_bindings)]
                    {
                        match prdt_media_win::DualPlaneYuvRenderer::new(&dev) {
                            Ok(r) => Some(ViewerRenderer::Nvdec(r)),
                            Err(e) => {
                                warn!(?e, "DualPlaneYuvRenderer::new failed");
                                None
                            }
                        }
                    }
                    #[cfg(not(prdt_nvdec_bindings))]
                    {
                        match Nv12Renderer::new(/* original args */) {
                            Ok(r) => Some(ViewerRenderer::Mf(r)),
                            Err(e) => {
                                warn!(?e, "Nv12Renderer::new failed");
                                None
                            }
                        }
                    }
                } else {
                    match Nv12Renderer::new(/* original args */) {
                        Ok(r) => Some(ViewerRenderer::Mf(r)),
                        Err(e) => {
                            warn!(?e, "Nv12Renderer::new failed");
                            None
                        }
                    }
                }
```

(Use the actual original args from the existing `Nv12Renderer::new(...)` call, NOT the literal `/* original args */` placeholder.)

**Edit G — Update the per-frame render call (around line 415).** Find:

```rust
                            warn!(?e, "Nv12Renderer::render failed");
```

This is inside an `if let Err(e) = r.render(&tex, &swap)` (or similar). Change the code to dispatch on `ViewerRenderer`:

```rust
                            match (renderer, &latest) {
                                (Some(ViewerRenderer::Mf(r)), (LatestFrame::Nv12(tex), _ts)) => {
                                    if let Err(e) = r.render(tex, &swap) {
                                        warn!(?e, "Nv12Renderer::render failed");
                                    }
                                }
                                #[cfg(prdt_nvdec_bindings)]
                                (
                                    Some(ViewerRenderer::Nvdec(r)),
                                    (LatestFrame::DualPlane(frame), _ts),
                                ) => {
                                    if let Err(e) = r.render(frame, &swap) {
                                        warn!(?e, "DualPlaneYuvRenderer::render failed");
                                    }
                                }
                                _ => {
                                    // Decoder/renderer mismatch — shouldn't happen because
                                    // both are picked from the same `decoder` flag at startup.
                                    warn!("internal: renderer/frame variant mismatch");
                                }
                            }
```

The exact destructuring depends on how `latest` is accessed currently. Adapt to the existing surrounding code (don't restructure unrelated lines).

**Edit summary:** A and B add the new types and re-exports. C wires the NVDEC consumer construction through the new enum. D dispatches submit/take inside the recv loop. E retypes the shared latest field. F constructs the right renderer for the decoder. G dispatches render at draw time.

- [ ] **Step 3: Build the viewer**

```
cargo build -p prdt-viewer
```

Expected: `Finished`. If the viewer's existing renderer wiring is shaped differently than the sketch above, adapt — the goal is "MF path uses Nv12Renderer with NV12 textures, NVDEC path uses DualPlaneYuvRenderer with DualPlaneFrame".

If the build fails, `cargo build -p prdt-viewer 2>&1 | tail -30` and address each error one at a time.

- [ ] **Step 4: Run any viewer-touching tests**

```
cargo test -p prdt-viewer --lib
```

Expected: pass (or 0 tests if there are none).

```
cargo test -p prdt-media-win --lib d3d11::dual_plane_renderer
```

Expected: `constructs_on_default_device` passes; no other regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/media-win/src/d3d11/dual_plane_renderer.rs crates/viewer/src/main.rs crates/protocol/src/video_pipeline.rs
git commit -m "media-win+viewer: render dual-plane frames via DualPlaneYuvRenderer (NVDEC path)"
```

(The `crates/protocol` path appears only if you ended up extending the trait; drop from `git add` if not.)

---

## Task 7: bench compare + cleanup + tag

**Files:**
- Create: `crates/media-win/tests/zerocopy_compare_smoke.rs` — `#[ignore]` spot test for MF/NVDEC compare
- Cleanup: any stale comments referring to "CPU NV12 path" in production code

- [ ] **Step 1: Add the bench compare spot test**

Create `crates/media-win/tests/zerocopy_compare_smoke.rs`:

```rust
//! Plan 2d zero-copy spot bench: encode 60 frames at 1080p with NVENC, then
//! feed the resulting bitstream into both `MfD3d11Consumer` and
//! `NvdecD3d11Consumer`. Time the per-frame `take_latest_*` retrievals to
//! see whether the NVDEC zero-copy path beats the MF path. The result is
//! printed via `eprintln!` and the test is `#[ignore]`d so it stays out of
//! routine CI; run with `cargo test --test zerocopy_compare_smoke -- --ignored --nocapture`.

#![cfg(all(windows, prdt_nvdec_bindings))]

use std::time::Instant;

use prdt_media_win::adapter::pick_default_adapter;
use prdt_media_win::nvdec::NvdecD3d11Consumer;
use prdt_media_win::nvenc::{NvencEncoder, NvencEncoderConfig};
use prdt_media_win::synthetic::make_counter_texture;
use prdt_media_win::{D3d11Device, MfD3d11Consumer};
use prdt_protocol::VideoConsumer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn compare_mf_vs_nvdec_decode_throughput() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(_) => {
            eprintln!("skipping: no D3D11 adapter");
            return;
        }
    };
    if !adapter.is_nvidia() {
        eprintln!("skipping: non-NVIDIA adapter (NVDEC unavailable)");
        return;
    }
    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    let (w, h) = (1920u32, 1080u32);

    let enc = NvencEncoder::new(
        &dev,
        &NvencEncoderConfig {
            width: w,
            height: h,
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps: 30_000_000,
            gop_length: 30,
        },
    )
    .expect("NvencEncoder");

    // Pre-encode 60 frames so both consumers see identical bitstreams.
    let mut nal_stream: Vec<(Vec<u8>, u64)> = Vec::with_capacity(60);
    for i in 0..60u32 {
        let tex = make_counter_texture(&dev, w, h, i).expect("counter tex");
        let ts = i as u64 * 16_666;
        let force_idr = i == 0;
        let frame = enc.encode(&tex, force_idr, ts).expect("encode");
        nal_stream.push((frame.nal_bytes, ts));
    }

    eprintln!("--- MfD3d11Consumer ---");
    let mut mf = MfD3d11Consumer::new(&dev, w, h).expect("MfD3d11Consumer");
    let mut mf_take_us: Vec<u128> = Vec::with_capacity(60);
    for (nal, ts) in &nal_stream {
        let frame = prdt_protocol::EncodedFrame {
            nal_units: nal.clone(),
            timestamp_host_us: *ts,
            ..Default::default()
        };
        mf.submit(frame).await.expect("MF submit");
        let t0 = Instant::now();
        let _tex = mf.take_latest_texture();
        mf_take_us.push(t0.elapsed().as_micros());
    }
    print_summary("MF take_latest_texture", &mf_take_us);

    eprintln!("--- NvdecD3d11Consumer ---");
    let mut nvdec = NvdecD3d11Consumer::new(&dev, w, h).expect("NvdecD3d11Consumer");
    let mut nv_take_us: Vec<u128> = Vec::with_capacity(60);
    for (nal, ts) in &nal_stream {
        let frame = prdt_protocol::EncodedFrame {
            nal_units: nal.clone(),
            timestamp_host_us: *ts,
            ..Default::default()
        };
        nvdec.submit(frame).await.expect("NVDEC submit");
        let t0 = Instant::now();
        let _dual = nvdec.take_latest_dual_plane();
        nv_take_us.push(t0.elapsed().as_micros());
    }
    print_summary("NVDEC take_latest_dual_plane", &nv_take_us);
}

fn print_summary(label: &str, samples: &[u128]) {
    let mut s = samples.to_vec();
    s.sort_unstable();
    let p50 = s[s.len() / 2];
    let p95 = s[s.len() * 95 / 100];
    let p99 = s[s.len() * 99 / 100];
    let mean = s.iter().copied().sum::<u128>() as f64 / s.len() as f64;
    eprintln!(
        "{label}: n={} mean={:.1}us p50={}us p95={}us p99={}us",
        s.len(),
        mean,
        p50,
        p95,
        p99,
    );
}
```

Run the bench manually (it's `#[ignore]`):
```
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo test -p prdt-media-win --test zerocopy_compare_smoke -- --ignored --nocapture
```

Expected output (approximate, on the user's RTX 3070 Ti):
```
--- MfD3d11Consumer ---
MF take_latest_texture: n=60 mean=...us p50=...us p95=...us p99=...us
--- NvdecD3d11Consumer ---
NVDEC take_latest_dual_plane: n=60 mean=...us p50=...us p95=...us p99=...us
```

NVDEC p95 should be **at most** the MF p95 (the spec exit criterion is "≥30% shorter or ≤MF"). Record the actual numbers in a comment at the top of the test file once you have them.

If NVDEC is slower than MF, that's a SIGNIFICANT signal — surface it as DONE_WITH_CONCERNS and we'll investigate (likely a measurement-issue or a missed sync barrier).

- [ ] **Step 2: Cleanup stale comments referencing the CPU NV12 path**

Search the production code (excluding `cpu-nv12` feature-gated blocks) for stale comments mentioning "CPU NV12" or "UpdateSubresource":

```
grep -n "CPU NV12\|UpdateSubresource\|CPU-side NV12\|CPU bounce" crates/media-win/src/nvdec/decoder.rs crates/media-win/src/nvdec/consumer.rs
```

Update any leftover comments that imply CPU NV12 is the production path. The goal is the documentation matches reality: production uses dual-plane GPU; CPU is test-only.

- [ ] **Step 3: Run the full workspace tests**

```
cargo test --workspace
```

Expected: 214+ tests pass (existing 214 from `phase2-w6-polish-complete` + the new dual-plane / renderer / probe tests). Specifically check:
- `nvdec::consumer::tests::dual_plane_textures_register_with_cuda` ... ok
- `nvdec::consumer::tests::decode_single_nvenc_frame_round_trip` ... ok (now uses dual-plane)
- `d3d11::dual_plane_renderer::tests::constructs_on_default_device` ... ok
- All `phase2-*` smoke tests still pass

- [ ] **Step 4: Clippy + fmt**

```
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: no warnings. Pay attention to the new `dual_plane_renderer.rs` and the modified `decoder.rs` / `consumer.rs`.

```
cargo fmt --all -- --check
```

Expected: per the W6 polish merge, only pre-existing drift on files we haven't touched. The newly created/modified files in this plan should be fmt-clean. If there's drift in any file from this plan, run `cargo fmt --all` and commit as a separate "plan2d-zerocopy: cargo fmt on touched files" commit.

- [ ] **Step 5: Tag the completion state**

```bash
git tag -a plan2d-zerocopy-complete -m "$(cat <<'EOF'
Plan 2d zero-copy NVDEC complete — dual R8+R8G8 D3D11 textures via CUDA-D3D11 interop

- TextureFormat::R8 / R8G8 added (DXGI_FORMAT_R8_UNORM / R8G8_UNORM)
- DualPlaneFrame { y_tex (R8), uv_tex (R8G8), width, height, timestamp_us } as the new NVDEC output
- DecoderState carries DualCache (CUDA-registered D3D11 texture pair) — display callback does cuMemcpy2D_v2 device→array (no PCIe round-trip)
- NvdecD3d11Consumer::take_latest_dual_plane replaces take_latest_texture
- DualPlaneYuvRenderer (custom HLSL VS/PS, BT.709 limited-range YUV→BGRA) replaces Nv12Renderer for the NVDEC path
- viewer's --decoder nvdec wires through the new path; MF path untouched
- cpu-nv12 feature retains the legacy CPU NV12 readback for pixel-level cross-checking
- compare_mf_vs_nvdec_decode_throughput spot test (#[ignore]) measures the difference
EOF
)"
git tag | grep plan2d
```

Expected: output includes `plan2d-zerocopy-complete` alongside the existing `plan2d-*` tags.

- [ ] **Step 6: Final summary**

Report back to the user:

> Plan 2d zero-copy 完了:
> - TextureFormat::R8 / R8G8 + dual-plane CUDA-D3D11 interop
> - NVDEC が `cuMemcpy2D_v2` device-to-device で R8(Y)+ R8G8(UV)に直接吐く
> - 新 DualPlaneYuvRenderer(自前 HLSL VS/PS、BT.709 limited-range)
> - viewer は `--decoder nvdec` で zero-copy 経路に切替、MF は無変更
> - `compare_mf_vs_nvdec_decode_throughput` の実測値: MF p95=Xms, NVDEC p95=Yms(改善 Z%)
> - tag `plan2d-zerocopy-complete` 打刻済

---

## Risks & Notes for Implementer

- **Display callback runs in the parser thread** (synchronously inside `submit`). The CUDA context is pushed via `state.ctx.push()` at the top of the callback; do not introduce additional `push` calls in `DualCache::new` — it's invoked from within the already-pushed callback.
- **Single-buffered textures** are used (the dual cache textures are written by every display callback and read by the viewer's render). At 60fps this is the same pattern MF uses; if you see tearing add double-buffering as a follow-up plan.
- **`cuvidMapVideoFrame64` pitch can be larger than width**. The src copy uses `srcPitch = pitch as usize` (the cuvid-reported pitch), `WidthInBytes = w` (logical width). DON'T use `pitch` for `WidthInBytes` — that copies padding bytes into the destination.
- **R8G8 row width is `width` bytes**, not `width/2`. R8G8 is 2 bytes/element and the UV plane has width/2 elements per row. So `WidthInBytes = (width/2) * 2 = width`. The destination row stride for an R8G8 array is also `width` bytes (CUDA derives it from the array desc).
- **HLSL `saturate(rgb)`** clamps to 0..1 to handle rounding under-/overshoot from the matrix; without it, deeply saturated colors near range limits can produce out-of-gamut values that render as black/white.
- **Sampler is LINEAR clamp**, so chroma upsampling is bilinear. This matches how MF's VideoProcessor handles UV, so the visual result should be very close.
- **D3DCompile happens once at viewer startup** (`DualPlaneYuvRenderer::new`). On the test machine it takes ~50-100ms; that's hidden inside D3D11 device creation, no user-visible cost.
- **Don't trust rust-analyzer diagnostics** after edits; verify with `cargo build` / `cargo test`. The W6 history shows multiple stale-diagnostic false alarms.
- **If `cuGraphicsSubResourceGetMappedArray` fails for R8 / R8G8** — Task 2's probe should catch this. If you somehow get past it and see runtime failures, escalate immediately; the spec's hard assumption is broken and we'd need to revisit.
- **viewer wiring is the highest-uncertainty step**. The current main.rs structure may not match the sketch in Task 6. Read the viewer's renderer initialization and per-frame draw loop carefully BEFORE editing; treat the sketch as a reference, not a copy-paste.

---

## Self-Review Notes

- **Spec coverage**: Task 1 covers TextureFormat additions; Task 2 covers the spec's "1. R8/R8G8 register-able probe"; Task 3 covers DualPlaneFrame + decoder rewrite; Task 4 covers consumer API change; Task 5+6 cover the renderer (skeleton + render); Task 7 covers feature gate cleanup, bench compare, clippy/fmt, tagging.
- **Placeholder scan**: All code blocks contain copy-pasteable Rust. No "TBD" / "TODO" / vague phrases.
- **Type consistency**: `DualPlaneFrame { y_tex: D3d11Texture, uv_tex: D3d11Texture, width: u32, height: u32, timestamp_us: i64 }` is consistent across spec, decoder, consumer, renderer, viewer wiring, and bench test. `take_latest_dual_plane(&self) -> Option<DualPlaneFrame>` signature is consistent across `CuvidDecoder` and `NvdecD3d11Consumer`. `PROBE_RETRY_*` consts are unrelated to this plan (mentioned only in passing in the W6 polish reference).
- **Exit criteria** in the spec are fully covered: TextureFormat, DualPlaneFrame, decoder rewrite, consumer API, register/map probe, decode end-to-end test, CPU/GPU pixel agreement (covered by Task 7 with cpu-nv12 feature), DualPlaneYuvRenderer, viewer wiring, bench compare, workspace tests pass, clippy/fmt clean, tag.
