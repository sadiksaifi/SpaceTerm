//! What the Servers section presents, and the requests it sends back.
//!
//! The client's server connections own this state and publish it as a [`ServersSnapshot`]. The
//! section keeps none of it in the Settings Document: a server's settings, status, and pairing
//! belong to that server, and every request goes through [`ServerRequests`].

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the client's server connections publish snapshots once they are wired to Settings"
    )
)]

use std::{num::NonZeroU64, rc::Rc};

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};
use spaceterm_client::management_cli::confirmation::{
    AssessmentToken, ManagementAction, ManagementAssessment,
};
use spaceterm_core::{
    authority::{FieldVersion, ServerEpoch},
    identity::{DeviceId, PairingId},
    operations::OperationId,
    pairing::{LocalPairingKey, PairingTarget},
};
use spaceterm_host::{
    native_deployment::contracts::NativeRequestId,
    startup::{DesiredServerState, InstallationId},
};
use spaceterm_protocol::{ProtocolError, negotiation::Negotiator};

use crate::platform::permission_access::AccessibilityNaming;
use crate::worktrees::path_template::WorktreePathTemplateError;

/// One server's stable identity on this device.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ServerKey(pub(crate) SharedString);

impl ServerKey {
    pub(crate) fn new(key: impl Into<SharedString>) -> Self {
        Self(key.into())
    }
}

/// How this device reaches a server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerRoute {
    /// The private local endpoint of the server on this device.
    Local,
    /// An SSH connection, named by its destination, such as `dev@build.example.com`.
    Ssh { destination: SharedString },
    /// A secure WebSocket connection, named by its authority, such as `build.example.com:7443`.
    Wss { authority: SharedString },
}

impl ServerRoute {
    pub(crate) fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// Whether this device can manage the server's service: through the local service manager or
    /// over SSH. A WSS connection reaches only the running server, so it can neither start the
    /// service nor obtain the host authorization a durable stop needs.
    pub(crate) fn manages_host(&self) -> bool {
        !matches!(self, Self::Wss { .. })
    }
}

/// Which side of an incompatible connection needs a newer SpaceTerm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IncompatibleSide {
    /// This device runs an older SpaceTerm than the server accepts.
    ThisDevice,
    /// The server runs an older SpaceTerm Server than this device accepts.
    Server,
}

/// What this device knows about a server's process and its connection to it.
///
/// No status means "unpair pending". Unpairing removes the server from this device at once, so it
/// leaves the list, and ADR 0038 forbids an obsolete connection staying in the saved-server list.
/// A [`PendingUnpair`] notice above the list reports the revocation still being retried.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerStatus {
    /// This device is connecting or reconnecting.
    Connecting,
    /// Connected, and the server accepts requests.
    Running,
    Starting,
    Stopping,
    /// Deliberately stopped. It stays stopped across reboots until someone starts it.
    Stopped,
    /// Unreachable. The machine, the network, or the server may be down.
    Offline,
    /// Reachable, but the two sides cannot use one protocol.
    Incompatible(IncompatibleSide),
    /// Reachable, but disjoint profiles do not establish which side an update would fix.
    UnsupportedProfiles,
    /// Installing `version`. The server restarts when it finishes.
    Updating {
        version: SharedString,
    },
}

/// Why a server refused a change to one of its settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingRejection {
    /// The Worktree Path Template is malformed.
    InvalidTemplate(WorktreePathTemplateError),
    /// The server cannot create directories there.
    Unwritable,
    /// The server could not save the setting for a reason it does not classify. The section shows
    /// fixed text and never the server's own message, which may carry paths or native errors.
    NotSaved,
}

/// Where a change this device sent to a server setting stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingEditState {
    /// Sent and not yet answered.
    Saving,
    /// The server refused it.
    Rejected(SettingRejection),
    /// Another device changed the setting first. The setting's value is now theirs.
    Conflict,
}

/// A change this device sent that the server has not applied. The draft survives a rejection or a
/// conflict so the person can correct or resend it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingEdit {
    pub(crate) draft: SharedString,
    pub(crate) state: SettingEditState,
}

/// One text setting owned by a server and shared by every device paired with it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerTextSetting {
    /// The value the server holds.
    pub(crate) value: SharedString,
    /// The server's revision of `value`. A change names the revision it replaces, so the server
    /// can detect a change another device made first.
    pub(crate) revision: u64,
    pub(crate) edit: Option<SettingEdit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerPermissionKind {
    Microphone,
    ScreenRecording,
    Accessibility,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerPermissionState {
    Allowed,
    NotAllowed,
    /// Nobody has granted or denied it yet.
    NotSetUp,
    /// The server's system has no such permission.
    NotNeeded,
    /// The owning server has no current observation.
    Unknown,
    /// The owning server cannot query this permission.
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServerPermission {
    pub(crate) kind: ServerPermissionKind,
    pub(crate) state: ServerPermissionState,
}

/// The system permissions a server reports for the programs in its Terminal Sessions. They belong
/// to the server's machine, not to this device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerPermissions {
    /// What the server's system calls the Accessibility permission.
    pub(crate) naming: AccessibilityNaming,
    pub(crate) entries: Vec<ServerPermission>,
}

/// One set of addresses a server can accept network connections on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BindScope {
    pub(crate) id: SharedString,
    /// A plain name, such as "Local network".
    pub(crate) label: SharedString,
    /// The addresses, such as "192.168.1.20, fd00::20".
    pub(crate) addresses: SharedString,
    pub(crate) endpoints: Vec<std::net::SocketAddr>,
}

/// Whether a server accepts SpaceTerm connections over the network. It is off until someone turns
/// it on for one server and one bind scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteAccess {
    pub(crate) version: FieldVersion,
    pub(crate) enabled: bool,
    /// The scope the server listens on, or listened on last.
    pub(crate) scope: Option<SharedString>,
    /// The scopes the server offers.
    pub(crate) scopes: Vec<BindScope>,
    /// A change this device sent that the server has not confirmed.
    pub(crate) pending: bool,
    /// Authenticated current bind endpoints. Kept separate from offered scope labels.
    pub(crate) endpoints: Vec<std::net::SocketAddr>,
    pub(crate) edit: Option<RemoteAccessEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteAccessEdit {
    pub(crate) draft: spaceterm_core::remote_access::RemoteAccessPolicy,
    pub(crate) base_version: FieldVersion,
    pub(crate) state: SettingEditState,
}

/// Protocol facts verified by the connection owner, never inferred from release labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VerifiedCompatibility {
    Compatible {
        major: u32,
    },
    Incompatible(IncompatibleSide),
    /// Disjoint profiles do not establish which side an update would fix.
    UnsupportedProfiles,
}

impl VerifiedCompatibility {
    /// Projects authenticated protocol facts without comparing release labels.
    pub(crate) fn connection_status(self) -> ServerStatus {
        match self {
            Self::Compatible { .. } => ServerStatus::Running,
            Self::Incompatible(side) => ServerStatus::Incompatible(side),
            Self::UnsupportedProfiles => ServerStatus::UnsupportedProfiles,
        }
    }

    /// Call only with the authenticated peer's advertisement. Parsing alone grants no trust.
    pub(crate) fn from_verified_advertisements(
        client: &[u32],
        server: &[u32],
    ) -> Result<Self, ProtocolError> {
        match Negotiator::select(client, server) {
            Ok(major) => Ok(Self::Compatible { major }),
            Err(ProtocolError::Incompatible) => {
                let side = if client[0] < server[server.len() - 1] {
                    Self::Incompatible(IncompatibleSide::ThisDevice)
                } else if server[0] < client[client.len() - 1] {
                    Self::Incompatible(IncompatibleSide::Server)
                } else {
                    Self::UnsupportedProfiles
                };
                Ok(side)
            }
            Err(error) => Err(error),
        }
    }
}

/// Supplied account-service lifecycle facts, not a selector for this client's host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerServiceLifecycle {
    LoginSession,
    BootWithLingering,
}

/// Signed installation comparison facts. The version is a display label only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerInstallation {
    pub(crate) installation: InstallationId,
    pub(crate) artifact_sha256: [u8; 32],
    pub(crate) version: SharedString,
}

/// Exact activation proposal retained by the native owner. It is not OS authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UpdateAssessment {
    pub(crate) reference: AssessmentToken,
    pub(crate) native_request: NativeRequestId,
    pub(crate) original_pairing: LocalPairingKey,
    pub(crate) selection_generation: NonZeroU64,
    pub(crate) approved: Option<InstallationId>,
    pub(crate) running: Option<InstallationId>,
    pub(crate) candidate: ServerInstallation,
    pub(crate) runtime_epoch: Option<ServerEpoch>,
    pub(crate) desired: DesiredServerState,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ServerInstallations {
    pub(crate) selection_generation: Option<NonZeroU64>,
    pub(crate) running: Option<ServerInstallation>,
    pub(crate) approved: Option<ServerInstallation>,
    pub(crate) staged: Option<ServerInstallation>,
    pub(crate) activation: Option<UpdateAssessment>,
}

/// One exact authorized pairing. A device that reenrolls has a different target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PairedDevice {
    pub(crate) target: PairingTarget,
    pub(crate) name: SharedString,
}

/// A bounded, revision-coherent page from the selected server's pairing owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PairedDevices {
    pub(crate) version: FieldVersion,
    pub(crate) this_device: DeviceId,
    pub(crate) total: u64,
    pub(crate) devices: Vec<PairedDevice>,
    /// Exact last pairing of this page. The server checks the page revision on continuation.
    pub(crate) next: Option<PairingId>,
}

impl PairedDevices {
    pub(crate) fn valid_for(&self, pairing: &LocalPairingKey) -> bool {
        let server = pairing.parts().server;
        self.version.get() != 0
            && self.this_device.bytes() != [0; 16]
            && self.devices.len() <= 256
            && self.total >= self.devices.len() as u64
            && self
                .devices
                .iter()
                .all(|device| device.target.parts().server == server)
            && self.devices.iter().enumerate().all(|(index, device)| {
                self.devices[..index]
                    .iter()
                    .all(|other| other.target.parts().pairing != device.target.parts().pairing)
            })
            && self.next.is_none_or(|next| {
                self.devices
                    .last()
                    .is_some_and(|last| last.target.parts().pairing == next)
                    && self.total > self.devices.len() as u64
            })
    }
}

/// Retained original requests and exact comparison prompts. Failures contain no native text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ServerManagement {
    pub(crate) pending: bool,
    pub(crate) end_all: Option<ManagementAssessment>,
    pub(crate) stop: Option<HostAssessment>,
    pub(crate) restart: Option<HostAssessment>,
    pub(crate) failure: Option<spaceterm_client::management_cli::ManagementFailure>,
    pub(crate) operation: Option<OperationId>,
    pub(crate) native_request: Option<NativeRequestId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostAssessment {
    pub(crate) reference: AssessmentToken,
    pub(crate) request: NativeRequestId,
    pub(crate) action: spaceterm_host::account_service::ServiceAction,
    pub(crate) affected_sessions: u32,
}

/// One server this device is paired with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerSummary {
    /// Presentation lookup only. Requests carry `pairing` independently of this key.
    pub(crate) key: ServerKey,
    pub(crate) pairing: LocalPairingKey,
    pub(crate) runtime_epoch: Option<ServerEpoch>,
    pub(crate) service_lifecycle: Option<ServerServiceLifecycle>,
    pub(crate) compatibility: Option<VerifiedCompatibility>,
    pub(crate) installations: ServerInstallations,
    pub(crate) paired_devices: Option<PairedDevices>,
    /// Latest owner-issued unpair assessment. It carries no authority until the owner revalidates it.
    pub(crate) unpair_assessment: Option<ManagementAssessment>,
    pub(crate) management: ServerManagement,
    pub(crate) name: SharedString,
    pub(crate) route: ServerRoute,
    pub(crate) status: ServerStatus,
    /// The version the server runs, when known.
    pub(crate) version: Option<SharedString>,
    /// A newer version installed beside the running one and not yet running.
    pub(crate) staged_version: Option<SharedString>,
    /// The server's New Worktree Location, once a live connection has read it.
    pub(crate) worktree_location: Option<ServerTextSetting>,
    pub(crate) permissions: Option<ServerPermissions>,
    pub(crate) remote_access: Option<RemoteAccess>,
}

impl ServerSummary {
    /// Whether this device holds a live connection, which server requests need.
    pub(crate) fn connected(&self) -> bool {
        self.status == ServerStatus::Running
    }

    pub(crate) fn activation_matches(&self, assessment: &UpdateAssessment) -> bool {
        assessment.original_pairing == self.pairing
            && Some(assessment.selection_generation) == self.installations.selection_generation
            && assessment.approved == self.installations.approved.as_ref().map(|v| v.installation)
            && assessment.running == self.installations.running.as_ref().map(|v| v.installation)
            && Some(&assessment.candidate) == self.installations.staged.as_ref()
            && assessment.candidate.artifact_sha256 != [0; 32]
            && assessment.runtime_epoch == self.runtime_epoch
            && assessment.desired
                == if self.status == ServerStatus::Stopped {
                    DesiredServerState::Stopped
                } else {
                    DesiredServerState::Running
                }
            && self.installations.activation.as_ref() == Some(assessment)
    }
}

/// A self-unpair whose server-side revocation is still being retried in the background. The
/// local connection is already gone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingUnpair {
    pub(crate) id: SharedString,
    pub(crate) server_name: SharedString,
    pub(crate) original_pairing: LocalPairingKey,
    pub(crate) operation: OperationId,
    pub(crate) phase: PendingUnpairPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingUnpairReason {
    Unreachable,
    Incompatible,
    RouteChanged,
    CredentialUnavailable,
    StorageUnavailable,
    OutcomeUnknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingUnpairPhase {
    AwaitingConfirmation(PendingUnpairReason),
    /// A verified receipt is durable; only exact local credential cleanup remains.
    CredentialCleanup,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ManagementComparison {
    EndAll(Box<ManagementAssessment>),
    Host(HostAssessment),
}

/// Canonical target captured when an action is presented, not resolved from later selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerRequestTarget {
    Server {
        pairing: LocalPairingKey,
        runtime_epoch: Option<ServerEpoch>,
        route: ServerRoute,
        /// A device inventory action cannot follow a replacement page revision.
        inventory_version: Option<FieldVersion>,
        assessment: Option<AssessmentToken>,
        management_comparison: Option<ManagementComparison>,
        remote_policy: Option<(
            FieldVersion,
            spaceterm_core::remote_access::RemoteAccessPolicy,
        )>,
    },
    PendingUnpair {
        pairing: LocalPairingKey,
        operation: OperationId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundServerRequest {
    pub(crate) target: ServerRequestTarget,
    pub(crate) request: ServerRequest,
}

impl BoundServerRequest {
    /// Captures current canonical scope. Receivers must still validate it against their registry.
    pub(crate) fn bind(request: ServerRequest, snapshot: &ServersSnapshot) -> Option<Self> {
        let target = if let Some(key) = request.server() {
            let server = snapshot.servers.iter().find(|server| &server.key == key)?;
            match &request {
                ServerRequest::ReadPairedDevices { .. }
                | ServerRequest::AssessUnpairDevice { .. }
                | ServerRequest::UnpairDevice { .. }
                    if !server.connected() =>
                {
                    return None;
                }
                ServerRequest::SetWorktreeLocation {
                    base_revision: 0, ..
                } => return None,
                ServerRequest::InstallUpdate {
                    version,
                    assessment,
                    ..
                } if !server.activation_matches(assessment)
                    || version != &assessment.candidate.version =>
                {
                    return None;
                }
                ServerRequest::ReadPairedDevices { after, version, .. }
                    if match (after, version) {
                        (None, None) => false,
                        (Some(after), Some(version)) => {
                            !server.paired_devices.as_ref().is_some_and(|page| {
                                page.valid_for(&server.pairing)
                                    && page.next == Some(*after)
                                    && page.version == *version
                            })
                        }
                        _ => true,
                    } =>
                {
                    return None;
                }
                ServerRequest::AssessUnpairDevice {
                    target, version, ..
                } if !server.paired_devices.as_ref().is_some_and(|page| {
                    page.valid_for(&server.pairing)
                        && page.version == *version
                        && page.devices.iter().any(|device| &device.target == target)
                }) =>
                {
                    return None;
                }
                ServerRequest::UnpairDevice {
                    target, assessment, ..
                } if target.parts().server != server.pairing.parts().server
                    || !matches!(assessment.action(), ManagementAction::Unpair { target: assessed } if assessed == target)
                    || server.unpair_assessment.as_ref() != Some(assessment.as_ref())
                    || !server.paired_devices.as_ref().is_some_and(|page| {
                        page.valid_for(&server.pairing)
                            && page.devices.iter().any(|device| &device.target == target)
                    }) =>
                {
                    return None;
                }
                _ => {}
            }
            ServerRequestTarget::Server {
                pairing: server.pairing.clone(),
                runtime_epoch: server.runtime_epoch,
                route: server.route.clone(),
                remote_policy: match &request {
                    ServerRequest::DisableRemoteAccess(_) => {
                        server.remote_access.as_ref().map(|access| {
                            (
                                access.version,
                                spaceterm_core::remote_access::RemoteAccessPolicy::disabled(
                                    server.pairing.parts().server,
                                ),
                            )
                        })
                    }
                    ServerRequest::EnableRemoteAccess { scope, .. } => {
                        server.remote_access.as_ref().and_then(|access| {
                            access
                                .scopes
                                .iter()
                                .find(|candidate| candidate.id == *scope)
                                .and_then(|candidate| {
                                    spaceterm_core::remote_access::RemoteAccessPolicy::enabled(
                                        server.pairing.parts().server,
                                        candidate.endpoints.clone(),
                                    )
                                    .ok()
                                    .map(|policy| (access.version, policy))
                                })
                        })
                    }
                    _ => None,
                },
                management_comparison: match &request {
                    ServerRequest::EndAllSessions(_) => server
                        .management
                        .end_all
                        .clone()
                        .map(|assessment| ManagementComparison::EndAll(Box::new(assessment))),
                    ServerRequest::Stop(_) => server
                        .management
                        .stop
                        .clone()
                        .map(ManagementComparison::Host),
                    ServerRequest::Restart(_) => server
                        .management
                        .restart
                        .clone()
                        .map(ManagementComparison::Host),
                    _ => None,
                },
                assessment: match &request {
                    ServerRequest::EndAllSessions(_) => server
                        .management
                        .end_all
                        .as_ref()
                        .map(|v| v.token().clone()),
                    ServerRequest::Stop(_) => {
                        server.management.stop.as_ref().map(|v| v.reference.clone())
                    }
                    ServerRequest::Restart(_) => server
                        .management
                        .restart
                        .as_ref()
                        .map(|v| v.reference.clone()),
                    _ => None,
                },
                inventory_version: match &request {
                    ServerRequest::AssessUnpairDevice { version, .. } => Some(*version),
                    ServerRequest::ReadPairedDevices { version, .. } => *version,
                    ServerRequest::UnpairDevice { .. } => {
                        server.paired_devices.as_ref().map(|page| page.version)
                    }
                    _ => None,
                },
            }
        } else {
            let ServerRequest::DismissUnpairNotice(id) = &request else {
                return None;
            };
            let pending = snapshot
                .pending_unpairs
                .iter()
                .find(|pending| &pending.id == id)?;
            if pending.operation.stream().pairing() != pending.original_pairing.parts().pairing {
                return None;
            }
            ServerRequestTarget::PendingUnpair {
                pairing: pending.original_pairing.clone(),
                operation: pending.operation,
            }
        };
        Some(Self { target, request })
    }

    pub(crate) fn still_matches(&self, snapshot: &ServersSnapshot) -> bool {
        Self::bind(self.request.clone(), snapshot).as_ref() == Some(self)
    }
}

/// Everything the Servers section presents at one moment.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ServersSnapshot {
    /// Every paired server, local first, in the order the client lists them.
    pub(crate) servers: Vec<ServerSummary>,
    /// Pending unpairs whose notice the person has not dismissed.
    pub(crate) pending_unpairs: Vec<PendingUnpair>,
}

/// One request from the Servers section. Each runs only after any confirmation it needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerRequest {
    Start(ServerKey),
    /// Read-only comparison prompts. They grant no execution authority.
    AssessStop(ServerKey),
    AssessRestart(ServerKey),
    AssessInstallUpdate(ServerKey),
    AssessEndAllSessions(ServerKey),
    /// Stops the server and suspends automatic startup until Start.
    Stop(ServerKey),
    /// Restarts the running version.
    Restart(ServerKey),
    /// Activates the staged version. A running server restarts; a stopped one stays stopped.
    InstallUpdate {
        server: ServerKey,
        version: SharedString,
        assessment: UpdateAssessment,
    },
    ReadPairedDevices {
        server: ServerKey,
        after: Option<PairingId>,
        version: Option<FieldVersion>,
    },
    /// Reads an assessment of the exact target from a revision-coherent device page.
    AssessUnpairDevice {
        server: ServerKey,
        target: PairingTarget,
        version: FieldVersion,
    },
    /// Unpairs the exact assessed lifetime. For the initiating device, the receiver first
    /// records restricted pending revocation and removes ordinary local use. Sending only
    /// the server command does not satisfy self-unpair's durable local-removal contract.
    UnpairDevice {
        server: ServerKey,
        target: PairingTarget,
        assessment: Box<ManagementAssessment>,
    },
    EndAllSessions(ServerKey),
    /// Removes this device's pairing now and revokes it at the server in the background.
    UnpairThisDevice(ServerKey),
    /// Changes the server's New Worktree Location. The receiver publishes the change as the
    /// setting's [`SettingEdit`] with `template` as its draft, from Saving to its outcome.
    SetWorktreeLocation {
        server: ServerKey,
        template: SharedString,
        /// The revision the person edited.
        base_revision: u64,
    },
    /// Drops this device's unapplied change and shows the server's value again.
    DiscardWorktreeLocationEdit(ServerKey),
    /// Turns remote connections on, or moves them, to the bind scope `scope`.
    EnableRemoteAccess {
        server: ServerKey,
        scope: SharedString,
    },
    /// Turns remote connections off, whatever scope they use.
    DisableRemoteAccess(ServerKey),
    /// Drops this client's rejected remote policy without changing server listeners.
    DiscardRemoteAccessEdit(ServerKey),
    /// Hides one pending-unpair notice. Revocation keeps retrying.
    DismissUnpairNotice(SharedString),
}

impl ServerRequest {
    /// The server the request acts on. A notice dismissal acts on no listed server.
    pub(crate) fn server(&self) -> Option<&ServerKey> {
        match self {
            Self::Start(server)
            | Self::AssessStop(server)
            | Self::AssessRestart(server)
            | Self::AssessInstallUpdate(server)
            | Self::AssessEndAllSessions(server)
            | Self::Stop(server)
            | Self::Restart(server)
            | Self::EndAllSessions(server)
            | Self::UnpairThisDevice(server)
            | Self::DiscardWorktreeLocationEdit(server)
            | Self::DisableRemoteAccess(server)
            | Self::DiscardRemoteAccessEdit(server)
            | Self::InstallUpdate { server, .. }
            | Self::ReadPairedDevices { server, .. }
            | Self::AssessUnpairDevice { server, .. }
            | Self::UnpairDevice { server, .. }
            | Self::SetWorktreeLocation { server, .. }
            | Self::EnableRemoteAccess { server, .. } => Some(server),
            Self::DismissUnpairNotice(_) => None,
        }
    }
}

/// Where the Servers section's requests go.
///
/// The receiver answers through the next [`ServersSnapshot`] it publishes, not through `send`.
/// Until a snapshot carries a New Worktree Location setting that differs from the one a change
/// went out against, the section keeps showing the sent text as being saved.
pub(crate) trait ServerRequests {
    fn send(&self, request: BoundServerRequest, cx: &mut App);
}

/// The published servers and the channel for requests about them.
pub(crate) struct Servers {
    snapshot: ServersSnapshot,
    requests: Rc<dyn ServerRequests>,
}

impl Servers {
    pub(crate) fn snapshot(&self) -> &ServersSnapshot {
        &self.snapshot
    }

    /// Replaces what the section presents.
    pub(crate) fn publish(&mut self, snapshot: ServersSnapshot, cx: &mut Context<Self>) {
        if self.snapshot != snapshot {
            self.snapshot = snapshot;
            cx.notify();
        }
    }

    pub(crate) fn requests(&self) -> Rc<dyn ServerRequests> {
        Rc::clone(&self.requests)
    }
}

/// The application's published servers, which the Settings window follows.
#[derive(Clone)]
pub(crate) struct InstalledServers(pub(crate) Entity<Servers>);

impl Global for InstalledServers {}

/// Installs the servers the Settings window presents and the receiver of its requests.
pub(crate) fn install(requests: Rc<dyn ServerRequests>, cx: &mut App) -> Entity<Servers> {
    let servers = cx.new(|_| Servers {
        snapshot: ServersSnapshot::default(),
        requests,
    });
    cx.set_global(InstalledServers(servers.clone()));
    servers
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
