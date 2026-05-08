# L0 Trait Extraction — Status

**Branch:** `phase-l0-trait-extraction`
**Plan:** `docs/superpowers/plans/2026-05-08-l0-trait-extraction.md`
**Base:** `master @ 9c7db33`

## Delivered

- New `crates/media-core` (Capturer / Encoder / Decoder traits + `EncodedPacket`).
- New `crates/input-core` (InputInjector / ClipboardProvider / VirtualDesktopGeometry traits).
- `prdt-media-win::HwHevcEncoder` now `impl prdt_media_core::Encoder<Frame = D3d11Texture>`. `MediaError::UnsupportedFormat` correctly routes to `EncodeError::FormatMismatch`.
- `prdt-input-win::SendInputInjector` impls `prdt_input_core::InputInjector`.
- `prdt-input-win::Win32Clipboard` (unit struct) impls `prdt_input_core::ClipboardProvider`.
- `prdt-input-win::Win32VirtualDesktop` impls `prdt_input_core::VirtualDesktopGeometry`.
- Empty skeleton crates `crates/media-linux` and `crates/input-linux` wired into the workspace, gated by `#![cfg(target_os = "linux")]`.

## Not delivered (deferred)

- Host / viewer code is **not** rewired through the new traits. Trait surface is exercised only by unit + smoke tests.
- No Linux implementation. `media-linux` / `input-linux` are empty.
- No `directories`-based key-path migration (separate cross-platform PR).
- No README rewrite or platform-support matrix.
- `EncodeError::DeviceLost` variant is not added — `media-win` adapter currently collapses `MediaError::DeviceRemoved` into `EncodeError::Backend(...)` with a comment noting the limitation. A follow-up task should add `DeviceLost` so L1 host wiring can distinguish "recreate device" from "transient encode failure".

## Regression posture

- `cargo check --workspace` green on Windows.
- L0 crates (`prdt-media-core`, `prdt-input-core`, `prdt-media-win`, `prdt-input-win`, `prdt-media-linux`, `prdt-input-linux`) all clippy-clean under `-D warnings`.
- 331 workspace tests passing (3 ignored, hardware-gated). No previously-passing test now fails.
- 8 new tests added across the new crates and adapter smoke suites; all green.
- Pre-existing concern (out of L0 scope): `prdt-host` has 8 clippy warnings from before this branch (unused-mut x6, large_enum_variant x1, unnecessary_map_or x1). Branch did not introduce these and did not fix them — separate clean-up PR recommended.

## Test counts

| Crate | New | Existing | Total | Ignored |
|---|---|---|---|---|
| prdt-media-core | 1 | 0 | 1 | 0 |
| prdt-input-core | 3 | 0 | 3 | 0 |
| prdt-media-win | 1 | 38+5 | 44 | 1 |
| prdt-input-win | 3 | 5 | 8 | 2 |

## Commits

| SHA | Subject |
|---|---|
| `dc57672` | L0 Task 1: add prdt-media-core crate with Capturer/Encoder/Decoder traits |
| `8970f44` | L0 Task 1 review fixes: dyn-compat test, rust-version, set_target_bitrate doc |
| `aa5793c` | L0 Task 2: add prdt-input-core crate with InputInjector / ClipboardProvider / VirtualDesktopGeometry traits |
| `19cce0a` | L0 Task 2 review fixes: ClipboardProvider::sequence_number takes &mut self, add InjectError::BackendUnavailable, clarify VirtualDesktopGeometry doc |
| `8ad158c` | L0 Task 3: media-win impls prdt-media-core::Encoder for HwHevcEncoder |
| `6590674` | L0 Task 3 review fixes: route UnsupportedFormat to FormatMismatch, doc DeviceRemoved limitation, alpha-order lib.rs mods, drop misleading test assertion |
| `41000d8` | L0 Task 4: input-win impls prdt-input-core traits (InputInjector / ClipboardProvider / VirtualDesktopGeometry) |
| `99f4f3c` | L0 Task 4 review fixes: drop unused Win32Clipboard.last_seq, simplify to unit struct, add L0 scope doc note |
| `941d994` | L0 Task 5: add prdt-media-linux skeleton crate (empty on non-Linux) |
| `38211de` | L0 Task 6: add prdt-input-linux skeleton crate (empty on non-Linux) |

## Next plan

L1 — Linux PoC: X11 capture + media-sw encode + uinput inject. Plan to be written separately. Requires a Linux dev environment; do not start without one.

Suggested follow-up clean-up tasks (independent of L1):

1. Add `EncodeError::DeviceLost` variant and update `media-win` adapter `map_err`.
2. Fix the 8 pre-existing clippy warnings in `prdt-host`.
3. Migrate `host-key.bin` / `viewer-key.bin` paths through the `directories` crate.
