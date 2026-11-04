//! The codes the service lane and the ledger carry. They are the CLI output contract's codes, so a
//! client of any major reads the same classification.
use crate::CoreError;
use crate::direct::DirectError;

/// A paired caller asked for a Network Access change that keeps no connected Paired Device's
/// route. Only the server's own machine makes that change.
pub const DISCONNECTS_EVERY_DEVICE: &str = "disconnects_every_device";

/// The contract code of a server failure.
pub fn code(error: &CoreError) -> &'static str {
    match error {
        CoreError::Direct(error) => direct_code(error),
        CoreError::NotSetUp => "not_set_up",
        CoreError::ServiceUnavailable => "service_unavailable",
        CoreError::ServiceBusy => "service_busy",
        CoreError::NotInstalled => "not_installed",
        CoreError::Stopped => "stopped",
        CoreError::LoginItemOff => "login_item_off",
        CoreError::Changed => "changed",
        CoreError::ActivationUnresolved => "activation_unresolved",
        CoreError::RuntimeUnavailable => "candidate_unverified",
        CoreError::CandidateMissing => "no_candidate",
        CoreError::AuthorizationRequired => "authorization_required",
        CoreError::NotRunning => "not_running",
        CoreError::StorageUnavailable => "storage_unavailable",
        CoreError::IncompatibleState => "incompatible_state",
        CoreError::UnsafeStorage => "unsafe_storage",
        CoreError::AuthenticationRefused => "authentication_refused",
        CoreError::IncompatibleProtocol(_) => "incompatible_protocol",
        CoreError::CandidateChanged => "candidate_changed",
        CoreError::Protocol => "protocol_error",
        CoreError::StartupFailed => "startup_failed",
        CoreError::Controlled | CoreError::StaleControl => "controlled",
        CoreError::SessionUnavailable => "session_ended",
        CoreError::Unsupported => "unsupported",
        CoreError::DirectoryUnavailable => "directory_unavailable",
        CoreError::CleanupUnresolved => "cleanup_unresolved",
        CoreError::Capacity => "capacity",
    }
}

fn direct_code(error: &DirectError) -> &'static str {
    match error {
        DirectError::Transport => "unreachable",
        DirectError::StorageUnavailable => "storage_unavailable",
        DirectError::CredentialUnavailable => "credential_unavailable",
        DirectError::InteractionRequired => "credential_interaction_required",
        DirectError::InvalidInvitation => "invalid_invitation",
        DirectError::Expired => "invitation_expired",
        DirectError::Rejected => "pairing_rejected",
        DirectError::ChangedClaim | DirectError::PolicyChanged => "changed",
        DirectError::AuthenticationRefused => "authentication_refused",
        DirectError::PinMismatch => "machine_identity_changed",
        DirectError::Revoked => "revoked",
        DirectError::IncompatibleProtocol => "incompatible_protocol",
        DirectError::Capacity => "capacity",
        DirectError::Disabled => "network_off",
        DirectError::InvalidRequest => "protocol_error",
        DirectError::InvalidAddress => "invalid_address",
        DirectError::AddressUnavailable => "address_unavailable",
        DirectError::PortInUse => "port_in_use",
        DirectError::IdentityMismatch => "identity_mismatch",
        DirectError::AlreadyDecided => "claim_decided",
        DirectError::Substituted => "comparison_mismatch",
        DirectError::WrongMachine => "wrong_machine",
    }
}
