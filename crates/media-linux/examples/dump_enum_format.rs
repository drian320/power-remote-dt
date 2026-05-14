//! Capture the bytes that `wayland_portal::format::build()` currently
//! produces and write them to a binary file. Used to refresh the
//! golden fixture under `tests/fixtures/enum_format_golden.bin` after
//! a verified end-to-end smoke success.
//!
//! Run via: `cargo run -p prdt-media-linux --example dump_enum_format -- <out_path>`

#![cfg(target_os = "linux")]

use std::io::Write;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("usage: dump_enum_format <out_path>"))?;
    let built = prdt_media_linux::wayland_portal::format::build();
    let bytes = &built.bytes[0];
    let mut f = std::fs::File::create(out)?;
    f.write_all(bytes)?;
    eprintln!("wrote {} bytes to {out}", bytes.len());
    Ok(())
}
