//! Performs what an update surface's click asks for: the desktop's own update, a Machine action,
//! the agreed confirmation before closing sessions, or an alert with the full sentence. Normal
//! update actions never open Settings.

use gpui::{AnyWindowHandle, App, Context, SharedString, WeakEntity, Window};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::{UpdateControl, front_workspace_window, service, whats_new};
use crate::updates::UpdateState;
use crate::updates::machines::store::{MachineUpdates, MachineUpdatesNotice, Surface};
use crate::updates::machines::{ActionFailure, MachineAction};
use crate::updates::presentation::{
    self, Command, Confirmation, DesktopAction, Explanation, ItemKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Answer {
    Proceed,
    Cancel,
}

/// Where a command's alert appears.
#[derive(Clone)]
pub(crate) enum Presenter {
    /// The front Workspace Window, for a notification or a failure report.
    Front,
    /// The window of the title-bar control that ran the command, which owns the alert.
    Control(AnyWindowHandle, WeakEntity<UpdateControl>),
}

/// Performs `command` after the current event, presenting any alert through `presenter`.
pub(crate) fn perform(command: Command, presenter: Presenter, cx: &mut App) {
    cx.defer(move |cx| perform_now(command, presenter, cx));
}

fn perform_now(command: Command, presenter: Presenter, cx: &mut App) {
    match command {
        Command::Desktop(DesktopAction::Update) => update_desktop(cx),
        Command::Desktop(DesktopAction::WhatsNew) => whats_new::open_whats_new(cx),
        Command::Desktop(DesktopAction::DismissFailure) => {
            if let Some(updates) = service(cx) {
                updates.update(cx, |updates, cx| updates.dismiss_failure(cx));
            }
        }
        Command::Machine(machine, action) => {
            if let Some(store) = MachineUpdates::global(cx) {
                store.update(cx, |store, cx| {
                    store.perform(machine, action, Surface::Window, cx)
                });
            }
        }
        Command::Confirm(confirmation) => confirm(confirmation, presenter, cx),
        Command::Explain(explanation) => explain(explanation, presenter, cx),
    }
}

/// Relaunches into a ready release without asking, or downloads it first.
fn update_desktop(cx: &mut App) {
    let Some(updates) = service(cx) else { return };
    let state = updates.read(cx).state().clone();
    updates.update(cx, |updates, cx| match state {
        UpdateState::Ready { .. } | UpdateState::Installing { .. } => updates.relaunch(cx),
        UpdateState::Available { .. } => updates.download(cx),
        UpdateState::Downloading { .. } | UpdateState::Verifying { .. } | UpdateState::Checking => {
        }
        UpdateState::Idle
        | UpdateState::UpToDate
        | UpdateState::Failed { .. }
        | UpdateState::Unavailable => updates.retry_download(cx),
    });
}

/// Presents `alert` through `presenter`, and passes the answer to `answered`.
fn present_in(
    presenter: Presenter,
    alert: Alert<Answer>,
    answered: impl FnOnce(Answer, &mut App) + 'static,
    cx: &mut App,
) {
    match presenter {
        Presenter::Control(window, control) => {
            let _ = window.update(cx, |_, window, cx| {
                let _ = control.update(cx, |_, cx| present_alert(alert, window, cx, answered));
            });
        }
        Presenter::Front => {
            if let Some(handle) = front_workspace_window(cx) {
                let _ = handle.update(cx, |_, window, cx| {
                    present_alert(alert, window, cx, answered);
                });
            }
        }
    }
}

fn button(
    answer: Answer,
    label: impl Into<SharedString>,
    role: ModalActionRole,
    selector: &'static str,
) -> ModalAction<Answer> {
    ModalAction::new(answer, label, role, selector)
}

fn present_alert<T: 'static>(
    alert: Alert<Answer>,
    window: &Window,
    cx: &mut Context<T>,
    answered: impl FnOnce(Answer, &mut App) + 'static,
) {
    let result = alert.present(window, cx, move |outcome, cx| {
        answered(
            match outcome {
                AlertOutcome::Activated { action_id, .. } => action_id,
                AlertOutcome::Dismissed { .. } => Answer::Cancel,
            },
            cx,
        );
    });
    if result.is_err() {
        eprintln!("failed to present an update alert");
    }
}

/// The destructive button performs every action. Cancel and Escape change nothing.
pub(super) fn confirmation_alert(confirmation: &Confirmation) -> Alert<Answer> {
    Alert::new(
        ModalId::new("update-confirm"),
        confirmation.title.clone(),
        confirmation.title.clone(),
        confirmation.message.clone(),
        vec![
            button(
                Answer::Proceed,
                confirmation.button.clone(),
                ModalActionRole::Affirmative,
                "update-confirm-proceed",
            )
            .with_intent(ModalActionIntent::Destructive)
            .with_emphasis(ModalActionEmphasis::Prominent),
            button(
                Answer::Cancel,
                "Cancel",
                ModalActionRole::Cancel,
                "update-confirm-cancel",
            ),
        ],
    )
    .intent(AlertIntent::Critical)
}

/// Asks first, then performs the confirmation's actions only while its approval holds. When the
/// sessions, the running Core or the Pending Version changed while the alert was open, the
/// approval covers other work, so the control asks again with what is true now.
fn confirm(confirmation: Confirmation, presenter: Presenter, cx: &mut App) {
    let alert = confirmation_alert(&confirmation);
    let again = presenter.clone();
    present_in(
        presenter,
        alert,
        move |answer, cx| {
            if answer != Answer::Proceed {
                return;
            }
            let Some(store) = MachineUpdates::global(cx) else {
                return;
            };
            let approved = store.update(cx, |store, cx| {
                store.perform_approved(&confirmation, Surface::Window, cx)
            });
            if approved.is_err()
                && let Some(command) = presentation::current(cx).ask_again(&confirmation)
            {
                perform(command, again, cx);
            }
        },
        cx,
    );
}

pub(super) fn explanation_alert(explanation: &Explanation) -> Alert<Answer> {
    Alert::new(
        ModalId::new("update-explain"),
        explanation.title.clone(),
        explanation.title.clone(),
        explanation.message.clone(),
        vec![
            button(
                Answer::Proceed,
                "OK",
                ModalActionRole::Affirmative,
                "update-explain-ok",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
        ],
    )
    .intent(AlertIntent::Informational)
}

fn explain(explanation: Explanation, presenter: Presenter, cx: &mut App) {
    let alert = explanation_alert(&explanation);
    let then = explanation.then;
    let next = presenter.clone();
    present_in(
        presenter,
        alert,
        move |answer, cx| {
            if answer == Answer::Proceed
                && let Some(then) = then
            {
                perform(*then, next, cx);
            }
        },
        cx,
    );
}

/// Explains each Machine action a window performed that did not succeed, in the front Workspace
/// Window. A confirmation that went stale asks again with the current facts.
pub(super) fn receive_failure(notice: &MachineUpdatesNotice, cx: &mut App) {
    let (Surface::Window, Err(failure)) = (notice.surface, notice.outcome) else {
        return;
    };
    // The Core's preview found other sessions than the person saw: ask again.
    if failure == ActionFailure::Changed {
        let presentation = presentation::current(cx);
        let command = presentation
            .machine(notice.machine)
            .and_then(|view| match notice.action {
                MachineAction::Restart => Some(view.restart_command()),
                MachineAction::Stop => Some(view.stop_command()),
                _ => None,
            });
        if let Some(command) = command {
            perform(command, Presenter::Front, cx);
        }
        return;
    }
    let Some((title, message)) =
        presentation::action_failure(&notice.name, notice.local, &notice.action, failure)
    else {
        return;
    };
    let explanation = Explanation {
        title,
        message,
        then: None,
    };
    perform(Command::Explain(explanation), Presenter::Front, cx);
}

/// The person clicked the overdue notification about `key`: bring a Workspace Window forward and
/// do what the item's control does now, which asks first wherever the control asks.
pub(super) fn reminder_clicked(key: ItemKey, cx: &mut App) {
    crate::app::activate_default_window(cx, None);
    cx.activate(true);
    let command = presentation::current(cx)
        .items()
        .into_iter()
        .find(|item| item.key == key)
        .and_then(|item| item.command);
    if let Some(command) = command {
        perform(command, Presenter::Front, cx);
    }
}
