//! Direct network servers this device is paired with, in the Servers Settings Section.
//!
//! The selected direct network server shows its state and the steps that state allows: Connect,
//! Resume, Cancel and Disconnect. Resume is the only step that lets the system ask before it
//! releases this device's key; everything SpaceTerm does in the background stays quiet. Unpair
//! This Device removes the connection here at once and asks the server to revoke this device,
//! and an unpair the server hasn't confirmed stays listed with what it still needs until it
//! settles. Every row reads the registry's snapshot, so rendering reaches no computer.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, ElementId, Entity, SharedString, Subscription, Task, Window, accesskit,
    div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ButtonSize, ButtonVariant, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ProgressRing, ProgressSize,
    ProgressState, TextInput, TextInputContentMode, TextInputEvent, TextInputVariant,
};

use super::SettingsWindow;
use super::catalog::SettingsRowId;
use super::server_list::ServerChoice;
use crate::direct_connections::{
    DirectConnections, DirectRouteId, DirectRouteStatus, JoinId, PendingUnpair, RefusalReason,
    UnpairState,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::AccessFailure;
use crate::ui::direct_servers::{DirectServerAction, UnpairAction, present, present_unpair};
use crate::ui::sidebar_window::form::{CaptionTone, badge};

pub(super) const STATUS_SELECTOR: &str = "settings-direct-server-status";
pub(super) const UNPAIR_SELF_SELECTOR: &str = "settings-direct-server-unpair";
pub(super) const UNPAIR_SELF_CONFIRM_SELECTOR: &str = "settings-direct-server-unpair-confirm";
pub(super) const UNPAIR_SELF_CANCEL_SELECTOR: &str = "settings-direct-server-unpair-cancel";
pub(super) const PENDING_SELECTOR: &str = "settings-pending-unpairs";
/// What a direct route says while SpaceTerm can't confirm its latest changes to direct
/// connections are saved. The record covers every connection, so this speaks of recent changes
/// rather than of any one connection.
pub(super) const UNSAVED_CHANGES: &str = "SpaceTerm couldn\u{2019}t confirm its latest changes to \
     direct connections are saved on this computer, so recent changes, such as a new pairing, may \
     be missing after SpaceTerm reopens.";
/// Why unpair requests appear after their connections are gone.
pub(super) const PENDING_GUIDANCE: &str = "SpaceTerm removed these connections on this computer. \
     It keeps finishing what each one still needs quietly, and asks for your permission only when \
     you choose Resume.";

/// The direct network rows: the registry they present and the Join with Invitation journey.
pub(super) struct DirectRouteRows {
    pub(super) connections: DirectConnections,
    /// The invitation the person pastes. Its value is obscured and taken out of the field when
    /// the person joins, so the field never keeps it.
    pub(super) invitation: Entity<TextInput>,
    /// The join underway or last finished, which the Join row reports.
    pub(super) join: Option<JoinId>,
    /// Why the latest Join didn't start.
    pub(super) join_failure: Option<crate::local_server::direct::DirectError>,
    /// Whether the person cancelled the latest join, which the registry reports as refused.
    pub(super) join_cancelled: bool,
    /// Why SpaceTerm couldn't save the person's cancellation or withdrawal of the latest join.
    pub(super) join_withdraw_failure: Option<crate::local_server::direct::DirectError>,
    /// Whether the Servers list already selected the latest join's new route.
    pub(super) join_selected: bool,
    /// Repaints a waiting join's countdown.
    pub(super) join_tick: Option<Task<()>>,
    /// Why the latest step on a route didn't take effect.
    pub(super) problem: Option<(DirectRouteId, AccessFailure)>,
    pub(super) _changes: crate::ui::direct_servers::DirectChanges,
    pub(super) _invitation_events: Subscription,
    pub(super) _work: Option<Task<()>>,
}

impl DirectRouteRows {
    /// Follows the registry so each state change repaints the rows and follows the selected
    /// server. Construction reads nothing.
    pub(super) fn new(
        connections: DirectConnections,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let invitation = cx.new(|cx| {
            TextInput::new(
                super::direct_join::INVITATION_SELECTOR,
                "Invitation",
                String::new(),
                window,
                cx,
            )
            .variant(TextInputVariant::Bare)
            .content_mode(TextInputContentMode::Obscured)
            .placeholder("Paste an invitation")
            .debug_selector(super::direct_join::INVITATION_SELECTOR)
        });
        let invitation_events = cx.subscribe_in(
            &invitation,
            window,
            |settings, _, event: &TextInputEvent, _, cx| match event {
                TextInputEvent::Submitted => settings.join_with_invitation(cx),
                TextInputEvent::ValueChanged(_) => {
                    if let Some(rows) = settings.direct_routes.as_mut()
                        && rows.join_failure.take().is_some()
                    {
                        cx.notify();
                    }
                }
                _ => {}
            },
        );
        let follow = crate::ui::direct_servers::DirectChanges::follow(&connections, |changes| {
            cx.spawn(async move |settings, cx| {
                while changes.recv().await.is_ok() {
                    let followed = settings.update(cx, |settings, cx| {
                        settings.follow_join(cx);
                        settings.sync_server_location(cx);
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
            invitation,
            join: None,
            join_failure: None,
            join_cancelled: false,
            join_withdraw_failure: None,
            join_selected: false,
            join_tick: None,
            problem: None,
            _changes: follow,
            _invitation_events: invitation_events,
            _work: None,
        }
    }
}

impl SettingsWindow {
    /// The selected direct route, its status and name, from the registry's snapshot.
    fn selected_direct_route(&self) -> Option<(DirectRouteId, DirectRouteStatus, SharedString)> {
        let Some(ServerChoice::Direct(route)) = self.selected_server() else {
            return None;
        };
        let rows = self.direct_routes.as_ref()?;
        let status = rows.connections.status(&route)?;
        let label = rows.connections.route(&route)?.label;
        Some((route, status, label.into()))
    }

    fn run_direct_action(
        &mut self,
        route: DirectRouteId,
        action: DirectServerAction,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.direct_routes.as_mut() else {
            return;
        };
        rows.problem = None;
        let connections = rows.connections.clone();
        match action {
            DirectServerAction::Connect => connections.connect(&route),
            // The person asked, so the system may ask them before it releases this device's key.
            DirectServerAction::Resume => connections.resume(&route),
            DirectServerAction::Cancel => connections.cancel(&route),
            DirectServerAction::Disconnect => connections.disconnect(&route),
        }
        cx.notify();
    }

    /// The selected direct network server's state, announced as it changes, and its steps.
    pub(super) fn render_direct_server_status(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((route, status, name)) = self.selected_direct_route() else {
            return div().into_any_element();
        };
        let presentation = present(status, &name);
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
        let state = div()
            .id(SharedString::from(format!("{STATUS_SELECTOR}-state")))
            .role(accesskit::Role::Status)
            .aria_label(SharedString::from(format!(
                "{name}, direct network: {}",
                presentation.state
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .child(state)
            .when(presentation.working, |state| {
                state.child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.text_muted))
                        .child(presentation.state),
                )
            });
        let leading = presentation.actions.first().copied();
        let owner = cx.weak_entity();
        let buttons = presentation.actions.iter().map(|action| {
            let action = *action;
            let owner = owner.clone();
            let primary = leading == Some(action)
                && matches!(
                    action,
                    DirectServerAction::Connect | DirectServerAction::Resume
                );
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
            .accessibility_description(SharedString::from(format!("{} {name}", action.label())))
            .on_activate(move |_, _, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.run_direct_action(route, action, cx);
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

    /// Unpair This Device, which ends this device's pairing with the selected server.
    pub(super) fn render_unpair_this_device(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((_, _, name)) = self.selected_direct_route() else {
            return div().into_any_element();
        };
        let owner = cx.weak_entity();
        Button::new(UNPAIR_SELF_SELECTOR, "Unpair This Device\u{2026}")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .debug_selector(UNPAIR_SELF_SELECTOR)
            .accessibility_description(SharedString::from(format!(
                "Unpair this device from {name}"
            )))
            .on_activate(move |_, window, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.confirm_unpair_this_device(window, cx);
                });
            })
            .into_any_element()
    }

    /// Asks before unpairing this device from the selected server, naming both.
    pub(super) fn confirm_unpair_this_device(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((route, status, name)) = self.selected_direct_route() else {
            return;
        };
        let owner = cx.weak_entity();
        // A server that already unpaired this device has nothing left to revoke, so the step
        // only removes what remains here.
        let revoked = status == DirectRouteStatus::Refused(RefusalReason::Revoked);
        let title = if revoked {
            format!("Remove the connection to \u{201C}{name}\u{201D}?")
        } else {
            format!("Unpair this device from \u{201C}{name}\u{201D}?")
        };
        let message = if revoked {
            format!(
                "\u{201C}{name}\u{201D} already unpaired this device. SpaceTerm removes the \
                 connection and this device\u{2019}s key for it from this computer."
            )
        } else {
            format!(
                "SpaceTerm removes the connection to \u{201C}{name}\u{201D} from this computer \
                 now and asks the server to revoke this device. Its Workspaces leave this window, \
                 and their Terminal Sessions keep running on that computer."
            )
        };
        let confirm = if revoked { "Remove" } else { "Unpair" };
        let result = Alert::new(
            ModalId::new("settings-direct-server-unpair"),
            title.clone(),
            title,
            message,
            vec![
                ModalAction::new(
                    true,
                    confirm,
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
        .detail(
            "To connect again, this device needs a new invitation from that server. SSH access \
             and other servers aren\u{2019}t affected.",
        )
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
            let _ = owner.update(cx, |settings, cx| settings.unpair_this_device(route, cx));
        });
        if result.is_err() {
            eprintln!("failed to present the Unpair This Device confirmation");
        }
    }

    /// Removes the route here and records the revocation, which finishes on its own. The
    /// selection moves on at once, since the route is gone from this computer.
    fn unpair_this_device(&mut self, route: DirectRouteId, cx: &mut Context<Self>) {
        let Some(rows) = self.direct_routes.as_mut() else {
            return;
        };
        let connections = rows.connections.clone();
        let removed = ServerChoice::Direct(route);
        let next = self.server_after_removing(&removed);
        if self.selected_server() == Some(removed) {
            self.selected_server = next;
        }
        let task = cx.spawn(async move |settings, cx| {
            let unpaired = cx
                .background_executor()
                .spawn(async move { connections.unpair_self(&route) })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                if let (Err(error), Some(rows)) = (unpaired, settings.direct_routes.as_mut()) {
                    rows.problem = Some((route, AccessFailure::from(error)));
                }
                settings.sync_server_location(cx);
                cx.notify();
            });
        });
        if let Some(rows) = self.direct_routes.as_mut() {
            rows.problem = None;
            rows._work = Some(task);
        }
        self.sync_server_location(cx);
        cx.notify();
    }

    /// The caption under one direct route row.
    pub(super) fn direct_route_caption(
        &self,
        row: SettingsRowId,
    ) -> Option<(SharedString, CaptionTone)> {
        let (route, status, name) = self.selected_direct_route()?;
        match row {
            SettingsRowId::DirectServerStatus => {
                let problem = self
                    .direct_routes
                    .as_ref()
                    .and_then(|rows| rows.problem)
                    .filter(|(failed, _)| *failed == route);
                if let Some((_, failure)) = problem {
                    return Some((failure.explanation(&name).into(), CaptionTone::Error));
                }
                let presentation = present(status, &name);
                // While SpaceTerm can't confirm its latest changes are saved, every route says so
                // here, wherever the person reached it from, until a save succeeds.
                let unsaved = self
                    .direct_routes
                    .as_ref()
                    .is_some_and(|rows| rows.connections.persistence_error());
                if unsaved {
                    let tone = match presentation.tone {
                        CaptionTone::Error => CaptionTone::Error,
                        _ => CaptionTone::Warning,
                    };
                    return Some((
                        format!("{} {UNSAVED_CHANGES}", presentation.explanation).into(),
                        tone,
                    ));
                }
                Some((presentation.explanation.into(), presentation.tone))
            }
            SettingsRowId::UnpairThisDevice => Some((
                if status == DirectRouteStatus::Refused(RefusalReason::Revoked) {
                    format!(
                        "\u{201C}{name}\u{201D} already unpaired this device. Remove the \
                         connection to forget it here."
                    )
                } else {
                    format!(
                        "Ends this device\u{2019}s pairing with \u{201C}{name}\u{201D} and removes \
                         the connection here at once, even while the server can\u{2019}t be \
                         reached."
                    )
                }
                .into(),
                CaptionTone::Guidance,
            )),
            _ => None,
        }
    }

    /// The unpair requests to show, from the registry's snapshot. Dismissing hides a request
    /// only while SpaceTerm can still finish it quietly: one that needs the person's permission
    /// shows again, because nothing else continues it.
    fn visible_pending_unpairs(&self) -> Vec<PendingUnpair> {
        self.direct_routes
            .as_ref()
            .map(|rows| rows.connections.pending_unpairs())
            .unwrap_or_default()
            .into_iter()
            .filter(|pending| !pending.dismissed || pending.state == UnpairState::NeedsInteraction)
            .collect()
    }

    pub(super) fn has_pending_unpairs(&self) -> bool {
        !self.visible_pending_unpairs().is_empty()
    }

    pub(super) fn pending_unpairs_caption(&self) -> Option<(SharedString, CaptionTone)> {
        let persistence = self
            .direct_routes
            .as_ref()
            .is_some_and(|rows| rows.connections.persistence_error());
        if persistence {
            return Some((
                SharedString::new_static(
                    "SpaceTerm couldn\u{2019}t confirm its latest changes to direct connections are \
                     saved on this computer, so these requests may not continue after SpaceTerm \
                     reopens.",
                ),
                CaptionTone::Warning,
            ));
        }
        Some((
            SharedString::new_static(PENDING_GUIDANCE),
            CaptionTone::Guidance,
        ))
    }

    fn run_unpair_action(
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

    /// Each unpair the server hasn't confirmed, or whose cleanup here is unfinished, with what it
    /// still needs and the steps that settle it. Nothing here reconnects or reads a server.
    pub(super) fn render_pending_unpairs(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let owner = cx.weak_entity();
        let entries =
            self.visible_pending_unpairs()
                .into_iter()
                .enumerate()
                .map(|(index, pending)| {
                    let (explanation, actions) = present_unpair(&pending);
                    let working = pending.state == UnpairState::Retrying;
                    let steps = actions.iter().map(|action| {
                        let action = *action;
                        let owner = owner.clone();
                        let pending = pending.clone();
                        let label = match (action, pending.state) {
                            (UnpairAction::Resume, UnpairState::Unconfirmed) => "Retry",
                            (action, _) => action.label(),
                        };
                        Button::new(
                            ElementId::from(SharedString::from(format!(
                                "{PENDING_SELECTOR}-{index}-{}",
                                action.slot()
                            ))),
                            label,
                        )
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Regular)
                        .tab_stop(true)
                        .debug_selector(format!("{PENDING_SELECTOR}-{index}-{}", action.slot()))
                        .accessibility_description(SharedString::from(format!(
                            "{label} the request for {}",
                            pending.server_label
                        )))
                        .on_activate(move |_, _, cx| {
                            let _ = owner.update(cx, |settings, cx| {
                                settings.run_unpair_action(&pending, action, cx);
                            });
                        })
                        .into_any_element()
                    });
                    let machine = SharedString::from(pending.server_label.clone());
                    div()
                        .id(ElementId::from(SharedString::from(format!(
                            "{PENDING_SELECTOR}-{index}"
                        ))))
                        .debug_selector(move || format!("{PENDING_SELECTOR}-{index}"))
                        .role(accesskit::Role::Group)
                        .aria_label(machine.clone())
                        .aria_description(SharedString::from(explanation.clone()))
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
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(appearance.spacing(6.0))
                                        .child(
                                            div()
                                                .chrome_text(
                                                    appearance.typography.style(TextRole::Body),
                                                )
                                                .text_color(gpui_color(colors.text))
                                                .child(machine.clone()),
                                        )
                                        .when(working, |heading| {
                                            heading.child(
                                                ProgressRing::new(
                                                    SharedString::from(format!(
                                                        "{PENDING_SELECTOR}-{index}-progress"
                                                    )),
                                                    "Trying again\u{2026}",
                                                    ProgressState::Indeterminate,
                                                )
                                                .size(ProgressSize::Compact),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .id(ElementId::from(SharedString::from(format!(
                                            "{PENDING_SELECTOR}-{index}-explanation"
                                        ))))
                                        .role(accesskit::Role::Label)
                                        .aria_value(SharedString::from(explanation.clone()))
                                        .chrome_text(
                                            appearance.typography.style(TextRole::Secondary),
                                        )
                                        .text_color(gpui_color(colors.text_secondary))
                                        .whitespace_normal()
                                        .child(SharedString::from(explanation)),
                                ),
                        )
                        .children(steps)
                });
        div()
            .debug_selector(|| PENDING_SELECTOR.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(8.0))
            .children(entries)
            .into_any_element()
    }
}
