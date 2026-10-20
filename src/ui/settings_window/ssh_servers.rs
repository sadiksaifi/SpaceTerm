//! SSH server connections in the Servers Settings Section.
//!
//! Each SSH server SpaceTerm knows appears with its state and the steps that state allows:
//! Connect, Cancel, Disconnect, and Install after the person reviews what installing does. The
//! connection list reads the registry's snapshot, so rendering asks no computer anything. The host
//! list reads this computer's SSH configuration when the section opens.

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
use crate::domain::SshDestination;
use crate::ssh::server_connection::{RemoteConnectionStatus, SshServerConnections};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{CaptionTone, badge};
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

/// The SSH servers row: the registry it presents, and the hosts SSH configuration names.
pub(super) struct SshServerRows {
    connections: SshServerConnections,
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
        let follow = cx.spawn(async move |settings, cx| {
            while changes.recv().await.is_ok() {
                if settings.update(cx, |_, cx| cx.notify()).is_err() {
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

    /// The destinations Connect to Server offers: every configured host not already listed.
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
        if matches!(action, SshServerAction::Connect | SshServerAction::Install) {
            crate::ui::ssh_servers::present_prompts_in(window, cx);
        }
        match action {
            SshServerAction::Connect => connections.connect(destination),
            SshServerAction::Cancel => connections.cancel(&destination),
            SshServerAction::Disconnect => connections.disconnect(&destination),
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

    /// Every known SSH server with its state and steps, and Connect to Server for the rest.
    pub(super) fn render_ssh_servers(
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
        let separator = gpui_color(appearance.separator(spaceterm_ui::ControlHost::Card));
        let owner = cx.weak_entity();
        let servers = listed
            .iter()
            .enumerate()
            .flat_map(|(index, (destination, status))| {
                let divider = (index > 0)
                    .then(|| div().h(gpui::px(HAIRLINE)).bg(separator).into_any_element());
                let row = render_server(index, destination, status, appearance, owner.clone());
                divider.into_iter().chain([row])
            });
        let servers: Vec<AnyElement> = servers.collect();
        let empty = servers.is_empty();
        let invalid = rows.invalid;
        let connect = {
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
                .variant(ButtonVariant::Primary)
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
            div()
                .flex()
                .flex_col()
                .gap(appearance.spacing(4.0))
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
            .gap(appearance.spacing(10.0))
            .children(persistence)
            .children(servers)
            .when(!empty, |list| {
                list.child(div().h(gpui::px(HAIRLINE)).bg(separator))
            })
            .child(connect)
            .into_any_element()
    }
}

/// One SSH server: its destination, state, explanation, and steps.
fn render_server(
    index: usize,
    destination: &SshDestination,
    status: &RemoteConnectionStatus,
    appearance: &ChromeAppearance,
    owner: gpui::WeakEntity<SettingsWindow>,
) -> AnyElement {
    let presentation = present(status);
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    let name = SharedString::from(destination.as_str().to_owned());
    let row_selector = format!("settings-ssh-server-{index}");
    let state_selector = presentation.state_selector;
    let state = if presentation.working {
        ProgressRing::new(
            SharedString::from(format!("{row_selector}-progress")),
            presentation.state,
            ProgressState::Indeterminate,
        )
        .size(ProgressSize::Compact)
        .debug_selector(format!("{row_selector}-progress"))
        .into_any_element()
    } else {
        div()
            .debug_selector(move || state_selector.to_owned())
            .child(badge(state_selector, presentation.state, appearance))
            .into_any_element()
    };
    // Assistive technology announces the state when it changes, as a status.
    let state = div()
        .id(SharedString::from(format!("{row_selector}-state")))
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
    let actions = presentation.actions.iter().map(|&action| {
        let owner = owner.clone();
        let destination = destination.clone();
        let primary = presentation.actions.first() == Some(&action)
            && matches!(action, SshServerAction::Connect | SshServerAction::Install);
        Button::new(
            ElementId::from(SharedString::from(format!(
                "{row_selector}-{}",
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
        .debug_selector(format!("{row_selector}-{}", action.selector()))
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
    let caption_color = match presentation.tone {
        CaptionTone::Guidance => colors.text_muted,
        CaptionTone::Warning => colors.warning,
        CaptionTone::Error => colors.error,
    };
    div()
        .id(SharedString::from(row_selector.clone()))
        .debug_selector({
            let row_selector = row_selector.clone();
            move || row_selector.clone()
        })
        .role(accesskit::Role::Group)
        .aria_label(name.clone())
        .flex()
        .flex_col()
        .w_full()
        .gap(appearance.spacing(3.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .chrome_text(appearance.typography.style(TextRole::Body))
                        .text_color(gpui_color(colors.text))
                        .child(name),
                )
                .child(state)
                .children(actions),
        )
        .child(
            div()
                .debug_selector(move || format!("{row_selector}-description"))
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(caption_color))
                .whitespace_normal()
                .child(presentation.explanation),
        )
        .into_any_element()
}
