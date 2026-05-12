//! Viewer-side cursor compositing state. Receives ControlMessage::CursorUpdate
//! over the wire, holds the latest position + cached bitmap, and exposes
//! a `composite_target` helper that the platform-specific renderer uses
//! to alpha-blend the cursor on top of the decoded frame.

use prdt_protocol::CursorBitmap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CursorState {
    /// Last received id; informational only (Codex flag: compositors may
    /// recycle ids, so don't use for cache invalidation).
    pub id: u32,
    pub position_x: i32,
    pub position_y: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    /// `None` until first bitmap received. `Some(width=0, height=0)` means
    /// "host signals invisible cursor — hide compositing".
    bitmap: Option<Arc<CursorBitmap>>,
}

impl CursorState {
    pub fn new() -> Self {
        Self {
            id: 0,
            position_x: 0,
            position_y: 0,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap: None,
        }
    }

    /// Apply a wire update. Position always overwrites; bitmap is replaced
    /// ONLY when the message carries one (bitmap-presence is the cache
    /// invalidation signal — NOT the id).
    pub fn apply(
        &mut self,
        id: u32,
        position_x: i32,
        position_y: i32,
        hotspot_x: i32,
        hotspot_y: i32,
        bitmap: Option<CursorBitmap>,
    ) {
        self.id = id;
        self.position_x = position_x;
        self.position_y = position_y;
        self.hotspot_x = hotspot_x;
        self.hotspot_y = hotspot_y;
        if let Some(b) = bitmap {
            // Defense in depth: protocol decoder already validates this, but a
            // buggy decoder upgrade or future field change could let bad values
            // through. Skip the bitmap update on mismatch; keep the cached bitmap.
            let valid_invisible = b.width == 0 && b.height == 0 && b.bgra.is_empty();
            let expected = (b.width as usize) * (b.height as usize) * 4;
            if valid_invisible || b.bgra.len() == expected {
                self.bitmap = Some(Arc::new(b));
            } else {
                tracing::warn!(
                    width = b.width,
                    height = b.height,
                    bgra_len = b.bgra.len(),
                    "skipping cursor bitmap update — len mismatch (kept cache)"
                );
            }
        }
        // If bitmap is None: keep cached (reuse).
    }

    /// `true` when we have a non-empty cached bitmap to draw.
    pub fn visible(&self) -> bool {
        self.bitmap
            .as_ref()
            .is_some_and(|b| b.width > 0 && b.height > 0)
    }

    pub fn bitmap(&self) -> Option<&CursorBitmap> {
        self.bitmap.as_deref()
    }
}

impl Default for CursorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_position_only_keeps_cached_bitmap() {
        let mut s = CursorState::new();
        let bmp = CursorBitmap {
            width: 2,
            height: 1,
            bgra: vec![0xff, 0, 0, 0xff, 0, 0xff, 0, 0xff],
        };
        s.apply(1, 10, 20, 0, 0, Some(bmp.clone()));
        assert!(s.visible());
        // Position-only update.
        s.apply(2, 100, 200, 0, 0, None);
        assert_eq!((s.position_x, s.position_y), (100, 200));
        assert_eq!(s.bitmap().map(|b| (b.width, b.height)), Some((2, 1)));
    }

    #[test]
    fn apply_invisible_bitmap_hides_compositing() {
        let mut s = CursorState::new();
        s.apply(
            1,
            0,
            0,
            0,
            0,
            Some(CursorBitmap {
                width: 0,
                height: 0,
                bgra: vec![],
            }),
        );
        assert!(!s.visible());
    }

    #[test]
    fn apply_new_bitmap_replaces_cache() {
        let mut s = CursorState::new();
        let b1 = CursorBitmap {
            width: 2,
            height: 1,
            bgra: vec![0u8; 8],
        };
        let b2 = CursorBitmap {
            width: 4,
            height: 2,
            bgra: vec![0u8; 32],
        };
        s.apply(1, 0, 0, 0, 0, Some(b1));
        s.apply(1, 0, 0, 0, 0, Some(b2)); // same id; bitmap-presence triggers swap
        assert_eq!(s.bitmap().map(|b| (b.width, b.height)), Some((4, 2)));
    }

    #[test]
    fn apply_skips_bitmap_with_len_mismatch_keeps_cache() {
        let mut s = CursorState::new();
        // Seed with a valid 2x1 bitmap.
        s.apply(
            1,
            0,
            0,
            0,
            0,
            Some(CursorBitmap {
                width: 2,
                height: 1,
                bgra: vec![0u8; 8],
            }),
        );
        assert!(s.visible());
        // Apply a malformed bitmap — claims 4x4 but only 4 bytes.
        s.apply(
            2,
            0,
            0,
            0,
            0,
            Some(CursorBitmap {
                width: 4,
                height: 4,
                bgra: vec![0u8; 4],
            }),
        );
        // Cache must still be the valid 2x1 from the first apply.
        assert_eq!(s.bitmap().map(|b| (b.width, b.height)), Some((2, 1)));
    }
}
