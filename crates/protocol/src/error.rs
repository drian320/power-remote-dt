/// Protocol-level error surface. Intentionally small and closed; add a
/// variant if a new failure mode appears rather than stuffing into a catch-all.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("packet too short: need >= {expected}, got {actual}")]
    PacketTooShort { expected: usize, actual: usize },

    #[error("bad magic: expected 0x{expected:02x}, got 0x{actual:02x}")]
    BadMagic { expected: u8, actual: u8 },

    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u8),

    #[error("unknown packet type: {0}")]
    UnknownPacketType(u8),

    #[error("unknown control kind: {0}")]
    UnknownControlKind(u8),

    #[error("unknown event kind: {0}")]
    UnknownEventKind(u8),

    #[error("payload length mismatch: header={header}, actual={actual}")]
    PayloadLengthMismatch { header: u32, actual: usize },

    #[error("bincode: {0}")]
    Bincode(#[from] bincode::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[test]
    fn error_display_is_stable() {
        let e = ProtocolError::BadMagic {
            expected: 0x52,
            actual: 0xAA,
        };
        assert_eq!(e.to_string(), "bad magic: expected 0x52, got 0xaa");

        let e = ProtocolError::PacketTooShort {
            expected: 16,
            actual: 3,
        };
        assert_eq!(e.to_string(), "packet too short: need >= 16, got 3");

        let _: ProtocolError = bincode::Error::from(Box::new(bincode::ErrorKind::SizeLimit)).into();
    }

    #[test]
    fn error_impls_std_error() {
        fn assert_is_error<E: std::error::Error + Send + Sync + 'static>() {}
        assert_is_error::<ProtocolError>();
        let _: fn(&ProtocolError, &mut fmt::Formatter<'_>) -> fmt::Result =
            <ProtocolError as fmt::Debug>::fmt;
    }
}
