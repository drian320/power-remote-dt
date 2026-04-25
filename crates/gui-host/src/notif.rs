//! Phase 4 G3 OS-toast notifications via `notify-rust`. Supports a 1-second
//! same-kind debounce to avoid notification floods (a flaky network
//! retrying handshake every 200ms would otherwise spam the user).

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifKind {
    Connected,
    Disconnected,
    Error,
}

pub struct Notifier {
    last_kind: Option<NotifKind>,
    last_at: Instant,
    /// Test mode: counts fire() invocations that pass the dedupe check
    /// without actually emitting an OS toast.
    #[cfg(test)]
    test_mode: bool,
    #[cfg(test)]
    test_fire_count: u32,
}

impl Default for Notifier {
    fn default() -> Self {
        Self {
            last_kind: None,
            // Far enough in the past that the first fire() is never
            // suppressed.
            last_at: Instant::now() - Duration::from_secs(3600),
            #[cfg(test)]
            test_mode: false,
            #[cfg(test)]
            test_fire_count: 0,
        }
    }
}

const DEBOUNCE: Duration = Duration::from_secs(1);

impl Notifier {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self {
            test_mode: true,
            ..Default::default()
        }
    }

    /// Fire a notification of the given kind. Same-kind events within
    /// `DEBOUNCE` of the last successful fire are silently dropped.
    pub fn fire(&mut self, kind: NotifKind, body: &str) {
        if self.last_kind == Some(kind) && self.last_at.elapsed() < DEBOUNCE {
            return;
        }
        self.last_kind = Some(kind);
        self.last_at = Instant::now();

        #[cfg(test)]
        if self.test_mode {
            self.test_fire_count += 1;
            return;
        }

        let summary = match kind {
            NotifKind::Connected => "Power Remote Desktop",
            NotifKind::Disconnected => "Power Remote Desktop",
            NotifKind::Error => "Power Remote Desktop",
        };
        if let Err(e) = notify_rust::Notification::new()
            .summary(summary)
            .body(body)
            .show()
        {
            tracing::warn!(?e, "notify-rust show failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_fire_emits() {
        let mut n = Notifier::new_for_test();
        n.fire(NotifKind::Connected, "first");
        assert_eq!(n.test_fire_count, 1);
    }

    #[test]
    fn debounce_swallows_same_kind_within_one_second() {
        let mut n = Notifier::new_for_test();
        n.fire(NotifKind::Connected, "x");
        n.fire(NotifKind::Connected, "y");
        n.fire(NotifKind::Connected, "z");
        assert_eq!(n.test_fire_count, 1);
    }

    #[test]
    fn different_kinds_do_not_dedupe() {
        let mut n = Notifier::new_for_test();
        n.fire(NotifKind::Connected, "a");
        n.fire(NotifKind::Error, "b");
        n.fire(NotifKind::Disconnected, "c");
        assert_eq!(n.test_fire_count, 3);
    }

    #[test]
    fn after_debounce_passes_same_kind_emits_again() {
        let mut n = Notifier::new_for_test();
        n.fire(NotifKind::Connected, "a");
        // Simulate elapsed time by rewinding `last_at` past DEBOUNCE.
        n.last_at = Instant::now() - Duration::from_secs(2);
        n.fire(NotifKind::Connected, "b");
        assert_eq!(n.test_fire_count, 2);
    }
}
