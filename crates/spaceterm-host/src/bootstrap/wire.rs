//! Closed account Bootstrap messages. Remote paths are forwarding descriptions only.

use super::setup::{BootstrapAccountFacts, BootstrapAccountRegistration};
use crate::{account_service::ServiceFailure, native_deployment::contracts::NativeRequestId};
use serde::{Deserialize, Serialize};
use spaceterm_core::{
    operations::RequestDigest,
    pairing::{EnrollmentProposal, PairingKeys},
};

/// Maximum size of one account or owner frame.
pub const FRAME_LIMIT: usize = 64 * 1024;
/// Maximum size of one signed-artifact transfer frame.
pub const TRANSFER_CHUNK: usize = 60 * 1024;

/// Fixed account operations. No operation carries a local path, shell command or environment.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AccountRequest {
    /// Read actual account registration and supervisor state.
    Inspect,
    /// Allocate one original installation request without submitting it.
    Reserve,
    /// Retain or recover one exact public owner proposal before publication.
    ReserveOwner {
        /// Public keys and nonce supplied by the native credential owner.
        proposal: EnrollmentProposal,
    },
    /// Receive and authenticate the exact signed artifact before installing.
    Install {
        /// Previously reserved native identity.
        request: NativeRequestId,
        /// Exact signed archive digest.
        artifact: [u8; 32],
        /// Bounded byte count for the following transfer frames.
        transfer_bytes: u64,
    },
    /// Authorize only the original public claim on the observed running registration.
    AuthorizeOwner {
        /// Previously reserved native identity.
        request: NativeRequestId,
        /// Exact original registration comparison.
        expected: BootstrapAccountRegistration,
        /// Public native claim, without credentials or bearers.
        proposal: EnrollmentProposal,
    },
    /// Read the canonical digest of an already retained owner request.
    OwnerBinding {
        /// Original native request identity.
        request: NativeRequestId,
        /// Exact original public proposal.
        proposal: EnrollmentProposal,
    },
    /// Read the result of an original install or owner request without resubmitting.
    Resolve {
        /// Original native request identity.
        request: NativeRequestId,
        /// Original approved archive identity.
        artifact: [u8; 32],
    },
    /// Read the original owner proposal matching native public keys.
    RetainedOwner {
        /// Candidate public keys, never a native credential locator.
        keys: PairingKeys,
    },
    /// Separately authorized Start of the exact deliberately stopped registration.
    Start {
        /// Exact stopped-registration comparison.
        expected: BootstrapAccountRegistration,
    },
    /// Transfer the channel to the existing account-owner enrollment endpoint.
    OwnerChannel,
}
/// Bounded native facts, with only a remote SSH forwarding description.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountObservation {
    /// Actual account, registration, service state and linger.
    pub facts: BootstrapAccountFacts,
    /// Never an IPC directory authority on the client. Used only as an SSH remote socket.
    pub remote_socket: Option<String>,
}
/// Fixed operation results. Failures contain classifications, not native output.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AccountReply {
    /// Read-only account observation.
    Observation(AccountObservation),
    /// Previously reserved original request identity.
    Reserved(NativeRequestId),
    /// Original claim-bound native authorization record.
    Owner {
        /// Actual registration after original-request admission.
        observation: AccountObservation,
        /// Canonical account/build/server/request/claim digest.
        digest: RequestDigest,
    },
    /// Original retained public proposal, or no matching request.
    RetainedOwner(Option<(NativeRequestId, EnrollmentProposal)>),
    /// Content-free native failure.
    Failure(ServiceFailure),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_wire_refuses_commands_paths_and_unknown_nested_registration_fields() {
        for bytes in [
            br#"{"Command":{"command":"untrusted"}}"#.as_slice(),
            br#"{"Reserve":{"path":"untrusted"}}"#.as_slice(),
            br#"{"Reserve":{"environment":{}}}"#.as_slice(),
        ] {
            assert!(serde_json::from_slice::<AccountRequest>(bytes).is_err());
        }
        let request = AccountRequest::Start {
            expected: BootstrapAccountRegistration::Stopped {
                pin: spaceterm_protocol::authentication::ServerPin {
                    identity: [1; 16],
                    public_key: [2; 32],
                },
                generation: 1,
            },
        };
        let mut value = serde_json::to_value(request).unwrap();
        value["Start"]["expected"]["Stopped"]["roots"] = serde_json::json!("untrusted");
        assert!(serde_json::from_value::<AccountRequest>(value).is_err());
    }

    #[test]
    fn bootstrap_wire_rejects_zero_server_pin() {
        let request = AccountRequest::Start {
            expected: BootstrapAccountRegistration::Stopped {
                pin: spaceterm_protocol::authentication::ServerPin {
                    identity: [0; 16],
                    public_key: [2; 32],
                },
                generation: 1,
            },
        };
        assert!(
            serde_json::from_slice::<AccountRequest>(&serde_json::to_vec(&request).unwrap())
                .is_err()
        );
    }
}
