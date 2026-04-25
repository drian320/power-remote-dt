# Plan 2d Zero-Copy NVDEC — Design

**Project**: power-remote-dt
**Phase**: Plan 2d (NVDEC optimization, follow-up to `plan2d-complete`)
**Step**: Eliminate the GPU→CPU→GPU NV12 bounce in the NVDEC decode path
**Date**: 2026-04-25
**Status**: Draft (built on `phase2-w6-polish-complete` master)

---

## Summary

現在の NVDEC 経路は decode 後に NV12 を一度 CPU pinned buffer に降ろし、`UpdateSubresource` で D3D11 テクスチャに上げ直している。1080p60 で約 120MB/s の PCIe 往復が常時発生し、`known_limitations.md` §2d で記録されている「単一 GPU loopback で MF より遅い」原因の主体。

実機検証(`probe_nv12_shader_resource_only_interop` を 2026-04-25 に実行)で「単一 NV12 D3D11 texture を CUDA に register しても UV plane を独立 CUarray として取り出せない(`Y=OK UV=FAIL`)」ことを確認した。よって NVIDIA SDK が標準とする **dual R8 + R8G8 D3D11 texture** パターンを採用する。

到達目標: NVDEC decode → R8/R8G8 D3D11 texture(CUDA-D3D11 interop で device-to-device コピー)→ 自前 YUV→BGRA pixel shader で swapchain に描画。CPU を通さない。

---

## Scope

### In-scope

- `TextureFormat::R8` / `R8G8` variant + `to_dxgi`(`R8_UNORM` / `R8G8_UNORM`)
- 既存 `D3d11Texture::new_for_cuda_interop` は format 引数で R8 / R8G8 を受理(現実装は format 不問の thin wrapper、追加実装不要見込み)
- `CuvidDecoder` の display callback を改修:
  - `cuvidMapVideoFrame64` で得た CUdeviceptr から **直接 CUDA-registered の R8 / R8G8 CUarray へ `cuMemcpy2D_v2` で device-to-device コピー**
  - CPU NV12 `Vec<u8>` パスは `#[cfg(any(test, feature = "cpu-nv12"))]` で残す(production はオフ)
- `DecodedFrame` の置換:
  - 新型 `DualPlaneFrame { y_tex: D3d11Texture, uv_tex: D3d11Texture, timestamp_us: i64 }`
  - 旧 `DecodedFrame { nv12: Vec<u8>, ... }` は test-only API として残す
- `NvdecD3d11Consumer`:
  - `nv12_cache` を `(y_cache: Option<D3d11Texture>, uv_cache: Option<D3d11Texture>, registrations: Option<(CUgraphicsResource, CUgraphicsResource)>)` に置換
  - `take_latest_texture` 削除(返り値型が単一 NV12 texture 前提のため使えない)
  - 新規 `take_latest_dual_plane() -> Option<DualPlaneFrame>` を追加
- 新 renderer `DualPlaneYuvRenderer`(`crates/media-win/src/d3d11/dual_plane_renderer.rs`):
  - 自前 vertex shader(フルスクリーン三角形)+ pixel shader(BT.709 limited-range YUV→BGRA)
  - Y(R8 SRV) と UV(R8G8 SRV、半解像度)を sampler でフィルタリング
  - swapchain への描画 API は `Nv12Renderer` と同じ shape(`render(&mut self, dual: &DualPlaneFrame, swapchain: &SwapChain)`)
- viewer 側 `--decoder nvdec` 経路を `DualPlaneYuvRenderer` に切替、`--decoder mf` は既存 `Nv12Renderer` のまま
- bench: 同一エンコード列を MF/NVDEC 双方に流して decode_p95 を比較する spot test

### Out (将来 / 別プラン)

- HDR / 10bit (P010 format)
- BT.601 ↔ BT.709 自動切替(HEVC SPS から colour_primaries を読んで matrix 選択)
- 解像度スケール shader(現状 swapchain サイズ = 入力サイズ前提、Plan 4 で letterbox)
- MF 経路の dual-plane 化(MF は単一 NV12 がそのまま速い)
- ステップピング interop(複数 GPU 跨ぎ)
- AV1 decode 経路

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| Interop 戦略 | dual R8 + R8G8 textures(NVIDIA SDK 標準パターン) | 単一 NV12 interop は実機検証で UV plane が CUarray として露出しないことが判明 |
| renderer 戦略 | NVDEC 専用に `DualPlaneYuvRenderer` 新設、MF 経路の `Nv12Renderer` は据え置き | 影響を NVDEC 経路に局所化、MF 側にリスクなし |
| Y texture format | `DXGI_FORMAT_R8_UNORM` (W×H, 1 byte/pixel) | NV12 の Y plane と同レイアウト |
| UV texture format | `DXGI_FORMAT_R8G8_UNORM` (W/2×H/2, 2 bytes/pixel) | NV12 の UV plane(half-width and half-height、interleaved UV)と一致 |
| 解像度サポート | 4K まで(NVDEC 出力 = 入力解像度) | RTX 3070 Ti / GTX 1080 共に 8K まで decode 可、shader 側に上限なし |
| color matrix | BT.709 limited-range, hardcoded | 現 NVENC は default で BT.709 を吐く、自動切替は YAGNI |
| CPU NV12 path | `#[cfg(any(test, feature = "cpu-nv12"))]` で残す | unit test 用に CPU 比較が要る、production からは外す |
| renderer API shape | `render(dual: &DualPlaneFrame, swapchain: &SwapChain)` | 既存 `Nv12Renderer::render(tex: &D3d11Texture, ...)` と並びを揃える |
| viewer 統一抽象 | trait は導入しない、`enum Renderer { Mf(Nv12Renderer), Nvdec(DualPlaneYuvRenderer) }` で分岐 | 現状 2 種で十分、trait 化は overengineer |
| bench 手段 | `prdt-latency-bench` の既存 stage 計測ではなく ad-hoc spot test | bench は wiring 工数が大きく、相対比較が見えれば十分 |

---

## Architecture

### データフロー(変更後)

```
NVENC encode → UDP → NVDEC decoder
                            ↓ cuvidMapVideoFrame64
                            ↓ CUdeviceptr (NV12, 同 GPU 上)
                            ↓ cuMemcpy2D_v2 (device-to-device)
                       ┌────┴───────────────┐
                       ↓                    ↓
                  CUarray (R8 Y)      CUarray (R8G8 UV)
                  ↑                    ↑
                  cuGraphicsD3D11Register / Map
                  ↑                    ↑
              D3d11Texture(R8)    D3d11Texture(R8G8)
                       ↓                    ↓
                  Y SRV               UV SRV
                       ↓                    ↓
                       └─────┬──────────────┘
                             ↓
                  DualPlaneYuvRenderer (PS: BT.709 limited)
                             ↓
                       BGRA swapchain
```

PCIe 経由なし、全部同 GPU 内 device-to-device。

### モジュール変更マップ

```
crates/media-win/src/d3d11/
  texture.rs
    + TextureFormat::R8         → DXGI_FORMAT_R8_UNORM
    + TextureFormat::R8G8       → DXGI_FORMAT_R8G8_UNORM
    + TextureFormat::bytes_per_pixel_y() の R8/R8G8 ケース

  dual_plane_renderer.rs (新規)
    + struct DualPlaneYuvRenderer { ... }
    + impl DualPlaneYuvRenderer {
    +     pub fn new(dev, input_w, input_h, output_w, output_h) -> Result<Self>
    +     pub fn render(&mut self, dual: &DualPlaneFrame, sc: &SwapChain) -> Result<()>
    + }
    + (内部) HLSL VS/PS バイトコードを include_bytes! で持つ、
    +        D3DCompile を build.rs で実行して .dxbc 生成

  mod.rs
    + pub use dual_plane_renderer::DualPlaneYuvRenderer;

crates/media-win/src/nvdec/
  decoder.rs
    - DecodedFrame { nv12: Vec<u8>, width, height, timestamp_us }
    + DualPlaneFrame { y_tex: D3d11Texture, uv_tex: D3d11Texture, width, height, timestamp_us }
    Display callback:
      - cuMemcpy2D_v2 → CPU pinned buffer → Vec<u8>
      + cuMemcpy2D_v2 → mapped CUarray (Y) and (UV)
    #[cfg(any(test, feature = "cpu-nv12"))] のみ:
      pub fn take_latest_frame() -> Option<DecodedFrame>  // 既存 CPU 経路
    新規:
      pub fn take_latest_dual_plane() -> Option<DualPlaneFrame>

  consumer.rs
    - nv12_cache: Mutex<Option<D3d11Texture>>
    + dual_cache: Mutex<DualCache>  // y_tex, uv_tex, registrations
    - take_latest_texture()
    + take_latest_dual_plane() -> Option<DualPlaneFrame>
    - upload_nv12_to_cache()
    + setup_dual_textures(width, height)  // texture creation + CUDA register

crates/viewer/src/main.rs
  - let renderer = Nv12Renderer::new(...);
  + let renderer: ViewerRenderer = match args.decoder {
  +     "mf" => ViewerRenderer::Mf(Nv12Renderer::new(...)),
  +     "nvdec" => ViewerRenderer::Nvdec(DualPlaneYuvRenderer::new(...)),
  + };
  - take_latest_texture() → render(tex)
  + take_latest()(decoder-specific dispatch)→ render(frame)
```

### `DualPlaneYuvRenderer` の HLSL

build.rs で D3DCompile を呼ぶか、`fxc.exe` を CI/local に置くかは実装時の判断。最小依存案: `windows::Win32::Graphics::Direct3D::Fxc::D3DCompile` を runtime 呼び出し(viewer 起動時に 1 回コンパイル)。.dxbc 事前ビルドは bindings 揃えの追加コスト。

```hlsl
// vertex.hlsl  --- フルスクリーン三角形
struct VsOut {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

VsOut main(uint id : SV_VertexID) {
    VsOut o;
    // 0,1,2 → (-1,-1), (3,-1), (-1,3) で 1 三角形が画面全部をカバー
    o.uv  = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2,-2) + float2(-1,1), 0, 1);
    return o;
}

// pixel.hlsl  --- BT.709 limited-range YUV → BGRA
Texture2D    YPlane  : register(t0);
Texture2D    UVPlane : register(t1);
SamplerState Samp    : register(s0);

float4 main(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    float  y  = YPlane .Sample(Samp, uv).r;
    float2 uv2 = UVPlane.Sample(Samp, uv).rg;

    // Limited range Y: 16/255 .. 235/255 → 0..1
    // Limited range Cb/Cr: 16/255 .. 240/255 → -0.5..0.5
    y          = (y          - 16.0/255.0) * (255.0/219.0);
    float cb   = (uv2.x      - 128.0/255.0) * (255.0/224.0);
    float cr   = (uv2.y      - 128.0/255.0) * (255.0/224.0);

    float3 rgb = float3(
        y +              1.5748 * cr,
        y - 0.1873 * cb - 0.4681 * cr,
        y + 1.8556 * cb
    );
    return float4(rgb, 1.0);  // BGRA target writes RGB → BGR via swizzle in target
}
```

注: HLSL の `float4` の RGBA 順、swapchain は `B8G8R8A8_UNORM`。output merger 時に `(R, G, B, 1.0)` を書けば BGRA target にそのまま BGR が乗る — `D3D11_RENDER_TARGET_BLEND_DESC` のデフォルトでは write mask は RGBA 全 ON、color チャネルが BGRA 形式の場合 RT 内部表現は BGRA。HLSL 側は常に float4 の x=R, y=G, z=B として扱われ、driver が RT の format に従ってバイト並びを変換する。よって上の return は正しい。

### `DualPlaneFrame` 構造体

```rust
pub struct DualPlaneFrame {
    pub y_tex: D3d11Texture,    // R8, width × height
    pub uv_tex: D3d11Texture,   // R8G8, width/2 × height/2
    pub width: u32,             // 元 NV12 の幅
    pub height: u32,            // 元 NV12 の高さ
    pub timestamp_us: i64,
}
```

`y_tex` と `uv_tex` の clone は cheap(`ID3D11Texture2D` の Arc-like ref count 増加だけ)。

### `NvdecD3d11Consumer` の dual cache

```rust
struct DualCache {
    y_tex: D3d11Texture,
    uv_tex: D3d11Texture,
    y_cuda_res: ffi::CUgraphicsResource,
    uv_cuda_res: ffi::CUgraphicsResource,
    width: u32,
    height: u32,
}
```

ライフサイクル:
1. 初回 `submit` 後 sequence callback で width/height が確定 → `setup_dual_textures` 呼び出し
2. `D3d11Texture::new_for_cuda_interop(R8, w, h)` + `new_for_cuda_interop(R8G8, w/2, h/2)`
3. 各々 `cuGraphicsD3D11RegisterResource` で `CUgraphicsResource` を保持
4. 解像度変化があれば(再ネゴ等) `Drop` で `cuGraphicsUnregisterResource` → 再構築
5. display callback ごとに `cuGraphicsMapResources(&[y, uv])` → `cuGraphicsSubResourceGetMappedArray` で 2 CUarray → `cuMemcpy2D_v2` 2 回 → `cuGraphicsUnmapResources`
6. `take_latest_dual_plane` は最新 `DualPlaneFrame` の `Mutex<Option<DualPlaneFrame>>` から swap

### CUDA 同期

- `cuMemcpy2D_v2`(同期 API)を使うので `cuStreamSynchronize` は不要
- async 化(`cuMemcpyAsync` + stream)は YAGNI(現状の bottleneck はコピーではなく display callback 側、ベンチで回ってから判断)

---

## Testing Strategy

### 1. `TextureFormat::{R8, R8G8}` unit

`crates/media-win/src/d3d11/texture.rs` の既存 `#[cfg(test)]` ブロックに追加:

```rust
#[test]
fn r8_format_dxgi_mapping() {
    assert_eq!(TextureFormat::R8.to_dxgi(), DXGI_FORMAT_R8_UNORM);
    assert_eq!(TextureFormat::R8.bytes_per_pixel_y(), 1);
}

#[test]
fn r8g8_format_dxgi_mapping() {
    assert_eq!(TextureFormat::R8G8.to_dxgi(), DXGI_FORMAT_R8G8_UNORM);
    assert_eq!(TextureFormat::R8G8.bytes_per_pixel_y(), 2);
}
```

### 2. `D3d11Texture::new_for_cuda_interop` で R8 / R8G8 が register できる probe

`crates/media-win/src/nvdec/consumer.rs` の `#[cfg(test)]` 内に新規:

```rust
#[cfg(prdt_nvdec_bindings)]
#[test]
fn dual_plane_textures_register_with_cuda() {
    // NVIDIA adapter が無いマシンは skip。
    // R8 (256×256) と R8G8 (128×128) を作って cuGraphicsD3D11RegisterResource に通す。
    // 両方 success かつ map → CUarray 取得 → unmap → unregister がノーエラーで完走することを確認。
}
```

### 3. Decode end-to-end(GPU 経路)

既存 `decode_single_nvenc_frame_round_trip` を改修して dual-plane 経路もカバー:

```rust
#[cfg(prdt_nvdec_bindings)]
#[test]
fn decode_emits_dual_plane_textures() {
    // 既存テスト同様 NVENC で 5 frames 焼く → NvdecD3d11Consumer に submit
    // → take_latest_dual_plane().unwrap()
    // → y_tex.format() == R8, uv_tex.format() == R8G8
    // → y_tex.width() == w, y_tex.height() == h
    // → uv_tex.width() == w/2, uv_tex.height() == h/2
}
```

### 4. CPU 比較 fallback test(色味確認用)

`#[cfg(feature = "cpu-nv12")]` で旧 CPU 経路を有効化、同一 NAL を CPU 経路と GPU 経路の両方で decode して Y plane の MAE(mean absolute error)が 1 LSB 以内であることを確認:

```rust
#[cfg(all(prdt_nvdec_bindings, feature = "cpu-nv12"))]
#[test]
fn cpu_and_gpu_paths_agree_within_1lsb() {
    // 同一 NAL を 2 つの decoder インスタンスに submit
    // CPU 経路: take_latest_frame().nv12 の Y/UV を直接比較
    // GPU 経路: y_tex を staging texture 経由で readback、uv も同様
    // pixel-by-pixel MAE ≤ 1
}
```

### 5. `DualPlaneYuvRenderer` smoke

`crates/media-win/src/d3d11/dual_plane_renderer.rs` の `#[cfg(test)]`:

```rust
#[test]
fn dual_plane_renderer_constructs() {
    // D3D11 device を作って DualPlaneYuvRenderer::new(1920, 1080, 1920, 1080) を呼ぶ
    // shader compile が通ること、processor 構築でエラーが出ないことを assert
}

#[test]
fn dual_plane_renderer_renders_solid_yellow() {
    // R8 を 1.0 (Y=1.0)、R8G8 を (0.5, 0.5)(Cb=0, Cr=0)で埋める → 真っ白に近い色
    // 別パターンで Y=0.5, U=1.0, V=0.0(青系)も試す
    // swapchain readback で BGRA pixel が期待値の ±2 LSB 以内
}
```

### 6. ベンチ比較(spot test、formal でない)

新ファイル `crates/media-win/tests/zerocopy_bench_compare.rs`:

```rust
#[test]
#[ignore]  // cargo test --ignored で明示実行
#[cfg(prdt_nvdec_bindings)]
fn compare_mf_vs_nvdec_decode_throughput() {
    // 同一 NAL stream(60 frames @ 1080p)を MfD3d11Consumer と NvdecD3d11Consumer で
    // それぞれ submit → take_latest_*() を loop、p50/p95 を eprintln
    // 2026-04-25 計測値を comment で記録(CI/regression にしない、参考値)
}
```

### 7. workspace test regression

`cargo test --workspace` で全テスト pass、特に既存の MF 経路の `mf_smoke_*` が無回帰。

---

## Exit Criteria

- [ ] `TextureFormat::R8` / `R8G8` 追加 + unit test
- [ ] `DualPlaneFrame` 型定義
- [ ] `CuvidDecoder` の display callback を device-to-device コピーに変更、CPU NV12 path は `#[cfg(any(test, feature = "cpu-nv12"))]` で残す
- [ ] `NvdecD3d11Consumer::take_latest_dual_plane()` 実装
- [ ] CUDA register / unregister のライフサイクル管理(`Drop` 含む)
- [ ] `dual_plane_textures_register_with_cuda` probe pass
- [ ] `decode_emits_dual_plane_textures` end-to-end test pass
- [ ] CPU/GPU 比較 test(feature gate 越し)pass
- [ ] `DualPlaneYuvRenderer` 実装(VS/PS HLSL、render API)
- [ ] viewer の `--decoder nvdec` 経路が新 renderer を使う
- [ ] `compare_mf_vs_nvdec_decode_throughput` で **NVDEC 改善が観測**(p95 が現状より少なくとも 30% 短縮、または ≤ MF の値、を comment で記録)
- [ ] workspace test 全 pass、clippy clean、fmt clean(touched files)
- [ ] git tag `plan2d-zerocopy-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| `cuGraphicsD3D11RegisterResource` が R8/R8G8 / SHADER_RESOURCE のみで register できない driver | 主機能不可、A 自体が崩壊 | spec 確定前の probe で確認(Y plane は既に OK 実績、R8 単独は R8 SRV-only NV12 サブと等価で driver が同じ extent code path を通る見込み)。最初のタスクで dual_plane_textures_register_with_cuda を書いて FAIL なら BLOCKED、user に報告 |
| `D3DCompile` runtime call の依存追加 | viewer 起動時間が +50-100ms | shader はそれぞれ 数 LOC、HLSL→DXBC コンパイルは driver 起動時間に隠れる |
| BT.709 limited-range が NVENC の現実出力と microscopically ずれる(色味差) | 視覚的に違和感 | feature `cpu-nv12` で CPU 経路と pixel-by-pixel 比較 → MAE ≤ 1 LSB を test で検証。差が大きければ matrix 定数を SDK 推奨値に置き換え |
| 解像度変化(再ネゴで sequence callback が再発火)時のキャッシュ寿命 | 古い CUgraphicsResource を unregister し損ねるとリーク | DualCache を `Drop` で必ず unregister、解像度変化時は old cache を Drop してから new を construct |
| HLSL の swapchain B8G8R8A8 順と RGBA 出力の対応 | 色味反転(BGR ↔ RGB) | HLSL float4 (R, G, B, 1.0) を返せば driver が RT format に従って正しい順で書き込む。`dual_plane_renderer_renders_solid_yellow` test で Y=1, UV=(0.5,0.5) → R/G/B 全部 ~1.0 を観測することで実証 |
| CUDA stream non-default 化を後で追加するときの破壊変更 | API 破壊 | 当面 `cuMemcpy2D_v2`(同期)で進めて async 化は別 plan に切り出し |

---

## Open Questions(実装中に決めてよい)

- `DualPlaneYuvRenderer` の sampler は `LINEAR` か `POINT` か。NV12 の UV は半解像度なので LINEAR(bilinear)推奨だが、shader の MIP level 指定 (`SampleLevel`) を併用するか
- HLSL のソースを `.hlsl` ファイルとして配置するか、Rust 文字列リテラル(`r#"..."#`)に埋め込むか — 規模が小さいので埋め込み推奨
- shader コンパイルエラー時の error report path — `D3DCompile` の error blob を `MediaError::Other(String)` に通す
- `cpu-nv12` feature を `prdt-media-win` の Cargo.toml に追加する正確な書式

---

## References

- Plan 2d 全体: `docs/superpowers/specs/` 配下に W3 当時のものはなく、project_overview.md の plan2d-* タグが履歴
- 既存 NVDEC 実装:
  - `crates/media-win/src/nvdec/decoder.rs`(CPU bounce 経路)
  - `crates/media-win/src/nvdec/consumer.rs`(NV12 cache + UpdateSubresource、interop probe)
- 既存 renderer: `crates/media-win/src/d3d11/nv12_renderer.rs`(VideoProcessor 経路、MF 用)
- CUDA-D3D11 interop 公式 sample: NVIDIA Video Codec SDK `Samples/AppDecode/AppDecD3D11/`(参考)
