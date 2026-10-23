//! SSH server connections in the Servers Settings Section.
//!
//! The selected SSH server shows its state and the steps that state allows: Connect, Cancel,
//! Disconnect, Check Status, and Install after the person reviews what installing does. Its facts
//! come from the latest observation, which outlives Disconnect as the facts last observed. Every
//! row reads connection and maintenance snapshots, so rendering asks no computer anything. The
//! host list reads this computer's SSH configuration when the section opens.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, ElementId, Entity, SharedString, Subscription, Task, Window, accesskit,
    div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, FieldState, Menu,
    MenuEntry, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId, TextInput,
    TextInputEvent, TextInputVariant,
};

use super::SettingsWindow;
use super::server_list::ServerChoice;
use crate::domain::SshDestination;
use crate::ssh::server_connection::{RemoteConnectionStatus, SshServerConnections};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::remote_maintenance::operations_persistence_warning;
use crate::ui::ssh_servers::{
    SshHostSource, SshServerAction, installation_consequence, persistence_warning, present,
};

pub(super) const CONTROL_SELECTOR: &str = "settings-ssh-servers";
pub(super) const CONNECT_MENU_SELECTOR: &str = "settings-ssh-server-connect-menu";
pub(super) const DESTINATION_SELECTOR: &str = "settings-ssh-server-destination";
pub(super) const DESTINATION_CONNECT_SELECTOR: &str = "settings-ssh-server-destination-connect";
/// Prefixes each host in the SSH configuration menu, followed by its position.
pub(super) const CONNECT_CHOICE_SELECTOR: &str = "settings-ssh-server-connect-choice";
pub(super) const INSTALL_CONFIRM_SELECTOR: &str = "settings-ssh-server-install-confirm";
pub(super) const INSTALL_CANCEL_SELECTOR: &str = "settings-ssh-server-install-cancel";
pub(super) const PERSISTENCE_SELECTOR: &str = "settings-ssh-servers-persistence";
/// The notice that this client couldn't read or write its own record of the maintenance
/// operations it submitted. It is a different record from the saved connections above it, so it
/// is a notice of its own that says only what that record changes.
pub(super) const OPERATIONS_PERSISTENCE_SELECTOR: &str = "settings-ssh-operations-persistence";
pub(super) const UNSAVED_REMOVAL_SELECTOR: &str = "settings-ssh-server-unsaved-removal";
pub(super) const REMOVE_AGAIN_SELECTOR: &str = "settings-ssh-server-remove-again";
/// The unsaved-removal notice's name for assistive technology.
pub(super) const UNSAVED_REMOVAL_NAME: &str = "Removal not saved";

/// The SSH servers row: the registry it presents, and the hosts SSH configuration names.
pub(super) struct SshServerRows {
    pub(super) connections: SshServerConnections,
    hosts: Option<Arc<dyn SshHostSource>>,
    /// The hosts read when the section last opened. `None` until the first reading.
    configured: Option<Vec<SshDestination>>,
    /// A destination the person types: a host name, an SSH alias, or `user@host`.
    destination: Entity<TextInput>,
    /// Whether the typed destination was refused when the person last asked to connect.
    invalid: bool,
    _changes: Task<()>,
    _reading: Option<Task<()>>,
    _destination_events: Subscription,
}

impl SshServerRows {
    /// Follows the registry so each state change repaints the row. Construction reads nothing.
    pub(super) fn new(
        connections: SshServerConnections,
        hosts: Option<Arc<dyn SshHostSource>>,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let destination = cx.new(|cx| {
            TextInput::new(DESTINATION_SELECTOR, "SSH host", String::new(), window, cx)
                .variant(TextInputVariant::Bare)
                .placeholder("Host or SSH alias")
                .input_length_limit(Some(1024))
                .debug_selector(DESTINATION_SELECTOR)
        });
        let destination_events = cx.subscribe_in(
            &destination,
            window,
            |settings, _, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Submitted => settings.connect_typed_destination(window, cx),
                TextInputEvent::ValueChanged(_) => {
                    if let Some(rows) = settings.ssh_servers.as_mut()
                        && rows.invalid
                    {
                        rows.invalid = false;
                        cx.notify();
                    }
                }
                _ => {}
            },
        );
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
            hosts,
            configured: None,
            destination,
            invalid: false,
            _changes: follow,
            _reading: None,
            _destination_events: destination_events,
        }
    }

    /// The field where the person types a host to connect to.
    pub(super) fn destination(&self) -> &Entity<TextInput> {
        &self.destination
    }

    /// The destinations the SSH Hosts menu offers: every configured host not already listed.
    fn offered(&self, listed: &[(SshDestination, RemoteConnectionStatus)]) -> Vec<SshDestination> {
        self.configured
            .iter()
            .flatten()
            .filter(|host| !listed.iter().any(|(listed, _)| listed == *host))
            .cloned()
            .collect()
    }
}

impl SettingsWindow {
    /// Reads the SSH configuration's hosts in the background when the Servers section opens.
    pub(super) fn read_ssh_hosts(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.ssh_servers.as_mut() else {
            return;
        };
        let Some(hosts) = rows.hosts.clone() else {
            return;
        };
        let reading = cx.background_spawn(async move { hosts.hosts() });
        rows._reading = Some(cx.spawn(async move |settings, cx| {
            let configured = reading.await;
            let _ = settings.update(cx, |settings, cx| {
                if let Some(rows) = settings.ssh_servers.as_mut() {
                    rows.configured = Some(configured);
                    cx.notify();
                }
            });
        }));
    }

    /// Connects to the destination the person typed. The text passes to SSH as one destination
    /// argument, so SSH configuration still decides the host, user, and keys.
    fn connect_typed_destination(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rows) = self.ssh_servers.as_mut() else {
            return;
        };
        let typed = rows.destination.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        let Ok(destination) = SshDestination::new(typed) else {
            rows.invalid = true;
            cx.notify();
            return;
        };
        rows.invalid = false;
        rows.destination.update(cx, |input, cx| {
            input.clear(cx);
        });
        self.run_ssh_server_action(destination, SshServerAction::Connect, window, cx);
    }

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

    /// The field and SSH Hosts menu that connect to another computer, under the server list.
    pub(super) fn render_ssh_connect(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(rows) = self.ssh_servers.as_ref() else {
            return div().into_any_element();
        };
        let listed = rows.connections.connections();
        let unsaved = rows.connections.unsaved_removals();
        // A removal that wasn't saved says exactly what returns; the general notice would only
        // repeat it less precisely.
        let persistence = rows
            .connections
            .persistence_error()
            .filter(|_| unsaved.is_empty());
        // The record of submitted operations is this client's own, so its failure says only what
        // it changes: a step that clears a finished operation, and what survives a relaunch.
        let operations = self
            .ssh_maintenance
            .as_ref()
            .and_then(|rows| rows.operations.persistence_error());
        let offered = rows.offered(&listed);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let owner = cx.weak_entity();
        let empty = listed.is_empty();
        let invalid = rows.invalid;
        let entries: Vec<MenuEntry<SshDestination>> = offered
            .iter()
            .enumerate()
            .map(|(index, destination)| {
                MenuEntry::action(destination.as_str().to_owned(), destination.clone())
                    .debug_selector(format!("{CONNECT_CHOICE_SELECTOR}-{index}"))
            })
            .collect();
        let configured = !entries.is_empty();
        let menu_owner = owner.clone();
        // A host from SSH configuration connects as soon as the person chooses it.
        let menu = configured.then(|| {
            Menu::new(ElementId::from(CONNECT_MENU_SELECTOR), "SSH Hosts", entries)
                .debug_selector(CONNECT_MENU_SELECTOR)
                .on_activate(move |activation, window, cx| {
                    let destination = activation.action().clone();
                    let _ = menu_owner.update(cx, |settings, cx| {
                        settings.run_ssh_server_action(
                            destination,
                            SshServerAction::Connect,
                            window,
                            cx,
                        );
                    });
                })
        });
        let input = rows.destination.clone();
        let focus = input.read(cx).focus_handle();
        let typed = !input.read(cx).value().trim().is_empty();
        let field = spaceterm_ui::field_frame(
            "settings-ssh-server-destination-frame",
            &focus,
            FieldState::default().invalid(invalid),
            RadiusRole::Control.pixels(),
            cx,
        )
        .h(appearance.height(28.0, 13.0))
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .px(appearance.spacing(8.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(colors.text))
        .on_click(move |_, window, cx| {
            focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(input);
        let connect_owner = owner.clone();
        let button = Button::new(DESTINATION_CONNECT_SELECTOR, "Connect")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .disabled(!typed)
            .tab_stop(true)
            .debug_selector(DESTINATION_CONNECT_SELECTOR)
            .accessibility_description("Connect to the SSH host you typed")
            .on_activate(move |_, window, cx| {
                let _ = connect_owner.update(cx, |settings, cx| {
                    settings.connect_typed_destination(window, cx);
                });
            });
        let guidance: SharedString = if invalid {
            "Enter one SSH host or alias, such as penguin or me@build.local, without spaces \
             or options."
                .into()
        } else if empty {
            "SpaceTerm signs in the way SSH does with your SSH configuration, then uses that \
             account\u{2019}s SpaceTerm server or offers to install one."
                .into()
        } else {
            "Connect to another computer by its host name or SSH alias.".into()
        };
        let guidance_color = if invalid {
            colors.error
        } else {
            colors.text_muted
        };
        // A retained-intent failure changes what happens at the next launch, so it stays visible
        // until SpaceTerm saves the record again.
        let persistence = persistence.map(|error| {
            div()
                .id(PERSISTENCE_SELECTOR)
                .debug_selector(|| PERSISTENCE_SELECTOR.to_owned())
                .role(accesskit::Role::Status)
                .aria_label(SharedString::new_static(persistence_warning(error)))
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.warning))
                .whitespace_normal()
                .child(persistence_warning(error))
        });
        let operations = operations.map(|error| {
            let notice = operations_persistence_warning(error);
            div()
                .id(OPERATIONS_PERSISTENCE_SELECTOR)
                .debug_selector(|| OPERATIONS_PERSISTENCE_SELECTOR.to_owned())
                .role(accesskit::Role::Status)
                .aria_label(SharedString::new_static(notice))
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.warning))
                .whitespace_normal()
                .child(notice)
        });
        let unsaved = (!unsaved.is_empty()).then(|| {
            let text = unsaved_removal_warning(&unsaved);
            let again_owner = owner.clone();
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .child(
                    div()
                        .id(UNSAVED_REMOVAL_SELECTOR)
                        .debug_selector(|| UNSAVED_REMOVAL_SELECTOR.to_owned())
                        .role(accesskit::Role::Status)
                        .aria_label(UNSAVED_REMOVAL_NAME)
                        .aria_description(SharedString::from(text.clone()))
                        .flex_1()
                        .min_w_0()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.warning))
                        .whitespace_normal()
                        .child(text),
                )
                .child(
                    Button::new(REMOVE_AGAIN_SELECTOR, "Remove Again")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Regular)
                        .tab_stop(true)
                        .debug_selector(REMOVE_AGAIN_SELECTOR)
                        .accessibility_description("Save the removal of these connections again")
                        .on_activate(move |_, _, cx| {
                            let _ = again_owner.update(cx, |settings, cx| {
                                if let Some(rows) = settings.ssh_servers.as_ref() {
                                    for destination in rows.connections.unsaved_removals() {
                                        rows.connections.remove(&destination);
                                    }
                                }
                                cx.notify();
                            });
                        }),
                )
        });
        div()
            .debug_selector(|| CONTROL_SELECTOR.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(4.0))
            .children(unsaved)
            .children(persistence)
            .children(operations)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(8.0))
                    .child(field)
                    .children(menu)
                    .child(button),
            )
            .child(
                div()
                    .debug_selector(|| "settings-ssh-server-destination-description".to_owned())
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(guidance_color))
                    .whitespace_normal()
                    .child(guidance),
            )
            .into_any_element()
    }
}

/// Names each removed connection whose removal wasn't saved and what that changes next time.
fn unsaved_removal_warning(unsaved: &[SshDestination]) -> String {
    let names = unsaved
        .iter()
        .map(|destination| format!("\u{201C}{}\u{201D}", destination.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SpaceTerm couldn\u{2019}t save that you removed {names}. It may appear again and \
         reconnect on its own when SpaceTerm reopens."
    )
}
