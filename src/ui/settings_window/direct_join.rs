//! What joining with an invitation says when it pauses or can't finish.
//!
//! None of it claims more than SpaceTerm knows about what the server recorded. Add Server presents
//! these in its join stages.

use crate::direct_connections::EnrollmentStage;
use crate::server_connection::direct::DirectError;

/// What a stop SpaceTerm couldn't save says. It states the rule rather than the server's state,
/// since a later quiet save may already have sent it, and it stays only until a save succeeds.
pub(super) const WITHDRAW_UNSAVED: &str = "Keep SpaceTerm open until the cancellation is saved.";

/// What a join that couldn't finish says. A declined request, a server that couldn't prove its
/// identity and a pairing interrupted on this computer each read differently, and none claims
/// more than SpaceTerm knows about what the server recorded.
pub(super) fn join_failure(error: DirectError) -> &'static str {
    match error {
        DirectError::Rejected => DECLINED,
        DirectError::InvalidInvitation => "Invalid or used invitation. Ask for a new one.",
        DirectError::Expired => "Invitation expired. Ask for a new one.",
        DirectError::WrongServer => "This invitation is for a different SpaceTerm app.",
        DirectError::IncompatibleProtocol => "SpaceTerm and this server are incompatible.",
        DirectError::AuthenticationRefused
        | DirectError::PinMismatch
        | DirectError::ChangedClaim
        | DirectError::Substituted => "SpaceTerm couldn\u{2019}t verify this server.",
        DirectError::CredentialUnavailable | DirectError::InteractionRequired => {
            "SpaceTerm couldn\u{2019}t save this device\u{2019}s key."
        }
        DirectError::StorageUnavailable => "SpaceTerm couldn\u{2019}t save the connection.",
        DirectError::Capacity => "SpaceTerm is finishing other pairings. Try again shortly.",
        DirectError::Transport => {
            "SpaceTerm couldn\u{2019}t reach the server. Check your network connection."
        }
        DirectError::Disabled
        | DirectError::Revoked
        | DirectError::InvalidRequest
        | DirectError::InvalidAddress
        | DirectError::AddressUnavailable
        | DirectError::PolicyChanged
        | DirectError::AlreadyDecided => "The server refused pairing.",
    }
}

/// What a join paused by a network failure says. The failure names no cause, so the copy names
/// none: a pending system prompt, a firewall and a server that stopped all read the same. The
/// request stays as it was, so trying again asks with the same invitation.
pub(super) fn join_paused(stage: EnrollmentStage) -> String {
    match stage {
        EnrollmentStage::Claim => {
            "SpaceTerm couldn\u{2019}t reach the server. Check your network, then try again."
        }
        EnrollmentStage::Approval => {
            "SpaceTerm lost contact with the server. Check your network, then try again."
        }
    }
    .to_owned()
}

/// What a request the server's owner declined says.
const DECLINED: &str = "The server\u{2019}s owner declined pairing.";
