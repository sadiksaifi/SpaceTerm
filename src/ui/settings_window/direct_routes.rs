//! Direct network servers this device is paired with, in the Servers Settings Section.
//!
//! A paired server's Status row offers Connect, Resume, Cancel and Disconnect. Resume is the only
//! step here that lets the system ask before it releases this device's key. In the background,
//! only a connected route's first attempt after launch may ask; retries stay quiet. Unpair removes the connection here at once and asks the server
//! to revoke this device, and an unpair the server hasn't confirmed stays in a notice until it
//! settles. Everything here reads the registry's snapshot, so rendering reaches no computer.

use gpui::{Context, SharedString, Task, Window};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::SettingsWindow;
use super::machine_list::MachineChoice;
use crate::direct_connections::{
    DirectConnections, DirectRouteId, DirectRouteStatus, PendingUnpair, RefusalReason,
    UnpairProgress,
};
use crate::ui::direct_access::AccessFailure;
use crate::ui::direct_machines::{DirectMachineAction, UnpairAction};

pub(super) const UNPAIR_SELF_CONFIRM_SELECTOR: &str = "settings-direct-server-unpair-confirm";
pub(super) const UNPAIR_SELF_CANCEL_SELECTOR: &str = "settings-direct-server-unpair-cancel";
/// What a direct route says while SpaceTerm can't confirm its latest changes to direct
/// connections are saved. The record covers every connection, so this speaks of recent changes
/// rather than of any one connection.
pub(super) const UNSAVED_CHANGES: &str = "SpaceTerm couldn\u{2019}t confirm it saved recent connection changes. They could be missing after you reopen SpaceTerm.";

/// The direct network rows: the registry they present. Add Server owns joining a new server.
pub(super) struct DirectRouteRows {
    pub(super) connections: DirectConnections,
    /// Why the latest step on a route didn't take effect.
    pub(super) problem: Option<(DirectRouteId, AccessFailure)>,
    pub(super) _changes: crate::ui::direct_machines::DirectChanges,
    pub(super) _work: Option<Task<()>>,
}

impl DirectRouteRows {
    /// Follows the registry so each state change repaints the rows and follows the selected
    /// server. Construction reads nothing.
    pub(super) fn new(connections: DirectConnections, cx: &mut Context<SettingsWindow>) -> Self {
        let follow = crate::ui::direct_machines::DirectChanges::follow(&connections, |changes| {
            cx.spawn(async move |settings, cx| {
                while changes.recv().await.is_ok() {
                    let followed = settings.update(cx, |settings, cx| {
                        settings.reconcile_machines(cx);
                        settings.sync_machine_location(cx);
                        cx.notify();
                    });
                    if followed.is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            connections,
            problem: None,
            _changes: follow,
            _work: None,
        }
    }
}

impl SettingsWindow {
    /// The selected direct route, its status and name, from the registry's snapshot.
    fn selected_direct_route(&self) -> Option<(DirectRouteId, DirectRouteStatus, SharedString)> {
        let Some(MachineChoice::Direct(route)) = self.selected_machine() else {
            return None;
        };
        let rows = self.direct_routes.as_ref()?;
        let status = rows.connections.status(&route)?;
        let label = rows.connections.route(&route)?.label;
        Some((route, status, label.into()))
    }

    /// Why the latest step on the selected paired server didn't take effect, under its Status.
    pub(super) fn direct_problem(&self) -> Option<SharedString> {
        let (route, _, name) = self.selected_direct_route()?;
        let (failed, failure) = self.direct_routes.as_ref()?.problem?;
        (failed == route).then(|| failure.explanation(&name).into())
    }

    pub(super) fn run_direct_action(
        &mut self,
        route: DirectRouteId,
        action: DirectMachineAction,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.direct_routes.as_mut() else {
            return;
        };
        rows.problem = None;
        let connections = rows.connections.clone();
        match action {
            DirectMachineAction::Connect => connections.connect(&route),
            // The person asked, so the system may ask them before it releases this device's key.
            DirectMachineAction::Resume => connections.resume(&route),
            DirectMachineAction::Cancel => connections.cancel(&route),
            DirectMachineAction::Disconnect => connections.disconnect(&route),
        }
        cx.notify();
    }

    /// Asks before Remove Machine removes the selected machine without reaching its service
    /// lane. A machine that already unpaired this device has nothing left to revoke. Any other
    /// keeps SpaceTerm Core and is asked to revoke this device once it answers.
    pub(super) fn confirm_unpair_this_device(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((route, status, name)) = self.selected_direct_route() else {
            return;
        };
        let owner = cx.weak_entity();
        let revoked = status == DirectRouteStatus::Refused(RefusalReason::Revoked);
        let here = self.this_computer.to_lowercase();
        let title = format!("Remove {name}?");
        let message = if revoked {
            format!("{name} already unpaired this device, so this removes it from {here}.")
        } else {
            format!(
                "SpaceTerm can\u{2019}t reach {name}, so this removes it from {here} and leaves SpaceTerm Core installed there."
            )
        };
        let result = Alert::new(
            ModalId::new("settings-direct-server-unpair"),
            title.clone(),
            title,
            message,
            vec![
                ModalAction::new(
                    true,
                    "Remove",
                    ModalActionRole::Affirmative,
                    UNPAIR_SELF_CONFIRM_SELECTOR,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    UNPAIR_SELF_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                if revoked {
                    settings.forget_machine(route, cx);
                } else {
                    settings.unpair_this_device(route, cx);
                }
            });
        });
        if result.is_err() {
            eprintln!("failed to present the Remove Machine confirmation");
        }
    }

    /// Removes the route here and records the revocation, which finishes on its own. The
    /// selection moves on at once, since the route is gone from this computer.
    fn unpair_this_device(&mut self, route: DirectRouteId, cx: &mut Context<Self>) {
        self.remove_route(route, DirectConnections::unpair_self, cx);
    }

    /// Removes a route whose Core already revoked this device, with its key, asking that Core
    /// nothing.
    pub(super) fn forget_machine(&mut self, route: DirectRouteId, cx: &mut Context<Self>) {
        self.remove_route(route, DirectConnections::forget, cx);
    }

    fn remove_route(
        &mut self,
        route: DirectRouteId,
        remove: fn(
            &DirectConnections,
            &DirectRouteId,
        ) -> Result<(), crate::machine_connection::direct::DirectError>,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.direct_routes.as_mut() else {
            return;
        };
        let connections = rows.connections.clone();
        let task = cx.spawn(async move |settings, cx| {
            let removed = cx
                .background_executor()
                .spawn(async move { remove(&connections, &route) })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                if let (Err(error), Some(rows)) = (removed, settings.direct_routes.as_mut()) {
                    rows.problem = Some((route, AccessFailure::from(error)));
                }
                // The selection gives way to this computer's machine once the route is gone.
                settings.reconcile_machines(cx);
                settings.sync_machine_location(cx);
                cx.notify();
            });
        });
        if let Some(rows) = self.direct_routes.as_mut() {
            rows.problem = None;
            rows._work = Some(task);
        }
        self.sync_machine_location(cx);
        cx.notify();
    }

    /// The unpair requests to show, from the registry's snapshot. Dismissing hides a request
    /// only while SpaceTerm can still finish it quietly: one that needs the person's permission
    /// shows again, because nothing else continues it.
    pub(super) fn visible_pending_unpairs(&self) -> Vec<PendingUnpair> {
        self.direct_routes
            .as_ref()
            .map(|rows| rows.connections.pending_unpairs())
            .unwrap_or_default()
            .into_iter()
            .filter(|pending| {
                !pending.dismissed || pending.state == UnpairProgress::NeedsInteraction
            })
            .collect()
    }

    pub(super) fn run_unpair_action(
        &mut self,
        pending: &PendingUnpair,
        action: UnpairAction,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.direct_routes.as_ref() else {
            return;
        };
        match action {
            // The person asked, so the system may ask them before it releases the key the
            // request was made with.
            UnpairAction::Resume => rows.connections.resume_unpair(pending.key),
            UnpairAction::Dismiss => rows.connections.dismiss_unpair_notice(pending.key),
        }
        cx.notify();
    }
}
