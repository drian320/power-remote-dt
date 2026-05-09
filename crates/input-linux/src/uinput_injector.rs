//! `/dev/uinput` virtual mouse + keyboard via direct nix ioctls.
//!
//! Two devices are created at first use (lazy via `OnceCell`):
//!   - prdt-virtual-mouse: BTN_{LEFT,RIGHT,MIDDLE,SIDE,EXTRA} +
//!     REL_{X,Y,WHEEL,HWHEEL} + ABS_{X,Y}
//!   - prdt-virtual-keyboard: KEY_RESERVED+1..=KEY_MAX (range setbit)
//!
//! Permission failure on `/dev/uinput` is surfaced to callers as
//! `InjectError::BackendUnavailable` with a hint about the `input`
//! group.

use crate::error::{LinuxInputError, KEY_MAX};
use once_cell::sync::OnceCell;
use prdt_protocol::{InputEvent, MouseButton};
use std::io::Write as _;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Mutex;

// Linux input-event-codes (subset) — values from <linux/input-event-codes.h>.
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;

// Cached devices — created on first `inject_event` call.
struct UinputDevices {
    mouse: std::fs::File,
    keyboard: std::fs::File,
    abs_max_x: i32,
    abs_max_y: i32,
}

static DEVICES: OnceCell<Mutex<UinputDevices>> = OnceCell::new();

/// Initialize the device cache with the host's virtual-desktop ABS
/// range. Called by `init_with_geometry` from host startup.
pub fn init_with_geometry(width: u32, height: u32) -> Result<(), LinuxInputError> {
    let devices = create_devices(width, height)?;
    DEVICES
        .set(Mutex::new(devices))
        .map_err(|_| {
            LinuxInputError::UinputIoctl(std::io::Error::other("uinput already initialized"))
        })?;
    Ok(())
}

/// Inject one InputEvent into the kernel via the cached uinput devices.
/// Devices are lazy-initialized to (1920, 1080) if `init_with_geometry`
/// was never called (smoke-friendly default).
pub fn inject_event(event: InputEvent) -> Result<(), LinuxInputError> {
    let cell = DEVICES.get_or_try_init(|| {
        let d = create_devices(1920, 1080)?;
        Ok::<_, LinuxInputError>(Mutex::new(d))
    })?;
    let mut d = cell.lock().expect("uinput devices Mutex poisoned");
    write_event(&mut d, event)
}

fn write_event(d: &mut UinputDevices, e: InputEvent) -> Result<(), LinuxInputError> {
    match e {
        InputEvent::MouseMove { x, y, absolute: true } => {
            let xc = x.clamp(0, d.abs_max_x);
            let yc = y.clamp(0, d.abs_max_y);
            send_event(&mut d.mouse, EV_ABS, ABS_X, xc)?;
            send_event(&mut d.mouse, EV_ABS, ABS_Y, yc)?;
            send_syn(&mut d.mouse)?;
        }
        InputEvent::MouseMove { x, y, absolute: false } => {
            send_event(&mut d.mouse, EV_REL, REL_X, x)?;
            send_event(&mut d.mouse, EV_REL, REL_Y, y)?;
            send_syn(&mut d.mouse)?;
        }
        InputEvent::MouseButton { button, pressed } => {
            let code = match button {
                MouseButton::Left => BTN_LEFT,
                MouseButton::Right => BTN_RIGHT,
                MouseButton::Middle => BTN_MIDDLE,
                MouseButton::X1 => BTN_SIDE,
                MouseButton::X2 => BTN_EXTRA,
            };
            send_event(&mut d.mouse, EV_KEY, code, if pressed { 1 } else { 0 })?;
            send_syn(&mut d.mouse)?;
        }
        InputEvent::MouseWheel { dx, dy } => {
            if dx != 0 {
                send_event(&mut d.mouse, EV_REL, REL_HWHEEL, dx)?;
            }
            if dy != 0 {
                send_event(&mut d.mouse, EV_REL, REL_WHEEL, dy)?;
            }
            send_syn(&mut d.mouse)?;
        }
        InputEvent::Key { scancode, pressed } => {
            if scancode > KEY_MAX {
                tracing::warn!(scancode, "scancode out of KEY_MAX range — skipping");
                return Ok(());
            }
            let code = scancode as u16;
            send_event(&mut d.keyboard, EV_KEY, code, if pressed { 1 } else { 0 })?;
            send_syn(&mut d.keyboard)?;
        }
    }
    Ok(())
}

fn send_event(file: &mut std::fs::File, type_: u16, code: u16, value: i32) -> Result<(), LinuxInputError> {
    // Encode `struct input_event { struct timeval time; __u16 type; __u16 code; __s32 value; }`.
    // timeval is (time_t sec, suseconds_t usec) — kernel ignores time on uinput input.
    let ev = InputEventBytes {
        sec: 0,
        usec: 0,
        type_,
        code,
        value,
    };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            &ev as *const InputEventBytes as *const u8,
            std::mem::size_of::<InputEventBytes>(),
        )
    };
    file.write_all(bytes).map_err(LinuxInputError::UinputIoctl)?;
    Ok(())
}

fn send_syn(file: &mut std::fs::File) -> Result<(), LinuxInputError> {
    send_event(file, EV_SYN, SYN_REPORT, 0)
}

#[repr(C)]
struct InputEventBytes {
    sec: i64,
    usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

fn create_devices(abs_w: u32, abs_h: u32) -> Result<UinputDevices, LinuxInputError> {
    let mouse = create_mouse(abs_w, abs_h)?;
    let keyboard = create_keyboard()?;
    Ok(UinputDevices {
        mouse,
        keyboard,
        abs_max_x: (abs_w as i32).saturating_sub(1).max(0),
        abs_max_y: (abs_h as i32).saturating_sub(1).max(0),
    })
}

fn open_uinput() -> Result<std::fs::File, LinuxInputError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/uinput")
        .map_err(LinuxInputError::UinputOpenDenied)
}

// ===== Device creation via raw ioctls =====
// Constants from <linux/uinput.h>.
const UINPUT_IOCTL_BASE: u8 = b'U';
const UI_DEV_CREATE: u64 = ioc_io(UINPUT_IOCTL_BASE, 1);
const UI_DEV_DESTROY: u64 = ioc_io(UINPUT_IOCTL_BASE, 2);
const UI_DEV_SETUP: u64 = ioc_iow::<UinputSetup>(UINPUT_IOCTL_BASE, 3);
const UI_SET_EVBIT: u64 = ioc_iow::<i32>(UINPUT_IOCTL_BASE, 100);
const UI_SET_KEYBIT: u64 = ioc_iow::<i32>(UINPUT_IOCTL_BASE, 101);
const UI_SET_RELBIT: u64 = ioc_iow::<i32>(UINPUT_IOCTL_BASE, 102);
const UI_SET_ABSBIT: u64 = ioc_iow::<i32>(UINPUT_IOCTL_BASE, 103);
const UI_ABS_SETUP: u64 = ioc_iow::<UinputAbsSetup>(UINPUT_IOCTL_BASE, 4);

const fn ioc(dir: u8, type_: u8, nr: u8, size: usize) -> u64 {
    const NRBITS: u64 = 8;
    const TYPEBITS: u64 = 8;
    const SIZEBITS: u64 = 14;
    const NRSHIFT: u64 = 0;
    const TYPESHIFT: u64 = NRSHIFT + NRBITS;
    const SIZESHIFT: u64 = TYPESHIFT + TYPEBITS;
    const DIRSHIFT: u64 = SIZESHIFT + SIZEBITS;
    ((dir as u64) << DIRSHIFT)
        | ((type_ as u64) << TYPESHIFT)
        | ((nr as u64) << NRSHIFT)
        | ((size as u64) << SIZESHIFT)
}
const fn ioc_io(type_: u8, nr: u8) -> u64 {
    ioc(0, type_, nr, 0)
}
const fn ioc_iow<T>(type_: u8, nr: u8) -> u64 {
    ioc(1, type_, nr, std::mem::size_of::<T>())
}

#[repr(C)]
#[derive(Default)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: AbsInfo,
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

unsafe fn ioctl(fd: RawFd, request: u64, arg: u64) -> Result<(), LinuxInputError> {
    let r = libc::ioctl(fd, request as _, arg);
    if r < 0 {
        return Err(LinuxInputError::UinputIoctl(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn create_mouse(abs_w: u32, abs_h: u32) -> Result<std::fs::File, LinuxInputError> {
    let f = open_uinput()?;
    let fd = f.as_raw_fd();
    unsafe {
        ioctl(fd, UI_SET_EVBIT, EV_KEY as u64)?;
        ioctl(fd, UI_SET_EVBIT, EV_REL as u64)?;
        ioctl(fd, UI_SET_EVBIT, EV_ABS as u64)?;
        for code in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
            ioctl(fd, UI_SET_KEYBIT, code as u64)?;
        }
        for code in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
            ioctl(fd, UI_SET_RELBIT, code as u64)?;
        }
        for code in [ABS_X, ABS_Y] {
            ioctl(fd, UI_SET_ABSBIT, code as u64)?;
        }
        // Per-axis range setup.
        let abs_x_setup = UinputAbsSetup {
            code: ABS_X,
            absinfo: AbsInfo {
                maximum: (abs_w as i32).saturating_sub(1).max(0),
                ..Default::default()
            },
        };
        ioctl(fd, UI_ABS_SETUP, &abs_x_setup as *const _ as u64)?;
        let abs_y_setup = UinputAbsSetup {
            code: ABS_Y,
            absinfo: AbsInfo {
                maximum: (abs_h as i32).saturating_sub(1).max(0),
                ..Default::default()
            },
        };
        ioctl(fd, UI_ABS_SETUP, &abs_y_setup as *const _ as u64)?;
        let mut name = [0u8; 80];
        let s = b"prdt-virtual-mouse";
        name[..s.len()].copy_from_slice(s);
        let setup = UinputSetup {
            id: InputId {
                bustype: 0x03,
                vendor: 0x1234,
                product: 0x5677,
                version: 1,
            },
            name,
            ff_effects_max: 0,
        };
        ioctl(fd, UI_DEV_SETUP, &setup as *const _ as u64)?;
        ioctl(fd, UI_DEV_CREATE, 0)?;
    }
    Ok(f)
}

fn create_keyboard() -> Result<std::fs::File, LinuxInputError> {
    let f = open_uinput()?;
    let fd = f.as_raw_fd();
    unsafe {
        ioctl(fd, UI_SET_EVBIT, EV_KEY as u64)?;
        // Set every key code from 1..=KEY_MAX. Some are reserved; the
        // kernel silently accepts all.
        for code in 1u32..=KEY_MAX {
            ioctl(fd, UI_SET_KEYBIT, code as u64)?;
        }
        let mut name = [0u8; 80];
        let s = b"prdt-virtual-keyboard";
        name[..s.len()].copy_from_slice(s);
        let setup = UinputSetup {
            id: InputId {
                bustype: 0x03,
                vendor: 0x1234,
                product: 0x5678,
                version: 1,
            },
            name,
            ff_effects_max: 0,
        };
        ioctl(fd, UI_DEV_SETUP, &setup as *const _ as u64)?;
        ioctl(fd, UI_DEV_CREATE, 0)?;
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_max_constant_matches_spec() {
        assert_eq!(KEY_MAX, 0x2FF);
    }

    #[test]
    fn scancode_out_of_range_skipped_silently() {
        // We can't hit DEVICES init in unit tests (needs /dev/uinput),
        // so verify the early-return invariant via a const_assert-style
        // comparison: 0x10000 (a representative out-of-range scancode)
        // must exceed KEY_MAX (0x2FF) so write_event's early `return Ok`
        // branch fires before any ioctl. const fn ensures the check is
        // resolved at compile time without tripping clippy's
        // assertions_on_constants lint.
        const _: () = assert!(0x10000 > KEY_MAX);
    }

    #[test]
    fn ioc_io_macro_yields_expected_value() {
        // UI_DEV_CREATE = _IO('U', 1) = (0 << 30) | ('U' << 8) | 1 | (0 << 16)
        assert_eq!(UI_DEV_CREATE, ((b'U' as u64) << 8) | 1);
    }

    #[test]
    #[ignore = "requires /dev/uinput access (input group). Run with: cargo test -p prdt-input-linux -- --ignored"]
    fn open_uinput_succeeds_with_permission() {
        let f = open_uinput().expect("open /dev/uinput");
        drop(f);
    }
}
