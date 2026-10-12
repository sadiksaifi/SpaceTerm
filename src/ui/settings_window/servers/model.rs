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

use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};

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
    /// over SSH. A WSS connection reaches only the running server.
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
    /// Another reason, in the server's words.
    Other(SharedString),
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
}

/// Whether a server accepts SpaceTerm connections over the network. It is off until someone turns
/// it on for one server and one bind scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteAccess {
    pub(crate) enabled: bool,
    /// The scope the server listens on, or listened on last.
    pub(crate) scope: Option<SharedString>,
    /// The scopes the server offers.
    pub(crate) scopes: Vec<BindScope>,
    /// A change this device sent that the server has not confirmed.
    pub(crate) pending: bool,
}

/// One server this device is paired with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerSummary {
    pub(crate) key: ServerKey,
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
}

/// A self-unpair whose server-side revocation is still being retried in the background. The
/// local connection is already gone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingUnpair {
    pub(crate) id: SharedString,
    pub(crate) server_name: SharedString,
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
    /// Stops the server and suspends automatic startup until Start.
    Stop(ServerKey),
    /// Restarts the running version.
    Restart(ServerKey),
    /// Activates the staged version. A running server restarts; a stopped one stays stopped.
    InstallUpdate {
        server: ServerKey,
        version: SharedString,
    },
    EndAllSessions(ServerKey),
    /// Removes this device's pairing now and revokes it at the server in the background.
    UnpairThisDevice(ServerKey),
    SetWorktreeLocation {
        server: ServerKey,
        template: SharedString,
        /// The revision the person edited.
        base_revision: u64,
    },
    /// Drops this device's unapplied change and shows the server's value again.
    DiscardWorktreeLocationEdit(ServerKey),
    SetRemoteAccess {
        server: ServerKey,
        enabled: bool,
        scope: SharedString,
    },
    /// Hides one pending-unpair notice. Revocation keeps retrying.
    DismissUnpairNotice(SharedString),
}

/// Where the Servers section's requests go.
pub(crate) trait ServerRequests {
    fn send(&self, request: ServerRequest, cx: &mut App);
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
