//! `tracing_subscriber::Layer` that buffers the last N formatted log lines
//! into a shared `VecDeque<String>`. Used by host GUI to show a recent
//! activity tail without changing the existing stderr output.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

#[derive(Clone)]
pub struct TailHandle {
    inner: Arc<Mutex<VecDeque<String>>>,
    capacity: usize,
}

impl TailHandle {
    pub fn snapshot(&self) -> Vec<String> {
        self.inner.lock().unwrap().iter().cloned().collect()
    }

    fn push(&self, line: String) {
        let mut q = self.inner.lock().unwrap();
        q.push_back(line);
        while q.len() > self.capacity {
            q.pop_front();
        }
    }
}

pub struct TailLayer {
    handle: TailHandle,
}

impl TailLayer {
    /// Build a TailLayer that retains at most `capacity` lines.
    pub fn new(capacity: usize) -> (Self, TailHandle) {
        let q = VecDeque::with_capacity(capacity);
        let handle = TailHandle {
            inner: Arc::new(Mutex::new(q)),
            capacity: capacity.max(1),
        };
        (
            Self {
                handle: handle.clone(),
            },
            handle,
        )
    }
}

impl<S: Subscriber> Layer<S> for TailLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        struct V(String);
        impl Visit for V {
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "message" {
                    // format as a quoted string so callers can distinguish
                    // the message from surrounding metadata
                    use std::fmt::Write;
                    let _ = write!(&mut self.0, "{value:?}");
                }
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    use std::fmt::Write;
                    // fmt::Arguments formats without quotes via {:?}; wrap
                    // manually so the output matches record_str's quoting
                    let s = format!("{value:?}");
                    let _ = write!(&mut self.0, "\"{s}\"");
                }
            }
        }
        let mut v = V(String::new());
        event.record(&mut v);
        let level = *event.metadata().level();
        let target = event.metadata().target();
        let line = format!("{level:5} {target}: {}", v.0);
        self.handle.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    #[test]
    fn captures_recent_events_up_to_capacity() {
        let (layer, handle) = TailLayer::new(3);
        let subscriber = Registry::default().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("a");
            tracing::info!("b");
            tracing::info!("c");
            tracing::info!("d");
        });
        let snap = handle.snapshot();
        assert_eq!(snap.len(), 3);
        assert!(snap[0].contains("\"b\""), "snap[0] = {:?}", snap[0]);
        assert!(snap[2].contains("\"d\""), "snap[2] = {:?}", snap[2]);
    }
}
