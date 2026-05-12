//! DMABUF receive path: `mmap(PROT_READ, MAP_PRIVATE)` of a
//! `F_DUPFD_CLOEXEC`-dup'd FD handed from the PipeWire `process`
//! callback. See `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md` §3.4.
//!
//! # Test-injection rationale (decided in T3 Step 1)
//!
//! Probe result (T3 Step 1): `pipewire::spa::buffer::Data` (libspa 0.9.2) is
//! `#[repr(transparent)] pub struct Data(spa_sys::spa_data)` with NO public
//! constructor — confirmed no `from_raw`, no `from_raw_ptr`, no publicly
//! accessible field. The struct also has NO lifetime parameter (the plan's
//! `Data<'_>` annotation is incorrect for this version). Additionally,
//! `Data` has a direct `pub fn fd(&self) -> RawFd` method, so the `SpaDataLike`
//! impl uses `self.fd()` directly rather than `(*self).as_raw().fd`.
//!
//! Because `Data` is opaque with no public constructor, we cannot
//! unsafe-cast zeroed bytes into a `&Data` for unit tests. Instead we
//! expose `pub trait SpaDataLike { fn fd, fn maxsize, fn mapoffset }`
//! with a blanket impl on `&Data` for production and a hand-written
//! `TestData` in the test module.
//!
//! If a future pipewire-rs release adds a public constructor, the trait
//! can be retired in favour of direct unsafe-construct. Production
//! behaviour is identical either way.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

/// Tiny trait over `pipewire::spa::buffer::Data` exposing the three
/// fields the DMABUF mmap helper needs. Lets unit tests inject a
/// hand-built stub without constructing a real `spa_data`.
pub trait SpaDataLike {
    fn fd(&self) -> i32;
    fn maxsize(&self) -> u32;
    fn mapoffset(&self) -> u32;
}

impl SpaDataLike for &pipewire::spa::buffer::Data {
    fn fd(&self) -> i32 {
        // Data::fd() returns RawFd (i32 on Linux); -1 is the sentinel.
        pipewire::spa::buffer::Data::fd(self)
    }
    fn maxsize(&self) -> u32 {
        // as_raw() is a safe fn returning &spa_data; maxsize is a plain u32 field.
        self.as_raw().maxsize
    }
    fn mapoffset(&self) -> u32 {
        self.as_raw().mapoffset
    }
}

/// CPU-mapped DMABUF plane. Owns the dup'd FD AND the mmap'd region.
///
/// **Field order is load-bearing.** `_fd: OwnedFd` is declared FIRST so
/// the auto-generated drop sequence is:
/// 1. Our explicit `impl Drop` runs `munmap(self.ptr, self.len)`.
/// 2. Auto-Drop of fields in declaration order: `_fd: OwnedFd` first
///    (closes the FD AFTER munmap), then `ptr` / `len` / `data_off`
///    (primitives, no-op).
///
/// Reversing the field order would close the FD before munmap, which is
/// safe per kernel semantics (mmap holds its own ref) but pointlessly
/// confusing — the explicit ordering documents intent. See spec §3.4
/// and risk #5 in the design doc.
pub struct MappedPlane {
    _fd: OwnedFd,
    ptr: *mut u8,
    len: usize,
    data_off: usize,
}

impl std::fmt::Debug for MappedPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedPlane")
            .field("ptr", &self.ptr)
            .field("len", &self.len)
            .field("data_off", &self.data_off)
            .finish()
    }
}

// SAFETY: `MappedPlane` owns the mmap region exclusively (no aliasing);
// the kernel guarantees the mapping is valid until munmap. Sending it
// across threads is safe because we never expose `ptr` outside `bytes()`.
unsafe impl Send for MappedPlane {}

impl Drop for MappedPlane {
    fn drop(&mut self) {
        // SAFETY: ptr / len came from a successful mmap (only path that
        // constructs MappedPlane). Never aliased — MappedPlane is the
        // sole owner. After munmap, `_fd: OwnedFd` auto-Drop closes the
        // dup'd FD.
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
        }
    }
}

impl MappedPlane {
    /// Slice of the mapped bytes starting at `data_off` (the chunk offset
    /// within the mapping). Callers should clamp further to
    /// `chunk.offset + chunk.size` for the per-frame valid region.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: ptr is non-null (mmap success), `len - data_off` bytes
        // are mapped read-only and the lifetime is tied to &self.
        unsafe { std::slice::from_raw_parts(self.ptr.add(self.data_off), self.len - self.data_off) }
    }
}

/// Map a DMABUF-backed plane into the process address space via
/// `mmap(PROT_READ, MAP_PRIVATE)`. The FD is dup'd with `F_DUPFD_CLOEXEC`
/// so the mapping outlives the PipeWire callback stack frame.
///
/// # Safety
///
/// Caller asserts `d` is a `SPA_DATA_DmaBuf`-typed `Data` (or stub) with
/// a valid open FD. Passing a closed FD or `fd == -1` returns
/// `Err(io::ErrorKind::InvalidData)`; passing a non-dmabuf FD is
/// well-defined (the kernel just refuses or returns a regular mapping).
///
/// Note: `D` is taken by value so that `&pipewire::spa::buffer::Data`
/// (which implements `SpaDataLike`) can be passed directly: the caller
/// writes `map_dmabuf_plane(&buf_data)` and `D` is inferred as the
/// reference type itself.
pub unsafe fn map_dmabuf_plane<D: SpaDataLike>(d: D) -> io::Result<MappedPlane> {
    let raw_fd = d.fd();
    if raw_fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "spa_data has no fd (fd<0)",
        ));
    }
    let map_len = (d.maxsize() as usize)
        .checked_add(d.mapoffset() as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "maxsize+mapoffset overflow"))?;
    if map_len == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "map_len == 0"));
    }

    // SAFETY: raw_fd is non-negative (checked above) and the caller's
    // contract states it's a valid open FD inside the callback. F_DUPFD_CLOEXEC
    // with minfd=3 keeps the new FD out of stdin/stdout/stderr.
    let dupfd = libc::fcntl(raw_fd, libc::F_DUPFD_CLOEXEC, 3);
    if dupfd < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: dupfd is a valid FD opened above; PROT_READ + MAP_PRIVATE
    // is safe on any mappable FD. The kernel handles dmabuf cache-coherency
    // semantics on PROT_READ (implicit sync; explicit sync is P5B-3+).
    let ptr = libc::mmap(
        std::ptr::null_mut(),
        map_len,
        libc::PROT_READ,
        libc::MAP_PRIVATE,
        dupfd,
        0,
    );
    if ptr == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        // SAFETY: dupfd is valid; close on a freshly-opened FD is safe.
        libc::close(dupfd);
        return Err(err);
    }

    // SAFETY: dupfd is a fresh FD we own; OwnedFd takes ownership and
    // will close it on Drop (after our explicit munmap, see field order).
    let fd = OwnedFd::from_raw_fd(dupfd);

    Ok(MappedPlane {
        _fd: fd,
        ptr: ptr.cast(),
        len: map_len,
        data_off: d.mapoffset() as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;

    /// In-test stand-in for `pipewire::spa::buffer::Data` carrying just the
    /// three fields the mmap helper reads.
    struct TestData {
        fd: i32,
        maxsize: u32,
        mapoffset: u32,
    }
    impl SpaDataLike for &TestData {
        fn fd(&self) -> i32 {
            self.fd
        }
        fn maxsize(&self) -> u32 {
            self.maxsize
        }
        fn mapoffset(&self) -> u32 {
            self.mapoffset
        }
    }

    /// Create a memfd_create-backed shared memory region of `len` bytes
    /// filled with `pattern` and return the raw FD (caller owns it).
    fn memfd_with_pattern(len: usize, pattern: &[u8]) -> i32 {
        // SAFETY: memfd_create is a wrapper over the kernel syscall;
        // name pointer is a valid C string with no embedded NULs.
        let name = b"prdt-test-memfd\0";
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr() as *const libc::c_char,
                0u32,
            )
        };
        assert!(
            fd >= 0,
            "memfd_create failed: {}",
            io::Error::last_os_error()
        );
        let fd = fd as i32;
        // SAFETY: fd is a freshly created memfd; ftruncate is safe on a fresh memfd.
        let r = unsafe { libc::ftruncate(fd, len as libc::off_t) };
        assert_eq!(r, 0, "ftruncate failed: {}", io::Error::last_os_error());
        // Write the pattern via the FD (we own it; OwnedFd round-trip would
        // close it, so use libc::write directly on a borrowed raw FD).
        let n = pattern.len();
        // SAFETY: fd is valid; ptr/len describe a borrowed slice we own.
        let written = unsafe { libc::write(fd, pattern.as_ptr() as *const _, n) };
        assert_eq!(
            written as usize,
            n,
            "write failed: {}",
            io::Error::last_os_error()
        );
        fd
    }

    #[test]
    fn map_dmabuf_plane_reads_known_pattern_and_drops_cleanly() {
        let pattern = b"P5B2A";
        let len = 4096usize; // one page
        let raw_fd = memfd_with_pattern(len, pattern);

        let data = TestData {
            fd: raw_fd,
            maxsize: len as u32,
            mapoffset: 0,
        };

        // SAFETY: TestData carries a valid memfd we just created; the
        // contract assertion (real `Data` is a DMABUF) is upheld by the
        // test setup (memfd is CPU-readable; the helper doesn't care
        // whether the kernel-side object is dmabuf or memfd, only that
        // it's mmappable read-only).
        let mapped = unsafe { map_dmabuf_plane(&data) }.expect("map ok");
        let bytes = mapped.bytes();
        assert_eq!(&bytes[..pattern.len()], pattern, "pattern mismatch");
        drop(mapped);

        // Closing the original fd should still succeed because
        // map_dmabuf_plane dup'd it; the dup'd copy was closed on
        // MappedPlane::drop. SAFETY: raw_fd is still our valid FD.
        let r = unsafe { libc::close(raw_fd) };
        assert_eq!(
            r,
            0,
            "close of original fd should succeed: {}",
            io::Error::last_os_error()
        );
    }

    #[test]
    fn map_dmabuf_plane_dup_keeps_original_fd_alive_after_drop() {
        let pattern = b"DUP-TEST";
        let len = 4096usize;
        let raw_fd = memfd_with_pattern(len, pattern);

        let data = TestData {
            fd: raw_fd,
            maxsize: len as u32,
            mapoffset: 0,
        };
        // SAFETY: see test above.
        let mapped = unsafe { map_dmabuf_plane(&data) }.expect("map ok");
        drop(mapped);

        // Original FD should still be usable for a read (we proved the
        // close in the previous test; this test proves the fd is alive
        // by issuing a non-destructive fstat).
        // SAFETY: raw_fd is still our valid FD; fstat is non-destructive.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::fstat(raw_fd, &mut st) };
        assert_eq!(
            r, 0,
            "fstat on original fd should succeed after MappedPlane drop"
        );
        assert_eq!(st.st_size as usize, len, "size should be {len}");

        // Cleanup.
        // SAFETY: raw_fd is still our valid FD.
        let _ = unsafe { libc::close(raw_fd) };
    }

    #[test]
    fn map_dmabuf_plane_invalid_fd_returns_err() {
        // FD -1 is the sentinel for "no fd"; the helper must error rather
        // than calling mmap on -1 (which kernel would EBADF anyway).
        let data = TestData {
            fd: -1,
            maxsize: 4096,
            mapoffset: 0,
        };
        // SAFETY: helper is unsafe because caller asserts dmabuf; this
        // call deliberately violates the precondition to test the guard.
        let r = unsafe { map_dmabuf_plane(&data) };
        assert!(r.is_err(), "fd=-1 must return Err");
        let e = r.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    // Silence unused-import warnings for AsRawFd / Write in this minimal test.
    #[allow(dead_code)]
    fn _silence_unused(_: &dyn AsRawFd, _: &dyn Write) {}
}
