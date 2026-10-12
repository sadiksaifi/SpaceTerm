//! Incremental private-IPC framing for serialized envelopes.
//!
//! The four-byte prefix is not part of the frame limit. Authentication metadata
//! is part of the serialized envelope and must fit inside that limit.

use crate::{ProtocolError, limits::MAX_FRAME_BYTES};

/// Decodes one frame per call without retaining subsequent frames.
///
/// An invalid prefix permanently fails this decoder. The caller must close the
/// connection rather than search the remaining bytes for another prefix.
pub struct FrameDecoder {
    max: usize,
    prefix: [u8; 4],
    prefix_len: usize,
    expected: Option<usize>,
    body: Vec<u8>,
    error: Option<ProtocolError>,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self {
            max: MAX_FRAME_BYTES,
            prefix: [0; 4],
            prefix_len: 0,
            expected: None,
            body: Vec::new(),
            error: None,
        }
    }
}

impl FrameDecoder {
    /// Creates an empty decoder at the hard frame ceiling.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns consumed bytes and at most one serialized envelope.
    pub fn push(&mut self, input: &[u8]) -> Result<(usize, Option<Vec<u8>>), ProtocolError> {
        let mut remaining = input;
        let frame = self.decode(&mut remaining)?;
        Ok((input.len() - remaining.len(), frame))
    }

    /// Creates a decoder with a reduced serialized-envelope limit.
    ///
    /// Returns [`ProtocolError::Malformed`] for zero and
    /// [`ProtocolError::LimitExceeded`] above the hard frame ceiling.
    pub fn with_limit(max: usize) -> Result<Self, ProtocolError> {
        if max == 0 {
            return Err(ProtocolError::Malformed);
        }
        if max > MAX_FRAME_BYTES {
            return Err(ProtocolError::LimitExceeded);
        }
        Ok(Self {
            max,
            ..Self::default()
        })
    }

    /// Consumes at most one frame, leaving subsequent bytes in `input`.
    ///
    /// Returns `None` when the prefix or body is incomplete. Prefix validation
    /// precedes body allocation. A zero length is malformed; a length above the
    /// configured limit exceeds admission bounds. After either error, further
    /// calls return the same error without consuming input.
    pub fn decode(&mut self, input: &mut &[u8]) -> Result<Option<Vec<u8>>, ProtocolError> {
        if let Some(error) = self.error {
            return Err(error);
        }

        if self.expected.is_none() {
            let count = (self.prefix.len() - self.prefix_len).min(input.len());
            self.prefix[self.prefix_len..self.prefix_len + count].copy_from_slice(&input[..count]);
            self.prefix_len += count;
            *input = &input[count..];
            if self.prefix_len < self.prefix.len() {
                return Ok(None);
            }

            let length = u32::from_be_bytes(self.prefix) as usize;
            self.prefix_len = 0;
            let error = if length == 0 {
                Some(ProtocolError::Malformed)
            } else if length > self.max {
                Some(ProtocolError::LimitExceeded)
            } else {
                None
            };
            if let Some(error) = error {
                self.error = Some(error);
                return Err(error);
            }

            self.expected = Some(length);
            self.body = Vec::with_capacity(length);
        }

        if let Some(expected) = self.expected {
            let count = (expected - self.body.len()).min(input.len());
            self.body.extend_from_slice(&input[..count]);
            *input = &input[count..];
            if self.body.len() == expected {
                self.expected = None;
                return Ok(Some(std::mem::take(&mut self.body)));
            }
        }
        Ok(None)
    }

    /// Bytes retained from an incomplete prefix or frame body.
    ///
    /// A fully parsed prefix is not counted. This is zero after a complete frame
    /// or an error, and never includes bytes from subsequent frames.
    pub fn buffered_bytes(&self) -> usize {
        self.prefix_len + self.body.len()
    }
}

/// Prefixes one nonempty serialized envelope with its big-endian byte length.
///
/// The hard 2 MiB ceiling includes authentication metadata in `envelope`, but not
/// the prefix. Empty envelopes are malformed; oversized envelopes exceed limits.
/// Callers must also enforce their connection's negotiated frame limit.
pub fn encode_frame(envelope: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if envelope.is_empty() {
        return Err(ProtocolError::Malformed);
    }
    if envelope.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::LimitExceeded);
    }
    let mut frame = Vec::with_capacity(envelope.len() + 4);
    frame.extend_from_slice(&(envelope.len() as u32).to_be_bytes());
    frame.extend_from_slice(envelope);
    Ok(frame)
}
