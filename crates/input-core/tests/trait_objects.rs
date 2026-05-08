use prdt_input_core::{
    ClipboardError, ClipboardProvider, InjectError, InputInjector, VirtualDesktopGeometry,
};
use prdt_protocol::{InputEvent, MonitorRect};

struct DummyInjector;

impl InputInjector for DummyInjector {
    fn inject(&self, _event: InputEvent) -> Result<(), InjectError> {
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "dummy"
    }
}

struct DummyClipboard {
    seq: u64,
    text: String,
}

impl ClipboardProvider for DummyClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        Ok(self.text.clone())
    }
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.text = text.to_string();
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }
    fn sequence_number(&self) -> u64 {
        self.seq
    }
    fn backend_name(&self) -> &'static str {
        "dummy"
    }
}

struct DummyDesktop;

impl VirtualDesktopGeometry for DummyDesktop {
    fn virtual_desktop_rect(&self) -> MonitorRect {
        MonitorRect::new(0, 0, 1920, 1080)
    }
}

#[test]
fn injector_through_dyn() {
    let inj: &dyn InputInjector = &DummyInjector;
    inj.inject(InputEvent::MouseMove {
        x: 0,
        y: 0,
        absolute: true,
    })
    .expect("inject");
    assert_eq!(inj.backend_name(), "dummy");
}

#[test]
fn clipboard_round_trip() {
    let mut cb = DummyClipboard {
        seq: 0,
        text: String::new(),
    };
    cb.write_text("hello").expect("write");
    assert_eq!(cb.read_text().expect("read"), "hello");
    assert_eq!(cb.sequence_number(), 1);
}

#[test]
fn desktop_rect_dimensions() {
    let d = DummyDesktop;
    let r = d.virtual_desktop_rect();
    assert_eq!(r.width(), 1920);
    assert_eq!(r.height(), 1080);
}
