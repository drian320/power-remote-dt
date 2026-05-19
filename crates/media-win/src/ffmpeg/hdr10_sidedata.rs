//! Thin wrapper around the cross-platform HDR10 SEI parser from
//! `prdt-media-ffmpeg`. Provides call-site plumbing (frame-loop iteration,
//! cache management) for the Windows FFmpeg NVDEC Main10 decoder.
//!
//! Cargo cfg gate: `#[cfg(feature = "media-win-ffmpeg-hdr10-any")]`.

#[cfg(feature = "media-win-ffmpeg-hdr10-any")]
mod inner {
    use prdt_media_core::Hdr10Metadata;

    /// Stateful HDR10 sidecar tracker. Caches the last-seen `Hdr10Metadata`
    /// so the downstream rendering loop can skip redundant swapchain calls
    /// (only forwards a new value when the metadata actually changes).
    ///
    /// Mirrors the `last_hdr10_meta` field pattern in `PlatformRender` (viewer
    /// win.rs) so both sides can deduplicate independently.
    pub struct Hdr10SidedataTracker {
        last: Option<Hdr10Metadata>,
    }

    impl Hdr10SidedataTracker {
        pub fn new() -> Self {
            Self { last: None }
        }

        /// Extract HDR10 sidecar from a decoded `AVFrame` (after `hw_download`)
        /// and update the cache. Returns the extracted metadata (or `None` if
        /// no HDR10 side-data was present on this frame).
        ///
        /// # Safety
        /// `frame` must be a valid `AVFrame` pointer with a valid `side_data`
        /// array of length `nb_side_data`. The pointer must remain valid for
        /// the duration of the call.
        pub unsafe fn extract(
            &mut self,
            frame: *const rusty_ffmpeg_win::ffi::AVFrame,
        ) -> Option<Hdr10Metadata> {
            // SAFETY: caller guarantees `frame` is a valid AVFrame for the call duration.
            let meta = unsafe { prdt_media_ffmpeg::extract_hdr10_sidecar(frame as *const _) };
            if let Some(m) = meta {
                self.last = Some(m);
            }
            // Return the freshly parsed value (or None if absent on this frame);
            // callers that need a cached fallback should inspect `self.last()`.
            meta
        }

        /// Return the most recently seen HDR10 metadata, if any.
        pub fn last(&self) -> Option<Hdr10Metadata> {
            self.last
        }
    }

    impl Default for Hdr10SidedataTracker {
        fn default() -> Self {
            Self::new()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tracker_starts_empty() {
            let t = Hdr10SidedataTracker::new();
            assert!(t.last().is_none());
        }

        #[test]
        fn default_is_empty() {
            let t = Hdr10SidedataTracker::default();
            assert!(t.last().is_none());
        }
    }
}

#[cfg(feature = "media-win-ffmpeg-hdr10-any")]
pub use inner::Hdr10SidedataTracker;
