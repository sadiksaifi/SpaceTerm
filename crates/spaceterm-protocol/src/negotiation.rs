//! Independently checked newest-common-major selection.

use crate::ProtocolError;

/// Current and complete previous protocol profiles, newest first.
pub const SUPPORTED_MAJORS: [u32; 2] = [2, 1];

/// Stateless version selection; both peers verify the same advertisements.
pub struct Negotiator;

impl Negotiator {
    /// Selects the newest shared major from bounded, strictly descending advertisements.
    pub fn select(client: &[u32], server: &[u32]) -> Result<u32, ProtocolError> {
        validate_advertisement(client)?;
        validate_advertisement(server)?;
        client
            .iter()
            .copied()
            .find(|major| server.contains(major))
            .ok_or(ProtocolError::Incompatible)
    }

    /// Rejects a substituted or downgraded selection.
    pub fn verify(client: &[u32], server: &[u32], selected: u32) -> Result<(), ProtocolError> {
        if Self::select(client, server)? == selected {
            Ok(())
        } else {
            Err(ProtocolError::Negotiation)
        }
    }
}

pub(crate) fn validate_advertisement(majors: &[u32]) -> Result<(), ProtocolError> {
    if majors.is_empty()
        || majors.len() > 16
        || majors.contains(&0)
        || majors.windows(2).any(|pair| pair[0] <= pair[1])
    {
        return Err(ProtocolError::Malformed);
    }
    Ok(())
}
