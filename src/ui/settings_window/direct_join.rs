//! Join with Invitation: pairs this device with another computer's server.
//!
//! The person pastes an invitation from that server's Pair Device dialog. The field obscures it,
//! keeps no history of it, and hands it to the registry once, emptying itself as it does. While
//! the server's owner decides, the row names the server and shows both fingerprints, so the
//! person can see which server they asked and which device asked. An approved pairing becomes
//! a saved direct route, which the Servers list then selects.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, accesskit, div};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, FieldState, ProgressRing, ProgressSize, ProgressState,
};
use zeroize::Zeroizing;

use super::SettingsWindow;
use super::server_list::ServerChoice;
use crate::direct_connections::JoinStatus;
use crate::local_server::direct::DirectError;
use crate::local_server::pane::control::ClientName;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{countdown, remaining, unix_now};

pub(super) const INVITATION_SELECTOR: &str = "settings-join-invitation";
pub(super) const JOIN_SELECTOR: &str = "settings-join-invitation-join";
pub(super) const JOIN_CANCEL_SELECTOR: &str = "settings-join-invitation-cancel";
pub(super) const JOIN_STATUS_SELECTOR: &str = "settings-join-invitation-status";
/// How often a waiting join repaints its countdown.
const TICK: Duration = Duration::from_secs(1);

/// What a join that couldn't finish says. A declined request, a server that couldn't prove its
/// identity and a pairing interrupted on this computer each read differently, and none claims
/// more than SpaceTerm knows about what the server recorded.
fn join_failure(error: DirectError) -> &'static str {
    match error {
        DirectError::Rejected => DECLINED,
        DirectError::InvalidInvitation => {
            "That isn\u{2019}t a valid invitation, or it was already used. Ask for a new one."
        }
        DirectError::Expired => {
            "The invitation expired before the server approved this device. Ask for a new one."
        }
        DirectError::WrongServer => {
            "This invitation comes from a different kind of SpaceTerm build, so this one can\u{2019}t \
             use it."
        }
        DirectError::IncompatibleProtocol => {
            "This SpaceTerm and that server have no protocol version in common. Update the older \
             one, then ask for a new invitation."
        }
        DirectError::AuthenticationRefused
        | DirectError::PinMismatch
        | DirectError::ChangedClaim
        | DirectError::Substituted => {
            "The server couldn\u{2019}t prove it\u{2019}s the one that made this invitation, so \
             SpaceTerm stopped and won\u{2019}t use it."
        }
        DirectError::CredentialUnavailable | DirectError::InteractionRequired => {
            "SpaceTerm couldn\u{2019}t keep this device\u{2019}s key in the system\u{2019}s \
             credential storage, so this device can\u{2019}t use the pairing."
        }
        DirectError::StorageUnavailable => {
            "SpaceTerm couldn\u{2019}t save the connection on this computer, so this device \
             can\u{2019}t use it. If the server already approved this device, SpaceTerm \
             withdraws that pairing and shows its progress under Unpair requests."
        }
        DirectError::Capacity => {
            "SpaceTerm is already finishing as many pairings as it can. Try again shortly."
        }
        DirectError::Transport => {
            "SpaceTerm couldn\u{2019}t reach the server at the invitation\u{2019}s address. Check \
             that this computer can reach it on the network."
        }
        DirectError::Disabled
        | DirectError::Revoked
        | DirectError::InvalidRequest
        | DirectError::InvalidAddress
        | DirectError::AlreadyDecided => {
            "The server refused the request, so this device isn\u{2019}t paired."
        }
    }
}

/// What a request the server's owner declined says.
const DECLINED: &str = "The server\u{2019}s owner declined this device, so it isn\u{2019}t paired.";

impl SettingsWindow {
    /// Hands the pasted invitation to the registry once. The field empties as it does, so the
    /// invitation lives on only in the registry's own join.
    pub(super) fn join_with_invitation(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.direct_routes.as_mut() else {
            return;
        };
        let text = Zeroizing::new(rows.invitation.update(cx, |input, cx| input.take_value(cx)));
        if text.trim().is_empty() {
            return;
        }
        rows.join_failure = None;
        rows.join_cancelled = false;
        rows.join_selected = false;
        let connections = rows.connections.clone();
        // The name this device gives the server, as every other client connection gives it. It
        // labels the device and proves nothing.
        let name = ClientName::this_computer();
        rows._work = Some(cx.spawn(async move |settings, cx| {
            let joined = cx
                .background_executor()
                .spawn(async move { connections.join(text, name) })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                let Some(rows) = settings.direct_routes.as_mut() else {
                    return;
                };
                match joined {
                    Ok(join) => rows.join = Some(join),
                    Err(error) => {
                        rows.join = None;
                        rows.join_failure = Some(error);
                    }
                }
                settings.follow_join(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Follows the join underway: the countdown while the server's owner decides, and the new
    /// route once they approve.
    pub(super) fn follow_join(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.direct_routes.as_mut() else {
            return;
        };
        let Some(join) = rows.join else {
            rows.join_tick = None;
            return;
        };
        match rows.connections.join_status(join) {
            Some(JoinStatus::AwaitingApproval { .. } | JoinStatus::Validating) => {
                if rows.join_tick.is_none() {
                    rows.join_tick = Some(cx.spawn(async move |settings, cx| {
                        loop {
                            cx.background_executor().timer(TICK).await;
                            if settings.update(cx, |_, cx| cx.notify()).is_err() {
                                return;
                            }
                        }
                    }));
                }
            }
            Some(JoinStatus::Approved(route)) => {
                rows.join_tick = None;
                // The person just paired with this server, so it is the one they want to see.
                if !rows.join_selected {
                    rows.join_selected = true;
                    self.select_server(ServerChoice::Direct(route), cx);
                }
            }
            _ => rows.join_tick = None,
        }
    }

    fn cancel_join(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.direct_routes.as_mut() else {
            return;
        };
        if let Some(join) = rows.join {
            rows.join_cancelled = true;
            rows.connections.cancel_join(join);
        }
        rows.join_tick = None;
        cx.notify();
    }

    /// The field that pairs this device using an invitation, and the join it started.
    pub(super) fn render_join_invitation(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(rows) = self.direct_routes.as_ref() else {
            return div().into_any_element();
        };
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let owner = cx.weak_entity();
        let status = rows
            .join
            .and_then(|join| rows.connections.join_status(join));
        let waiting = matches!(
            status,
            Some(JoinStatus::Validating | JoinStatus::AwaitingApproval { .. })
        );
        let input = rows.invitation.clone();
        let focus = input.read(cx).focus_handle();
        let typed = !input.read(cx).value().is_empty();
        let field = spaceterm_ui::field_frame(
            "settings-join-invitation-frame",
            &focus,
            FieldState::default()
                .disabled(waiting)
                .invalid(rows.join_failure.is_some()),
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
        let join = {
            let owner = owner.clone();
            Button::new(JOIN_SELECTOR, "Join")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .disabled(!typed || waiting)
                .tab_stop(true)
                .debug_selector(JOIN_SELECTOR)
                .accessibility_description(
                    "Ask the server that made this invitation to pair with this device",
                )
                .on_activate(move |_, _, cx| {
                    let _ = owner.update(cx, |settings, cx| settings.join_with_invitation(cx));
                })
        };
        // Text that assistive technology reads needs an identity, which its order supplies.
        let texts = std::cell::Cell::new(0_usize);
        let readable = |text: SharedString| {
            let index = texts.get();
            texts.set(index + 1);
            gpui::Text::new(
                gpui::ElementId::from(SharedString::from(format!(
                    "{INVITATION_SELECTOR}-text-{index}"
                ))),
                text,
            )
        };
        let secondary = |text: SharedString, color: crate::appearance::Color| {
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(color))
                .whitespace_normal()
                .child(readable(text))
        };
        let report: Option<AnyElement> = match (&status, rows.join_failure) {
            (_, Some(error)) => Some(
                div()
                    .id(JOIN_STATUS_SELECTOR)
                    .debug_selector(|| JOIN_STATUS_SELECTOR.to_owned())
                    .role(accesskit::Role::Alert)
                    .child(secondary(join_failure(error).into(), colors.error))
                    .into_any_element(),
            ),
            (Some(JoinStatus::Validating), None) => Some(
                ProgressRing::new(
                    JOIN_STATUS_SELECTOR,
                    "Checking the invitation\u{2026}",
                    ProgressState::Indeterminate,
                )
                .size(ProgressSize::Compact)
                .debug_selector(JOIN_STATUS_SELECTOR)
                .into_any_element(),
            ),
            (
                Some(JoinStatus::AwaitingApproval {
                    server_name,
                    server_fingerprint,
                    device_fingerprint,
                    expires_at,
                }),
                None,
            ) => {
                let left = countdown(remaining(*expires_at, unix_now()));
                let cancel = {
                    let owner = owner.clone();
                    Button::new(JOIN_CANCEL_SELECTOR, "Cancel")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Regular)
                        .tab_stop(true)
                        .debug_selector(JOIN_CANCEL_SELECTOR)
                        .accessibility_description(SharedString::from(format!(
                            "Stop asking {server_name} to pair with this device"
                        )))
                        .on_activate(move |_, _, cx| {
                            let _ = owner.update(cx, |settings, cx| settings.cancel_join(cx));
                        })
                };
                Some(
                    div()
                        .id(JOIN_STATUS_SELECTOR)
                        .debug_selector(|| JOIN_STATUS_SELECTOR.to_owned())
                        .role(accesskit::Role::Status)
                        .aria_label(SharedString::from(format!(
                            "Waiting for approval on {server_name}"
                        )))
                        .flex()
                        .flex_row()
                        .items_start()
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
                                            ProgressRing::new(
                                                "settings-join-invitation-waiting",
                                                "Waiting for approval",
                                                ProgressState::Indeterminate,
                                            )
                                            .size(ProgressSize::Compact),
                                        )
                                        .child(
                                            div()
                                                .chrome_text(
                                                    appearance.typography.style(TextRole::Body),
                                                )
                                                .text_color(gpui_color(colors.text))
                                                .child(readable(SharedString::from(format!(
                                                    "Waiting for approval on \u{201C}{server_name}\u{201D}"
                                                )))),
                                        ),
                                )
                                .child(secondary(
                                    SharedString::from(format!(
                                        "On that computer, approve the request in the Pair \
                                         Device dialog. It expires in {left}."
                                    )),
                                    colors.text_secondary,
                                ))
                                .child(secondary(
                                    SharedString::from(format!(
                                        "This device\u{2019}s fingerprint {device_fingerprint}"
                                    )),
                                    colors.text_secondary,
                                ))
                                .child(secondary(
                                    SharedString::from(format!(
                                        "Server fingerprint {server_fingerprint}"
                                    )),
                                    colors.text_secondary,
                                )),
                        )
                        .child(cancel)
                        .into_any_element(),
                )
            }
            (Some(JoinStatus::Approved(_)), None) => Some(
                div()
                    .id(JOIN_STATUS_SELECTOR)
                    .debug_selector(|| JOIN_STATUS_SELECTOR.to_owned())
                    .role(accesskit::Role::Status)
                    .child(secondary(
                        "Paired. SpaceTerm is connecting to the server, which is selected below."
                            .into(),
                        colors.text_secondary,
                    ))
                    .into_any_element(),
            ),
            (Some(JoinStatus::Rejected), None) => Some(
                div()
                    .id(JOIN_STATUS_SELECTOR)
                    .debug_selector(|| JOIN_STATUS_SELECTOR.to_owned())
                    .role(accesskit::Role::Status)
                    .child(secondary(
                        // A request the server already received is withdrawn in the background and
                        // listed under Unpair requests until it is.
                        if rows.join_cancelled {
                            "You cancelled the request. SpaceTerm withdraws anything the server \
                             already received."
                        } else {
                            DECLINED
                        }
                        .into(),
                        colors.text_secondary,
                    ))
                    .into_any_element(),
            ),
            (Some(JoinStatus::Expired), None) => Some(
                div()
                    .id(JOIN_STATUS_SELECTOR)
                    .debug_selector(|| JOIN_STATUS_SELECTOR.to_owned())
                    .role(accesskit::Role::Status)
                    .child(secondary(
                        join_failure(DirectError::Expired).into(),
                        colors.text_secondary,
                    ))
                    .into_any_element(),
            ),
            (Some(JoinStatus::Failed(error)), None) => Some(
                div()
                    .id(JOIN_STATUS_SELECTOR)
                    .debug_selector(|| JOIN_STATUS_SELECTOR.to_owned())
                    .role(accesskit::Role::Alert)
                    .child(secondary(join_failure(*error).into(), colors.error))
                    .into_any_element(),
            ),
            (None, None) => None,
        };
        div()
            .debug_selector(|| "settings-join-invitation-row".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(8.0))
                    .child(field)
                    .child(join),
            )
            .child(secondary(
                "Pair with another computer\u{2019}s server over the network using an invitation \
                 from its Pair Device dialog."
                    .into(),
                colors.text_muted,
            ))
            .children(report)
            .into_any_element()
    }
}
