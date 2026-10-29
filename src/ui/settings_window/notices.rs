//! Notices at the top of Servers: work SpaceTerm is still finishing that no server row shows.
//!
//! An unpair a server hasn't confirmed (ADR 0038) and a record SpaceTerm couldn't save each get
//! one notice. Dismissing a
//! notice only hides it. It never cancels the retries behind it. A notice whose work waits for the
//! person is hidden in this window only, so it returns the next time Settings opens.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{AnyElement, Context, ElementId, SharedString, accesskit, div, px};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, Icon, IconButton, IconName};

use super::SettingsWindow;
use crate::direct_connections::{PendingUnpair, UnpairProgress};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_servers::UnpairAction;

pub(super) const NOTICE_SELECTOR: &str = "settings-server-notice";

/// The notices the person hid in this window. Hiding one changes nothing it reports.
#[derive(Default)]
pub(super) struct HiddenNotices(HashSet<String>);

/// A step one notice offers.
#[derive(Clone, Debug)]
enum NoticeStep {
    Unpair(PendingUnpair, UnpairAction),
}

/// How a notice is hidden.
#[derive(Clone, Debug)]
enum Dismissal {
    /// The registry keeps the request's notice hidden across launches and keeps retrying.
    Unpair(PendingUnpair),
    /// Hidden in this window only.
    Window(String),
}

struct Notice {
    id: String,
    message: SharedString,
    steps: Vec<(&'static str, NoticeStep)>,
    dismissal: Option<Dismissal>,
}

/// What one unconfirmed unpair says, in one or two short sentences naming the server.
pub(super) fn unpair_message(pending: &PendingUnpair) -> String {
    let server = &pending.server_label;
    match (
        pending.enrollment_cleanup,
        pending.server_confirmed,
        pending.state,
    ) {
        (true, _, state) => {
            let next = match state {
                UnpairProgress::NeedsInteraction => "Choose Resume to continue cancelling.",
                UnpairProgress::Retrying => "It will retry automatically.",
                UnpairProgress::Unconfirmed => "Choose Try Again to continue cancelling.",
            };
            format!("SpaceTerm is cancelling the unfinished pairing with {server}. {next}")
        }
        (false, true, UnpairProgress::NeedsInteraction) => {
            format!("Choose Resume to finish unpairing from {server}.")
        }
        (false, true, UnpairProgress::Retrying) => format!("Finishing unpairing from {server}…"),
        (false, true, UnpairProgress::Unconfirmed) => {
            format!("Choose Try Again to finish unpairing from {server}.")
        }
        (false, false, UnpairProgress::NeedsInteraction) => {
            format!("Unpair from {server} isn\u{2019}t confirmed. Choose Resume.")
        }
        (false, false, UnpairProgress::Retrying) => {
            format!("Unpair from {server} isn\u{2019}t confirmed yet.")
        }
        (false, false, UnpairProgress::Unconfirmed) => {
            format!("Unpair from {server} isn\u{2019}t confirmed. Choose Try Again.")
        }
    }
}

impl SettingsWindow {
    fn notices(&self) -> Vec<Notice> {
        let hidden = &self.hidden_notices.0;
        let mut notices = Vec::new();
        for pending in self.visible_pending_unpairs() {
            let id = format!("unpair-{}", hex(&pending.key));
            if hidden.contains(&id) {
                continue;
            }
            let (_, actions) = crate::ui::direct_servers::present_unpair(&pending);
            let steps = actions
                .iter()
                .filter(|action| **action == UnpairAction::Resume)
                .map(|action| {
                    let label = match pending.state {
                        UnpairProgress::Unconfirmed => "Try Again",
                        _ => action.label(),
                    };
                    (label, NoticeStep::Unpair(pending.clone(), *action))
                })
                .collect();
            // A request SpaceTerm finishes quietly stays hidden across launches. One that waits
            // for the person is hidden here only, so Resume is offered again next time.
            let dismissal = Some(if actions.contains(&UnpairAction::Dismiss) {
                Dismissal::Unpair(pending.clone())
            } else {
                Dismissal::Window(id.clone())
            });
            notices.push(Notice {
                id,
                message: unpair_message(&pending).into(),
                steps,
                dismissal,
            });
        }
        if self
            .direct_routes
            .as_ref()
            .is_some_and(|rows| rows.connections.persistence_error())
        {
            let id = "direct-record".to_owned();
            if !hidden.contains(&id) {
                notices.push(Notice {
                    message: SharedString::new_static(super::direct_routes::UNSAVED_CHANGES),
                    steps: Vec::new(),
                    dismissal: Some(Dismissal::Window(id.clone())),
                    id,
                });
            }
        }
        notices
    }

    fn take_notice_step(&mut self, step: NoticeStep, cx: &mut Context<Self>) {
        match step {
            NoticeStep::Unpair(pending, action) => self.run_unpair_action(&pending, action, cx),
        }
    }

    fn dismiss_notice(&mut self, dismissal: Dismissal, cx: &mut Context<Self>) {
        match dismissal {
            Dismissal::Unpair(pending) => {
                self.run_unpair_action(&pending, UnpairAction::Dismiss, cx)
            }
            Dismissal::Window(id) => {
                self.hidden_notices.0.insert(id);
                cx.notify();
            }
        }
    }

    /// The notices above Servers' cards, or `None` when nothing needs telling.
    pub(super) fn render_server_notices(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notices = self.notices();
        if notices.is_empty() {
            return None;
        }
        let pair = appearance.semantic_text_pairs.warning_status;
        let icon_size = appearance.icons.metrics(IconRole::Status).glyph_size;
        let control_icon = appearance.icons.metrics(IconRole::Control).glyph_size;
        let rendered = notices.into_iter().map(|notice| {
            let selector = format!("{NOTICE_SELECTOR}-{}", notice.id);
            let steps = notice
                .steps
                .into_iter()
                .map(|(label, step)| {
                    let owner = cx.weak_entity();
                    Button::new(
                        ElementId::from(SharedString::from(format!("{selector}-{label}"))),
                        label,
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .tab_stop(true)
                    .debug_selector(format!(
                        "{selector}-{}",
                        label
                            .trim_end_matches('\u{2026}')
                            .to_lowercase()
                            .replace(' ', "-")
                    ))
                    .on_activate(move |_, _, cx| {
                        let step = step.clone();
                        let _ =
                            owner.update(cx, |settings, cx| settings.take_notice_step(step, cx));
                    })
                    .into_any_element()
                })
                // The steps name their selectors after the notice's, which the notice then keeps.
                .collect::<Vec<_>>();
            let dismiss = notice.dismissal.map(|dismissal| {
                let owner = cx.weak_entity();
                IconButton::new(
                    SharedString::from(format!("{selector}-dismiss")),
                    "Dismiss",
                    move |foreground| {
                        Icon::new(IconName::X, control_icon, foreground).into_any_element()
                    },
                )
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tab_stop(true)
                .debug_selector(format!("{selector}-dismiss"))
                .on_activate(move |_, _, cx| {
                    let dismissal = dismissal.clone();
                    let _ = owner.update(cx, |settings, cx| settings.dismiss_notice(dismissal, cx));
                })
            });
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .role(accesskit::Role::Status)
                .aria_label(notice.message.clone())
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .p(appearance.spacing(10.0))
                .rounded(RadiusRole::Card.pixels())
                .bg(gpui_color(pair.background))
                .border(px(HAIRLINE))
                .border_color(gpui_color(appearance.colors.warning_border))
                .child(div().flex_none().child(Icon::new(
                    IconName::Info,
                    icon_size,
                    gpui_color(pair.primary),
                )))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .chrome_text(appearance.typography.style(TextRole::Body))
                        .text_color(gpui_color(pair.primary))
                        .whitespace_normal()
                        .child(notice.message),
                )
                .children(steps)
                .children(dismiss)
                .into_any_element()
        });
        Some(
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap(appearance.spacing(8.0))
                .children(rendered)
                .into_any_element(),
        )
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(state: UnpairProgress) -> PendingUnpair {
        PendingUnpair {
            key: [1; 32],
            server_label: "Studio".to_owned(),
            state,
            dismissed: false,
            enrollment_cleanup: false,
            self_unpair: true,
            server_confirmed: false,
            abandoned: false,
        }
    }

    #[test]
    fn an_unconfirmed_unpair_names_the_server_and_keeps_trying() {
        assert_eq!(
            unpair_message(&pending(UnpairProgress::Retrying)),
            "Unpair from Studio isn\u{2019}t confirmed yet."
        );
    }

    #[test]
    fn a_confirmed_unpair_reports_only_the_key_cleanup_left() {
        let mut confirmed = pending(UnpairProgress::Retrying);
        confirmed.server_confirmed = true;

        let message = unpair_message(&confirmed);

        assert_eq!(message, "Finishing unpairing from Studio…");
        assert!(!message.contains("isn\u{2019}t confirmed"), "{message}");
    }
}
