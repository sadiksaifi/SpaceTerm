//! What joining with an invitation says when it pauses or can't finish.
//!
//! None of it claims more than SpaceTerm knows about what the server recorded. Add Server presents
//! these in its join stages.

use crate::direct_connections::EnrollmentStage;
use crate::local_server::direct::DirectError;

/// What a stop SpaceTerm couldn't save says. It states the rule rather than the server's state,
/// since a later quiet save may already have sent it, and it stays only until a save succeeds.
pub(super) const WITHDRAW_UNSAVED: &str = "SpaceTerm couldn\u{2019}t save this request on this computer yet. \
     It asks the server only after the request is saved, and the request may be gone if SpaceTerm \
     reopens first.";

/// What a join that couldn't finish says. A declined request, a server that couldn't prove its
/// identity and a pairing interrupted on this computer each read differently, and none claims
/// more than SpaceTerm knows about what the server recorded.
pub(super) fn join_failure(error: DirectError) -> &'static str {
    match error {
        DirectError::Rejected => DECLINED,
        DirectError::InvalidInvitation => {
            "That isn\u{2019}t a valid invitation, or it was already used. Ask for a new one."
        }
        DirectError::Expired => {
            "The invitation expired before the server approved this device. Ask for a new one."
        }
        DirectError::WrongServer => {
            "This invitation comes from a different kind of SpaceTerm build, so this one can\u{2019}t \
             use it."
        }
        DirectError::IncompatibleProtocol => {
            "This SpaceTerm and that server have no protocol version in common. Update the older \
             one, then ask for a new invitation."
        }
        DirectError::AuthenticationRefused
        | DirectError::PinMismatch
        | DirectError::ChangedClaim
        | DirectError::Substituted => {
            "The server couldn\u{2019}t prove it\u{2019}s the one that made this invitation, so \
             SpaceTerm stopped and won\u{2019}t use it."
        }
        DirectError::CredentialUnavailable | DirectError::InteractionRequired => {
            "SpaceTerm couldn\u{2019}t keep this device\u{2019}s key in the system\u{2019}s \
             credential storage, so this device can\u{2019}t use the pairing."
        }
        DirectError::StorageUnavailable => {
            "SpaceTerm couldn\u{2019}t save the connection on this computer, so this device \
             can\u{2019}t use it. If the server already approved this device, SpaceTerm \
             withdraws that pairing and shows its progress in a notice on Servers."
        }
        DirectError::Capacity => {
            "SpaceTerm is already finishing as many pairings as it can. Try again shortly."
        }
        DirectError::Transport => {
            "SpaceTerm couldn\u{2019}t reach the server at the invitation\u{2019}s address. Check \
             that this computer can reach it on the network."
        }
        DirectError::Disabled
        | DirectError::Revoked
        | DirectError::InvalidRequest
        | DirectError::InvalidAddress
        | DirectError::AlreadyDecided => {
            "The server refused the request, so this device isn\u{2019}t paired."
        }
    }
}

/// What a join paused by a network failure says. The failure names no cause, so the copy names
/// none: a pending system prompt, a firewall and a server that stopped all read the same. The
/// request stays as it was, so trying again asks with the same invitation.
pub(super) fn join_paused(stage: EnrollmentStage) -> String {
    let what = match stage {
        EnrollmentStage::Claim => {
            "SpaceTerm didn\u{2019}t get an answer from the server at the invitation\u{2019}s \
             address."
        }
        EnrollmentStage::Approval => {
            "SpaceTerm lost contact with the server while waiting for approval."
        }
    };
    // Some systems ask before an app reaches the local network, so the copy covers that prompt
    // without claiming one appeared.
    let next = "If this computer asks whether SpaceTerm can find devices on the local network, \
                allow it. Otherwise, check that this computer can reach the server on the network. \
                Then choose Try Again.";
    format!(
        "{what} {next} SpaceTerm keeps this request until the invitation expires, and asks again \
         only when you choose Try Again."
    )
}

/// What a request the server's owner declined says.
const DECLINED: &str = "The server\u{2019}s owner declined this device, so it isn\u{2019}t paired.";
