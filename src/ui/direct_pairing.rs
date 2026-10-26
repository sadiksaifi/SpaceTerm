//! Invite Device: one invitation link for another device, and the person's answer when a device
//! uses it.
//!
//! The sheet opens a single-use invitation on the selected server and shows its link as a QR code
//! for phones and as text to Copy for computers. Copy is the only way the link reaches the
//! clipboard. The first device that uses the link replaces the sheet with "Allow <device>?",
//! which shows the code both screens derive from the authenticated exchange, and the device pairs
//! only when the person allows that exact claim. Closing Invite Device withdraws the invitation,
//! which also refuses a claim nobody answered, so nothing it showed can be used afterwards.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, ClipboardItem, Context, ElementId, Entity, Rgba,
    SharedString, Task, Window, accesskit, canvas, div, fill, point, px, size,
};
use qrcodegen::{QrCode, QrCodeEcc};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, Dialog, DialogActionRequest, DialogCloseDecision,
    DialogInitialFocus, DialogOutcome, DialogPendingCompletion, DialogSize, Icon, IconName,
    ModalAction, ModalActionEmphasis, ModalActionRole, ModalCloseReason, ModalId, ProgressRing,
    ProgressSize, ProgressState,
};

use crate::server_connection::direct::ClaimId;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_access::{
    AccessFailure, Claim, Device, DirectAccessControl, OpenInvitation, code_speech, code_text,
    countdown, remaining,
};

pub(crate) const MODAL_ID: &str = "invite-device";
pub(crate) const ALLOW_MODAL_ID: &str = "invite-device-allow";
pub(crate) const DONE_SELECTOR: &str = "invite-device-done";
pub(crate) const RENEW_SELECTOR: &str = "invite-device-renew";
pub(crate) const ALLOW_SELECTOR: &str = "invite-device-allow";
pub(crate) const DONT_ALLOW_SELECTOR: &str = "invite-device-dont-allow";
pub(crate) const QR_SELECTOR: &str = "invite-device-qr";
pub(crate) const LINK_SELECTOR: &str = "invite-device-link";
pub(crate) const COPY_SELECTOR: &str = "invite-device-copy";
pub(crate) const STATUS_SELECTOR: &str = "invite-device-status";
pub(crate) const CODE_SELECTOR: &str = "invite-device-code";
/// How often an open invitation looks for claims and its countdown repaints.
const POLL: Duration = Duration::from_secs(1);
/// How long Copy reads "Copied" after the person copies the link.
const COPIED_FEEDBACK: Duration = Duration::from_secs(2);
/// The QR code's side, quiet zone included, which a phone reads from an arm's length.
const QR_EXTENT: f32 = 208.0;
/// The light margin, in modules, that QR readers need around the code.
const QR_QUIET_ZONE: i32 = 4;
/// QR readers expect dark modules on a light ground in every appearance.
const QR_DARK: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const QR_LIGHT: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// The sheet's actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SheetAction {
    Done,
    NewInvitation,
}

/// The person's answer to a device that used the invitation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Answer {
    DontAllow,
    Allow,
}

/// Where the invitation stands.
enum Stage {
    Opening,
    Waiting {
        invitation: OpenInvitation,
        /// The link as a QR code, or `None` when the link is too long for one.
        code: Option<Rc<QrCode>>,
    },
    /// A device used the invitation and the person is deciding whether it may connect.
    Asking {
        invitation: ClaimId,
        deadline: Instant,
        claim: Box<Claim>,
    },
    /// The person allowed the device and the server is pairing it.
    Allowing(SharedString),
    Allowed(Device),
    /// The person didn't allow the device.
    Declined(SharedString),
    /// The invitation's time ran out, or the server ended it, before a device paired.
    Expired,
    Failed(AccessFailure),
    Closed,
}

/// What Invite Device presents for a stage. Each surface has its own title and actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Surface {
    /// The invitation, or the work of making or answering one, with Done.
    Sheet,
    /// The invitation can't be used, with New Invitation as the primary action.
    Ended,
    /// A device paired, with Done.
    Allowed,
    /// "Allow <device>?", with Don't Allow and Allow.
    Asking,
}

impl Stage {
    fn surface(&self) -> Option<Surface> {
        match self {
            Self::Opening | Self::Waiting { .. } | Self::Allowing(_) => Some(Surface::Sheet),
            Self::Declined(_) | Self::Expired | Self::Failed(_) => Some(Surface::Ended),
            Self::Allowed(_) => Some(Surface::Allowed),
            Self::Asking { .. } => Some(Surface::Asking),
            Self::Closed => None,
        }
    }

    /// The invitation this stage still holds open on the server.
    fn open_invitation(&self) -> Option<ClaimId> {
        match self {
            Self::Waiting { invitation, .. } => Some(invitation.id),
            Self::Asking { invitation, .. } => Some(*invitation),
            _ => None,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Waiting { invitation, .. } => Some(invitation.deadline),
            Self::Asking { deadline, .. } => Some(*deadline),
            _ => None,
        }
    }
}

/// Invite Device's content and state. It is the body of each surface it presents, and lives as
/// long as one of them is open.
pub(crate) struct InviteDevice {
    control: Arc<dyn DirectAccessControl>,
    server: SharedString,
    window: AnyWindowHandle,
    /// Tells the presenting view about a device the person allowed, even after Invite Device
    /// closes, so its device list stays current.
    on_paired: Rc<dyn Fn(&mut App)>,
    stage: Stage,
    /// The surface on screen, if any.
    shown: Option<Surface>,
    /// Counts presented surfaces, so the result of one this sheet replaced lands nowhere.
    surface: u64,
    /// Counts invitations, so a result for a replaced one lands nowhere.
    generation: u64,
    copied: bool,
    _poll: Option<Task<()>>,
    _copied: Option<Task<()>>,
}

impl InviteDevice {
    /// Opens an invitation on `control`'s server and presents Invite Device in `window` until the
    /// person closes it. `on_paired` hears of each device the person allows, in the presenting
    /// entity's own update.
    pub(crate) fn present<T: 'static>(
        control: Arc<dyn DirectAccessControl>,
        server: SharedString,
        on_paired: impl Fn(&mut T, &mut Context<T>) + 'static,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> bool {
        let owner = cx.weak_entity();
        let on_paired: Rc<dyn Fn(&mut App)> = Rc::new(move |cx: &mut App| {
            let _ = owner.update(cx, |owner, cx| on_paired(owner, cx));
        });
        let body = cx.new(|cx: &mut Context<Self>| {
            // Invite Device ends with its window too, and the invitation it holds ends with it.
            cx.on_release(|body: &mut Self, cx: &mut App| body.withdraw_open(cx))
                .detach();
            Self {
                control,
                server,
                window: window.window_handle(),
                on_paired,
                stage: Stage::Opening,
                shown: None,
                surface: 0,
                generation: 0,
                copied: false,
                _poll: None,
                _copied: None,
            }
        });
        body.update(cx, |body, cx| {
            body.open_invitation(window, cx);
            body.shown.is_some()
        })
    }

    /// Runs `update` on `body` in the window it presents in. Answers false when that window is
    /// gone.
    fn in_window(
        body: &Entity<Self>,
        cx: &mut App,
        update: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>),
    ) -> bool {
        let window = body.read(cx).window;
        window
            .update(cx, |_, window, cx| {
                body.update(cx, |body, cx| update(body, window, cx));
            })
            .is_ok()
    }

    /// Presents the surface the stage needs, replacing the one on screen when it differs.
    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(surface) = self.stage.surface() else {
            return;
        };
        if self.shown == Some(surface) {
            cx.notify();
            return;
        }
        let replace = self.shown.is_some();
        self.surface = self.surface.wrapping_add(1);
        let presented = match surface {
            Surface::Asking => self.present_question(replace, window, cx),
            _ => self.present_sheet(surface, replace, window, cx),
        };
        if presented {
            self.shown = Some(surface);
        } else {
            self.shown = None;
            self.close(cx);
        }
        cx.notify();
    }

    fn present_sheet(
        &self,
        surface: Surface,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let shown = self.surface;
        let mut actions = vec![ModalAction::new(
            SheetAction::Done,
            "Done",
            ModalActionRole::Cancel,
            DONE_SELECTOR,
        )];
        let mut focus = SheetAction::Done;
        if surface == Surface::Ended {
            actions.push(
                ModalAction::new(
                    SheetAction::NewInvitation,
                    "New Invitation",
                    ModalActionRole::Affirmative,
                    RENEW_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent)
                .default_action(true),
            );
            focus = SheetAction::NewInvitation;
        }
        let title = format!("Invite Device to {}", self.server);
        let dialog = Dialog::new(
            ModalId::new(MODAL_ID),
            title.clone(),
            title,
            actions,
            DialogInitialFocus::Action(focus),
        )
        .description("Scan the code or send the link.")
        .size(DialogSize::Regular)
        .body(cx.entity());
        let renewing = cx.entity();
        let on_action = move |request: DialogActionRequest<SheetAction>,
                              _: DialogPendingCompletion,
                              cx: &mut App| {
            match request.action_id() {
                SheetAction::Done => DialogCloseDecision::Allow,
                // The sheet stays, and shows the new invitation once the person asked for it.
                SheetAction::NewInvitation => {
                    let renewing = renewing.clone();
                    cx.defer(move |cx| {
                        Self::in_window(&renewing, cx, |body, window, cx| {
                            body.open_invitation(window, cx);
                        });
                    });
                    DialogCloseDecision::Deny {
                        first_invalid: None,
                    }
                }
            }
        };
        let closing = cx.entity();
        let on_result = move |_: DialogOutcome<SheetAction>, cx: &mut App| {
            closing.update(cx, |body, cx| {
                if body.surface == shown {
                    body.shown = None;
                    body.close(cx);
                }
            });
        };
        if replace {
            dialog
                .replace_active(window, cx, on_action, on_result, |_, _| {})
                .is_ok()
        } else {
            dialog.present(window, cx, on_action, on_result).is_ok()
        }
    }

    /// Asks the person whether the device that used the invitation may connect.
    fn present_question(&self, replace: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Stage::Asking { claim, .. } = &self.stage else {
            return false;
        };
        let shown = self.surface;
        let device = claim.name.clone();
        let dialog = Dialog::new(
            ModalId::new(ALLOW_MODAL_ID),
            format!("Allow {device}?"),
            format!("Allow \u{201C}{device}\u{201D}?"),
            vec![
                ModalAction::new(
                    Answer::DontAllow,
                    "Don\u{2019}t Allow",
                    ModalActionRole::Cancel,
                    DONT_ALLOW_SELECTOR,
                ),
                ModalAction::new(
                    Answer::Allow,
                    "Allow",
                    ModalActionRole::Affirmative,
                    ALLOW_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent)
                .default_action(true),
            ],
            DialogInitialFocus::Action(Answer::Allow),
        )
        .description(format!("It gets full access to \"{}\".", self.server))
        .size(DialogSize::Compact)
        .body(cx.entity());
        let answering = cx.entity();
        let on_result = move |outcome: DialogOutcome<Answer>, cx: &mut App| {
            let current = answering.read(cx).surface == shown;
            if !current {
                // Invite Device replaced the question itself, such as when the invitation expired.
                return;
            }
            answering.update(cx, |body, _| body.shown = None);
            let answer = match outcome {
                DialogOutcome::Completed { action_id, .. } => Some(action_id),
                DialogOutcome::Dismissed(ModalCloseReason::Cancelled) => Some(Answer::DontAllow),
                // Nobody answered, so the device stays out.
                DialogOutcome::Dismissed(_) | DialogOutcome::ProgrammaticallyCompleted => None,
            };
            let answered = answer.is_some_and(|answer| {
                Self::in_window(&answering, cx, |body, window, cx| match answer {
                    Answer::Allow => body.allow(window, cx),
                    Answer::DontAllow => body.decline(window, cx),
                })
            });
            if !answered {
                answering.update(cx, |body, cx| body.close(cx));
            }
        };
        let on_action = |_: DialogActionRequest<Answer>,
                         _: DialogPendingCompletion,
                         _: &mut App| { DialogCloseDecision::Allow };
        if replace {
            dialog
                .replace_active(window, cx, on_action, on_result, |_, _| {})
                .is_ok()
        } else {
            dialog.present(window, cx, on_action, on_result).is_ok()
        }
    }

    /// Asks the server for a fresh invitation, withdrawing the previous one.
    fn open_invitation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.withdraw_open(cx);
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.stage = Stage::Opening;
        self.copied = false;
        self._copied = None;
        self.show(window, cx);
        if self.shown.is_none() {
            return;
        }
        let control = Arc::clone(&self.control);
        self._poll = Some(cx.spawn_in(window, async move |body, cx| {
            let reading = Arc::clone(&control);
            let opened = cx
                .background_executor()
                .spawn(async move { reading.invite() })
                .await;
            let opened = body.update_in(cx, |body, window, cx| {
                if body.generation != generation {
                    // A replaced request still opened an invitation, so end it at once.
                    if let Ok(invitation) = &opened {
                        body.withdraw(invitation.id, cx);
                    }
                    return false;
                }
                body.stage = match opened {
                    Ok(invitation) => {
                        let code = QrCode::encode_text(&invitation.text, QrCodeEcc::Medium)
                            .ok()
                            .map(Rc::new);
                        Stage::Waiting { invitation, code }
                    }
                    Err(failure) => Stage::Failed(failure),
                };
                body.show(window, cx);
                matches!(body.stage, Stage::Waiting { .. })
            });
            if !matches!(opened, Ok(true)) {
                return;
            }
            loop {
                let read = body
                    .update_in(cx, |body, window, cx| body.tick(generation, window, cx))
                    .unwrap_or(false);
                if read {
                    let reading = Arc::clone(&control);
                    let claims = cx
                        .background_executor()
                        .spawn(async move { reading.claims() })
                        .await;
                    let _ = body.update_in(cx, |body, window, cx| {
                        body.receive(generation, claims, window, cx);
                    });
                }
                let open = body
                    .update(cx, |body, _| {
                        body.generation == generation && body.stage.deadline().is_some()
                    })
                    .unwrap_or(false);
                if !open {
                    return;
                }
                cx.background_executor().timer(POLL).await;
            }
        }));
    }

    /// Ends an invitation whose time ran out and repaints the countdown otherwise. Answers whether
    /// to look for claims.
    fn tick(&mut self, generation: u64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.generation != generation {
            return false;
        }
        let Some(deadline) = self.stage.deadline() else {
            return false;
        };
        if remaining(deadline, Instant::now()) == 0 {
            self.expire(window, cx);
            return false;
        }
        cx.notify();
        matches!(self.stage, Stage::Waiting { .. })
    }

    /// Asks the person about the first device that used this invitation.
    fn receive(
        &mut self,
        generation: u64,
        claims: Result<Vec<Claim>, AccessFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation {
            return;
        }
        let Stage::Waiting { invitation, .. } = &self.stage else {
            return;
        };
        match claims {
            Ok(claims) => {
                let Some(claim) = claims
                    .into_iter()
                    .find(|claim| invitation.claimed_by(claim))
                else {
                    return;
                };
                // The link has done its work, so the state that held it drops and wipes it.
                self.stage = Stage::Asking {
                    invitation: invitation.id,
                    deadline: invitation.deadline,
                    claim: Box::new(claim),
                };
                self.copied = false;
                self._copied = None;
                self.show(window, cx);
            }
            Err(AccessFailure::InvitationEnded) => self.expire(window, cx),
            // A missed reading changes nothing; the next one looks again.
            Err(_) => {}
        }
    }

    fn expire(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.withdraw_open(cx);
        self.stage = Stage::Expired;
        self.show(window, cx);
    }

    /// Pairs the device that made the claim, exactly as the person saw it.
    fn allow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Asking { .. }) {
            return;
        }
        let Stage::Asking {
            invitation, claim, ..
        } = std::mem::replace(&mut self.stage, Stage::Closed)
        else {
            return;
        };
        self.stage = Stage::Allowing(claim.name.clone());
        self.show(window, cx);
        let generation = self.generation;
        let control = Arc::clone(&self.control);
        let on_paired = Rc::clone(&self.on_paired);
        // The answer is the person's decision, so it completes and reaches the device list even
        // when Invite Device closes first.
        cx.spawn_in(window, async move |body, cx| {
            let approved = cx
                .background_executor()
                .spawn(async move {
                    let approved = control.approve(&claim.claim);
                    // The invitation is used either way; withdrawing it frees its place.
                    let _ = control.withdraw(invitation);
                    approved
                })
                .await;
            if approved.is_ok() {
                let _ = cx.update(|_, cx| on_paired(cx));
            }
            let _ = body.update_in(cx, |body, window, cx| {
                if body.generation != generation {
                    return;
                }
                body.stage = match approved {
                    Ok(device) => Stage::Allowed(device),
                    Err(failure) => Stage::Failed(failure),
                };
                body.show(window, cx);
            });
        })
        .detach();
    }

    /// Refuses the device that made the claim. The invitation is used, so it ends too.
    fn decline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Asking { .. }) {
            return;
        }
        let Stage::Asking {
            invitation, claim, ..
        } = std::mem::replace(&mut self.stage, Stage::Closed)
        else {
            return;
        };
        self.stage = Stage::Declined(claim.name.clone());
        self.show(window, cx);
        let control = Arc::clone(&self.control);
        let id = claim.id;
        // Withdrawing the invitation also refuses its claim if the refusal didn't arrive.
        cx.background_executor()
            .spawn(async move {
                let _ = control.reject(id);
                let _ = control.withdraw(invitation);
            })
            .detach();
    }

    /// Puts the link on the clipboard because the person asked. Nothing else writes it there.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Stage::Waiting { invitation, .. } = &self.stage else {
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

    /// Ends the invitation the stage holds open, if any, which also refuses an unanswered claim.
    fn withdraw_open(&self, cx: &App) {
        if let Some(invitation) = self.stage.open_invitation() {
            self.withdraw(invitation, cx);
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

    /// Ends Invite Device after its surface closed. A pairing already allowed still completes,
    /// and an invitation still being opened is withdrawn once it arrives.
    fn close(&mut self, cx: &mut Context<Self>) {
        self.withdraw_open(cx);
        self.generation = self.generation.wrapping_add(1);
        self.stage = Stage::Closed;
        self.copied = false;
        self._copied = None;
        cx.notify();
    }
}

/// Paints `code` as sharp dark modules on a light square with its quiet zone.
fn qr_code(code: Rc<QrCode>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let modules = code.size() + QR_QUIET_ZONE * 2;
            let scale = window.scale_factor();
            let extent = f32::from(bounds.size.width).min(f32::from(bounds.size.height));
            // Whole device pixels per module keep every edge sharp, which readers need.
            let module_pixels = ((extent * scale) / modules as f32).floor().max(1.0);
            let module = px(module_pixels / scale);
            let side = module * modules as f32;
            let inset = ((extent - f32::from(side)) / 2.0 * scale).floor() / scale;
            let origin = bounds.origin + point(px(inset), px(inset));
            window.paint_quad(fill(Bounds::new(origin, size(side, side)), QR_LIGHT));
            for y in 0..code.size() {
                let mut x = 0;
                while x < code.size() {
                    if !code.get_module(x, y) {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < code.size() && code.get_module(x, y) {
                        x += 1;
                    }
                    let at = origin
                        + point(
                            module * (start + QR_QUIET_ZONE) as f32,
                            module * (y + QR_QUIET_ZONE) as f32,
                        );
                    let run = size(module * (x - start) as f32, module);
                    window.paint_quad(fill(Bounds::new(at, run), QR_DARK));
                }
            }
        },
    )
    .size(px(QR_EXTENT))
}

impl Render for InviteDevice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::appearance::shared_chrome(cx);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let readable = |id: &'static str, text: SharedString| {
            gpui::Text::new(ElementId::from(SharedString::new_static(id)), text)
        };
        let secondary = |id: &'static str, text: SharedString| {
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(colors.text_secondary))
                .whitespace_normal()
                .text_center()
                .child(readable(id, text))
        };
        let emphasized = |id: &'static str, text: SharedString| {
            div()
                .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                .text_color(gpui_color(colors.text))
                .whitespace_normal()
                .text_center()
                .child(readable(id, text))
        };
        // A status names what changed once, so assistive technology announces it.
        let status = |content: AnyElement| {
            div()
                .id(STATUS_SELECTOR)
                .debug_selector(|| STATUS_SELECTOR.to_owned())
                .role(accesskit::Role::Status)
                .flex()
                .flex_col()
                .items_center()
                .gap(appearance.spacing(4.0))
                .child(content)
        };
        let progress = |id: &'static str, label: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(appearance.spacing(6.0))
                .child(
                    ProgressRing::new(id, label.clone(), ProgressState::Indeterminate)
                        .size(ProgressSize::Compact),
                )
                .child(secondary("invite-device-progress-label", label))
        };
        let qr_tile = |content: AnyElement| {
            div()
                .size(px(QR_EXTENT))
                .flex()
                .items_center()
                .justify_center()
                .rounded(RadiusRole::Control.pixels())
                .overflow_hidden()
                .child(content)
        };
        let content: AnyElement = match &self.stage {
            Stage::Opening => qr_tile(
                progress(
                    "invite-device-opening",
                    "Creating an invitation\u{2026}".into(),
                )
                .into_any_element(),
            )
            .into_any_element(),
            Stage::Waiting { invitation, code } => {
                let left = remaining(invitation.deadline, Instant::now());
                let owner = cx.weak_entity();
                let copy = Button::new(COPY_SELECTOR, if self.copied { "Copied" } else { "Copy" })
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .tab_stop(true)
                    .debug_selector(COPY_SELECTOR)
                    .accessibility_description("Copy invitation link")
                    .on_activate(move |_, _, cx| {
                        let _ = owner.update(cx, |body, cx| body.copy(cx));
                    });
                let qr = match code {
                    Some(code) => div()
                        .id(QR_SELECTOR)
                        .debug_selector(|| QR_SELECTOR.to_owned())
                        // The code is named and never described, so assistive technology keeps no
                        // copy of the link it carries.
                        .role(accesskit::Role::Image)
                        .aria_label("Invitation link QR code")
                        .child(qr_tile(qr_code(Rc::clone(code)).into_any_element()))
                        .into_any_element(),
                    None => secondary(
                        "invite-device-no-qr",
                        "Link too long for a QR code. Copy it.".into(),
                    )
                    .into_any_element(),
                };
                let link = div()
                    .id(LINK_SELECTOR)
                    .debug_selector(|| LINK_SELECTOR.to_owned())
                    // The link is shown to read and Copy, and never published as accessible text,
                    // so assistive technology and its recordings keep no copy of it.
                    .role(accesskit::Role::Group)
                    .aria_label("Invitation link")
                    .flex_1()
                    .min_w_0()
                    .px(appearance.spacing(8.0))
                    .py(appearance.spacing(4.0))
                    .rounded(RadiusRole::Control.pixels())
                    .border_1()
                    .border_color(gpui_color(colors.input_border))
                    .bg(gpui_color(colors.input_background))
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(colors.input_text))
                    .truncate()
                    .child(SharedString::from(invitation.text.to_string()));
                // The sheet's width bounds the column and its link row. A centered column
                // otherwise takes its content's width, and the link has no place to break, so it
                // would push Copy out of the sheet.
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(appearance.spacing(12.0))
                    .child(qr)
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(appearance.spacing(8.0))
                            .child(link)
                            // Copy keeps its own width; the link gives up what the row lacks.
                            .child(div().flex_none().child(copy)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(appearance.spacing(6.0))
                            .child(
                                ProgressRing::new(
                                    "invite-device-waiting",
                                    "Waiting for a device",
                                    ProgressState::Indeterminate,
                                )
                                .size(ProgressSize::Compact),
                            )
                            .child(secondary(
                                "invite-device-countdown",
                                format!("Expires in {}", countdown(left)).into(),
                            )),
                    )
                    .into_any_element()
            }
            Stage::Asking { claim, .. } => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(appearance.spacing(8.0))
                .child(secondary(
                    "invite-device-compare",
                    format!(
                        "Allow only if \u{201C}{}\u{201D} shows this code.",
                        claim.name
                    )
                    .into(),
                ))
                .child(
                    div()
                        .id(CODE_SELECTOR)
                        .debug_selector(|| CODE_SELECTOR.to_owned())
                        .role(accesskit::Role::Label)
                        .aria_label("Code")
                        .aria_value(code_speech(claim.code))
                        .chrome_text(appearance.typography.style(TextRole::Title))
                        .text_color(gpui_color(colors.text))
                        .child(code_text(claim.code)),
                )
                .into_any_element(),
            Stage::Allowing(name) => progress(
                "invite-device-allowing",
                format!("Allowing \u{201C}{name}\u{201D}\u{2026}").into(),
            )
            .into_any_element(),
            Stage::Allowed(device) => status(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(appearance.spacing(6.0))
                    .child(Icon::new(
                        IconName::Check,
                        px(28.0),
                        gpui_color(colors.success),
                    ))
                    .child(emphasized(
                        "invite-device-allowed",
                        format!("\"{}\" can connect to \"{}\".", device.name, self.server).into(),
                    ))
                    .into_any_element(),
            )
            .into_any_element(),
            Stage::Declined(name) => status(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(appearance.spacing(4.0))
                    .child(emphasized(
                        "invite-device-declined",
                        format!("\u{201C}{name}\u{201D} wasn\u{2019}t allowed.").into(),
                    ))
                    .child(secondary(
                        "invite-device-declined-detail",
                        "Create a new invitation.".into(),
                    ))
                    .into_any_element(),
            )
            .into_any_element(),
            Stage::Expired => status(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(appearance.spacing(4.0))
                    .child(emphasized("invite-device-expired", "Expired".into()))
                    .child(secondary(
                        "invite-device-expired-detail",
                        "Invitation unavailable.".into(),
                    ))
                    .into_any_element(),
            )
            .into_any_element(),
            Stage::Failed(failure) => status(
                div()
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(colors.error))
                    .whitespace_normal()
                    .text_center()
                    .child(readable(
                        "invite-device-failed",
                        failure.explanation(&self.server).into(),
                    ))
                    .into_any_element(),
            )
            .into_any_element(),
            Stage::Closed => div().into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .w_full()
            .py(appearance.spacing(4.0))
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gpui::{KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext};
    use spaceterm_ui::ModalLayer;
    use spaceterm_ui::a11y_testing::A11yTree;
    use zeroize::Zeroizing;

    use super::*;
    use crate::server_connection::direct::{NetworkPolicy, PairingClaim};
    use crate::ui::direct_access::{AccessState, unix_now};

    const SERVER: &str = "studio";
    const LINK: &str = "spaceterm://pair/test-invitation";

    /// A server's invitation operations as the test scripts them, recording what Invite Device
    /// asked of it.
    struct ScriptedServer {
        /// The server's clock when the test began, in Unix seconds. It doesn't move, so every
        /// invitation with one lifetime expires in the same second, as on a server issuing them
        /// within one second.
        issued_at: u64,
        /// How long each invitation the server opens lasts, in seconds.
        lifetime: Mutex<u64>,
        /// The link each invitation the server opens carries.
        link: Mutex<String>,
        invitations: Mutex<Vec<(ClaimId, u64)>>,
        claims: Mutex<Vec<Claim>>,
        approved: Mutex<Vec<ClaimId>>,
        rejected: Mutex<Vec<ClaimId>>,
        withdrawn: Mutex<Vec<ClaimId>>,
    }

    impl ScriptedServer {
        /// A server whose invitations last `lifetime` seconds from now.
        fn new(lifetime: u64) -> Arc<Self> {
            Arc::new(Self {
                issued_at: unix_now(),
                lifetime: Mutex::new(lifetime),
                link: Mutex::new(LINK.to_owned()),
                invitations: Mutex::default(),
                claims: Mutex::default(),
                approved: Mutex::default(),
                rejected: Mutex::default(),
                withdrawn: Mutex::default(),
            })
        }

        fn invitation(&self) -> (ClaimId, u64) {
            *self
                .invitations
                .lock()
                .expect("invitations")
                .last()
                .expect("an invitation")
        }

        /// Opens an invitation the way another client's Invite Device does.
        fn invite_elsewhere(&self) -> ClaimId {
            self.invite().expect("an invitation").id
        }

        fn expiry(&self, invitation: ClaimId) -> u64 {
            self.invitations
                .lock()
                .expect("invitations")
                .iter()
                .find(|(id, _)| *id == invitation)
                .map(|(_, expires_at)| *expires_at)
                .expect("an invitation the server opened")
        }

        /// A device named `name` claims `invitation`, with an exchange whose hash starts with
        /// `exchange`.
        fn claim(&self, id: u8, name: &str, exchange: [u8; 4], invitation: ClaimId) {
            let mut exchange_hash = [0; 32];
            exchange_hash[..4].copy_from_slice(&exchange);
            let claim = PairingClaim {
                id: [id; 32],
                invitation_id: invitation,
                server: [1; 32],
                device_public_key: [id; 32],
                device_name: name.into(),
                expires_at_unix_seconds: self.expiry(invitation),
                exchange_hash,
            };
            self.claims.lock().expect("claims").push(Claim {
                id: claim.id,
                name: name.to_owned().into(),
                code: claim.comparison_code(),
                claim,
            });
        }
    }

    impl DirectAccessControl for ScriptedServer {
        fn status(&self) -> Result<AccessState, AccessFailure> {
            Err(AccessFailure::Unreachable)
        }

        fn set_policy(
            &self,
            _: Option<NetworkPolicy>,
            _: bool,
        ) -> Result<AccessState, AccessFailure> {
            Err(AccessFailure::Refused)
        }

        fn invite(&self) -> Result<OpenInvitation, AccessFailure> {
            let mut invitations = self.invitations.lock().expect("invitations");
            let id = [u8::try_from(invitations.len() + 1).expect("few invitations"); 32];
            let lifetime = *self.lifetime.lock().expect("lifetime");
            invitations.push((id, self.issued_at + lifetime));
            Ok(OpenInvitation {
                id,
                text: Zeroizing::new(self.link.lock().expect("link").clone()),
                deadline: Instant::now() + std::time::Duration::from_secs(lifetime),
            })
        }

        fn withdraw(&self, invitation: ClaimId) -> Result<(), AccessFailure> {
            self.withdrawn.lock().expect("withdrawn").push(invitation);
            Ok(())
        }

        fn claims(&self) -> Result<Vec<Claim>, AccessFailure> {
            Ok(self.claims.lock().expect("claims").clone())
        }

        fn approve(&self, claim: &PairingClaim) -> Result<Device, AccessFailure> {
            self.approved.lock().expect("approved").push(claim.id);
            Ok(Device {
                pairing: [9; 32],
                name: claim.device_name.as_str().to_owned().into(),
                fingerprint: SharedString::default(),
                last_connected_at: None,
            })
        }

        fn reject(&self, claim: ClaimId) -> Result<(), AccessFailure> {
            self.rejected.lock().expect("rejected").push(claim);
            Ok(())
        }

        fn devices(&self) -> Result<Vec<Device>, AccessFailure> {
            Ok(Vec::new())
        }
    }

    /// The view that presents Invite Device, counting the devices it hears were paired.
    struct Host {
        paired: usize,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    fn invite<'a>(
        server: &Arc<ScriptedServer>,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Host>, &'a mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let (host, cx) = cx.add_window_view(|_, _| Host { paired: 0 });
        let control: Arc<dyn DirectAccessControl> = Arc::clone(server) as _;
        cx.update(|window, cx| {
            window.activate_window();
            host.update(cx, |_, cx| {
                assert!(InviteDevice::present(
                    control,
                    SERVER.into(),
                    |host: &mut Host, _| host.paired += 1,
                    window,
                    cx,
                ));
            });
        });
        cx.run_until_parked();
        (host, cx)
    }

    /// Lets the invitation look for claims once more.
    fn poll(cx: &mut VisualTestContext) {
        cx.executor().advance_clock(POLL);
        cx.run_until_parked();
    }

    fn click(selector: &str, cx: &mut VisualTestContext) {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        let position = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"))
            .center();
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();
    }

    fn shown(selector: &str, cx: &mut VisualTestContext) -> bool {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        cx.debug_bounds(selector).is_some()
    }

    /// Presses Return the way a keyboard does. A focused button acts when the key comes back
    /// up, so a key-down alone leaves the focused action unpressed.
    fn press_return(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("enter");
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").expect("Return parses"),
        });
        cx.run_until_parked();
    }

    /// The selector the modal gives an action.
    fn action(selector: &str) -> String {
        format!("modal-action-{selector}")
    }

    /// Whether any Label the window publishes contains `text`.
    fn says(text: &str, cx: &mut VisualTestContext) -> bool {
        A11yTree::read(cx).with_role("Label").iter().any(|node| {
            node["aria"]["value"]
                .as_str()
                .is_some_and(|value| value.contains(text))
        })
    }

    #[gpui::test]
    fn copy_puts_the_exact_link_on_the_clipboard_only_when_the_person_asks(
        cx: &mut TestAppContext,
    ) {
        let server = ScriptedServer::new(300);
        let (_, cx) = invite(&server, cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            None,
            "showing an invitation never overwrites the clipboard"
        );

        click(COPY_SELECTOR, cx);

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(LINK.to_owned())
        );
    }

    #[gpui::test]
    fn a_real_length_link_leaves_copy_in_view_and_usable(cx: &mut TestAppContext) {
        // A real invitation is about a thousand characters of unbroken base64url.
        let link = format!(
            "spaceterm://pair/{}",
            "eyJzY2hlbWEiOjEsImlkIjpbMTIsMzQsNTYsNzgsOTAsMTIs".repeat(20)
        );
        assert!(link.len() > 950);
        let server = ScriptedServer::new(300);
        *server.link.lock().expect("link") = link.clone();
        let (_, cx) = invite(&server, cx);

        let viewport = cx
            .debug_bounds("modal-body-viewport")
            .expect("Invite Device shows its body");
        let field = cx.debug_bounds(LINK_SELECTOR).expect("the link shows");
        let copy = cx.debug_bounds(COPY_SELECTOR).expect("Copy shows");
        assert!(copy.size.width > px(0.0), "Copy has room: {copy:?}");
        assert!(
            viewport.left() <= field.left() && field.right() <= copy.left(),
            "the link ends before Copy: {field:?}, {copy:?}"
        );
        assert!(
            copy.right() <= viewport.right(),
            "Copy is inside the sheet: {copy:?} within {viewport:?}"
        );

        click(COPY_SELECTOR, cx);

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(link),
            "Copy takes the whole link, not what the field has room to show"
        );
    }

    #[gpui::test]
    fn the_qr_code_is_named_for_assistive_technology_without_its_link(cx: &mut TestAppContext) {
        let server = ScriptedServer::new(300);
        let (_, cx) = invite(&server, cx);

        let tree = A11yTree::read(cx);

        assert!(tree.find("Invitation link QR code").is_some());
        assert!(!says(LINK, cx), "no published text carries the link");
    }

    #[gpui::test]
    fn a_device_using_the_invitation_is_asked_about_with_its_comparison_code(
        cx: &mut TestAppContext,
    ) {
        let server = ScriptedServer::new(300);
        let (_, cx) = invite(&server, cx);

        let (invitation, _) = server.invitation();
        // An exchange hash starting 00 00 30 39 derives 12345.
        server.claim(3, "MacBook", [0, 0, 0x30, 0x39], invitation);
        poll(cx);

        let tree = A11yTree::read(cx);
        let code = tree.node("Code");
        assert_eq!(code["aria"]["value"], "0 1 2 3 4 5");
    }

    #[gpui::test]
    fn return_allows_exactly_the_device_shown_and_its_presenter_hears_of_it(
        cx: &mut TestAppContext,
    ) {
        let server = ScriptedServer::new(300);
        let (host, cx) = invite(&server, cx);
        let (ours, _) = server.invitation();
        let theirs = server.invite_elsewhere();
        server.claim(4, "Elsewhere", [0; 4], theirs);
        server.claim(3, "MacBook", [0; 4], ours);
        poll(cx);

        press_return(cx);

        assert_eq!(*server.approved.lock().expect("approved"), vec![[3; 32]]);
        assert_eq!(host.read_with(cx, |host, _| host.paired), 1);
        assert!(says("\"MacBook\" can connect to \"studio\".", cx));
    }

    #[gpui::test]
    fn escape_refuses_the_device_and_ends_the_used_invitation(cx: &mut TestAppContext) {
        let server = ScriptedServer::new(300);
        let (_, cx) = invite(&server, cx);
        let (invitation, _) = server.invitation();
        server.claim(3, "MacBook", [0; 4], invitation);
        poll(cx);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(server.approved.lock().expect("approved").is_empty());
        assert_eq!(*server.rejected.lock().expect("rejected"), vec![[3; 32]]);
        assert_eq!(
            *server.withdrawn.lock().expect("withdrawn"),
            vec![invitation]
        );
        assert!(
            shown(&action(RENEW_SELECTOR), cx),
            "New Invitation is offered"
        );
    }

    #[gpui::test]
    fn a_claim_on_another_invitation_with_the_same_expiry_asks_nothing(cx: &mut TestAppContext) {
        let server = ScriptedServer::new(300);
        let (_, cx) = invite(&server, cx);
        let (ours, _) = server.invitation();
        let theirs = server.invite_elsewhere();
        assert_eq!(server.expiry(ours), server.expiry(theirs));

        server.claim(4, "Elsewhere", [0; 4], theirs);
        poll(cx);

        assert!(!shown(&action(ALLOW_SELECTOR), cx));
        assert!(shown(COPY_SELECTOR, cx), "the invitation stays open");
    }

    #[gpui::test]
    fn done_withdraws_the_invitation_so_nobody_can_use_it(cx: &mut TestAppContext) {
        let server = ScriptedServer::new(300);
        let (_, cx) = invite(&server, cx);
        let (invitation, _) = server.invitation();

        click(&action(DONE_SELECTOR), cx);

        assert_eq!(
            *server.withdrawn.lock().expect("withdrawn"),
            vec![invitation]
        );
        assert!(!shown(COPY_SELECTOR, cx));
    }

    #[gpui::test]
    fn closing_while_a_device_waits_for_an_answer_refuses_it(cx: &mut TestAppContext) {
        let server = ScriptedServer::new(300);
        let (_, cx) = invite(&server, cx);
        let (invitation, _) = server.invitation();
        server.claim(3, "MacBook", [0; 4], invitation);
        poll(cx);

        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();

        assert!(server.approved.lock().expect("approved").is_empty());
        assert_eq!(
            *server.withdrawn.lock().expect("withdrawn"),
            vec![invitation]
        );
    }

    #[gpui::test]
    fn an_expired_invitation_says_so_and_return_opens_a_new_one(cx: &mut TestAppContext) {
        let server = ScriptedServer::new(0);
        let (_, cx) = invite(&server, cx);
        let (expired, _) = server.invitation();
        assert!(says("Expired", cx));
        assert_eq!(*server.withdrawn.lock().expect("withdrawn"), vec![expired]);

        *server.lifetime.lock().expect("lifetime") = 300;
        press_return(cx);

        assert_eq!(server.invitations.lock().expect("invitations").len(), 2);
        assert!(
            shown(COPY_SELECTOR, cx),
            "the new invitation is ready to copy"
        );
    }
}
