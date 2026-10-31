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
//! and failures carry only a typed classification. An invitation's link lives only in the value
//! that shows it, which never prints, persists or logs it.

use std::net::SocketAddr;

use gpui::SharedString;
use zeroize::Zeroizing;

use crate::server_connection::ServerConnection;
use crate::server_connection::direct::{
    ClaimId, ComparisonCode, DeviceInfo, DirectError, DirectStatus, Invitation, ListenerState,
    NetworkAddresses, NetworkPolicy, PairingClaim, PairingId,
};

/// Whether a server listens for direct network connections, and where it could.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessState {
    /// The server's name, which every step that changes its access names.
    pub(crate) server_name: SharedString,
    /// The address the server listens at while access is on. `None` is off.
    pub(crate) policy: Option<SocketAddr>,
    /// The address the owner chose last, which the server keeps while access is off too, so
    /// turning access on again uses it.
    pub(crate) configured: Option<SocketAddr>,
    pub(crate) listener: Listener,
    /// The addresses the server can listen at, as the server read them from its own machine.
    /// `None` means the server couldn't read them.
    pub(crate) addresses: Option<NetworkAddresses>,
}

/// What the server's direct network listener is doing now, as distinct from what was chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Listener {
    Off,
    Listening(SocketAddr),
    /// The chosen address is no longer on the server's machine, such as after a Wi-Fi change.
    AddressGone,
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
    /// When the server last admitted a connection from the device, in Unix seconds. `None` means
    /// it never connected.
    pub(crate) last_connected_at: Option<u64>,
}

/// One device asking to pair through an open invitation, as the server validated its claim.
#[derive(Clone)]
pub(crate) struct Claim {
    /// The exact claim the person sees, which approval sends back unchanged.
    pub(crate) claim: PairingClaim,
    pub(crate) id: ClaimId,
    /// The name the device gave itself. It labels the device and proves nothing.
    pub(crate) name: SharedString,
    /// The code the device shows too, derived from this claim's authenticated exchange. The
    /// person compares it and never types it.
    pub(crate) code: ComparisonCode,
}

/// An open invitation. Its link is shown while Invite Device is open and copied only when the
/// person asks, so this value has no `Debug` and the link is wiped when it drops.
pub(crate) struct OpenInvitation {
    pub(crate) id: ClaimId,
    pub(crate) text: Zeroizing<String>,
    /// The server address the link carries, when it is an IP address.
    pub(crate) address: Option<std::net::IpAddr>,
    /// When the countdown reaches zero, on this computer's monotonic clock. It only presents the
    /// lifetime the server gave the invitation: the server alone decides when it expires.
    pub(crate) deadline: std::time::Instant,
}

impl OpenInvitation {
    /// Whether `claim` used this invitation, as the server recorded in the claim.
    pub(crate) fn claimed_by(&self, claim: &Claim) -> bool {
        claim.claim.invitation_id == self.id
    }
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
    /// The server couldn't listen at the address and port, so nothing changed.
    AddressUnavailable,
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
    /// Saves whether access is on, and at `policy` when given, durably, and answers once the server
    /// has applied it. `None` keeps the address chosen before. Turning access off leaves every
    /// pairing in place.
    fn set_policy(
        &self,
        policy: Option<NetworkPolicy>,
        enabled: bool,
    ) -> Result<AccessState, AccessFailure>;
    /// Opens one single-use expiring invitation whose link carries `address`, which must be
    /// one the server listens at, or the server's own choice.
    fn invite(&self, address: Option<std::net::IpAddr>) -> Result<OpenInvitation, AccessFailure>;
    /// Ends an invitation nobody needs any more, so no device can use it.
    fn withdraw(&self, invitation: ClaimId) -> Result<(), AccessFailure>;
    /// The validated claims waiting for an answer.
    fn claims(&self) -> Result<Vec<Claim>, AccessFailure>;
    /// Pairs exactly the device and server that `claim` names.
    fn approve(&self, claim: &PairingClaim) -> Result<Device, AccessFailure>;
    fn reject(&self, claim: ClaimId) -> Result<(), AccessFailure>;
    fn devices(&self) -> Result<Vec<Device>, AccessFailure>;
}

impl DirectAccessControl for ServerConnection {
    fn status(&self) -> Result<AccessState, AccessFailure> {
        self.direct_status().map(access_state).map_err(classify)
    }

    fn set_policy(
        &self,
        policy: Option<NetworkPolicy>,
        enabled: bool,
    ) -> Result<AccessState, AccessFailure> {
        self.set_direct_policy(policy, enabled)
            .map(access_state)
            .map_err(classify)
    }

    fn invite(&self, address: Option<std::net::IpAddr>) -> Result<OpenInvitation, AccessFailure> {
        let advertise = if let Some(address) = address {
            let status = self.direct_status().map_err(classify)?;
            let spaceterm_server::direct::ListenerState::Listening { addr } = status.listener
            else {
                return Err(classify(spaceterm_server::direct::DirectError::Disabled));
            };
            Some(std::net::SocketAddr::new(address, addr.port()).to_string())
        } else {
            None
        };
        self.invite_device(advertise)
            .map(open_invitation)
            .map_err(classify)
    }

    fn withdraw(&self, invitation: ClaimId) -> Result<(), AccessFailure> {
        self.withdraw_invitation(invitation).map_err(classify)
    }

    fn claims(&self) -> Result<Vec<Claim>, AccessFailure> {
        self.pairing_claims(None)
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
    let configured = status.policy.map(|policy| policy.bind);
    AccessState {
        server_name: status.server_name.into(),
        policy: configured.filter(|_| status.enabled),
        configured,
        listener: match status.listener {
            ListenerState::Off => Listener::Off,
            // Starting binds within seconds, once the previous address's connections end.
            ListenerState::Listening { addr } | ListenerState::Starting { addr } => {
                Listener::Listening(addr)
            }
            ListenerState::Failed(DirectError::InvalidAddress) => Listener::AddressGone,
            ListenerState::Failed(_) => Listener::Failed,
        },
        addresses: status.network_addresses.ok(),
    }
}

fn open_invitation(invitation: Invitation) -> OpenInvitation {
    OpenInvitation {
        id: invitation.id(),
        address: invitation
            .endpoint()
            .strip_prefix("quic://")
            .and_then(|authority| authority.parse::<SocketAddr>().ok())
            .map(|address| address.ip()),
        text: invitation.display_text(),
        deadline: std::time::Instant::now() + invitation.lifetime(),
    }
}

fn claim(claim: PairingClaim) -> Claim {
    Claim {
        id: claim.id,
        name: claim.device_name.as_str().to_owned().into(),
        code: claim.comparison_code(),
        claim,
    }
}

fn device(device: DeviceInfo) -> Device {
    Device {
        fingerprint: device.fingerprint().into(),
        name: device.name.as_str().to_owned().into(),
        pairing: device.pairing,
        last_connected_at: device.last_connected_at_unix_seconds,
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
        DirectError::AddressUnavailable => AccessFailure::AddressUnavailable,
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
        | DirectError::PolicyChanged
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
    /// Why reading the server's access failed. A read changes nothing, so it never says that a
    /// change failed.
    pub(crate) fn read_explanation(self, server: &str) -> String {
        match self {
            Self::Storage => format!(
                "\u{201C}{server}\u{201D} couldn\u{2019}t read its network settings. Try again."
            ),
            other => other.explanation(server),
        }
    }

    /// Why the person's change didn't take effect.
    pub(crate) fn explanation(self, server: &str) -> String {
        match self {
            Self::Unreachable => {
                format!("SpaceTerm can\u{2019}t reach \u{201C}{server}\u{201D}. Try again.")
            }
            Self::Unsupported => {
                format!(
                    "Update the server on \u{201C}{server}\u{201D} to use direct network connections."
                )
            }
            Self::InvalidAddress => {
                format!(
                    "Enter an IP address of \u{201C}{server}\u{201D}, optionally followed by a port."
                )
            }
            Self::AddressUnavailable => format!(
                "\u{201C}{server}\u{201D} couldn\u{2019}t listen at that address and port. Another app may be using the port."
            ),
            Self::Disabled => {
                format!("Allow devices to connect to \u{201C}{server}\u{201D} before inviting one.")
            }
            Self::Capacity => {
                format!("\u{201C}{server}\u{201D} has reached its device or connection limit.")
            }
            Self::Storage => {
                format!("\u{201C}{server}\u{201D} couldn\u{2019}t save the change. Try again.")
            }
            Self::InvitationEnded => "Create a new invitation to invite this device.".to_owned(),
            Self::ClaimExpired => {
                "The invitation expired. Create a new one to invite this device.".to_owned()
            }
            Self::ClaimDecided => {
                "Another SpaceTerm already answered this device\u{2019}s request.".to_owned()
            }
            Self::ClaimChanged => {
                "SpaceTerm refused the changed request. Invite the device again.".to_owned()
            }
            Self::Refused => format!("\u{201C}{server}\u{201D} refused the request."),
        }
    }
}

/// A comparison code as both screens show it: two groups of three digits, such as "042 917".
pub(crate) fn code_text(code: ComparisonCode) -> SharedString {
    let digits = code.to_string();
    let (first, second) = digits.split_at(digits.len() / 2);
    format!("{first} {second}").into()
}

/// A comparison code as assistive technology reads it: one digit at a time.
pub(crate) fn code_speech(code: ComparisonCode) -> SharedString {
    let digits = code.to_string();
    let mut speech = String::with_capacity(digits.len() * 2);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 {
            speech.push(' ');
        }
        speech.push(digit);
    }
    speech.into()
}

/// How long until `deadline`, in whole seconds, as of `now`.
pub(crate) fn remaining(deadline: std::time::Instant, now: std::time::Instant) -> u64 {
    deadline.saturating_duration_since(now).as_secs()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_connection::direct::ServerPin;

    fn claim_with_exchange(prefix: [u8; 4]) -> PairingClaim {
        let mut exchange_hash = [0; 32];
        exchange_hash[..4].copy_from_slice(&prefix);
        PairingClaim {
            id: [1; 32],
            invitation_id: [7; 32],
            server: [2; 32],
            device_public_key: [3; 32],
            device_name: "MacBook".into(),
            expires_at_unix_seconds: 1_000,
            exchange_hash,
        }
    }

    fn status(listener: ListenerState) -> DirectStatus {
        DirectStatus {
            pin: ServerPin {
                build: "1.0.0".to_owned(),
                server: [2; 32],
                public_key: [4; 32],
                certificate: [5; 32],
            },
            policy: NetworkPolicy::parse("192.168.1.20").ok(),
            enabled: true,
            server_name: "studio".to_owned(),
            listener,
            network_addresses: Ok(NetworkAddresses {
                available: vec!["192.168.1.21".parse().expect("an address")],
                suggested: None,
            }),
            setup_generation: 0,
        }
    }

    #[test]
    fn a_comparison_code_keeps_its_leading_zero_in_two_groups_of_three() {
        // 0x00003039 is 12345.
        let code = claim(claim_with_exchange([0, 0, 0x30, 0x39])).code;

        assert_eq!(code_text(code).as_ref(), "012 345");
    }

    #[test]
    fn a_comparison_code_is_spoken_one_digit_at_a_time() {
        let code = claim(claim_with_exchange([0, 0, 0x30, 0x39])).code;

        assert_eq!(code_speech(code).as_ref(), "0 1 2 3 4 5");
    }

    #[test]
    fn a_listening_address_the_server_no_longer_has_reads_as_gone() {
        let state = access_state(status(ListenerState::Failed(DirectError::InvalidAddress)));

        assert_eq!(state.listener, Listener::AddressGone);
    }

    #[test]
    fn a_listener_that_failed_otherwise_reads_as_failed() {
        let state = access_state(status(ListenerState::Failed(DirectError::Transport)));

        assert_eq!(state.listener, Listener::Failed);
    }

    #[test]
    fn addresses_the_server_couldnt_read_are_unknown_rather_than_empty() {
        let mut unread = status(ListenerState::Off);
        unread.network_addresses = Err(DirectError::Transport);

        assert_eq!(access_state(unread).addresses, None);
    }

    #[test]
    fn an_invitation_recognizes_a_claim_that_names_it() {
        let invitation = OpenInvitation {
            id: [7; 32],
            text: Zeroizing::new("spaceterm://pair/test".to_owned()),
            address: None,
            deadline: std::time::Instant::now(),
        };

        assert!(invitation.claimed_by(&claim(claim_with_exchange([0; 4]))));
    }

    #[test]
    fn an_invitation_ignores_a_claim_on_another_invitation_that_expires_with_it() {
        let invitation = OpenInvitation {
            id: [7; 32],
            text: Zeroizing::new("spaceterm://pair/test".to_owned()),
            address: None,
            deadline: std::time::Instant::now(),
        };
        let mut other = claim_with_exchange([0; 4]);
        other.invitation_id = [8; 32];

        assert!(!invitation.claimed_by(&claim(other)));
    }
}
