//! GPU integration smoke tests. These require a working D3D11 device and
//! will fail on headless CI without a GPU. We do NOT mark them `#[ignore]`
//! by default — the dev machine must pass them — but Plan 2a tasks document
//! this explicitly so CI fails loudly if ever run on a non-GPU runner.

#![cfg(windows)]

use prdt_media_win::{
    synthetic::{bgra_with_counter, decode_counter_bgra, make_counter_texture},
    D3d11Device, D3d11Texture, MmcssScope, TextureFormat,
};

#[test]
fn full_round_trip_counter_through_gpu() {
    let dev = D3d11Device::create_default().expect("D3D11 device");
    for seq in [0u32, 1, 123, 999_999, u32::MAX] {
        let tex = make_counter_texture(&dev, 256, 144, seq).expect("make texture");
        let buf = tex.read_back_bgra_or_rgba(&dev).expect("readback");
        let decoded = decode_counter_bgra(&buf).expect("decode counter");
        assert_eq!(decoded, seq);
    }
}

#[test]
fn default_texture_creation_all_formats() {
    let dev = D3d11Device::create_default().expect("D3D11 device");
    let _ = D3d11Texture::new_default(&dev, 64, 64, TextureFormat::Bgra8).unwrap();
    let _ = D3d11Texture::new_default(&dev, 64, 64, TextureFormat::Rgba8).unwrap();
    // NV12 often has strict size alignment (even dims). Ensure a 64x64 case works.
    let _ = D3d11Texture::new_default(&dev, 64, 64, TextureFormat::Nv12).unwrap();
}

#[test]
fn mmcss_games_under_task() {
    // Attach MMCSS, do some GPU work, detach. This is a sanity check that
    // MMCSS registration doesn't interact badly with D3D11 calls.
    let _scope = MmcssScope::games().expect("MMCSS games");
    let dev = D3d11Device::create_default().expect("D3D11 device");
    let buf = bgra_with_counter(128, 128, 42, (0, 0, 0));
    assert!(!buf.is_empty());
    let tex = make_counter_texture(&dev, 128, 128, 7).expect("texture");
    let back = tex.read_back_bgra_or_rgba(&dev).unwrap();
    assert_eq!(decode_counter_bgra(&back), Some(7));
}
