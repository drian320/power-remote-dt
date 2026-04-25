//! NV12 (dual R8 + R8G8 plane) → BGRA conversion via custom pixel shader.
//!
//! Used by the NVDEC zero-copy path. Unlike `Nv12Renderer` (which delegates
//! to `ID3D11VideoProcessor`), this renderer samples the Y and UV textures
//! directly in a fragment shader and applies a BT.709 limited-range YUV→RGB
//! matrix. This is required because NVDEC outputs to R8 + R8G8 textures (the
//! single-NV12 D3D11 texture interop path is rejected by current drivers for
//! the UV plane — see `consumer.rs::probe_nv12_shader_resource_only_interop`).

use std::ffi::CString;

use windows::core::PCSTR;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11PixelShader, ID3D11SamplerState, ID3D11VertexShader, D3D11_COMPARISON_NEVER,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_SAMPLER_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
};

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
    #[allow(dead_code)] // used in Task 6 render()
    dev: D3d11Device,
    #[allow(dead_code)] // used in Task 6 render()
    vs: ID3D11VertexShader,
    #[allow(dead_code)] // used in Task 6 render()
    ps: ID3D11PixelShader,
    #[allow(dead_code)] // used in Task 6 render()
    sampler: ID3D11SamplerState,
}

impl DualPlaneYuvRenderer {
    /// Compile the VS/PS, create a linear-clamp sampler. The renderer is
    /// dimension-agnostic — `render()` (Task 6) uses the swapchain's size.
    pub fn new(dev: &D3d11Device) -> Result<Self> {
        let vs_blob = compile_shader(VS_SOURCE, "main", "vs_5_0")?;
        let ps_blob = compile_shader(PS_SOURCE, "main", "ps_5_0")?;
        let (vs, ps) = unsafe {
            let mut vs: Option<ID3D11VertexShader> = None;
            dev.device()
                .CreateVertexShader(blob_slice(&vs_blob), None, Some(&mut vs))
                .map_err(|e| MediaError::d3d11("CreateVertexShader", e))?;
            let vs =
                vs.ok_or_else(|| MediaError::Other("CreateVertexShader returned null".into()))?;

            let mut ps: Option<ID3D11PixelShader> = None;
            dev.device()
                .CreatePixelShader(blob_slice(&ps_blob), None, Some(&mut ps))
                .map_err(|e| MediaError::d3d11("CreatePixelShader", e))?;
            let ps =
                ps.ok_or_else(|| MediaError::Other("CreatePixelShader returned null".into()))?;

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
        let sampler = sampler
            .ok_or_else(|| MediaError::Other("CreateSamplerState returned null".into()))?;

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
            PCSTR(c"shader".as_ptr() as *const u8),
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
    code.ok_or_else(|| {
        MediaError::Other(format!("D3DCompile({entry}/{target}) returned null blob"))
    })
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
