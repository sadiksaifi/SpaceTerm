//! The words the Servers section uses for each server state and each confirmation, derived from
//! the published state alone so every state reads one way.

use gpui::SharedString;
use spaceterm_ui::IconName;

use super::model::{
    BindScope, IncompatibleSide, PendingUnpair, PendingUnpairPhase, PendingUnpairReason,
    ServerPermissionKind, ServerPermissionState, ServerRoute, ServerServiceLifecycle, ServerStatus,
    ServerSummary, SettingRejection,
};
use crate::platform::permission_access::AccessibilityNaming;
use crate::worktrees::path_template::WorktreePathTemplateError;

/// How a status glyph is tinted. The status text always accompanies it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatusTone {
    Positive,
    Neutral,
    Attention,
}

/// The short status a list row and the Status row show beside the glyph.
pub(super) fn status_label(status: &ServerStatus) -> SharedString {
    match status {
        ServerStatus::Connecting => "Connecting…".into(),
        ServerStatus::Running => "Running".into(),
        ServerStatus::Starting => "Starting…".into(),
        ServerStatus::Stopping => "Stopping…".into(),
        ServerStatus::Stopped => "Stopped".into(),
        ServerStatus::Offline => "Offline".into(),
        ServerStatus::Incompatible(_) | ServerStatus::UnsupportedProfiles => "Incompatible".into(),
        ServerStatus::Updating { version } => format!("Updating to {version}…").into(),
    }
}

pub(super) fn status_icon(status: &ServerStatus) -> IconName {
    match status {
        ServerStatus::Running => IconName::CircleCheck,
        ServerStatus::Connecting | ServerStatus::Starting | ServerStatus::Stopping => {
            IconName::CircleDashed
        }
        ServerStatus::Stopped => IconName::CircleStop,
        ServerStatus::Offline => IconName::CloudOff,
        ServerStatus::Incompatible(_) | ServerStatus::UnsupportedProfiles => {
            IconName::TriangleAlert
        }
        ServerStatus::Updating { .. } => IconName::CircleFadingArrowUp,
    }
}

pub(super) fn status_tone(status: &ServerStatus) -> StatusTone {
    match status {
        ServerStatus::Running => StatusTone::Positive,
        ServerStatus::Incompatible(_) | ServerStatus::UnsupportedProfiles => StatusTone::Attention,
        _ => StatusTone::Neutral,
    }
}

/// What the status means for the person and what they can do about it.
pub(super) fn status_explanation(server: &ServerSummary) -> SharedString {
    let name = &server.name;
    match &server.status {
        ServerStatus::Connecting => format!("Connecting to {name}.").into(),
        ServerStatus::Running if server.route.is_local() => {
            match server.service_lifecycle {
                Some(ServerServiceLifecycle::LoginSession) => "Its Terminal Sessions keep running when SpaceTerm quits. The server starts at login and stops at logout. Installing and managing it does not require a password.".into(),
                Some(ServerServiceLifecycle::BootWithLingering) => "Its Terminal Sessions keep running when SpaceTerm quits. With lingering enabled, the server starts at boot and keeps running after logout. Enabling lingering asks for administrator authorization once. Start, Stop, Restart, and updates do not require a password.".into(),
                None => "Its Terminal Sessions keep running when SpaceTerm quits.".into(),
            }
        }
        ServerStatus::Running if server.route.manages_host() => {
            "Its Terminal Sessions keep running when this device disconnects.".into()
        }
        // Stopping needs the host's authorization, which a WSS connection cannot obtain.
        ServerStatus::Running => {
            "Its Terminal Sessions keep running when this device disconnects. To stop it, connect over SSH or use its machine.".into()
        }
        ServerStatus::Starting => format!("{name} is starting.").into(),
        ServerStatus::Stopping => {
            format!("{name} is stopping, and its Terminal Sessions are ending.").into()
        }
        ServerStatus::Stopped if server.route.manages_host() => {
            "It stays stopped, including after its machine restarts, until you start it.".into()
        }
        ServerStatus::Stopped => {
            "It stays stopped until someone starts it on its machine or over SSH.".into()
        }
        ServerStatus::Offline if server.route.is_local() => {
            format!("SpaceTerm can't reach {name}. It keeps trying.").into()
        }
        ServerStatus::Offline => format!(
            "SpaceTerm can't reach {name}. Its machine or network may be down. SpaceTerm keeps trying."
        )
        .into(),
        ServerStatus::Incompatible(IncompatibleSide::ThisDevice) => format!(
            "{name} needs a newer SpaceTerm on this device. Update SpaceTerm in Updates. SpaceTerm restarts, and {name}'s Terminal Sessions keep running."
        )
        .into(),
        ServerStatus::Incompatible(IncompatibleSide::Server) => format!(
            "{name} runs an older SpaceTerm Server than this device can use. Update {name} to connect. It restarts, and its Terminal Sessions end for every client."
        )
        .into(),
        ServerStatus::UnsupportedProfiles => format!(
            "{name} and this device support different protocol profiles. Their verified profiles do not establish which side needs updating. Other compatible servers remain available."
        ).into(),
        ServerStatus::Updating { version } => {
            format!("Installing version {version}. {name} restarts when it finishes.").into()
        }
    }
}

/// How this device reaches the server, as the list row's second line names it.
pub(super) fn route_summary(route: &ServerRoute) -> SharedString {
    match route {
        ServerRoute::Local => "Local".into(),
        ServerRoute::Ssh { destination } => format!("SSH · {destination}").into(),
        ServerRoute::Wss { authority } => format!("WSS · {authority}").into(),
    }
}

/// How this device reaches the server, as the Connection row states it.
pub(super) fn route_description(route: &ServerRoute) -> SharedString {
    match route {
        ServerRoute::Local => "Private local connection".into(),
        ServerRoute::Ssh { destination } => format!("SSH to {destination}").into(),
        ServerRoute::Wss { authority } => format!("Secure WebSocket to {authority}").into(),
    }
}

/// The list row's second line: the route, then the version when it is known.
pub(super) fn list_detail(server: &ServerSummary) -> SharedString {
    match &server.version {
        Some(version) => format!("{} · Version {version}", route_summary(&server.route)).into(),
        None => route_summary(&server.route),
    }
}

/// The accessible description of one list row, which carries what the row shows beside its name.
pub(super) fn list_description(server: &ServerSummary) -> SharedString {
    format!("{}, {}", status_label(&server.status), list_detail(server)).into()
}

pub(super) fn permission_name(
    kind: ServerPermissionKind,
    naming: AccessibilityNaming,
) -> &'static str {
    match (kind, naming) {
        (ServerPermissionKind::Microphone, _) => "Microphone",
        (ServerPermissionKind::ScreenRecording, _) => "Screen recording",
        (ServerPermissionKind::Accessibility, AccessibilityNaming::Accessibility) => {
            "Accessibility"
        }
        (ServerPermissionKind::Accessibility, AccessibilityNaming::DeviceControl) => {
            "Device control"
        }
    }
}

pub(super) fn permission_state(state: ServerPermissionState) -> &'static str {
    match state {
        ServerPermissionState::Allowed => "Allowed",
        ServerPermissionState::NotAllowed => "Not Allowed",
        ServerPermissionState::NotSetUp => "Not Set Up",
        ServerPermissionState::NotNeeded => "Not Needed",
    }
}

/// Says whose permissions these are. Grants this app holds on this device never apply to the
/// server, even on the same machine.
pub(super) fn permissions_explanation(server: &ServerSummary) -> SharedString {
    let name = &server.name;
    if server.route.is_local() {
        format!(
            "Programs in {name}'s Terminal Sessions use the SpaceTerm Server's own permissions. Permissions you gave this app don't carry over."
        )
        .into()
    } else {
        format!(
            "Programs in {name}'s Terminal Sessions use the SpaceTerm Server's permissions on its machine. Permissions on this device don't carry over."
        )
        .into()
    }
}

/// Why a server refused a New Worktree Location, in the words the Git section uses for the same
/// template problems.
pub(super) fn rejection_caption(name: &str, rejection: &SettingRejection) -> SharedString {
    match rejection {
        SettingRejection::InvalidTemplate(problem) => format!(
            "{name} didn't accept this location. {}",
            template_problem(*problem)
        )
        .into(),
        SettingRejection::Unwritable => format!(
            "{name} can't create folders there. Choose a location its account can write to."
        )
        .into(),
        SettingRejection::NotSaved => {
            format!("{name} couldn't save this location. Try again.").into()
        }
    }
}

fn template_problem(problem: WorktreePathTemplateError) -> SharedString {
    super::super::git::worktree_location_caption("", Some(problem)).0
}

/// The caption shown when another device changed the setting before this device's change arrived.
pub(super) fn conflict_caption(current: &str) -> SharedString {
    format!(
        "Another device changed this to \"{current}\" first. Your change wasn't saved and is still in the field."
    )
    .into()
}

/// One confirmation's complete text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Confirmation {
    /// Names the modal and prefixes its action selectors.
    pub(super) id: &'static str,
    pub(super) title: SharedString,
    pub(super) message: SharedString,
    pub(super) detail: Option<SharedString>,
    pub(super) confirm: &'static str,
    /// Whether confirming ends work, which colors the action and makes Cancel the default.
    pub(super) destructive: bool,
}

/// What survives a server restart, shared by Restart and an update that restarts.
fn restart_detail(name: &str) -> SharedString {
    format!(
        "{name} keeps its identity, its paired devices, and its saved Workspaces, Tabs, and Panes. Each Pane starts a fresh shell when you next use it."
    )
    .into()
}

/// Only a route that manages the host offers Stop, so this device can always start the server
/// again.
pub(super) fn stop_confirmation(server: &ServerSummary) -> Confirmation {
    let name = &server.name;
    Confirmation {
        id: "settings-server-stop",
        title: format!("Stop {name}?").into(),
        message: format!(
            "Every Terminal Session on {name} ends for every client. {name} stays stopped, including after its machine restarts, until you start it again."
        )
        .into(),
        detail: Some("Its saved Workspaces, Tabs, and Panes remain.".into()),
        confirm: "Stop Server",
        destructive: true,
    }
}

pub(super) fn restart_confirmation(server: &ServerSummary) -> Confirmation {
    let name = &server.name;
    Confirmation {
        id: "settings-server-restart",
        title: format!("Restart {name}?").into(),
        message: format!("Every Terminal Session on {name} ends for every client.").into(),
        detail: Some(restart_detail(name)),
        confirm: "Restart Server",
        destructive: true,
    }
}

/// Activating a staged version restarts a running server. A stopped server stays stopped, so
/// nothing ends.
pub(super) fn update_confirmation(server: &ServerSummary, version: &str) -> Confirmation {
    let name = &server.name;
    if server.status == ServerStatus::Stopped {
        return Confirmation {
            id: "settings-server-update",
            title: format!("Update {name} to Version {version}?").into(),
            message: format!("{name} is stopped and stays stopped after the update.").into(),
            detail: Some(
                "Its identity, paired devices, and saved Workspaces, Tabs, and Panes remain."
                    .into(),
            ),
            confirm: "Install Update",
            destructive: false,
        };
    }
    Confirmation {
        id: "settings-server-update",
        title: format!("Update {name} to Version {version}?").into(),
        message: format!(
            "{name} restarts to finish the update. Every Terminal Session on {name} ends for every client."
        )
        .into(),
        detail: Some(restart_detail(name)),
        confirm: "Update and Restart",
        destructive: true,
    }
}

pub(super) fn end_all_sessions_confirmation(server: &ServerSummary) -> Confirmation {
    let name = &server.name;
    Confirmation {
        id: "settings-server-end-all-sessions",
        title: format!("End All Sessions on {name}?").into(),
        message: format!(
            "Every live Terminal Session on {name} ends for every client, and its terminal output and scrollback are cleared."
        )
        .into(),
        detail: Some(
            format!(
                "{name} keeps running, and its saved Workspaces, Tabs, and Panes remain. Repository files and Git Worktree directories stay on disk."
            )
            .into(),
        ),
        confirm: "End All Sessions",
        destructive: true,
    }
}

pub(super) fn unpair_confirmation(server: &ServerSummary) -> Confirmation {
    let name = &server.name;
    Confirmation {
        id: "settings-server-unpair",
        title: format!("Unpair This Device from {name}?").into(),
        message: format!(
            "This device stops using {name} now. {name}'s Workspaces leave this device's windows, and {name} leaves this list."
        )
        .into(),
        detail: Some(
            format!(
                "{name} and its Terminal Sessions keep running for other devices. If {name} can't be reached, SpaceTerm keeps asking it to revoke this device in the background. To use {name} again, pair this device with it again."
            )
            .into(),
        ),
        confirm: "Unpair",
        destructive: true,
    }
}

pub(super) fn enable_remote_access_confirmation(
    server: &ServerSummary,
    scope: &BindScope,
) -> Confirmation {
    let name = &server.name;
    Confirmation {
        id: "settings-server-remote-access-enable",
        title: format!("Allow Remote Connections to {name}?").into(),
        message: format!(
            "{name} accepts SpaceTerm connections over the network on {} ({}). Paired devices can then connect without SSH.",
            scope.label, scope.addresses
        )
        .into(),
        detail: Some(
            format!(
                "This stays on, including after {name} restarts or updates, until you turn it off."
            )
            .into(),
        ),
        confirm: "Allow",
        destructive: false,
    }
}

/// Moving remote connections disconnects every device on the addresses left behind, which can
/// include this device when it connects over WSS.
pub(super) fn change_scope_confirmation(server: &ServerSummary, scope: &BindScope) -> Confirmation {
    let name = &server.name;
    let detail = if server.route.manages_host() {
        format!("Terminal Sessions on {name} keep running.")
    } else {
        format!(
            "This device connects to {name} over the network. If its address isn't in {}, it disconnects too. Terminal Sessions on {name} keep running.",
            scope.label
        )
    };
    Confirmation {
        id: "settings-server-remote-access-scope",
        title: format!("Listen on {}?", scope.label).into(),
        message: format!(
            "{name} accepts SpaceTerm connections over the network on {} instead. Devices connected on other addresses disconnect.",
            scope.addresses
        )
        .into(),
        detail: Some(detail.into()),
        confirm: "Change",
        destructive: true,
    }
}

/// Turning remote connections off disconnects every device connected over the network, including
/// this device when it connects over WSS.
pub(super) fn disable_remote_access_confirmation(server: &ServerSummary) -> Confirmation {
    let name = &server.name;
    let (message, detail) = if server.route.manages_host() {
        (
            format!(
                "{name} stops accepting network connections. Devices connected to {name} over the network disconnect."
            ),
            format!("Terminal Sessions on {name} keep running."),
        )
    } else {
        (
            format!(
                "{name} stops accepting network connections. Devices connected to {name} over the network disconnect, including this one."
            ),
            format!(
                "Terminal Sessions on {name} keep running. To turn remote connections on again, connect to {name} over SSH or on its machine."
            ),
        )
    };
    Confirmation {
        id: "settings-server-remote-access-disable",
        title: format!("Turn Off Remote Connections to {name}?").into(),
        message: message.into(),
        detail: Some(detail.into()),
        confirm: "Turn Off",
        destructive: true,
    }
}

/// The notice for one pending unpair. Dismissing it leaves the background revocation running.
pub(super) fn pending_unpair_text(pending: &PendingUnpair) -> (SharedString, SharedString) {
    let name = &pending.server_name;
    match pending.phase {
        PendingUnpairPhase::AwaitingConfirmation(reason) => {
            let reason = match reason {
                PendingUnpairReason::Unreachable => "The server can't be reached.",
                PendingUnpairReason::Incompatible => "This device and the server cannot use one protocol.",
                PendingUnpairReason::RouteChanged => "The saved route no longer identifies the original server.",
                PendingUnpairReason::CredentialUnavailable => "The required credential is unavailable.",
                PendingUnpairReason::StorageUnavailable => "The revocation record is unavailable.",
                PendingUnpairReason::OutcomeUnknown => "The server's reply could not be verified.",
            };
            (
                format!("Unpair pending for {name}").into(),
                format!("This device's connection to {name} was removed. {reason} SpaceTerm keeps asking {name} to confirm in the background, including after you dismiss this.").into(),
            )
        }
        PendingUnpairPhase::CredentialCleanup => (
            format!("Unpair cleanup pending for {name}").into(),
            format!("{name} confirmed revocation. SpaceTerm is removing only this pairing's retained credential in the background, including after you dismiss this.").into(),
        ),
    }
}

/// Fixed classifications only. Native errors and provider text never enter presentation.
pub(super) fn management_failure(
    failure: spaceterm_client::management_cli::ManagementFailure,
) -> SharedString {
    use spaceterm_client::management_cli::ManagementFailure as Failure;
    match failure {
        Failure::StaleAssessment | Failure::ConfirmationRequired => "The affected work changed or the confirmation expired. Review the action again before confirming.",
        Failure::OutcomePending | Failure::Indeterminate => "The original request is still being resolved. No replacement request will be sent.",
        Failure::Offline => "The server is offline. Reconnect before reviewing this action.",
        Failure::Incompatible => "The protocol is incompatible. Check which client or server needs updating before trying this action.",
        Failure::StorageLocked => "Storage is locked. Unlock it before reviewing this action.",
        Failure::StorageUnavailable | Failure::IoUnavailable => "Storage or the service connection is unavailable. The action was not confirmed.",
        Failure::PairingRequired | Failure::Revoked => "This pairing is no longer authorized. Pair deliberately before using this server.",
        Failure::AuthorizationDenied => "Authorization was denied. The action was not confirmed.",
        Failure::AuthorizationUnavailable | Failure::NativeUnavailable | Failure::UnsupportedHost => "This connection cannot manage the server's account service. Use its machine or an authorized SSH connection.",
        Failure::CredentialUnavailable => "The pairing credential is unavailable. The action was not confirmed.",
        Failure::UnsafeNativeState => "The server's saved service state could not be verified. The action was not confirmed.",
        Failure::LimitExceeded => "The server cannot accept this request now. No replacement request was sent.",
        Failure::Usage => "The request was not accepted. Review the action again before confirming.",
    }.into()
}
