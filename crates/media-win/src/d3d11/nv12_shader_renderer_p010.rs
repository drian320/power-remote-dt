//! P010 → R10G10B10A2_UNORM (HDR10) conversion via custom pixel shader.
//!
//! Sibling of `nv12_shader_renderer.rs` for the 10-bit P010LE path.
//! Takes a single P010 texture produced by the 10-bit decode path, creates
//! two SRVs on it — `R16_UNORM` (Y plane) and `R16G16_UNORM` (UV plane) —
//! and runs a BT.2020 NCL Y′CbCr → R′G′B′ matrix. Because the swapchain is
//! set to `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`, the compositor
//! expects PQ-encoded RGB output; P010 samples are already in PQ-encoded
//! Y′CbCr space, so the matrix conversion stays in PQ space (no EOTF
//! inverse + re-encode needed).
//!
//! Only compiled when the `media-win-hdr10` feature is active.

use std::ffi::CString;

use windows::core::Interface;
use windows::core::PCSTR;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D::{
    D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11PixelShader, ID3D11Resource, ID3D11SamplerState, ID3D11ShaderResourceView,
    ID3D11VertexShader, D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_SAMPLER_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0,
    D3D11_TEX2D_SRV, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16_UNORM, DXGI_FORMAT_R16_UNORM};

use crate::d3d11::swapchain::SwapChain;
use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::{MediaError, Result};

// Shared fullscreen-triangle vertex shader (identical to the 8-bit sibling).
const VS_SOURCE: &str = r#"
struct VsOut {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

VsOut main(uint id : SV_VertexID) {
    VsOut o;
    o.uv  = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return o;
}
"#;

// P010 BT.2020 NCL Y′CbCr → R′G′B′ pixel shader.
//
// P010 stores 10 valid bits in the HIGH 10 bits of each 16-bit container.
// Sampling as R16_UNORM / R16G16_UNORM returns values in [0, 1] mapped
// from [0, 65535]. The valid 10-bit codes map to [0, 65535] in 64-unit
// steps (each code = 64 in 16-bit). We scale back to 10-bit code space
// by multiplying by 65535/64 ≈ 1023.98 ≈ 1023 (exact: 65535.0 / 64.0).
//
// BT.2020 limited-range 10-bit:
//   Y′  legal range [64, 940]  → normalised Y′_l = (code - 64) / 876
//   Cb′  legal range [64, 960] → normalised Cb_l = (code - 512) / 896
//   Cr′  legal range [64, 960] → normalised Cr_l = (code - 512) / 896
//
// BT.2020 NCL Y′CbCr → R′G′B′ matrix (ITU-R BT.2020-2 Table 4):
//   R′ =  Y′ + 1.4746 * Cr′
//   G′ =  Y′ - 0.16455 * Cb′ - 0.57135 * Cr′
//   B′ =  Y′ + 1.8814 * Cb′
//
// The output is PQ-encoded R′G′B′ in [0, 1] suitable for an
// R10G10B10A2_UNORM swapchain configured with
// DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020. The OS compositor maps
// the 10-bit PQ values to the display's native luminance.
const PS_SOURCE: &str = r#"
Texture2D<float>  YPlane  : register(t0);   // R16_UNORM  (Y plane)
Texture2D<float2> UVPlane : register(t1);   // R16G16_UNORM (interleaved UV)
SamplerState      Samp    : register(s0);

float4 main(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    // Scale R16_UNORM [0,1] back to 10-bit code space [0, 1023].
    float  y_code  = YPlane .Sample(Samp, uv).r  * (65535.0 / 64.0);
    float2 uv_code = UVPlane.Sample(Samp, uv).rg * (65535.0 / 64.0);

    // BT.2020 limited-range normalisation to [0, 1] signal range.
    float y_l  = (y_code  -  64.0) / 876.0;
    float cb_l = (uv_code.x - 512.0) / 896.0;
    float cr_l = (uv_code.y - 512.0) / 896.0;

    // BT.2020 NCL inverse matrix. Input/output stay in PQ-encoded space;
    // no EOTF is applied (swapchain color space is SMPTE ST 2084 PQ).
    float3 rgb = float3(
        y_l +              1.4746  * cr_l,
        y_l - 0.16455 * cb_l - 0.57135 * cr_l,
        y_l + 1.8814  * cb_l
    );
    return float4(saturate(rgb), 1.0);
}
"#;

/// P010 → R10G10B10A2_UNORM (HDR10) renderer using a custom pixel shader.
///
/// Takes a single P010 `D3d11Texture` (created with `D3D11_BIND_SHADER_RESOURCE`)
/// and creates two SRVs on it — `R16_UNORM` for Y, `R16G16_UNORM` for UV.
/// The output swapchain must be in `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`.
pub struct Nv12ShaderRendererP010 {
    dev: D3d11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl Nv12ShaderRendererP010 {
    /// Compile the VS/PS and create a linear-clamp sampler.
    pub fn new(dev: &D3d11Device) -> Result<Self> {
        let vs_blob = compile_shader(VS_SOURCE, "main", "vs_5_0")?;
        let ps_blob = compile_shader(PS_SOURCE, "main", "ps_5_0")?;
        let (vs, ps) = unsafe {
            let mut vs: Option<ID3D11VertexShader> = None;
            dev.device()
                .CreateVertexShader(blob_slice(&vs_blob), None, Some(&mut vs))
                .map_err(|e| MediaError::d3d11("CreateVertexShader (P010)", e))?;
            let vs = vs.ok_or_else(|| {
                MediaError::Other("CreateVertexShader (P010) returned null".into())
            })?;

            let mut ps: Option<ID3D11PixelShader> = None;
            dev.device()
                .CreatePixelShader(blob_slice(&ps_blob), None, Some(&mut ps))
                .map_err(|e| MediaError::d3d11("CreatePixelShader (P010)", e))?;
            let ps = ps.ok_or_else(|| {
                MediaError::Other("CreatePixelShader (P010) returned null".into())
            })?;

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
                .map_err(|e| MediaError::d3d11("CreateSamplerState (P010)", e))?;
        }
        let sampler = sampler
            .ok_or_else(|| MediaError::Other("CreateSamplerState (P010) returned null".into()))?;

        Ok(Self {
            dev: dev.clone(),
            vs,
            ps,
            sampler,
        })
    }

    /// Render the P010 `input` into the HDR10 `swap` back-buffer. Creates
    /// per-frame Y (`R16_UNORM`) and UV (`R16G16_UNORM`) SRVs on the same
    /// P010 texture; the driver picks the correct plane from the format.
    pub fn render(&self, input: &D3d11Texture, swap: &SwapChain) -> Result<()> {
        let out_w = swap.width();
        let out_h = swap.height();

        // Reuse the swapchain's cached backbuffer RTV — creating a second RTV
        // on the same flip-discard backbuffer triggered DXGI_ERROR_DEVICE_REMOVED
        // on Intel iGPU (see nv12_shader_renderer.rs for the full rationale).
        let rtv = swap
            .rtv()
            .ok_or_else(|| MediaError::Other("SwapChain has no cached RTV".into()))?
            .clone();

        // Both SRVs are on the SAME P010 texture; the format selects the plane.
        let input_res: ID3D11Resource = input
            .raw()
            .cast()
            .map_err(|e| MediaError::d3d11("input P010 -> Resource", e))?;

        let make_srv = |fmt| -> Result<ID3D11ShaderResourceView> {
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
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            unsafe {
                self.dev
                    .device()
                    .CreateShaderResourceView(&input_res, Some(&desc), Some(&mut srv))
                    .map_err(|e| MediaError::d3d11("CreateShaderResourceView (P010)", e))?;
            }
            srv.ok_or_else(|| {
                MediaError::Other("CreateShaderResourceView (P010) returned null".into())
            })
        };
        let y_srv = make_srv(DXGI_FORMAT_R16_UNORM)?;
        let uv_srv = make_srv(DXGI_FORMAT_R16G16_UNORM)?;

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
            ctx.IASetInputLayout(None);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(y_srv.clone()), Some(uv_srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(3, 0);
            // Unbind SRVs so the texture can be re-uploaded next frame.
            ctx.PSSetShaderResources(0, Some(&[None, None]));
            ctx.OMSetRenderTargets(Some(&[None]), None);
        });
        Ok(())
    }
}

unsafe impl Send for Nv12ShaderRendererP010 {}
unsafe impl Sync for Nv12ShaderRendererP010 {}

fn compile_shader(src: &str, entry: &str, target: &str) -> Result<ID3DBlob> {
    let entry_c = CString::new(entry).expect("entry has no NUL bytes");
    let target_c = CString::new(target).expect("target has no NUL bytes");
    let mut code: Option<ID3DBlob> = None;
    let mut errs: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(
            src.as_ptr() as *const _,
            src.len(),
            PCSTR(c"shader_p010".as_ptr() as *const u8),
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
            "D3DCompile({entry}/{target}) P010 failed: {e}: {err_msg}"
        )));
    }
    code.ok_or_else(|| {
        MediaError::Other(format!(
            "D3DCompile({entry}/{target}) P010 returned null blob"
        ))
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

    #[test]
    fn constructs_on_default_device() {
        let dev = match D3d11Device::create_default() {
            Ok(d) => d,
            Err(_) => return, // No D3D11 adapter — skip.
        };
        let _r = Nv12ShaderRendererP010::new(&dev).expect("Nv12ShaderRendererP010::new");
    }
}
