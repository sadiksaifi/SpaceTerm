//! SSH server connections in the Servers Settings Section.
//!
//! The selected SSH server shows its state and the steps that state allows: Connect, Cancel,
//! Disconnect, Check Status, and Install after the person reviews what installing does. Its facts
//! come from the latest observation, which outlives Disconnect as the facts last observed. Every
//! row reads connection and maintenance snapshots, so rendering asks no computer anything. Add
//! Server owns connecting to a new host.

use gpui::{Context, Task, Window};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId,
};

use super::SettingsWindow;
use super::server_list::ServerChoice;
use crate::domain::SshDestination;
use crate::ssh::server_connection::{RemoteConnectionStatus, SshServerConnections};
use crate::ui::ssh_servers::{SshServerAction, installation_consequence, present};

pub(super) const INSTALL_CONFIRM_SELECTOR: &str = "settings-ssh-server-install-confirm";
pub(super) const INSTALL_CANCEL_SELECTOR: &str = "settings-ssh-server-install-cancel";

/// The SSH servers rows: the registry they present.
pub(super) struct SshServerRows {
    pub(super) connections: SshServerConnections,
    _changes: Task<()>,
}

impl SshServerRows {
    /// Follows the registry so each state change repaints the rows. Construction reads nothing.
    pub(super) fn new(connections: SshServerConnections, cx: &mut Context<SettingsWindow>) -> Self {
        let changes = connections.subscribe();
        // A server that becomes presented or withdrawn changes whose Server Settings the section
        // can read, so each change follows it as well as repainting.
        let follow = cx.spawn(async move |settings, cx| {
            while changes.recv().await.is_ok() {
                let followed = settings.update(cx, |settings, cx| {
                    settings.reconcile_servers(cx);
                    settings.sync_server_location(cx);
                    cx.notify();
                });
                if followed.is_err() {
                    break;
                }
            }
        });
        Self {
            connections,
            _changes: follow,
        }
    }
}

impl SettingsWindow {
    pub(super) fn run_ssh_server_action(
        &mut self,
        destination: SshDestination,
        action: SshServerAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.ssh_servers.as_ref() else {
            return;
        };
        let connections = rows.connections.clone();
        // The person asked here, so any sign-in or host approval this needs appears here.
        if matches!(
            action,
            SshServerAction::Connect | SshServerAction::Install | SshServerAction::CheckStatus
        ) {
            crate::ui::ssh_servers::present_prompts_in(window, cx);
        }
        match action {
            SshServerAction::Connect => {
                connections.connect(destination.clone());
                // The server the person just asked to connect to is the one they want to see.
                self.select_server(ServerChoice::Ssh(destination), cx);
            }
            SshServerAction::Cancel => connections.cancel(&destination),
            SshServerAction::Disconnect | SshServerAction::StopReconnecting => {
                connections.disconnect(&destination)
            }
            SshServerAction::CheckStatus => connections.check(&destination),
            SshServerAction::Remove => {
                connections.remove(&destination);
                // The selection gives way to this computer's server once the connection is gone.
                self.reconcile_servers(cx);
                self.sync_server_location(cx);
            }
            SshServerAction::Install => self.confirm_ssh_installation(destination, window, cx),
        }
        cx.notify();
    }

    /// Explains what installing the offered server does on that computer and installs it only
    /// once the person approves. The approval names the exact offer it was shown, so a later
    /// offer is never installed by this answer.
    fn confirm_ssh_installation(
        &mut self,
        destination: SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.ssh_servers.as_ref() else {
            return;
        };
        let connections = rows.connections.clone();
        let RemoteConnectionStatus::InstallationOffered {
            offer_id,
            version,
            platform,
        } = connections.status(&destination)
        else {
            return;
        };
        let title = "Install SpaceTerm Server?";
        let message = format!(
            "\u{201C}{}\u{201D} has no SpaceTerm server for this account. SpaceTerm will install \
             verified version {version} and connect to it.",
            destination.as_str()
        );
        let result = Alert::new(
            ModalId::new("settings-ssh-server-install"),
            title,
            title,
            message,
            vec![
                ModalAction::new(
                    true,
                    "Install",
                    ModalActionRole::Affirmative,
                    INSTALL_CONFIRM_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    INSTALL_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Informational)
        .detail(installation_consequence(&platform.0))
        .present(window, cx, move |outcome, _| {
            if matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                connections.install(&destination, offer_id);
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SSH server installation confirmation");
        }
    }

    /// The steps the selected SSH server's state allows. Check Status asks for a fresh observation
    /// only of a server SpaceTerm isn't presenting: a presented one reports its state already.
    pub(super) fn ssh_server_actions(
        &self,
        destination: &SshDestination,
        status: &RemoteConnectionStatus,
    ) -> Vec<SshServerAction> {
        let presented = self
            .ssh_servers
            .as_ref()
            .is_some_and(|rows| rows.connections.server(destination).is_some());
        let mut actions: Vec<_> = present(status)
            .actions
            .iter()
            .copied()
            .filter(|action| *action != SshServerAction::CheckStatus || !presented)
            .collect();
        let ends = |actions: &[SshServerAction]| {
            actions.iter().any(|action| {
                matches!(
                    action,
                    SshServerAction::Disconnect | SshServerAction::Cancel
                )
            })
        };
        // A presented server can always be withdrawn, such as one stopped after it connected.
        if presented && !ends(&actions) {
            actions.push(SshServerAction::Disconnect);
        }
        // A failed Connect still reconnects quietly after relaunch; offer to end that intent.
        if self.ssh_server_reconnects(destination) && !ends(&actions) {
            actions.push(SshServerAction::StopReconnecting);
        }
        actions
    }

    /// Whether SpaceTerm tries `destination` again on its own although nothing for it runs now.
    fn ssh_server_reconnects(&self, destination: &SshDestination) -> bool {
        self.ssh_servers
            .as_ref()
            .is_some_and(|rows| rows.connections.reconnects(destination))
    }
}
