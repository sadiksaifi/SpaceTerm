//! Performs what an update surface's click asks for, one command per action on every surface:
//! the desktop's own update, a Machine action, the confirmation before a restart ends Terminal
//! Sessions, or the alert of a failure the person opened. Only a click that a Machine refuses at
//! once opens an alert unasked, because the person is waiting on that click. Normal update
//! actions never open Settings.

use std::rc::Rc;

use gpui::{AnyWindowHandle, App, Context, SharedString, WeakEntity, Window};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::{UpdateControl, front_workspace_window, service, whats_new};
use crate::updates::confirm::{Confirmation, Explanation};
use crate::updates::copy::dialogs;
use crate::updates::machines::store::{self, MachineUpdates, MachineUpdatesNotice, Surface};
use crate::updates::machines::{ActionFailure, MachineAction, MachineId};
use crate::updates::state::{self, Command, DesktopAction, Target};

const OK: &str = "OK";
const CANCEL: &str = "Cancel";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Answer {
    Proceed,
    Cancel,
}

/// Where a command's alert appears.
#[derive(Clone)]
pub(crate) enum Presenter {
    /// The front Workspace Window, for a notification or a refusal.
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
        // Relaunch to Update: SpaceTerm relaunches at once and never asks.
        Command::Desktop(DesktopAction::Update) => {
            if let Some(updates) = service(cx) {
                updates.update(cx, |updates, cx| updates.relaunch(cx));
            }
        }
        // Download: downloads only. Relaunching stays a separate step.
        Command::Desktop(DesktopAction::Download) => {
            if let Some(updates) = service(cx) {
                updates.update(cx, |updates, cx| updates.download_now(cx));
            }
        }
        Command::Desktop(DesktopAction::WhatsNew) => whats_new::open_whats_new(cx),
        Command::Desktop(DesktopAction::DismissFailure) => {
            if let Some(updates) = service(cx) {
                updates.update(cx, |updates, cx| updates.dismiss_failure(cx));
            }
        }
        Command::Machine(machine, action @ (MachineAction::Restart | MachineAction::Stop)) => {
            request(vec![(machine, action)], false, presenter, cx);
        }
        Command::Machine(machine, action) => {
            if let Some(store) = MachineUpdates::global(cx) {
                store.update(cx, |store, cx| {
                    store.perform(machine, action, Surface::Window, cx)
                });
            }
        }
        // The confirmation is built again from the Cores' previews before it shows.
        Command::Confirm(confirmation) => request(confirmation.actions, false, presenter, cx),
        Command::Explain(explanation) => explain(explanation, presenter, cx),
        // The title-bar pill opens its own list; no other surface offers this command.
        Command::ShowList => {}
    }
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

/// D1 and D2: the destructive button performs every action. Return never confirms it: Cancel
/// is the safe answer, and Cancel and Escape change nothing.
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
            .with_intent(ModalActionIntent::Destructive),
            button(
                Answer::Cancel,
                CANCEL,
                ModalActionRole::Cancel,
                "update-confirm-cancel",
            ),
        ],
    )
    .intent(AlertIntent::Critical)
}

/// Restarts or stops on the Cores' own previews, confirming through `presenter`. The store
/// records Cancel's deferral for the Ask stage itself.
fn request(
    actions: Vec<(MachineId, MachineAction)>,
    always: bool,
    presenter: Presenter,
    cx: &mut App,
) {
    let ask: store::Ask = Rc::new(move |confirmation: Confirmation, answered, cx| {
        let alert = confirmation_alert(&confirmation);
        present_in(
            presenter.clone(),
            alert,
            move |answer, cx| answered(answer == Answer::Proceed, cx),
            cx,
        );
    });
    store::request(actions, always, Surface::Window, ask, cx);
}

/// The label of the button that runs a failure's follow-up command, when the explanation names
/// none.
fn follow_up_label(explanation: &Explanation, then: &Command) -> SharedString {
    explanation.proceed.clone().unwrap_or_else(|| {
        match then {
            Command::Machine(_, MachineAction::CheckAgain) => dialogs::CHECK_AGAIN,
            _ => dialogs::TRY_AGAIN,
        }
        .into()
    })
}

/// D3 to D8: a failure's alert, with its follow-up and Cancel, or OK alone.
pub(super) fn explanation_alert(explanation: &Explanation) -> Alert<Answer> {
    let actions = match &explanation.then {
        Some(then) => vec![
            button(
                Answer::Proceed,
                follow_up_label(explanation, then),
                ModalActionRole::Affirmative,
                "update-explain-proceed",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
            button(
                Answer::Cancel,
                CANCEL,
                ModalActionRole::Cancel,
                "update-explain-cancel",
            ),
        ],
        None => vec![
            button(
                Answer::Proceed,
                OK,
                ModalActionRole::Affirmative,
                "update-explain-ok",
            )
            .with_emphasis(ModalActionEmphasis::Prominent)
            .default_action(true),
        ],
    };
    Alert::new(
        ModalId::new("update-explain"),
        explanation.title.clone(),
        explanation.title.clone(),
        explanation.message.clone(),
        actions,
    )
    .intent(AlertIntent::Warning)
}

/// The follow-up runs on its button. OK and Cancel dismiss the failure's title-bar step.
fn explain(explanation: Explanation, presenter: Presenter, cx: &mut App) {
    let alert = explanation_alert(&explanation);
    let Explanation { then, dismiss, .. } = explanation;
    let next = presenter.clone();
    present_in(
        presenter,
        alert,
        move |answer, cx| match (answer, then) {
            (Answer::Proceed, Some(then)) => perform(*then, next, cx),
            _ => {
                if let Some(dismiss) = dismiss {
                    perform(*dismiss, next, cx);
                }
            }
        },
        cx,
    );
}

/// D5: the alert for a restart a Machine refused at its preview, while the person waits on the
/// click. An action that already ran reports through the title bar's failure or not-confirmed
/// step instead, whatever its outcome.
pub(super) fn refusal(notice: &MachineUpdatesNotice) -> Option<Explanation> {
    if notice.action != MachineAction::Restart {
        return None;
    }
    let (title, message) = notice.unasked_alert()?;
    Some(Explanation::ok(title, message))
}

/// Answers a Machine action a window performed that did not succeed. A confirmation that went
/// stale asks again with the current facts.
pub(super) fn receive_failure(notice: &MachineUpdatesNotice, cx: &mut App) {
    let (Surface::Window, Err(failure)) = (notice.surface, notice.outcome) else {
        return;
    };
    // The Core starts nothing that is off in Login Items, so Start opens that list, where the
    // person turns it on. Nothing starts by itself: the window offers Start again.
    if failure == ActionFailure::LoginItemOff {
        crate::local_startup::open_login_items();
        return;
    }
    // The Core refused its preview: the sessions or the Pending Version changed since. The
    // window previews again and always asks, so nothing runs again by itself.
    if failure == ActionFailure::Changed {
        if matches!(notice.action, MachineAction::Restart | MachineAction::Stop) {
            request(
                vec![(notice.machine, notice.action.clone())],
                true,
                Presenter::Front,
                cx,
            );
        }
        return;
    }
    if let Some(explanation) = refusal(notice) {
        perform(Command::Explain(explanation), Presenter::Front, cx);
    }
}

/// The person clicked the reminder about `key`: bring a Workspace Window forward with the click's
/// activation `token`, and perform the step. SpaceTerm relaunches; a Core opens its confirmation.
pub(super) fn reminder_clicked(key: Target, token: Option<&str>, cx: &mut App) {
    crate::app::activate_default_window(cx, token);
    cx.activate(true);
    let view = state::current(cx);
    let row = match key {
        Target::App => Some(view.app_row()),
        Target::Machine(machine) => view.machine_row(machine),
    };
    if let Some(action) = row.and_then(|row| row.action) {
        perform(action.command, Presenter::Front, cx);
    }
}
