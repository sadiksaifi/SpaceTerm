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

/// Authenticated normalized creation-default replay extension.
pub const CAPABILITY_NORMALIZED_CREATION_TOPOLOGY: u32 = 1;
/// Server-owned Git extension. Not advertised until the converter is installed.
pub const CAPABILITY_SERVER_GIT: u32 = 520_001;
/// Owning-server directory extension. Not advertised by the declaration freeze.
pub const CAPABILITY_SERVER_DIRECTORIES: u32 = 520_002;
/// Complete canonical-domain conversion and revision-exact resource contract.
pub const CAPABILITY_CANONICAL_DOMAIN: u32 = 520_003;
/// Exact Tab membership versions and Pane runtime failure classifications.
/// Frozen canonical-domain peers do not know these nested organization fields.
pub const CAPABILITY_ORGANIZATION_COMPARISONS: u32 = 520_004;

/// Fenced Pane presentation subscriptions and explicit connection context.
pub const CAPABILITY_TERMINAL_STREAMS: u32 = 520_005;

/// Content-free native bell and command-completion notices.
pub const CAPABILITY_NATIVE_NOTIFICATIONS: u32 = 520_006;

/// Native account lifecycle comparisons on paired local connections.
pub const CAPABILITY_ACCOUNT_RUNTIME: u32 = 520_007;

/// Revision-exact cell-addressed application pointer and wheel input.
pub const CAPABILITY_APPLICATION_MOUSE: u32 = 520_008;

/// Sequence-qualified runtime metadata independent of semantic content.
pub const CAPABILITY_RUNTIME_METADATA: u32 = 520_009;

/// Bounded fragmented metadata snapshots for small negotiated frames.
pub const CAPABILITY_RUNTIME_METADATA_FRAGMENTS: u32 = 520_010;

/// Exact originating runtime request and independent native completion correlation.
pub const CAPABILITY_NATIVE_PASTE_CORRELATION: u32 = 520_011;
/// Enhanced-only Copy, never an ordinary legacy key fallback.
pub const CAPABILITY_ENHANCED_COPY: u32 = 520_012;
/// Render-only owning-server platform, permission, and bind-scope comparisons.
pub const CAPABILITY_SERVER_MACHINE_FACTS: u32 = 520_013;

/// Additive envelope registration: tag, authenticated capability, permitted directions.
pub const EXTENSION_ENVELOPES: &[(u32, u32, bool, bool)] = &[
    (32, CAPABILITY_SERVER_GIT, true, false),
    (33, CAPABILITY_SERVER_GIT, false, true),
    (34, CAPABILITY_SERVER_DIRECTORIES, true, false),
    (35, CAPABILITY_SERVER_DIRECTORIES, false, true),
    (36, CAPABILITY_CANONICAL_DOMAIN, true, false),
    (37, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (38, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (39, CAPABILITY_CANONICAL_DOMAIN, true, true),
    (40, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (41, CAPABILITY_CANONICAL_DOMAIN, true, false),
    (42, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (43, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (44, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (45, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (46, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (47, CAPABILITY_CANONICAL_DOMAIN, false, true),
    (48, CAPABILITY_TERMINAL_STREAMS, true, true),
    (49, CAPABILITY_TERMINAL_STREAMS, true, false),
    (50, CAPABILITY_TERMINAL_STREAMS, false, true),
    (51, CAPABILITY_ACCOUNT_RUNTIME, true, false),
    (52, CAPABILITY_ACCOUNT_RUNTIME, false, true),
    (53, CAPABILITY_RUNTIME_METADATA, false, true),
    (54, CAPABILITY_RUNTIME_METADATA_FRAGMENTS, false, true),
];
