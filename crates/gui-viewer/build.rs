//! Note: previously embedded a Windows version resource via winres into
//! prdt-viewer.exe. That responsibility moved to crates/client/build.rs
//! (the prdt.exe bin). Embedding here caused CVT1100 duplicate-VERSION
//! when both prdt-host and prdt-viewer libs were linked into one bin.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
