//! Major-specific payloads. The authenticated kind, not a claimed class, selects admission.

use crate::{
    ProtocolError,
    limits::{MAX_FRAME_BYTES, NegotiatedLimits},
    preflight,
    wire::{v1, v2},
};
use prost::Message;

/// Independently generated current and previous profiles.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Major1(v1::Message),
    Major2(v2::Message),
}

impl Payload {
    /// Major implemented by this payload's concrete DTO.
    pub fn major(&self) -> u32 {
        match self {
            Self::Major1(_) => 1,
            Self::Major2(_) => 2,
        }
    }

    /// Computes serialized payload length without allocating an encoded buffer.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Major1(value) => value.encoded_len(),
            Self::Major2(value) => value.encoded_len(),
        }
    }

    /// Encodes after length admission, then applies the same structural checks as receive.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.encode_with_limits(NegotiatedLimits::default())
    }

    /// Encodes under a connection's negotiated ceilings.
    pub fn encode_with_limits(&self, limits: NegotiatedLimits) -> Result<Vec<u8>, ProtocolError> {
        limits.validate()?;
        let length = self.encoded_len();
        if length > limits.frame_bytes {
            return Err(ProtocolError::LimitExceeded);
        }
        let bytes = match self {
            Self::Major1(value) => value.encode_to_vec(),
            Self::Major2(value) => value.encode_to_vec(),
        };
        admit(self.major(), &bytes, limits)?;
        crate::semantics::validate(self)?;
        Ok(bytes)
    }

    /// Decodes only after allocation-free structural, count, byte and nesting admission.
    pub fn decode(major: u32, bytes: &[u8]) -> Result<Self, ProtocolError> {
        Self::decode_with_limits(major, bytes, NegotiatedLimits::default())
    }

    /// Decodes under a connection's negotiated ceilings.
    pub fn decode_with_limits(
        major: u32,
        bytes: &[u8],
        limits: NegotiatedLimits,
    ) -> Result<Self, ProtocolError> {
        admit(major, bytes, limits)?;
        let value = match major {
            1 => v1::Message::decode(bytes)
                .map(Self::Major1)
                .map_err(|_| ProtocolError::Malformed),
            2 => v2::Message::decode(bytes)
                .map(Self::Major2)
                .map_err(|_| ProtocolError::Malformed),
            _ => Err(ProtocolError::Incompatible),
        }?;
        crate::semantics::validate(&value)?;
        Ok(value)
    }
}

fn admit(major: u32, bytes: &[u8], limits: NegotiatedLimits) -> Result<(), ProtocolError> {
    limits.validate()?;
    if bytes.len() > limits.frame_bytes.min(MAX_FRAME_BYTES) {
        return Err(ProtocolError::LimitExceeded);
    }
    let schema = match major {
        1 => preflight::SPACETERM_V1_MESSAGE,
        2 => preflight::SPACETERM_V2_MESSAGE,
        _ => return Err(ProtocolError::Incompatible),
    };
    // Count and grammar admission must precede generated decoding. A repeated
    // empty embedded message can otherwise allocate much more than its wire size.
    preflight::check(schema, bytes, limits)?;
    let tag = preflight::payload_tag(bytes)?;
    let maximum = match tag {
        14 => limits.frame_bytes,
        46 | 47 => limits.frame_bytes.min(1024 * 1024),
        16 | 43 => limits.resource_chunk_bytes.saturating_add(2048),
        18 | 19 => limits
            .clipboard_bytes
            .max(limits.paste_bytes)
            .saturating_add(2048),
        11 if nested_field(bytes, 5)?.is_some() => limits.paste_bytes.saturating_add(2048),
        _ => limits.management_bytes,
    };
    if bytes.len() > maximum {
        return Err(ProtocolError::LimitExceeded);
    }
    Ok(())
}

// Inspect a field in the single embedded payload without decoding it. The
// descriptor preflight has already validated its grammar and uniqueness.
fn nested_field(mut bytes: &[u8], target: u64) -> Result<Option<&[u8]>, ProtocolError> {
    preflight::varint(&mut bytes)?;
    let mut inner = preflight::length_delimited(&mut bytes)?;
    while !inner.is_empty() {
        let tag = preflight::varint(&mut inner)?;
        match tag & 7 {
            0 => {
                preflight::varint(&mut inner)?;
            }
            2 => {
                let value = preflight::length_delimited(&mut inner)?;
                if tag >> 3 == target {
                    return Ok(Some(value));
                }
            }
            _ => return Err(ProtocolError::Malformed),
        }
    }
    Ok(None)
}
