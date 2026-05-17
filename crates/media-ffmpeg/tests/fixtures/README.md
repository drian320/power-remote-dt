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

---

## `main10_sample.hevc`

Annex-B HEVC Main10 IDR fixture (320×240, 1 frame) for the P3 PR2 round-trip
test `crates/media-ffmpeg/tests/main10_decode.rs::sw_main10::round_trip_sw_main10_fixture`.

The frame must carry HDR10 SEI (mastering display colour volume + content light
level) so the `hdr10` sidecar assertion passes.

**The committed file is currently an empty placeholder.** The test is
`#[ignore]`d until the real fixture is generated and committed.

### To generate

On any Linux host with `ffmpeg` ≥ 5 and `libx265`:

```sh
# Generate a synthetic 320×240 BT.2020 PQ test signal (yuv420p10le, 1 frame)
# and encode it to HEVC Main10 with HDR10 SEI using ffmpeg's libx265 encoder.
ffmpeg -f lavfi \
  -i "color=c=0x101010:size=320x240:rate=1:duration=1,format=yuv420p10le" \
  -vf "colorspace=all=bt2020:iall=bt2020:iprimaries=bt2020:itrc=smpte2084:ispace=bt2020ncl" \
  -c:v libx265 \
  -x265-params "profile=main10:hdr10=1:hdr10-opt=1:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,50):max-cll=1000,400:annexb=1" \
  -frames:v 1 \
  -f hevc \
  crates/media-ffmpeg/tests/fixtures/main10_sample.hevc
```

Alternatively, use the dev-container encoder examples once the Main10 encoder
is merged to master. Commit the generated file (typically ~5 KB).

### Verification after generation

Run the test without `--ignored` (it must pass once the fixture is present):

```sh
./scripts/dev-container.sh bash -c \
  'cargo test -p prdt-media-ffmpeg \
     --features ffmpeg-decode-hevc-sw-main10-ffmpeg5 \
     --test main10_decode \
     -- sw_main10::round_trip_sw_main10_fixture --include-ignored'
```
