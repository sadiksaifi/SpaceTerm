//! SSH server connections in the Servers Settings Section.
//!
//! The selected SSH server shows its state and the steps that state allows: Connect, Cancel,
//! Disconnect, Check Status, and Install after the person reviews what installing does. Its facts
//! come from the latest observation, which outlives Disconnect as the facts last observed. Every
//! row reads the registry's snapshot, so rendering asks no computer anything. The host list reads
//! this computer's SSH configuration when the section opens.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, ElementId, Entity, SharedString, Subscription, Task, Window, accesskit,
    div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, FieldState, Menu,
    MenuEntry, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId, ProgressRing,
    ProgressSize, ProgressState, TextInput, TextInputEvent, TextInputVariant,
};

use super::SettingsWindow;
use super::catalog::SettingsRowId;
use super::server_list::ServerChoice;
use crate::domain::SshDestination;
use crate::ssh::server_connection::{
    IncompatibilityKind, RemoteConnectionStatus, ServerAvailability, SshServerConnections,
    SshServerFacts,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{CaptionTone, badge};
use crate::ui::ssh_servers::{
    SshHostSource, SshServerAction, installation_consequence, persistence_warning, platform_name,
    present,
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
pub(super) const STATUS_SELECTOR: &str = "settings-ssh-server-status";
pub(super) const REMOVE_SELECTOR: &str = "settings-ssh-server-remove";

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

    fn run_ssh_server_action(
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
            SshServerAction::Disconnect => connections.disconnect(&destination),
            SshServerAction::CheckStatus => connections.check(&destination),
            SshServerAction::Remove => {
                let next = self.server_after_removing(&destination);
                connections.remove(&destination);
                self.selected_server = next;
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

    /// The selected SSH server and its latest status, read from the registry's snapshot.
    fn selected_ssh_server(&self) -> Option<(SshDestination, RemoteConnectionStatus)> {
        let Some(ServerChoice::Ssh(destination)) = self.selected_server() else {
            return None;
        };
        let rows = self.ssh_servers.as_ref()?;
        let status = rows.connections.status(&destination);
        Some((destination, status))
    }

    /// One SSH detail row's value and caption for the selected server, from its latest status
    /// and the facts the registry last observed.
    fn selected_ssh_server_fact(
        &self,
        row: SettingsRowId,
    ) -> Option<(SharedString, Option<SharedString>)> {
        let (destination, status) = self.selected_ssh_server()?;
        let last = self
            .ssh_servers
            .as_ref()
            .and_then(|rows| rows.connections.last_facts(&destination));
        Some(ssh_server_fact(row, &status, last.as_ref()))
    }

    /// The steps the selected SSH server's state allows. Check Status asks for a fresh observation
    /// only of a server SpaceTerm isn't presenting: a presented one reports its state already.
    fn ssh_server_actions(
        &self,
        destination: &SshDestination,
        status: &RemoteConnectionStatus,
    ) -> Vec<SshServerAction> {
        let presented = self
            .ssh_servers
            .as_ref()
            .is_some_and(|rows| rows.connections.server(destination).is_some());
        present(status)
            .actions
            .iter()
            .copied()
            .filter(|action| *action != SshServerAction::CheckStatus || !presented)
            .collect()
    }

    /// The selected SSH server's state, announced as it changes, and the steps it allows.
    pub(super) fn render_ssh_server_status(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((destination, status)) = self.selected_ssh_server() else {
            return div().into_any_element();
        };
        let presentation = present(&status);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let state_selector = presentation.state_selector;
        let state = if presentation.working {
            ProgressRing::new(
                SharedString::from(format!("{STATUS_SELECTOR}-progress")),
                presentation.state,
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(format!("{STATUS_SELECTOR}-progress"))
            .into_any_element()
        } else {
            div()
                .debug_selector(move || state_selector.to_owned())
                .child(badge(state_selector, presentation.state, appearance))
                .into_any_element()
        };
        // Assistive technology announces the state when it changes, as a status.
        let state = div()
            .id(SharedString::from(format!("{STATUS_SELECTOR}-state")))
            .role(accesskit::Role::Status)
            .aria_label(SharedString::from(format!(
                "{}: {}",
                destination.as_str(),
                presentation.state
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .child(state)
            // Progress names its phase beside the ring, because the ring alone says nothing.
            .when(presentation.working, |state| {
                state.child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.text_muted))
                        .child(presentation.state),
                )
            });
        let actions = self.ssh_server_actions(&destination, &status);
        let leading = actions.first().copied();
        let owner = cx.weak_entity();
        let buttons = actions.into_iter().map(|action| {
            let owner = owner.clone();
            let destination = destination.clone();
            let primary = leading == Some(action)
                && matches!(action, SshServerAction::Connect | SshServerAction::Install);
            Button::new(
                ElementId::from(SharedString::from(format!(
                    "{STATUS_SELECTOR}-{}",
                    action.selector()
                ))),
                action.label(),
            )
            .variant(if primary {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Outline
            })
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .debug_selector(action.selector())
            .accessibility_description(SharedString::from(format!(
                "{} {}",
                action.label().trim_end_matches('\u{2026}'),
                destination.as_str()
            )))
            .on_activate(move |_, window, cx| {
                let destination = destination.clone();
                let _ = owner.update(cx, |settings, cx| {
                    settings.run_ssh_server_action(destination, action, window, cx);
                });
            })
            .into_any_element()
        });
        div()
            .debug_selector(|| STATUS_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(state)
            .children(buttons)
            .into_any_element()
    }

    /// One fact about the selected SSH server, as its latest observation reported it.
    pub(super) fn render_ssh_server_fact(
        &self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
    ) -> AnyElement {
        let value = self
            .selected_ssh_server_fact(row)
            .map_or_else(|| SharedString::new_static(UNKNOWN), |(value, _)| value);
        self.render_server_value(row, value, appearance)
    }

    /// Remove Connection, which forgets the selected saved connection at once.
    pub(super) fn render_remove_server(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((destination, _)) = self.selected_ssh_server() else {
            return div().into_any_element();
        };
        let owner = cx.weak_entity();
        Button::new(REMOVE_SELECTOR, "Remove Connection")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .debug_selector(REMOVE_SELECTOR)
            .accessibility_description(SharedString::from(format!(
                "Remove the saved connection to {}",
                destination.as_str()
            )))
            .on_activate(move |_, window, cx| {
                let destination = destination.clone();
                let _ = owner.update(cx, |settings, cx| {
                    settings.run_ssh_server_action(
                        destination,
                        SshServerAction::Remove,
                        window,
                        cx,
                    );
                });
            })
            .into_any_element()
    }

    /// The caption under one SSH detail row.
    pub(super) fn ssh_server_caption(
        &self,
        row: SettingsRowId,
    ) -> Option<(SharedString, CaptionTone)> {
        let (destination, status) = self.selected_ssh_server()?;
        match row {
            SettingsRowId::SshServerStatus => {
                let presentation = present(&status);
                Some((presentation.explanation, presentation.tone))
            }
            SettingsRowId::RemoveServer => Some((
                SharedString::from(format!(
                    "SpaceTerm forgets this connection and stops reconnecting to it. The server on \
                     \u{201C}{}\u{201D}, its Terminal Sessions and your SSH access stay, and work \
                     already sent to the server isn\u{2019}t discarded.",
                    destination.as_str()
                )),
                CaptionTone::Guidance,
            )),
            _ => self
                .selected_ssh_server_fact(row)?
                .1
                .map(|caption| (caption, CaptionTone::Guidance)),
        }
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
        let persistence = rows.connections.persistence_error();
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
        div()
            .debug_selector(|| CONTROL_SELECTOR.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(4.0))
            .children(persistence)
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

/// A fact the server has not reported yet.
const UNKNOWN: &str = "Unknown";

/// The facts the latest observation carries, which only a connection or Check Status reports.
fn observed_facts(status: &RemoteConnectionStatus) -> Option<&SshServerFacts> {
    match status {
        RemoteConnectionStatus::Connected { facts, .. }
        | RemoteConnectionStatus::Checked { facts, .. } => Some(facts),
        _ => None,
    }
}

/// One SSH detail row's value and caption, from the server's latest status, or from the facts
/// `last` observed when that status carries none.
fn ssh_server_fact(
    row: SettingsRowId,
    status: &RemoteConnectionStatus,
    last: Option<&SshServerFacts>,
) -> (SharedString, Option<SharedString>) {
    let current = observed_facts(status);
    let facts = current.or(last);
    // The version row leads the facts, so its caption says when a stale set was observed.
    let earlier = (current.is_none() && last.is_some()).then(|| {
        SharedString::new_static(
            "SpaceTerm last observed these details when it connected or checked the status. \
             They may have changed since.",
        )
    });
    let unknown = || SharedString::new_static(UNKNOWN);
    match row {
        SettingsRowId::SshServerVersion => match facts {
            Some(SshServerFacts {
                version: Some(version),
                ..
            }) => (SharedString::from(version.clone()), earlier),
            Some(_) => (SharedString::new_static("Not Installed"), earlier),
            None => (
                unknown(),
                Some(SharedString::new_static(
                    "SpaceTerm learns these details when it connects or checks the status.",
                )),
            ),
        },
        SettingsRowId::SshServerPlatform => facts.map_or_else(
            || (unknown(), None),
            |facts| {
                (
                    SharedString::from(platform_name(&facts.platform.0, &facts.platform.1)),
                    None,
                )
            },
        ),
        SettingsRowId::SshServerCompatibility => match status {
            RemoteConnectionStatus::Connected { protocol_major, .. } => (
                SharedString::new_static("Compatible"),
                Some(SharedString::from(format!(
                    "Both use protocol version {protocol_major}."
                ))),
            ),
            RemoteConnectionStatus::Checked {
                availability: ServerAvailability::Running,
                ..
            } => (SharedString::new_static("Compatible"), None),
            RemoteConnectionStatus::Incompatible {
                kind: IncompatibilityKind::Platform,
            } => (SharedString::new_static("Unsupported Computer"), None),
            RemoteConnectionStatus::Incompatible { .. } => {
                (SharedString::new_static("Incompatible"), None)
            }
            _ => (unknown(), None),
        },
        SettingsRowId::SshServerStartup => match facts {
            Some(facts) if facts.starts_automatically => (
                SharedString::new_static("Yes"),
                Some(SharedString::new_static(
                    "That computer\u{2019}s service manager starts the server for this account \
                     on its own.",
                )),
            ),
            Some(_) => (
                SharedString::new_static("No"),
                Some(SharedString::new_static(
                    "Nothing on that computer starts the server on its own. Connecting starts it \
                     unless it was stopped.",
                )),
            ),
            None => (unknown(), None),
        },
        _ => (unknown(), None),
    }
}
