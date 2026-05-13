//! Golden-file regression test for `wayland_portal::format::build()`.
//!
//! The fixture `tests/fixtures/enum_format_golden.bin` is captured
//! immediately after the N100 GNOME 46 Wayland smoke confirms
//! `pipewire stream: state Streaming` and BGRA frames flowing through
//! to the P5C-1 VAAPI encoder. Until that fixture exists in the repo
//! this test is marked `#[ignore]` (T7 commit only ships the test
//! scaffold; fixture capture + un-ignore land together in T9 after
//! T8 reports success).
//!
//! To run after the fixture lands:
//!   cargo test -p prdt-media-linux --test format_golden -- --ignored

#![cfg(target_os = "linux")]

const FIXTURE_PATH: &str = "tests/fixtures/enum_format_golden.bin";

/// Byte-for-byte regression test for `wayland_portal::format::build()`.
///
/// The fixture `tests/fixtures/enum_format_golden.bin` is captured
/// immediately after the N100 GNOME 46 Wayland smoke confirms
/// `pipewire stream: state Streaming` and BGRA frames flowing through
/// to the P5C-1 VAAPI encoder. Until that fixture exists in the repo
/// this test is marked `#[ignore]` (T7 commit only ships the test
/// scaffold; fixture capture + un-ignore land together in T9 after
/// T8 reports success).
///
/// To run after the fixture lands:
///   cargo test -p prdt-media-linux --test format_golden -- --ignored
#[test]
#[ignore = "fixture is captured in T9 after the T8 N100 smoke; un-ignore then"]
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
