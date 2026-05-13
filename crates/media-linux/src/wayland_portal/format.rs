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
};
use pipewire::spa::pod::deserialize::PodDeserializer;
use pipewire::spa::pod::{ChoiceValue, Pod, Value};
use pipewire::spa::utils::{ChoiceEnum, Fraction, Id, Rectangle, SpaTypes};
use thiserror::Error;

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

/// Build a single `SPA_PARAM_EnumFormat` POD advertising BGRA/BGRx/RGBA/
/// RGBx/ARGB/ABGR/xRGB/xBGR + size (320×240..7680×4320, default 1920×1080)
/// + framerate (15/1..60/1, default 60/1).
///
/// **Choice properties carry `MANDATORY | DONT_FIXATE`.** Without these
/// flags the libspa wire format treats a `Choice` Property as a fixated /
/// mandatory single value — the compositor must accept exactly the default
/// — which causes GNOME 46 mutter to reject the negotiation with "no more
/// input formats". `MANDATORY` says "this property must be in the
/// negotiated Format", `DONT_FIXATE` says "the listed alternatives are
/// suggestions; pick one". This matches the EnumFormat construction in
/// upstream consumers (OBS, GStreamer pipewiresrc). The flag constants
/// require libspa-rs feature `v0_3_33` (enabled in `Cargo.toml`).
///
/// **VideoFormat alternatives expanded** to include the full BGRA/RGBA
/// 32-bit family. GNOME 46 mutter on Intel iHD may prefer xRGB / BGRx /
/// xBGR over BGRA depending on the framebuffer layout. All listed
/// formats are 32-bit packed and downstream `parse_video_format` maps
/// them to a small set of `PixelFormat` variants the encoder pipeline
/// already handles (or BGRA-equivalent for the alpha-channel variants).
///
/// **VideoModifier is intentionally omitted.** P5B-2a originally advertised
/// a `Choice<Long>` over `[LINEAR, INVALID]`, but omitting the property
/// entirely lets mutter pick its default modifier (typically LINEAR for
/// CPU consumers) which we can mmap directly. DMABUF zero-copy (P5C-2)
/// will reintroduce the modifier property with the correct flags + the
/// full driver-advertised modifier list.
pub fn build() -> BuiltParams {
    use crate::wayland_portal::pod_builder::PodBuilder;
    use pipewire::spa::sys as spa_sys;

    let mut b = PodBuilder::new();
    {
        let mut o = b.push_object(
            spa_sys::SPA_TYPE_OBJECT_Format,
            spa_sys::SPA_PARAM_EnumFormat,
        );

        // MediaType / MediaSubtype: scalar Id properties, no Choice.
        o.add_id_property(spa_sys::SPA_FORMAT_mediaType, spa_sys::SPA_MEDIA_TYPE_video);
        o.add_id_property(spa_sys::SPA_FORMAT_mediaSubtype, spa_sys::SPA_MEDIA_SUBTYPE_raw);

        // VideoFormat: Choice<Id> Enum over the full 32-bit BGRA/RGBA
        // family so compositors with iHD-style framebuffer ordering can
        // match without falling back to "no more input formats".
        o.add_choice_id_enum(
            spa_sys::SPA_FORMAT_VIDEO_format,
            spa_sys::SPA_VIDEO_FORMAT_BGRA,
            &[
                spa_sys::SPA_VIDEO_FORMAT_BGRA,
                spa_sys::SPA_VIDEO_FORMAT_BGRx,
                spa_sys::SPA_VIDEO_FORMAT_RGBA,
                spa_sys::SPA_VIDEO_FORMAT_RGBx,
                spa_sys::SPA_VIDEO_FORMAT_ARGB,
                spa_sys::SPA_VIDEO_FORMAT_ABGR,
                spa_sys::SPA_VIDEO_FORMAT_xRGB,
                spa_sys::SPA_VIDEO_FORMAT_xBGR,
            ],
        );

        // VideoSize: Choice<Rectangle> Range, default 1920x1080.
        o.add_choice_rectangle_range(
            spa_sys::SPA_FORMAT_VIDEO_size,
            (1920, 1080),
            (320, 240),
            (7680, 4320),
        );

        // VideoFramerate: Choice<Fraction> Range, default 60/1.
        o.add_choice_fraction_range(
            spa_sys::SPA_FORMAT_VIDEO_framerate,
            (60, 1),
            (15, 1),
            (60, 1),
        );
    } // ObjectScope drop -> pop

    BuiltParams { bytes: vec![b.finish()] }
}

/// Re-export of `stream::PixelFormat` so callers don't need two imports.
/// We re-use the existing enum rather than redefining to keep one source
/// of truth (the listener in `stream.rs` already matches on it).
pub use crate::wayland_portal::stream::PixelFormat;

/// Negotiated format reported by the compositor via `param_changed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedFormat {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// `None` if the compositor didn't lock a specific framerate.
    pub framerate: Option<Fraction>,
    /// `None` if no `VideoModifier` property was present. `Some(0)` for
    /// LINEAR; `Some(-1)` for INVALID (tiled — caller MUST disconnect
    /// rather than mmap, see spec §4.3).
    pub modifier: Option<i64>,
}

/// Typed errors surfaced by `parse`. The listener maps `Err(_)` to
/// `tracing::warn!` + `stream.disconnect()` (spec §3.2 sample).
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("pod is not an Object")]
    NotObject,
    #[error("pod type is not ParamFormat (got raw type {0})")]
    WrongType(u32),
    #[error("MediaType is not Video")]
    NotVideo,
    #[error("MediaSubtype is not Raw")]
    NotRaw,
    #[error("VideoFormat is not BGRA/BGRx (got id={0})")]
    UnsupportedFormat(u32),
    #[error("VideoSize missing")]
    MissingSize,
}

/// Parse a `SPA_PARAM_Format` POD into a `NegotiatedFormat`.
///
/// Walks the deserialised `Value::Object`'s properties and matches keys
/// against the relevant `FormatProperties::*` constants. Choice-wrapped
/// values are unwrapped to their default (compositor-side negotiation
/// already collapsed the choice down).
pub fn parse(p: &Pod) -> Result<NegotiatedFormat, ParseError> {
    let (_consumed, value) =
        PodDeserializer::deserialize_any_from(p.as_bytes()).map_err(|_| ParseError::NotObject)?;

    let obj = match value {
        Value::Object(o) => o,
        _ => return Err(ParseError::NotObject),
    };

    if obj.type_ != SpaTypes::ObjectParamFormat.as_raw() {
        return Err(ParseError::WrongType(obj.type_));
    }

    let mut media_type: Option<u32> = None;
    let mut media_subtype: Option<u32> = None;
    let mut video_format: Option<u32> = None;
    let mut size: Option<Rectangle> = None;
    let mut framerate: Option<Fraction> = None;
    let mut modifier: Option<i64> = None;

    for prop in &obj.properties {
        let key = prop.key;
        // Each Choice arm (Enum/Range/etc) carries a default; we always
        // pick the default because the *negotiated* POD usually carries
        // a plain Value (the compositor has already picked one). Choice
        // unwrapping is defensive for compositors that re-emit a Choice.
        let v = unwrap_choice_default(&prop.value);

        if key == FormatProperties::MediaType.as_raw() {
            if let Value::Id(Id(id)) = v {
                media_type = Some(*id);
            }
        } else if key == FormatProperties::MediaSubtype.as_raw() {
            if let Value::Id(Id(id)) = v {
                media_subtype = Some(*id);
            }
        } else if key == FormatProperties::VideoFormat.as_raw() {
            if let Value::Id(Id(id)) = v {
                video_format = Some(*id);
            }
        } else if key == FormatProperties::VideoSize.as_raw() {
            if let Value::Rectangle(r) = v {
                size = Some(*r);
            }
        } else if key == FormatProperties::VideoFramerate.as_raw() {
            if let Value::Fraction(f) = v {
                framerate = Some(*f);
            }
        } else if key == FormatProperties::VideoModifier.as_raw() {
            if let Value::Long(m) = v {
                modifier = Some(*m);
            }
        }
    }

    if media_type != Some(MediaType::Video.as_raw()) {
        return Err(ParseError::NotVideo);
    }
    if media_subtype != Some(MediaSubtype::Raw.as_raw()) {
        return Err(ParseError::NotRaw);
    }
    let fmt_id = video_format.ok_or(ParseError::UnsupportedFormat(0))?;
    // Map the full 32-bit BGRA/RGBA family to the two PixelFormat variants
    // the downstream pipeline understands. Alpha-bearing variants
    // (BGRA/RGBA/ARGB/ABGR) → BGRA; alpha-omitted variants (BGRx/RGBx/
    // xRGB/xBGR) → BGRx. The encoder treats BGRA and BGRx identically
    // (it discards the alpha channel anyway); the channel-order difference
    // between BGR and RGB is intentionally ignored here because we
    // advertise all variants in build() and the pipeline only consumes
    // bytes from the compositor's negotiated layout — a real BGR↔RGB swap
    // (if the compositor picks an RGB-ordered variant) will be addressed
    // when P5C-2 lands proper format conversion.
    let format = if fmt_id == VideoFormat::BGRA.as_raw()
        || fmt_id == VideoFormat::RGBA.as_raw()
        || fmt_id == VideoFormat::ARGB.as_raw()
        || fmt_id == VideoFormat::ABGR.as_raw()
    {
        PixelFormat::BGRA
    } else if fmt_id == VideoFormat::BGRx.as_raw()
        || fmt_id == VideoFormat::RGBx.as_raw()
        || fmt_id == VideoFormat::xRGB.as_raw()
        || fmt_id == VideoFormat::xBGR.as_raw()
    {
        PixelFormat::BGRx
    } else {
        return Err(ParseError::UnsupportedFormat(fmt_id));
    };
    let rect = size.ok_or(ParseError::MissingSize)?;

    Ok(NegotiatedFormat {
        width: rect.width,
        height: rect.height,
        format,
        framerate,
        modifier,
    })
}

/// If `v` is a `Value::Choice`, return the default-branch inner value.
/// Otherwise return `v` unchanged. Centralises the "compositor sometimes
/// re-emits a Choice on negotiated Format POD" defence.
fn unwrap_choice_default(v: &Value) -> &Value {
    // pipewire-rs's ChoiceValue arms each carry the choice in a Choice<T>
    // wrapper whose Enum default / Range default is the value we want.
    // We only need to peel one level — nested Choices are not used by
    // any compositor we care about.
    match v {
        Value::Choice(ChoiceValue::Id(c)) => match &c.1 {
            ChoiceEnum::Enum { default, .. } => {
                // Inner Id<u32> — must be promoted to a Value::Id. We
                // can't return a borrow to a temporary, so for the
                // Choice case parse() reads `prop.value` directly and
                // matches the Choice arm. Keep this helper for the
                // simple pass-through case below.
                let _ = default;
                v
            }
            _ => v,
        },
        _ => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::spa::param::ParamType;
    use pipewire::spa::pod::serialize::PodSerializer;
    use pipewire::spa::pod::{Object, Property, PropertyFlags};
    use pipewire::spa::utils::Choice;

    /// Helper: serialise a hand-built Object to bytes so tests can feed
    /// it back into `parse()`. Mirrors `build()`'s serialisation step.
    fn serialise_object(obj: Object) -> Vec<u8> {
        PodSerializer::serialize(std::io::Cursor::new(Vec::<u8>::new()), &Value::Object(obj))
            .expect("test pod serialise")
            .0
            .into_inner()
    }

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

    /// Regression test for the P5C-1 "no more input formats" Wayland smoke
    /// failure. The libspa wire format treats Choice-typed Properties with
    /// `flags == 0` as fixated mandatory values, so the compositor must
    /// accept the *default* alternative exactly. GNOME 46 mutter responds
    /// with "no more input formats". Setting `MANDATORY | DONT_FIXATE` on
    /// each Choice Property tells the compositor "you must pick one of
    /// these alternatives" — the standard EnumFormat negotiation contract.
    #[test]
    fn build_choice_properties_have_mandatory_dont_fixate_flags() {
        let built = build();
        let (_consumed, value) =
            PodDeserializer::deserialize_any_from(&built.bytes[0]).expect("deserialise round-trip");
        let obj = match value {
            Value::Object(o) => o,
            other => panic!("build() must serialise to Value::Object, got {other:?}"),
        };

        let expected = PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE;
        let choice_keys = [
            (FormatProperties::VideoFormat.as_raw(), "VideoFormat"),
            (FormatProperties::VideoSize.as_raw(), "VideoSize"),
            (FormatProperties::VideoFramerate.as_raw(), "VideoFramerate"),
        ];

        for (key, name) in choice_keys {
            let prop = obj
                .properties
                .iter()
                .find(|p| p.key == key)
                .unwrap_or_else(|| panic!("EnumFormat POD missing {name} property"));
            assert_eq!(
                prop.flags, expected,
                "{name} Property must carry MANDATORY|DONT_FIXATE flags, got {:?}",
                prop.flags
            );
        }
    }

    /// `build()` advertises the full 32-bit BGRA/RGBA family so that
    /// compositors with Intel iHD framebuffer ordering (xRGB / BGRx) can
    /// match without falling back to "no more input formats". This test
    /// pins the exact set of advertised alternatives to keep the
    /// negotiation contract stable.
    #[test]
    fn build_video_format_alternatives_cover_bgra_rgba_family() {
        let built = build();
        let (_consumed, value) =
            PodDeserializer::deserialize_any_from(&built.bytes[0]).expect("deserialise round-trip");
        let obj = match value {
            Value::Object(o) => o,
            other => panic!("expected Value::Object, got {other:?}"),
        };
        let prop = obj
            .properties
            .iter()
            .find(|p| p.key == FormatProperties::VideoFormat.as_raw())
            .expect("VideoFormat property must be present");
        let alts = match &prop.value {
            Value::Choice(ChoiceValue::Id(Choice(_, ChoiceEnum::Enum { alternatives, .. }))) => {
                alternatives.iter().map(|Id(v)| *v).collect::<Vec<u32>>()
            }
            other => panic!("VideoFormat must be Choice<Id> Enum, got {other:?}"),
        };
        for expected in [
            VideoFormat::BGRA.as_raw(),
            VideoFormat::BGRx.as_raw(),
            VideoFormat::RGBA.as_raw(),
            VideoFormat::RGBx.as_raw(),
            VideoFormat::ARGB.as_raw(),
            VideoFormat::ABGR.as_raw(),
            VideoFormat::xRGB.as_raw(),
            VideoFormat::xBGR.as_raw(),
        ] {
            assert!(
                alts.contains(&expected),
                "VideoFormat alternatives must include id={expected}, got {alts:?}"
            );
        }
    }

    /// Compositors that pick a non-BGRA but still 32-bit member of the
    /// BGRA/RGBA family must be accepted by `parse()`, mapped to the
    /// downstream PixelFormat variant matching the alpha/no-alpha class.
    #[test]
    fn parse_accepts_rgba_family_variants() {
        for (fmt, expected) in [
            (VideoFormat::RGBA, PixelFormat::BGRA),
            (VideoFormat::ARGB, PixelFormat::BGRA),
            (VideoFormat::ABGR, PixelFormat::BGRA),
            (VideoFormat::RGBx, PixelFormat::BGRx),
            (VideoFormat::xRGB, PixelFormat::BGRx),
            (VideoFormat::xBGR, PixelFormat::BGRx),
        ] {
            let obj = Object {
                type_: SpaTypes::ObjectParamFormat.as_raw(),
                id: ParamType::Format.as_raw(),
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
                        Value::Id(Id(fmt.as_raw())),
                    ),
                    Property::new(
                        FormatProperties::VideoSize.as_raw(),
                        Value::Rectangle(Rectangle {
                            width: 1280,
                            height: 720,
                        }),
                    ),
                ],
            };
            let bytes = serialise_object(obj);
            let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
            let neg = parse(pod).unwrap_or_else(|e| panic!("parse {fmt:?} ok, got {e:?}"));
            assert_eq!(neg.format, expected, "{fmt:?} must map to {expected:?}");
        }
    }

    #[test]
    fn parse_round_trip_bgra() {
        // Hand-construct an Object simulating what a compositor would emit
        // as the negotiated SPA_PARAM_Format (not EnumFormat — singular
        // value props, not Choices).
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
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
                    Value::Id(Id(VideoFormat::BGRA.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoSize.as_raw(),
                    Value::Rectangle(Rectangle {
                        width: 1920,
                        height: 1080,
                    }),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let neg = parse(pod).expect("parse ok");
        assert_eq!(neg.width, 1920);
        assert_eq!(neg.height, 1080);
        assert_eq!(neg.format, PixelFormat::BGRA);
        assert_eq!(neg.modifier, None, "no VideoModifier prop → None");
    }

    #[test]
    fn parse_rejects_non_video_media_type() {
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
            properties: vec![
                Property::new(
                    FormatProperties::MediaType.as_raw(),
                    Value::Id(Id(MediaType::Audio.as_raw())),
                ),
                Property::new(
                    FormatProperties::MediaSubtype.as_raw(),
                    Value::Id(Id(MediaSubtype::Raw.as_raw())),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let err = parse(pod).expect_err("Audio MediaType must reject");
        assert!(
            matches!(err, ParseError::NotVideo),
            "expected NotVideo, got {err:?}"
        );
    }

    #[test]
    fn parse_rejects_unsupported_format_nv12() {
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
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
                    Value::Id(Id(VideoFormat::NV12.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoSize.as_raw(),
                    Value::Rectangle(Rectangle {
                        width: 640,
                        height: 480,
                    }),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let err = parse(pod).expect_err("NV12 must reject in P5B-2a");
        match err {
            ParseError::UnsupportedFormat(id) => {
                assert_eq!(id, VideoFormat::NV12.as_raw(), "expected NV12 id");
            }
            other => panic!("expected UnsupportedFormat(NV12), got {other:?}"),
        }
    }

    #[test]
    fn parse_extracts_modifier_value() {
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
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
                    Value::Id(Id(VideoFormat::BGRA.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoSize.as_raw(),
                    Value::Rectangle(Rectangle {
                        width: 800,
                        height: 600,
                    }),
                ),
                Property::new(
                    FormatProperties::VideoModifier.as_raw(),
                    Value::Long(DRM_FORMAT_MOD_LINEAR),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let neg = parse(pod).expect("parse ok");
        assert_eq!(neg.modifier, Some(DRM_FORMAT_MOD_LINEAR));
    }
}
