//! Golden-file regression test for `wayland_portal::format::build()`.
//!
//! The fixture `tests/fixtures/enum_format_golden.bin` was captured after the
//! N100 GNOME 46 Wayland smoke confirmed `pipewire stream: state Streaming`
//! and BGRA frames flowing through to the P5C-1 VAAPI encoder (2026-05-13).
//! The fixture is committed in T9 and this test runs unconditionally.
//!
//! To refresh the fixture after a verified end-to-end smoke:
//!   cargo run -p prdt-media-linux --example dump_enum_format -- \
//!       crates/media-linux/tests/fixtures/enum_format_golden.bin

#![cfg(target_os = "linux")]

const FIXTURE_PATH: &str = "tests/fixtures/enum_format_golden.bin";

/// Byte-for-byte regression test for `wayland_portal::format::build()`.
///
/// The fixture `tests/fixtures/enum_format_golden.bin` was captured after the
/// N100 GNOME 46 Wayland smoke confirmed `pipewire stream: state Streaming`
/// and BGRA frames flowing through to the P5C-1 VAAPI encoder (2026-05-13).
///
/// To refresh the fixture after a verified end-to-end smoke:
///   cargo run -p prdt-media-linux --example dump_enum_format -- \
///       crates/media-linux/tests/fixtures/enum_format_golden.bin
#[test]
fn enum_format_matches_golden_fixture() {
    let actual = prdt_media_linux::wayland_portal::format::build().bytes[0].clone();
    let fixture = std::fs::read(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read fixture {FIXTURE_PATH}: {e}"));
    if actual != fixture {
        // Surface a helpful diff hint — length first, then first
        // diverging offset.
        eprintln!(
            "build() bytes diverged from fixture (actual={} bytes, fixture={} bytes)",
            actual.len(),
            fixture.len()
        );
        let n = actual.len().min(fixture.len());
        let mut first_diff = None;
        for i in 0..n {
            if actual[i] != fixture[i] {
                first_diff = Some(i);
                break;
            }
        }
        if let Some(off) = first_diff {
            eprintln!("first divergence at byte {off}: actual=0x{:02x} fixture=0x{:02x}",
                      actual[off], fixture[off]);
        } else {
            eprintln!("prefix matches; lengths differ at offset {n}");
        }
        eprintln!(
            "To refresh after a verified end-to-end smoke: \n  cargo run -p prdt-media-linux --example dump_enum_format -- crates/media-linux/tests/fixtures/enum_format_golden.bin"
        );
        panic!("build() bytes diverged from golden fixture");
    }
}
