//! Pair Device: one invitation, and the answer to each device that claims it.
//!
//! The dialog opens a single-use invitation on the selected server and shows it while the dialog
//! stays open. The person passes it to the other device themselves; Copy is the only way it
//! reaches the clipboard. Each device that claims the invitation appears with the name it gave
//! itself and its fingerprint, and pairs only when the person approves that exact claim. Closing
//! the dialog withdraws the invitation, so nothing the dialog showed can be used afterwards.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, ElementId, EventEmitter, SharedString, Task, Window,
    accesskit, div,
};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, Dialog, DialogCloseDecision, DialogInitialFocus,
    DialogOutcome, DialogSize, ModalAction, ModalActionRole, ModalId, ProgressRing, ProgressSize,
    ProgressState,
};

use crate::local_server::direct::ClaimId;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{
    AccessFailure, Claim, Device, DirectAccessControl, OpenInvitation, countdown, remaining,
    unix_now,
};

pub(crate) const MODAL_ID: &str = "pair-device";
pub(crate) const DONE_SELECTOR: &str = "pair-device-done";
pub(crate) const INVITATION_SELECTOR: &str = "pair-device-invitation";
pub(crate) const COPY_SELECTOR: &str = "pair-device-copy";
pub(crate) const RENEW_SELECTOR: &str = "pair-device-renew";
pub(crate) const CLAIM_SELECTOR: &str = "pair-device-claim";
pub(crate) const APPROVE_SELECTOR: &str = "pair-device-approve";
pub(crate) const REJECT_SELECTOR: &str = "pair-device-reject";
/// How often an open invitation looks for claims and its countdown repaints.
const POLL: Duration = Duration::from_secs(1);
/// How long Copy reads "Copied" after the person copies the invitation.
const COPIED_FEEDBACK: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairAction {
    Done,
}

/// Where the invitation stands.
enum Invitation {
    Opening,
    Open(OpenInvitation),
    /// The invitation's time ran out, or the server ended it, before any device paired.
    Ended,
    /// A device paired, which used the invitation.
    Paired(Device),
    Failed(AccessFailure),
}

/// A device the person paired in this dialog.
pub(crate) struct DevicePaired;

/// The Pair Device dialog's body, owned by the dialog for as long as it is open.
pub(crate) struct PairDevice {
    control: Arc<dyn DirectAccessControl>,
    server: SharedString,
    server_fingerprint: SharedString,
    invitation: Invitation,
    claims: Vec<Claim>,
    /// The claim whose answer the server is deciding. One answer is asked at a time.
    answering: Option<ClaimId>,
    /// Why the latest answer didn't take effect.
    problem: Option<AccessFailure>,
    copied: bool,
    /// Counts invitations, so a result for a replaced one lands nowhere.
    generation: u64,
    _poll: Option<Task<()>>,
    _work: Option<Task<()>>,
    _copied: Option<Task<()>>,
}

impl EventEmitter<DevicePaired> for PairDevice {}

impl PairDevice {
    /// Opens an invitation on `control`'s server and presents it in `window` until the person
    /// closes the dialog. `on_paired` hears of each device the person pairs here, in the
    /// presenting entity's own update.
    pub(crate) fn present<T: 'static>(
        control: Arc<dyn DirectAccessControl>,
        server: SharedString,
        server_fingerprint: SharedString,
        on_paired: impl Fn(&mut T, &mut Context<T>) + 'static,
        window: &Window,
        cx: &mut Context<T>,
    ) -> bool {
        let body = cx.new(|cx| {
            let mut body = Self {
                control,
                server: server.clone(),
                server_fingerprint,
                invitation: Invitation::Opening,
                claims: Vec::new(),
                answering: None,
                problem: None,
                copied: false,
                generation: 0,
                _poll: None,
                _work: None,
                _copied: None,
            };
            body.open_invitation(cx);
            body
        });
        cx.subscribe(&body, move |owner, _, _: &DevicePaired, cx| {
            on_paired(owner, cx)
        })
        .detach();
        // The dialog lets go of its body as it closes, so its result keeps the body to withdraw
        // the invitation it shows.
        let closing = body.clone();
        let dialog = Dialog::new(
            ModalId::new(MODAL_ID),
            format!("Pair a device with {server}"),
            "Pair Device",
            vec![ModalAction::new(
                PairAction::Done,
                "Done",
                ModalActionRole::Cancel,
                DONE_SELECTOR,
            )],
            DialogInitialFocus::Action(PairAction::Done),
        )
        .description(format!(
            "Another computer can ask to pair with \u{201C}{server}\u{201D} using this \
             invitation. It connects only after you approve it here."
        ))
        .size(DialogSize::Wide)
        .body(body);
        dialog
            .present(
                window,
                cx,
                |_, _, _| DialogCloseDecision::Allow,
                move |_: DialogOutcome<PairAction>, cx| {
                    closing.update(cx, |body, cx| body.close(cx));
                },
            )
            .is_ok()
    }

    /// Asks the server for a fresh invitation, forgetting the previous one and its claims.
    fn open_invitation(&mut self, cx: &mut Context<Self>) {
        self.withdraw_open(cx);
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.invitation = Invitation::Opening;
        self.claims.clear();
        self.answering = None;
        self.problem = None;
        self.copied = false;
        self._poll = None;
        let control = Arc::clone(&self.control);
        self._work = Some(cx.spawn(async move |body, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move { control.invite() })
                .await;
            let _ = body.update(cx, |body, cx| {
                if body.generation != generation {
                    // A replaced request still opened an invitation, so end it at once.
                    if let Ok(invitation) = opened {
                        body.withdraw(invitation.id, cx);
                    }
                    return;
                }
                body.invitation = match opened {
                    Ok(invitation) => Invitation::Open(invitation),
                    Err(failure) => Invitation::Failed(failure),
                };
                body.follow_claims(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Reads the claims on the open invitation each second, and ends it when its time runs out.
    fn follow_claims(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.invitation, Invitation::Open(_)) {
            return;
        }
        let generation = self.generation;
        let control = Arc::clone(&self.control);
        self._poll = Some(cx.spawn(async move |body, cx| {
            loop {
                let expired = body
                    .update(cx, |body, cx| {
                        let Invitation::Open(invitation) = &body.invitation else {
                            return true;
                        };
                        if remaining(invitation.expires_at, unix_now()) == 0 {
                            body.withdraw_open(cx);
                            body.invitation = Invitation::Ended;
                            body.claims.clear();
                            cx.notify();
                            return true;
                        }
                        // The countdown moves even when no claim does.
                        cx.notify();
                        false
                    })
                    .unwrap_or(true);
                if expired {
                    return;
                }
                let reading = Arc::clone(&control);
                let claims = cx
                    .background_executor()
                    .spawn(async move { reading.claims() })
                    .await;
                let ended = body
                    .update(cx, |body, cx| {
                        if body.generation != generation {
                            return true;
                        }
                        match claims {
                            Ok(claims) => body.claims = claims,
                            Err(AccessFailure::InvitationEnded) => {
                                body.invitation = Invitation::Ended;
                                body.claims.clear();
                            }
                            // A missed reading leaves the claims shown; the next one replaces
                            // them.
                            Err(_) => {}
                        }
                        cx.notify();
                        !matches!(body.invitation, Invitation::Open(_))
                    })
                    .unwrap_or(true);
                if ended {
                    return;
                }
                cx.background_executor().timer(POLL).await;
            }
        }));
    }

    /// Pairs the device that made `claim`, exactly as the person saw it.
    fn approve(&mut self, claim: ClaimId, cx: &mut Context<Self>) {
        if self.answering.is_some() {
            return;
        }
        let Some(shown) = self.claims.iter().find(|shown| shown.id == claim).cloned() else {
            return;
        };
        self.answering = Some(claim);
        self.problem = None;
        let generation = self.generation;
        let control = Arc::clone(&self.control);
        self._work = Some(cx.spawn(async move |body, cx| {
            let approved = cx
                .background_executor()
                .spawn(async move { control.approve(&shown.claim) })
                .await;
            let _ = body.update(cx, |body, cx| {
                if body.generation != generation {
                    return;
                }
                body.answering = None;
                match approved {
                    Ok(device) => {
                        body._poll = None;
                        body.claims.clear();
                        body.invitation = Invitation::Paired(device);
                        cx.emit(DevicePaired);
                    }
                    Err(failure) => {
                        body.claims.retain(|shown| shown.id != claim);
                        body.problem = Some(failure);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Refuses `claim`. The invitation stays open for another device until its time runs out.
    fn reject(&mut self, claim: ClaimId, cx: &mut Context<Self>) {
        if self.answering.is_some() {
            return;
        }
        self.answering = Some(claim);
        self.problem = None;
        let generation = self.generation;
        let control = Arc::clone(&self.control);
        self._work = Some(cx.spawn(async move |body, cx| {
            let rejected = cx
                .background_executor()
                .spawn(async move { control.reject(claim) })
                .await;
            let _ = body.update(cx, |body, cx| {
                if body.generation != generation {
                    return;
                }
                body.answering = None;
                body.claims.retain(|shown| shown.id != claim);
                if let Err(failure) = rejected {
                    body.problem = Some(failure);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Puts the invitation on the clipboard because the person asked. Nothing else writes it
    /// there.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Invitation::Open(invitation) = &self.invitation else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(invitation.text.to_string()));
        self.copied = true;
        self._copied = Some(cx.spawn(async move |body, cx| {
            cx.background_executor().timer(COPIED_FEEDBACK).await;
            let _ = body.update(cx, |body, cx| {
                body.copied = false;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Ends the open invitation, if any. Its text is wiped when the state that held it drops.
    fn withdraw_open(&self, cx: &App) {
        if let Invitation::Open(invitation) = &self.invitation {
            self.withdraw(invitation.id, cx);
        }
    }

    fn withdraw(&self, invitation: ClaimId, cx: &App) {
        let control = Arc::clone(&self.control);
        // Nobody waits on the answer: an invitation the server keeps still expires on its own.
        cx.background_executor()
            .spawn(async move {
                let _ = control.withdraw(invitation);
            })
            .detach();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.withdraw_open(cx);
        self.generation = self.generation.wrapping_add(1);
        self.invitation = Invitation::Ended;
        self.claims.clear();
        self._poll = None;
        self._work = None;
        self._copied = None;
        cx.notify();
    }
}

impl Render for PairDevice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::appearance::shared_chrome(cx);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        // Text that assistive technology reads needs an identity, which its order supplies.
        let texts = std::cell::Cell::new(0_usize);
        let readable = |text: SharedString| {
            let index = texts.get();
            texts.set(index + 1);
            gpui::Text::new(
                ElementId::from(SharedString::from(format!("{MODAL_ID}-text-{index}"))),
                text,
            )
        };
        let secondary = |text: SharedString| {
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.text_secondary))
                .whitespace_normal()
                .child(readable(text))
        };
        let owner = cx.weak_entity();
        let invitation: AnyElement = match &self.invitation {
            Invitation::Opening => ProgressRing::new(
                "pair-device-opening",
                "Creating an invitation\u{2026}",
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .into_any_element(),
            Invitation::Open(open) => {
                let left = remaining(open.expires_at, unix_now());
                let copy = {
                    let owner = owner.clone();
                    Button::new(COPY_SELECTOR, if self.copied { "Copied" } else { "Copy" })
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Regular)
                        .tab_stop(true)
                        .debug_selector(COPY_SELECTOR)
                        .accessibility_description("Copy the invitation to the clipboard")
                        .on_activate(move |_, _, cx| {
                            let _ = owner.update(cx, |body, cx| body.copy(cx));
                        })
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(6.0))
                    .child(
                        div()
                            .id(INVITATION_SELECTOR)
                            .debug_selector(|| INVITATION_SELECTOR.to_owned())
                            // The invitation is shown to read and Copy, and never published as
                            // accessible text, so assistive technology and its recordings keep no
                            // copy of it.
                            .role(accesskit::Role::Group)
                            .aria_label("Invitation, available to Copy")
                            .p(appearance.spacing(10.0))
                            .rounded(RadiusRole::Control.pixels())
                            .border_1()
                            .border_color(gpui_color(colors.input_border))
                            .bg(gpui_color(colors.input_background))
                            .chrome_text(appearance.typography.style(TextRole::Body))
                            .text_color(gpui_color(colors.input_text))
                            .whitespace_normal()
                            .child(SharedString::from(open.text.to_string())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(appearance.spacing(8.0))
                            .child(copy)
                            .child(secondary(SharedString::from(format!(
                                "Expires in {}. It works once.",
                                countdown(left)
                            )))),
                    )
                    .child(secondary(SharedString::from(
                        "On the other computer, open SpaceTerm Settings, choose Servers, then \
                         Join with Invitation and paste it.",
                    )))
                    .into_any_element()
            }
            Invitation::Ended | Invitation::Failed(_) => {
                let explanation = match &self.invitation {
                    Invitation::Failed(failure) => failure.explanation(&self.server),
                    _ => "This invitation can\u{2019}t be used any more.".to_owned(),
                };
                let renew = {
                    let owner = owner.clone();
                    Button::new(RENEW_SELECTOR, "New Invitation")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Regular)
                        .tab_stop(true)
                        .debug_selector(RENEW_SELECTOR)
                        .on_activate(move |_, _, cx| {
                            let _ = owner.update(cx, |body, cx| body.open_invitation(cx));
                        })
                };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(8.0))
                    .child(div().flex_1().child(secondary(explanation.into())))
                    .child(renew)
                    .into_any_element()
            }
            Invitation::Paired(device) => div()
                .id("pair-device-paired")
                .role(accesskit::Role::Status)
                .flex()
                .flex_col()
                .gap(appearance.spacing(4.0))
                .child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                        .text_color(gpui_color(colors.text))
                        .child(gpui::Text::new(
                            ElementId::from("pair-device-paired-text"),
                            SharedString::from(format!(
                                "\u{201C}{}\u{201D} is paired with \u{201C}{}\u{201D}.",
                                device.name, self.server
                            )),
                        )),
                )
                .child(secondary(SharedString::from(format!(
                    "It can connect now. Device fingerprint {}.",
                    device.fingerprint
                ))))
                .into_any_element(),
        };
        let claims = self.claims.iter().enumerate().map(|(index, claim)| {
            let answering = self.answering.is_some();
            let this = self.answering == Some(claim.id);
            let id = claim.id;
            let approve = {
                let owner = owner.clone();
                Button::new(
                    ElementId::from(SharedString::from(format!("{APPROVE_SELECTOR}-{index}"))),
                    "Approve",
                )
                .variant(ButtonVariant::Primary)
                .size(ButtonSize::Regular)
                .tab_stop(true)
                .disabled(answering)
                .debug_selector(format!("{APPROVE_SELECTOR}-{index}"))
                .accessibility_description(SharedString::from(format!(
                    "Pair {} with {}",
                    claim.name, self.server
                )))
                .on_activate(move |_, _, cx| {
                    let _ = owner.update(cx, |body, cx| body.approve(id, cx));
                })
            };
            let reject = {
                let owner = owner.clone();
                Button::new(
                    ElementId::from(SharedString::from(format!("{REJECT_SELECTOR}-{index}"))),
                    "Reject",
                )
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .tab_stop(true)
                .disabled(answering)
                .debug_selector(format!("{REJECT_SELECTOR}-{index}"))
                .accessibility_description(SharedString::from(format!(
                    "Refuse the request from {}",
                    claim.name
                )))
                .on_activate(move |_, _, cx| {
                    let _ = owner.update(cx, |body, cx| body.reject(id, cx));
                })
            };
            div()
                .id(ElementId::from(SharedString::from(format!(
                    "{CLAIM_SELECTOR}-{index}"
                ))))
                .debug_selector(move || format!("{CLAIM_SELECTOR}-{index}"))
                .role(accesskit::Role::Group)
                .aria_label(SharedString::from(format!("Request from {}", claim.name)))
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .p(appearance.spacing(10.0))
                .rounded(RadiusRole::Control.pixels())
                .border_1()
                .border_color(gpui_color(colors.border))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(appearance.spacing(2.0))
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                                .text_color(gpui_color(colors.text))
                                .truncate()
                                .child(readable(claim.name.clone())),
                        )
                        .child(secondary(SharedString::from(format!(
                            "Device fingerprint {}",
                            claim.fingerprint
                        ))))
                        .child(secondary(SharedString::from(format!(
                            "The request expires in {}.",
                            countdown(remaining(claim.expires_at, unix_now()))
                        ))))
                        .child(secondary(SharedString::from(format!(
                            "Asks to pair with \u{201C}{}\u{201D}, fingerprint {}",
                            self.server, self.server_fingerprint
                        )))),
                )
                .when(this, |row| {
                    row.child(
                        ProgressRing::new(
                            SharedString::from(format!("{CLAIM_SELECTOR}-{index}-progress")),
                            "Answering\u{2026}",
                            ProgressState::Indeterminate,
                        )
                        .size(ProgressSize::Compact),
                    )
                })
                .child(reject)
                .child(approve)
        });
        let waiting = matches!(self.invitation, Invitation::Open(_)) && self.claims.is_empty();
        div()
            .flex()
            .flex_col()
            .gap(appearance.spacing(12.0))
            .child(invitation)
            .when(!self.claims.is_empty(), |body| {
                body.child(secondary(SharedString::from(
                    "The name is what the device calls itself. Approve only a request you \
                     expect.",
                )))
                .children(claims)
            })
            .when(waiting, |body| {
                body.child(
                    div()
                        .id("pair-device-waiting")
                        .role(accesskit::Role::Status)
                        .child(secondary(SharedString::from(
                            "Waiting for a device to use this invitation\u{2026}",
                        ))),
                )
            })
            .when_some(self.problem, |body, problem| {
                body.child(
                    div()
                        .id("pair-device-problem")
                        .role(accesskit::Role::Alert)
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.error))
                        .whitespace_normal()
                        .child(readable(SharedString::from(
                            problem.explanation(&self.server),
                        ))),
                )
            })
    }
}
