//! Hard admission ceilings. Negotiation can reduce, but never increase, these limits.

mod hard;
pub use hard::*;

/// Peer-agreed ceilings. Construction rejects zero and values above hard limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedLimits {
    pub frame_bytes: usize,
    pub management_bytes: usize,
    pub semantic_rows: usize,
    pub semantic_cells: usize,
    pub resource_chunk_bytes: usize,
    pub concurrent_transfers: usize,
    pub paste_bytes: usize,
    pub clipboard_bytes: usize,
}

impl Default for NegotiatedLimits {
    fn default() -> Self {
        Self {
            frame_bytes: MAX_FRAME_BYTES,
            management_bytes: MAX_MANAGEMENT_BYTES,
            semantic_rows: MAX_SEMANTIC_ROWS,
            semantic_cells: MAX_SEMANTIC_CELLS,
            resource_chunk_bytes: MAX_RESOURCE_CHUNK_BYTES,
            concurrent_transfers: MAX_CONCURRENT_TRANSFERS,
            paste_bytes: MAX_PASTE_BYTES,
            clipboard_bytes: MAX_CLIPBOARD_BYTES,
        }
    }
}

impl NegotiatedLimits {
    /// Validates all values, including relationships needed by authenticated framing.
    pub fn validate(self) -> Result<Self, crate::ProtocolError> {
        let hard = Self::default();
        for (value, maximum) in [
            (self.frame_bytes, hard.frame_bytes),
            (self.management_bytes, hard.management_bytes),
            (self.semantic_rows, hard.semantic_rows),
            (self.semantic_cells, hard.semantic_cells),
            (self.resource_chunk_bytes, hard.resource_chunk_bytes),
            (self.concurrent_transfers, hard.concurrent_transfers),
            (self.paste_bytes, hard.paste_bytes),
            (self.clipboard_bytes, hard.clipboard_bytes),
        ] {
            if value == 0 {
                return Err(crate::ProtocolError::Malformed);
            }
            if value > maximum {
                return Err(crate::ProtocolError::LimitExceeded);
            }
        }
        if self.frame_bytes < 1024 || self.management_bytes > self.frame_bytes {
            return Err(crate::ProtocolError::Malformed);
        }
        Ok(self)
    }

    /// Reduces each ceiling independently. Neither peer can raise the other's limit.
    pub fn intersect(self, other: Self) -> Result<Self, crate::ProtocolError> {
        self.validate()?;
        other.validate()?;
        Self {
            frame_bytes: self.frame_bytes.min(other.frame_bytes),
            management_bytes: self.management_bytes.min(other.management_bytes),
            semantic_rows: self.semantic_rows.min(other.semantic_rows),
            semantic_cells: self.semantic_cells.min(other.semantic_cells),
            resource_chunk_bytes: self.resource_chunk_bytes.min(other.resource_chunk_bytes),
            concurrent_transfers: self.concurrent_transfers.min(other.concurrent_transfers),
            paste_bytes: self.paste_bytes.min(other.paste_bytes),
            clipboard_bytes: self.clipboard_bytes.min(other.clipboard_bytes),
        }
        .validate()
    }

    pub(crate) fn to_wire(self) -> crate::wire::common::Limits {
        crate::wire::common::Limits {
            frame_bytes: self.frame_bytes as u32,
            management_bytes: self.management_bytes as u32,
            semantic_rows: self.semantic_rows as u32,
            semantic_cells: self.semantic_cells as u32,
            resource_chunk_bytes: self.resource_chunk_bytes as u32,
            concurrent_transfers: self.concurrent_transfers as u32,
            paste_bytes: self.paste_bytes as u32,
            clipboard_bytes: self.clipboard_bytes as u32,
        }
    }

    pub(crate) fn from_wire(
        value: &crate::wire::common::Limits,
    ) -> Result<Self, crate::ProtocolError> {
        Self {
            frame_bytes: value.frame_bytes as usize,
            management_bytes: value.management_bytes as usize,
            semantic_rows: value.semantic_rows as usize,
            semantic_cells: value.semantic_cells as usize,
            resource_chunk_bytes: value.resource_chunk_bytes as usize,
            concurrent_transfers: value.concurrent_transfers as usize,
            paste_bytes: value.paste_bytes as usize,
            clipboard_bytes: value.clipboard_bytes as usize,
        }
        .validate()
    }
}
