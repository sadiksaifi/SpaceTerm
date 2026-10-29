//! The codes the service lane and the ledger carry. They are the CLI output contract's codes, so a
//! client of any major reads the same classification.
use crate::ServerError;
use crate::direct::DirectError;

/// The contract code of a server failure.
pub fn code(error: &ServerError) -> &'static str {
    match error {
        ServerError::Direct(error) => direct_code(error),
        ServerError::NotSetUp => "not_set_up",
        ServerError::ServiceUnavailable => "service_unavailable",
        ServerError::ServiceBusy => "service_busy",
        ServerError::NotInstalled => "not_installed",
        ServerError::Stopped => "stopped",
        ServerError::Changed => "changed",
        ServerError::ActivationUnresolved => "activation_unresolved",
        ServerError::ReturnUnavailable => "return_unavailable",
        ServerError::RuntimeUnavailable => "candidate_unverified",
        ServerError::CandidateMissing => "no_candidate",
        ServerError::AuthorizationRequired => "authorization_required",
        ServerError::NotRunning => "not_running",
        ServerError::StorageUnavailable => "storage_unavailable",
        ServerError::IncompatibleState => "incompatible_state",
        ServerError::UnsafeStorage => "unsafe_storage",
        ServerError::AuthenticationRefused => "authentication_refused",
        ServerError::IncompatibleProtocol => "incompatible_protocol",
        ServerError::MissingRequiredCapability => "missing_capability",
        ServerError::CandidateChanged => "candidate_changed",
        ServerError::Protocol => "protocol_error",
        ServerError::StartupFailed => "startup_failed",
        ServerError::Controlled | ServerError::StaleControl => "controlled",
        ServerError::SessionUnavailable => "session_ended",
        ServerError::Unsupported => "unsupported",
        ServerError::DirectoryUnavailable => "directory_unavailable",
        ServerError::CleanupUnresolved => "cleanup_unresolved",
        ServerError::Capacity => "capacity",
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
        DirectError::PinMismatch => "server_identity_changed",
        DirectError::Revoked => "revoked",
        DirectError::IncompatibleProtocol => "incompatible_protocol",
        DirectError::Capacity => "capacity",
        DirectError::Disabled => "network_off",
        DirectError::InvalidRequest => "protocol_error",
        DirectError::InvalidAddress => "invalid_address",
        DirectError::AlreadyDecided => "claim_decided",
        DirectError::Substituted => "comparison_mismatch",
        DirectError::WrongServer => "wrong_server",
    }
}
