//! Pairing-owned pure codecs. Native authorization and enrollment/revocation
//! orchestration are not supplied by this declaration freeze.

use crate::ProtocolError;
use crate::authentication::Principal;
use crate::limits::NegotiatedLimits;
use crate::wire::{common, pairing};

/// The pairing owner validates exact target, operation/digest, direction,
/// receipt bytes and restricted purpose. No ordinary connection is returned.
pub trait RestrictedExtensionCodec: Send + Sync {
    fn encode_restricted_extension(
        &self,
        payload: &pairing::RestrictedExtension,
        principal: Principal,
        direction: common::Direction,
        limits: NegotiatedLimits,
    ) -> Result<Vec<u8>, ProtocolError>;
    fn decode_restricted_extension(
        &self,
        bytes: &[u8],
        principal: Principal,
        direction: common::Direction,
        limits: NegotiatedLimits,
    ) -> Result<pairing::RestrictedExtension, ProtocolError>;
}
