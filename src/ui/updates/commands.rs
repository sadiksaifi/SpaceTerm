//! Performs what an update surface's click asks for: the desktop's own update, a Machine action,
//! the agreed confirmation before closing sessions, or an alert with the full sentence. Normal
//! update actions never open Settings.

use gpui::{AnyWindowHandle, App, Context, SharedString, Window};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::{front_workspace_window, service, whats_new};
use crate::ui::WorkspaceManager;
use crate::updates::UpdateState;
use crate::updates::machines::store::{MachineUpdates, MachineUpdatesNotice};
use crate::updates::machines::{ActionFailure, MachineAction};
use crate::updates::presentation::{self, Command, Confirmation, DesktopAction, Explanation};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Answer {
    Proceed,
    Cancel,
}

/// Performs `command` after the current event, presenting any alert in `window` when it is a
/// Workspace Window, or else in the front one.
pub(crate) fn perform(command: Command, window: Option<AnyWindowHandle>, cx: &mut App) {
    cx.defer(move |cx| perform_now(command, window, cx));
}

fn perform_now(command: Command, window: Option<AnyWindowHandle>, cx: &mut App) {
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
                store.update(cx, |store, cx| store.perform(machine, action, cx));
            }
        }
        Command::Confirm(confirmation) => present_in(window, cx, move |window, cx| {
            confirm(confirmation, window, cx)
        }),
        Command::Explain(explanation) => present_in(window, cx, move |window, cx| {
            explain(explanation, window, cx)
        }),
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

fn present_in(
    window: Option<AnyWindowHandle>,
    cx: &mut App,
    present: impl FnOnce(&mut Window, &mut Context<WorkspaceManager>) + 'static,
) {
    let handle = window
        .and_then(|window| window.downcast::<WorkspaceManager>())
        .or_else(|| front_workspace_window(cx));
    if let Some(handle) = handle {
        let _ = handle.update(cx, |_, window, cx| present(window, cx));
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

pub(super) fn confirm<T: 'static>(
    confirmation: Confirmation,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let actions = confirmation.actions.clone();
    present_alert(
        confirmation_alert(&confirmation),
        window,
        cx,
        move |answer, cx| {
            if answer != Answer::Proceed {
                return;
            }
            let Some(store) = MachineUpdates::global(cx) else {
                return;
            };
            store.update(cx, |store, cx| {
                for (machine, action) in actions {
                    store.perform(machine, action, cx);
                }
            });
        },
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

fn explain<T: 'static>(explanation: Explanation, window: &mut Window, cx: &mut Context<T>) {
    let then = explanation.then.clone();
    let handle = window.window_handle();
    present_alert(
        explanation_alert(&explanation),
        window,
        cx,
        move |answer, cx| {
            if answer == Answer::Proceed
                && let Some(then) = then
            {
                perform(*then, Some(handle), cx);
            }
        },
    );
}

/// Explains each Machine action that did not succeed, in the front Workspace Window. A
/// confirmation that went stale asks again with the current facts.
pub(super) fn receive_failure(notice: &MachineUpdatesNotice, cx: &mut App) {
    if notice.failure == ActionFailure::Changed && notice.action == MachineAction::Restart {
        let presentation = presentation::current(cx);
        if let Some(command) = presentation
            .machine(notice.machine)
            .map(presentation::MachineView::restart_command)
        {
            perform(command, None, cx);
        }
        return;
    }
    let Some((title, message)) =
        presentation::action_failure(&notice.name, notice.local, &notice.action, notice.failure)
    else {
        return;
    };
    let explanation = Explanation {
        title,
        message,
        then: None,
    };
    perform(Command::Explain(explanation), None, cx);
}
