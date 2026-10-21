//! Remote server maintenance in the selected SSH server's detail.
//!
//! Two rows carry it. Background service reports the server's own state and offers Start, Stop,
//! Restart and Uninstall where that state allows them. Server update offers the Update Server
//! journey, and finishes or returns an update the person already confirmed. Both read the
//! operations owner's snapshot, so rendering asks no computer anything, and the section reads
//! nothing from another machine until the person chooses a step.
//!
//! A destructive step describes its change first and performs only the exact change the person
//! then confirms. Cancelling the confirmation abandons that description, and nothing was sent.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, AnyWindowHandle, Context, ElementId, SharedString, Task, accesskit, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ProgressRing, ProgressSize,
    ProgressState,
};

use super::SettingsWindow;
use super::catalog::SettingsRowId;
use super::server_list::ServerChoice;
use crate::domain::SshDestination;
use crate::local_server::service::maintenance::PreparedChange;
use crate::ssh::server_maintenance::SubmittedKind;
use crate::ssh::server_operations::{
    RemoteMaintenanceRequest, RemoteOperationHandle, RemoteOperationState, RemoteServerOperations,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::remote_maintenance::{
    MaintenanceConfirmation, MaintenancePresentation, MaintenanceRow, RemoteAction,
    discarded_operation, observed, present, removed_presentation,
};
use crate::ui::sidebar_window::form::{CaptionTone, badge};

pub(super) const SERVICE_CONTROL_SELECTOR: &str = "settings-ssh-server-service-control";
pub(super) const SERVICE_PROGRESS_SELECTOR: &str = "settings-ssh-server-service-progress";
pub(super) const UPDATE_CONTROL_SELECTOR: &str = "settings-ssh-server-update-control";
pub(super) const UPDATE_PROGRESS_SELECTOR: &str = "settings-ssh-server-update-progress";
pub(super) const REMOVED_SELECTOR: &str = "settings-removed-server-operations";

/// The confirmation selectors of one described change, so a test answers the exact alert it
/// reviewed.
const fn confirmation_selectors(
    request: RemoteMaintenanceRequest,
) -> (&'static str, &'static str, &'static str) {
    use crate::local_server::service::control::Action;
    match request {
        RemoteMaintenanceRequest::Service(Action::Stop) => (
            "settings-ssh-server-stop",
            "settings-ssh-server-stop-confirm",
            "settings-ssh-server-stop-cancel",
        ),
        RemoteMaintenanceRequest::Service(Action::Restart) => (
            "settings-ssh-server-restart",
            "settings-ssh-server-restart-confirm",
            "settings-ssh-server-restart-cancel",
        ),
        RemoteMaintenanceRequest::Service(Action::Uninstall) => (
            "settings-ssh-server-uninstall",
            "settings-ssh-server-uninstall-confirm",
            "settings-ssh-server-uninstall-cancel",
        ),
        RemoteMaintenanceRequest::Update => (
            "settings-ssh-server-update-change",
            "settings-ssh-server-update-confirm",
            "settings-ssh-server-update-cancel",
        ),
        RemoteMaintenanceRequest::Return => (
            "settings-ssh-server-return",
            "settings-ssh-server-return-confirm",
            "settings-ssh-server-return-cancel",
        ),
    }
}

/// The confirmation selectors of a Discard. It describes no change on the other computer, so it
/// belongs to no request.
const DISCARD_SELECTORS: (&str, &str, &str) = (
    "settings-ssh-server-discard",
    "settings-ssh-server-discard-confirm",
    "settings-ssh-server-discard-cancel",
);

/// The maintenance rows: the owner they read, and the change this window asked it to describe.
pub(super) struct SshMaintenanceRows {
    pub(super) operations: Arc<dyn RemoteServerOperations>,
    /// The change this window asked the owner to describe. It presents that change's
    /// confirmation when the description is ready, and nothing else does.
    asked: Option<(SshDestination, RemoteMaintenanceRequest)>,
    /// A confirmation is open. The rows start nothing until the person answers it.
    asking: bool,
    /// The window the confirmation is presented in, since a description finishes in the
    /// background.
    window: AnyWindowHandle,
    _changes: Task<()>,
}

impl SshMaintenanceRows {
    /// Follows the owner so every state change repaints the rows and presents a change this
    /// window asked for. Construction reads nothing and reaches no computer.
    pub(super) fn new(
        operations: Arc<dyn RemoteServerOperations>,
        window: AnyWindowHandle,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let changes = operations.subscribe();
        let follow = cx.spawn(async move |settings, cx| {
            while changes.recv().await.is_ok() {
                let followed = settings.update(cx, |settings, cx| {
                    settings.continue_asked_maintenance(cx);
                    cx.notify();
                });
                if followed.is_err() {
                    break;
                }
            }
        });
        Self {
            operations,
            asked: None,
            asking: false,
            window,
            _changes: follow,
        }
    }
}

impl SettingsWindow {
    /// The selected SSH server and its maintenance state, from the owner's snapshot.
    fn selected_maintenance(&self) -> Option<(SshDestination, RemoteOperationState)> {
        let Some(ServerChoice::Ssh(destination)) = self.selected_server() else {
            return None;
        };
        let rows = self.ssh_maintenance.as_ref()?;
        let state = rows.operations.state(&destination);
        Some((destination, state))
    }

    fn maintenance_presentation(&self, row: MaintenanceRow) -> Option<MaintenancePresentation> {
        let (destination, state) = self.selected_maintenance()?;
        let rows = self.ssh_maintenance.as_ref()?;
        // Before any maintenance read, what the connection already observed decides what the row
        // offers, so a connected server can be stopped without a reading step first.
        if matches!(state, RemoteOperationState::Unread)
            && let Some(connections) = self.ssh_servers.as_ref()
            && let Some(presentation) = observed(
                destination.as_str(),
                &connections.connections.status(&destination),
                row,
            )
        {
            return Some(presentation);
        }
        let asked = rows
            .asked
            .as_ref()
            .filter(|(asked, _)| *asked == destination)
            .map(|(_, request)| *request);
        Some(present(destination.as_str(), &state, row, asked))
    }

    /// The caption under one maintenance row, or `None` for another row.
    pub(super) fn ssh_maintenance_caption(
        &self,
        row: SettingsRowId,
    ) -> Option<(SharedString, CaptionTone)> {
        let presentation = self.maintenance_presentation(maintenance_row(row)?)?;
        Some((presentation.explanation, presentation.tone))
    }

    /// Takes one step another surface offered, such as the sidebar machine heading menu. The
    /// step still runs through the row that owns it, so the row's own rules decide what happens.
    pub(crate) fn request_remote_maintenance(
        &mut self,
        destination: SshDestination,
        action: RemoteAction,
        cx: &mut Context<Self>,
    ) {
        self.request_maintenance(destination, action.row(), action, cx);
    }

    /// Takes one step from a maintenance row. A step the row no longer offers does nothing, so a
    /// click that arrives after the state changed never acts on the state it was offered for.
    fn request_maintenance(
        &mut self,
        destination: SshDestination,
        row: MaintenanceRow,
        action: RemoteAction,
        cx: &mut Context<Self>,
    ) {
        let Some(presentation) = self.maintenance_presentation(row) else {
            return;
        };
        if !presentation.actions.contains(&action) {
            return;
        }
        let Some(rows) = self.ssh_maintenance.as_ref() else {
            return;
        };
        if rows.asking {
            return;
        }
        let operations = Arc::clone(&rows.operations);
        let state = operations.state(&destination);
        match action {
            RemoteAction::Check => operations.refresh(&destination),
            RemoteAction::Start => operations.start(&destination),
            RemoteAction::Cancel => operations.cancel(&destination),
            RemoteAction::Finish | RemoteAction::CheckAgain => operations.resume(&destination),
            RemoteAction::Acknowledge => {
                if let Some(handle) = acknowledged(&state) {
                    operations.acknowledge(handle);
                }
            }
            RemoteAction::Discard => {
                if let Some(kind) = discarded_operation(&state) {
                    self.confirm_discard(destination, kind, cx);
                }
            }
            step => {
                let Some(request) = step.request() else {
                    return;
                };
                // A change already described is the change the person reviews. Describing again
                // would ask the machine a second time for the same answer.
                if let RemoteOperationState::Described {
                    handle,
                    request: described,
                    change,
                    ..
                } = &state
                {
                    if *described == request {
                        self.confirm_maintenance(destination, *handle, request, change, cx);
                    }
                    return;
                }
                if let Some(rows) = self.ssh_maintenance.as_mut() {
                    rows.asked = Some((destination.clone(), request));
                }
                operations.describe(&destination, request);
            }
        }
        cx.notify();
    }

    /// Presents the confirmation for a change this window asked the owner to describe, once the
    /// description is ready. A description that ended otherwise is left to the row to report.
    fn continue_asked_maintenance(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.ssh_maintenance.as_ref() else {
            return;
        };
        if rows.asking {
            return;
        }
        let Some((destination, request)) = rows.asked.clone() else {
            return;
        };
        match rows.operations.state(&destination) {
            RemoteOperationState::Described {
                handle,
                request: described,
                change,
                ..
            } if described == request => {
                if let Some(rows) = self.ssh_maintenance.as_mut() {
                    rows.asked = None;
                }
                self.confirm_maintenance(destination, handle, request, &change, cx);
            }
            RemoteOperationState::Working { .. } => {}
            _ => {
                if let Some(rows) = self.ssh_maintenance.as_mut() {
                    rows.asked = None;
                }
            }
        }
    }

    /// Asks before the described change. Cancel is the default, so Return and Escape both leave
    /// the other computer as it is, and cancelling abandons the description.
    fn confirm_maintenance(
        &mut self,
        destination: SshDestination,
        handle: RemoteOperationHandle,
        request: RemoteMaintenanceRequest,
        change: &PreparedChange,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.ssh_maintenance.as_ref() else {
            return;
        };
        let Some(confirmation) = MaintenanceConfirmation::of(destination.as_str(), request, change)
        else {
            return;
        };
        let operations = Arc::clone(&rows.operations);
        self.ask(
            confirmation,
            confirmation_selectors(request),
            move |confirmed, _| {
                if confirmed {
                    operations.confirm(handle);
                } else {
                    // The described change is abandoned, and nothing was sent.
                    operations.cancel(&destination);
                }
            },
            cx,
        );
    }

    /// Asks before Discard retires the only record that could still say what a submitted
    /// operation did. Nothing is sent to the machine either way, and cancelling keeps the record.
    fn confirm_discard(
        &mut self,
        destination: SshDestination,
        kind: &SubmittedKind,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.ssh_maintenance.as_ref() else {
            return;
        };
        let operations = Arc::clone(&rows.operations);
        self.ask(
            MaintenanceConfirmation::for_discard(destination.as_str(), kind),
            DISCARD_SELECTORS,
            move |confirmed, _| {
                if confirmed {
                    operations.discard(&destination);
                }
            },
            cx,
        );
    }

    /// Presents one confirmation and reports what the person answered. Cancel is the default, so
    /// Return and Escape both answer no, and the rows start nothing while it is open.
    fn ask(
        &mut self,
        confirmation: MaintenanceConfirmation,
        selectors: (&'static str, &'static str, &'static str),
        answered: impl FnOnce(bool, &mut gpui::App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.ssh_maintenance.as_ref() else {
            return;
        };
        let (id, confirm_selector, cancel_selector) = selectors;
        let handle_window = rows.window;
        if let Some(rows) = self.ssh_maintenance.as_mut() {
            rows.asking = true;
        }
        let owner = cx.weak_entity();
        let entity = cx.entity();
        // A confirmation may be asked for outside any window update, such as when a description
        // finishes in the background, so the alert is presented through the Settings window it
        // belongs to.
        cx.defer(move |cx| {
            let presented = handle_window.update(cx, |_, window, cx| {
                entity.update(cx, |_, cx| {
                    let alert = Alert::new(
                        ModalId::new(id),
                        confirmation.title,
                        confirmation.title,
                        confirmation.message.clone(),
                        vec![
                            ModalAction::new(
                                true,
                                confirmation.confirm,
                                ModalActionRole::Affirmative,
                                confirm_selector,
                            )
                            .with_intent(ModalActionIntent::Destructive)
                            .with_emphasis(ModalActionEmphasis::Prominent),
                            ModalAction::new(
                                false,
                                "Cancel",
                                ModalActionRole::Cancel,
                                cancel_selector,
                            ),
                        ],
                    )
                    .intent(AlertIntent::Critical)
                    .detail(confirmation.detail.clone());
                    let result = alert.present(window, cx, move |outcome, cx| {
                        let confirmed = matches!(
                            outcome,
                            AlertOutcome::Activated {
                                action_id: true,
                                ..
                            }
                        );
                        let _ = owner.update(cx, |settings, cx| {
                            if let Some(rows) = settings.ssh_maintenance.as_mut() {
                                rows.asking = false;
                            }
                            cx.notify();
                        });
                        answered(confirmed, cx);
                    });
                    if result.is_err() {
                        eprintln!("failed to present the remote server maintenance confirmation");
                    }
                    result.is_ok()
                })
            });
            // An alert that could not be presented asks nothing, so the rows are free again.
            if !presented.unwrap_or(false) {
                entity.update(cx, |settings, cx| {
                    if let Some(rows) = settings.ssh_maintenance.as_mut() {
                        rows.asking = false;
                    }
                    cx.notify();
                });
            }
        });
    }

    /// The selected server's service state, or progress while an operation runs, and its steps.
    pub(super) fn render_ssh_server_service(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_maintenance_row(
            MaintenanceRow::Service,
            SERVICE_CONTROL_SELECTOR,
            SERVICE_PROGRESS_SELECTOR,
            appearance,
            cx,
        )
    }

    /// The Update Server journey for the selected server.
    pub(super) fn render_ssh_server_update(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_maintenance_row(
            MaintenanceRow::Update,
            UPDATE_CONTROL_SELECTOR,
            UPDATE_PROGRESS_SELECTOR,
            appearance,
            cx,
        )
    }

    fn render_maintenance_row(
        &mut self,
        row: MaintenanceRow,
        control_selector: &'static str,
        progress_selector: &'static str,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((destination, _)) = self.selected_maintenance() else {
            return div().into_any_element();
        };
        let Some(presentation) = self.maintenance_presentation(row) else {
            return div().into_any_element();
        };
        let machine = SharedString::from(destination.as_str().to_owned());
        let state = presentation.state.map(|label| {
            let selector = presentation.state_selector.clone();
            let id = SharedString::from(format!("{selector}-badge"));
            let shown = if presentation.working {
                ProgressRing::new(progress_selector, label, ProgressState::Indeterminate)
                    .size(ProgressSize::Compact)
                    .debug_selector(progress_selector)
                    .into_any_element()
            } else {
                div()
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.to_string()
                    })
                    .child(badge(id, label, appearance))
                    .into_any_element()
            };
            // Assistive technology announces the state when it changes, as a status.
            div()
                .id(SharedString::from(format!("{control_selector}-state")))
                .role(accesskit::Role::Status)
                .aria_label(SharedString::from(format!("{machine}: {label}")))
                .child(shown)
        });
        let owner = cx.weak_entity();
        let actions = presentation.actions.into_iter().map(|action| {
            let owner = owner.clone();
            let destination = destination.clone();
            let primary = matches!(action, RemoteAction::Start | RemoteAction::Acknowledge);
            Button::new(
                ElementId::from(SharedString::from(format!(
                    "{control_selector}-{}",
                    action.slot()
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
            .accessibility_description(action.consequence(machine.as_ref()))
            .on_activate(move |_, _, cx| {
                let destination = destination.clone();
                let _ = owner.update(cx, |settings, cx| {
                    settings.request_maintenance(destination, row, action, cx);
                });
            })
            .into_any_element()
        });
        div()
            .debug_selector(move || control_selector.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .children(state)
            .children(actions)
            .into_any_element()
    }
}

/// Why a removed connection still appears, and what the person can do about it.
pub(super) const REMOVED_GUIDANCE: &str = "You removed these saved connections. SpaceTerm still \
     has the result of work you asked for on them. Nothing here reconnects, and answering \
     doesn\u{2019}t bring a connection back.";

impl SettingsWindow {
    /// The servers whose saved connection was removed and that still have something to say,
    /// ordered by destination as the owner reports them.
    fn removed_operations(&self) -> Vec<(SshDestination, MaintenancePresentation)> {
        let Some(rows) = self.ssh_maintenance.as_ref() else {
            return Vec::new();
        };
        rows.operations
            .entries()
            .into_iter()
            .filter(|entry| entry.removed)
            .filter_map(|entry| {
                let presentation = removed_presentation(entry.destination.as_str(), &entry.state)?;
                Some((entry.destination, presentation))
            })
            .collect()
    }

    pub(super) fn has_removed_operations(&self) -> bool {
        !self.removed_operations().is_empty()
    }

    /// Settles one removed connection's operation. A removed connection is no longer a selected
    /// server, so only resolving its original and acknowledging a validated result reach it, and
    /// neither recreates the connection.
    fn request_removed_maintenance(
        &mut self,
        destination: SshDestination,
        action: RemoteAction,
        cx: &mut Context<Self>,
    ) {
        let offered = self
            .removed_operations()
            .iter()
            .any(|(listed, presentation)| {
                *listed == destination && presentation.actions.contains(&action)
            });
        if !offered {
            return;
        }
        let Some(rows) = self.ssh_maintenance.as_ref() else {
            return;
        };
        if rows.asking {
            return;
        }
        let operations = Arc::clone(&rows.operations);
        let state = operations.state(&destination);
        match action {
            RemoteAction::Finish | RemoteAction::CheckAgain => operations.resume(&destination),
            RemoteAction::Acknowledge => {
                if let Some(handle) = acknowledged(&state) {
                    operations.acknowledge(handle);
                }
            }
            RemoteAction::Discard => {
                if let Some(kind) = discarded_operation(&state) {
                    self.confirm_discard(destination, kind, cx);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    /// Each removed connection's server, what SpaceTerm still has to tell the person about it,
    /// and the one step that settles it. Nothing here reconnects or reads that machine.
    pub(super) fn render_removed_operations(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let owner = cx.weak_entity();
        let entries = self
            .removed_operations()
            .into_iter()
            .map(|(destination, presentation)| {
                let machine = SharedString::from(destination.as_str().to_owned());
                let steps: Vec<_> = presentation
                    .actions
                    .into_iter()
                    .map(|action| {
                        let owner = owner.clone();
                        let destination = destination.clone();
                        let machine = machine.clone();
                        Button::new(
                            ElementId::from(SharedString::from(format!(
                                "{REMOVED_SELECTOR}-{machine}-{}",
                                action.slot()
                            ))),
                            action.label(),
                        )
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Regular)
                        .tab_stop(true)
                        .debug_selector(format!("{}-{machine}", action.selector()))
                        .accessibility_description(action.consequence(machine.as_ref()))
                        .on_activate(move |_, _, cx| {
                            let destination = destination.clone();
                            let _ = owner.update(cx, |settings, cx| {
                                settings.request_removed_maintenance(destination, action, cx);
                            });
                        })
                        .into_any_element()
                    })
                    .collect();
                div()
                    .id(SharedString::from(format!("{REMOVED_SELECTOR}-{machine}")))
                    .debug_selector({
                        let machine = machine.clone();
                        move || format!("{REMOVED_SELECTOR}-{machine}")
                    })
                    .role(accesskit::Role::Group)
                    .aria_label(machine.clone())
                    .aria_description(presentation.explanation.clone())
                    .flex()
                    .flex_row()
                    .items_start()
                    .w_full()
                    .gap(appearance.spacing(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(appearance.spacing(2.0))
                            .child(
                                div()
                                    .chrome_text(appearance.typography.style(TextRole::Body))
                                    .text_color(gpui_color(colors.text))
                                    .child(machine.clone()),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "{REMOVED_SELECTOR}-{machine}-explanation"
                                    )))
                                    .role(accesskit::Role::Label)
                                    .aria_value(presentation.explanation.clone())
                                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                                    .text_color(gpui_color(colors.text_secondary))
                                    .whitespace_normal()
                                    .child(presentation.explanation),
                            ),
                    )
                    .children(steps)
            });
        div()
            .debug_selector(|| REMOVED_SELECTOR.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(8.0))
            .children(entries)
            .into_any_element()
    }
}

/// The row one Settings row identity presents, or `None` for a row outside maintenance.
pub(super) const fn maintenance_row(row: SettingsRowId) -> Option<MaintenanceRow> {
    match row {
        SettingsRowId::SshServerService => Some(MaintenanceRow::Service),
        SettingsRowId::SshServerUpdate => Some(MaintenanceRow::Update),
        _ => None,
    }
}

/// The validated result an acknowledgement retires, if the state holds one.
const fn acknowledged(state: &RemoteOperationState) -> Option<RemoteOperationHandle> {
    match state {
        RemoteOperationState::Resolved { handle, .. } => Some(*handle),
        _ => None,
    }
}

#[cfg(test)]
#[path = "ssh_maintenance_tests.rs"]
mod tests;
