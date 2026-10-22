//! Direct network access to one SpaceTerm Server, as its authorized clients manage it.
//!
//! Every authorized client manages a server's direct network access equally: whether the server
//! listens, the invitations that let another device ask to pair, the approval of an exact claim,
//! and the devices already paired. Each operation runs over the client's existing connection to
//! that server, so the authority is whatever admitted that connection, and reaching the server
//! over the network grants nothing.
//!
//! The operations block, so callers run them on the background executor. Their results are
//! presentation values: fingerprints are already formatted, names are labels the device chose,
//! and failures carry only a typed classification. An invitation's text lives only in the value
//! that shows it, which never prints, persists or logs it.

use std::net::SocketAddr;

use gpui::SharedString;
use zeroize::Zeroizing;

use crate::local_server::LocalServer;
use crate::local_server::direct::{
    ClaimId, DeviceInfo, DirectError, DirectStatus, Invitation, ListenerState, NetworkPolicy,
    PairingClaim, PairingId,
};

/// Whether a server listens for direct network connections, and the identity devices verify.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessState {
    /// The server's name, which every step that changes its access names.
    pub(crate) server_name: SharedString,
    /// The address the owner chose, which the server keeps across restarts. `None` is off.
    pub(crate) policy: Option<SocketAddr>,
    pub(crate) listener: Listener,
    /// The server's own fingerprint, which a joining device shows beside its approval wait.
    pub(crate) server_fingerprint: SharedString,
}

/// What the server's direct network listener is doing now, as distinct from what was chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Listener {
    Off,
    Listening(SocketAddr),
    /// The owner turned access on and the server could not listen at that address.
    Failed,
}

/// One device paired with the server for direct network connections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Device {
    pub(crate) pairing: PairingId,
    /// The name the device gave itself. It labels the device and proves nothing.
    pub(crate) name: SharedString,
    pub(crate) fingerprint: SharedString,
}

/// One device asking to pair through an open invitation, as the server validated its claim.
#[derive(Clone)]
pub(crate) struct Claim {
    /// The exact claim the person sees, which approval sends back unchanged.
    pub(crate) claim: PairingClaim,
    pub(crate) id: ClaimId,
    pub(crate) name: SharedString,
    pub(crate) fingerprint: SharedString,
    pub(crate) expires_at: u64,
}

/// An open invitation. Its text is shown while the Pair Device dialog is open and copied only
/// when the person asks, so this value has no `Debug` and the text is wiped when it drops.
pub(crate) struct OpenInvitation {
    pub(crate) id: ClaimId,
    pub(crate) text: Zeroizing<String>,
    pub(crate) expires_at: u64,
}

/// Why a direct network operation didn't happen. Each kind reads one way to the person.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessFailure {
    /// The server isn't running, or this client isn't connected to it.
    Unreachable,
    /// The server's version doesn't offer direct network connections.
    Unsupported,
    /// The address isn't one the server can listen at.
    InvalidAddress,
    /// Direct network access is off, so the server opens no invitation.
    Disabled,
    /// The server already holds as many devices or connections as it allows.
    Capacity,
    /// The server couldn't durably save the change, so nothing changed.
    Storage,
    /// The invitation expired, or was used or withdrawn.
    InvitationEnded,
    /// The claim expired or was withdrawn before the answer arrived.
    ClaimExpired,
    /// Another client answered this claim first.
    ClaimDecided,
    /// The claim no longer matches the device and server the person was shown.
    ClaimChanged,
    /// Anything else the server refused.
    Refused,
}

/// The blocking direct network operations on one server, through an existing connection.
pub(crate) trait DirectAccessControl: Send + Sync {
    fn status(&self) -> Result<AccessState, AccessFailure>;
    /// Saves `policy` durably and answers once the server has applied it. `None` turns access off
    /// and leaves every pairing in place.
    fn set_policy(&self, policy: Option<NetworkPolicy>) -> Result<AccessState, AccessFailure>;
    /// Opens one single-use expiring invitation.
    fn invite(&self) -> Result<OpenInvitation, AccessFailure>;
    /// Ends an invitation nobody needs any more, so no device can use it.
    fn withdraw(&self, invitation: ClaimId) -> Result<(), AccessFailure>;
    /// The validated claims waiting for an answer.
    fn claims(&self) -> Result<Vec<Claim>, AccessFailure>;
    /// Pairs exactly the device and server that `claim` names.
    fn approve(&self, claim: &PairingClaim) -> Result<Device, AccessFailure>;
    fn reject(&self, claim: ClaimId) -> Result<(), AccessFailure>;
    fn devices(&self) -> Result<Vec<Device>, AccessFailure>;
}

impl DirectAccessControl for LocalServer {
    fn status(&self) -> Result<AccessState, AccessFailure> {
        self.direct_status().map(access_state).map_err(classify)
    }

    fn set_policy(&self, policy: Option<NetworkPolicy>) -> Result<AccessState, AccessFailure> {
        self.set_direct_policy(policy)
            .map(access_state)
            .map_err(classify)
    }

    fn invite(&self) -> Result<OpenInvitation, AccessFailure> {
        self.invite_device().map(open_invitation).map_err(classify)
    }

    fn withdraw(&self, invitation: ClaimId) -> Result<(), AccessFailure> {
        self.withdraw_invitation(invitation).map_err(classify)
    }

    fn claims(&self) -> Result<Vec<Claim>, AccessFailure> {
        self.pairing_claims()
            .map(|claims| claims.into_iter().map(claim).collect())
            .map_err(classify)
    }

    fn approve(&self, claim: &PairingClaim) -> Result<Device, AccessFailure> {
        self.approve_device(claim).map(device).map_err(classify)
    }

    fn reject(&self, claim: ClaimId) -> Result<(), AccessFailure> {
        self.reject_device(claim).map_err(classify)
    }

    fn devices(&self) -> Result<Vec<Device>, AccessFailure> {
        self.paired_devices()
            .map(|devices| devices.into_iter().map(device).collect())
            .map_err(classify)
    }
}

fn access_state(status: DirectStatus) -> AccessState {
    AccessState {
        server_name: status.server_name.into(),
        policy: status.policy.map(|policy| policy.bind),
        listener: match status.listener {
            ListenerState::Off => Listener::Off,
            ListenerState::Listening { addr } => Listener::Listening(addr),
            ListenerState::Failed(_) => Listener::Failed,
        },
        server_fingerprint: status.pin.fingerprint().into(),
    }
}

fn open_invitation(invitation: Invitation) -> OpenInvitation {
    OpenInvitation {
        id: invitation.id(),
        text: invitation.display_text(),
        expires_at: invitation.expires_at_unix_seconds(),
    }
}

fn claim(claim: PairingClaim) -> Claim {
    Claim {
        id: claim.id,
        name: claim.device_name.as_str().to_owned().into(),
        fingerprint: claim.device_fingerprint().into(),
        expires_at: claim.expires_at_unix_seconds,
        claim,
    }
}

fn device(device: DeviceInfo) -> Device {
    Device {
        fingerprint: device.fingerprint().into(),
        name: device.name.as_str().to_owned().into(),
        pairing: device.pairing,
    }
}

/// The one place a server's direct network error becomes something the person can read. Server
/// errors convert to their direct network classification first.
fn classify(error: impl Into<DirectError>) -> AccessFailure {
    match error.into() {
        // A route whose own key or credential storage is unavailable can't reach the server.
        DirectError::Transport
        | DirectError::InteractionRequired
        | DirectError::CredentialUnavailable
        | DirectError::Revoked => AccessFailure::Unreachable,
        DirectError::IncompatibleProtocol => AccessFailure::Unsupported,
        DirectError::InvalidAddress => AccessFailure::InvalidAddress,
        DirectError::Disabled => AccessFailure::Disabled,
        DirectError::Capacity => AccessFailure::Capacity,
        DirectError::StorageUnavailable => AccessFailure::Storage,
        DirectError::InvalidInvitation => AccessFailure::InvitationEnded,
        DirectError::Expired => AccessFailure::ClaimExpired,
        // A claim another client declined is one already answered.
        DirectError::AlreadyDecided | DirectError::Rejected => AccessFailure::ClaimDecided,
        DirectError::ChangedClaim | DirectError::Substituted | DirectError::WrongServer => {
            AccessFailure::ClaimChanged
        }
        DirectError::AuthenticationRefused
        | DirectError::PinMismatch
        | DirectError::InvalidRequest => AccessFailure::Refused,
    }
}

impl From<DirectError> for AccessFailure {
    fn from(error: DirectError) -> Self {
        classify(error)
    }
}

impl AccessFailure {
    /// What happened, in one sentence naming `server`.
    pub(crate) fn explanation(self, server: &str) -> String {
        match self {
            Self::Unreachable => format!(
                "SpaceTerm can\u{2019}t reach the server on \u{201C}{server}\u{201D} right now, so \
                 nothing changed."
            ),
            Self::Unsupported => format!(
                "The server on \u{201C}{server}\u{201D} doesn\u{2019}t offer direct network \
                 connections. Update it to use them."
            ),
            Self::InvalidAddress => format!(
                "Enter one of \u{201C}{server}\u{201D}\u{2019}s own IP addresses, such as \
                 192.168.1.20, optionally followed by a port."
            ),
            Self::Disabled => format!(
                "Turn on direct network access for \u{201C}{server}\u{201D} before pairing a \
                 device."
            ),
            Self::Capacity => format!(
                "The server on \u{201C}{server}\u{201D} already has as many devices or \
                 connections as it allows."
            ),
            Self::Storage => format!(
                "The server on \u{201C}{server}\u{201D} couldn\u{2019}t save the change, so \
                 nothing changed."
            ),
            Self::InvitationEnded => {
                "This invitation can\u{2019}t be used any more. Create a new one to pair a device."
                    .to_owned()
            }
            Self::ClaimExpired => {
                "That request expired before your answer arrived, so the device isn\u{2019}t \
                 paired."
                    .to_owned()
            }
            Self::ClaimDecided => {
                "Someone using another SpaceTerm answered that request first.".to_owned()
            }
            Self::ClaimChanged => {
                "That request no longer matches the device and server shown, so SpaceTerm \
                 refused it. The device isn\u{2019}t paired."
                    .to_owned()
            }
            Self::Refused => format!(
                "The server on \u{201C}{server}\u{201D} refused the request, so nothing changed."
            ),
        }
    }
}

/// The address this computer uses to reach other machines, which the local server's Turn On
/// suggests. Choosing it sends nothing: connecting a UDP socket only selects a route.
pub(crate) fn this_computers_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // A documentation address, which no network routes traffic to.
    socket
        .connect((std::net::Ipv4Addr::new(192, 0, 2, 1), 9))
        .ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_unspecified() && !address.is_loopback()).then_some(address)
}

/// How long until `expires_at`, in whole seconds, as of `now`. Both are Unix seconds.
pub(crate) fn remaining(expires_at: u64, now: u64) -> u64 {
    expires_at.saturating_sub(now)
}

/// A countdown such as "4:05", which the person reads at a glance.
pub(crate) fn countdown(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The current time in Unix seconds, or zero before the epoch.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
