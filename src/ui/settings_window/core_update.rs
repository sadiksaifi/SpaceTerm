//! Settings reads installed Core versions here. All running Machine updates and restarts belong
//! to the shared Machine update store, including previews, confirmations and completion.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyWindowHandle, App, AppContext as _, Context, Task};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::{SettingsWindow, machine_list::MachineChoice};
use crate::local_startup::CandidateError;
use crate::machine_connection::CoreError;
use crate::machine_connection::service::activation::{ActivationStatus, is_released_downgrade};
use crate::machine_connection::service::{RetainedInstallation, Service, ServiceStatus};
use crate::updates::machines::store::{self, MachineUpdates, Surface};
use crate::updates::machines::{ActionFailure, MachineAction};
use crate::updates::presentation::{self, Command, Confirmation, MachineView};

pub(super) const UPDATE_SELECTOR: &str = "settings-server-update";
pub(super) const UPDATE_CONFIRM_SELECTOR: &str = "settings-server-update-confirm";
pub(super) const UPDATE_CANCEL_SELECTOR: &str = "settings-server-update-cancel";

/// The server build this SpaceTerm includes, as its signed manifest declares it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IncludedCore {
    pub(super) version: String,
    pub(super) commit: String,
    /// The whole signed package's digest.
    pub(super) package: String,
    /// Whether it is byte-for-byte the approved build, so there is nothing to update.
    pub(super) approved: bool,
    /// Whether it is an earlier release than the approved build, which an update never installs.
    pub(super) earlier: bool,
}

/// What one read of the server's version found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UpdateFacts {
    /// The service's status, or why it refused while an update it records is unresolved.
    pub(super) status: Result<ServiceStatus, CoreError>,
    /// The durably approved build. `None` while no service is installed.
    pub(super) approved: Option<RetainedInstallation>,
    /// The server build this SpaceTerm includes, or why it offers none.
    pub(super) included: Result<IncludedCore, CandidateError>,
    /// The last update the service records.
    pub(super) activation: Option<ActivationStatus>,
}

/// Reads native installation state only. The Machine store owns every update action.
pub(super) trait CoreUpdateControl: Send + Sync {
    fn read(&self) -> Result<UpdateFacts, CoreError>;
}

impl CoreUpdateControl for Service {
    fn read(&self) -> Result<UpdateFacts, CoreError> {
        read_facts(self, self.activation_status()?, Service::status)
    }
}

fn read_facts(
    service: &Service,
    activation: Option<ActivationStatus>,
    status: fn(&Service) -> Result<ServiceStatus, CoreError>,
) -> Result<UpdateFacts, CoreError> {
    // Keep the recorded original visible even when the status read refuses an unresolved update.
    // The service row supplies its recovery status independently of the live update store.
    let status = match (status(service), &activation) {
        (Ok(status), _) => Ok(status),
        (Err(error), Some(_)) => Err(error),
        (Err(error), None) => return Err(error),
    };
    // An installed service lists only its approved build.
    let approved = match status {
        Ok(status) if status.installed => service.retained_installations()?.into_iter().next(),
        _ => None,
    };
    let included = crate::local_startup::discover_candidate().map(|candidate| IncludedCore {
        version: candidate.version().to_owned(),
        commit: candidate.commit().to_owned(),
        package: candidate.digest().to_owned(),
        approved: approved
            .as_ref()
            .is_some_and(|approved| candidate.is_approved(approved)),
        // The service's own policy, so the row never offers what activation refuses.
        earlier: approved
            .as_ref()
            .is_some_and(|approved| is_released_downgrade(candidate.version(), &approved.version)),
    });
    Ok(UpdateFacts {
        status,
        approved,
        included,
        activation,
    })
}

/// Whether an installed bundle offers a newer Core. This read stages and starts nothing.
pub(crate) fn update_offered(service: &Service) -> bool {
    CoreUpdateControl::read(service).is_ok_and(|facts| {
        facts
            .included
            .is_ok_and(|included| !included.approved && !included.earlier)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UpdateStep {
    Check,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct UpdateState {
    pub(super) facts: Option<UpdateFacts>,
    pub(super) failure: Option<CoreError>,
    pub(super) working: Option<UpdateStep>,
}

pub(super) struct MachineUpdateRow {
    control: Arc<dyn CoreUpdateControl>,
    pub(super) state: UpdateState,
    read_outdated: bool,
    _operation: Option<Task<()>>,
}

impl MachineUpdateRow {
    pub(super) fn new(control: Arc<dyn CoreUpdateControl>) -> Self {
        Self {
            control,
            state: UpdateState::default(),
            read_outdated: false,
            _operation: None,
        }
    }
}

impl SettingsWindow {
    pub(super) fn check_core_update(&mut self, cx: &mut Context<Self>) {
        let Some(row) = &mut self.core_update else {
            return;
        };
        if row.state.working.is_some() {
            return;
        }
        row.state.working = Some(UpdateStep::Check);
        let control = Arc::clone(&row.control);
        let read = cx.background_spawn(async move { control.read() });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let read = read.await;
            let _ = settings.update(cx, |settings, cx| {
                let Some(row) = &mut settings.core_update else {
                    return;
                };
                row.state.working = None;
                if std::mem::take(&mut row.read_outdated) {
                    settings.check_core_update(cx);
                    return;
                }
                match read {
                    Ok(facts) => {
                        row.state.facts = Some(facts);
                        row.state.failure = None;
                    }
                    Err(error) => {
                        row.state.facts = None;
                        row.state.failure = Some(error);
                    }
                }
                settings.continue_requested_start(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn reread_core_update(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = &mut self.core_update
            && row.state.working.is_some()
        {
            row.read_outdated = true;
        } else {
            self.check_core_update(cx);
        }
    }

    /// The same Machine presentation that the title bar reads. Saved route identity, rather
    /// than its editable label, selects a paired Machine.
    pub(super) fn machine_update_view(
        &self,
        choice: &MachineChoice,
        cx: &App,
    ) -> Option<MachineView> {
        let shared = presentation::current(cx);
        match choice {
            MachineChoice::Local => shared.machines.into_iter().find(|view| view.local),
            MachineChoice::Direct(route) => {
                let machine = self
                    .direct_routes
                    .as_ref()?
                    .connections
                    .route(route)?
                    .paired
                    .pin
                    .core;
                shared
                    .machines
                    .into_iter()
                    .find(|view| view.machine == machine)
            }
        }
    }

    pub(crate) fn request_core_update(&mut self, cx: &mut Context<Self>) {
        let window = self.update_window;
        let Some(view) = self.machine_update_view(&MachineChoice::Local, cx) else {
            return;
        };
        if matches!(
            view.state,
            presentation::MachineState::Ready { .. }
                | presentation::MachineState::Overdue { .. }
                | presentation::MachineState::CoreTooOld { .. }
        ) && let Some(button) = view.settings_row().button
        {
            self.perform_update_command(button.command, window, cx);
        }
    }

    pub(super) fn request_machine_restart(
        &mut self,
        choice: &MachineChoice,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.machine_update_view(choice, cx) else {
            return;
        };
        self.perform_update_command(
            Command::Machine(view.machine, MachineAction::Restart),
            window,
            cx,
        );
    }

    pub(super) fn perform_update_command(
        &mut self,
        command: Command,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = owner.update(cx, |settings, cx| {
                settings.perform_update_command_now(command, window, cx)
            });
        });
    }

    fn perform_update_command_now(
        &mut self,
        command: Command,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        match command {
            Command::Confirm(confirmation) => {
                self.request_machine_actions(confirmation.actions, false, window, cx)
            }
            Command::Machine(machine, action @ (MachineAction::Restart | MachineAction::Stop)) => {
                self.request_machine_actions(vec![(machine, action)], false, window, cx)
            }
            Command::Machine(machine, action) => {
                if let Some(store) = MachineUpdates::global(cx) {
                    store.update(cx, |store, cx| {
                        store.perform(machine, action, Surface::Settings, cx)
                    });
                }
            }
            command @ Command::Desktop(_) => crate::ui::updates::commands::perform(
                command,
                crate::ui::updates::commands::Presenter::Front,
                cx,
            ),
            Command::Explain(explanation) => {
                // This runs inside an update of Settings, which the alert must update again.
                let owner = cx.weak_entity();
                cx.defer(move |cx| present_explanation(owner, explanation, window, cx));
            }
        }
    }

    fn request_machine_actions(
        &mut self,
        actions: Vec<(crate::updates::machines::MachineId, MachineAction)>,
        always: bool,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        let ask: store::Ask = Rc::new(move |confirmation: Confirmation, answered, cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = owner.update(cx, |_, cx| {
                    let _ = Alert::new(
                        ModalId::new("settings-server-update"),
                        confirmation.title.clone(),
                        confirmation.title,
                        confirmation.message,
                        vec![
                            ModalAction::new(
                                true,
                                confirmation.button,
                                ModalActionRole::Affirmative,
                                UPDATE_CONFIRM_SELECTOR,
                            )
                            .with_intent(ModalActionIntent::Destructive)
                            .with_emphasis(ModalActionEmphasis::Prominent),
                            ModalAction::new(
                                false,
                                "Cancel",
                                ModalActionRole::Cancel,
                                UPDATE_CANCEL_SELECTOR,
                            ),
                        ],
                    )
                    .intent(AlertIntent::Critical)
                    .present(window, cx, move |outcome, cx| {
                        answered(
                            matches!(
                                outcome,
                                AlertOutcome::Activated {
                                    action_id: true,
                                    ..
                                }
                            ),
                            cx,
                        )
                    });
                });
            });
        });
        store::request(actions, always, Surface::Settings, ask, cx);
    }

    pub(super) fn observe_machine_updates(&mut self, cx: &mut Context<Self>) {
        let Some(store) = MachineUpdates::global(cx) else {
            return;
        };
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&store, |settings, _, notice, cx| {
            if notice.surface != Surface::Settings {
                return;
            }
            let Err(failure) = notice.outcome else { return };
            let window = settings.update_window;
            if failure == ActionFailure::Changed
                && matches!(notice.action, MachineAction::Restart | MachineAction::Stop)
            {
                settings.request_machine_actions(
                    vec![(notice.machine, notice.action.clone())],
                    true,
                    window,
                    cx,
                );
            } else if failure == ActionFailure::LoginItemOff {
                crate::local_startup::open_login_items();
            } else if let Some((title, message)) =
                presentation::action_failure(&notice.name, notice.local, &notice.action, failure)
            {
                settings.perform_update_command(
                    Command::Explain(presentation::Explanation {
                        title,
                        message,
                        then: None,
                    }),
                    window,
                    cx,
                );
            }
        })
        .detach();
    }
}

/// Presents `explanation` over Settings, outside any update of it.
fn present_explanation(
    owner: gpui::WeakEntity<SettingsWindow>,
    explanation: presentation::Explanation,
    window: AnyWindowHandle,
    cx: &mut App,
) {
    let Some(presenter) = owner.upgrade() else {
        return;
    };
    let handle = window;
    let _ = window.update(cx, |_, window, cx| {
        presenter.update(cx, |_, cx| {
            let _ = Alert::new(
                ModalId::new("settings-update-explanation"),
                explanation.title.clone(),
                explanation.title,
                explanation.message,
                vec![ModalAction::new(
                    true,
                    "OK",
                    ModalActionRole::Affirmative,
                    "settings-update-explanation-ok",
                )],
            )
            .present(window, cx, move |outcome, cx| {
                if matches!(
                    outcome,
                    AlertOutcome::Activated {
                        action_id: true,
                        ..
                    }
                ) && let Some(then) = explanation.then
                {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.perform_update_command(*then, handle, cx)
                    });
                }
            });
        });
    });
}

#[cfg(test)]
pub(in crate::ui::settings_window) mod testing {
    use super::*;
    use std::sync::Mutex;
    pub(in crate::ui::settings_window) fn approved() -> RetainedInstallation {
        super::super::machines::testing::retained("1.4.0", 'a')
    }
    pub(in crate::ui::settings_window) fn included() -> RetainedInstallation {
        super::super::machines::testing::retained("1.5.0", 'b')
    }
    pub(in crate::ui::settings_window) fn available() -> UpdateFacts {
        UpdateFacts {
            status: Ok(super::super::machines::testing::running()),
            approved: Some(approved()),
            included: Ok(IncludedCore {
                version: included().version,
                commit: included().commit,
                package: "b".repeat(64),
                approved: false,
                earlier: false,
            }),
            activation: None,
        }
    }
    pub(in crate::ui::settings_window) struct ScriptedUpdate {
        facts: Mutex<UpdateFacts>,
        reads: Mutex<usize>,
    }
    impl ScriptedUpdate {
        pub(in crate::ui::settings_window) fn new(facts: UpdateFacts) -> Arc<Self> {
            Arc::new(Self {
                facts: Mutex::new(facts),
                reads: Mutex::new(0),
            })
        }
        pub(in crate::ui::settings_window) fn reads(&self) -> usize {
            *self.reads.lock().unwrap()
        }
    }
    impl CoreUpdateControl for ScriptedUpdate {
        fn read(&self) -> Result<UpdateFacts, CoreError> {
            *self.reads.lock().unwrap() += 1;
            Ok(self.facts.lock().unwrap().clone())
        }
    }
}

#[cfg(test)]
#[path = "core_update_tests.rs"]
mod tests;
