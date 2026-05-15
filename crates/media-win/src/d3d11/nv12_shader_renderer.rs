//! Single-NV12-texture → BGRA conversion via custom pixel shader.
//!
//! This bypasses `ID3D11VideoProcessor` entirely (issue #19 Bug 4: the
//! Intel iGPU driver rejected `CreateVideoProcessorInputView` for every
//! documented-allowed `BindFlags` value on CPU-uploaded NV12 textures —
//! `0x0`, `0x8`, `0x20` were all `E_INVALIDARG` even though
//! `CheckVideoProcessorFormat` reported NV12 input as supported).
//!
//! Approach: take the single NV12 texture produced by `CpuI420Uploader`,
//! create two SRVs on it — one as `R8_UNORM` (Y plane) and one as
//! `R8G8_UNORM` (UV plane) — and run the same BT.709 limited-range
//! YUV→RGB pixel shader used by `DualPlaneYuvRenderer`. Standard D3D11
//! NV12 sampling pattern; no video-processor involvement.

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
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM};

use crate::d3d11::swapchain::SwapChain;
use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::{MediaError, Result};

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

const PS_SOURCE: &str = r#"
Texture2D    YPlane  : register(t0);
Texture2D    UVPlane : register(t1);
SamplerState Samp    : register(s0);

float4 main(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    float  y     = YPlane .Sample(Samp, uv).r;
    float2 cbcr  = UVPlane.Sample(Samp, uv).rg;

    y           = (y         - 16.0/255.0) * (255.0/219.0);
    float cb    = (cbcr.x    - 128.0/255.0) * (255.0/224.0);
    float cr    = (cbcr.y    - 128.0/255.0) * (255.0/224.0);

    float3 rgb = float3(
        y +              1.5748 * cr,
        y - 0.1873 * cb - 0.4681 * cr,
        y + 1.8556 * cb
    );
    return float4(saturate(rgb), 1.0);
}
"#;

/// NV12 → BGRA renderer using a custom pixel shader. Takes a single NV12
/// `D3d11Texture` (created with `D3D11_BIND_SHADER_RESOURCE`) and creates
/// two SRVs on it — `R8_UNORM` for Y, `R8G8_UNORM` for UV — letting the
/// driver pick the correct plane from each format. Used by the OpenH264
/// SW-decode path via `CpuI420Uploader`.
pub struct Nv12ShaderRenderer {
    dev: D3d11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl Nv12ShaderRenderer {
    /// Compile the VS/PS, create a linear-clamp sampler. Dimension-agnostic;
    /// `render()` uses the swapchain's size as the viewport.
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
        let sampler =
            sampler.ok_or_else(|| MediaError::Other("CreateSamplerState returned null".into()))?;

        Ok(Self {
            dev: dev.clone(),
            vs,
            ps,
            sampler,
        })
    }

    /// Render the single NV12 `input` into the `swap` back-buffer. Creates
    /// per-frame Y and UV SRVs (cheap; matches the per-frame view pattern
    /// used by `DualPlaneYuvRenderer`).
    pub fn render(&self, input: &D3d11Texture, swap: &SwapChain) -> Result<()> {
        let out_w = swap.width();
        let out_h = swap.height();

        // Reuse the swapchain's cached backbuffer RTV. Creating a *second*
        // RTV on the same flip-discard backbuffer triggered
        // DXGI_ERROR_DEVICE_REMOVED (0x887A0005) on the Intel iGPU during
        // the 2026-05-15 smoke. The SwapChain creates and caches its own
        // RTV at construction (and refreshes it on resize), so no extra
        // CreateRenderTargetView is needed here.
        let rtv = swap
            .rtv()
            .ok_or_else(|| MediaError::Other("SwapChain has no cached RTV".into()))?
            .clone();

        // Both SRVs are on the SAME NV12 texture; the format alone tells
        // the driver which plane to expose (R8 -> Y, R8G8 -> UV).
        let input_res: ID3D11Resource = input
            .raw()
            .cast()
            .map_err(|e| MediaError::d3d11("input NV12 -> Resource", e))?;
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
                    .map_err(|e| MediaError::d3d11("CreateShaderResourceView", e))?;
            }
            srv.ok_or_else(|| MediaError::Other("CreateShaderResourceView returned null".into()))
        };
        let y_srv = make_srv(DXGI_FORMAT_R8_UNORM)?;
        let uv_srv = make_srv(DXGI_FORMAT_R8G8_UNORM)?;

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

unsafe impl Send for Nv12ShaderRenderer {}
unsafe impl Sync for Nv12ShaderRenderer {}

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

    #[test]
    fn constructs_on_default_device() {
        let dev = match D3d11Device::create_default() {
            Ok(d) => d,
            Err(_) => return, // No D3D11 adapter — skip.
        };
        let _r = Nv12ShaderRenderer::new(&dev).expect("Nv12ShaderRenderer::new");
    }
}
