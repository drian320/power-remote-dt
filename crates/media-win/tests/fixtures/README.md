# F-WIN-FFMPEG PR3.5 HDR10 parity fixture

`hevc_main10_hdr10_sample.hevc` — 16-frame, 1920×1080, HEVC Main10 raw
bitstream carrying both `mastering_display_colour_volume` and
`content_light_level_info` SEIs. Used by the
`crates/media-win/tests/hdr10_parity.rs` parity test which decodes the
same bitstream through MF (`MfHevcMain10Decoder`) and FFmpeg-NVDEC
(`HevcNvdecMain10FfmpegDecoderWindowsAdapter`) and asserts the extracted
`Hdr10Metadata` matches byte-for-byte.

## Provenance

| Field         | Value                                                                 |
| ------------- | --------------------------------------------------------------------- |
| Generated     | 2026-05-19                                                            |
| Resolution    | 1920×1080                                                             |
| Frame rate    | 30 fps                                                                |
| Duration      | 0.534 s (16 frames)                                                   |
| Pixel format  | yuv420p10le                                                           |
| Encoder       | libx265 (via ffmpeg 5.1.9-0+deb12u1 in dev container)                 |
| File size     | 345 612 bytes                                                         |
| sha256        | `048d6790062ae9df6789b70b541509da65d1c003d22822cfbf3db3ffcb2286e0`    |

## Regeneration command (verbatim, runnable from repo root)

```sh
./scripts/dev-container.sh bash -c '
  cd crates/media-win/tests/fixtures && \
  ffmpeg -y -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=0.534" \
    -pix_fmt yuv420p10le \
    -c:v libx265 \
    -x265-params "master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(10000000,1):max-cll=1000,400:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc" \
    -frames:v 16 \
    hevc_main10_hdr10_sample.hevc
'
```

The x265 chromaticity convention uses 1/50000 fixed-point units:

| Primary | Chromaticity (x, y)   | Encoded as (50000ths) |
| ------- | --------------------- | --------------------- |
| Red     | (0.708, 0.292)        | (35400, 14600)        |
| Green   | (0.170, 0.797)        | (8500, 39850)         |
| Blue    | (0.131, 0.046)        | (6550, 2300)          |
| White   | D65 (0.3127, 0.329)   | (15635, 16450)        |

Luminance is encoded as `L(max, min)` in 1/10000 cd/m² units, so
`L(10000000,1)` ⇒ max = 1000 cd/m², min = 0.0001 cd/m². MaxCLL = 1000,
MaxFALL = 400 (cd/m², plain ints).

## Provenance assertion (run before vendoring an updated fixture)

```sh
./scripts/dev-container.sh bash -c '
  ffprobe -hide_banner -loglevel error -show_frames -of json \
    crates/media-win/tests/fixtures/hevc_main10_hdr10_sample.hevc \
  | python3 -c "
import json, sys
frames = json.load(sys.stdin)[\"frames\"]
sd = {e[\"side_data_type\"] for e in frames[0][\"side_data_list\"]}
assert \"Mastering display metadata\" in sd, f\"missing MDCV SEI: {sd}\"
assert \"Content light level metadata\" in sd, f\"missing CLL SEI: {sd}\"
print(\"OK: both SEIs present on frame 0\")
"
'
```

Expected output:

```
OK: both SEIs present on frame 0
```

If either SEI is missing the libx265 command above silently dropped
them (x265 quietly skips `master-display` if any sub-parameter parses
wrong) and the fixture is broken — re-run the regeneration command,
re-record the sha256, and bump the table above.

## Why this fixture is vendored (not regenerated at test time)

Two reasons:

1. **CI determinism.** Windows-latest does not have ffmpeg+libx265 in
   its toolchain; only Linux dev-container has them via the
   `Dockerfile.dev` update in commit (this PR). Vendoring the bitstream
   lets the parity test run on every windows-latest CI invocation.
2. **Provenance freeze.** Both decoders must see byte-identical input
   for the parity comparison to be meaningful. Re-encoding per test
   run would introduce frame-level differences from x265's
   non-determinism.

If you need to bump the fixture (e.g. add more frames, change SEIs),
follow the procedure above and update both the sha256 in this README
and the constant in `hdr10_parity.rs`.
