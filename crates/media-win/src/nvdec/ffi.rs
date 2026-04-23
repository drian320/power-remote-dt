//! Auto-generated NVDEC (cuvid) bindings. See build.rs (Plan 2d).
//!
//! Empty when CUDA_PATH isn't set — the outer consumer falls back to a
//! `NotAvailable` runtime error so unrelated parts of the build keep
//! compiling.

#![allow(clippy::all)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(unused)]
#![allow(improper_ctypes)]
#![allow(unnecessary_transmutes)]

include!(concat!(env!("OUT_DIR"), "/nvdec_bindings.rs"));
