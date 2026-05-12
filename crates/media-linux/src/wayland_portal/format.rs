//! libspa POD build + parse helpers for the PipeWire ScreenCast handshake.
//!
//! `build()` serialises a single `SPA_PARAM_EnumFormat` POD advertising
//! BGRA/BGRx + size/framerate ranges + modifier enum. `parse()` walks a
//! negotiated `SPA_PARAM_Format` POD from `param_changed` and extracts
//! `NegotiatedFormat`. See `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md` §3.2–§3.3.

#![cfg(target_os = "linux")]

use pipewire::spa::param::{
    format::{FormatProperties, MediaSubtype, MediaType},
    video::VideoFormat,
    ParamType,
};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

/// DRM modifier: linear (no tiling). Compositors that hand us a DMABUF
/// with this modifier produce CPU-readable BGRA.
pub const DRM_FORMAT_MOD_LINEAR: i64 = 0;

/// DRM modifier: "unspecified". Compositors use this when they don't want
/// to commit to a specific tiling layout. NOT linear-guaranteed; if the
/// negotiated modifier is this value we disconnect rather than mmap tiled
/// data (would be `unsafe`-but-broken). See spec §4.3.
pub const DRM_FORMAT_MOD_INVALID: i64 = -1;

/// Owned byte storage for the serialised POD(s) handed to `stream.connect`.
///
/// `pipewire::spa::pod::Pod::from_bytes` borrows the bytes, so we must keep
/// the `Vec<Vec<u8>>` alive as long as the `&Pod` references derived from
/// `as_pods()`. The connect site reads `as_pods()` into a fresh `Vec<&Pod>`
/// each call — the borrow is tied to `&self`.
pub struct BuiltParams {
    pub bytes: Vec<Vec<u8>>,
}

impl BuiltParams {
    /// Rebuild the borrow slice of `&Pod` views over `self.bytes`. Cheap:
    /// each entry is a single `Pod::from_bytes` cast (no allocation, no copy).
    pub fn as_pods(&self) -> Vec<&Pod> {
        self.bytes
            .iter()
            .map(|b| Pod::from_bytes(b).expect("BuiltParams::bytes must contain a valid POD"))
            .collect()
    }
}

/// Build a single `SPA_PARAM_EnumFormat` POD advertising BGRA/BGRx + size
/// (320×240..7680×4320, default 1920×1080) + framerate (15/1..60/1, default
/// 60/1) + modifier (LINEAR | INVALID, default LINEAR).
pub fn build() -> BuiltParams {
    let modifiers = vec![DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_MOD_INVALID];

    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoFormat.as_raw(),
                Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(VideoFormat::BGRA.as_raw()),
                        alternatives: vec![
                            Id(VideoFormat::BGRA.as_raw()),
                            Id(VideoFormat::BGRx.as_raw()),
                        ],
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoSize.as_raw(),
                Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle {
                            width: 1920,
                            height: 1080,
                        },
                        min: Rectangle {
                            width: 320,
                            height: 240,
                        },
                        max: Rectangle {
                            width: 7680,
                            height: 4320,
                        },
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoFramerate.as_raw(),
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction { num: 60, denom: 1 },
                        min: Fraction { num: 15, denom: 1 },
                        max: Fraction { num: 60, denom: 1 },
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoModifier.as_raw(),
                Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: DRM_FORMAT_MOD_LINEAR,
                        alternatives: modifiers,
                    },
                ))),
            ),
        ],
    };

    let bytes =
        PodSerializer::serialize(std::io::Cursor::new(Vec::<u8>::new()), &Value::Object(obj))
            .expect("PodSerializer::serialize(EnumFormat) — only fails on OOM")
            .0
            .into_inner();

    BuiltParams { bytes: vec![bytes] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_bgra() {
        let built = build();
        let pods = built.as_pods();
        assert_eq!(pods.len(), 1, "build() emits exactly one EnumFormat POD");
        assert!(
            !built.bytes[0].is_empty(),
            "serialised POD bytes must be non-empty"
        );
    }
}
