# Golden fixtures for the A13 byte-stability test

## `byte_stable_nvenc_h265.bin`

Encoded H.265 Annex-B output of the deterministic 30-frame I420 sequence
defined by `examples/gen_byte_stable_fixture.rs`, encoded by
`HevcNvencFfmpegEncoder` with the fixed config
`(320×240, fps=30, bitrate=4 Mbps, gop=30)`.

**The committed file is currently an empty placeholder.** Per P2.5 plan §3
(iter-3 M2), the real golden bytestream must be generated **once** on real
NVIDIA hardware against the pre-R6 master commit (currently `56a81fc`).

The empty placeholder lets `include_bytes!` compile in
`hevc_nvenc_encoder::tests::byte_stable_against_master_fixture`; the test is
`#[ignore]`d so the dev-container's CUDA-less environment skips it cleanly.
On the smoke runner, the test will fail loudly with a length mismatch until
a real fixture is generated and committed.

### To regenerate

```sh
git checkout 56a81fc   # pre-R6 master
./scripts/dev-container.sh bash -c \
  'cargo run -p prdt-media-ffmpeg \
     --features ffmpeg-encode-hevc-nvenc-ffmpeg5 \
     --example gen_byte_stable_fixture \
     --target x86_64-unknown-linux-gnu \
     -- crates/media-ffmpeg/tests/fixtures/byte_stable_nvenc_h265.bin'
git add crates/media-ffmpeg/tests/fixtures/byte_stable_nvenc_h265.bin
```

Then return to the P2.5 branch and run the test under the smoke runner; it
will pass when the post-R6 encoder produces byte-identical output to the
pre-R6 golden.

### When to re-generate

The encoder's output is deterministic given identical FFmpeg + NVENC SDK +
driver + config. If the smoke runner's driver/SDK/FFmpeg ABI changes
materially, the fixture must be re-generated and committed in the same PR
that bumps the floor.
