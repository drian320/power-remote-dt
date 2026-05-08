use prdt_protocol::{InputEvent, MonitorRect};

use crate::{ClipboardError, InjectError};

/// Inject one `InputEvent` (mouse move / button / wheel / key) into the
/// host's local input system. Synchronous and best-effort.
pub trait InputInjector: Send {
    fn inject(&self, event: InputEvent) -> Result<(), InjectError>;
    fn backend_name(&self) -> &'static str;
}

/// Read / write the user's primary clipboard text channel. Backends may
/// hold transient state (e.g. a Wayland portal handle) so the trait
/// requires `&mut self` for both calls.
pub trait ClipboardProvider: Send {
    fn read_text(&mut self) -> Result<String, ClipboardError>;
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;

    /// Monotonic counter that bumps each time the user changes the
    /// system clipboard. Used by the host's clipboard-sync poller to
    /// avoid round-tripping unchanged content.
    fn sequence_number(&self) -> u64;

    fn backend_name(&self) -> &'static str;
}

/// Returns the bounding rect of the host's combined virtual desktop in
/// the same coordinate system the host backend's `InputInjector`
/// expects for absolute pointer events.
pub trait VirtualDesktopGeometry: Send {
    fn virtual_desktop_rect(&self) -> MonitorRect;
}
